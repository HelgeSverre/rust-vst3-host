//! Internal VST3 plugin implementation

use crate::{
    audio::AudioBuffers,
    error::{Error, Result},
    midi::{MidiChannel, MidiEvent},
    parameters::Parameter,
    plugin::{PluginInfo, PluginInternal},
};
use std::ptr;
use std::sync::{Arc, Mutex};
use vst3::Steinberg::Vst::BusDirections_::*;
use vst3::Steinberg::Vst::Event_::EventTypes_::*;
use vst3::Steinberg::Vst::MediaTypes_::*;
use vst3::Steinberg::{IPlugView, IPlugViewTrait};
use vst3::{ComPtr, ComWrapper, Interface, Steinberg::Vst::*, Steinberg::*};


use super::{
    com_implementations::{create_event_list, ComponentHandler, HostEventList, ParameterChanges},
    module_loader::{load_module, VstModule},
};

/// Internal plugin implementation that handles all VST3 COM interactions
pub struct PluginImpl {
    // Core VST3 interfaces
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,

    // Plugin metadata
    pub(crate) info: PluginInfo,

    // Processing state
    is_active: bool,
    is_processing: bool,
    sample_rate: f64,
    block_size: usize,

    // Host data structures
    process_data: Option<Box<HostProcessData>>,
    component_handler: Option<ComWrapper<ComponentHandler>>,

    // Event handling
    input_events: ComWrapper<HostEventList>,
    output_events: ComWrapper<HostEventList>,

    // Plugin view
    plugin_view: Option<ComPtr<IPlugView>>,

    // VST3 module handle (kept alive)
    _module: Box<dyn VstModule>,
}

// Processing data structure
struct HostProcessData {
    process_data: ProcessData,
    input_buffers: Vec<Vec<f32>>,
    output_buffers: Vec<Vec<f32>>,
    input_bus_buffers: Vec<AudioBusBuffers>,
    output_bus_buffers: Vec<AudioBusBuffers>,
    process_context: ProcessContext,
    input_param_changes: ComWrapper<ParameterChanges>,
    output_param_changes: ComWrapper<ParameterChanges>,
}

// We store the raw pointers separately as they're recreated each process call
struct AudioBufferPointers {
    input_channel_pointers: Vec<Vec<*mut f32>>,
    output_channel_pointers: Vec<Vec<*mut f32>>,
}

