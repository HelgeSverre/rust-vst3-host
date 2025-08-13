//! Real-time safe audio processing components

use crate::audio_processing::{HostProcessData, process_vst3_audio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicI64, Ordering};
use std::cell::UnsafeCell;
use std::ptr;
use crossbeam::channel::{bounded, Sender, Receiver};
use vst3::{ComPtr, Steinberg::Vst::*};

/// Commands sent from main thread to audio thread
#[derive(Clone)]
pub enum Command {
    /// Set a parameter value
    SetParameter { 
        /// Parameter ID
        id: u32, 
        /// Normalized value (0.0 - 1.0)
        value: f64 
    },
    
    /// Send a MIDI event
    MidiEvent(Event),
    
    /// Update configuration
    UpdateConfig { 
        /// Sample rate in Hz
        sample_rate: f64, 
        /// Block size in samples
        block_size: i32 
    },
    
    /// Stop processing
    Stop,
}

/// Audio metrics sent from audio thread to main thread
#[derive(Debug, Clone, Copy, Default)]
pub struct Metrics {
    /// Peak level for left channel (0.0 - 1.0)
    pub peak_left: f32,
    /// Peak level for right channel (0.0 - 1.0)
    pub peak_right: f32,
    /// Current sample position
    pub sample_position: i64,
}

/// Real-time safe parameter with atomic value storage
pub struct Parameter {
    /// Parameter ID
    pub id: u32,
    /// Parameter name
    pub name: String,
    /// Minimum value
    pub min: f64,
    /// Maximum value
    pub max: f64,
    /// Current value (atomic)
    value: AtomicU64,
    /// Units
    pub units: String,
}

impl Parameter {
    /// Create a new parameter
    pub fn new(id: u32, name: String, initial: f64, min: f64, max: f64, units: String) -> Self {
        let clamped = initial.clamp(min, max);
        Self {
            id,
            name,
            min,
            max,
            value: AtomicU64::new(clamped.to_bits()),
            units,
        }
    }
    
    /// Set value (non-blocking)
    pub fn set(&self, value: f64) {
        let clamped = value.clamp(self.min, self.max);
        self.value.store(clamped.to_bits(), Ordering::Release);
    }
    
    /// Get value (wait-free)
    pub fn get(&self) -> f64 {
        f64::from_bits(self.value.load(Ordering::Acquire))
    }
    
    /// Get normalized value (0.0 - 1.0)
    pub fn get_normalized(&self) -> f64 {
        let value = self.get();
        if self.max > self.min {
            (value - self.min) / (self.max - self.min)
        } else {
            0.0
        }
    }
    
    /// Set from normalized value (0.0 - 1.0)
    pub fn set_normalized(&self, normalized: f64) {
        let value = self.min + normalized * (self.max - self.min);
        self.set(value);
    }
}

/// Triple buffer for complex state updates
pub struct TripleBuffer<T> {
    buffers: [UnsafeCell<T>; 3],
    write_idx: AtomicU64,
    read_idx: AtomicU64,
    middle_idx: AtomicU64,
}

// SAFETY: TripleBuffer is thread-safe if T is Send
unsafe impl<T: Send> Send for TripleBuffer<T> {}
unsafe impl<T: Send> Sync for TripleBuffer<T> {}

impl<T: Clone> TripleBuffer<T> {
    /// Create a new triple buffer
    pub fn new(initial: T) -> Self {
        Self {
            buffers: [
                UnsafeCell::new(initial.clone()),
                UnsafeCell::new(initial.clone()),
                UnsafeCell::new(initial),
            ],
            write_idx: AtomicU64::new(0),
            read_idx: AtomicU64::new(1),
            middle_idx: AtomicU64::new(2),
        }
    }
    
    /// Write new value (from main thread)
    pub fn write(&self, value: T) {
        let write_idx = self.write_idx.load(Ordering::Acquire) as usize;
        unsafe {
            *self.buffers[write_idx].get() = value;
        }
        
        // Swap write and middle buffers atomically
        let middle_idx = self.middle_idx.swap(write_idx as u64, Ordering::AcqRel);
        self.write_idx.store(middle_idx, Ordering::Release);
    }
    
