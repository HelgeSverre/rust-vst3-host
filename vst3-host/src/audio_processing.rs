//! Real-time audio processing implementation

use crate::com_implementations::{EventList, ParameterChanges};
use crate::realtime::{AudioState, ProcessorWrapper};
use std::ptr;
use std::sync::atomic::Ordering;
use vst3::{ComWrapper, Steinberg::Vst::*, Steinberg::*};

#[allow(dead_code)]

/// Pre-allocated host process data for real-time audio
pub struct HostProcessData {
    /// VST3 process data structure
    pub process_data: ProcessData,
    /// Process context
    pub process_context: ProcessContext,
    
    /// Pre-allocated audio buffers
    pub input_buffers: Vec<Vec<f32>>,
    pub output_buffers: Vec<Vec<f32>>,
    
    /// Bus buffer structures
    pub input_bus_buffers: Vec<AudioBusBuffers>,
    pub output_bus_buffers: Vec<AudioBusBuffers>,
    
    /// Channel pointers for VST3
    pub input_channel_pointers: Vec<Vec<*mut f32>>,
    pub output_channel_pointers: Vec<Vec<*mut f32>>,
    
    /// Event lists (lock-free)
    pub input_events: ComWrapper<EventList>,
    pub output_events: ComWrapper<EventList>,
    pub input_events_ptr: *mut IEventList,
    pub output_events_ptr: *mut IEventList,
    
    /// Parameter changes (lock-free)
    pub input_param_changes: ComWrapper<ParameterChanges>,
    pub output_param_changes: ComWrapper<ParameterChanges>,
    pub input_param_changes_ptr: *mut IParameterChanges,
    pub output_param_changes_ptr: *mut IParameterChanges,
}

// SAFETY: HostProcessData is designed for single-threaded use in audio callback
unsafe impl Send for HostProcessData {}
unsafe impl Sync for HostProcessData {}

impl HostProcessData {
    pub fn new(max_block_size: usize, sample_rate: f64) -> Self {
        unsafe {
            // Create event lists
            let input_events = ComWrapper::new(EventList::new(256));
            let output_events = ComWrapper::new(EventList::new(256));
            
            let input_com_ptr = input_events
                .to_com_ptr::<IEventList>()
                .expect("Failed to get input events pointer");
            let output_com_ptr = output_events
                .to_com_ptr::<IEventList>()
                .expect("Failed to get output events pointer");
            
            let input_events_ptr = input_com_ptr.clone().into_raw();
            let output_events_ptr = output_com_ptr.clone().into_raw();
            
            // Create parameter changes
            let input_param_changes = ComWrapper::new(ParameterChanges::new());
            let output_param_changes = ComWrapper::new(ParameterChanges::new());
            
            let input_param_com_ptr = input_param_changes
                .to_com_ptr::<IParameterChanges>()
                .expect("Failed to get input param changes pointer");
            let output_param_com_ptr = output_param_changes
                .to_com_ptr::<IParameterChanges>()
                .expect("Failed to get output param changes pointer");
            
            let input_param_changes_ptr = input_param_com_ptr.clone().into_raw();
            let output_param_changes_ptr = output_param_com_ptr.clone().into_raw();
            
            let mut data = Self {
                process_data: std::mem::zeroed(),
                process_context: std::mem::zeroed(),
                input_buffers: Vec::new(),
                output_buffers: Vec::new(),
                input_bus_buffers: Vec::new(),
                output_bus_buffers: Vec::new(),
                input_channel_pointers: Vec::new(),
                output_channel_pointers: Vec::new(),
                input_events,
                output_events,
                input_events_ptr,
                output_events_ptr,
                input_param_changes,
                output_param_changes,
                input_param_changes_ptr,
                output_param_changes_ptr,
            };
            
            // Initialize process context
            data.process_context.sampleRate = sample_rate;
            data.process_context.tempo = 120.0;
            data.process_context.timeSigNumerator = 4;
            data.process_context.timeSigDenominator = 4;
            data.process_context.state = ProcessContext_::StatesAndFlags_::kPlaying
                | ProcessContext_::StatesAndFlags_::kTempoValid
                | ProcessContext_::StatesAndFlags_::kTimeSigValid
                | ProcessContext_::StatesAndFlags_::kContTimeValid
                | ProcessContext_::StatesAndFlags_::kSystemTimeValid
                | ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid;
            
            // Set up process data
            data.process_data.processMode = ProcessModes_::kRealtime as i32;
            data.process_data.numSamples = max_block_size as i32;
            data.process_data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
            data.process_data.processContext = &mut data.process_context;
            data.process_data.inputEvents = data.input_events_ptr;
            data.process_data.outputEvents = data.output_events_ptr;
            data.process_data.inputParameterChanges = data.input_param_changes_ptr;
            data.process_data.outputParameterChanges = data.output_param_changes_ptr;
            
            data
        }
    }
    