impl PluginImpl {
    /// Get parameter changes captured from the plugin GUI
    pub fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
        if let Some(ref handler) = self.component_handler {
            if let Ok(mut changes) = handler.parameter_changes.lock() {
                let result = changes.clone();
                changes.clear(); // Clear after reading
                result
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }

    /// Load a VST3 plugin from the given path
    pub fn load(path: &std::path::Path) -> Result<Self> {
        unsafe {
            log::info!("=== PLUGIN LOADING START ===");
            log::info!("Loading plugin from: {}", path.display());

            // Load the VST3 module using platform-specific loader
            log::debug!("Step 1: Loading VST3 module...");
            let module = load_module(path)?;
            log::debug!("✅ VST3 module loaded successfully");

            // Get factory from module
            log::debug!("Step 2: Getting factory from module...");
            let factory_ptr = module.get_factory()?;
            log::debug!("✅ Factory obtained, ptr: {:?}", factory_ptr);
            if factory_ptr.is_null() {
                return Err(Error::PluginLoadFailed(
                    "GetPluginFactory returned null".to_string(),
                ));
            }

            log::debug!("Step 3: Wrapping factory in ComPtr...");
            let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or_else(|| {
                Error::PluginLoadFailed("Failed to create factory ComPtr".to_string())
            })?;
            log::debug!("✅ Factory wrapped successfully");

            // Find and create the audio component
            log::debug!("Step 4: Creating audio component...");
            let component = Self::create_component(&factory)?;
            log::debug!("✅ Component created successfully");

            // Initialize component
            log::debug!("Step 5: Initializing component...");
            let init_result = component.initialize(ptr::null_mut());
            log::debug!("✅ Component initialized with result: {:#x}", init_result);

            // CRITICAL: Activate event buses for MIDI processing
            log::debug!("Step 6: Activating event buses...");
            Self::activate_event_buses(&component)?;
            log::debug!("✅ Event buses activated");

            // Get processor interface
            log::debug!("Step 7: Getting IAudioProcessor interface...");
            let processor = component.cast::<IAudioProcessor>().ok_or_else(|| {
                Error::InterfaceError("Component does not implement IAudioProcessor".to_string())
            })?;
            log::debug!("✅ IAudioProcessor interface obtained");

            // Create component handler for parameter change notifications
            log::debug!("Step 8: Creating component handler...");
            let parameter_changes = Arc::new(Mutex::new(Vec::new()));
            let component_handler =
                ComWrapper::new(ComponentHandler::new(parameter_changes.clone()));
            log::debug!("✅ Component handler created");

            // Get or create controller (handles both single-component and separate controller)
            log::debug!("Step 9: Getting or creating controller...");
            let controller = Self::get_or_create_controller(&component, &factory)?;
            log::debug!("✅ Controller obtained: {}", controller.is_some());

            // Connect component and controller if they are separate
            if let Some(ref ctrl) = controller {
                log::debug!("Step 10: Connecting component and controller...");
                Self::connect_component_and_controller(&component, ctrl)?;
                log::debug!("✅ Component and controller connected");

                // Set component handler on controller for parameter change notifications
                log::debug!("Step 11: Setting component handler on controller...");
                if let Some(handler_ptr) = component_handler.to_com_ptr::<IComponentHandler>() {
                    let result = ctrl.setComponentHandler(handler_ptr.into_raw());
                    if result == kResultOk {
                        log::debug!("✅ Component handler set on controller successfully");
                    } else {
                        log::warn!(
                            "Failed to set component handler on controller: {:#x}",
                            result
                        );
                    }
                } else {
                    log::error!("Failed to get IComponentHandler COM pointer");
                }

                // TEMPORARILY DISABLED: Transfer component state to controller
                // This was causing hangs with some plugins like Dexed
                // Self::transfer_component_state(&component, ctrl)?;
                log::debug!("State transfer temporarily disabled to prevent hangs");
            }

            // Activate component (important for parameter access)
            log::debug!("Step 12: Activating component...");
            let activate_result = component.setActive(1);
            log::debug!("Component activation result: {:#x}", activate_result);
            let is_active = if activate_result == kResultOk {
                log::debug!("Component activated successfully during initialization");
                true
            } else {
                log::warn!(
                    "Component activation failed during initialization: {:#x}",
                    activate_result
                );
                false
            };

            // Create event lists
            log::debug!("Step 13: Creating event lists...");
            let input_events = create_event_list();
            let output_events = create_event_list();
            log::debug!("✅ Event lists created");

            // Extract plugin info from the factory and component
            let info = Self::extract_plugin_info(path, &factory, &component, &controller)?;

            let has_gui = controller.is_some() && {
                if let Some(ref ctrl) = controller {
                    unsafe {
                        let view_type = b"editor\0".as_ptr() as *const i8;
                        let view_ptr = ctrl.createView(view_type);
                        if !view_ptr.is_null() {
                            // Clean up the test view immediately
                            let view = ComPtr::<IPlugView>::from_raw(view_ptr).unwrap();
                            view.removed();
                            true
                        } else {
                            false
                        }
                    }
                } else {
                    false
                }
            };

            let mut updated_info = info;
            updated_info.has_gui = has_gui;

            log::info!("=== PLUGIN LOADING COMPLETE ===");
            log::info!(
                "Plugin info: {} by {}",
                updated_info.name,
                updated_info.vendor
            );
            log::info!("Has GUI: {}, Active: {}", updated_info.has_gui, is_active);

            Ok(Self {
                component,
                processor,
                controller,
                info: updated_info,
                is_active,
                is_processing: false,
                sample_rate: 44100.0,
                block_size: 512,
                process_data: None,
                component_handler: Some(component_handler),
                input_events,
                output_events,
                plugin_view: None,
                _module: module,
            })
        }
    }

    /// Extract plugin info from factory and component
    fn extract_plugin_info(
        path: &std::path::Path,
        factory: &ComPtr<IPluginFactory>,
        component: &ComPtr<IComponent>,
        _controller: &Option<ComPtr<IEditController>>,
    ) -> Result<PluginInfo> {
        unsafe {
            // Get factory info
            let mut factory_info: PFactoryInfo = std::mem::zeroed();
            factory.getFactoryInfo(&mut factory_info);
            let vendor = crate::internal::utils::c_str_to_string(&factory_info.vendor);

            // Find audio component class info
            let num_classes = factory.countClasses();
            for i in 0..num_classes {
                let mut class_info: PClassInfo = std::mem::zeroed();
                if factory.getClassInfo(i, &mut class_info) == kResultOk {
                    let category = crate::internal::utils::c_str_to_string(&class_info.category);
                    
                    if category.contains("Audio Module Class") {
                        let name = crate::internal::utils::c_str_to_string(&class_info.name);
                        let cid = class_info.cid;
                        let uid = format!("{:08X}{:08X}{:08X}{:08X}", 
                            u32::from_be_bytes([cid[0] as u8, cid[1] as u8, cid[2] as u8, cid[3] as u8]),
                            u32::from_be_bytes([cid[4] as u8, cid[5] as u8, cid[6] as u8, cid[7] as u8]),
                            u32::from_be_bytes([cid[8] as u8, cid[9] as u8, cid[10] as u8, cid[11] as u8]),
                            u32::from_be_bytes([cid[12] as u8, cid[13] as u8, cid[14] as u8, cid[15] as u8])
                        );

                        // Count audio buses
                        let audio_inputs = component.getBusCount(kAudio as i32, kInput as i32) as u32;
                        let audio_outputs = component.getBusCount(kAudio as i32, kOutput as i32) as u32;
                        
                        return Ok(PluginInfo {
                            path: path.to_path_buf(),
                            name,
                            vendor,
                            version: "1.0.0".to_string(), // Default version
                            category: "Audio Effect".to_string(), // Default, could be refined
                            uid,
                            audio_inputs,
                            audio_outputs,
                            has_gui: false, // Will be updated by caller
                            has_midi_input: true, // Default - could be refined
                            has_midi_output: false, // Default - could be refined
                        });
                    }
                }
            }
            
            Err(Error::PluginLoadFailed("No audio component found".to_string()))
        }
    }

    /// Find and create the audio component from the factory
    unsafe fn create_component(factory: &ComPtr<IPluginFactory>) -> Result<ComPtr<IComponent>> {
        let num_classes = factory.countClasses();

        for i in 0..num_classes {
            let mut class_info = std::mem::zeroed();
            if factory.getClassInfo(i, &mut class_info) == kResultOk {
                let category = crate::internal::utils::c_str_to_string(&class_info.category);

                if category.contains("Audio Module Class") {
                    let mut component_ptr: *mut IComponent = ptr::null_mut();

                    let result = factory.createInstance(
                        class_info.cid.as_ptr() as *const i8,
                        IComponent::IID.as_ptr() as *const i8,
                        &mut component_ptr as *mut _ as *mut _,
                    );

                    if result == kResultOk && !component_ptr.is_null() {
                        return ComPtr::from_raw(component_ptr).ok_or_else(|| {
                            Error::PluginLoadFailed("Failed to create component".to_string())
                        });
                    }
                }
            }
        }

        Err(Error::PluginLoadFailed(
            "No audio component found in plugin".to_string(),
        ))
    }

    /// Set up processing with current configuration
    fn setup_processing(&mut self) -> Result<()> {
        unsafe {
            // Set up processing
            let setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: self.block_size as i32,
                sampleRate: self.sample_rate,
            };

            let result = self.processor.setupProcessing(&setup as *const _ as *mut _);
            if result != kResultOk {
                return Err(Error::InterfaceError(format!(
                    "Failed to setup processing: {:#x}",
                    result
                )));
            }

            // Create process data
            self.create_process_data()?;

            Ok(())
        }
    }

