//! Internal VST3 plugin implementation

use crate::{
    audio::{AudioBuffers, AudioBusBuffer, AudioBusConfig, AudioBusLayout, BusAudioBuffers},
    error::{Error, Result},
    midi::{MidiChannel, MidiEvent, PluginEvent},
    parameters::{Parameter, ParameterChange},
    plugin::{
        decode_state_snapshot, encode_state_snapshot, PluginInfo, PluginInternal, StateContext,
        StateSnapshot,
    },
};
use crossbeam_queue::ArrayQueue;
use std::ptr;
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, ThreadId};
use vst3::Steinberg::Vst::BusDirections_::*;
use vst3::Steinberg::Vst::Event_::EventTypes_::*;
use vst3::Steinberg::Vst::MediaTypes_::*;
use vst3::Steinberg::{
    IPlugView, IPlugViewContentScaleSupport, IPlugViewContentScaleSupportTrait, IPlugViewTrait,
};
use vst3::{ComPtr, ComWrapper, Interface, Steinberg::Vst::*, Steinberg::*};

#[cfg(target_os = "linux")]
use super::com_implementations::RunLoopRegistry;
use super::{
    com_implementations::{
        create_event_list, create_host_application, create_host_plug_frame,
        create_memory_stream_from_with_metadata, create_memory_stream_with_metadata,
        create_state_restore_stream, ComponentHandler, ConnectionPair, HostApplication,
        HostEventList, HostPlugFrame, ParameterChanges, StreamStateType, MAX_EDITOR_FEEDBACK,
        MAX_QUEUED_EVENTS,
    },
    module_loader::{load_module, VstModule},
};

/// Cap on the buffered output MIDI a plugin emits, so a host that never polls can't grow it
/// forever. The buffer is pre-reserved to this size so steady-state pushes never reallocate.
const MAX_OUTPUT_MIDI: usize = 4096;

/// Cap on simultaneously tracked `note_on` ids (see `PluginImpl::active_notes`). Far above any
/// real polyphony, and bounds a caller that starts notes it never releases.
const MAX_TRACKED_NOTES: usize = 1024;

/// Cap on parameter changes queued for the next block (see `PluginImpl::pending_param_changes`).
/// The queue's only drain is `process()`, which returns early while the plugin isn't processing —
/// so a host whose stream keeps running after `stop_processing`, or one automating parameters
/// before playback starts, would otherwise grow it forever and then flood a single block with the
/// whole backlog. The sibling of `MAX_QUEUED_EVENTS` on the MIDI side: pre-reserved so the audio
/// path never reallocates, and changes past the cap are dropped (and counted).
const MAX_PENDING_PARAM_CHANGES: usize = 4096;
const MIDI_CONTROLLER_COUNT: usize = ControllerNumbers_::kCtrlProgramChange as usize + 1;
const MIDI_CHANNEL_COUNT: usize = 16;
const MAX_OUTPUT_PARAMETER_FEEDBACK: usize = 4096;

/// Cap on the event-input buses consulted when building the MIDI-mapping table. That table is
/// dense — `buses × MIDI_CHANNEL_COUNT × MIDI_CONTROLLER_COUNT` entries — so the bus count a
/// plugin reports is a multiplier on a host allocation and on a burst of controller COM calls.
/// One event input is the norm; 32 is far past any real plugin, and mirrors the `MAX_*` caps on
/// every other plugin-sized host buffer.
const MAX_MIDI_MAPPING_BUSES: usize = 32;

/// Cap on parameter values waiting to be mirrored into `IEditController` (see
/// `PluginImpl::deferred_controller_sync`). Bounded so a host that never services the plugin
/// cannot grow it; when full the oldest value is dropped, because the newest value for a
/// parameter is the one worth applying.
const MAX_DEFERRED_CONTROLLER_SYNC: usize = 4096;

#[derive(Default)]
struct MidiMappingCache {
    buses: usize,
    assignments: Vec<Option<u32>>,
    mapped: Vec<crate::midi::MidiControllerAssignment>,
    output: Vec<crate::midi::MidiControllerAssignment>,
}

/// Which controller-derived caches a `restartComponent` or host notification left stale.
///
/// Rebuilding either one is a burst of `IEditController` calls, which belong to the main-thread
/// domain — but notifications arrive on whichever thread the plugin chose. So an off-control
/// thread only records what went stale here, and the next control-thread entry point rebuilds.
#[derive(Default, Clone, Copy)]
struct DirtyCaches {
    midi_mapping: bool,
    program_change: bool,
}

impl MidiMappingCache {
    fn index(&self, bus: i32, channel: i16, controller: u16) -> Option<usize> {
        let bus = usize::try_from(bus).ok()?;
        let channel = usize::try_from(channel).ok()?;
        let controller = usize::from(controller);
        (bus < self.buses && channel < MIDI_CHANNEL_COUNT && controller < MIDI_CONTROLLER_COUNT)
            .then_some((bus * MIDI_CHANNEL_COUNT + channel) * MIDI_CONTROLLER_COUNT + controller)
    }

    fn get(&self, bus: i32, channel: i16, controller: u16) -> Option<u32> {
        self.index(bus, channel, controller)
            .and_then(|index| self.assignments[index])
    }
}

/// How many event-input buses the MIDI-mapping table covers, given the count a plugin reports.
/// Negative counts read as none, and the total is capped at [`MAX_MIDI_MAPPING_BUSES`].
fn midi_mapping_bus_count(reported: i32) -> usize {
    (reported.max(0) as usize).min(MAX_MIDI_MAPPING_BUSES)
}

#[derive(Clone, Copy)]
struct ProgramChangeMapping {
    unit_id: i32,
    param_id: u32,
    program_count: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessingSampleSize {
    F32,
    F64,
}

impl ProcessingSampleSize {
    fn symbolic(self) -> i32 {
        match self {
            Self::F32 => SymbolicSampleSizes_::kSample32 as i32,
            Self::F64 => SymbolicSampleSizes_::kSample64 as i32,
        }
    }
}

#[derive(Default)]
struct BusActivationState {
    audio_inputs: Vec<bool>,
    audio_outputs: Vec<bool>,
    event_inputs: Vec<bool>,
    event_outputs: Vec<bool>,
}

/// Internal plugin implementation that handles all VST3 COM interactions
pub struct PluginImpl {
    // Core VST3 interfaces
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    /// True when the component and controller are the same object (single-component
    /// plugin). Then `IComponent::setState` already restores the controller, and calling
    /// `setComponentState` on top of it would double-apply and corrupt parameters.
    single_component: bool,

    // Plugin metadata
    pub(crate) info: PluginInfo,
    pub(crate) compatibility: Vec<crate::discovery::ClassCompatibility>,

    // Transport survives buffer/layout reconfiguration.
    position: crate::plugin::TransportPosition,
    continuous_samples: i64,
    latency: u32,
    tail: u32,
    // Processing state
    is_active: bool,
    is_processing: bool,
    sample_rate: f64,
    block_size: usize,
    /// The configuration the plugin's last successful `setupProcessing` carried, so
    /// `start_processing` can tell a re-start (nothing to do) from a configuration change
    /// (which has to be applied while the component is inactive). `None` until the first setup.
    applied_setup: Option<AppliedSetup>,
    /// Transport tempo (BPM) advertised in the host `ProcessContext`.
    tempo: f64,
    /// Time signature numerator advertised in the host `ProcessContext`.
    time_sig_numerator: i32,
    /// Time signature denominator advertised in the host `ProcessContext`.
    time_sig_denominator: i32,
    /// Whether the transport is playing (the `kPlaying` flag in `ProcessContext.state`).
    playing: bool,
    /// Real-time vs offline processing, baked into `ProcessSetup`/`process_data` at setup.
    process_mode: crate::plugin::ProcessMode,
    /// Optional VST3 3.7 declaration of exactly which ProcessContext fields the processor reads.
    /// `None` is the safe legacy path for older processors and advertises the historical context.
    process_context_requirements: Option<u32>,
    /// Current IPrefetchableSupport value, or `None` for pre-3.6 processors.
    prefetchable_support: Option<u32>,
    sample_size: ProcessingSampleSize,
    bus_activation: BusActivationState,
    /// Monotonic allocator for per-voice note ids (note_on); 0/-1 reserved for "unset".
    next_note_id: i32,
    // `(note_id, channel, pitch)` for notes started via `note_on`, so `note_off` can address the
    // release by pitch as well as by id. Pre-reserved and capped, so tracking never allocates on
    // the audio thread and a caller that never releases its notes can't grow it without bound.
    active_notes: Vec<(i32, i16, i16)>,
    /// Counts ordinary (noteId = -1) note-ons by channel/pitch. Fixed-size so MIDI tracking
    /// never allocates on the device callback.
    ordinary_note_counts: [u16; MIDI_CHANNEL_COUNT * 128],

    // Controller-derived routing tables. Both are built outside process() and only read from
    // the callback; `restartComponent` merely records invalidation flags atomically.
    midi_mapping_cache: MidiMappingCache,
    program_change_cache: Vec<ProgramChangeMapping>,
    dirty_caches: DirtyCaches,
    live_controllers: ArrayQueue<(MidiChannel, crate::midi::MidiController)>,
    unit_cache: Mutex<Option<Vec<crate::plugin::PluginUnit>>>,

    // Host data structures
    process_data: Option<Box<HostProcessData>>,
    component_handler: Option<ComWrapper<ComponentHandler>>,
    connection: Option<ConnectionPair>,

    // Parameter changes queued by the host (set_parameter / automation) to be fed into the
    // processor's input parameter queue at the start of the next process() block. Serialized
    // with process() by the caller's &mut access, so a plain Vec (no lock) is sufficient.
    // Pre-reserved to MAX_PENDING_PARAM_CHANGES and capped there — see the constant.
    pending_param_changes: Vec<ParameterChange>,
    // How many parameter changes have been dropped because the queue was full. Reported in the
    // warning so a host sees a running total rather than one line per lost change.
    dropped_param_changes: u64,

    // Parameter edits the plugin's *own editor* reported via `IComponentHandler::performEdit`,
    // after `process()` has routed them into the processor's input queue. Drained by the host
    // via `get_parameter_changes()` to update its UI. Separate from the raw performEdit sink
    // (`component_handler.parameter_changes`) so feeding the DSP and updating the display are
    // not two consumers racing to drain the same buffer.
    gui_param_changes_for_host: Arc<Mutex<Vec<(u32, f64)>>>,
    // Processor-originated output parameter points. Bounded and lock-free: process() pushes,
    // the host/UI drains through get_parameter_changes().
    output_param_feedback: Arc<ArrayQueue<(u32, f64)>>,
    // Parameter values that reached the processor queue from a thread that must not call
    // `IEditController` (the audio callback, via the playback handles). Bounded, lock-free and
    // drop-oldest; the control-thread service paths drain it into setParamNormalized so the
    // plugin's own editor, get_parameter/format_parameter and saved state track the DSP.
    deferred_controller_sync: ArrayQueue<(u32, f64)>,

    // Event handling
    input_events: ComWrapper<HostEventList>,
    output_events: ComWrapper<HostEventList>,
    // Holds a block's queued input events while `process` distributes them over the block's
    // chunks: each chunk is staged into `input_events` with only the events that fall inside it.
    // Pre-reserved to MAX_QUEUED_EVENTS (the input list's own cap), so splitting a block never
    // allocates on the audio thread.
    chunk_events: Vec<Option<PluginEvent>>,
    // MIDI the plugin has emitted (captured from output_events after each process block,
    // converted to MidiEvent), buffered for the host to poll. A lock-free bounded queue so the
    // audio thread can push without locking and a UI thread can drain concurrently; when full
    // the oldest event is dropped (bounded memory if the host never polls).
    output_events_owned: Arc<ArrayQueue<PluginEvent>>,

    // Plugin view
    plugin_view: Option<ComPtr<IPlugView>>,
    editor_scale_factor: f32,

    // Editor resize plumbing: the IPlugFrame handed to the plugin's view, and the slot it
    // writes requested sizes into (drained via take_editor_resize_request).
    plug_frame: ComWrapper<HostPlugFrame>,
    editor_resize: Arc<Mutex<Option<(i32, i32)>>>,
    // Linux IRunLoop registrations from the plugin's editor (fd handlers +
    // timers), serviced on the host UI thread via `service_run_loop`.
    #[cfg(target_os = "linux")]
    run_loop: Arc<Mutex<RunLoopRegistry>>,

    // VST3 module handle (kept alive). Declared after every plugin-side COM reference above so
    // those are released while the module's vtables still exist, and before `_host_app` below.
    _module: Box<dyn VstModule>,

    // Host application context passed to initialize() — kept alive for the plugin's lifetime
    // because the plugin may retain a reference to it. It has to outlive `_module`: the SDK's
    // `CPluginFactory::setHostContext` parks this pointer in a module-global (`gPluginContext`)
    // without an addRef, and module teardown — static destructors, `DeinitModule` — still reads
    // it. Rust drops fields in declaration order, so this one is declared last.
    _host_app: ComWrapper<HostApplication>,

    // Thread which initialized the component/controller. Lifecycle-sensitive restart handling
    // and controller cache refresh must return to this thread.
    control_thread: ThreadId,
}

// Processing data structure
struct HostProcessData {
    process_data: ProcessData,
    sample_buffers: HostSampleBuffers,
    input_bus_buffers: Vec<AudioBusBuffers>,
    output_bus_buffers: Vec<AudioBusBuffers>,
    process_context: ProcessContext,
    process_context_requirements: Option<u32>,
    transport_tempo: f64,
    input_param_changes: ComWrapper<ParameterChanges>,
    output_param_changes: ComWrapper<ParameterChanges>,
    // Preallocated channel-pointer arrays, built once in prepare_buffers. The audio buffers'
    // addresses are stable after allocation, so process() reuses these instead of rebuilding
    // them every block — keeping the steady-state audio path allocation-free.
}

enum CallerAudioBuffers<'a> {
    Flat(&'a mut AudioBuffers),
    Buses(&'a mut BusAudioBuffers),
}

impl CallerAudioBuffers<'_> {
    fn frame_count(&self, fallback: usize) -> usize {
        match self {
            Self::Flat(buffers) => buffers
                .outputs
                .iter()
                .chain(&buffers.inputs)
                .map(Vec::len)
                .next()
                .unwrap_or(fallback),
            Self::Buses(buffers) => buffers
                .outputs
                .iter()
                .chain(&buffers.inputs)
                .flat_map(|bus| &bus.channels)
                .map(Vec::len)
                .next()
                .unwrap_or(buffers.block_size),
        }
    }
}

struct TypedSampleBuffers<T> {
    inputs: Vec<Vec<T>>,
    outputs: Vec<Vec<T>>,
    input_channel_ptrs: SendChannelPtrs<T>,
    output_channel_ptrs: SendChannelPtrs<T>,
}

enum HostSampleBuffers {
    F32(TypedSampleBuffers<f32>),
    F64(TypedSampleBuffers<f64>),
}

/// The processing configuration handed to the plugin by `setupProcessing`, cached so a
/// re-start can tell "nothing changed" (skip setup, the plugin still has it) from "the host
/// reconfigured me" (setup must be re-run, and only while the component is inactive).
#[derive(Debug, Clone, Copy, PartialEq)]
struct AppliedSetup {
    sample_rate: f64,
    block_size: usize,
    /// The VST3 `ProcessModes` value, not the safe enum — this is compared against what was
    /// actually written into `ProcessSetup`.
    process_mode: i32,
    symbolic_sample_size: i32,
}

/// Per-bus channel pointers into the (audio-thread-owned) audio buffers.
///
/// `Send` because the pointers are only ever dereferenced on the one thread that owns the
/// `HostProcessData` (and thus the buffers they point into); the `Plugin` is moved to that
/// thread as a unit. The raw pointers never escape to another thread.
struct SendChannelPtrs<T>(Vec<Vec<*mut T>>);
unsafe impl<T: Send> Send for SendChannelPtrs<T> {}

#[derive(Clone, Copy)]
struct SavedProcessAudioIo {
    num_inputs: i32,
    num_outputs: i32,
    inputs: *mut AudioBusBuffers,
    outputs: *mut AudioBusBuffers,
}

fn hide_audio_io_for_zero_sample(
    process_data: &mut ProcessData,
    frames: usize,
) -> Option<SavedProcessAudioIo> {
    if frames != 0 {
        return None;
    }
    let saved = SavedProcessAudioIo {
        num_inputs: process_data.numInputs,
        num_outputs: process_data.numOutputs,
        inputs: process_data.inputs,
        outputs: process_data.outputs,
    };
    process_data.numInputs = 0;
    process_data.numOutputs = 0;
    process_data.inputs = ptr::null_mut();
    process_data.outputs = ptr::null_mut();
    Some(saved)
}

fn restore_process_audio_io(process_data: &mut ProcessData, saved: Option<SavedProcessAudioIo>) {
    if let Some(saved) = saved {
        process_data.numInputs = saved.num_inputs;
        process_data.numOutputs = saved.num_outputs;
        process_data.inputs = saved.inputs;
        process_data.outputs = saved.outputs;
    }
}

fn build_typed_sample_buffers<T: Default + Clone>(
    block_size: usize,
    input_buses: &[AudioBusBuffers],
    input_active: &[bool],
    output_buses: &[AudioBusBuffers],
    output_active: &[bool],
) -> TypedSampleBuffers<T> {
    fn side<T: Default + Clone>(
        block_size: usize,
        buses: &[AudioBusBuffers],
        active: &[bool],
    ) -> (Vec<Vec<T>>, SendChannelPtrs<T>) {
        let channel_count: usize = buses
            .iter()
            .zip(active.iter().copied().chain(std::iter::repeat(false)))
            .filter(|(_, active)| *active)
            .map(|(bus, _)| bus.numChannels.max(0) as usize)
            .sum();
        let mut buffers = vec![vec![T::default(); block_size]; channel_count];
        let mut next_channel = 0usize;
        let mut pointers = Vec::with_capacity(buses.len());
        for (index, bus) in buses.iter().enumerate() {
            let channels = bus.numChannels.max(0) as usize;
            let mut bus_pointers = Vec::with_capacity(channels);
            if active.get(index).copied().unwrap_or(false) {
                for _ in 0..channels {
                    bus_pointers.push(buffers[next_channel].as_mut_ptr());
                    next_channel += 1;
                }
            } else {
                // VST3 requires the host to supply the channel-buffer *array* for every bus
                // whether or not the bus is active; only the sample addresses inside it may be
                // null while a bus is inactive. `numChannels` stays as reported, so a plugin
                // that walks the array sees the shape it expects.
                bus_pointers.resize(channels, ptr::null_mut());
            }
            pointers.push(bus_pointers);
        }
        (buffers, SendChannelPtrs(pointers))
    }

    let (inputs, input_channel_ptrs) = side(block_size, input_buses, input_active);
    let (outputs, output_channel_ptrs) = side(block_size, output_buses, output_active);
    TypedSampleBuffers {
        inputs,
        outputs,
        input_channel_ptrs,
        output_channel_ptrs,
    }
}

impl HostSampleBuffers {
    fn input_channel_count(&self) -> usize {
        match self {
            Self::F32(samples) => samples.inputs.len(),
            Self::F64(samples) => samples.inputs.len(),
        }
    }

    fn output_channel_count(&self) -> usize {
        match self {
            Self::F32(samples) => samples.outputs.len(),
            Self::F64(samples) => samples.outputs.len(),
        }
    }

    fn copy_inputs_from(&mut self, inputs: &[Vec<f32>], frame_offset: usize, frames: usize) {
        match self {
            Self::F32(samples) => {
                for (index, destination) in samples.inputs.iter_mut().enumerate() {
                    let source = inputs.get(index);
                    let start = source
                        .map(|source| frame_offset.min(source.len()))
                        .unwrap_or(0);
                    let count = source
                        .map(|source| {
                            frames
                                .min(destination.len())
                                .min(source.len().saturating_sub(start))
                        })
                        .unwrap_or(0);
                    if let Some(source) = source {
                        destination[..count].copy_from_slice(&source[start..start + count]);
                    }
                    let end = frames.min(destination.len());
                    destination[count..end].fill(0.0);
                }
            }
            Self::F64(samples) => {
                for (index, destination) in samples.inputs.iter_mut().enumerate() {
                    let source = inputs.get(index);
                    let start = source
                        .map(|source| frame_offset.min(source.len()))
                        .unwrap_or(0);
                    let count = source
                        .map(|source| {
                            frames
                                .min(destination.len())
                                .min(source.len().saturating_sub(start))
                        })
                        .unwrap_or(0);
                    if let Some(source) = source {
                        for (to, from) in destination[..count]
                            .iter_mut()
                            .zip(&source[start..start + count])
                        {
                            *to = f64::from(*from);
                        }
                    }
                    let end = frames.min(destination.len());
                    destination[count..end].fill(0.0);
                }
            }
        }
    }

    fn copy_inputs_from_buses(
        &mut self,
        inputs: &[AudioBusBuffer],
        frame_offset: usize,
        frames: usize,
        buses: &[AudioBusBuffers],
        active: &[bool],
    ) {
        match self {
            Self::F32(samples) => copy_bus_inputs_to(
                &mut samples.inputs,
                inputs,
                frame_offset,
                frames,
                buses,
                active,
                |sample| sample,
            ),
            Self::F64(samples) => copy_bus_inputs_to(
                &mut samples.inputs,
                inputs,
                frame_offset,
                frames,
                buses,
                active,
                f64::from,
            ),
        }
    }

    fn clear_outputs(&mut self) {
        match self {
            Self::F32(samples) => {
                for channel in &mut samples.outputs {
                    channel.fill(0.0);
                }
            }
            Self::F64(samples) => {
                for channel in &mut samples.outputs {
                    channel.fill(0.0);
                }
            }
        }
    }

    fn update_input_silence_flags(
        &self,
        buses: &mut [AudioBusBuffers],
        active: &[bool],
        frames: usize,
    ) {
        match self {
            Self::F32(samples) => {
                update_bus_silence_flags(&samples.inputs, buses, active, frames, false)
            }
            Self::F64(samples) => {
                update_bus_silence_flags(&samples.inputs, buses, active, frames, false)
            }
        }
    }

    fn update_output_silence_flags(
        &self,
        buses: &mut [AudioBusBuffers],
        active: &[bool],
        frames: usize,
    ) {
        match self {
            Self::F32(samples) => {
                update_bus_silence_flags(&samples.outputs, buses, active, frames, true)
            }
            Self::F64(samples) => {
                update_bus_silence_flags(&samples.outputs, buses, active, frames, true)
            }
        }
    }

    fn copy_outputs_to(
        &self,
        outputs: &mut [Vec<f32>],
        frame_offset: usize,
        frames: usize,
        buses: &[AudioBusBuffers],
        active: &[bool],
    ) {
        match self {
            Self::F32(samples) => {
                copy_bus_outputs_to(
                    &samples.outputs,
                    outputs,
                    frame_offset,
                    frames,
                    buses,
                    active,
                    |sample| sample,
                );
            }
            Self::F64(samples) => {
                copy_bus_outputs_to(
                    &samples.outputs,
                    outputs,
                    frame_offset,
                    frames,
                    buses,
                    active,
                    |sample| sample as f32,
                );
            }
        }
    }