    /// Prepare buffers based on plugin configuration
    /// This should be called for each process() to ensure fresh buffers
    pub unsafe fn prepare_buffers(
        &mut self,
        processor_wrapper: &ProcessorWrapper,
        block_size: usize,
    ) {
        use BusDirections_::*;
        use MediaTypes_::*;
        
        let component = processor_wrapper.component();
        
        // Get bus counts
        let input_bus_count = component.getBusCount(kAudio as i32, kInput as i32);
        let output_bus_count = component.getBusCount(kAudio as i32, kOutput as i32);
        
        
        // Prepare input buffers
        self.input_bus_buffers.clear();
        self.input_buffers.clear();
        self.input_channel_pointers.clear();
        
        for bus_idx in 0..input_bus_count {
            let mut bus_info: BusInfo = std::mem::zeroed();
            if component.getBusInfo(kAudio as i32, kInput as i32, bus_idx, &mut bus_info) == kResultOk {
                let channel_count = bus_info.channelCount;
                
                // Create buffers for this bus
                let mut channel_ptrs = Vec::new();
                let start_idx = self.input_buffers.len();
                
                for _ in 0..channel_count {
                    self.input_buffers.push(vec![0.0f32; block_size]);
                }
                
                // Get pointers after all buffers are created
                for i in 0..channel_count {
                    channel_ptrs.push(self.input_buffers[start_idx + i as usize].as_mut_ptr());
                }
                
                self.input_channel_pointers.push(channel_ptrs);
                
                // Create AudioBusBuffers
                let mut audio_bus_buffer: AudioBusBuffers = std::mem::zeroed();
                audio_bus_buffer.numChannels = channel_count;
                audio_bus_buffer.__field0.channelBuffers32 = 
                    if self.input_channel_pointers.last().unwrap().is_empty() {
                        ptr::null_mut()
                    } else {
                        self.input_channel_pointers.last_mut().unwrap().as_mut_ptr()
                    };
                
                self.input_bus_buffers.push(audio_bus_buffer);
            }
        }
        
        // Prepare output buffers
        self.output_bus_buffers.clear();
        self.output_buffers.clear();
        self.output_channel_pointers.clear();
        
        for bus_idx in 0..output_bus_count {
            let mut bus_info: BusInfo = std::mem::zeroed();
            if component.getBusInfo(kAudio as i32, kOutput as i32, bus_idx, &mut bus_info) == kResultOk {
                let channel_count = bus_info.channelCount;
                
                // Create buffers for this bus
                let mut channel_ptrs = Vec::new();
                let start_idx = self.output_buffers.len();
                
                for _ in 0..channel_count {
                    self.output_buffers.push(vec![0.0f32; block_size]);
                }
                
                // Get pointers after all buffers are created
                for i in 0..channel_count {
                    channel_ptrs.push(self.output_buffers[start_idx + i as usize].as_mut_ptr());
                }
                
                self.output_channel_pointers.push(channel_ptrs);
                
                // Create AudioBusBuffers
                let mut audio_bus_buffer: AudioBusBuffers = std::mem::zeroed();
                audio_bus_buffer.numChannels = channel_count;
                audio_bus_buffer.__field0.channelBuffers32 = 
                    if self.output_channel_pointers.last().unwrap().is_empty() {
                        ptr::null_mut()
                    } else {
                        self.output_channel_pointers.last_mut().unwrap().as_mut_ptr()
                    };
                
                self.output_bus_buffers.push(audio_bus_buffer);
            }
        }
        
        // Update ProcessData pointers
        self.process_data.numInputs = self.input_bus_buffers.len() as i32;
        self.process_data.numOutputs = self.output_bus_buffers.len() as i32;
        self.process_data.inputs = if self.input_bus_buffers.is_empty() {
            ptr::null_mut()
        } else {
            self.input_bus_buffers.as_mut_ptr()
        };
        self.process_data.outputs = if self.output_bus_buffers.is_empty() {
            ptr::null_mut()
        } else {
            self.output_bus_buffers.as_mut_ptr()
        };
    }
    