    /// Create processing data structures
    fn create_process_data(&mut self) -> Result<()> {
        unsafe {
            let mut data = Box::new(HostProcessData {
                process_data: std::mem::zeroed(),
                input_buffers: Vec::new(),
                output_buffers: Vec::new(),
                input_bus_buffers: Vec::new(),
                output_bus_buffers: Vec::new(),
                process_context: std::mem::zeroed(),
                input_param_changes: ComWrapper::new(ParameterChanges::default()),
                output_param_changes: ComWrapper::new(ParameterChanges::default()),
            });

            // Initialize process context
            data.process_context.sampleRate = self.sample_rate;
            data.process_context.tempo = 120.0;
            data.process_context.timeSigNumerator = 4;
            data.process_context.timeSigDenominator = 4;
            data.process_context.state = ProcessContext_::StatesAndFlags_::kPlaying
                | ProcessContext_::StatesAndFlags_::kTempoValid
                | ProcessContext_::StatesAndFlags_::kTimeSigValid;

            // Set up process data
            data.process_data.processMode = ProcessModes_::kRealtime as i32;
            data.process_data.numSamples = self.block_size as i32;
            data.process_data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
            data.process_data.processContext = &mut data.process_context;

            // Set up event lists
            data.process_data.inputEvents = self
                .input_events
                .to_com_ptr::<IEventList>()
                .map(|ptr| ptr.into_raw())
                .unwrap_or(ptr::null_mut());
            data.process_data.outputEvents = self
                .output_events
                .to_com_ptr::<IEventList>()
                .map(|ptr| ptr.into_raw())
                .unwrap_or(ptr::null_mut());

            // Set up parameter changes
            data.process_data.inputParameterChanges = data
                .input_param_changes
                .to_com_ptr::<IParameterChanges>()
                .map(|ptr| ptr.into_raw())
                .unwrap_or(ptr::null_mut());
            data.process_data.outputParameterChanges = data
                .output_param_changes
                .to_com_ptr::<IParameterChanges>()
                .map(|ptr| ptr.into_raw())
                .unwrap_or(ptr::null_mut());

            // Prepare buffers
            self.prepare_buffers(&mut data)?;

            self.process_data = Some(data);
            Ok(())
        }
    }