    fn copy_outputs_to_buses(
        &self,
        outputs: &mut [AudioBusBuffer],
        frame_offset: usize,
        frames: usize,
        buses: &[AudioBusBuffers],
        active: &[bool],
    ) {
        match self {
            Self::F32(samples) => copy_bus_outputs_to_buses(
                &samples.outputs,
                outputs,
                frame_offset,
                frames,
                buses,
                active,
                |sample| sample,
            ),
            Self::F64(samples) => copy_bus_outputs_to_buses(
                &samples.outputs,
                outputs,
                frame_offset,
                frames,
                buses,
                active,
                |sample| sample as f32,
            ),
        }
    }
}

fn copy_bus_inputs_to<T: Copy + Default>(
    destinations: &mut [Vec<T>],
    inputs: &[AudioBusBuffer],
    frame_offset: usize,
    frames: usize,
    buses: &[AudioBusBuffers],
    active: &[bool],
    convert: impl Fn(f32) -> T,
) {
    let mut destination_index = 0usize;
    for (bus_index, bus) in buses.iter().enumerate() {
        if !active.get(bus_index).copied().unwrap_or(false) {
            continue;
        }
        let source_bus = inputs.get(bus_index);
        for channel in 0..bus.numChannels.max(0) as usize {
            let Some(destination) = destinations.get_mut(destination_index) else {
                return;
            };
            let source = source_bus.and_then(|bus| bus.channels.get(channel));
            let start = source
                .map(|source| frame_offset.min(source.len()))
                .unwrap_or(0);
            let count = source
                .map(|source| {
                    frames
                        .min(destination.len())
                        .min(source.len().saturating_sub(start))
                })
                .unwrap_or(0);
            if let Some(source) = source {
                for (to, from) in destination[..count]
                    .iter_mut()
                    .zip(&source[start..start + count])
                {
                    *to = convert(*from);
                }
            }
            let clear_end = frames.min(destination.len());
            destination[count..clear_end].fill(T::default());
            destination_index += 1;
        }
    }
}

fn copy_bus_outputs_to_buses<T: Copy>(
    samples: &[Vec<T>],
    outputs: &mut [AudioBusBuffer],
    frame_offset: usize,
    frames: usize,
    buses: &[AudioBusBuffers],
    active: &[bool],
    convert: impl Fn(T) -> f32,
) {
    let mut sample_index = 0usize;
    for (bus_index, bus) in buses.iter().enumerate() {
        let is_active = active.get(bus_index).copied().unwrap_or(false);
        let Some(destination_bus) = outputs.get_mut(bus_index) else {
            return;
        };
        for channel in 0..bus.numChannels.max(0) as usize {
            let Some(destination) = destination_bus.channels.get_mut(channel) else {
                return;
            };
            let start = frame_offset.min(destination.len());
            let clear_count = frames.min(destination.len().saturating_sub(start));
            if !is_active {
                destination[start..start + clear_count].fill(0.0);
                continue;
            }
            let source = samples.get(sample_index + channel);
            let count = source
                .map(|source| clear_count.min(source.len()))
                .unwrap_or(0);
            if channel < 64 && bus.silenceFlags & (1u64 << channel) != 0 {
                destination[start..start + count].fill(0.0);
            } else if let Some(source) = source {
                for (to, from) in destination[start..start + count]
                    .iter_mut()
                    .zip(&source[..count])
                {
                    *to = convert(*from);
                }
            }
            destination[start + count..start + clear_count].fill(0.0);
        }
        if is_active {
            sample_index += bus.numChannels.max(0) as usize;
        }
    }
}

fn channel_mask(channel_count: i32) -> u64 {
    match channel_count.max(0) as u32 {
        0 => 0,
        64.. => u64::MAX,
        count => (1u64 << count) - 1,
    }
}

fn update_bus_silence_flags<T: PartialEq + Default>(
    samples: &[Vec<T>],
    buses: &mut [AudioBusBuffers],
    active: &[bool],
    frames: usize,
    preserve_plugin_flags: bool,
) {
    let zero = T::default();
    let mut sample_index = 0usize;
    for (bus_index, bus) in buses.iter_mut().enumerate() {
        let channel_count = bus.numChannels.max(0) as usize;
        if !active.get(bus_index).copied().unwrap_or(false) {
            bus.silenceFlags = channel_mask(bus.numChannels);
            continue;
        }

        let mut computed = 0u64;
        for channel in 0..channel_count {
            if channel < 64
                && samples
                    .get(sample_index + channel)
                    .is_none_or(|samples| samples.iter().take(frames).all(|sample| sample == &zero))
            {
                computed |= 1u64 << channel;
            }
        }
        bus.silenceFlags = if preserve_plugin_flags {
            (bus.silenceFlags | computed) & channel_mask(bus.numChannels)
        } else {
            computed
        };
        sample_index += channel_count;
    }
}

fn prepare_output_silence_flags(buses: &mut [AudioBusBuffers], active: &[bool]) {
    for (index, bus) in buses.iter_mut().enumerate() {
        bus.silenceFlags = if active.get(index).copied().unwrap_or(false) {
            0
        } else {
            channel_mask(bus.numChannels)
        };
    }
}

fn copy_bus_outputs_to<T: Copy>(
    samples: &[Vec<T>],
    outputs: &mut [Vec<f32>],
    frame_offset: usize,
    frames: usize,
    buses: &[AudioBusBuffers],
    active: &[bool],
    convert: impl Fn(T) -> f32,
) {
    let mut sample_index = 0usize;
    let mut destination_index = 0usize;
    for (bus_index, bus) in buses.iter().enumerate() {
        if !active.get(bus_index).copied().unwrap_or(false) {
            continue;
        }
        for channel in 0..bus.numChannels.max(0) as usize {
            let source = samples.get(sample_index + channel);
            let Some(destination) = outputs.get_mut(destination_index) else {
                return;
            };
            let start = frame_offset.min(destination.len());
            let count = source
                .map(|source| {
                    frames
                        .min(source.len())
                        .min(destination.len().saturating_sub(start))
                })
                .unwrap_or(0);
            if channel < 64 && bus.silenceFlags & (1u64 << channel) != 0 {
                destination[start..start + count].fill(0.0);
            } else if let Some(source) = source {
                for (to, from) in destination[start..start + count]
                    .iter_mut()
                    .zip(&source[..count])
                {
                    *to = convert(*from);
                }
            }
            let clear_end = frames.min(destination.len().saturating_sub(start));
            destination[start + count..start + clear_end].fill(0.0);
            destination_index += 1;
        }
        sample_index += bus.numChannels.max(0) as usize;
    }
    for destination in outputs.iter_mut().skip(destination_index) {
        let start = frame_offset.min(destination.len());
        let count = frames.min(destination.len().saturating_sub(start));
        destination[start..start + count].fill(0.0);
    }
}

fn view_rect_size(rect: &ViewRect) -> Result<(i32, i32)> {
    let width = rect
        .right
        .checked_sub(rect.left)
        .filter(|width| *width > 0)
        .ok_or_else(|| Error::Other("plugin editor returned an invalid width".to_string()))?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .filter(|height| *height > 0)
        .ok_or_else(|| Error::Other("plugin editor returned an invalid height".to_string()))?;
    Ok((width, height))
}

/// Offer a content scale factor to an editor view.
///
/// `Ok(true)` means the view took the factor. `Ok(false)` means it declined — either it does not
/// implement `IPlugViewContentScaleSupport` at all, or it implements it and answered with a
/// non-success code. Declining is a normal answer, not a failure: JUCE editors implement the
/// interface and return `kResultFalse` on macOS, where the window server already scales the
/// backing store. Treating that as an error takes the editor away from every JUCE plugin, so it
/// never becomes one. Only a nonsensical *request* (non-finite or non-positive) is an error.
unsafe fn set_view_scale_factor(view: &ComPtr<IPlugView>, factor: f32) -> Result<bool> {
    if !factor.is_finite() || factor <= 0.0 {
        return Err(Error::Other(
            "editor scale factor must be finite and greater than zero".to_string(),
        ));
    }
    let Some(scale_support) = view.cast::<IPlugViewContentScaleSupport>() else {
        return Ok(false);
    };
    let result = scale_support.setContentScaleFactor(factor);
    let accepted = result == kResultOk || result == kResultTrue;
    if !accepted {
        log::debug!("plugin declined editor scale factor {factor}: {result:#x}");
    }
    Ok(accepted)
}

/// Fill `event` with the VST3 note event a MIDI note-on maps to, returning whether that message
/// *releases* the note rather than starting it.
///
/// MIDI uses a note-on with velocity 0 as an alias for note-off. VST3 has no running status and
/// no such alias, so the alias has to become a real `kNoteOffEvent`: a zero-velocity
/// `kNoteOnEvent` leaves the plugin holding the voice while the host's own note tracker counts
/// it as released, and `midi_panic` then has nothing left to release it with.
fn write_midi_note_on(event: &mut Event, channel: MidiChannel, note: u8, velocity: u8) -> bool {
    if velocity == 0 {
        write_note_off_event(event, channel, note, 0);
        return true;
    }
    event.r#type = kNoteOnEvent as u16;
    event.__field0.noteOn.channel = channel.as_index() as i16;
    event.__field0.noteOn.pitch = note as i16;
    event.__field0.noteOn.tuning = 0.0;
    event.__field0.noteOn.velocity = velocity as f32 / 127.0;
    event.__field0.noteOn.length = 0;
    event.__field0.noteOn.noteId = -1;
    false
}

/// Fill `event` with a `kNoteOffEvent` for an ordinary (host-untracked) note.
fn write_note_off_event(event: &mut Event, channel: MidiChannel, note: u8, velocity: u8) {
    event.r#type = kNoteOffEvent as u16;
    event.__field0.noteOff.channel = channel.as_index() as i16;
    event.__field0.noteOff.pitch = note as i16;
    event.__field0.noteOff.velocity = velocity as f32 / 127.0;
    event.__field0.noteOff.noteId = -1;
    event.__field0.noteOff.tuning = 0.0;
}

impl PluginImpl {
    fn ensure_control_thread(&self, operation: &str) -> Result<()> {
        if thread::current().id() == self.control_thread {
            Ok(())
        } else {
            Err(Error::Other(format!(
                "{operation} must run on the plugin control thread"
            )))
        }
    }

    fn ensure_stream_size(&self, data: &[u8]) -> Result<()> {
        if data.len() <= super::com_implementations::MAX_STREAM_BYTES {
            Ok(())
        } else {
            Err(Error::Other(format!(
                "plugin data stream is too large ({} bytes, maximum {})",
                data.len(),
                super::com_implementations::MAX_STREAM_BYTES
            )))
        }
    }

    fn check_controller_result(result: tresult, operation: &str) -> Result<()> {
        if result == kResultOk || result == kResultTrue {
            Ok(())
        } else {
            Err(Error::Other(format!("{operation} failed: {result:#x}")))
        }
    }

    /// Configure the transport advertised to the plugin in the host `ProcessContext`.
    ///
    /// Call before processing starts (the values are baked into the context when
    /// `create_process_data` runs during `start_processing`). The musical playhead
    /// (`projectTimeMusic`) derives from `tempo` as the transport advances.
    pub fn set_transport(
        &mut self,
        tempo: f64,
        time_sig_numerator: i32,
        time_sig_denominator: i32,
    ) {
        self.tempo = tempo;
        self.time_sig_numerator = time_sig_numerator;
        self.time_sig_denominator = time_sig_denominator;
    }

    /// Update the transport tempo for the **next** processed block, even while processing is
    /// active: the stored tempo (used to rebuild the context after a reconfigure) and the live
    /// `ProcessContext` both move, and the musical playhead derives from the new tempo.
    #[allow(clippy::unnecessary_cast)]
    fn update_tempo(&mut self, bpm: f64) {
        self.tempo = bpm;
        if let Some(ref mut data) = self.process_data {
            data.transport_tempo = bpm;
            if process_context_needs(
                data.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedTempo as u32,
            ) {
                data.process_context.tempo = bpm;
            }
        }
    }

    /// Update the transport time signature for the **next** processed block, even while
    /// processing is active (stored fields plus the live `ProcessContext`).
    #[allow(clippy::unnecessary_cast)]
    fn update_time_signature(&mut self, numerator: i32, denominator: i32) {
        self.time_sig_numerator = numerator;
        self.time_sig_denominator = denominator;
        if let Some(ref mut data) = self.process_data {
            if process_context_needs(
                data.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedTimeSignature as u32,
            ) {
                data.process_context.timeSigNumerator = numerator;
                data.process_context.timeSigDenominator = denominator;
            }
        }
    }

    /// Toggle the transport playing state (`kPlaying`) for the **next** processed block, even
    /// while processing is active (stored field plus the live `ProcessContext.state`).
    fn update_playing(&mut self, playing: bool) {
        self.playing = playing;
        if let Some(ref mut data) = self.process_data {
            data.process_context.state =
                process_context_state(data.process_context_requirements, playing);
        }
    }

    /// Apply the host-configured sample rate / block size before processing starts. Called at
    /// load so `setupProcessing` (which runs at `start_processing`) uses the builder's settings
    /// rather than the internal defaults.
    pub fn set_audio_config(&mut self, sample_rate: f64, block_size: usize) {
        self.sample_rate = sample_rate;
        self.block_size = block_size;
    }

    /// Get parameter changes the plugin's editor made (for the host to update its UI).
    ///
    /// Returns edits that `process()` has already routed into the processor's input queue, so
    /// the DSP and the host display stay in sync. (Before processing has started, falls back to
    /// the raw performEdit sink so edits aren't lost.)
    pub fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
        self.drain_deferred_controller_sync();
        let mut changes = Vec::new();
        while let Some(change) = self.output_param_feedback.pop() {
            changes.push(change);
        }
        // Both drains take the elements in place rather than `mem::take`-ing the `Vec`: taking it
        // would leave a zero-capacity buffer behind, so the next block's `append` would
        // reallocate — on the audio thread, for the stash.
        if let Ok(mut stash) = self.gui_param_changes_for_host.lock() {
            if !stash.is_empty() {
                changes.extend(stash.drain(..));
            }
        }
        // Not processing yet (process() hasn't run to move edits into the stash): drain the raw
        // performEdit sink directly so the host UI still reflects editor changes.
        if !self.is_processing {
            if let Some(ref handler) = self.component_handler {
                if let Ok(mut raw_changes) = handler.parameter_changes.lock() {
                    changes.extend(raw_changes.drain(..));
                }
            }
        }
        changes
    }

    /// Push a value to the edit controller, tolerating the result codes real plugins return.
    ///
    /// The SDK's reference `EditController` answers `kResultTrue` on success and `kResultFalse`
    /// for an id it doesn't own, which makes the code look like a usable success signal — but
    /// shipping plugins don't honour it. Dexed (JUCE) returns `kResultFalse` for parameter ids
    /// 0/1/2 that it then applies correctly, so refusing to apply on a false would break
    /// automation on real plugins. Log it and carry on; `log` skips the formatting entirely when
    /// the level is disabled.
    fn apply_controller_parameter(&self, id: u32, value: f64) {
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        let result = unsafe { controller.setParamNormalized(id, value) };
        if result != kResultOk && result != kResultTrue {
            log::debug!(
                "setParamNormalized({id}) returned {result:#x}; applying anyway \
                 (many plugins report kResultFalse on success)"
            );
        }
    }

    /// Keep `IEditController` in step with a value queued for the processor.
    ///
    /// Controller calls belong to the main-thread domain, so a value arriving from the audio
    /// callback is parked in a bounded queue instead and applied by the next control-thread
    /// service call. The queue drops its oldest entry when full: the newest value for a
    /// parameter is the one worth showing.
    fn mirror_parameter_to_controller(&self, id: u32, value: f64) {
        if thread::current().id() == self.control_thread {
            self.apply_controller_parameter(id, value);
        } else {
            self.deferred_controller_sync.force_push((id, value));
        }
    }

    /// Apply parameter values that were queued off the control thread to `IEditController`.
    /// A no-op anywhere but the control thread, so the audio path never reaches the controller.
    fn drain_deferred_controller_sync(&self) {
        if thread::current().id() != self.control_thread {
            return;
        }
        while let Some((id, value)) = self.deferred_controller_sync.pop() {
            self.apply_controller_parameter(id, value);
        }
    }

    /// Rebuild whichever controller-derived caches were marked stale off the control thread,
    /// and apply parameter values queued for the controller from the same place. A no-op
    /// anywhere but the control thread; both halves are `IEditController` traffic.
    fn service_control_thread_caches(&mut self) {
        if thread::current().id() != self.control_thread {
            return;
        }
        self.drain_deferred_controller_sync();
        if std::mem::take(&mut self.dirty_caches.midi_mapping) {
            self.refresh_midi_mapping_cache();
        }
        if std::mem::take(&mut self.dirty_caches.program_change) {
            self.refresh_program_change_cache();
        }
    }

