//! A tiny, deterministic VST3 test instrument for verifying `vst3-host`.
//!
//! It plays one voice per MIDI note (keyed by the host-assigned `noteId`) and implements
//! `INoteExpressionController` with a single **Tuning** expression: a per-note pitch bend of
//! ±1 octave (normalized value 0.5 = no bend). That lets the host prove note-expression /
//! MPE end-to-end — something the bundled Dexed (no note expression) can't do.
//!
//! It also exposes two parameters so the host has something to drive: **Cutoff** (#0, a crude
//! one-pole low-pass on the mixed output) and **Waveform** (#1, a stepped Sine/Saw select).
//! Both default to a clean open sine so existing tests stay deterministic.
//!
//! Modeled on the `vst3` crate's `gain.rs` example. The only non-obvious detail: the macOS
//! bundle-entry symbols must be lowercase `bundleEntry`/`bundleExit` (the SDK convention our
//! CFBundle loader looks up), so we override the export names.

#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
// VST3 enum constants are generated as u32 on some targets and i32 on others, so the `as u32`
// casts are needed cross-platform even where clippy sees them as redundant (matches the host).
#![allow(clippy::unnecessary_cast)]

use std::ffi::{c_char, c_void, CString};
use std::ptr;
use std::slice;
use std::sync::Mutex;

use vst3::{uid, Class, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

const PLUGIN_NAME: &str = "VST3 Host Test Synth";

/// Parameter id for the crude low-pass cutoff (normalized 0..1, 1.0 = fully open).
const CUTOFF_PARAM_ID: u32 = 0;
/// Parameter id for the oscillator waveform (stepped: `< 0.5` = sine, `>= 0.5` = saw).
const WAVEFORM_PARAM_ID: u32 = 1;

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c = CString::new(src).unwrap_or_default();
    for (s, d) in c.as_bytes_with_nul().iter().zip(dst.iter_mut()) {
        *d = *s as c_char;
    }
    if c.as_bytes_with_nul().len() > dst.len() {
        if let Some(last) = dst.last_mut() {
            *last = 0;
        }
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (s, d) in src.encode_utf16().zip(dst.iter_mut()) {
        *d = s as TChar;
        len += 1;
    }
    if len < dst.len() {
        dst[len] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

/// One sounding sine voice.
#[derive(Clone, Copy)]
struct Voice {
    note_id: i32,
    base_freq: f64,
    phase: f64,
    /// Tuning expression, normalized 0..1 (0.5 = no bend).
    tuning: f64,
    active: bool,
}

struct SynthState {
    sample_rate: f64,
    voices: Vec<Voice>,
    /// Low-pass cutoff, normalized 0..1 (1.0 = fully open). Read from input parameter changes.
    cutoff: f64,
    /// Oscillator waveform: `< 0.5` = sine, `>= 0.5` = saw.
    waveform: f64,
    /// One-pole low-pass filter state, one per output channel.
    lp: [f64; 2],
}

struct TestSynthProcessor {
    state: Mutex<SynthState>,
}

impl Class for TestSynthProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl TestSynthProcessor {
    const CID: TUID = uid(0x54455354, 0x53594E54, 0x50524F43, 0x00000001);

    fn new() -> Self {
        Self {
            state: Mutex::new(SynthState {
                sample_rate: 48_000.0,
                voices: Vec::new(),
                cutoff: 1.0,
                waveform: 0.0, // sine by default (deterministic for tests); demo opts into saw
                lp: [0.0; 2],
            }),
        }
    }
}

/// MIDI note number → frequency (A4=69=440 Hz).
fn note_freq(pitch: f64) -> f64 {
    440.0 * 2f64.powf((pitch - 69.0) / 12.0)
}

impl IPluginBaseTrait for TestSynthProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for TestSynthProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        *class_id = TestSynthController::CID;
        kResultOk
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media_type: MediaType, dir: BusDirection) -> i32 {
        match media_type as MediaTypes {
            MediaTypes_::kAudio => match dir as BusDirections {
                BusDirections_::kOutput => 1,
                _ => 0,
            },
            MediaTypes_::kEvent => match dir as BusDirections {
                BusDirections_::kInput => 1,
                _ => 0,
            },
            _ => 0,
        }
    }
    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        let mt = media_type as MediaTypes;
        let d = dir as BusDirections;
        if mt == MediaTypes_::kAudio && d == BusDirections_::kOutput && index == 0 {
            let bus = &mut *bus;
            bus.mediaType = MediaTypes_::kAudio as MediaType;
            bus.direction = BusDirections_::kOutput as BusDirection;
            bus.channelCount = 2;
            copy_wstring("Output", &mut bus.name);
            bus.busType = BusTypes_::kMain as BusType;
            bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
            kResultOk
        } else if mt == MediaTypes_::kEvent && d == BusDirections_::kInput && index == 0 {
            let bus = &mut *bus;
            bus.mediaType = MediaTypes_::kEvent as MediaType;
            bus.direction = BusDirections_::kInput as BusDirection;
            bus.channelCount = 16;
            copy_wstring("Event In", &mut bus.name);
            bus.busType = BusTypes_::kMain as BusType;
            bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
            kResultOk
        } else {
            kInvalidArgument
        }
    }
    unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
}