    /// Prepare audio buffers based on plugin bus configuration
    unsafe fn prepare_buffers(&mut self, data: &mut HostProcessData) -> Result<()> {
        // Get bus counts
        let input_bus_count = self.component.getBusCount(kAudio as i32, kInput as i32);
        let output_bus_count = self.component.getBusCount(kAudio as i32, kOutput as i32);

        // Clear existing buffers
        data.input_buffers.clear();
        data.output_buffers.clear();
        data.input_bus_buffers.clear();
        data.output_bus_buffers.clear();

        // Prepare input buffers
        for bus_idx in 0..input_bus_count {
            let mut bus_info: BusInfo = std::mem::zeroed();
            if self
                .component
                .getBusInfo(kAudio as i32, kInput as i32, bus_idx, &mut bus_info)
                == kResultOk
            {
                let channel_count = bus_info.channelCount;

                // Activate the bus
                self.component
                    .activateBus(kAudio as i32, kInput as i32, bus_idx, 1);

                // Create buffers for this bus
                for _ in 0..channel_count {
                    let buffer = vec![0.0f32; self.block_size];
                    data.input_buffers.push(buffer);
                }

                // Create AudioBusBuffers struct
                let mut audio_bus_buffer: AudioBusBuffers = std::mem::zeroed();
                audio_bus_buffer.numChannels = channel_count;
                data.input_bus_buffers.push(audio_bus_buffer);
            }
        }

        // Prepare output buffers
        for bus_idx in 0..output_bus_count {
            let mut bus_info: BusInfo = std::mem::zeroed();
            if self
                .component
                .getBusInfo(kAudio as i32, kOutput as i32, bus_idx, &mut bus_info)
                == kResultOk
            {
                let channel_count = bus_info.channelCount;

                // Activate the bus
                self.component
                    .activateBus(kAudio as i32, kOutput as i32, bus_idx, 1);

                // Create buffers for this bus
                for _ in 0..channel_count {
                    let buffer = vec![0.0f32; self.block_size];
                    data.output_buffers.push(buffer);
                }

                // Create AudioBusBuffers struct
                let mut audio_bus_buffer: AudioBusBuffers = std::mem::zeroed();
                audio_bus_buffer.numChannels = channel_count;
                data.output_bus_buffers.push(audio_bus_buffer);
            }
        }

        // Set up process data counts
        data.process_data.numInputs = data.input_bus_buffers.len() as i32;
        data.process_data.numOutputs = data.output_bus_buffers.len() as i32;

        log::debug!(
            "Prepared buffers: {} input buses, {} output buses, {} input channels, {} output channels",
            input_bus_count,
            output_bus_count,
            data.input_buffers.len(),
            data.output_buffers.len()
        );

        Ok(())
    }
}