    fn refresh_midi_mapping_cache(&mut self) {
        let buses = unsafe {
            midi_mapping_bus_count(self.component.getBusCount(kEvent as i32, kInput as i32))
        };
        let mut cache = MidiMappingCache {
            buses,
            assignments: vec![None; buses * MIDI_CHANNEL_COUNT * MIDI_CONTROLLER_COUNT],
            mapped: Vec::new(),
            output: Vec::new(),
        };
        if let Some(controller) = self.controller.as_ref() {
            if let Some(mapped) = super::midi_mapping::assignments(controller, kInput) {
                for item in &mapped {
                    if let crate::midi::MidiController::Midi1(cc) = item.controller {
                        if let Some(index) =
                            cache.index(item.bus, item.channel.as_index() as i16, cc)
                        {
                            if let Some(slot) = cache.assignments.get_mut(index) {
                                *slot = Some(item.parameter_id);
                            }
                        }
                    }
                }
                cache.mapped = mapped;
                cache.output =
                    super::midi_mapping::assignments(controller, kOutput).unwrap_or_default();
                self.midi_mapping_cache = cache;
                return;
            }
        }
        let Some(mapping) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IMidiMapping>())
        else {
            self.midi_mapping_cache = cache;
            return;
        };
        unsafe {
            for bus in 0..buses {
                for channel in 0..MIDI_CHANNEL_COUNT {
                    for controller in 0..MIDI_CONTROLLER_COUNT {
                        let mut id = 0;
                        if mapping.getMidiControllerAssignment(
                            bus as i32,
                            channel as i16,
                            controller as CtrlNumber,
                            &mut id,
                        ) == kResultOk
                        {
                            let index = (bus * MIDI_CHANNEL_COUNT + channel)
                                * MIDI_CONTROLLER_COUNT
                                + controller;
                            cache.assignments[index] = Some(id);
                            if let Some(channel) = MidiChannel::from_index(channel as u8) {
                                cache.mapped.push(crate::midi::MidiControllerAssignment {
                                    bus: bus as i32,
                                    channel,
                                    controller: crate::midi::MidiController::Midi1(
                                        controller as u16,
                                    ),
                                    parameter_id: id,
                                });
                            }
                        }
                    }
                }
            }
        }
        self.midi_mapping_cache = cache;
    }

    fn refresh_program_change_cache(&mut self) {
        self.program_change_cache.clear();
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        let Some(unit_info) = controller.cast::<IUnitInfo>() else {
            return;
        };
        unsafe {
            let mut unit_lists = Vec::new();
            for index in 0..unit_info.getUnitCount() {
                let mut unit: UnitInfo = std::mem::zeroed();
                if unit_info.getUnitInfo(index, &mut unit) == kResultOk {
                    unit_lists.push((unit.id, unit.programListId));
                }
            }
            let mut list_counts = Vec::new();
            for index in 0..unit_info.getProgramListCount() {
                let mut list: ProgramListInfo = std::mem::zeroed();
                if unit_info.getProgramListInfo(index, &mut list) == kResultOk {
                    list_counts.push((list.id, list.programCount));
                }
            }
            for index in 0..controller.getParameterCount() {
                let mut parameter: ParameterInfo = std::mem::zeroed();
                if controller.getParameterInfo(index, &mut parameter) != kResultOk
                    || parameter.flags & ParameterInfo_::ParameterFlags_::kIsProgramChange == 0
                {
                    continue;
                }
                let Some((_, list_id)) = unit_lists
                    .iter()
                    .find(|(unit_id, _)| *unit_id == parameter.unitId)
                else {
                    continue;
                };
                let Some((_, program_count)) = list_counts.iter().find(|(id, _)| id == list_id)
                else {
                    continue;
                };
                if *program_count > 0 {
                    self.program_change_cache.push(ProgramChangeMapping {
                        unit_id: parameter.unitId,
                        param_id: parameter.id,
                        program_count: *program_count,
                    });
                }
            }
        }
    }

    fn cached_program_change(&self, unit_id: i32) -> Option<ProgramChangeMapping> {
        self.program_change_cache
            .iter()
            .copied()
            .find(|mapping| mapping.unit_id == unit_id)
    }

    /// Load a VST3 plugin from the given path
    pub fn load(path: &std::path::Path) -> Result<Self> {
        Self::load_with_class(path, None)
    }

    /// Load a specific current or moduleinfo-retired audio class from a VST3 bundle.
    pub fn load_class(path: &std::path::Path, requested_class_id: &str) -> Result<Self> {
        Self::load_with_class(path, Some(requested_class_id))
    }

    fn load_with_class(path: &std::path::Path, requested_class_id: Option<&str>) -> Result<Self> {
        let requested_class_id = requested_class_id
            .map(|requested| -> Result<String> {
                crate::internal::utils::parse_class_uid(requested).ok_or_else(|| {
                    Error::PluginLoadFailed(
                        "class id must be exactly 32 hexadecimal characters".to_string(),
                    )
                })?;
                Ok(requested.to_ascii_uppercase())
            })
            .transpose()?;
        // A file is authoritative. The runtime compatibility class is queried only when the
        // bundle has no moduleinfo.json.
        let module_info = crate::internal::module_info::read(path)?;

        unsafe {
            log::info!("=== PLUGIN LOADING START ===");
            log::info!("Loading plugin from: {}", path.display());

            // Load the VST3 module using platform-specific loader. Declared first so it drops
            // LAST: everything below lives in the module's address space, and the COM teardown
            // (whether via `InitializedComponent` or `Drop for PluginImpl`) must complete before
            // the module unmaps.
            log::debug!("Step 1: Loading VST3 module...");
            let module = load_module(path)?;
            log::debug!("VST3 module loaded successfully");

            // Get factory from module
            log::debug!("Step 2: Getting factory from module...");
            let factory_ptr = module.get_factory()?;
            log::debug!("Factory obtained, ptr: {:?}", factory_ptr);
            if factory_ptr.is_null() {
                return Err(Error::PluginLoadFailed(
                    "GetPluginFactory returned null".to_string(),
                ));
            }

            log::debug!("Step 3: Wrapping factory in ComPtr...");
            let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or_else(|| {
                Error::PluginLoadFailed("Failed to create factory ComPtr".to_string())
            })?;
            log::debug!("Factory wrapped successfully");

            // Factory3 must receive the host context before any class is instantiated. Keep the
            // same context alive for the complete component/controller lifetime.
            let host_app = create_host_application();
            let host_ctx = host_app.to_com_ptr::<IHostApplication>();
            let context = host_ctx
                .as_ref()
                .map(|p| p.as_ptr() as *mut FUnknown)
                .unwrap_or(ptr::null_mut());
            if let Some(factory3) = factory.cast::<IPluginFactory3>() {
                let result = factory3.setHostContext(context);
                if result != kResultOk && result != kResultTrue {
                    log::warn!("IPluginFactory3::setHostContext failed: {result:#x}");
                }
            }
            let compatibility = match module_info.as_ref() {
                Some(module_info) => module_info.compatibility.clone(),
                None => crate::internal::module_info::read_factory_compatibility(&factory)?,
            };
            let target_class_id = match requested_class_id.as_deref() {
                Some(requested) if module_info.is_some() => Some(
                    module_info
                        .as_ref()
                        .and_then(|module_info| module_info.resolve_class_id(requested))
                        .ok_or_else(|| {
                            Error::PluginLoadFailed(format!(
                                "class id {requested} is not declared by {}",
                                path.display()
                            ))
                        })?
                        .to_string(),
                ),
                Some(requested) => Some(
                    crate::internal::module_info::resolve_factory_audio_class_id(
                        &factory,
                        &compatibility,
                        requested,
                    )?,
                ),
                None => None,
            };

            // Find and create the audio component
            log::debug!("Step 4: Creating audio component...");
            let (component, class_index) =
                Self::create_component(&factory, target_class_id.as_deref())?;
            log::debug!("Component created successfully (class index {class_index})");

            // Initialize component with a host-application context. Passing null here
            // crashes plugins that query the host (u-he, Waves, ...); see HostApplication.
            log::debug!("Step 5: Initializing component...");
            let init_result = component.initialize(context);
            if init_result != kResultOk {
                // A component that failed to initialize must NOT be terminated (terminate pairs
                // with a *successful* initialize), and must certainly not be driven on through
                // activateBus/setActive/process — so bail before anything else touches it. The
                // ComPtr releases it here and the module unloads as this frame unwinds.
                return Err(Error::PluginLoadFailed(format!(
                    "IComponent::initialize failed: {init_result:#x}"
                )));
            }
            log::debug!("Component initialized with result: {init_result:#x}");

            // The component is live from here: it may have spawned threads and registered
            // callbacks, so every failure below has to run the full ordered teardown before the
            // module unloads. The guard does that until the finished `PluginImpl` takes over.
            let mut initialized = InitializedComponent::new(component.clone());

            // VST3 3.7 requires hosts to query this after initialize and before activation.
            // Older processors do not expose it; `None` selects the safe legacy context.
            let process_context_requirements = component
                .cast::<IProcessContextRequirements>()
                .map(|requirements| requirements.getProcessContextRequirements());
            let prefetchable_support = Self::query_prefetchable_support(&component);

            // Establish the component's declared default bus state while it is inactive.
            log::debug!("Step 6: Activating default buses...");
            let bus_activation = Self::activate_default_buses(&component)?;
            log::debug!("Default buses activated");

            // Get processor interface
            log::debug!("Step 7: Getting IAudioProcessor interface...");
            let processor = component.cast::<IAudioProcessor>().ok_or_else(|| {
                Error::InterfaceError("Component does not implement IAudioProcessor".to_string())
            })?;
            let sample_size = Self::select_sample_size(&processor)?;
            log::debug!("IAudioProcessor interface obtained");

            // Create component handler for parameter change notifications
            log::debug!("Step 8: Creating component handler...");
            let parameter_changes = Arc::new(Mutex::new(Vec::with_capacity(MAX_EDITOR_FEEDBACK)));
            let component_handler =
                ComWrapper::new(ComponentHandler::new(parameter_changes.clone()));
            log::debug!("Component handler created");

            // Get or create controller (handles both single-component and separate controller)
            log::debug!("Step 9: Getting or creating controller...");
            // A component that directly implements IEditController is a single-component
            // plugin; this distinction matters for state restore (see `single_component`).
            let single_component = component.cast::<IEditController>().is_some();
            let controller = Self::get_or_create_controller(&component, &factory, context)?;
            initialized.attach_controller(controller.clone(), single_component);
            log::debug!(
                "Controller obtained: {} (single_component: {single_component})",
                controller.is_some()
            );

            // Connect component and controller if they are separate
            if let Some(ref ctrl) = controller {
                log::debug!("Step 10: Connecting component and controller...");
                let connection = Self::connect_component_and_controller(&component, ctrl)?;
                initialized.attach_connection(connection);
                log::debug!("Component and controller connected");

                // Set component handler on controller for parameter change notifications
                log::debug!("Step 11: Setting component handler on controller...");
                if let Some(handler_ptr) = component_handler.as_com_ref::<IComponentHandler>() {
                    let result = ctrl.setComponentHandler(handler_ptr.as_ptr());
                    if result == kResultOk {
                        log::debug!("Component handler set on controller successfully");
                    } else {
                        log::warn!(
                            "Failed to set component handler on controller: {:#x}",
                            result
                        );
                    }
                } else {
                    log::error!("Failed to get IComponentHandler COM pointer");
                }

                if !single_component {
                    // A separate controller does not share the component's state implicitly. The
                    // initial synchronization belongs here: both objects are initialized and
                    // connected, while the component is still inactive.
                    let component_state =
                        create_memory_stream_with_metadata(None, StreamStateType::Project);
                    let component_state_ptr =
                        component_state.to_com_ptr::<IBStream>().ok_or_else(|| {
                            Error::InterfaceError(
                                "failed to create initial component-state stream".to_string(),
                            )
                        })?;
                    let get_state_result = component.getState(component_state_ptr.as_ptr());
                    if get_state_result == kResultOk || get_state_result == kResultTrue {
                        let controller_state = create_memory_stream_from_with_metadata(
                            component_state.to_vec(),
                            None,
                            StreamStateType::Project,
                        );
                        let controller_state_ptr =
                            controller_state.to_com_ptr::<IBStream>().ok_or_else(|| {
                                Error::InterfaceError(
                                    "failed to create initial controller-state stream".to_string(),
                                )
                            })?;
                        let set_state_result =
                            ctrl.setComponentState(controller_state_ptr.as_ptr());
                        if set_state_result != kResultOk
                            && set_state_result != kResultTrue
                            && set_state_result != kNotImplemented
                            && set_state_result != kResultFalse
                        {
                            return Err(Error::PluginLoadFailed(format!(
                                "initial controller state synchronization failed: \
                                 {set_state_result:#x}"
                            )));
                        }
                    } else if get_state_result != kNotImplemented
                        && get_state_result != kResultFalse
                    {
                        return Err(Error::PluginLoadFailed(format!(
                            "initial component state read failed: {get_state_result:#x}"
                        )));
                    }
                }
            }

            // Editor resize plumbing (an IPlugFrame the view can call into),
            // plus the Linux IRunLoop registry VSTGUI-based editors need.
            let editor_resize = Arc::new(Mutex::new(None));
            #[cfg(target_os = "linux")]
            let run_loop = Arc::new(Mutex::new(RunLoopRegistry::new()));
            #[cfg(target_os = "linux")]
            let plug_frame = create_host_plug_frame(editor_resize.clone(), run_loop.clone());
            #[cfg(not(target_os = "linux"))]
            let plug_frame = create_host_plug_frame(editor_resize.clone());

            // Create event lists
            log::debug!("Step 13: Creating event lists...");
            let input_events = create_event_list();
            let output_events = create_event_list();
            log::debug!("Event lists created");

            // Extract plugin info from the factory and component. Keyed on the class that was
            // actually instantiated, so a multi-class factory reports the uid the host can
            // reload (or respawn under isolation) to get this same component back.
            let info = Self::extract_plugin_info(path, &factory, &component, class_index)?;

            let has_gui = controller.is_some() && {
                if let Some(ref ctrl) = controller {
                    let view_type = c"editor".as_ptr();
                    let view_ptr = ctrl.createView(view_type);
                    if !view_ptr.is_null() {
                        // Release the probe view; never call `removed()` on it — that pairs with
                        // `attached()`, and an unmatched `removed()` crashes some plugins that
                        // initialize their close state only on attach.
                        let _ = ComPtr::<IPlugView>::from_raw(view_ptr);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };

            let mut updated_info = info;
            updated_info.has_gui = has_gui;

            host_app.configure_data_exchange(
                processor.as_ptr(),
                controller
                    .as_ref()
                    .and_then(|controller| controller.cast::<IDataExchangeReceiver>()),
            );

            log::info!(
                "Plugin info: {} by {}",
                updated_info.name,
                updated_info.vendor
            );

            let mut plugin = Self {
                component,
                processor,
                controller,
                single_component,
                info: updated_info,
                compatibility,
                is_active: false,
                is_processing: false,
                sample_rate: 44100.0,
                block_size: 512,
                applied_setup: None,
                tempo: 120.0,
                time_sig_numerator: 4,
                time_sig_denominator: 4,
                playing: true,
                process_mode: crate::plugin::ProcessMode::Realtime,
                process_context_requirements,
                prefetchable_support,
                sample_size,
                bus_activation,
                next_note_id: 1,
                active_notes: Vec::with_capacity(MAX_TRACKED_NOTES),
                ordinary_note_counts: [0; MIDI_CHANNEL_COUNT * 128],
                midi_mapping_cache: MidiMappingCache::default(),
                program_change_cache: Vec::new(),
                dirty_caches: DirtyCaches::default(),
                live_controllers: ArrayQueue::new(256),
                unit_cache: Mutex::new(None),
                process_data: None,
                component_handler: Some(component_handler),
                connection: initialized.take_connection(),
                pending_param_changes: Vec::with_capacity(MAX_PENDING_PARAM_CHANGES),
                dropped_param_changes: 0,
                gui_param_changes_for_host: Arc::new(Mutex::new(Vec::with_capacity(
                    MAX_EDITOR_FEEDBACK,
                ))),
                output_param_feedback: Arc::new(ArrayQueue::new(MAX_OUTPUT_PARAMETER_FEEDBACK)),
                deferred_controller_sync: ArrayQueue::new(MAX_DEFERRED_CONTROLLER_SYNC),
                input_events,
                output_events,
                chunk_events: Vec::with_capacity(MAX_QUEUED_EVENTS),
                output_events_owned: Arc::new(ArrayQueue::new(MAX_OUTPUT_MIDI)),
                plugin_view: None,
                editor_scale_factor: 1.0,
                plug_frame,
                editor_resize,
                #[cfg(target_os = "linux")]
                run_loop,
                _module: module,
                _host_app: host_app,
                control_thread: thread::current().id(),
                position: Default::default(),
                continuous_samples: 0,
                latency: 0,
                tail: 0,
            };
            // From here on `plugin` owns the teardown: dropping it runs the same ordered
            // sequence the guard would.
            initialized.disarm();

            // IMidiMapping and unit/program metadata are controller calls and therefore belong
            // here on the loading thread, never in process() or a playback command drain.
            plugin.refresh_midi_mapping_cache();
            plugin.refresh_program_change_cache();

            // Give the processor its own currently-advertised arrangements before the first
            // setup. A refusal is legitimate (the plugin keeps its layout); other failures are
            // surfaced instead of being silently discarded.
            plugin.negotiate_default_bus_arrangements()?;

            // VST3 lifecycle: `setupProcessing` and bus activation require an INACTIVE
            // component, and a plugin sizes its DSP buffers when it goes active, from the
            // ProcessSetup it was last given. So set up first, activate second — activating an
            // un-set-up component hands it whatever defaults it was born with.
            //
            // Both are best-effort here, and neither failure fails the load: this runs at the
            // internal default configuration (the host applies its real sample rate and block
            // size immediately after load), the plugin remains fully inspectable either way, and
            // `start_processing` re-runs both with the real settings and reports a genuine
            // failure there.
            if let Err(e) = plugin.setup_processing() {
                log::warn!("setupProcessing failed at load ({e}); deferring to start_processing");
            } else if let Err(e) = plugin.activate() {
                log::warn!(
                    "Component activation failed at load ({e}); deferring to start_processing"
                );
            }

            log::info!("=== PLUGIN LOADING COMPLETE ===");
            log::info!(
                "Has GUI: {}, Active: {}",
                plugin.info.has_gui,
                plugin.is_active
            );
            Ok(plugin)
        }
    }

    /// Put the component into the active state (`IComponent::setActive(true)`).
    ///
    /// Only valid once the component has been given a `ProcessSetup` — see the ordering note in
    /// [`Self::load`].
    fn activate(&mut self) -> Result<()> {
        let result = self.set_component_active(true);
        if result != kResultOk {
            return Err(Error::Other(format!(
                "Failed to activate component: {result:#x}"
            )));
        }
        self.is_active = true;
        Ok(())
    }

    fn set_component_active(&self, active: bool) -> tresult {
        if !active {
            self._host_app.flush_data_exchange();
            // The processor closes queues from its setActive(false) callback. Mark the host-side
            // gate inactive before entering that callback so closeQueue is accepted there.
            self._host_app.set_data_exchange_active(false);
        }
        let result = unsafe { self.component.setActive(u8::from(active)) };
        if result == kResultOk || result == kResultTrue {
            self._host_app.set_data_exchange_active(active);
        } else if !active {
            // Deactivation was rejected; the processor remains active and its queues stay live.
            self._host_app.set_data_exchange_active(true);
        }
        result
    }

    /// Describe the plugin class at `class_index` — the class [`Self::create_component`]
    /// actually instantiated, not merely the first audio class the factory lists. A factory can
    /// export several audio classes (a synth plus its effect sibling, or per-format variants),
    /// and the reported `uid` is what a host stores to reload this exact plugin.
    fn extract_plugin_info(
        path: &std::path::Path,
        factory: &ComPtr<IPluginFactory>,
        component: &ComPtr<IComponent>,
        class_index: i32,
    ) -> Result<PluginInfo> {
        unsafe {
            // Get factory info
            let mut factory_info: PFactoryInfo = std::mem::zeroed();
            factory.getFactoryInfo(&mut factory_info);
            let mut vendor = crate::internal::utils::c_str_to_string(&factory_info.vendor);

            let mut class_info: PClassInfo = std::mem::zeroed();
            if factory.getClassInfo(class_index, &mut class_info) != kResultOk {
                return Err(Error::PluginLoadFailed(format!(
                    "Could not read class info for the instantiated class (index {class_index})"
                )));
            }

            let mut name = crate::internal::utils::c_str_to_string(&class_info.name);
            let uid = crate::internal::utils::format_class_uid(&class_info.cid);

            // Count audio buses
            let audio_inputs = component.getBusCount(kAudio as i32, kInput as i32) as u32;
            let audio_outputs = component.getBusCount(kAudio as i32, kOutput as i32) as u32;

            // Real version + sub-categories via IPluginFactory2 when the factory
            // provides it; left empty (honest) rather than faked when it doesn't.
            let (mut version, mut category) = factory
                .cast::<IPluginFactory2>()
                .and_then(|f2| {
                    let mut info2: PClassInfo2 = std::mem::zeroed();
                    if f2.getClassInfo2(class_index, &mut info2) == kResultOk {
                        Some((
                            crate::internal::utils::c_str_to_string(&info2.version),
                            crate::internal::utils::c_str_to_string(&info2.subCategories),
                        ))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            // Factory3 carries class/vendor/version names as UTF-16. Prefer it so non-ASCII
            // metadata is not mangled, while retaining Factory2/legacy fallbacks.
            if let Some(factory3) = factory.cast::<IPluginFactory3>() {
                let mut info3: PClassInfoW = std::mem::zeroed();
                if factory3.getClassInfoUnicode(class_index, &mut info3) == kResultOk {
                    let utf16 = |value: &[u16]| {
                        let end = value.iter().position(|&ch| ch == 0).unwrap_or(value.len());
                        String::from_utf16_lossy(&value[..end])
                    };
                    let unicode_name = utf16(&info3.name);
                    let unicode_vendor = utf16(&info3.vendor);
                    let unicode_version = utf16(&info3.version);
                    if !unicode_name.is_empty() {
                        name = unicode_name;
                    }
                    if !unicode_vendor.is_empty() {
                        vendor = unicode_vendor;
                    }
                    if !unicode_version.is_empty() {
                        version = unicode_version;
                    }
                    let unicode_category =
                        crate::internal::utils::c_str_to_string(&info3.subCategories);
                    if !unicode_category.is_empty() {
                        category = unicode_category;
                    }
                }
            }

            // MIDI capability from the presence of event buses, not a guess.
            let has_midi_input = component.getBusCount(kEvent as i32, kInput as i32) > 0;
            let has_midi_output = component.getBusCount(kEvent as i32, kOutput as i32) > 0;

            Ok(PluginInfo {
                path: path.to_path_buf(),
                name,
                vendor,
                version,
                category,
                uid,
                audio_inputs,
                audio_outputs,
                has_gui: false, // Will be updated by caller
                has_midi_input,
                has_midi_output,
            })
        }
    }

    /// Find and create the audio component from the factory, returning it together with the
    /// factory class index it came from (so the reported [`PluginInfo`] describes that class).
    unsafe fn create_component(
        factory: &ComPtr<IPluginFactory>,
        target_class_id: Option<&str>,
    ) -> Result<(ComPtr<IComponent>, i32)> {
        let num_classes = factory.countClasses();

        for i in 0..num_classes {
            let mut class_info = std::mem::zeroed();
            if factory.getClassInfo(i, &mut class_info) == kResultOk {
                let category = crate::internal::utils::c_str_to_string(&class_info.category);

                let class_id = crate::internal::utils::format_class_uid(&class_info.cid);
                let is_target = target_class_id.is_none_or(|target| {
                    crate::internal::utils::class_uid_matches(&class_id, target)
                });

                if category.contains("Audio Module Class") && is_target {
                    let mut component_ptr: *mut IComponent = ptr::null_mut();

                    let result = factory.createInstance(
                        class_info.cid.as_ptr() as *const std::os::raw::c_char,
                        IComponent::IID.as_ptr() as *const std::os::raw::c_char,
                        &mut component_ptr as *mut _ as *mut _,
                    );

                    if result == kResultOk && !component_ptr.is_null() {
                        let component = ComPtr::from_raw(component_ptr).ok_or_else(|| {
                            Error::PluginLoadFailed("Failed to create component".to_string())
                        })?;
                        return Ok((component, i));
                    }
                }
            }
        }

        Err(Error::PluginLoadFailed(match target_class_id {
            Some(class_id) => format!("Audio component class {class_id} was not found in plugin"),
            None => "No audio component found in plugin".to_string(),
        }))
    }

    /// Map the configured [`ProcessMode`](crate::plugin::ProcessMode) to the VST3 enum value.
    fn vst_process_mode(&self) -> i32 {
        match self.process_mode {
            crate::plugin::ProcessMode::Offline => ProcessModes_::kOffline as i32,
            crate::plugin::ProcessMode::Prefetch => ProcessModes_::kPrefetch as i32,
            crate::plugin::ProcessMode::Realtime => ProcessModes_::kRealtime as i32,
        }
    }

    #[allow(clippy::unnecessary_cast)]
    unsafe fn query_prefetchable_support(component: &ComPtr<IComponent>) -> Option<u32> {
        let support = component.cast::<IPrefetchableSupport>()?;
        let mut value = ePrefetchableSupport_::kIsNotYetPrefetchable as u32;
        let result = support.getPrefetchableSupport(&mut value);
        (result == kResultOk || result == kResultTrue).then_some(value)
    }

    unsafe fn select_sample_size(
        processor: &ComPtr<IAudioProcessor>,
    ) -> Result<ProcessingSampleSize> {
        let sample32 = SymbolicSampleSizes_::kSample32 as i32;
        if processor.canProcessSampleSize(sample32) == kResultOk {
            return Ok(ProcessingSampleSize::F32);
        }
        let sample64 = SymbolicSampleSizes_::kSample64 as i32;
        if processor.canProcessSampleSize(sample64) == kResultOk {
            return Ok(ProcessingSampleSize::F64);
        }
        Err(Error::InterfaceError(
            "plugin supports neither 32-bit nor 64-bit processing".to_string(),
        ))
    }

    unsafe fn negotiate_default_bus_arrangements(&self) -> Result<()> {
        let arrangements = self.bus_arrangements()?;
        let mut inputs: Vec<u64> = arrangements.inputs.iter().map(|a| a.raw()).collect();
        let mut outputs: Vec<u64> = arrangements.outputs.iter().map(|a| a.raw()).collect();
        let result = self.processor.setBusArrangements(
            inputs.as_mut_ptr(),
            inputs.len() as i32,
            outputs.as_mut_ptr(),
            outputs.len() as i32,
        );
        if result == kResultOk || result == kResultTrue || result == kResultFalse {
            if result == kResultFalse {
                log::debug!("plugin declined its advertised default bus arrangements");
            }
            Ok(())
        } else {
            Err(Error::InterfaceError(format!(
                "default setBusArrangements failed: {result:#x}"
            )))
        }
    }

    /// The configuration currently baked into the plugin via `setupProcessing`.
    fn current_setup(&self) -> AppliedSetup {
        AppliedSetup {
            sample_rate: self.sample_rate,
            block_size: self.block_size,
            process_mode: self.vst_process_mode(),
            symbolic_sample_size: self.sample_size.symbolic(),
        }
    }

    /// Set up processing with the current configuration. VST3 requires the component to be
    /// **inactive**; every caller either runs before the first activation or deactivates first.
    fn setup_processing(&mut self) -> Result<()> {
        unsafe {
            // Set up processing
            let setup = ProcessSetup {
                processMode: self.vst_process_mode(),
                symbolicSampleSize: self.sample_size.symbolic(),
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
            self.applied_setup = Some(self.current_setup());
            self.latency = self.processor.getLatencySamples();
            self.tail = self.processor.getTailSamples();

            Ok(())
        }
    }

    /// Create processing data structures
    // `as u32` on the StatesAndFlags_ constants is required where they are generated as
    // `i32`; on targets where they are already `u32` clippy flags it as redundant.
    #[allow(clippy::unnecessary_cast)]
    fn create_process_data(&mut self) -> Result<()> {
        unsafe {
            let mut data = Box::new(HostProcessData {
                process_data: std::mem::zeroed(),
                sample_buffers: match self.sample_size {
                    ProcessingSampleSize::F32 => HostSampleBuffers::F32(TypedSampleBuffers {
                        inputs: Vec::new(),
                        outputs: Vec::new(),
                        input_channel_ptrs: SendChannelPtrs(Vec::new()),
                        output_channel_ptrs: SendChannelPtrs(Vec::new()),
                    }),
                    ProcessingSampleSize::F64 => HostSampleBuffers::F64(TypedSampleBuffers {
                        inputs: Vec::new(),
                        outputs: Vec::new(),
                        input_channel_ptrs: SendChannelPtrs(Vec::new()),
                        output_channel_ptrs: SendChannelPtrs(Vec::new()),
                    }),
                },
                input_bus_buffers: Vec::new(),
                output_bus_buffers: Vec::new(),
                process_context: std::mem::zeroed(),
                process_context_requirements: self.process_context_requirements,
                transport_tempo: self.tempo,
                input_param_changes: ComWrapper::new(ParameterChanges::default()),
                output_param_changes: ComWrapper::new(ParameterChanges::default()),
            });

            // Initialize process context
            data.process_context.sampleRate = self.sample_rate;
            data.process_context.projectTimeSamples = self.position.samples;
            data.process_context.projectTimeMusic = self.position.quarter_notes;
            data.process_context.continousTimeSamples = self.continuous_samples;
            if process_context_needs(
                self.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedSystemTime as u32,
            ) && self.process_context_requirements.is_some()
            {
                data.process_context.systemTime = current_system_time_nanos();
            }
            if process_context_needs(
                self.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedTempo as u32,
            ) {
                data.process_context.tempo = self.tempo;
            }
            if process_context_needs(
                self.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedTimeSignature as u32,
            ) {
                data.process_context.timeSigNumerator = self.time_sig_numerator;
                data.process_context.timeSigDenominator = self.time_sig_denominator;
            }
            data.process_context.state =
                process_context_state(self.process_context_requirements, self.playing);

            // Set up process data
            data.process_data.processMode = self.vst_process_mode();
            data.process_data.numSamples = self.block_size as i32;
            data.process_data.symbolicSampleSize = self.sample_size.symbolic();
            data.process_data.processContext = &mut data.process_context;

            // Set up event lists
            data.process_data.inputEvents = self
                .input_events
                .as_com_ref::<IEventList>()
                .map(|ptr| ptr.as_ptr())
                .unwrap_or(ptr::null_mut());
            data.process_data.outputEvents = self
                .output_events
                .as_com_ref::<IEventList>()
                .map(|ptr| ptr.as_ptr())
                .unwrap_or(ptr::null_mut());

            // Set up parameter changes
            data.process_data.inputParameterChanges = data
                .input_param_changes
                .as_com_ref::<IParameterChanges>()
                .map(|ptr| ptr.as_ptr())
                .unwrap_or(ptr::null_mut());
            data.process_data.outputParameterChanges = data
                .output_param_changes
                .as_com_ref::<IParameterChanges>()
                .map(|ptr| ptr.as_ptr())
                .unwrap_or(ptr::null_mut());

            // Prepare buffers
            self.prepare_buffers(&mut data)?;

            self.process_data = Some(data);
            Ok(())
        }
    }

    /// Prepare audio buffers based on plugin bus configuration
    unsafe fn prepare_buffers(&mut self, data: &mut HostProcessData) -> Result<()> {
        let input_bus_count = self.component.getBusCount(kAudio as i32, kInput as i32);
        let output_bus_count = self.component.getBusCount(kAudio as i32, kOutput as i32);
        data.input_bus_buffers.clear();
        data.output_bus_buffers.clear();

        let channel_count = |direction: i32, bus_idx: i32| {
            let mut bus_info: BusInfo = std::mem::zeroed();
            if self
                .component
                .getBusInfo(kAudio as i32, direction, bus_idx, &mut bus_info)
                == kResultOk
            {
                return bus_info.channelCount.max(0);
            }
            // A failed getBusInfo must not remove the slot and shift every later bus index.
            // Fall back to the processor's arrangement, whose set bits define its channels.
            let mut arrangement = 0u64;
            if self
                .processor
                .getBusArrangement(direction, bus_idx, &mut arrangement)
                == kResultOk
            {
                arrangement.count_ones() as i32
            } else {
                0
            }
        };

        for bus_idx in 0..input_bus_count {
            let mut bus: AudioBusBuffers = std::mem::zeroed();
            bus.numChannels = channel_count(kInput as i32, bus_idx);
            data.input_bus_buffers.push(bus);
        }
        for bus_idx in 0..output_bus_count {
            let mut bus: AudioBusBuffers = std::mem::zeroed();
            bus.numChannels = channel_count(kOutput as i32, bus_idx);
            data.output_bus_buffers.push(bus);
        }

        data.process_data.numInputs = data.input_bus_buffers.len() as i32;
        data.process_data.numOutputs = data.output_bus_buffers.len() as i32;

        match &mut data.sample_buffers {
            HostSampleBuffers::F32(samples) => {
                *samples = build_typed_sample_buffers(
                    self.block_size,
                    &data.input_bus_buffers,
                    &self.bus_activation.audio_inputs,
                    &data.output_bus_buffers,
                    &self.bus_activation.audio_outputs,
                );
                for (bus, pointers) in data
                    .input_bus_buffers
                    .iter_mut()
                    .zip(&mut samples.input_channel_ptrs.0)
                {
                    bus.__field0.channelBuffers32 = if pointers.is_empty() {
                        ptr::null_mut()
                    } else {
                        pointers.as_mut_ptr()
                    };
                }
                for (bus, pointers) in data
                    .output_bus_buffers
                    .iter_mut()
                    .zip(&mut samples.output_channel_ptrs.0)
                {
                    bus.__field0.channelBuffers32 = if pointers.is_empty() {
                        ptr::null_mut()
                    } else {
                        pointers.as_mut_ptr()
                    };
                }
            }
            HostSampleBuffers::F64(samples) => {
                *samples = build_typed_sample_buffers(
                    self.block_size,
                    &data.input_bus_buffers,
                    &self.bus_activation.audio_inputs,
                    &data.output_bus_buffers,
                    &self.bus_activation.audio_outputs,
                );
                for (bus, pointers) in data
                    .input_bus_buffers
                    .iter_mut()
                    .zip(&mut samples.input_channel_ptrs.0)
                {
                    bus.__field0.channelBuffers64 = if pointers.is_empty() {
                        ptr::null_mut()
                    } else {
                        pointers.as_mut_ptr()
                    };
                }
                for (bus, pointers) in data
                    .output_bus_buffers
                    .iter_mut()
                    .zip(&mut samples.output_channel_ptrs.0)
                {
                    bus.__field0.channelBuffers64 = if pointers.is_empty() {
                        ptr::null_mut()
                    } else {
                        pointers.as_mut_ptr()
                    };
                }
            }
        }

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

        log::debug!(
            "Prepared buffers: {} input buses, {} output buses, {} input channels, {} output channels",
            input_bus_count,
            output_bus_count,
            data.sample_buffers.input_channel_count(),
            data.sample_buffers.output_channel_count()
        );

        Ok(())
    }
}

impl PluginImpl {
    /// Process exactly `frames` samples starting at `frame_offset` within the caller's
    /// buffers. `frames` is always <= the configured `block_size`, which is what the plugin
    /// was set up to accept; `process` splits a larger caller block into successive chunks.
    ///
    /// The plugin-facing event list has already been staged with this chunk's events (see
    /// [`stage_chunk_events`]); queued parameter changes are selected the same way here, by the
    /// chunk their offset falls in. `is_last` marks the chunk that absorbs anything scheduled
    /// past the end of the caller's block.
    fn process_chunk(
        &mut self,
        buffers: &mut CallerAudioBuffers<'_>,
        frame_offset: usize,
        frames: usize,
        is_last: bool,
    ) -> Result<()> {
        if let Some(ref mut data) = self.process_data {
            unsafe {
                // Clear output events only - input events should be preserved for processing
                self.output_events.clear();

                // Clear the output parameter queue too. VST3's ProcessData::outputParameterChanges
                // describes changes for the *current* processing block only (see
                // Steinberg::Vst::ProcessData / IParameterChanges docs); the reference
                // ParameterChanges host helper exposes clearQueue() for exactly this reset.
                // Without this, addParameterData()/addPoint() would keep appending to queues
                // from prior blocks, mixing stale points into new ones, growing point storage
                // unbounded, and risking a reallocation on the audio thread long after warm-up.
                data.output_param_changes.clear_all();

                // VST3 allows numSamples to vary up to the maximum given to setupProcessing; the
                // caller's block may be shorter (BufferSize::Default gives variable sizes) or, if
                // it was longer, `process` has already split it into chunks of at most that
                // maximum.
                let frames = frames.min(self.block_size);
                data.process_data.numSamples = frames as i32;

                // Feed the queued parameter changes that belong to this chunk into the
                // processor's input queue, rebased to the chunk — the same routing the events
                // got in `stage_chunk_events`, so automation and MIDI stay aligned across a
                // split block.
                for pc in &self.pending_param_changes {
                    if let Some(off) = chunk_offset(pc.sample_offset, frame_offset, frames, is_last)
                    {
                        data.input_param_changes.enqueue(pc.id, off, pc.value);
                    }
                }

                // Route parameter edits the plugin's *own editor* reported via performEdit into
                // the processor too, so turning a knob in the plugin GUI affects the audio — not
                // just the host's display. (Some plugins relay editor→processor internally over
                // the component/controller connection; others rely on the host to do this. We do
                // it unconditionally; a plugin that also self-relays just gets the same value
                // twice in the same block, which is idempotent.) Drained here at offset 0 and
                // stashed for the host's display poll (get_parameter_changes).
                if let Some(ref handler) = self.component_handler {
                    drain_editor_edits(
                        &handler.parameter_changes,
                        &data.input_param_changes,
                        &self.gui_param_changes_for_host,
                    );
                }

                match buffers {
                    CallerAudioBuffers::Flat(buffers) => {
                        data.sample_buffers
                            .copy_inputs_from(&buffers.inputs, frame_offset, frames)
                    }
                    CallerAudioBuffers::Buses(buffers) => {
                        data.sample_buffers.copy_inputs_from_buses(
                            &buffers.inputs,
                            frame_offset,
                            frames,
                            &data.input_bus_buffers,
                            &self.bus_activation.audio_inputs,
                        )
                    }
                }
                data.sample_buffers.clear_outputs();
                data.sample_buffers.update_input_silence_flags(
                    &mut data.input_bus_buffers,
                    &self.bus_activation.audio_inputs,
                    frames,
                );
                prepare_output_silence_flags(
                    &mut data.output_bus_buffers,
                    &self.bus_activation.audio_outputs,
                );

                // Channel pointers and process-data input/output pointers were wired once in
                // prepare_buffers (buffer addresses are stable), so there's nothing to rebuild
                // here — keeping the steady-state path allocation-free.

                // A zero-sample flush carries events/parameter queues only. The VST3 process
                // contract requires no audio buses or pointers for that call.
                let saved_audio_io = hide_audio_io_for_zero_sample(&mut data.process_data, frames);
                if data.process_context.state & ProcessContext_::StatesAndFlags_::kSystemTimeValid
                    != 0
                {
                    data.process_context.systemTime = current_system_time_nanos();
                }
                self._host_app.enter_data_exchange_process();
                let process_result = self.processor.process(&mut data.process_data);
                self._host_app.leave_data_exchange_process();
                restore_process_audio_io(&mut data.process_data, saved_audio_io);

                // Everything from here to the output copy is per-block cleanup and MUST run even
                // when the plugin reported failure. Returning early instead would leave this
                // block's events and parameter changes queued, so the next block would re-deliver
                // every one of them — re-triggering held notes and growing the queues without
                // bound for as long as the plugin keeps failing.

                // Advance the transport so tempo-synced DSP (LFOs, sync'd delays/arps) sees
                // a moving playhead instead of a frozen time-0. The context describes the
                // block that was just processed; advancing here means the next block starts
                // at the new sample position.
                advance_process_context(
                    &mut data.process_context,
                    data.process_context_requirements,
                    data.transport_tempo,
                    frames as i64,
                );
                self.position.samples = data.process_context.projectTimeSamples;
                self.position.quarter_notes = data.process_context.projectTimeMusic;
                self.continuous_samples = data.process_context.continousTimeSamples;

                // Clear the staged input events AFTER processing, so the plugin got to see them
                // and the next chunk starts from an empty list.
                self.input_events.clear();
                // Clear the input parameter queue too, so this block's values don't
                // re-stick on the next block.
                data.input_param_changes.clear_all();

                // Processor-originated automation belongs to this block. Copy it into a
                // bounded lock-free feedback queue, then clear it on both success and failure
                // so stale points can never be re-reported.
                let feedback = &self.output_param_feedback;
                data.output_param_changes
                    .for_each_active_point(|id, _offset, value| {
                        feedback.force_push((id, value));
                    });
                data.output_param_changes.clear_all();

                // Capture any MIDI the plugin emitted this block (arpeggiators, MPE, etc.).
                // Drain the event list in place (no `mem::take`) and push each converted event
                // into the lock-free output queue with `force_push`, which drops the oldest event
                // when full. No lock and no allocation on the audio thread even while MIDI flows.
                if !self.output_events.is_empty() {
                    let out = &self.output_events_owned;
                    self.output_events.drain_each(|event| {
                        out.force_push(event);
                    });
                }

                if process_result != kResultOk {
                    // Leave the caller's buffers as they were (the playback bridges pre-fill them
                    // with silence) rather than copying out whatever the failed call left behind.
                    Err(Error::ProcessFailed(process_result))
                } else {
                    data.sample_buffers.update_output_silence_flags(
                        &mut data.output_bus_buffers,
                        &self.bus_activation.audio_outputs,
                        frames,
                    );
                    match buffers {
                        CallerAudioBuffers::Flat(buffers) => data.sample_buffers.copy_outputs_to(
                            &mut buffers.outputs,
                            frame_offset,
                            frames,
                            &data.output_bus_buffers,
                            &self.bus_activation.audio_outputs,
                        ),
                        CallerAudioBuffers::Buses(buffers) => {
                            data.sample_buffers.copy_outputs_to_buses(
                                &mut buffers.outputs,
                                frame_offset,
                                frames,
                                &data.output_bus_buffers,
                                &self.bus_activation.audio_outputs,
                            )
                        }
                    }
                    Ok(())
                }
            }
        } else {
            Err(Error::Other("Process data not initialized".to_string()))
        }
    }

    /// Distribute the block's queued input events over its chunks and process each one.
    ///
    /// `total` is the caller's block length, already known non-zero by the caller; the empty
    /// block is handled separately (some plugins use a zero-sample call to flush pending
    /// parameter changes).
    fn process_chunks(&mut self, buffers: &mut CallerAudioBuffers<'_>, total: usize) -> Result<()> {
        // `.max(1)`: a zero block size would make every chunk empty and never advance `offset`,
        // spinning forever on the audio thread. `Vst3HostBuilder::build` rejects 0, but
        // `block_size` is plain state and this loop must terminate regardless.
        let step = self.block_size.max(1);
        let mut offset = 0;
        while offset < total {
            let frames = (total - offset).min(step);
            let is_last = offset + frames >= total;
            stage_chunk_events(
                &mut self.chunk_events,
                &self.input_events,
                offset,
                frames,
                is_last,
            );
            self.process_chunk(buffers, offset, frames, is_last)?;
            offset += frames;
        }
        Ok(())
    }

    fn current_audio_bus_layout(&self) -> Result<AudioBusLayout> {
        let data = self
            .process_data
            .as_ref()
            .ok_or_else(|| Error::Other("Process data not initialized".to_string()))?;
        let side = |buses: &[AudioBusBuffers], active: &[bool]| {
            buses
                .iter()
                .enumerate()
                .map(|(index, bus)| AudioBusConfig {
                    channel_count: bus.numChannels.max(0) as usize,
                    active: active.get(index).copied().unwrap_or(false),
                })
                .collect()
        };
        Ok(AudioBusLayout {
            inputs: side(&data.input_bus_buffers, &self.bus_activation.audio_inputs),
            outputs: side(&data.output_bus_buffers, &self.bus_activation.audio_outputs),
        })
    }

    fn validate_bus_buffers(&self, buffers: &BusAudioBuffers) -> Result<()> {
        let data = self
            .process_data
            .as_ref()
            .ok_or_else(|| Error::Other("Process data not initialized".to_string()))?;
        let validate = |label: &str,
                        supplied: &[AudioBusBuffer],
                        expected: &[AudioBusBuffers],
                        active: &[bool]|
         -> Result<()> {
            if supplied.len() != expected.len() {
                return Err(Error::Other(format!(
                    "{label} bus count mismatch: expected {}, got {}",
                    expected.len(),
                    supplied.len()
                )));
            }
            for (index, (supplied, expected)) in supplied.iter().zip(expected).enumerate() {
                let expected_active = active.get(index).copied().unwrap_or(false);
                if supplied.active != expected_active {
                    return Err(Error::Other(format!(
                        "{label} bus {index} activation is stale: expected {expected_active}, got {}",
                        supplied.active
                    )));
                }
                let expected_channels = expected.numChannels.max(0) as usize;
                if supplied.channels.len() != expected_channels {
                    return Err(Error::Other(format!(
                        "{label} bus {index} channel count mismatch: expected {expected_channels}, got {}",
                        supplied.channels.len()
                    )));
                }
            }
            Ok(())
        };
        validate(
            "input",
            &buffers.inputs,
            &data.input_bus_buffers,
            &self.bus_activation.audio_inputs,
        )?;
        validate(
            "output",
            &buffers.outputs,
            &data.output_bus_buffers,
            &self.bus_activation.audio_outputs,
        )
    }

    fn process_buffer_view(&mut self, buffers: &mut CallerAudioBuffers<'_>) -> Result<()> {
        if !self.is_active || !self.is_processing {
            return Err(Error::NotProcessing);
        }

        let _denormal = crate::internal::denormal::DenormalGuard::new();
        let total = buffers.frame_count(self.block_size);
        self.input_events.take_into_slots(&mut self.chunk_events);

        let result = if total == 0 {
            stage_chunk_events(&mut self.chunk_events, &self.input_events, 0, 0, true);
            self.process_chunk(buffers, 0, 0, true)
        } else {
            self.process_chunks(buffers, total)
        };

        self.chunk_events.clear();
        self.pending_param_changes.clear();
        result
    }
}

impl PluginInternal for PluginImpl {
    fn midi_controller_assignments(
        &self,
        direction: crate::audio::BusDirection,
    ) -> Result<Vec<crate::midi::MidiControllerAssignment>> {
        Ok(match direction {
            crate::audio::BusDirection::Input => &self.midi_mapping_cache.mapped,
            crate::audio::BusDirection::Output => &self.midi_mapping_cache.output,
        }
        .clone())
    }
    fn send_midi_controller_at(
        &mut self,
        bus: i32,
        channel: MidiChannel,
        controller: crate::midi::MidiController,
        value: f64,
        offset: i32,
    ) -> Result<()> {
        // Copy one small assignment at a time: no allocation on the audio thread.
        for i in 0..self.midi_mapping_cache.mapped.len() {
            let mapping = self.midi_mapping_cache.mapped[i];
            if mapping.bus == bus && mapping.channel == channel && mapping.controller == controller
            {
                self.queue_processor_parameter_at(mapping.parameter_id, value, offset)?;
            }
        }
        Ok(())
    }
    fn notify_live_midi_controller(
        &mut self,
        bus: i32,
        channel: MidiChannel,
        controller: crate::midi::MidiController,
    ) -> Result<bool> {
        self.ensure_control_thread("MIDI learn notification")?;
        Ok(self
            .controller
            .as_ref()
            .is_some_and(|c| super::midi_mapping::learn(c, bus, channel, controller)))
    }
    fn send_live_midi_event(&mut self, event: MidiEvent) -> Result<()> {
        self.send_midi_event(event)?;
        if let Some(address) = event.controller_address() {
            self.live_controllers.force_push(address);
        }
        Ok(())
    }
    fn keyswitches(
        &self,
        bus: i32,
        channel: MidiChannel,
    ) -> Result<Vec<crate::midi::KeyswitchInfo>> {
        self.ensure_control_thread("keyswitch query")?;
        let Some(controller) = self
            .controller
            .as_ref()
            .and_then(|c| c.cast::<IKeyswitchController>())
        else {
            return Ok(Vec::new());
        };
        // SAFETY: queried on the UI thread; output structs are initialized and bounded.
        unsafe {
            let count = controller.getKeyswitchCount(bus, channel.as_index() as i16);
            if !(0..=4096).contains(&count) {
                return Err(Error::Other("invalid keyswitch count".into()));
            }
            let mut out = Vec::with_capacity(count as usize);
            for i in 0..count {
                let mut info: vst3::Steinberg::Vst::KeyswitchInfo = std::mem::zeroed();
                if controller.getKeyswitchInfo(bus, channel.as_index() as i16, i, &mut info)
                    == kResultOk
                {
                    out.push(crate::midi::KeyswitchInfo {
                        type_id: info.typeId,
                        title: super::utils::vst_string_to_string(&info.title),
                        short_title: super::utils::vst_string_to_string(&info.shortTitle),
                        key_min: info.keyswitchMin,
                        key_max: info.keyswitchMax,
                        remapped_key: (info.keyRemapped >= 0).then_some(info.keyRemapped),
                        unit_id: info.unitId,
                        flags: info.flags,
                    });
                }
            }
            Ok(out)
        }
    }

    fn set_transport_position(&mut self, position: crate::plugin::TransportPosition) -> Result<()> {
        self.position = position;
        if let Some(data) = self.process_data.as_mut() {
            data.process_context.projectTimeSamples = position.samples;
            data.process_context.projectTimeMusic = position.quarter_notes;
        }
        Ok(())
    }
    fn transport_position(&self) -> Result<crate::plugin::TransportPosition> {
        Ok(self.position)
    }

    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        self.set_parameter_at(id, value, 0)
    }

    fn set_parameter_at(&mut self, id: u32, value: f64, sample_offset: i32) -> Result<()> {
        if self.controller.is_none() {
            return Err(Error::InterfaceError("No controller available".to_string()));
        }
        // VST3 requires both halves: the controller (for GUI/display/formatting) and the
        // processor's input queue (so the change reaches the audio DSP) at `sample_offset` in
        // the next process() block. `queue_processor_parameter_at` owns both.
        self.queue_processor_parameter_at(id, value, sample_offset)
    }

    fn queue_processor_parameter_at(
        &mut self,
        id: u32,
        value: f64,
        sample_offset: i32,
    ) -> Result<()> {
        // Every route into the processor queue also updates the controller, so a value set
        // through `AudioHandle`/`RtControl` (or a mapped MIDI controller) is reflected by the
        // plugin's own editor, `get_parameter`, `format_parameter` and saved state — deferred
        // to the control thread when it arrives from the audio callback.
        self.mirror_parameter_to_controller(id, value);
        if self.pending_param_changes.len() >= MAX_PENDING_PARAM_CHANGES {
            // Only reachable when nothing is draining the queue (the plugin isn't
            // processing), so the dropped change would have arrived as part of a stale flood
            // anyway. The controller half above still ran, so the plugin's own display
            // stays correct.
            self.dropped_param_changes += 1;
            log::warn!(
                "dropping parameter change for {id}, queue full at \
                     {MAX_PENDING_PARAM_CHANGES} (is the plugin processing?); \
                     {} dropped so far",
                self.dropped_param_changes
            );
            return Ok(());
        }
        self.pending_param_changes.push(ParameterChange {
            id,
            value,
            sample_offset,
        });
        Ok(())
    }

    fn set_tempo(&mut self, bpm: f64) -> Result<()> {
        self.update_tempo(bpm);
        Ok(())
    }

    fn set_time_signature(&mut self, numerator: i32, denominator: i32) -> Result<()> {
        self.update_time_signature(numerator, denominator);
        Ok(())
    }

    fn set_playing(&mut self, playing: bool) -> Result<()> {
        self.update_playing(playing);
        Ok(())
    }

    fn get_parameter(&self, id: u32) -> Result<f64> {
        self.drain_deferred_controller_sync();
        if let Some(ref controller) = self.controller {
            unsafe { Ok(controller.getParamNormalized(id)) }
        } else {
            Err(Error::InterfaceError("No controller available".to_string()))
        }
    }

    fn get_all_parameters(&self) -> Result<Vec<Parameter>> {
        self.drain_deferred_controller_sync();
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
                                & ParameterInfo_::ParameterFlags_::kCanAutomate)
                                != 0,
                            is_read_only: (info.flags
                                & ParameterInfo_::ParameterFlags_::kIsReadOnly)
                                != 0,
                            is_bypass: (info.flags & ParameterInfo_::ParameterFlags_::kIsBypass)
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

    fn format_parameter(&self, id: u32, normalized: f64) -> Result<String> {
        self.drain_deferred_controller_sync();
        if let Some(ref controller) = self.controller {
            unsafe {
                let mut buf: String128 = std::mem::zeroed();
                if controller.getParamStringByValue(id, normalized, &mut buf) == kResultOk {
                    return Ok(crate::internal::utils::vst_string_to_string(&buf));
                }
            }
            Err(Error::InvalidParameter(format!(
                "Plugin could not format parameter {id}"
            )))
        } else {
            Err(Error::InterfaceError("No controller available".to_string()))
        }
    }

    fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        self.process_buffer_view(&mut CallerAudioBuffers::Flat(buffers))
    }

    fn audio_bus_layout(&self) -> Result<AudioBusLayout> {
        self.current_audio_bus_layout()
    }

    fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> Result<()> {
        self.validate_bus_buffers(buffers)?;
        self.process_buffer_view(&mut CallerAudioBuffers::Buses(buffers))
    }

    fn reconfigure(&mut self, sample_rate: f64, block_size: usize) -> Result<()> {
        if self.is_processing {
            return Err(Error::Other(
                "cannot reconfigure while processing".to_string(),
            ));
        }
        // VST3 requires the component to be inactive when setupProcessing is called.
        let was_active = self.is_active;
        if was_active {
            self.set_component_active(false);
            self.is_active = false;
        }

        let (old_sr, old_bs) = (self.sample_rate, self.block_size);
        self.sample_rate = sample_rate;
        self.block_size = block_size;

        // Re-run setupProcessing and rebuild process data / buffers for the new size. On
        // failure restore the cached config so it stays consistent with the still-current
        // (previous) process_data — `process_data` is only swapped in on success. The
        // component is left inactive; `start_processing` reactivates and re-runs setup.
        if let Err(e) = self.setup_processing() {
            self.sample_rate = old_sr;
            self.block_size = old_bs;
            return Err(e);
        }

        if was_active {
            let result = self.set_component_active(true);
            if result != kResultOk {
                return Err(Error::Other(format!(
                    "Failed to reactivate after reconfigure: {:#x}",
                    result
                )));
            }
            self.is_active = true;
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_cast)]
    fn set_process_mode(&mut self, mode: crate::plugin::ProcessMode) -> Result<()> {
        if self.is_processing {
            return Err(Error::Other(
                "cannot set process mode while processing".to_string(),
            ));
        }
        if mode == crate::plugin::ProcessMode::Prefetch {
            match self.prefetchable_support {
                Some(value)
                    if value == ePrefetchableSupport_::kIsNeverPrefetchable as u32
                        || value == ePrefetchableSupport_::kIsNotYetPrefetchable as u32 =>
                {
                    return Err(Error::Other(
                        "plugin currently reports that prefetch processing is unsupported"
                            .to_string(),
                    ));
                }
                // A processor without IPrefetchableSupport predates the policy interface. Keep
                // the legacy behaviour and let setupProcessing accept or reject kPrefetch.
                _ => {}
            }
        }
        // VST3 requires the component inactive for setupProcessing; mirror reconfigure.
        let was_active = self.is_active;
        if was_active {
            self.set_component_active(false);
            self.is_active = false;
        }

        // Store the mode first so setup_processing bakes it into BOTH ProcessSetup and
        // the freshly rebuilt process_data; restore it if setup fails so the cached mode
        // always reflects the last successfully-applied configuration.
        let old_mode = self.process_mode;
        self.process_mode = mode;
        if let Err(e) = self.setup_processing() {
            self.process_mode = old_mode;
            return Err(e);
        }

        if was_active {
            let result = self.set_component_active(true);
            if result != kResultOk {
                return Err(Error::Other(format!(
                    "Failed to reactivate after set_process_mode: {:#x}",
                    result
                )));
            }
            self.is_active = true;
        }
        Ok(())
    }

    fn bus_arrangements(&self) -> Result<crate::audio::BusArrangements> {
        use crate::audio::SpeakerArrangement;
        unsafe {
            let read = |dir: i32| -> Vec<SpeakerArrangement> {
                let count = self.component.getBusCount(kAudio as i32, dir);
                (0..count)
                    .map(|idx| {
                        let mut arr: u64 = 0;
                        self.processor.getBusArrangement(dir, idx, &mut arr);
                        SpeakerArrangement::from_raw(arr)
                    })
                    .collect()
            };
            Ok(crate::audio::BusArrangements {
                inputs: read(kInput as i32),
                outputs: read(kOutput as i32),
            })
        }
    }

    fn set_bus_arrangements(
        &mut self,
        inputs: &[crate::audio::SpeakerArrangement],
        outputs: &[crate::audio::SpeakerArrangement],
    ) -> Result<()> {
        if self.is_processing {
            return Err(Error::Other(
                "cannot set bus arrangements while processing".to_string(),
            ));
        }
        let mut in_raw: Vec<u64> = inputs.iter().map(|a| a.raw()).collect();
        let mut out_raw: Vec<u64> = outputs.iter().map(|a| a.raw()).collect();
        unsafe {
            // VST3 requires the component inactive for setBusArrangements + setupProcessing.
            let was_active = self.is_active;
            if was_active {
                self.set_component_active(false);
                self.is_active = false;
            }

            let arrangement_result = self.processor.setBusArrangements(
                in_raw.as_mut_ptr(),
                in_raw.len() as i32,
                out_raw.as_mut_ptr(),
                out_raw.len() as i32,
            );
            if arrangement_result == kResultFalse {
                if was_active {
                    let result = self.set_component_active(true);
                    self.is_active = result == kResultOk;
                }
                return Err(Error::Other(
                    "plugin declined the requested bus arrangements".to_string(),
                ));
            }
            if arrangement_result != kResultOk && arrangement_result != kResultTrue {
                if was_active {
                    let result = self.set_component_active(true);
                    self.is_active = result == kResultOk;
                }
                return Err(Error::InterfaceError(format!(
                    "setBusArrangements failed: {arrangement_result:#x}"
                )));
            }

            self.setup_processing()?;

            if was_active {
                let result = self.set_component_active(true);
                if result != kResultOk {
                    return Err(Error::Other(format!(
                        "Failed to reactivate after set_bus_arrangements: {:#x}",
                        result
                    )));
                }
                self.is_active = true;
            }
        }
        Ok(())
    }

    fn set_bus_active(
        &mut self,
        media_type: crate::audio::MediaType,
        direction: crate::audio::BusDirection,
        bus_index: i32,
        active: bool,
    ) -> Result<()> {
        use crate::audio::{BusDirection, MediaType};
        if self.is_processing {
            return Err(Error::Other(
                "cannot activate a bus while processing".to_string(),
            ));
        }
        // VST3 bus activation requires the component inactive (it's a setup-time operation).
        let media = match media_type {
            MediaType::Audio => kAudio as i32,
            MediaType::Event => kEvent as i32,
        };
        let dir = match direction {
            BusDirection::Input => kInput as i32,
            BusDirection::Output => kOutput as i32,
        };
        unsafe {
            let count = self.component.getBusCount(media, dir);
            if bus_index < 0 || bus_index >= count {
                return Err(Error::InvalidParameter(format!(
                    "bus index {bus_index} out of range for {media_type:?} {direction:?} \
                     bus (count {count})"
                )));
            }

            let was_active = self.is_active;
            if was_active {
                self.set_component_active(false);
                self.is_active = false;
            }

            let result = self
                .component
                .activateBus(media, dir, bus_index, active as u8);

            let update_result = if result == kResultOk {
                let states = match (media_type, direction) {
                    (MediaType::Audio, BusDirection::Input) => {
                        &mut self.bus_activation.audio_inputs
                    }
                    (MediaType::Audio, BusDirection::Output) => {
                        &mut self.bus_activation.audio_outputs
                    }
                    (MediaType::Event, BusDirection::Input) => {
                        &mut self.bus_activation.event_inputs
                    }
                    (MediaType::Event, BusDirection::Output) => {
                        &mut self.bus_activation.event_outputs
                    }
                };
                if let Some(state) = states.get_mut(bus_index as usize) {
                    *state = active;
                }
                if media_type == MediaType::Audio && self.applied_setup.is_some() {
                    self.create_process_data()
                } else {
                    Ok(())
                }
            } else {
                Err(Error::Other(format!(
                    "activateBus failed for {media_type:?} {direction:?} bus {bus_index}: \
                     {result:#x}"
                )))
            };

            if was_active {
                let reactivate = self.set_component_active(true);
                if reactivate != kResultOk {
                    return Err(Error::Other(format!(
                        "Failed to reactivate after set_bus_active: {reactivate:#x}"
                    )));
                }
                self.is_active = true;
            }

            update_result?;
        }
        Ok(())
    }

    fn send_midi_event(&mut self, event: MidiEvent) -> Result<()> {
        self.send_midi_event_at(event, 0)
    }

    fn send_midi_event_at(&mut self, event: MidiEvent, sample_offset: i32) -> Result<()> {
        // Floor to non-negative (the VST3 SDK treats a negative sampleOffset as undefined);
        // `process()` additionally clamps queued offsets to the actual block length.
        let sample_offset = sample_offset.max(0);
        unsafe {
            let mut vst_event: Event = std::mem::zeroed();
            vst_event.busIndex = 0;
            vst_event.sampleOffset = sample_offset;
            vst_event.ppqPosition = 0.0;
            vst_event.flags = Event_::EventFlags_::kIsLive as u16;

            match event {
                MidiEvent::NoteOn {
                    channel,
                    note,
                    velocity,
                } => {
                    let index = channel.as_index() as usize * 128 + note as usize;
                    let released = write_midi_note_on(&mut vst_event, channel, note, velocity);
                    self.ordinary_note_counts[index] = if released {
                        self.ordinary_note_counts[index].saturating_sub(1)
                    } else {
                        self.ordinary_note_counts[index].saturating_add(1)
                    };
                }
                MidiEvent::NoteOff {
                    channel,
                    note,
                    velocity,
                } => {
                    let index = channel.as_index() as usize * 128 + note as usize;
                    self.ordinary_note_counts[index] =
                        self.ordinary_note_counts[index].saturating_sub(1);
                    write_note_off_event(&mut vst_event, channel, note, velocity);
                }
                MidiEvent::ControlChange {
                    channel,
                    controller,
                    value,
                } => {
                    return self.queue_mapped_midi_controller(
                        channel,
                        controller as u16,
                        value as f64 / 127.0,
                        sample_offset,
                    );
                }
                MidiEvent::PitchBend { channel, value } => {
                    return self.queue_mapped_midi_controller(
                        channel,
                        ControllerNumbers_::kPitchBend as u16,
                        value as f64 / 16_383.0,
                        sample_offset,
                    );
                }
                MidiEvent::ChannelAftertouch { channel, pressure } => {
                    return self.queue_mapped_midi_controller(
                        channel,
                        ControllerNumbers_::kAfterTouch as u16,
                        pressure as f64 / 127.0,
                        sample_offset,
                    );
                }
                MidiEvent::PolyAftertouch {
                    channel,
                    note,
                    pressure,
                } => {
                    // Per-note pressure maps to a first-class VST3 poly-pressure event.
                    vst_event.r#type = kPolyPressureEvent as u16;
                    vst_event.__field0.polyPressure.channel = channel.as_index() as i16;
                    vst_event.__field0.polyPressure.pitch = note as i16;
                    vst_event.__field0.polyPressure.pressure = pressure as f32 / 127.0;
                    vst_event.__field0.polyPressure.noteId = -1;
                }
                MidiEvent::ProgramChange { channel, program } => {
                    // Prefer the channel-aware IMidiMapping route. Multi-timbral instruments
                    // commonly expose kCtrlProgramChange there without publishing their patch
                    // banks through IUnitInfo (OsTIrus is one example).
                    self.service_control_thread_caches();
                    let controller = ControllerNumbers_::kCtrlProgramChange as u16;
                    if self
                        .midi_mapping_cache
                        .get(0, channel.as_index() as i16, controller)
                        .is_some()
                    {
                        return self.queue_mapped_midi_controller(
                            channel,
                            controller,
                            program as f64 / 127.0,
                            sample_offset,
                        );
                    }

                    // Otherwise use the root unit's IUnitInfo program-change parameter. A MIDI
                    // channel cannot be mapped to a unit generically, so this compatibility path
                    // targets the conventional root unit regardless of channel.
                    if let Some(mapping) = self.cached_program_change(0) {
                        let index = program as i32;
                        if index < mapping.program_count {
                            let value = if mapping.program_count > 1 {
                                index as f64 / (mapping.program_count - 1) as f64
                            } else {
                                0.0
                            };
                            self.queue_processor_parameter_at(
                                mapping.param_id,
                                value,
                                sample_offset,
                            )?;
                        }
                    }
                    return Ok(());
                }
            }

            self.input_events.add_raw_event(&vst_event);
        }
        Ok(())
    }

    fn send_plugin_event(&mut self, event: PluginEvent) -> Result<()> {
        self.input_events.add_event(event);
        Ok(())
    }

    fn start_processing(&mut self) -> Result<()> {
        unsafe {
            // The configuration may have moved since the last setup (`set_audio_config` after
            // load, or a device change), and a plugin sizes its DSP buffers when it goes active,
            // from the ProcessSetup it was last given. So apply a changed configuration the way
            // `reconfigure` does — deactivate, set up, reactivate — rather than calling
            // setupProcessing on a running component, which VST3 forbids. An unchanged
            // configuration needs no setup at all: the plugin still has it.
            if self.applied_setup != Some(self.current_setup()) {
                if self.is_active {
                    self.set_component_active(false);
                    self.is_active = false;
                }
                self.setup_processing()?;
            }

            if !self.is_active {
                self.activate()?;
            }

            // Start processing. `setProcessing` is an optional notification — a plugin
            // may return kNotImplemented (e.g. u-he), which is not an error: it simply
            // doesn't need the start/stop signal and still processes audio normally.
            let result = self.processor.setProcessing(1);
            if result != kResultOk && result != kNotImplemented {
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
                self.set_component_active(false);
                self.is_active = false;
            }

            // Nothing can still be sounding, so no note-on is outstanding.
            self.active_notes.clear();

            Ok(())
        }
    }

    fn has_editor(&self) -> bool {
        // First check our cached value
        if self.info.has_gui {
            return true;
        }

        // An open editor is proof enough, and probing for a second view while one is attached
        // upsets plugins that assume a single live view.
        if self.plugin_view.is_some() {
            return true;
        }

        // Otherwise do a runtime check
        if let Some(ref controller) = self.controller {
            unsafe {
                // Check if controller can create an editor view
                let view_type = c"editor".as_ptr();
                let view_ptr = controller.createView(view_type);
                if !view_ptr.is_null() {
                    // Release the probe view; never call `removed()` on it — that pairs with
                    // `attached()`, and an unmatched `removed()` crashes some plugins that
                    // initialize their close state only on attach.
                    let _ = ComPtr::<IPlugView>::from_raw(view_ptr);
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
        if parent.is_null() {
            return Err(Error::Other(
                "editor parent window handle is null".to_string(),
            ));
        }

        if let Some(ref controller) = self.controller {
            unsafe {
                // Create editor view
                let view_type = c"editor".as_ptr();
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
                view_rect_size(&view_rect)?;

                // Hand the plugin an IPlugFrame (before attach, per the SDK) so it can
                // request host-side resizes; requests land in `editor_resize`.
                let frame = self.plug_frame.to_com_ptr::<IPlugFrame>().ok_or_else(|| {
                    Error::Other("Failed to create editor plug frame".to_string())
                })?;
                let frame_result = view.setFrame(frame.as_ptr());
                if frame_result != kResultOk && frame_result != kResultTrue {
                    return Err(Error::Other(format!(
                        "Plugin rejected editor plug frame: {frame_result:#x}"
                    )));
                }

                // Offer the current scale factor, but never let the answer decide whether the
                // editor opens: a view that declines simply renders at its own scale.
                let _ = set_view_scale_factor(&view, self.editor_scale_factor);

                // Platform-specific attachment
                #[cfg(target_os = "macos")]
                let platform_type = kPlatformTypeNSView;
                #[cfg(target_os = "windows")]
                let platform_type = kPlatformTypeHWND;
                #[cfg(target_os = "linux")]
                let platform_type = kPlatformTypeX11EmbedWindowID;
                #[cfg(target_os = "android")]
                {
                    view.setFrame(std::ptr::null_mut());
                    return Err(Error::Other(
                        "embedded VST3 editors are not supported on Android".to_string(),
                    ));
                }

                // Check platform support
                #[cfg(not(target_os = "android"))]
                if view.isPlatformTypeSupported(platform_type) != kResultOk {
                    view.setFrame(std::ptr::null_mut());
                    return Err(Error::Other("Platform type not supported".to_string()));
                }

                // Attach to parent window
                #[cfg(not(target_os = "android"))]
                let attach_result = view.attached(parent, platform_type);
                #[cfg(not(target_os = "android"))]
                if attach_result != kResultOk {
                    view.setFrame(std::ptr::null_mut());
                    return Err(Error::Other(format!(
                        "Failed to attach view: {:#x}",
                        attach_result
                    )));
                }

                #[cfg(not(target_os = "android"))]
                {
                    self.plugin_view = Some(view);
                    Ok(())
                }
            }
        } else {
            Err(Error::Other("No controller available".to_string()))
        }
    }

    fn close_editor(&mut self) -> Result<()> {
        let mut close_error = None;
        if let Some(view) = self.plugin_view.take() {
            unsafe {
                let removed_result = view.removed();
                // Break the view -> host-frame reference before releasing the view. The frame
                // intentionally never retains the view, so this also avoids a COM retain cycle.
                let frame_result = view.setFrame(std::ptr::null_mut());
                if removed_result != kResultOk && removed_result != kResultTrue {
                    close_error = Some(Error::Other(format!(
                        "Failed to detach editor view: {removed_result:#x}"
                    )));
                } else if frame_result != kResultOk && frame_result != kResultTrue {
                    close_error = Some(Error::Other(format!(
                        "Failed to clear editor plug frame: {frame_result:#x}"
                    )));
                }
            }
        }
        // Drop any run-loop registrations the editor left behind. A well-behaved plugin
        // unregisters its own handlers and timers during `removed()`, but one that doesn't would
        // otherwise leave live ComPtrs into a view that no longer exists — and since
        // `Plugin::service_run_loop` is public with no editor-open guard, a host driving it from
        // its frame loop would then dispatch straight into the removed view.
        #[cfg(target_os = "linux")]
        if let Ok(mut reg) = self.run_loop.lock() {
            reg.handlers.clear();
            reg.timers.clear();
        }
        close_error.map_or(Ok(()), Err)
    }

    #[cfg(target_os = "linux")]
    fn service_run_loop(&mut self) {
        use vst3::Steinberg::Linux::{IEventHandlerTrait, ITimerHandlerTrait};

        // Fire due timers. Snapshot the handlers, then invoke with the lock
        // RELEASED: a callback may re-enter registerTimer/unregisterTimer
        // (VSTGUI does), which takes the same lock.
        let now = std::time::Instant::now();
        let mut due = Vec::new();
        if let Ok(mut reg) = self.run_loop.lock() {
            for timer in reg.timers.iter_mut() {
                if now >= timer.due {
                    timer.due = now + std::time::Duration::from_millis(timer.interval_ms);
                    due.push(timer.handler.clone());
                }
            }
        }
        for handler in due {
            unsafe { handler.onTimer() };
        }

        // Poll registered fds (zero timeout - never blocks the UI thread)
        // and notify ready ones. Same snapshot-then-invoke pattern.
        let handlers: Vec<_> = match self.run_loop.lock() {
            Ok(reg) => reg.handlers.clone(),
            Err(_) => return,
        };
        if handlers.is_empty() {
            return;
        }
        let mut fds: Vec<libc::pollfd> = handlers
            .iter()
            .map(|&(_, fd)| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 0) };
        if ready > 0 {
            for (pfd, (handler, fd)) in fds.iter().zip(handlers.iter()) {
                if pfd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
                    unsafe { handler.onFDIsSet(*fd) };
                }
            }
        }
    }

    fn get_editor_size(&self) -> Result<(i32, i32)> {
        if let Some(view) = self.plugin_view.as_ref() {
            unsafe {
                let mut view_rect = ViewRect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if view.getSize(&mut view_rect) != kResultOk {
                    return Err(Error::Other("Failed to query open editor size".to_string()));
                }
                return view_rect_size(&view_rect);
            }
        }

        if let Some(ref controller) = self.controller {
            unsafe {
                // Create a temporary view to get size
                let view_type = c"editor".as_ptr();
                let view_ptr = controller.createView(view_type);
                if view_ptr.is_null() {
                    return Err(Error::Other(
                        "Failed to create view for size query".to_string(),
                    ));
                }

                let view = ComPtr::<IPlugView>::from_raw(view_ptr)
                    .ok_or_else(|| Error::Other("Failed to wrap plug view".to_string()))?;

                // Get view size
                let mut view_rect = ViewRect {
                    left: 0,
                    top: 0,
                    right: 400,
                    bottom: 300,
                };

                let result = view.getSize(&mut view_rect);

                // Released when `view` drops; do not call `removed()` — it pairs with
                // `attached()`, which this probe never calls.

                if result == kResultOk {
                    view_rect_size(&view_rect)
                } else {
                    Ok((800, 600)) // Default size
                }
            }
        } else {
            Err(Error::Other("No controller available".to_string()))
        }
    }

    fn is_editor_open(&self) -> bool {
        self.plugin_view.is_some()
    }

    fn attached_editor_size(&self) -> Result<Option<(i32, i32)>> {
        let Some(view) = self.plugin_view.as_ref() else {
            return Ok(None);
        };

        unsafe {
            let mut view_rect = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if view.getSize(&mut view_rect) != kResultOk {
                return Err(Error::Other(
                    "Failed to query attached editor size".to_string(),
                ));
            }
            view_rect_size(&view_rect).map(Some)
        }
    }

    fn attached_editor_can_resize(&self) -> Option<bool> {
        self.plugin_view
            .as_ref()
            .map(|view| unsafe { view.canResize() == kResultTrue })
    }

    fn editor_can_resize(&self) -> bool {
        unsafe {
            if let Some(view) = self.plugin_view.as_ref() {
                return view.canResize() == kResultTrue;
            }

            let Some(controller) = self.controller.as_ref() else {
                return false;
            };
            let view_ptr = controller.createView(c"editor".as_ptr());
            let Some(view) = ComPtr::<IPlugView>::from_raw(view_ptr) else {
                return false;
            };
            view.canResize() == kResultTrue
        }
    }

    fn resize_editor(&mut self, width: i32, height: i32) -> Result<(i32, i32)> {
        if width <= 0 || height <= 0 {
            return Err(Error::Other(
                "editor dimensions must be greater than zero".to_string(),
            ));
        }
        let view = self
            .plugin_view
            .as_ref()
            .ok_or_else(|| Error::Other("Plugin editor is not open".to_string()))?;

        unsafe {
            if view.canResize() != kResultTrue {
                let mut current = ViewRect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if view.getSize(&mut current) != kResultOk {
                    return Err(Error::Other(
                        "Plugin editor is fixed-size and its size could not be queried".to_string(),
                    ));
                }
                return view_rect_size(&current);
            }

            let mut requested = ViewRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            // The view constrains `requested` in place. `kResultFalse` means it left the rect
            // alone (nothing to constrain, or it wants a different size than asked for) — a
            // refusal to adapt, not a broken call — so take whatever rect it ended up with and
            // only reject result codes that mean the call itself failed.
            let constraint_result = view.checkSizeConstraint(&mut requested);
            let constrained = constraint_result == kResultOk
                || constraint_result == kResultTrue
                || constraint_result == kResultFalse
                || constraint_result == kNotImplemented;
            if !constrained {
                return Err(Error::Other(format!(
                    "Plugin failed to check the editor size constraint: {constraint_result:#x}"
                )));
            }
            let accepted = view_rect_size(&requested)?;

            // The SDK calls onSize only when the size actually changes; re-sending the current
            // one makes VSTGUI-based editors rebuild their frame for nothing.
            let mut current = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if view.getSize(&mut current) == kResultOk
                && view_rect_size(&current).ok() == Some(accepted)
            {
                return Ok(accepted);
            }

            let resize_result = view.onSize(&mut requested);
            if resize_result != kResultOk && resize_result != kResultTrue {
                return Err(Error::Other(format!(
                    "Plugin rejected editor resize: {resize_result:#x}"
                )));
            }
            Ok(accepted)
        }
    }

    fn set_editor_scale_factor(&mut self, factor: f32) -> Result<bool> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(Error::Other(
                "editor scale factor must be finite and greater than zero".to_string(),
            ));
        }
        let supported = match self.plugin_view.as_ref() {
            Some(view) => unsafe { set_view_scale_factor(view, factor)? },
            None => false,
        };
        self.editor_scale_factor = factor;
        Ok(supported)
    }

    fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
        self.get_parameter_changes()
    }

    fn take_parameter_edits(&mut self) -> Vec<crate::plugin::ParameterEdit> {
        self.service_control_thread_caches();
        self.component_handler
            .as_ref()
            .map(|h| h.take_parameter_edits())
            .unwrap_or_default()
    }

    fn take_host_notifications(&mut self) -> Vec<crate::plugin::HostNotification> {
        let notifications = self
            .component_handler
            .as_ref()
            .map(|handler| handler.take_host_notifications())
            .unwrap_or_default();
        if notifications
            .iter()
            .any(crate::plugin::HostNotification::invalidates_unit_cache)
        {
            *self
                .unit_cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = None;
            // Mark the program-change table stale rather than clearing it: an emptied table
            // silently turns every MIDI ProgramChange into a no-op until something else happens
            // to rebuild it, whereas a stale table still routes to the previous parameter.
            self.dirty_caches.program_change = true;
        }
        self.service_control_thread_caches();
        notifications
    }

    fn take_data_exchange_blocks(&mut self) -> Vec<crate::plugin::DataExchangeBlock> {
        self._host_app.take_data_exchange_blocks()
    }

    fn execute_context_menu_item(&mut self, menu_id: u64, item_id: u32) -> Result<()> {
        self.component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?
            .execute_context_menu_item(menu_id, item_id)
    }

    fn dismiss_context_menu(&mut self, menu_id: u64) -> Result<()> {
        self.component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?
            .dismiss_context_menu(menu_id)
    }

    fn take_restart_flags(&mut self) -> crate::plugin::RestartFlags {
        let flags = self
            .component_handler
            .as_ref()
            .map(|h| h.take_restart_flags())
            .unwrap_or_default();
        let bits = flags.bits();
        if bits & (RestartFlags_::kParamTitlesChanged | RestartFlags_::kReloadComponent) != 0 {
            *self.unit_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
        // `restartComponent` can arrive on any thread, and rebuilding these tables is a burst of
        // main-thread-domain controller calls (the MIDI map alone is buses × 16 × 130 of them).
        // So record what went stale and let the control thread rebuild.
        if bits & RestartFlags_::kMidiCCAssignmentChanged != 0 {
            self.dirty_caches.midi_mapping = true;
        }
        if bits & (RestartFlags_::kParamIDMappingChanged | RestartFlags_::kParamTitlesChanged) != 0
        {
            self.dirty_caches.midi_mapping = true;
            self.dirty_caches.program_change = true;
        }
        self.service_control_thread_caches();
        flags
    }

    fn service_host_requests(&mut self) -> Result<crate::plugin::RestartFlags> {
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "restart requests must be serviced on the plugin control thread".to_string(),
            ));
        }

        for _ in 0..self.live_controllers.capacity() {
            let Some((channel, controller)) = self.live_controllers.pop() else {
                break;
            };
            let _ = self.notify_live_midi_controller(0, channel, controller)?;
        }
        let flags = self.take_restart_flags();
        if !flags.latency_changed() && !flags.io_changed() {
            return Ok(flags);
        }

        unsafe {
            let was_processing = self.is_processing;
            let was_active = self.is_active;

            if was_processing {
                let result = self.processor.setProcessing(0);
                if result != kResultOk && result != kNotImplemented {
                    return Err(Error::Other(format!(
                        "failed to suspend processing for restartComponent: {result:#x}"
                    )));
                }
                self.is_processing = false;
            }

            if was_active {
                let result = self.set_component_active(false);
                if result != kResultOk {
                    if was_processing {
                        let resume = self.processor.setProcessing(1);
                        self.is_processing = resume == kResultOk || resume == kNotImplemented;
                    }
                    return Err(Error::Other(format!(
                        "failed to deactivate for restartComponent: {result:#x}"
                    )));
                }
                self.is_active = false;
            }

            let apply_result = if flags.io_changed() {
                match Self::activate_default_buses(&self.component) {
                    Ok(activation) => {
                        self.bus_activation = activation;
                        self.negotiate_default_bus_arrangements()
                            .and_then(|()| self.setup_processing())
                    }
                    Err(error) => Err(error),
                }
            } else {
                Ok(())
            };

            self.latency = self.processor.getLatencySamples();
            self.tail = self.processor.getTailSamples();
            let reactivate_result = if was_active { self.activate() } else { Ok(()) };

            let resume_result = if was_processing && reactivate_result.is_ok() {
                let result = self.processor.setProcessing(1);
                if result == kResultOk || result == kNotImplemented {
                    self.is_processing = true;
                    Ok(())
                } else {
                    Err(Error::Other(format!(
                        "failed to resume processing after restartComponent: {result:#x}"
                    )))
                }
            } else {
                Ok(())
            };

            apply_result?;
            reactivate_result?;
            resume_result?;
        }

        Ok(flags)
    }

    fn take_output_events(&self) -> Vec<PluginEvent> {
        let mut out = Vec::new();
        while let Some(event) = self.output_events_owned.pop() {
            out.push(event);
        }
        out
    }

    fn output_midi_handle(&self) -> Option<crate::plugin::OutputMidiConsumer> {
        Some(crate::plugin::OutputMidiConsumer::from_queue(
            self.output_events_owned.clone(),
        ))
    }

    fn output_event_handle(&self) -> Option<crate::plugin::OutputEventConsumer> {
        Some(crate::plugin::OutputEventConsumer::from_queue(
            self.output_events_owned.clone(),
        ))
    }

    fn take_editor_resize_request(&self) -> Option<(i32, i32)> {
        self.editor_resize.lock().ok().and_then(|mut s| s.take())
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn tail_samples(&self) -> u32 {
        self.tail
    }

    fn midi_cc_to_parameter(&self, bus: i32, channel: i16, cc: u16) -> Option<u32> {
        self.midi_mapping_cache.get(bus, channel, cc)
    }

    fn note_on(
        &mut self,
        channel: MidiChannel,
        note: u8,
        velocity: u8,
        sample_offset: i32,
    ) -> Result<crate::midi::NoteId> {
        let id = self.next_note_id;
        self.next_note_id = self.next_note_id.wrapping_add(1).max(1);
        // Remember which (channel, pitch) this id stands for. VST3 note-off carries both the
        // noteId *and* the pitch/channel, and plugins that don't track note ids (most non-MPE
        // synths) match the release by pitch — so `note_off` has to reproduce them.
        if self.active_notes.len() >= MAX_TRACKED_NOTES {
            // Full only if a caller started notes it never released (a MIDI panic sends CCs, not
            // note-offs, so entries can also be left behind that way). Evict one in O(1) rather
            // than refusing to track, so new notes keep getting correct releases.
            self.active_notes.swap_remove(0);
        }
        self.active_notes
            .push((id, channel.as_index() as i16, note as i16));
        unsafe {
            let mut ev: Event = std::mem::zeroed();
            ev.busIndex = 0;
            ev.sampleOffset = sample_offset.max(0);
            ev.flags = Event_::EventFlags_::kIsLive as u16;
            ev.r#type = kNoteOnEvent as u16;
            ev.__field0.noteOn.channel = channel.as_index() as i16;
            ev.__field0.noteOn.pitch = note as i16;
            ev.__field0.noteOn.velocity = velocity as f32 / 127.0;
            ev.__field0.noteOn.noteId = id;
            self.input_events.add_raw_event(&ev);
        }
        Ok(crate::midi::NoteId(id))
    }

    fn note_off(&mut self, id: crate::midi::NoteId, sample_offset: i32) -> Result<()> {
        // Recover the note's channel and pitch from the note-on. Without them the event carries
        // pitch 0 on channel 1, which any synth matching releases by pitch ignores — leaving the
        // real note sounding forever.
        let tracked = self
            .active_notes
            .iter()
            .position(|&(tracked_id, _, _)| tracked_id == id.0)
            .map(|i| self.active_notes.swap_remove(i));
        unsafe {
            let mut ev: Event = std::mem::zeroed();
            ev.busIndex = 0;
            ev.sampleOffset = sample_offset.max(0);
            ev.flags = Event_::EventFlags_::kIsLive as u16;
            ev.r#type = kNoteOffEvent as u16;
            ev.__field0.noteOff.noteId = id.0;
            if let Some((_, channel, pitch)) = tracked {
                ev.__field0.noteOff.channel = channel;
                ev.__field0.noteOff.pitch = pitch;
            }
            // A release velocity of 0 is the SDK's own default for "unspecified".
            self.input_events.add_raw_event(&ev);
        }
        Ok(())
    }

    fn send_note_expression(
        &mut self,
        id: crate::midi::NoteId,
        kind: crate::midi::NoteExpressionType,
        value: f64,
        sample_offset: i32,
    ) -> Result<()> {
        unsafe {
            let mut ev: Event = std::mem::zeroed();
            ev.busIndex = 0;
            ev.sampleOffset = sample_offset.max(0);
            ev.flags = Event_::EventFlags_::kIsLive as u16;
            ev.r#type = kNoteExpressionValueEvent as u16;
            ev.__field0.noteExpressionValue.typeId = kind.type_id();
            ev.__field0.noteExpressionValue.noteId = id.0;
            ev.__field0.noteExpressionValue.value = value.clamp(0.0, 1.0);
            self.input_events.add_raw_event(&ev);
        }
        Ok(())
    }

    fn note_expressions(
        &self,
        bus: i32,
        channel: i16,
    ) -> Result<Vec<crate::midi::NoteExpressionInfo>> {
        use crate::midi::{NoteExpressionInfo, NoteExpressionType};
        let Some(ctrl) = self
            .controller
            .as_ref()
            .and_then(|c| c.cast::<INoteExpressionController>())
        else {
            return Ok(Vec::new());
        };
        unsafe {
            let count = ctrl.getNoteExpressionCount(bus, channel);
            let mut out = Vec::with_capacity(count.max(0) as usize);
            for i in 0..count {
                let mut info: NoteExpressionTypeInfo = std::mem::zeroed();
                if ctrl.getNoteExpressionInfo(bus, channel, i, &mut info) == kResultOk {
                    use NoteExpressionTypeInfo_::NoteExpressionTypeFlags_ as Flags;
                    let flags = info.flags;
                    out.push(NoteExpressionInfo {
                        kind: NoteExpressionType::from_type_id(info.typeId),
                        title: crate::internal::utils::vst_string_to_string(&info.title),
                        short_title: crate::internal::utils::vst_string_to_string(&info.shortTitle),
                        units: crate::internal::utils::vst_string_to_string(&info.units),
                        default_value: info.valueDesc.defaultValue,
                        min: info.valueDesc.minimum,
                        max: info.valueDesc.maximum,
                        step_count: info.valueDesc.stepCount,
                        is_bipolar: flags & Flags::kIsBipolar as i32 != 0,
                        is_one_shot: flags & Flags::kIsOneShot as i32 != 0,
                        is_absolute: flags & Flags::kIsAbsolute as i32 != 0,
                    });
                }
            }
            Ok(out)
        }
    }

    fn get_units(&self) -> Result<Vec<crate::plugin::PluginUnit>> {
        use crate::plugin::PluginUnit;
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "unit metadata must be queried on the plugin control thread".to_string(),
            ));
        }
        if let Some(units) = self
            .unit_cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .cloned()
        {
            return Ok(units);
        }
        let Some(ref controller) = self.controller else {
            return Ok(Vec::new());
        };
        // IUnitInfo is optional; plugins without it (no units/program lists) return empty.
        let Some(unit_info) = controller.cast::<IUnitInfo>() else {
            return Ok(Vec::new());
        };
        unsafe {
            // First resolve program lists (id -> program names), then attach to units.
            let mut lists: std::collections::HashMap<i32, Vec<String>> =
                std::collections::HashMap::new();
            let list_count = unit_info.getProgramListCount();
            for i in 0..list_count {
                let mut pl: ProgramListInfo = std::mem::zeroed();
                if unit_info.getProgramListInfo(i, &mut pl) != kResultOk {
                    continue;
                }
                let mut programs = Vec::with_capacity(pl.programCount.max(0) as usize);
                for p in 0..pl.programCount {
                    let mut name: String128 = std::mem::zeroed();
                    let s = if unit_info.getProgramName(pl.id, p, &mut name) == kResultOk {
                        crate::internal::utils::vst_string_to_string(&name)
                    } else {
                        String::new()
                    };
                    programs.push(s);
                }
                lists.insert(pl.id, programs);
            }

            let unit_count = unit_info.getUnitCount();
            let mut units = Vec::with_capacity(unit_count.max(0) as usize);
            for i in 0..unit_count {
                let mut ui: UnitInfo = std::mem::zeroed();
                if unit_info.getUnitInfo(i, &mut ui) != kResultOk {
                    continue;
                }
                let programs = lists.get(&ui.programListId).cloned().unwrap_or_default();
                units.push(PluginUnit {
                    id: ui.id,
                    parent_id: ui.parentUnitId,
                    name: crate::internal::utils::vst_string_to_string(&ui.name),
                    program_list_id: (ui.programListId >= 0).then_some(ui.programListId),
                    programs,
                });
            }
            *self
                .unit_cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(units.clone());
            Ok(units)
        }
    }

    fn select_program(&mut self, unit_id: i32, program_index: i32) -> Result<()> {
        self.service_control_thread_caches();
        if self.cached_program_change(unit_id).is_none()
            && thread::current().id() == self.control_thread
        {
            self.refresh_program_change_cache();
        }
        let mapping = self.cached_program_change(unit_id).ok_or_else(|| {
            Error::InvalidParameter(format!(
                "unknown unit {unit_id} or no program-change parameter"
            ))
        })?;
        let param_id = mapping.param_id;
        let program_count = mapping.program_count;
        if program_index < 0 || program_index >= program_count {
            return Err(Error::InvalidParameter(format!(
                "program index {program_index} out of range for unit {unit_id} \
                 ({program_count} programs)"
            )));
        }
        // VST3 maps a discrete program list onto the program-change parameter's normalized
        // 0..1 range: index N of a count-C list is N / (C-1). A single-program list maps to 0.
        let normalized = if program_count > 1 {
            program_index as f64 / (program_count - 1) as f64
        } else {
            0.0
        };
        // Drive both halves (controller for display, processor queue for the DSP), exactly as
        // set_parameter does — a program change is just a parameter change in VST3.
        self.set_parameter_at(param_id, normalized, 0)
    }

    fn selected_unit(&self) -> Result<Option<i32>> {
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "selected unit must be queried on the plugin control thread".to_string(),
            ));
        }
        let Some(unit_info) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitInfo>())
        else {
            return Ok(None);
        };
        let unit_id = unsafe { unit_info.getSelectedUnit() };
        Ok((unit_id >= 0).then_some(unit_id))
    }

    fn select_unit(&mut self, unit_id: i32) -> Result<()> {
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "unit selection must run on the plugin control thread".to_string(),
            ));
        }
        let unit_info = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitInfo>())
            .ok_or_else(|| Error::Other("plugin does not implement IUnitInfo".to_string()))?;
        let result = unsafe { unit_info.selectUnit(unit_id) };
        if result == kResultOk {
            Ok(())
        } else {
            Err(Error::InvalidParameter(format!(
                "plugin rejected unit {unit_id}: {result:#x}"
            )))
        }
    }

    fn program_pitch_names(
        &self,
        program_list_id: i32,
        program_index: i32,
    ) -> Result<Vec<crate::plugin::ProgramPitchName>> {
        if program_index < 0 {
            return Err(Error::InvalidParameter(format!(
                "program index must be non-negative, got {program_index}"
            )));
        }
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "program pitch names must be queried on the plugin control thread".to_string(),
            ));
        }
        let Some(unit_info) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitInfo>())
        else {
            return Ok(Vec::new());
        };
        unsafe {
            if unit_info.hasProgramPitchNames(program_list_id, program_index) != kResultTrue {
                return Ok(Vec::new());
            }
            let mut names = Vec::new();
            for midi_pitch in 0_i16..=127 {
                let mut name: String128 = std::mem::zeroed();
                if unit_info.getProgramPitchName(
                    program_list_id,
                    program_index,
                    midi_pitch,
                    &mut name,
                ) == kResultOk
                {
                    names.push(crate::plugin::ProgramPitchName {
                        midi_pitch,
                        name: crate::internal::utils::vst_string_to_string(&name),
                    });
                }
            }
            Ok(names)
        }
    }

    fn get_program_data(
        &self,
        program_list_id: i32,
        program_index: i32,
    ) -> Result<Option<Vec<u8>>> {
        if program_index < 0 {
            return Err(Error::InvalidParameter(format!(
                "program index must be non-negative, got {program_index}"
            )));
        }
        self.ensure_control_thread("program data read")?;
        let Some(data_interface) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IProgramListData>())
        else {
            return Ok(None);
        };
        unsafe {
            if data_interface.programDataSupported(program_list_id) != kResultTrue {
                return Ok(None);
            }
            let stream = create_memory_stream_with_metadata(None, StreamStateType::TrackPreset);
            let stream_ptr = stream
                .as_com_ref::<IBStream>()
                .ok_or_else(|| Error::InterfaceError("failed to create IBStream".to_string()))?;
            let result =
                data_interface.getProgramData(program_list_id, program_index, stream_ptr.as_ptr());
            if result == kResultOk {
                Ok(Some(stream.to_vec()))
            } else {
                Err(Error::Other(format!(
                    "IProgramListData::getProgramData failed: {result:#x}"
                )))
            }
        }
    }

    fn set_program_data(
        &mut self,
        program_list_id: i32,
        program_index: i32,
        data: &[u8],
    ) -> Result<()> {
        if program_index < 0 {
            return Err(Error::InvalidParameter(format!(
                "program index must be non-negative, got {program_index}"
            )));
        }
        self.ensure_stream_size(data)?;
        self.ensure_control_thread("program data restore")?;
        let data_interface = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IProgramListData>())
            .ok_or_else(|| {
                Error::Other("plugin does not implement IProgramListData".to_string())
            })?;
        unsafe {
            if data_interface.programDataSupported(program_list_id) != kResultTrue {
                return Err(Error::Other(format!(
                    "plugin does not support data for program list {program_list_id}"
                )));
            }
            let stream = create_memory_stream_from_with_metadata(
                data.to_vec(),
                None,
                StreamStateType::TrackPreset,
            );
            let stream_ptr = stream
                .as_com_ref::<IBStream>()
                .ok_or_else(|| Error::InterfaceError("failed to create IBStream".to_string()))?;
            let result =
                data_interface.setProgramData(program_list_id, program_index, stream_ptr.as_ptr());
            if result == kResultOk {
                *self
                    .unit_cache
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = None;
                self.refresh_program_change_cache();
                Ok(())
            } else {
                Err(Error::Other(format!(
                    "IProgramListData::setProgramData failed: {result:#x}"
                )))
            }
        }
    }

    fn get_unit_data(&self, unit_id: i32) -> Result<Option<Vec<u8>>> {
        self.ensure_control_thread("unit data read")?;
        let Some(data_interface) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitData>())
        else {
            return Ok(None);
        };
        unsafe {
            if data_interface.unitDataSupported(unit_id) != kResultTrue {
                return Ok(None);
            }
            let stream = create_memory_stream_with_metadata(None, StreamStateType::TrackPreset);
            let stream_ptr = stream
                .as_com_ref::<IBStream>()
                .ok_or_else(|| Error::InterfaceError("failed to create IBStream".to_string()))?;
            let result = data_interface.getUnitData(unit_id, stream_ptr.as_ptr());
            if result == kResultOk {
                Ok(Some(stream.to_vec()))
            } else {
                Err(Error::Other(format!(
                    "IUnitData::getUnitData failed: {result:#x}"
                )))
            }
        }
    }

    fn set_unit_data(&mut self, unit_id: i32, data: &[u8]) -> Result<()> {
        self.ensure_stream_size(data)?;
        self.ensure_control_thread("unit data restore")?;
        let data_interface = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitData>())
            .ok_or_else(|| Error::Other("plugin does not implement IUnitData".to_string()))?;
        unsafe {
            if data_interface.unitDataSupported(unit_id) != kResultTrue {
                return Err(Error::Other(format!(
                    "plugin does not support data for unit {unit_id}"
                )));
            }
            let stream = create_memory_stream_from_with_metadata(
                data.to_vec(),
                None,
                StreamStateType::TrackPreset,
            );
            let stream_ptr = stream
                .as_com_ref::<IBStream>()
                .ok_or_else(|| Error::InterfaceError("failed to create IBStream".to_string()))?;
            let result = data_interface.setUnitData(unit_id, stream_ptr.as_ptr());
            if result == kResultOk {
                *self
                    .unit_cache
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = None;
                self.refresh_program_change_cache();
                Ok(())
            } else {
                Err(Error::Other(format!(
                    "IUnitData::setUnitData failed: {result:#x}"
                )))
            }
        }
    }

    fn begin_host_edit(&mut self, parameter_id: u32) -> Result<()> {
        self.ensure_control_thread("host edit begin")?;
        let editing = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IEditControllerHostEditing>())
            .ok_or_else(|| {
                Error::Other("plugin does not implement IEditControllerHostEditing".to_string())
            })?;
        Self::check_controller_result(
            unsafe { editing.beginEditFromHost(parameter_id) },
            "IEditControllerHostEditing::beginEditFromHost",
        )
    }

    fn end_host_edit(&mut self, parameter_id: u32) -> Result<()> {
        self.ensure_control_thread("host edit end")?;
        let editing = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IEditControllerHostEditing>())
            .ok_or_else(|| {
                Error::Other("plugin does not implement IEditControllerHostEditing".to_string())
            })?;
        Self::check_controller_result(
            unsafe { editing.endEditFromHost(parameter_id) },
            "IEditControllerHostEditing::endEditFromHost",
        )
    }

    fn send_midi_learn(&mut self, bus: i32, channel: i16, controller: u16) -> Result<()> {
        if bus < 0 || !(0..16).contains(&i32::from(channel)) || controller > 129 {
            return Err(Error::InvalidParameter(format!(
                "invalid live MIDI controller address ({bus}, {channel}, {controller})"
            )));
        }
        self.ensure_control_thread("MIDI learn notification")?;
        let channel = MidiChannel::from_index(channel as u8)
            .ok_or_else(|| Error::InvalidParameter("invalid channel".into()))?;
        if self.notify_live_midi_controller(
            bus,
            channel,
            crate::midi::MidiController::Midi1(controller),
        )? {
            Ok(())
        } else {
            Err(Error::Other(
                "plugin declined MIDI learn or does not support it".into(),
            ))
        }
    }

    fn set_automation_state(&mut self, state: crate::plugin::AutomationState) -> Result<()> {
        use crate::plugin::AutomationState;
        self.ensure_control_thread("automation state update")?;
        let automation = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IAutomationState>())
            .ok_or_else(|| {
                Error::Other("plugin does not implement IAutomationState".to_string())
            })?;
        let state = match state {
            AutomationState::Off => IAutomationState_::AutomationStates_::kNoAutomation,
            AutomationState::Read => IAutomationState_::AutomationStates_::kReadState,
            AutomationState::Write => IAutomationState_::AutomationStates_::kWriteState,
            AutomationState::ReadWrite => IAutomationState_::AutomationStates_::kReadWriteState,
        };
        Self::check_controller_result(
            unsafe { automation.setAutomationState(state) },
            "IAutomationState::setAutomationState",
        )
    }

    fn remap_parameter_id(&self, old_plugin_uid: &str, old_param_id: u32) -> Result<Option<u32>> {
        let old_uid = crate::internal::utils::parse_class_uid(old_plugin_uid).ok_or_else(|| {
            Error::InvalidParameter(
                "plugin UID must contain exactly 32 hexadecimal characters".to_string(),
            )
        })?;
        self.ensure_control_thread("parameter id remapping")?;
        let Some(remapper) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IRemapParamID>())
        else {
            return Ok(None);
        };

        let mut new_param_id = 0;
        let result = unsafe {
            remapper.getCompatibleParamID(&old_uid as *const TUID, old_param_id, &mut new_param_id)
        };
        if result == kResultOk || result == kResultTrue {
            Ok(Some(new_param_id))
        } else if result == kResultFalse || result == kNotImplemented || result == kNoInterface {
            Ok(None)
        } else {
            Err(Error::InterfaceError(format!(
                "IRemapParamID::getCompatibleParamID failed: {result:#x}"
            )))
        }
    }

    fn midi_panic(&mut self) -> Result<()> {
        // Release per-voice notes with their exact ids first.
        for &(note_id, channel, pitch) in self.active_notes.iter() {
            unsafe {
                let mut event: Event = std::mem::zeroed();
                event.busIndex = 0;
                event.sampleOffset = 0;
                event.flags = Event_::EventFlags_::kIsLive as u16;
                event.r#type = kNoteOffEvent as u16;
                event.__field0.noteOff.noteId = note_id;
                event.__field0.noteOff.channel = channel;
                event.__field0.noteOff.pitch = pitch;
                self.input_events.add_raw_event(&event);
            }
        }
        self.active_notes.clear();

        // Ordinary MIDI events use noteId -1, so release every channel/pitch that is active.
        for index in 0..self.ordinary_note_counts.len() {
            if self.ordinary_note_counts[index] == 0 {
                continue;
            }
            let channel = (index / 128) as i16;
            let pitch = (index % 128) as i16;
            unsafe {
                let mut event: Event = std::mem::zeroed();
                event.busIndex = 0;
                event.sampleOffset = 0;
                event.flags = Event_::EventFlags_::kIsLive as u16;
                event.r#type = kNoteOffEvent as u16;
                event.__field0.noteOff.noteId = -1;
                event.__field0.noteOff.channel = channel;
                event.__field0.noteOff.pitch = pitch;
                self.input_events.add_raw_event(&event);
            }
            self.ordinary_note_counts[index] = 0;
        }

        // Also route the standard panic controllers through IMidiMapping for plugins that
        // explicitly expose them as parameters. Never place legacy-MIDI-out events on input.
        for channel in 0..MIDI_CHANNEL_COUNT {
            let Some(channel) = MidiChannel::from_index(channel as u8) else {
                continue;
            };
            for controller in [123u16, 120, 121] {
                self.queue_mapped_midi_controller(channel, controller, 0.0, 0)?;
            }
        }
        Ok(())
    }

    fn output_channel_count(&self) -> usize {
        unsafe {
            let bus_count = self.component.getBusCount(kAudio as i32, kOutput as i32);
            let mut total = 0usize;
            for i in 0..bus_count {
                if !self
                    .bus_activation
                    .audio_outputs
                    .get(i as usize)
                    .copied()
                    .unwrap_or(false)
                {
                    continue;
                }
                let mut info: BusInfo = std::mem::zeroed();
                if self
                    .component
                    .getBusInfo(kAudio as i32, kOutput as i32, i, &mut info)
                    == kResultOk
                {
                    total += info.channelCount.max(0) as usize;
                } else {
                    let mut arrangement = 0u64;
                    if self
                        .processor
                        .getBusArrangement(kOutput as i32, i, &mut arrangement)
                        == kResultOk
                    {
                        total += arrangement.count_ones() as usize;
                    }
                }
            }
            total
        }
    }

    fn save_state(&self) -> Result<Vec<u8>> {
        self.drain_deferred_controller_sync();
        unsafe {
            let component_stream =
                create_memory_stream_with_metadata(None, StreamStateType::Project);
            let component_ptr = component_stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                Error::InterfaceError("Failed to create component state stream".into())
            })?;
            let result = self.component.getState(component_ptr.as_ptr());
            if result != kResultOk {
                return Err(Error::Other(format!(
                    "Plugin does not provide state (getState: {result:#x})"
                )));
            }

            // A single-component plugin's "controller" is the component itself, so asking it for
            // its state returns the blob above a second time: the envelope would carry the same
            // bytes twice, the combined size cap would effectively halve, and `load_state` would
            // apply `setState` twice. `load_state` has the matching guard.
            let controller = if let Some(controller) =
                self.controller.as_ref().filter(|_| !self.single_component)
            {
                let stream = create_memory_stream_with_metadata(None, StreamStateType::Project);
                let stream_ptr = stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                    Error::InterfaceError("Failed to create controller state stream".into())
                })?;
                let result = controller.getState(stream_ptr.as_ptr());
                if result == kResultOk {
                    Some(stream.to_vec())
                } else if result == kNotImplemented || result == kResultFalse {
                    None
                } else {
                    return Err(Error::Other(format!(
                        "Failed to save controller state (getState: {result:#x})"
                    )));
                }
            } else {
                None
            };

            encode_state_snapshot(&StateSnapshot {
                component: component_stream.to_vec(),
                controller,
            })
        }
    }

    fn load_state_with_context(&mut self, data: &[u8], context: &StateContext) -> Result<()> {
        let snapshot = decode_state_snapshot(data)?;
        let was_processing = self.is_processing;
        let was_active = self.is_active;

        unsafe {
            if was_processing {
                let result = self.processor.setProcessing(0);
                if result != kResultOk && result != kNotImplemented {
                    return Err(Error::Other(format!(
                        "Failed to stop processing before state restore: {result:#x}"
                    )));
                }
                self.is_processing = false;
            }
            if was_active {
                let result = self.set_component_active(false);
                if result != kResultOk {
                    if was_processing {
                        let resumed = self.processor.setProcessing(1);
                        self.is_processing = resumed == kResultOk || resumed == kNotImplemented;
                    }
                    return Err(Error::Other(format!(
                        "Failed to deactivate component before state restore: {result:#x}"
                    )));
                }
                self.is_active = false;
            }

            let apply_result = (|| -> Result<()> {
                let comp_stream = create_state_restore_stream(snapshot.component.clone(), context);
                let comp_ptr = comp_stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                    Error::InterfaceError("Failed to create component state stream".into())
                })?;
                let result = self.component.setState(comp_ptr.as_ptr());
                if result != kResultOk && result != kNotImplemented {
                    return Err(Error::Other(format!(
                        "Failed to restore component state (setState: {result:#x})"
                    )));
                }

                if !self.single_component {
                    if let Some(controller) = self.controller.as_ref() {
                        let stream =
                            create_state_restore_stream(snapshot.component.clone(), context);
                        let stream_ptr = stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                            Error::InterfaceError(
                                "Failed to create controller component-state stream".into(),
                            )
                        })?;
                        let result = controller.setComponentState(stream_ptr.as_ptr());
                        if result != kResultOk && result != kNotImplemented {
                            return Err(Error::Other(format!(
                                "Failed to sync component state to controller \
                                 (setComponentState: {result:#x})"
                            )));
                        }
                    }
                }

                if let (Some(controller), Some(controller_state)) =
                    (self.controller.as_ref(), snapshot.controller.as_ref())
                {
                    let stream = create_state_restore_stream(controller_state.clone(), context);
                    let stream_ptr = stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                        Error::InterfaceError("Failed to create controller state stream".into())
                    })?;
                    let result = controller.setState(stream_ptr.as_ptr());
                    if result != kResultOk && result != kNotImplemented {
                        return Err(Error::Other(format!(
                            "Failed to restore controller state (setState: {result:#x})"
                        )));
                    }
                }

                self.pending_param_changes.clear();
                if let Some(process_data) = self.process_data.as_ref() {
                    process_data.input_param_changes.clear_all();
                }
                *self
                    .unit_cache
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = None;
                self.refresh_midi_mapping_cache();
                self.refresh_program_change_cache();
                Ok(())
            })();

            let restore_result = (|| -> Result<()> {
                if was_active {
                    self.activate()?;
                }
                if was_processing {
                    let result = self.processor.setProcessing(1);
                    if result != kResultOk && result != kNotImplemented {
                        return Err(Error::Other(format!(
                            "Failed to resume processing after state restore: {result:#x}"
                        )));
                    }
                    self.is_processing = true;
                }
                Ok(())
            })();

            match (apply_result, restore_result) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(apply), Ok(())) => Err(apply),
                (Ok(()), Err(restore)) => Err(restore),
                (Err(apply), Err(restore)) => Err(Error::Other(format!(
                    "{apply}; additionally failed to restore plugin lifecycle: {restore}"
                ))),
            }
        }
    }
}