impl IAudioProcessorTrait for TestSynthProcessor {
    unsafe fn setBusArrangements(
        &self,
        _inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins != 0 || num_outs != 1 {
            return kResultFalse;
        }
        if *outputs != SpeakerArr::kStereo {
            return kResultFalse;
        }
        kResultTrue
    }
    unsafe fn getBusArrangement(
        &self,
        dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if dir as BusDirections == BusDirections_::kOutput && index == 0 {
            *arr = SpeakerArr::kStereo;
            kResultOk
        } else {
            kInvalidArgument
        }
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        match size as SymbolicSampleSizes {
            SymbolicSampleSizes_::kSample32 => kResultOk,
            _ => kNotImplemented,
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if let Ok(mut s) = self.state.lock() {
            s.sample_rate = (*setup).sampleRate;
            s.voices.clear();
            s.lp = [0.0; 2];
        }
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = &*data;
        let Ok(mut state) = self.state.lock() else {
            return kResultOk;
        };

        // Apply input events: note on/off (keyed by noteId) and Tuning note-expression.
        if let Some(events) = ComRef::from_raw(data.inputEvents) {
            let count = events.getEventCount();
            for i in 0..count {
                let mut ev: Event = std::mem::zeroed();
                if events.getEvent(i, &mut ev) != kResultOk {
                    continue;
                }
                match ev.r#type as u32 {
                    t if t == Event_::EventTypes_::kNoteOnEvent as u32 => {
                        let n = ev.__field0.noteOn;
                        state.voices.push(Voice {
                            note_id: n.noteId,
                            base_freq: note_freq(n.pitch as f64),
                            phase: 0.0,
                            tuning: 0.5,
                            active: true,
                        });
                    }
                    t if t == Event_::EventTypes_::kNoteOffEvent as u32 => {
                        let n = ev.__field0.noteOff;
                        for v in state.voices.iter_mut() {
                            if v.note_id == n.noteId || n.noteId == -1 {
                                v.active = false;
                            }
                        }
                    }
                    t if t == Event_::EventTypes_::kNoteExpressionValueEvent as u32 => {
                        let nx = ev.__field0.noteExpressionValue;
                        if nx.typeId == NoteExpressionTypeIDs_::kTuningTypeID as u32 {
                            for v in state.voices.iter_mut() {
                                if v.note_id == nx.noteId {
                                    v.tuning = nx.value;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Read parameter changes the host queued (cutoff + waveform). We take the last point of
        // each queue for the block (block-granular — crude, but enough for a test synth).
        if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
            for i in 0..changes.getParameterCount() {
                let queue = changes.getParameterData(i);
                let Some(queue) = ComRef::from_raw(queue) else {
                    continue;
                };
                let points = queue.getPointCount();
                if points <= 0 {
                    continue;
                }
                let mut offset = 0i32;
                let mut value = 0f64;
                if queue.getPoint(points - 1, &mut offset, &mut value) != kResultOk {
                    continue;
                }
                match queue.getParameterId() {
                    CUTOFF_PARAM_ID => state.cutoff = value.clamp(0.0, 1.0),
                    WAVEFORM_PARAM_ID => state.waveform = value.clamp(0.0, 1.0),
                    _ => {}
                }
            }
        }

        let num_samples = data.numSamples as usize;
        if data.numOutputs < 1 || num_samples == 0 {
            return kResultOk;
        }
        let out_buses = slice::from_raw_parts(data.outputs, data.numOutputs as usize);
        if out_buses[0].numChannels < 1 {
            return kResultOk;
        }
        // Raw per-channel output pointers (channelBuffers32 is *mut *mut f32). We write through
        // these directly rather than building overlapping &mut slices (which would be UB).
        let out_ptrs: Vec<*mut f32> = slice::from_raw_parts(
            out_buses[0].__field0.channelBuffers32,
            out_buses[0].numChannels as usize,
        )
        .to_vec();

        // Clear output.
        for &p in &out_ptrs {
            for s in 0..num_samples {
                *p.add(s) = 0.0;
            }
        }

        let sr = state.sample_rate.max(1.0);
        let amp = 0.25_f32;
        let saw = state.waveform >= 0.5;
        for v in state.voices.iter_mut().filter(|v| v.active) {
            // Tuning: normalized 0..1, 0.5 = center; ±1 octave at the extremes.
            let bend_semitones = (v.tuning - 0.5) * 24.0;
            let freq = v.base_freq * 2f64.powf(bend_semitones / 12.0);
            let inc = std::f64::consts::TAU * freq / sr;
            let mut phase = v.phase;
            for s in 0..num_samples {
                // Sine, or a naive (aliasing) saw ramp -1..1 — crude on purpose.
                let sample = if saw {
                    (2.0 * (phase / std::f64::consts::TAU) - 1.0) as f32 * amp
                } else {
                    (phase.sin() as f32) * amp
                };
                for &p in &out_ptrs {
                    *p.add(s) += sample;
                }
                phase += inc;
                if phase > std::f64::consts::TAU {
                    phase -= std::f64::consts::TAU;
                }
            }
            v.phase = phase;
        }

        // Crude one-pole low-pass filter on the mixed output (per channel), driven by Cutoff.
        let fc = 20.0 * 1000f64.powf(state.cutoff); // ~20 Hz (closed) .. ~20 kHz (open)
        let alpha = (1.0 - (-std::f64::consts::TAU * fc / sr).exp()).clamp(0.0, 1.0);
        for (ch, &p) in out_ptrs.iter().enumerate() {
            if ch >= state.lp.len() {
                break;
            }
            let mut y = state.lp[ch];
            for s in 0..num_samples {
                let x = *p.add(s) as f64;
                y += alpha * (x - y);
                *p.add(s) = y as f32;
            }
            state.lp[ch] = y;
        }

        // Drop voices that finished (note off).
        state.voices.retain(|v| v.active);
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for TestSynthProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

struct TestSynthController {
    cutoff: Mutex<f64>,
    waveform: Mutex<f64>,
}

impl Class for TestSynthController {
    type Interfaces = (IEditController, INoteExpressionController);
}

impl TestSynthController {
    const CID: TUID = uid(0x54455354, 0x53594E54, 0x4354524C, 0x00000001);

    fn new() -> Self {
        Self {
            cutoff: Mutex::new(1.0),
            waveform: Mutex::new(0.0), // sine by default
        }
    }
}

impl IPluginBaseTrait for TestSynthController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for TestSynthController {
    unsafe fn setComponentState(&self, _s: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn setState(&self, _s: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _s: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> i32 {
        2
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let info = &mut *info;
        match index {
            0 => {
                info.id = CUTOFF_PARAM_ID;
                copy_wstring("Cutoff", &mut info.title);
                copy_wstring("Cutoff", &mut info.shortTitle);
                copy_wstring("", &mut info.units);
                info.stepCount = 0; // continuous
                info.defaultNormalizedValue = 1.0;
                info.unitId = 0;
                info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
                kResultOk
            }
            1 => {
                info.id = WAVEFORM_PARAM_ID;
                copy_wstring("Waveform", &mut info.title);
                copy_wstring("Wave", &mut info.shortTitle);
                copy_wstring("", &mut info.units);
                info.stepCount = 1; // two discrete values: Sine / Saw
                info.defaultNormalizedValue = 0.0; // Sine
                info.unitId = 0;
                info.flags = (ParameterInfo_::ParameterFlags_::kCanAutomate
                    | ParameterInfo_::ParameterFlags_::kIsList) as i32;
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }
    unsafe fn getParamStringByValue(&self, id: u32, v: f64, s: *mut String128) -> tresult {
        match id {
            WAVEFORM_PARAM_ID => {
                let label = if v >= 0.5 { "Saw" } else { "Sine" };
                copy_wstring(label, &mut *s);
                kResultOk
            }
            CUTOFF_PARAM_ID => {
                let text = format!("{:.0}%", v * 100.0);
                copy_wstring(&text, &mut *s);
                kResultOk
            }
            _ => kNotImplemented,
        }
    }
    unsafe fn getParamValueByString(&self, _id: u32, _s: *mut TChar, _v: *mut f64) -> tresult {
        kNotImplemented
    }
    unsafe fn normalizedParamToPlain(&self, _id: u32, v: f64) -> f64 {
        v
    }
    unsafe fn plainParamToNormalized(&self, _id: u32, v: f64) -> f64 {
        v
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        match id {
            CUTOFF_PARAM_ID => *self.cutoff.lock().unwrap_or_else(|p| p.into_inner()),
            WAVEFORM_PARAM_ID => *self.waveform.lock().unwrap_or_else(|p| p.into_inner()),
            _ => 0.0,
        }
    }
    unsafe fn setParamNormalized(&self, id: u32, v: f64) -> tresult {
        match id {
            CUTOFF_PARAM_ID => *self.cutoff.lock().unwrap_or_else(|p| p.into_inner()) = v,
            WAVEFORM_PARAM_ID => *self.waveform.lock().unwrap_or_else(|p| p.into_inner()) = v,
            _ => {}
        }
        kResultOk
    }
    unsafe fn setComponentHandler(&self, _h: *mut IComponentHandler) -> tresult {
        kResultOk
    }
    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        ptr::null_mut()
    }
}

impl INoteExpressionControllerTrait for TestSynthController {
    unsafe fn getNoteExpressionCount(&self, _bus: i32, _channel: i16) -> i32 {
        1
    }
    unsafe fn getNoteExpressionInfo(
        &self,
        _bus: i32,
        _channel: i16,
        index: i32,
        info: *mut NoteExpressionTypeInfo,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let info = &mut *info;
        info.typeId = NoteExpressionTypeIDs_::kTuningTypeID as u32;
        copy_wstring("Tuning", &mut info.title);
        copy_wstring("Tun", &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.unitId = 0;
        info.valueDesc.defaultValue = 0.5;
        info.valueDesc.minimum = 0.0;
        info.valueDesc.maximum = 1.0;
        info.valueDesc.stepCount = 0;
        info.associatedParameterId = 0;
        info.flags = NoteExpressionTypeInfo_::NoteExpressionTypeFlags_::kIsBipolar as i32;
        kResultOk
    }
    unsafe fn getNoteExpressionStringByValue(
        &self,
        _bus: i32,
        _channel: i16,
        _id: u32,
        _value: f64,
        _string: *mut String128,
    ) -> tresult {
        kNotImplemented
    }
    unsafe fn getNoteExpressionValueByString(
        &self,
        _bus: i32,
        _channel: i16,
        _id: u32,
        _string: *const TChar,
        _value: *mut f64,
    ) -> tresult {
        kNotImplemented
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = &mut *info;
        copy_cstring("vst3-host", &mut info.vendor);
        copy_cstring(
            "https://github.com/HelgeSverre/rust-vst3-host",
            &mut info.url,
        );
        copy_cstring("test@example.com", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        2
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let info = &mut *info;
        match index {
            0 => {
                info.cid = TestSynthProcessor::CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
                copy_cstring("Audio Module Class", &mut info.category);
                copy_cstring(PLUGIN_NAME, &mut info.name);
                kResultOk
            }
            1 => {
                info.cid = TestSynthController::CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
                copy_cstring("Component Controller Class", &mut info.category);
                copy_cstring(PLUGIN_NAME, &mut info.name);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }
    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let instance = match *(cid as *const TUID) {
            TestSynthProcessor::CID => Some(
                ComWrapper::new(TestSynthProcessor::new())
                    .to_com_ptr::<FUnknown>()
                    .unwrap(),
            ),
            TestSynthController::CID => Some(
                ComWrapper::new(TestSynthController::new())
                    .to_com_ptr::<FUnknown>()
                    .unwrap(),
            ),
            _ => None,
        };
        if let Some(instance) = instance {
            let ptr = instance.as_ptr();
            ((*(*ptr).vtbl).queryInterface)(ptr, iid as *mut TUID, obj)
        } else {
            kInvalidArgument
        }
    }
}

#[no_mangle]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .unwrap()
        .into_raw()
}

// macOS: the SDK convention (and our CFBundle loader) uses LOWERCASE bundleEntry/bundleExit.
#[cfg(target_os = "macos")]
#[export_name = "bundleEntry"]
extern "system" fn bundle_entry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[export_name = "bundleExit"]
extern "system" fn bundle_exit() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleEntry(_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleExit() -> bool {
    true
}