impl PluginInternal for PluginImpl {
    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        if let Some(ref controller) = self.controller {
            unsafe {
                controller.setParamNormalized(id, value);
            }
            Ok(())
        } else {
            Err(Error::InterfaceError("No controller available".to_string()))
        }
    }

    fn get_parameter(&self, id: u32) -> Result<f64> {
        if let Some(ref controller) = self.controller {
            unsafe { Ok(controller.getParamNormalized(id)) }
        } else {
            Err(Error::InterfaceError("No controller available".to_string()))
        }
    }

    fn get_all_parameters(&self) -> Result<Vec<Parameter>> {
        let mut params = Vec::new();

        if let Some(ref controller) = self.controller {
            unsafe {
                let count = controller.getParameterCount();

                for i in 0..count {
                    let mut info: ParameterInfo = std::mem::zeroed();
                    if controller.getParameterInfo(i, &mut info) == kResultOk {
                        let param = Parameter {
                            id: info.id,
                            name: crate::internal::utils::vst_string_to_string(&info.title),
                            value: controller.getParamNormalized(info.id),
                            min: 0.0,
                            max: 1.0,
                            default: info.defaultNormalizedValue,
                            unit: crate::internal::utils::vst_string_to_string(&info.units),
                            step_count: info.stepCount,
                            can_automate: (info.flags
                                & ParameterInfo_::ParameterFlags_::kCanAutomate as i32)
                                != 0,
                            is_read_only: (info.flags
                                & ParameterInfo_::ParameterFlags_::kIsReadOnly as i32)
                                != 0,
                            is_bypass: (info.flags
                                & ParameterInfo_::ParameterFlags_::kIsBypass as i32)
                                != 0,
                            flags: info.flags as u32,
                        };
                        params.push(param);
                    }
                }
            }
        }

        Ok(params)
    }

    fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        if !self.is_active || !self.is_processing {
            return Err(Error::Other("Plugin is not processing".to_string()));
        }

        if let Some(ref mut data) = self.process_data {
            unsafe {
                // Clear output events only - input events should be preserved for processing
                self.output_events.clear();

                // Copy input audio to plugin buffers
                for (ch_idx, channel) in buffers.inputs.iter().enumerate() {
                    if ch_idx < data.input_buffers.len() {
                        data.input_buffers[ch_idx].copy_from_slice(channel);
                    }
                }

                // Clear output buffers
                for buffer in &mut data.output_buffers {
                    buffer.fill(0.0);
                }

                // Set up input buffer pointers
                let mut input_channel_ptrs: Vec<Vec<*mut f32>> = Vec::new();
                let mut input_channel_offset = 0;

                for bus in &data.input_bus_buffers {
                    let mut bus_ptrs = Vec::new();
                    for _ in 0..bus.numChannels {
                        if input_channel_offset < data.input_buffers.len() {
                            bus_ptrs.push(data.input_buffers[input_channel_offset].as_mut_ptr());
                            input_channel_offset += 1;
                        }
                    }
                    input_channel_ptrs.push(bus_ptrs);
                }

                // Set up output buffer pointers
                let mut output_channel_ptrs: Vec<Vec<*mut f32>> = Vec::new();
                let mut output_channel_offset = 0;

                for bus in &data.output_bus_buffers {
                    let mut bus_ptrs = Vec::new();
                    for _ in 0..bus.numChannels {
                        if output_channel_offset < data.output_buffers.len() {
                            bus_ptrs.push(data.output_buffers[output_channel_offset].as_mut_ptr());
                            output_channel_offset += 1;
                        }
                    }
                    output_channel_ptrs.push(bus_ptrs);
                }

                // Update input bus buffer pointers
                for (i, bus) in data.input_bus_buffers.iter_mut().enumerate() {
                    if i < input_channel_ptrs.len() && !input_channel_ptrs[i].is_empty() {
                        bus.__field0.channelBuffers32 = input_channel_ptrs[i].as_mut_ptr();
                    }
                }

                // Update output bus buffer pointers
                for (i, bus) in data.output_bus_buffers.iter_mut().enumerate() {
                    if i < output_channel_ptrs.len() && !output_channel_ptrs[i].is_empty() {
                        bus.__field0.channelBuffers32 = output_channel_ptrs[i].as_mut_ptr();
                    }
                }

                // Update process data pointers
                data.process_data.inputs = if data.input_bus_buffers.is_empty() {
                    ptr::null_mut()
                } else {
                    data.input_bus_buffers.as_mut_ptr()
                };

                data.process_data.outputs = if data.output_bus_buffers.is_empty() {
                    ptr::null_mut()
                } else {
                    data.output_bus_buffers.as_mut_ptr()
                };

                // Process
                let result = self.processor.process(&mut data.process_data);
                if result != kResultOk {
                    return Err(Error::Other(format!("Process failed: {:#x}", result)));
                }

                // Clear input events AFTER processing so plugin can see them
                self.input_events.clear();

                // Copy output to provided buffers
                for (ch_idx, channel) in buffers.outputs.iter_mut().enumerate() {
                    if ch_idx < data.output_buffers.len() {
                        channel.copy_from_slice(&data.output_buffers[ch_idx]);
                    }
                }

                Ok(())
            }
        } else {
            Err(Error::Other("Process data not initialized".to_string()))
        }
    }

    fn send_midi_event(&mut self, event: MidiEvent) -> Result<()> {
        unsafe {
            let mut vst_event: Event = std::mem::zeroed();
            vst_event.busIndex = 0;
            vst_event.sampleOffset = 0;
            vst_event.ppqPosition = 0.0;
            vst_event.flags = Event_::EventFlags_::kIsLive as u16;

            match event {
                MidiEvent::NoteOn {
                    channel,
                    note,
                    velocity,
                } => {
                    vst_event.r#type = kNoteOnEvent as u16;
                    vst_event.__field0.noteOn.channel = channel.as_index() as i16;
                    vst_event.__field0.noteOn.pitch = note as i16;
                    vst_event.__field0.noteOn.tuning = 0.0;
                    vst_event.__field0.noteOn.velocity = velocity as f32 / 127.0;
                    vst_event.__field0.noteOn.length = 0;
                    vst_event.__field0.noteOn.noteId = -1;
                }
                MidiEvent::NoteOff {
                    channel,
                    note,
                    velocity,
                } => {
                    vst_event.r#type = kNoteOffEvent as u16;
                    vst_event.__field0.noteOff.channel = channel.as_index() as i16;
                    vst_event.__field0.noteOff.pitch = note as i16;
                    vst_event.__field0.noteOff.velocity = velocity as f32 / 127.0;
                    vst_event.__field0.noteOff.noteId = -1;
                    vst_event.__field0.noteOff.tuning = 0.0;
                }
                MidiEvent::ControlChange {
                    channel,
                    controller,
                    value,
                } => {
                    // For now, convert to legacy MIDI
                    // In the future, could use PolyPressureEvent for some CCs
                    return self.send_legacy_midi_cc(channel, controller, value);
                }
                _ => {
                    // Other events not yet implemented
                    return Err(Error::MidiError(
                        "MIDI event type not yet implemented".to_string(),
                    ));
                }
            }

            self.input_events.add_event(vst_event);
        }
        Ok(())
    }

    fn start_processing(&mut self) -> Result<()> {
        unsafe {
            // Component should already be activated during initialization
            // But activate it if somehow it's not active
            if !self.is_active {
                log::warn!("Component not active, attempting to activate");
                let result = self.component.setActive(1);
                if result != kResultOk {
                    return Err(Error::Other(format!("Failed to activate: {:#x}", result)));
                }
                self.is_active = true;
            }

            // Setup processing
            self.setup_processing()?;

            // Start processing
            let result = self.processor.setProcessing(1);
            if result != kResultOk {
                return Err(Error::Other(format!(
                    "Failed to start processing: {:#x}",
                    result
                )));
            }

            self.is_processing = true;
            log::debug!("Plugin processing started successfully");
            Ok(())
        }
    }

    fn stop_processing(&mut self) -> Result<()> {
        unsafe {
            if self.is_processing {
                self.processor.setProcessing(0);
                self.is_processing = false;
            }

            if self.is_active {
                self.component.setActive(0);
                self.is_active = false;
            }

            Ok(())
        }
    }

    fn has_editor(&self) -> bool {
        // First check our cached value
        if self.info.has_gui {
            return true;
        }

        // Otherwise do a runtime check
        if let Some(ref controller) = self.controller {
            unsafe {
                // Check if controller can create an editor view
                let view_type = b"editor\0".as_ptr() as *const i8;
                let view_ptr = controller.createView(view_type);
                if !view_ptr.is_null() {
                    // Clean up the test view
                    let view = ComPtr::<IPlugView>::from_raw(view_ptr).unwrap();
                    view.removed();
                    true
                } else {
                    false
                }
            }
        } else {
            false
        }
    }

    fn open_editor(&mut self, parent: *mut std::ffi::c_void) -> Result<()> {
        if self.plugin_view.is_some() {
            return Err(Error::Other("Editor already open".to_string()));
        }

        if let Some(ref controller) = self.controller {
            unsafe {
                // Create editor view
                let view_type = b"editor\0".as_ptr() as *const i8;
                let view_ptr = controller.createView(view_type);
                if view_ptr.is_null() {
                    return Err(Error::Other("Failed to create editor view".to_string()));
                }

                let view = ComPtr::<IPlugView>::from_raw(view_ptr)
                    .ok_or_else(|| Error::Other("Failed to wrap view".to_string()))?;

                // Get view size
                let mut view_rect = ViewRect {
                    left: 0,
                    top: 0,
                    right: 400,
                    bottom: 300,
                };

                if view.getSize(&mut view_rect) != kResultOk {
                    return Err(Error::Other("Failed to get view size".to_string()));
                }

                // Platform-specific attachment
                #[cfg(target_os = "macos")]
                let platform_type = b"NSView\0".as_ptr() as *const i8;
                #[cfg(target_os = "windows")]
                let platform_type = b"HWND\0".as_ptr() as *const i8;
                #[cfg(target_os = "linux")]
                let platform_type = b"X11EmbedWindowID\0".as_ptr() as *const i8;

                // Check platform support
                if view.isPlatformTypeSupported(platform_type) != kResultOk {
                    return Err(Error::Other("Platform type not supported".to_string()));
                }

                // Attach to parent window
                let attach_result = view.attached(parent, platform_type);
                if attach_result != kResultOk {
                    return Err(Error::Other(format!(
                        "Failed to attach view: {:#x}",
                        attach_result
                    )));
                }

                self.plugin_view = Some(view);
                Ok(())
            }
        } else {
            Err(Error::Other("No controller available".to_string()))
        }
    }

    fn close_editor(&mut self) -> Result<()> {
        if let Some(view) = self.plugin_view.take() {
            unsafe {
                view.removed();
            }
        }
        Ok(())
    }

    fn get_editor_size(&self) -> Result<(i32, i32)> {
        if let Some(ref controller) = self.controller {
            unsafe {
                // Create a temporary view to get size
                let view_type = b"editor\0".as_ptr() as *const i8;
                let view_ptr = controller.createView(view_type);
                if view_ptr.is_null() {
                    return Err(Error::Other(
                        "Failed to create view for size query".to_string(),
                    ));
                }

                let view = ComPtr::<IPlugView>::from_raw(view_ptr).unwrap();

                // Get view size
                let mut view_rect = ViewRect {
                    left: 0,
                    top: 0,
                    right: 400,
                    bottom: 300,
                };

                let result = view.getSize(&mut view_rect);

                // Clean up the temporary view
                view.removed();

                if result == kResultOk {
                    let width = view_rect.right - view_rect.left;
                    let height = view_rect.bottom - view_rect.top;
                    Ok((width, height))
                } else {
                    Ok((800, 600)) // Default size
                }
            }
        } else {
            Err(Error::Other("No controller available".to_string()))
        }
    }

    fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
        self.get_parameter_changes()
    }
}