/// Convert a raw VST3 `Event` (as a plugin emits into its output event list) into a safe
/// [`MidiEvent`]. Returns `None` for event types this library doesn't model.
#[cfg(test)]
#[allow(non_upper_case_globals)]
// kNoteOnEvent etc. are VST3 SDK constants
// SDK enum constants (event types, controller numbers) are generated as `i32` on some
// targets (Windows) and `u32` on others (macOS); the `u8` field casts are likewise needed
// where `c_char` is `i8`. We match the `u32` scrutinee against `<const> as u32` and allow
// the cast clippy flags as redundant on the targets where it already matches.
#[allow(clippy::unnecessary_cast)]
pub(crate) fn event_to_midi(e: &Event) -> Option<MidiEvent> {
    unsafe {
        match e.r#type as u32 {
            t if t == kNoteOnEvent as u32 => {
                let n = &e.__field0.noteOn;
                Some(MidiEvent::NoteOn {
                    channel: MidiChannel::from_index(n.channel as u8)?,
                    note: (n.pitch.clamp(0, 127)) as u8,
                    velocity: (n.velocity * 127.0).round().clamp(0.0, 127.0) as u8,
                })
            }
            t if t == kNoteOffEvent as u32 => {
                let n = &e.__field0.noteOff;
                Some(MidiEvent::NoteOff {
                    channel: MidiChannel::from_index(n.channel as u8)?,
                    note: (n.pitch.clamp(0, 127)) as u8,
                    velocity: (n.velocity * 127.0).round().clamp(0.0, 127.0) as u8,
                })
            }
            t if t == kPolyPressureEvent as u32 => {
                let p = &e.__field0.polyPressure;
                Some(MidiEvent::PolyAftertouch {
                    channel: MidiChannel::from_index(p.channel as u8)?,
                    note: (p.pitch.clamp(0, 127)) as u8,
                    pressure: (p.pressure * 127.0).round().clamp(0.0, 127.0) as u8,
                })
            }
            t if t == kLegacyMIDICCOutEvent as u32 => {
                let c = &e.__field0.midiCCOut;
                let channel = MidiChannel::from_index(c.channel as u8)?;
                let value = (c.value as u8) & 0x7F;
                match c.controlNumber as u32 {
                    n if n == ControllerNumbers_::kPitchBend as u32 => Some(MidiEvent::PitchBend {
                        channel,
                        value: (((c.value2 as u16) & 0x7F) << 7) | value as u16,
                    }),
                    n if n == ControllerNumbers_::kAfterTouch as u32 => {
                        Some(MidiEvent::ChannelAftertouch {
                            channel,
                            pressure: value,
                        })
                    }
                    cc if cc < 128 => Some(MidiEvent::ControlChange {
                        channel,
                        controller: cc as u8,
                        value,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

impl PluginImpl {
    /// Translate a MIDI controller through the prebuilt IMidiMapping table and queue the
    /// resulting processor automation (which also mirrors the value to the controller, so a
    /// plugin's editor follows CC-driven changes). Unmapped controllers are intentionally
    /// ignored: VST3 does not permit legacy controller-out events on the component's input
    /// event list.
    fn queue_mapped_midi_controller(
        &mut self,
        channel: MidiChannel,
        controller: u16,
        normalized: f64,
        sample_offset: i32,
    ) -> Result<()> {
        // Rebuild the table here if a restart invalidated it off-thread; a no-op on the audio
        // thread, which must never make controller calls.
        self.service_control_thread_caches();
        self.send_midi_controller_at(
            0,
            channel,
            crate::midi::MidiController::Midi1(controller),
            normalized,
            sample_offset,
        )?;
        Ok(())
    }

    #[allow(clippy::unnecessary_cast)]
    unsafe fn activate_default_buses(component: &ComPtr<IComponent>) -> Result<BusActivationState> {
        let read_and_activate = |media: i32, direction: i32| -> Result<Vec<bool>> {
            let count = component.getBusCount(media, direction);
            let mut states = Vec::with_capacity(count.max(0) as usize);
            for index in 0..count {
                let mut info: BusInfo = std::mem::zeroed();
                let active = component.getBusInfo(media, direction, index, &mut info) == kResultOk
                    && (info.flags & BusInfo_::BusFlags_::kDefaultActive as u32) != 0;
                if active {
                    let result = component.activateBus(media, direction, index, 1);
                    if result != kResultOk {
                        return Err(Error::InterfaceError(format!(
                            "activateBus failed for default-active bus \
                             ({media}, {direction}, {index}): {result:#x}"
                        )));
                    }
                }
                states.push(active);
            }
            Ok(states)
        };

        Ok(BusActivationState {
            audio_inputs: read_and_activate(kAudio as i32, kInput as i32)?,
            audio_outputs: read_and_activate(kAudio as i32, kOutput as i32)?,
            event_inputs: read_and_activate(kEvent as i32, kInput as i32)?,
            event_outputs: read_and_activate(kEvent as i32, kOutput as i32)?,
        })
    }

    /// Get or create controller (handles both single-component and separate controller)
    unsafe fn get_or_create_controller(
        component: &ComPtr<IComponent>,
        factory: &ComPtr<IPluginFactory>,
        context: *mut FUnknown,
    ) -> Result<Option<ComPtr<IEditController>>> {
        // First, try to cast component to IEditController (single component)
        if let Some(controller) = component.cast::<IEditController>() {
            log::debug!("Component implements IEditController (single component)");
            return Ok(Some(controller));
        }

        // If not single component, try to get separate controller
        log::debug!("Component is separate from controller, getting controller class ID...");
        let mut controller_cid: [std::os::raw::c_char; 16] = [0; 16];
        let result = component.getControllerClassId(&mut controller_cid);

        if result != kResultOk {
            log::warn!("Failed to get controller class ID: {:#x}", result);
            return Ok(None);
        }

        log::debug!("Got controller class ID, creating controller...");
        let mut controller_ptr: *mut IEditController = ptr::null_mut();
        let create_result = factory.createInstance(
            controller_cid.as_ptr(),
            IEditController::IID.as_ptr() as *const std::os::raw::c_char,
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

        // Initialize controller with the same host context as the component.
        log::debug!("Initializing controller...");
        let init_result = controller.initialize(context);
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
    ) -> Result<Option<ConnectionPair>> {
        // Try to get connection points
        let comp_cp = component.cast::<IConnectionPoint>();
        let ctrl_cp = controller.cast::<IConnectionPoint>();

        if let (Some(comp_cp), Some(ctrl_cp)) = (comp_cp, ctrl_cp) {
            let connection = ConnectionPair::connect(comp_cp, ctrl_cp);
            if connection.is_some() {
                log::debug!("Components connected successfully");
            } else {
                log::warn!("Component connection proxy not established (continuing)");
            }
            Ok(connection)
        } else {
            log::debug!("Components do not support IConnectionPoint - might be single component");
            Ok(None) // Not an error - single components don't need connection
        }
    }
}

/// Map a sample offset within the caller's block onto one chunk of it.
///
/// Returns the offset rebased to the chunk (`0..frames`), or `None` when the offset belongs to
/// a different chunk. Anything scheduled past the end of the caller's block lands at the end of
/// the final chunk rather than being dropped — a note scheduled near `block_size` still sounds
/// when the device hands over a shorter block.
fn chunk_offset(
    sample_offset: i32,
    chunk_start: usize,
    frames: usize,
    is_last: bool,
) -> Option<i32> {
    let start = chunk_start as i64;
    let end = start + frames as i64;
    // A negative offset is undefined in VST3; treat it as the start of the block.
    let offset = i64::from(sample_offset.max(0));
    if offset < start {
        None
    } else if offset < end {
        Some((offset - start) as i32)
    } else if is_last {
        Some(frames.saturating_sub(1) as i32)
    } else {
        None
    }
}

/// Move the edits the plugin's editor made with `performEdit` into the processor's input
/// queue at offset 0, and into the stash the host's display poll drains.
///
/// Runs on the audio thread, so it never waits: `performEdit` takes `edits` on the editor's
/// thread and the host's poll takes `stash` on its own, and either may hold its lock when a
/// block starts. A lock that is held leaves the edits where they are for the next block,
/// which loses none of them and delays them by one block. A poisoned `edits` is skipped and a
/// poisoned `stash` gets no copy, as with a blocking lock. Returns whether the edits were
/// drained.
fn drain_editor_edits(
    edits: &Mutex<Vec<(u32, f64)>>,
    input: &ParameterChanges,
    stash: &Mutex<Vec<(u32, f64)>>,
) -> bool {
    let Ok(mut gui_changes) = edits.try_lock() else {
        return false;
    };
    if gui_changes.is_empty() {
        return true;
    }
    // Taken before any edit is enqueued, so a busy stash cannot leave an edit delivered to
    // the processor and still pending for the next block.
    let mut stash = match stash.try_lock() {
        Ok(stash) => Some(stash),
        Err(TryLockError::Poisoned(_)) => None,
        Err(TryLockError::WouldBlock) => return false,
    };
    for &(id, value) in gui_changes.iter() {
        input.enqueue(id, 0, value);
    }
    let Some(stash) = stash.as_mut() else {
        gui_changes.clear();
        return true;
    };
    // Bounded: nothing drains the stash unless the host polls `get_parameter_changes`, and
    // the realtime runner never does, so an unbounded append here would grow forever and
    // reallocate on the audio thread. Both buffers are pre-reserved to the cap, so the
    // steady-state append allocates nothing.
    let room = MAX_EDITOR_FEEDBACK.saturating_sub(stash.len());
    if room >= gui_changes.len() {
        stash.append(&mut gui_changes);
    } else {
        stash.extend(gui_changes.drain(..room));
        gui_changes.clear();
    }
    true
}

/// Stage the events belonging to one chunk into the plugin-facing event list, rebasing each
/// offset to the chunk start (see [`chunk_offset`]). Replaces whatever the list held, so each
/// chunk of a split block sees exactly its own events, in order and with their spacing intact.
fn stage_chunk_events(
    queued: &mut [Option<PluginEvent>],
    list: &HostEventList,
    chunk_start: usize,
    frames: usize,
    is_last: bool,
) {
    list.reset_with(queued.iter_mut().filter_map(|slot| {
        let offset = chunk_offset(slot.as_ref()?.sample_offset, chunk_start, frames, is_last)?;
        slot.take().map(|mut event| {
            event.sample_offset = offset;
            event
        })
    }));
}

/// Ordered teardown for a component that has been initialized but whose `PluginImpl` doesn't
/// exist yet.
///
/// Between `IComponent::initialize` returning success and the finished `PluginImpl`, `load` can
/// still fail — and by then the plugin may have spawned threads and registered callbacks.
/// Releasing its interfaces and unloading the module without `terminate()` leaves that code
/// running inside memory the unload is about to unmap. This guard runs the same sequence
/// `Drop for PluginImpl` does, unless [`Self::disarm`] hands the job to the built plugin.
///
/// It holds its own references (COM refcounts), so `load` keeps using the originals as it
/// builds; the extra references are released when the guard drops either way.
struct InitializedComponent {
    component: ComPtr<IComponent>,
    controller: Option<ComPtr<IEditController>>,
    single_component: bool,
    connection: Option<ConnectionPair>,
    armed: bool,
}

impl InitializedComponent {
    fn new(component: ComPtr<IComponent>) -> Self {
        Self {
            component,
            controller: None,
            single_component: false,
            connection: None,
            armed: true,
        }
    }

    /// Record the controller so a later failure disconnects and terminates it too.
    fn attach_controller(
        &mut self,
        controller: Option<ComPtr<IEditController>>,
        single_component: bool,
    ) {
        self.controller = controller;
        self.single_component = single_component;
    }

    fn attach_connection(&mut self, connection: Option<ConnectionPair>) {
        self.connection = connection;
    }

    fn take_connection(&mut self) -> Option<ConnectionPair> {
        self.connection.take()
    }

    /// Cancel the teardown: the finished `PluginImpl` owns the lifecycle from here.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InitializedComponent {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        log::debug!("Plugin load failed after initialize; terminating the component");
        unsafe {
            terminate_component(
                &self.component,
                self.controller.as_ref(),
                self.single_component,
                self.connection.as_ref(),
            );
        }
    }
}

/// Disconnect and terminate a live component/controller pair, in VST3 order.
///
/// The mirror of the load sequence: break the component↔controller connection, terminate the
/// controller, then terminate the component. Many plugins (dual-component ones especially) rely
/// on `terminate()` to break that link before release and crash without it. A single-component
/// plugin is one object exposed as both halves, so it has no connection pair and is terminated
/// once, as the component.
unsafe fn terminate_component(
    component: &ComPtr<IComponent>,
    controller: Option<&ComPtr<IEditController>>,
    single_component: bool,
    connection: Option<&ConnectionPair>,
) {
    if let Some(connection) = connection {
        connection.disconnect();
    }
    if let Some(controller) = controller {
        if !single_component {
            controller.terminate();
        }
    }
    component.terminate();
}

impl Drop for PluginImpl {
    fn drop(&mut self) {
        // VST3 teardown order: detach the editor, stop processing, deactivate the component,
        // disconnect the component/controller connection points, terminate the controller and the
        // component, then drop the COM references. Many plugins (dual-component ones especially)
        // rely on `terminate()` to break the controller↔component link before release and crash
        // without it.
        //
        // The editor goes first. A view that is still `attached()` when the controller is
        // terminated — and then merely Released as a field, without `removed()` — leaves the
        // plugin's platform frame and its idle timer alive, pointing at a host window the caller
        // is about to destroy. `open_editor` and `close_editor` pair attach/removed correctly, but
        // a host that drops the plugin with the editor open (or is unwound past its own close)
        // never reaches `close_editor`, so do it here too. It is a no-op when no view is attached.
        let _ = PluginInternal::close_editor(self);

        let _ = self.stop_processing();
        self._host_app.shutdown_data_exchange();

        unsafe {
            if self.is_active {
                self.set_component_active(false);
                self.is_active = false;
            }

            terminate_component(
                &self.component,
                self.controller.as_ref(),
                self.single_component,
                self.connection.as_ref(),
            );
        }
    }
}

/// The `ProcessContext.state` flags the host advertises each block: transport playing, with
/// a valid tempo, time signature, continuous sample time, and musical (quarter-note)
/// playhead. The last two are essential — without `kContTimeValid`/`kProjectTimeMusicValid`
/// a spec-conformant plugin treats the advancing `continousTimeSamples`/`projectTimeMusic`
/// (see [`advance_process_context`]) as invalid and ignores it. The `as u32` cast is needed
/// because the `StatesAndFlags_` constants are generated as `i32` on some targets (Windows)
/// and `u32` on others (macOS).
#[allow(clippy::unnecessary_cast)] // the `as u32` is needed where the constants are i32 (Windows)
const PROCESS_CONTEXT_STATE: u32 = (ProcessContext_::StatesAndFlags_::kPlaying
    | ProcessContext_::StatesAndFlags_::kTempoValid
    | ProcessContext_::StatesAndFlags_::kTimeSigValid
    | ProcessContext_::StatesAndFlags_::kContTimeValid
    | ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid)
    as u32;

/// The `kPlaying` bit on its own, factored out of [`PROCESS_CONTEXT_STATE`] so the playing
/// state can be toggled at runtime without disturbing the validity flags.
#[allow(clippy::unnecessary_cast)] // the `as u32` is needed where the constant is i32 (Windows)
const PROCESS_CONTEXT_PLAYING: u32 = ProcessContext_::StatesAndFlags_::kPlaying as u32;

#[allow(clippy::unnecessary_cast)]
fn process_context_needs(requirements: Option<u32>, flag: u32) -> bool {
    match requirements {
        Some(requirements) => requirements & flag != 0,
        None => true,
    }
}

/// Compute the validity flags for the fields the processor requested. Processors predating
/// IProcessContextRequirements retain the host's complete legacy transport context.
#[allow(clippy::unnecessary_cast)]
fn process_context_state(requirements: Option<u32>, playing: bool) -> u32 {
    let Some(requirements) = requirements else {
        return if playing {
            PROCESS_CONTEXT_STATE | PROCESS_CONTEXT_PLAYING
        } else {
            PROCESS_CONTEXT_STATE & !PROCESS_CONTEXT_PLAYING
        };
    };

    use IProcessContextRequirements_::Flags_ as R;
    use ProcessContext_::StatesAndFlags_ as S;
    let mappings = [
        (R::kNeedSystemTime as u32, S::kSystemTimeValid as u32),
        (
            R::kNeedContinousTimeSamples as u32,
            S::kContTimeValid as u32,
        ),
        (
            R::kNeedProjectTimeMusic as u32,
            S::kProjectTimeMusicValid as u32,
        ),
        (R::kNeedTempo as u32, S::kTempoValid as u32),
        (R::kNeedTimeSignature as u32, S::kTimeSigValid as u32),
    ];
    let mut state = mappings
        .iter()
        .filter(|(required, _)| requirements & required != 0)
        .fold(0, |state, (_, valid)| state | valid);
    if playing {
        state |= S::kPlaying as u32;
    }
    state
}

fn current_system_time_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

/// Advance the transport in a `ProcessContext` by `frames` samples after a processed block.
/// Keeps `continousTimeSamples`/`projectTimeSamples` (and the musical playhead derived from
/// the current tempo) moving so tempo-synced plugins don't see a frozen time-0.
#[allow(clippy::unnecessary_cast)]
fn advance_process_context(
    ctx: &mut ProcessContext,
    requirements: Option<u32>,
    transport_tempo: f64,
    frames: i64,
) {
    use IProcessContextRequirements_::Flags_ as R;
    let playing = ctx.state & PROCESS_CONTEXT_PLAYING != 0;
    if playing {
        ctx.projectTimeSamples = ctx.projectTimeSamples.saturating_add(frames);
        if ctx.sampleRate > 0.0 {
            ctx.projectTimeMusic += frames as f64 / ctx.sampleRate * (transport_tempo / 60.0);
        }
    }
    if process_context_needs(requirements, R::kNeedContinousTimeSamples as u32) {
        ctx.continousTimeSamples = ctx.continousTimeSamples.wrapping_add(frames);
    }
}

#[cfg(test)]
mod process_buffer_tests {
    use super::*;

    fn buses(channels: &[i32]) -> Vec<AudioBusBuffers> {
        channels
            .iter()
            .map(|&num_channels| {
                let mut bus: AudioBusBuffers = unsafe { std::mem::zeroed() };
                bus.numChannels = num_channels;
                bus
            })
            .collect()
    }

    /// An inactive bus still gets a full channel-pointer array — the spec requires the array
    /// itself for every bus and only permits the *addresses* in it to be null. Sample storage is
    /// allocated for active channels only.
    #[test]
    fn typed_buffers_give_inactive_buses_an_array_of_null_channel_pointers() {
        let inputs = buses(&[2, 1, 4]);
        let outputs = buses(&[2]);
        let samples =
            build_typed_sample_buffers::<f64>(32, &inputs, &[true, false, true], &outputs, &[true]);

        assert_eq!(samples.inputs.len(), 6);
        assert_eq!(
            samples
                .input_channel_ptrs
                .0
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            [2, 1, 4]
        );
        assert!(samples.input_channel_ptrs.0[1].iter().all(|p| p.is_null()));
        assert!(samples.input_channel_ptrs.0[0]
            .iter()
            .chain(&samples.input_channel_ptrs.0[2])
            .all(|p| !p.is_null()));
        assert_eq!(samples.outputs.len(), 2);
    }

    #[test]
    fn sample64_storage_converts_to_and_from_the_public_f32_buffers() {
        let mut storage = HostSampleBuffers::F64(build_typed_sample_buffers(
            4,
            &buses(&[1]),
            &[true],
            &buses(&[1]),
            &[true],
        ));
        storage.copy_inputs_from(&[vec![0.25, -0.5, 1.0, 0.0]], 0, 4);
        let HostSampleBuffers::F64(samples) = &mut storage else {
            unreachable!()
        };
        assert_eq!(samples.inputs[0], [0.25, -0.5, 1.0, 0.0]);
        samples.outputs[0].copy_from_slice(&[0.5, -0.25, 2.0, 0.0]);
        let mut outputs = vec![vec![0.0; 4]];
        let output_buses = buses(&[1]);
        storage.copy_outputs_to(&mut outputs, 0, 4, &output_buses, &[true]);
        assert_eq!(outputs[0], [0.5, -0.25, 2.0, 0.0]);
    }

    #[test]
    fn bus_buffers_route_active_buses_without_flattening_inactive_slots() {
        let input_buses = buses(&[2, 1, 1]);
        let output_buses = buses(&[1, 2, 1]);
        let active = [true, false, true];
        let mut storage = HostSampleBuffers::F32(build_typed_sample_buffers(
            4,
            &input_buses,
            &active,
            &output_buses,
            &active,
        ));
        let inputs = vec![
            AudioBusBuffer {
                active: true,
                channels: vec![vec![1.0; 4], vec![2.0; 4]],
            },
            AudioBusBuffer {
                active: false,
                channels: vec![vec![99.0; 4]],
            },
            AudioBusBuffer {
                active: true,
                channels: vec![vec![3.0; 4]],
            },
        ];
        storage.copy_inputs_from_buses(&inputs, 0, 4, &input_buses, &active);
        let HostSampleBuffers::F32(samples) = &mut storage else {
            unreachable!()
        };
        assert_eq!(samples.inputs[0], [1.0; 4]);
        assert_eq!(samples.inputs[1], [2.0; 4]);
        assert_eq!(samples.inputs[2], [3.0; 4]);

        samples.outputs[0].fill(4.0);
        samples.outputs[1].fill(5.0);
        let mut outputs = vec![
            AudioBusBuffer::new(1, 4, true),
            AudioBusBuffer::new(2, 4, false),
            AudioBusBuffer::new(1, 4, true),
        ];
        outputs[1].channels[0].fill(99.0);
        outputs[1].channels[1].fill(99.0);
        storage.copy_outputs_to_buses(&mut outputs, 0, 4, &output_buses, &active);
        assert_eq!(outputs[0].channels[0], [4.0; 4]);
        assert_eq!(outputs[1].channels[0], [0.0; 4]);
        assert_eq!(outputs[1].channels[1], [0.0; 4]);
        assert_eq!(outputs[2].channels[0], [5.0; 4]);
    }

    #[test]
    fn silence_flags_are_computed_per_bus_and_inactive_slots_are_silent() {
        let mut storage = HostSampleBuffers::F32(build_typed_sample_buffers(
            4,
            &buses(&[2, 1]),
            &[true, false],
            &buses(&[2]),
            &[true],
        ));
        storage.copy_inputs_from(&[vec![0.0, 0.0, 0.0, 0.0], vec![0.0, 0.5, 0.0, 0.0]], 0, 4);
        let mut input_buses = buses(&[2, 1]);
        storage.update_input_silence_flags(&mut input_buses, &[true, false], 4);
        assert_eq!(input_buses[0].silenceFlags, 0b01);
        assert_eq!(input_buses[1].silenceFlags, 0b1);
    }

    #[test]
    fn plugin_output_silence_flags_zero_only_the_declared_channels() {
        let mut storage = HostSampleBuffers::F32(build_typed_sample_buffers(
            4,
            &[],
            &[],
            &buses(&[2]),
            &[true],
        ));
        let HostSampleBuffers::F32(samples) = &mut storage else {
            unreachable!()
        };
        samples.outputs[0].copy_from_slice(&[0.5, 0.5, 0.5, 0.5]);
        samples.outputs[1].copy_from_slice(&[0.25, 0.25, 0.25, 0.25]);
        let mut output_buses = buses(&[2]);
        output_buses[0].silenceFlags = 0b01;
        storage.update_output_silence_flags(&mut output_buses, &[true], 4);
        assert_eq!(output_buses[0].silenceFlags, 0b01);

        let mut outputs = vec![vec![9.0; 4], vec![9.0; 4]];
        storage.copy_outputs_to(&mut outputs, 0, 4, &output_buses, &[true]);
        assert_eq!(outputs[0], [0.0; 4]);
        assert_eq!(outputs[1], [0.25; 4]);
    }

    #[test]
    fn actually_zero_output_is_marked_silent_even_when_plugin_omits_the_flag() {
        let mut storage = HostSampleBuffers::F64(build_typed_sample_buffers(
            4,
            &[],
            &[],
            &buses(&[2]),
            &[true],
        ));
        let HostSampleBuffers::F64(samples) = &mut storage else {
            unreachable!()
        };
        samples.outputs[0].fill(0.0);
        samples.outputs[1].fill(1.0);
        let mut output_buses = buses(&[2]);
        prepare_output_silence_flags(&mut output_buses, &[true]);
        storage.update_output_silence_flags(&mut output_buses, &[true], 4);
        assert_eq!(output_buses[0].silenceFlags, 0b01);
    }

    #[test]
    fn zero_sample_flush_hides_then_restores_all_audio_buses() {
        let mut input_bus: AudioBusBuffers = unsafe { std::mem::zeroed() };
        let mut output_bus: AudioBusBuffers = unsafe { std::mem::zeroed() };
        let mut process_data: ProcessData = unsafe { std::mem::zeroed() };
        process_data.numInputs = 1;
        process_data.numOutputs = 1;
        process_data.inputs = &mut input_bus;
        process_data.outputs = &mut output_bus;
        let original_inputs = process_data.inputs;
        let original_outputs = process_data.outputs;

        let saved = hide_audio_io_for_zero_sample(&mut process_data, 0);
        assert_eq!(process_data.numInputs, 0);
        assert_eq!(process_data.numOutputs, 0);
        assert!(process_data.inputs.is_null());
        assert!(process_data.outputs.is_null());

        restore_process_audio_io(&mut process_data, saved);
        assert_eq!(process_data.numInputs, 1);
        assert_eq!(process_data.numOutputs, 1);
        assert_eq!(process_data.inputs, original_inputs);
        assert_eq!(process_data.outputs, original_outputs);
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    #[allow(clippy::unnecessary_cast)] // `as u32` needed where the constants are i32 (Windows)
    fn process_context_state_advertises_playhead_validity() {
        // The advancing continous/musical playhead is only honored by conformant plugins if
        // its validity flags are set.
        use ProcessContext_::StatesAndFlags_ as F;
        assert_ne!(PROCESS_CONTEXT_STATE & F::kPlaying as u32, 0);
        assert_ne!(PROCESS_CONTEXT_STATE & F::kTempoValid as u32, 0);
        assert_ne!(PROCESS_CONTEXT_STATE & F::kTimeSigValid as u32, 0);
        assert_ne!(PROCESS_CONTEXT_STATE & F::kContTimeValid as u32, 0);
        assert_ne!(PROCESS_CONTEXT_STATE & F::kProjectTimeMusicValid as u32, 0);
    }

    #[test]
    #[allow(clippy::unnecessary_cast)] // `as u32` needed where the constants are i32 (Windows)
    fn process_context_state_toggles_playing_without_disturbing_validity() {
        use ProcessContext_::StatesAndFlags_ as F;
        let playing = process_context_state(None, true);
        let stopped = process_context_state(None, false);
        // kPlaying tracks the playing flag.
        assert_ne!(playing & F::kPlaying as u32, 0);
        assert_eq!(stopped & F::kPlaying as u32, 0);
        // The validity flags survive in both states.
        for flag in [
            F::kTempoValid as u32,
            F::kTimeSigValid as u32,
            F::kContTimeValid as u32,
            F::kProjectTimeMusicValid as u32,
        ] {
            assert_ne!(playing & flag, 0);
            assert_ne!(stopped & flag, 0);
        }
    }

    #[test]
    fn advance_moves_playhead_and_musical_time() {
        let mut ctx: ProcessContext = unsafe { std::mem::zeroed() };
        ctx.sampleRate = 48_000.0;
        ctx.tempo = 120.0;
        ctx.state = PROCESS_CONTEXT_PLAYING;
        // One second of audio at 48 kHz in 512-sample blocks.
        let blocks = 48_000 / 512;
        for _ in 0..blocks {
            advance_process_context(&mut ctx, None, 120.0, 512);
        }
        let advanced = (blocks * 512) as i64;
        assert_eq!(ctx.projectTimeSamples, advanced);
        assert_eq!(ctx.continousTimeSamples, advanced);
        // ~0.992 s elapsed at 120 BPM → ~1.98 quarter notes; just assert it moved forward.
        assert!(ctx.projectTimeMusic > 1.9 && ctx.projectTimeMusic < 2.1);
    }

    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn explicit_requirements_advertise_only_requested_fields() {
        use IProcessContextRequirements_::Flags_ as R;
        use ProcessContext_::StatesAndFlags_ as S;
        let requirements = R::kNeedTempo as u32 | R::kNeedContinousTimeSamples as u32;
        let state = process_context_state(Some(requirements), true);
        assert_ne!(state & S::kTempoValid as u32, 0);
        assert_ne!(state & S::kContTimeValid as u32, 0);
        assert_ne!(state & S::kPlaying as u32, 0);
        assert_eq!(state & S::kTimeSigValid as u32, 0);
        assert_eq!(state & S::kProjectTimeMusicValid as u32, 0);
    }
}

#[cfg(test)]
mod chunked_block_tests {
    use super::*;
    use crate::internal::com_implementations::create_event_list;

    fn note_on_at(offset: i32, pitch: i16) -> PluginEvent {
        PluginEvent {
            bus_index: 0,
            sample_offset: offset,
            ppq_position: 0.0,
            flags: 0,
            data: crate::midi::PluginEventData::NoteOn {
                channel: 0,
                pitch,
                tuning: 0.0,
                velocity: 1.0,
                length: 0,
                note_id: -1,
            },
        }
    }

    /// Read back what the plugin would see: `(sampleOffset, pitch)` per staged event.
    fn staged(list: &HostEventList) -> Vec<(i32, i16)> {
        let mut out = Vec::new();
        list.drain_each(|e| {
            let crate::midi::PluginEventData::NoteOn { pitch, .. } = e.data else {
                panic!("expected note on");
            };
            out.push((e.sample_offset, pitch))
        });
        out
    }

    /// A device block larger than the configured block size is split into chunks, and each
    /// queued event has to land in the chunk that actually contains its offset — rebased to
    /// that chunk. Routing everything to chunk 0 (clamped to its end) fires late events ~20 ms
    /// early and collapses the spacing between them.
    #[test]
    fn events_are_routed_to_the_chunk_that_contains_them() {
        // A 2048-frame device block processed in 512-frame chunks.
        let mut queued = [
            Some(note_on_at(0, 60)),    // chunk 0, offset 0
            Some(note_on_at(100, 61)),  // chunk 0, offset 100
            Some(note_on_at(512, 62)),  // chunk 1, offset 0 (first sample of the chunk)
            Some(note_on_at(1500, 63)), // chunk 2, offset 476
        ];
        let list = create_event_list();

        stage_chunk_events(&mut queued, &list, 0, 512, false);
        assert_eq!(staged(&list), vec![(0, 60), (100, 61)]);

        stage_chunk_events(&mut queued, &list, 512, 512, false);
        assert_eq!(staged(&list), vec![(0, 62)]);

        stage_chunk_events(&mut queued, &list, 1024, 512, false);
        assert_eq!(staged(&list), vec![(476, 63)]);

        // The final chunk holds nothing: every event was delivered exactly once, earlier.
        stage_chunk_events(&mut queued, &list, 1536, 512, true);
        assert_eq!(staged(&list), Vec::new());
    }

    /// Offsets past the end of the caller's block still have to sound; they collapse into the
    /// last chunk rather than being dropped (the pre-split behaviour, preserved).
    #[test]
    fn offsets_past_the_block_land_in_the_final_chunk() {
        let mut queued = [Some(note_on_at(9_000, 64)), Some(note_on_at(-5, 65))];
        let list = create_event_list();

        // A non-final chunk ignores the overshoot; the negative offset is treated as 0.
        stage_chunk_events(&mut queued, &list, 0, 512, false);
        assert_eq!(staged(&list), vec![(0, 65)]);

        // The final chunk absorbs it at its last sample.
        stage_chunk_events(&mut queued, &list, 512, 512, true);
        assert_eq!(staged(&list), vec![(511, 64)]);
    }

    /// A block that fits in one chunk keeps the old semantics: exact offsets inside the block,
    /// anything past the end clamped to its last sample.
    #[test]
    fn single_chunk_block_clamps_to_its_own_length() {
        let mut queued = [
            Some(note_on_at(0, 60)),
            Some(note_on_at(200, 61)),
            Some(note_on_at(999, 62)),
        ];
        let list = create_event_list();

        stage_chunk_events(&mut queued, &list, 0, 256, true);
        assert_eq!(staged(&list), vec![(0, 60), (200, 61), (255, 62)]);
    }

    #[test]
    fn chunk_offset_maps_each_offset_once() {
        // Every offset in a 3-chunk block belongs to exactly one chunk.
        let chunks = [
            (0usize, 512usize, false),
            (512, 512, false),
            (1024, 512, true),
        ];
        for offset in [0, 1, 511, 512, 1023, 1024, 1535] {
            let hits: Vec<_> = chunks
                .iter()
                .filter_map(|&(start, frames, last)| {
                    chunk_offset(offset, start, frames, last).map(|o| (start, o))
                })
                .collect();
            assert_eq!(hits.len(), 1, "offset {offset} routed to {hits:?}");
            let (start, rebased) = hits[0];
            assert_eq!(start as i32 + rebased, offset);
        }
    }
}

#[cfg(test)]
mod output_midi_tests {
    use super::*;

    fn blank_event() -> Event {
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn converts_note_on() {
        let mut e = blank_event();
        e.r#type = kNoteOnEvent as u16;
        e.__field0.noteOn.channel = 0;
        e.__field0.noteOn.pitch = 60;
        e.__field0.noteOn.velocity = 1.0;
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 127
            })
        );
    }

    #[test]
    fn converts_note_off() {
        let mut e = blank_event();
        e.r#type = kNoteOffEvent as u16;
        e.__field0.noteOff.channel = 1;
        e.__field0.noteOff.pitch = 64;
        e.__field0.noteOff.velocity = 0.0;
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOff {
                channel: MidiChannel::Ch2,
                note: 64,
                velocity: 0
            })
        );
    }

    #[test]
    fn converts_legacy_cc_and_pitchbend() {
        // A plain CC.
        let mut cc = blank_event();
        cc.r#type = kLegacyMIDICCOutEvent as u16;
        cc.__field0.midiCCOut.controlNumber = 1; // mod wheel
        cc.__field0.midiCCOut.channel = 0;
        cc.__field0.midiCCOut.value = 64;
        assert_eq!(
            event_to_midi(&cc),
            Some(MidiEvent::ControlChange {
                channel: MidiChannel::Ch1,
                controller: 1,
                value: 64
            })
        );

        // Pitch bend round-trips the 14-bit value (LSB in value, MSB in value2).
        let mut pb = blank_event();
        pb.r#type = kLegacyMIDICCOutEvent as u16;
        pb.__field0.midiCCOut.controlNumber = ControllerNumbers_::kPitchBend as u8;
        pb.__field0.midiCCOut.channel = 0;
        pb.__field0.midiCCOut.value = (10000 & 0x7F) as std::os::raw::c_char;
        pb.__field0.midiCCOut.value2 = ((10000 >> 7) & 0x7F) as std::os::raw::c_char;
        assert_eq!(
            event_to_midi(&pb),
            Some(MidiEvent::PitchBend {
                channel: MidiChannel::Ch1,
                value: 10000
            })
        );
    }

    #[test]
    fn ignores_unknown_event_types() {
        let mut e = blank_event();
        e.r#type = 9999;
        assert_eq!(event_to_midi(&e), None);
    }

    /// MIDI's velocity-0 note-on is a note-off alias, and VST3 has no running status to express
    /// it. Emitting a zero-velocity `kNoteOnEvent` leaves the plugin sounding the voice while
    /// the host's note tracker has already counted the release, so `midi_panic` cannot clear it.
    #[test]
    fn velocity_zero_note_on_is_written_as_a_note_off() {
        let mut e = blank_event();
        let released = write_midi_note_on(&mut e, MidiChannel::Ch1, 60, 0);
        assert!(released);
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOff {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 0
            })
        );
    }

    #[test]
    fn ordinary_note_on_is_written_as_a_note_on() {
        let mut e = blank_event();
        let released = write_midi_note_on(&mut e, MidiChannel::Ch2, 64, 127);
        assert!(!released);
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOn {
                channel: MidiChannel::Ch2,
                note: 64,
                velocity: 127
            })
        );
    }

    #[test]
    fn note_off_keeps_its_release_velocity() {
        let mut e = blank_event();
        write_note_off_event(&mut e, MidiChannel::Ch1, 60, 64);
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOff {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 64
            })
        );
    }
}