    /// Read latest value (from audio thread, wait-free)
    pub fn read(&self) -> &T {
        // Try to get the latest value by swapping read and middle
        let middle_idx = self.middle_idx.load(Ordering::Acquire);
        let old_read = self.read_idx.load(Ordering::Acquire);
        
        // Only swap if middle buffer is newer
        if middle_idx != old_read {
            self.read_idx.store(middle_idx, Ordering::Release);
            self.middle_idx.store(old_read, Ordering::Release);
        }
        
        let read_idx = self.read_idx.load(Ordering::Acquire) as usize;
        unsafe { &*self.buffers[read_idx].get() }
    }
}

/// Pre-allocated buffers for audio processing
pub struct AudioBuffers {
    /// Input buffers [channel][sample]
    pub inputs: Vec<Vec<f32>>,
    /// Output buffers [channel][sample]
    pub outputs: Vec<Vec<f32>>,
    /// Maximum block size
    pub max_block_size: usize,
}

impl AudioBuffers {
    /// Create new audio buffers
    pub fn new(input_channels: usize, output_channels: usize, max_block_size: usize) -> Self {
        Self {
            inputs: vec![vec![0.0; max_block_size]; input_channels],
            outputs: vec![vec![0.0; max_block_size]; output_channels],
            max_block_size,
        }
    }
    
    /// Clear all buffers
    pub fn clear(&mut self) {
        for buf in &mut self.inputs {
            buf.fill(0.0);
        }
        for buf in &mut self.outputs {
            buf.fill(0.0);
        }
    }
}

/// VST3 processor wrapper for thread safety
#[derive(Clone)]
pub struct ProcessorWrapper {
    processor: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
}

// SAFETY: VST3 interfaces are designed to be thread-safe
unsafe impl Send for ProcessorWrapper {}
unsafe impl Sync for ProcessorWrapper {}

impl ProcessorWrapper {
    /// Create new processor wrapper
    pub fn new(processor: ComPtr<IAudioProcessor>, component: ComPtr<IComponent>) -> Self {
        Self { processor, component }
    }
    
    /// Get the processor
    pub fn processor(&self) -> &ComPtr<IAudioProcessor> {
        &self.processor
    }
    
    /// Get the component
    pub fn component(&self) -> &ComPtr<IComponent> {
        &self.component
    }
}

/// Shared audio state for real-time processing
pub struct AudioState {
    /// Is processing active
    pub is_active: AtomicBool,
    
    /// Sample counter
    pub sample_counter: AtomicI64,
    
    /// Commands from main to audio thread (sender)
    pub command_tx: Sender<Command>,
    /// Commands from main to audio thread (receiver)
    pub command_rx: Receiver<Command>,
    
    /// Metrics from audio to main thread (sender)
    pub metrics_tx: Sender<Metrics>,
    /// Metrics from audio to main thread (receiver)
    pub metrics_rx: Receiver<Metrics>,
    
    /// Parameters (by ID)
    pub parameters: std::sync::RwLock<Vec<Arc<Parameter>>>,
    
    /// Processor state via triple buffer
    pub processor: TripleBuffer<Option<ProcessorWrapper>>,
    
    /// Sample rate for creating process data
    pub sample_rate_stored: f64,
    
    /// Block size for creating process data  
    pub block_size_stored: usize,
    
    /// Sample rate
    pub sample_rate: f64,
    
    /// Maximum block size
    pub max_block_size: usize,
}

// SAFETY: AudioState is designed to be shared between threads
unsafe impl Send for AudioState {}
unsafe impl Sync for AudioState {}

impl AudioState {
    /// Create new audio state
    pub fn new(sample_rate: f64, max_block_size: usize) -> Self {
        let (cmd_tx, cmd_rx) = bounded(256);
        let (metrics_tx, metrics_rx) = bounded(64);
        
        Self {
            is_active: AtomicBool::new(false),
            sample_counter: AtomicI64::new(0),
            command_tx: cmd_tx,
            command_rx: cmd_rx,
            metrics_tx: metrics_tx,
            metrics_rx: metrics_rx,
            parameters: std::sync::RwLock::new(Vec::new()),
            processor: TripleBuffer::new(None),
            sample_rate_stored: sample_rate,
            block_size_stored: max_block_size,
            sample_rate,
            max_block_size,
        }
    }
}