    /// Clear all buffers
    pub fn clear_buffers(&mut self) {
        // Clear audio buffers
        for buffer in &mut self.input_buffers {
            buffer.fill(0.0);
        }
        for buffer in &mut self.output_buffers {
            buffer.fill(0.0);
        }
        
        // Clear BOTH input and output events (like vst3-inspector)
        self.input_events.clear();
        self.output_events.clear();
        
        // Clear parameter changes
        self.input_param_changes.clear();
        self.output_param_changes.clear();
    }
    
}

// No Drop implementation needed - the ComWrapper fields will handle cleanup automatically

/// Process audio with a VST3 plugin in real-time
pub fn process_vst3_audio(
    processor_wrapper: &ProcessorWrapper,
    state: &AudioState,
    process_data: &mut HostProcessData,
    input: &[f32],
    output: &mut [f32],
    channels: usize,
) -> bool {
    let frames = output.len() / channels;
    
    unsafe {
        // Update sample count and ensure ProcessData is consistent
        process_data.process_data.numSamples = frames as i32;
        
        // Don't clear buffers here - they should already be prepared
        
        // Update continuous time position and timing info
        let sample_pos = state.sample_counter.load(Ordering::Acquire);
        process_data.process_context.continousTimeSamples = sample_pos;
        process_data.process_context.projectTimeSamples = sample_pos;
        process_data.process_context.projectTimeMusic = sample_pos as f64 / state.sample_rate / 60.0 * process_data.process_context.tempo;
        process_data.process_context.systemTime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        
        // Copy input to plugin buffers (if plugin has inputs)
        if !process_data.input_buffers.is_empty() && channels > 0 && input.len() == output.len() {
            for frame in 0..frames {
                for ch in 0..channels.min(process_data.input_buffers.len()) {
                    process_data.input_buffers[ch][frame] = input[frame * channels + ch];
                }
            }
        }
        
        
        // Process the audio
        let result = processor_wrapper.processor().process(&mut process_data.process_data);
        
        if result == kResultOk {
            // Copy output to the provided buffer
            if !process_data.output_buffers.is_empty() && channels > 0 {
                let plugin_channels = process_data.output_buffers.len();
                
                // Check if we're getting any non-zero output
                let mut has_output = false;
                
                for frame in 0..frames {
                    if plugin_channels == 1 && channels == 2 {
                        // Mono to stereo - copy to both channels
                        let sample = process_data.output_buffers[0][frame];
                        if sample != 0.0 && !has_output {
                            has_output = true;
                        }
                        output[frame * channels] = sample;
                        output[frame * channels + 1] = sample;
                    } else {
                        // Direct copy for matching channel counts
                        for ch in 0..channels.min(plugin_channels) {
                            let sample = process_data.output_buffers[ch][frame];
                            if sample != 0.0 && !has_output {
                                has_output = true;
                            }
                            output[frame * channels + ch] = sample;
                        }
                    }
                }
            }
            
            // Clear both input and output events after processing
            process_data.input_events.clear();
            process_data.output_events.clear();
            
            true
        } else {
            output.fill(0.0);
            false
        }
    }
}