#[cfg(test)]
mod midi_mapping_cache_tests {
    use super::*;

    /// The mapping table is `buses × 16 × 131` entries, so the bus count a plugin reports sizes
    /// a host allocation. An implausible (or hostile) count must not be taken at face value.
    #[test]
    fn bus_count_is_clamped_to_a_sane_range() {
        assert_eq!(midi_mapping_bus_count(-1), 0);
        assert_eq!(midi_mapping_bus_count(0), 0);
        assert_eq!(midi_mapping_bus_count(1), 1);
        assert_eq!(midi_mapping_bus_count(i32::MAX), MAX_MIDI_MAPPING_BUSES);
    }

    #[test]
    fn mapping_table_includes_vst3_program_change_controller() {
        let mut cache = MidiMappingCache {
            buses: 1,
            assignments: vec![None; MIDI_CHANNEL_COUNT * MIDI_CONTROLLER_COUNT],
            ..Default::default()
        };
        let controller = ControllerNumbers_::kCtrlProgramChange as u16;
        let index = cache
            .index(0, MidiChannel::Ch16.as_index() as i16, controller)
            .expect("program change must fit in the MIDI mapping table");
        cache.assignments[index] = Some(42);

        assert_eq!(cache.get(0, 15, controller), Some(42));
    }
}

