use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use vst3::{ComPtr, ComWrapper, Steinberg::*, Steinberg::Vst::*};
use crate::com_implementations::{HostEventList, MonitoredEventList, ParameterChanges, create_event_list, create_monitored_event_list};
use crate::data_structures::MidiDirection;

pub struct HostProcessData {
    pub process_data: ProcessData,
    pub input_buffers: Vec<Vec<f32>>,
    pub output_buffers: Vec<Vec<f32>>,
    pub input_bus_buffers: Vec<AudioBusBuffers>,
    pub output_bus_buffers: Vec<AudioBusBuffers>,
    pub input_channel_pointers: Vec<Vec<*mut f32>>,
    pub output_channel_pointers: Vec<Vec<*mut f32>>,
    pub process_context: ProcessContext,
    pub input_events: ComWrapper<HostEventList>,
    pub output_events: ComWrapper<HostEventList>,
    pub monitored_input_events: Option<ComWrapper<MonitoredEventList>>,
    pub monitored_output_events: Option<ComWrapper<MonitoredEventList>>,
    pub input_events_ptr: *mut IEventList,
    pub output_events_ptr: *mut IEventList,
    pub input_param_changes: ComWrapper<ParameterChanges>,
    pub output_param_changes: ComWrapper<ParameterChanges>,
    pub input_param_changes_ptr: *mut IParameterChanges,
    pub output_param_changes_ptr: *mut IParameterChanges,
}