impl PluginImpl {
    /// Send a legacy MIDI CC event
    fn send_legacy_midi_cc(
        &mut self,
        channel: MidiChannel,
        controller: u8,
        value: u8,
    ) -> Result<()> {
        let event = unsafe {
            let mut event: Event = std::mem::zeroed();
            event.busIndex = 0;
            event.sampleOffset = 0;
            event.ppqPosition = 0.0;
            event.flags = Event_::EventFlags_::kIsLive as u16;
            event.r#type = kLegacyMIDICCOutEvent as u16;

            event.__field0.midiCCOut.controlNumber = controller;
            event.__field0.midiCCOut.channel = channel.as_index() as i8;
            event.__field0.midiCCOut.value = value as i8;
            event.__field0.midiCCOut.value2 = 0;
            event
        };

        self.input_events.add_event(event);
        Ok(())
    }

    /// Activate all event buses for MIDI processing
    unsafe fn activate_event_buses(component: &ComPtr<IComponent>) -> Result<()> {
        // Activate event input buses
        let event_input_count = component.getBusCount(kEvent as i32, kInput as i32);
        for i in 0..event_input_count {
            let mut bus_info = std::mem::zeroed();
            let info_result = component.getBusInfo(kEvent as i32, kInput as i32, i, &mut bus_info);
            let name = if info_result == kResultOk {
                crate::internal::utils::vst_string_to_string(&bus_info.name)
            } else {
                format!("#{}", i)
            };

            let activate_result = component.activateBus(kEvent as i32, kInput as i32, i, 1);
            log::debug!(
                "Event Input Bus {} (index {}): activation result = {:#x}",
                name,
                i,
                activate_result
            );
        }

        // Activate event output buses
        let event_output_count = component.getBusCount(kEvent as i32, kOutput as i32);
        for i in 0..event_output_count {
            let mut bus_info = std::mem::zeroed();
            let info_result = component.getBusInfo(kEvent as i32, kOutput as i32, i, &mut bus_info);
            let name = if info_result == kResultOk {
                crate::internal::utils::vst_string_to_string(&bus_info.name)
            } else {
                format!("#{}", i)
            };

            let activate_result = component.activateBus(kEvent as i32, kOutput as i32, i, 1);
            log::debug!(
                "Event Output Bus {} (index {}): activation result = {:#x}",
                name,
                i,
                activate_result
            );
        }

        Ok(())
    }