#[cfg(test)]
mod editor_scale_tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
    use vst3::Class;

    /// A view that implements `IPlugViewContentScaleSupport` and declines the factor, which is
    /// what JUCE editors do on macOS.
    struct DecliningScaleView {
        calls: Arc<AtomicUsize>,
        result: Arc<AtomicI32>,
    }

    impl Class for DecliningScaleView {
        type Interfaces = (IPlugView, IPlugViewContentScaleSupport);
    }

    impl IPlugViewTrait for DecliningScaleView {
        unsafe fn isPlatformTypeSupported(&self, _type: FIDString) -> tresult {
            kResultOk
        }
        unsafe fn attached(&self, _parent: *mut std::ffi::c_void, _type: FIDString) -> tresult {
            kResultOk
        }
        unsafe fn removed(&self) -> tresult {
            kResultOk
        }
        unsafe fn onWheel(&self, _distance: f32) -> tresult {
            kResultOk
        }
        unsafe fn onKeyDown(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
            kResultOk
        }
        unsafe fn onKeyUp(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
            kResultOk
        }
        unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
            *size = ViewRect {
                left: 0,
                top: 0,
                right: 640,
                bottom: 480,
            };
            kResultOk
        }
        unsafe fn onSize(&self, _new_size: *mut ViewRect) -> tresult {
            kResultOk
        }
        unsafe fn onFocus(&self, _state: TBool) -> tresult {
            kResultOk
        }
        unsafe fn setFrame(&self, _frame: *mut IPlugFrame) -> tresult {
            kResultOk
        }
        unsafe fn canResize(&self) -> tresult {
            kResultTrue
        }
        unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
            kResultOk
        }
    }

    impl IPlugViewContentScaleSupportTrait for DecliningScaleView {
        unsafe fn setContentScaleFactor(&self, _factor: f32) -> tresult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.load(Ordering::SeqCst)
        }
    }

    fn view(result: i32) -> (ComPtr<IPlugView>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let wrapper = ComWrapper::new(DecliningScaleView {
            calls: Arc::clone(&calls),
            result: Arc::new(AtomicI32::new(result)),
        });
        (wrapper.to_com_ptr::<IPlugView>().expect("view"), calls)
    }

    /// The open path asks every view for its scale factor, and a view that answers
    /// `kResultFalse` has *declined* — which is what JUCE editors do on macOS. Treating a
    /// decline as an error takes "Open GUI" away from every JUCE plugin on the platform, so the
    /// open path must never see one.
    #[test]
    fn a_declined_scale_factor_is_not_an_error() {
        let (view, calls) = view(kResultFalse);
        let accepted = unsafe { set_view_scale_factor(&view, 1.0) };
        assert!(!accepted.expect("a decline is not an error"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_accepted_scale_factor_reports_support() {
        let (view, calls) = view(kResultOk);
        assert!(unsafe { set_view_scale_factor(&view, 2.0) }.expect("accepted"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A nonsensical *request* is still the caller's bug, and stays an error.
    #[test]
    fn a_nonsensical_scale_factor_is_rejected_without_reaching_the_view() {
        let (view, calls) = view(kResultOk);
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(
                unsafe { set_view_scale_factor(&view, bad) }.is_err(),
                "{bad}"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod sdk_transport_regressions {
    use super::*;

    #[test]
    fn tempo_changes_preserve_accumulated_beats_and_stop_freezes_project_time() {
        let mut context: ProcessContext = unsafe { std::mem::zeroed() };
        context.sampleRate = 48_000.0;
        context.state = process_context_state(None, true);
        advance_process_context(&mut context, None, 120.0, 480_000);
        assert_eq!(context.projectTimeMusic, 20.0);
        advance_process_context(&mut context, None, 60.0, 48_000);
        assert_eq!(context.projectTimeMusic, 21.0);
        context.state = process_context_state(None, false);
        advance_process_context(&mut context, None, 60.0, 48_000);
        assert_eq!(context.projectTimeMusic, 21.0);
        assert_eq!(context.projectTimeSamples, 528_000);
        assert_eq!(context.continousTimeSamples, 576_000);
    }

    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn requesting_unknown_transport_data_never_makes_it_valid() {
        use ProcessContext_::StatesAndFlags_ as S;
        let state = process_context_state(Some(u32::MAX), true);
        for unsupported in [
            S::kBarPositionValid,
            S::kCycleValid,
            S::kClockValid,
            S::kChordValid,
            S::kSmpteValid,
        ] {
            assert_eq!(state & unsupported as u32, 0);
        }
        assert_ne!(state & S::kTempoValid as u32, 0);
    }
}

#[cfg(test)]
mod editor_edit_drain_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    type Edits = Arc<Mutex<Vec<(u32, f64)>>>;

    /// Runs one drain while another thread holds `held`, and returns its answer, or an error
    /// when it has not answered in ten seconds: a drain that waits for the lock fails rather
    /// than hangs, since the holder lets go only after the drain answers or the time is up.
    fn drain_while_held(
        held: &Edits,
        edits: &Edits,
        input: &ParameterChanges,
        stash: &Edits,
    ) -> std::result::Result<bool, mpsc::RecvTimeoutError> {
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = {
            let held = Arc::clone(held);
            thread::spawn(move || {
                let _guard = held.lock().unwrap();
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        };
        held_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let answer = thread::scope(|s| {
            s.spawn(|| {
                done_tx
                    .send(drain_editor_edits(edits, input, stash))
                    .unwrap()
            });
            let answer = done_rx.recv_timeout(Duration::from_secs(10));
            release_tx.send(()).unwrap();
            answer
        });
        holder.join().unwrap();
        answer
    }

    fn lists(edits: Vec<(u32, f64)>) -> (Edits, ParameterChanges, Edits) {
        let stash = Vec::with_capacity(MAX_EDITOR_FEEDBACK);
        (
            Arc::new(Mutex::new(edits)),
            ParameterChanges::default(),
            Arc::new(Mutex::new(stash)),
        )
    }

    /// The audio thread must not wait for `performEdit`: while the editor's thread holds the
    /// edit list, the drain returns at once and keeps the edits for the next block.
    #[test]
    fn a_held_edit_list_is_skipped_and_drained_the_next_block() {
        let (edits, input, stash) = lists(vec![(7, 0.25), (9, 0.5)]);

        let drained = drain_while_held(&edits, &edits, &input, &stash);
        assert_eq!(drained, Ok(false), "the drain waited on the editor's lock");
        assert_eq!(unsafe { input.getParameterCount() }, 0);
        assert_eq!(
            edits.lock().unwrap().len(),
            2,
            "the edits are kept for the next block"
        );

        assert!(drain_editor_edits(&edits, &input, &stash));
        assert_eq!(unsafe { input.getParameterCount() }, 2);
        assert!(edits.lock().unwrap().is_empty());
        assert_eq!(*stash.lock().unwrap(), vec![(7, 0.25), (9, 0.5)]);
    }

    /// The host's display poll holds the stash: the drain neither waits nor hands the
    /// processor an edit it would hand it again next block.
    #[test]
    fn a_held_stash_leaves_every_edit_pending() {
        let (edits, input, stash) = lists(vec![(3, 1.0)]);

        let drained = drain_while_held(&stash, &edits, &input, &stash);
        assert_eq!(
            drained,
            Ok(false),
            "the drain waited on the display poll's lock"
        );
        assert_eq!(unsafe { input.getParameterCount() }, 0);
        assert_eq!(*edits.lock().unwrap(), vec![(3, 1.0)]);

        assert!(drain_editor_edits(&edits, &input, &stash));
        assert_eq!(unsafe { input.getParameterCount() }, 1);
        assert_eq!(*stash.lock().unwrap(), vec![(3, 1.0)]);
    }
}