impl HostProcessData {
    /// Creates a new HostProcessData with MIDI event monitoring
    /// 
    /// # Safety
    /// This function is unsafe because it creates raw pointers to COM interfaces
    /// that must be properly managed and released when no longer needed.
    pub unsafe fn new_with_monitoring(
        block_size: i32, 
        sample_rate: f64,
        event_monitor: Arc<Mutex<Vec<(Instant, MidiDirection, Event)>>>
    ) -> Self {
        let monitored_input_events = create_monitored_event_list(MidiDirection::Input, event_monitor.clone());
        let monitored_output_events = create_monitored_event_list(MidiDirection::Output, event_monitor.clone());
        
        // Get COM pointers
        let input_com_ptr = monitored_input_events.to_com_ptr::<IEventList>()
            .expect("Failed to get input events pointer");
        let output_com_ptr = monitored_output_events.to_com_ptr::<IEventList>()
            .expect("Failed to get output events pointer");
            
        let input_events_ptr = input_com_ptr.clone().into_raw();
        let output_events_ptr = output_com_ptr.clone().into_raw();
        
        // For compatibility, also create regular event lists
        let input_events = create_event_list();
        let output_events = create_event_list();
        
        // Create parameter changes
        let input_param_changes = ComWrapper::new(ParameterChanges::default());
        let output_param_changes = ComWrapper::new(ParameterChanges::default());
        
        let input_param_com_ptr = input_param_changes.to_com_ptr::<IParameterChanges>()
            .expect("Failed to get input param changes pointer");
        let output_param_com_ptr = output_param_changes.to_com_ptr::<IParameterChanges>()
            .expect("Failed to get output param changes pointer");
            
        let input_param_changes_ptr = input_param_com_ptr.clone().into_raw();
        let output_param_changes_ptr = output_param_com_ptr.clone().into_raw();
        
        let mut data = Self {
            process_data: std::mem::zeroed(),
            input_buffers: Vec::new(),
            output_buffers: Vec::new(),
            input_bus_buffers: Vec::new(),
            output_bus_buffers: Vec::new(),
            input_channel_pointers: Vec::new(),
            output_channel_pointers: Vec::new(),
            process_context: std::mem::zeroed(),
            input_events,
            output_events,
            monitored_input_events: Some(monitored_input_events),
            monitored_output_events: Some(monitored_output_events),
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
        data.process_context.state = ProcessContext_::StatesAndFlags_::kPlaying |
                                     ProcessContext_::StatesAndFlags_::kTempoValid |
                                     ProcessContext_::StatesAndFlags_::kTimeSigValid |
                                     ProcessContext_::StatesAndFlags_::kContTimeValid |
                                     ProcessContext_::StatesAndFlags_::kSystemTimeValid;
        
        // Set up process data
        data.process_data.processMode = ProcessModes_::kRealtime as i32;
        data.process_data.numSamples = block_size;
        data.process_data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
        data.process_data.processContext = &mut data.process_context;
        data.process_data.inputEvents = data.input_events_ptr;
        data.process_data.outputEvents = data.output_events_ptr;
        data.process_data.inputParameterChanges = data.input_param_changes_ptr;
        data.process_data.outputParameterChanges = data.output_param_changes_ptr;
        
        data
    }
    
    /// Creates a new HostProcessData without MIDI event monitoring
    /// 
    /// # Safety
    /// This function is unsafe because it creates raw pointers to COM interfaces
    /// that must be properly managed and released when no longer needed.
    pub unsafe fn new(block_size: i32, sample_rate: f64) -> Self {
        let input_events = create_event_list();
        let output_events = create_event_list();
        
        // Get COM pointers but don't release ownership yet
        let input_com_ptr = input_events.to_com_ptr::<IEventList>()
            .expect("Failed to get input events pointer");
        let output_com_ptr = output_events.to_com_ptr::<IEventList>()
            .expect("Failed to get output events pointer");
            
        // Clone the pointers (increases ref count) before getting raw
        let input_events_ptr = input_com_ptr.clone().into_raw();
        let output_events_ptr = output_com_ptr.clone().into_raw();
        
        // Create parameter changes
        let input_param_changes = ComWrapper::new(ParameterChanges::default());
        let output_param_changes = ComWrapper::new(ParameterChanges::default());
        
        // Get COM pointers for parameter changes
        let input_param_com_ptr = input_param_changes.to_com_ptr::<IParameterChanges>()
            .expect("Failed to get input param changes pointer");
        let output_param_com_ptr = output_param_changes.to_com_ptr::<IParameterChanges>()
            .expect("Failed to get output param changes pointer");
            
        let input_param_changes_ptr = input_param_com_ptr.clone().into_raw();
        let output_param_changes_ptr = output_param_com_ptr.clone().into_raw();
        
        let mut data = Self {
            process_data: std::mem::zeroed(),
            input_buffers: Vec::new(),
            output_buffers: Vec::new(),
            input_bus_buffers: Vec::new(),
            output_bus_buffers: Vec::new(),
            input_channel_pointers: Vec::new(),
            output_channel_pointers: Vec::new(),
            process_context: std::mem::zeroed(),
            input_events,
            output_events,
            monitored_input_events: None,
            monitored_output_events: None,
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
        data.process_context.state = ProcessContext_::StatesAndFlags_::kPlaying |
                                     ProcessContext_::StatesAndFlags_::kTempoValid |
                                     ProcessContext_::StatesAndFlags_::kTimeSigValid |
                                     ProcessContext_::StatesAndFlags_::kContTimeValid |
                                     ProcessContext_::StatesAndFlags_::kSystemTimeValid;
        
        // Set up process data
        data.process_data.processMode = ProcessModes_::kRealtime as i32;
        data.process_data.numSamples = block_size;
        data.process_data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
        data.process_data.processContext = &mut data.process_context;
        data.process_data.inputEvents = data.input_events_ptr;
        data.process_data.outputEvents = data.output_events_ptr;
        data.process_data.inputParameterChanges = data.input_param_changes_ptr;
        data.process_data.outputParameterChanges = data.output_param_changes_ptr;
        
        data
    }
    
    /// Prepares audio buffers based on plugin bus configuration
    /// 
    /// # Safety
    /// This function is unsafe because it directly manipulates raw pointers for audio buffers
    /// and assumes the provided IComponent is valid.
    pub unsafe fn prepare_buffers(&mut self, component: &ComPtr<IComponent>, block_size: i32) -> Result<(), String> {
        use BusDirections_::*;
        use MediaTypes_::*;
        
        // Get bus counts
        let input_bus_count = component.getBusCount(kAudio as i32, kInput as i32);
        let output_bus_count = component.getBusCount(kAudio as i32, kOutput as i32);
        
        // println!("🎵 Preparing buffers: {} input buses, {} output buses", input_bus_count, output_bus_count);
        
        // Prepare input buffers
        self.input_bus_buffers.clear();
        self.input_buffers.clear();
        self.input_channel_pointers.clear();
        
        for bus_idx in 0..input_bus_count {
            let mut bus_info: BusInfo = std::mem::zeroed();
            if component.getBusInfo(kAudio as i32, kInput as i32, bus_idx, &mut bus_info) == kResultOk {
                let channel_count = bus_info.channelCount;
                
                // Create buffers for this bus
                let mut bus_buffers = Vec::new();
                let mut channel_ptrs = Vec::new();
                
                for _ in 0..channel_count {
                    let mut buffer = vec![0.0f32; block_size as usize];
                    let ptr = buffer.as_mut_ptr();
                    bus_buffers.push(buffer);
                    channel_ptrs.push(ptr);
                }
                
                self.input_buffers.extend(bus_buffers);
                self.input_channel_pointers.push(channel_ptrs);
                
                // Create AudioBusBuffers
                let mut audio_bus_buffer: AudioBusBuffers = std::mem::zeroed();
                audio_bus_buffer.numChannels = channel_count;
                audio_bus_buffer.__field0.channelBuffers32 = if self.input_channel_pointers.last().unwrap().is_empty() { 
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
                let mut bus_buffers = Vec::new();
                let mut channel_ptrs = Vec::new();
                
                for _ in 0..channel_count {
                    let mut buffer = vec![0.0f32; block_size as usize];
                    let ptr = buffer.as_mut_ptr();
                    bus_buffers.push(buffer);
                    channel_ptrs.push(ptr);
                }
                
                self.output_buffers.extend(bus_buffers);
                self.output_channel_pointers.push(channel_ptrs);
                
                // Create AudioBusBuffers
                let mut audio_bus_buffer: AudioBusBuffers = std::mem::zeroed();
                audio_bus_buffer.numChannels = channel_count;
                audio_bus_buffer.__field0.channelBuffers32 = if self.output_channel_pointers.last().unwrap().is_empty() { 
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
        
        Ok(())
    }
    
    /// Clears all audio buffers and event lists
    /// 
    /// # Safety
    /// This function is unsafe because it manipulates the internal state of COM interfaces
    /// through raw pointers.
    pub unsafe fn clear_buffers(&mut self) {
        // Clear input buffers
        for buffer in &mut self.input_buffers {
            buffer.fill(0.0);
        }
        
        // Clear output buffers  
        for buffer in &mut self.output_buffers {
            buffer.fill(0.0);
        }
        
        // Clear events
        if let Some(ref monitored_input) = self.monitored_input_events {
            monitored_input.clear();
        } else {
            self.input_events.events.lock().unwrap().clear();
        }
        
        if let Some(ref monitored_output) = self.monitored_output_events {
            monitored_output.clear();
        } else {
            self.output_events.events.lock().unwrap().clear();
        }
        
        // Make sure pointers are still valid
        if self.process_data.inputEvents.is_null() {
            println!("WARNING: inputEvents pointer is null!");
            self.process_data.inputEvents = self.input_events_ptr;
        }
        if self.process_data.outputEvents.is_null() {
            println!("WARNING: outputEvents pointer is null!");
            self.process_data.outputEvents = self.output_events_ptr;
        }
    }
}

impl Drop for HostProcessData {
    fn drop(&mut self) {
        unsafe {
            // Release the COM pointers we cloned
            if !self.input_events_ptr.is_null() {
                let ptr = ComPtr::<IEventList>::from_raw(self.input_events_ptr);
                drop(ptr);
            }
            if !self.output_events_ptr.is_null() {
                let ptr = ComPtr::<IEventList>::from_raw(self.output_events_ptr);
                drop(ptr);
            }
            if !self.input_param_changes_ptr.is_null() {
                let ptr = ComPtr::<IParameterChanges>::from_raw(self.input_param_changes_ptr);
                drop(ptr);
            }
            if !self.output_param_changes_ptr.is_null() {
                let ptr = ComPtr::<IParameterChanges>::from_raw(self.output_param_changes_ptr);
                drop(ptr);
            }
        }
        self.input_events_ptr = ptr::null_mut();
        self.output_events_ptr = ptr::null_mut();
        self.input_param_changes_ptr = ptr::null_mut();
        self.output_param_changes_ptr = ptr::null_mut();
    }
}