use std::ptr;
use vst3::{ComPtr, ComWrapper, Steinberg::*, Steinberg::Vst::*};
use crate::com_implementations::{MyEventList, ParameterChanges, create_event_list};

pub struct HostProcessData {
    pub process_data: ProcessData,
    pub input_buffers: Vec<Vec<f32>>,
    pub output_buffers: Vec<Vec<f32>>,
    pub input_bus_buffers: Vec<AudioBusBuffers>,
    pub output_bus_buffers: Vec<AudioBusBuffers>,
    pub input_channel_pointers: Vec<Vec<*mut f32>>,
    pub output_channel_pointers: Vec<Vec<*mut f32>>,
    pub process_context: ProcessContext,
    pub input_events: ComWrapper<MyEventList>,
    pub output_events: ComWrapper<MyEventList>,
    pub input_events_ptr: *mut IEventList,
    pub output_events_ptr: *mut IEventList,
    pub input_param_changes: ComWrapper<ParameterChanges>,
    pub output_param_changes: ComWrapper<ParameterChanges>,
    pub input_param_changes_ptr: *mut IParameterChanges,
    pub output_param_changes_ptr: *mut IParameterChanges,
}

impl HostProcessData {
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
        data.process_context.state = ProcessContext_::StatesAndFlags_::kPlaying as u32 |
                                     ProcessContext_::StatesAndFlags_::kTempoValid as u32 |
                                     ProcessContext_::StatesAndFlags_::kTimeSigValid as u32 |
                                     ProcessContext_::StatesAndFlags_::kContTimeValid as u32 |
                                     ProcessContext_::StatesAndFlags_::kSystemTimeValid as u32;
        
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
    
    pub unsafe fn prepare_buffers(&mut self, component: &ComPtr<IComponent>, block_size: i32) -> Result<(), String> {
        use BusDirections_::*;
        use MediaTypes_::*;
        
        // Get bus counts
        let input_bus_count = component.getBusCount(kAudio as i32, kInput as i32);
        let output_bus_count = component.getBusCount(kAudio as i32, kOutput as i32);
        
        println!("🎵 Preparing buffers: {} input buses, {} output buses", input_bus_count, output_bus_count);
        
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
        self.input_events.events.borrow_mut().clear();
        self.output_events.events.borrow_mut().clear();
        
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