    /// Get or create controller (handles both single-component and separate controller)
    unsafe fn get_or_create_controller(
        component: &ComPtr<IComponent>,
        factory: &ComPtr<IPluginFactory>,
    ) -> Result<Option<ComPtr<IEditController>>> {
        // First, try to cast component to IEditController (single component)
        if let Some(controller) = component.cast::<IEditController>() {
            log::debug!("Component implements IEditController (single component)");
            return Ok(Some(controller));
        }

        // If not single component, try to get separate controller
        log::debug!("Component is separate from controller, getting controller class ID...");
        let mut controller_cid = [0i8; 16];
        let result = component.getControllerClassId(&mut controller_cid);

        if result != kResultOk {
            log::warn!("Failed to get controller class ID: {:#x}", result);
            return Ok(None);
        }

        log::debug!("Got controller class ID, creating controller...");
        let mut controller_ptr: *mut IEditController = ptr::null_mut();
        let create_result = factory.createInstance(
            controller_cid.as_ptr(),
            IEditController::IID.as_ptr() as *const i8,
            &mut controller_ptr as *mut _ as *mut _,
        );

        if create_result != kResultOk || controller_ptr.is_null() {
            log::warn!(
                "Failed to create controller: {:#x}, ptr is null: {}",
                create_result,
                controller_ptr.is_null()
            );
            return Ok(None);
        }

        let controller = ComPtr::<IEditController>::from_raw(controller_ptr)
            .ok_or_else(|| Error::InterfaceError("Failed to wrap controller".to_string()))?;

        // Initialize controller
        log::debug!("Initializing controller...");
        let init_result = controller.initialize(ptr::null_mut());
        if init_result != kResultOk {
            log::warn!("Failed to initialize controller: {:#x}", init_result);
            return Ok(None);
        }

        log::debug!("Controller created and initialized successfully");
        Ok(Some(controller))
    }