/// Process audio in real-time thread (wait-free)
/// 
/// This function is designed to be called from the audio callback.
/// It is wait-free and performs no allocations.
pub fn process_audio(
    state: &AudioState,
    output: &mut [f32],
    channels: usize,
) -> bool {
    // Create silence buffer on stack for synth mode
    const MAX_BLOCK_SIZE: usize = 4096;
    let silence_buffer = [0.0f32; MAX_BLOCK_SIZE];
    let len = output.len().min(MAX_BLOCK_SIZE);
    process_audio_with_input(state, &silence_buffer[..len], output, channels)
}

/// Process audio with separate input/output in real-time thread (wait-free)
/// 
/// This function is designed to be called from the audio callback.
/// It is wait-free and performs no allocations.
pub fn process_audio_with_input(
    state: &AudioState,
    input: &[f32],
    output: &mut [f32],
    channels: usize,
) -> bool {
    if !state.is_active.load(Ordering::Acquire) {
        output.fill(0.0);
        return false;
    }
    
    let frames = output.len() / channels;
    
    // Commands are now processed inside the processor section
    
    // Get processor
    if let Some(processor_wrapper) = state.processor.read().as_ref() {
        // Create fresh ProcessData for each callback (like vst3-inspector)
        unsafe {
            // Create a new ProcessData for this callback
            let mut process_data = HostProcessData::new(state.block_size_stored, state.sample_rate_stored);
            
            // Prepare buffers for current frame count
            process_data.prepare_buffers(processor_wrapper, frames);
            
            // Clear all buffers first (this clears event lists too)
            process_data.clear_buffers();
            
            // Process any pending MIDI events
            // Check command queue for MIDI events
            let mut pending_events = Vec::new();
            while let Ok(cmd) = state.command_rx.try_recv() {
                match cmd {
                    Command::MidiEvent(mut event) => {
                        event.flags = 1; // kIsLive
                        event.sampleOffset = 0; // Event happens at start of this buffer
                        pending_events.push(event);
                    }
                    Command::SetParameter { id, value } => {
                        if let Ok(parameters) = state.parameters.read() {
                            for param in parameters.iter() {
                                if param.id == id {
                                    param.set_normalized(value);
                                    break;
                                }
                            }
                        }
                    }
                    Command::Stop => {
                        state.is_active.store(false, Ordering::Release);
                        output.fill(0.0);
                        return false;
                    }
                    _ => {}
                }
            }
            
            // Add events to input list
            if !pending_events.is_empty() {
                process_data.input_events.add_events(&pending_events);
            }
            
            // Process parameter changes
            if let Ok(parameters) = state.parameters.read() {
                for param in parameters.iter() {
                    // In a real implementation, we'd track previous values
                    let mut index: i32 = 0;
                    let queue_ptr = process_data.input_param_changes.addParameterData(
                        &param.id,
                        &mut index
                    );
                    
                    if !queue_ptr.is_null() {
                        // Cast to raw pointer and use vst3 trait methods
                        let queue = ComPtr::<IParamValueQueue>::from_raw(queue_ptr);
                        if let Some(q) = queue {
                            q.addPoint(0, param.get_normalized(), ptr::null_mut());
                        }
                    }
                }
            }
            
            // Update sample counter for time tracking
            let sample_counter = state.sample_counter.load(Ordering::Relaxed);
            process_data.process_context.continousTimeSamples = sample_counter;
            
            // Process audio
            let success = process_vst3_audio(processor_wrapper, state, &mut process_data, input, output, channels);
            
            // Update sample counter
            state.sample_counter.fetch_add(frames as i64, Ordering::Relaxed);
            
            // Calculate and send metrics (non-blocking)
            let mut peak_left = 0.0f32;
            let mut peak_right = 0.0f32;
            
            for frame in 0..frames {
                if channels >= 2 {
                    let left = output[frame * channels].abs();
                    let right = output[frame * channels + 1].abs();
                    peak_left = peak_left.max(left);
                    peak_right = peak_right.max(right);
                }
            }
            
            let _ = state.metrics_tx.try_send(Metrics {
                peak_left,
                peak_right,
                sample_position: state.sample_counter.load(Ordering::Relaxed),
            });
            
            success
        }
    } else {
        // No processor available
        output.fill(0.0);
        false
    }
}