//! Internal VST3 plugin implementation

use crate::{
    audio::AudioBuffers,
    error::{Error, Result},
    midi::{MidiChannel, MidiEvent},
    parameters::Parameter,
    plugin::{PluginInfo, PluginInternal},
};
use std::ptr;
use vst3::Steinberg::Vst::BusDirections_::*;
use vst3::Steinberg::Vst::Event_::EventTypes_::*;
use vst3::Steinberg::Vst::MediaTypes_::*;
use vst3::Steinberg::{IPlugView, IPlugViewTrait};
use vst3::{ComPtr, ComWrapper, Interface, Steinberg::Vst::*, Steinberg::*};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use libloading::os::unix::{Library, Symbol};
#[cfg(target_os = "windows")]
use libloading::os::windows::{Library, Symbol};

use super::com_implementations::{
    create_event_list, ComponentHandler, HostEventList, ParameterChanges,
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

    // Library handle (kept alive)
    _library: Library,
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
    /// Load a VST3 plugin from the given path
    pub fn load(path: &std::path::Path, info: PluginInfo) -> Result<Self> {
        unsafe {
            // Load the library
            let library = Library::new(path)
                .map_err(|e| Error::PluginLoadFailed(format!("Failed to load library: {}", e)))?;

            // Get factory function
            type GetPluginFactoryFunc = unsafe extern "C" fn() -> *mut IPluginFactory;
            let get_factory: Symbol<GetPluginFactoryFunc> =
                library.get(b"GetPluginFactory\0").map_err(|e| {
                    Error::PluginLoadFailed(format!("Failed to find GetPluginFactory: {}", e))
                })?;

            let factory_ptr = get_factory();
            if factory_ptr.is_null() {
                return Err(Error::PluginLoadFailed(
                    "GetPluginFactory returned null".to_string(),
                ));
            }

            let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or_else(|| {
                Error::PluginLoadFailed("Failed to create factory ComPtr".to_string())
            })?;

            // Find and create the audio component
            let component = Self::create_component(&factory)?;

            // Initialize component
            component.initialize(ptr::null_mut());

            // Get processor interface
            let processor = component.cast::<IAudioProcessor>().ok_or_else(|| {
                Error::InterfaceError("Component does not implement IAudioProcessor".to_string())
            })?;

            // Try to get controller
            let controller = component.cast::<IEditController>();

            // Create event lists
            let input_events = create_event_list();
            let output_events = create_event_list();

            // Update has_gui based on actual capability
            let mut updated_info = info;
            updated_info.has_gui = controller.is_some() && {
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

            Ok(Self {
                component,
                processor,
                controller,
                info: updated_info,
                is_active: false,
                is_processing: false,
                sample_rate: 44100.0,
                block_size: 512,
                process_data: None,
                component_handler: None,
                input_events,
                output_events,
                plugin_view: None,
                _library: library,
            })
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
        let output_bus_count = self.component.getBusCount(kAudio as i32, kOutput as i32);

        // Prepare output buffers (most important for instruments)
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
                let audio_bus_buffer: AudioBusBuffers = std::mem::zeroed();
                data.output_bus_buffers.push(audio_bus_buffer);
            }
        }

        // We'll set up the actual pointers during process() to avoid storing raw pointers
        data.process_data.numInputs = 0;
        data.process_data.numOutputs = data.output_bus_buffers.len() as i32;

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
                // Clear events
                self.input_events.clear();
                self.output_events.clear();

                // Clear output buffers
                for buffer in &mut data.output_buffers {
                    buffer.fill(0.0);
                }

                // Create temporary pointer arrays for this process call
                let mut output_channel_ptrs: Vec<Vec<*mut f32>> = Vec::new();
                let mut channel_offset = 0;

                for bus in &data.output_bus_buffers {
                    let mut bus_ptrs = Vec::new();
                    for _ in 0..bus.numChannels {
                        if channel_offset < data.output_buffers.len() {
                            bus_ptrs.push(data.output_buffers[channel_offset].as_mut_ptr());
                            channel_offset += 1;
                        }
                    }
                    output_channel_ptrs.push(bus_ptrs);
                }

                // Update the bus buffer pointers
                for (i, bus) in data.output_bus_buffers.iter_mut().enumerate() {
                    if i < output_channel_ptrs.len() && !output_channel_ptrs[i].is_empty() {
                        bus.__field0.channelBuffers32 = output_channel_ptrs[i].as_mut_ptr();
                    }
                }

                // Update process data pointers
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
            // Activate component if needed
            if !self.is_active {
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