    /// Connect component and controller via IConnectionPoint
    unsafe fn connect_component_and_controller(
        component: &ComPtr<IComponent>,
        controller: &ComPtr<IEditController>,
    ) -> Result<()> {
        // Try to get connection points
        let comp_cp = component.cast::<IConnectionPoint>();
        let ctrl_cp = controller.cast::<IConnectionPoint>();

        if let (Some(comp_cp), Some(ctrl_cp)) = (comp_cp, ctrl_cp) {
            // Connect component to controller
            let result1 = comp_cp.connect(ctrl_cp.as_ptr());
            let result2 = ctrl_cp.connect(comp_cp.as_ptr());

            if result1 == kResultOk && result2 == kResultOk {
                log::debug!("Components connected successfully");
                Ok(())
            } else {
                log::warn!(
                    "Component connection failed: comp->ctrl={:#x}, ctrl->comp={:#x}",
                    result1,
                    result2
                );
                Err(Error::InterfaceError(
                    "Failed to connect components".to_string(),
                ))
            }
        } else {
            log::debug!("Components do not support IConnectionPoint - might be single component");
            Ok(()) // Not an error - single components don't need connection
        }
    }

    /// Transfer component state to controller
    unsafe fn transfer_component_state(
        component: &ComPtr<IComponent>,
        controller: &ComPtr<IEditController>,
    ) -> Result<()> {
        // Get state from component
        let mut state_ptr: *mut vst3::Steinberg::IBStream = ptr::null_mut();

        // First check if component supports state saving
        let save_result = component.getState(&mut state_ptr as *mut _ as *mut _);

        if save_result != kResultOk || state_ptr.is_null() {
            log::debug!("Component does not provide state or state is empty");
            return Ok(()); // Not an error - some plugins don't have state
        }

        // Wrap the state stream
        let state_stream = ComPtr::<vst3::Steinberg::IBStream>::from_raw(state_ptr)
            .ok_or_else(|| Error::InterfaceError("Failed to wrap state stream".to_string()))?;

        // Set state on controller
        let set_result = controller.setComponentState(state_stream.as_ptr());

        if set_result == kResultOk {
            log::debug!("Component state transferred to controller successfully");
        } else {
            log::warn!(
                "Failed to set component state on controller: {:#x}",
                set_result
            );
        }

        Ok(())
    }
}

impl Drop for PluginImpl {
    fn drop(&mut self) {
        // Ensure clean shutdown
        let _ = self.stop_processing();

        unsafe {
            // Terminate component
            if let Some(ref controller) = self.controller {
                controller.terminate();
            }
            self.component.terminate();
        }
    }
}
