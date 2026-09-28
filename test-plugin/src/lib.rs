//! A tiny, deterministic VST3 test instrument for verifying `vst3-host`.
//!
//! It plays one voice per MIDI note (keyed by the host-assigned `noteId`) and implements
//! `INoteExpressionController` with a single **Tuning** expression: a per-note pitch bend of
//! ±1 octave (normalized value 0.5 = no bend). That lets the host prove note-expression /
//! MPE end-to-end — something the bundled Dexed (no note expression) can't do.
//!
//! It also exposes parameters so the host has something to drive: **Cutoff** (#0) and
//! **Resonance** (#4) of a per-voice 24 dB/oct zero-delay-feedback ladder low-pass,
//! **Waveform** (#1, stepped Sine / Saw / Super Saw), **Detune** (#2) and **Mix** (#3) for the
//! super saw, a full **amp ADSR** (#5–#8), a **filter ADSR** (#9–#12) and **Filter Env
//! Amount** (#13) that pushes the cutoff up per note (the trance pluck). The super saw stacks
//! 7 detuned saws after Adam Szabo's JP-8000 analysis ("How To Emulate The Super Saw", 2010):
//! golden-ratio start phases (the JP-8000's oscillators free-run, so note-on phase is
//! effectively random — a fixed scramble keeps tests deterministic), a high-pass at the
//! fundamental to remove the sub-fundamental beating rumble, and Szabo's detune/mix curves.
//! super saw is stereo (side oscillators panned equal-power, adjacent detunes on opposite
//! sides), band-limited (polyBLEP), and drifts a deterministic ~±1.6 cents like free-running
//! hardware; the filter keytracks and its ladder input is gently tanh-driven. Voices are
//! velocity-sensitive. Beyond sound, it exercises the host's optional plugin interfaces:
//! events are handled **sample-accurately** (the block is split at event offsets), state
//! **persists** via getState/setState (all params, versioned blob), three factory programs
//! (**Program**, #14) are exposed through `IUnitInfo` + a `kIsProgramChange` parameter,
//! `IMidiMapping` routes mod wheel / GM2 sound controllers (CC 71-74) onto parameters, and
//! the synth is **bitimbral**: MIDI channel 1 plays the live parameters while channel 2
//! plays the factory preset chosen by **Ch2 Program** (#15) at **Ch2 Level** (#16) —
//! exercising per-channel event routing in the host.
//! Defaults keep the old behavior (sine, sustain 1, env amount 0) so tests stay deterministic.
//!
//! The factory exports **two** audio classes so the host's multi-class handling
//! (`Vst3Host::load_plugin_class`) has something to resolve against: the dual-object synth above
//! (class index 0, the one a plain `load_plugin` picks) and a **single-component** variant
//! ([`TestSynthSingle`], class index 3) — one object implementing both `IComponent` and
//! `IEditController`, whose `getControllerClassId` reports none. It shares this file's DSP and
//! state format and exposes five parameters, so the host's single-component state path (one
//! stream, no controller half, applied exactly once) is verifiable end to end.
//!
//! The dual synth's controller also implements a real `IPlugView` — no drawing, but the whole
//! embedding protocol: platform-type negotiation, attach/remove tracking, `getSize`/`onSize`,
//! `checkSizeConstraint` clamping and `IPlugViewContentScaleSupport`. Right after it is
//! attached it asks the host to resize it once through `IPlugFrame::resizeView`, which closes
//! the loop on the host's resize chain. What the view saw — and what the host said about the
//! streams it restores state from — is republished as read-only parameters (ids 1000+, see
//! "Editor and state instrumentation" below) so a host test can assert on it without a GUI.
//!
//! Modeled on the `vst3` crate's `gain.rs` example. The only non-obvious detail: the macOS
//! bundle-entry symbols must be lowercase `bundleEntry`/`bundleExit` (the SDK convention our
//! CFBundle loader looks up), so we override the export names.

#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
// VST3 enum constants are generated as u32 on some targets and i32 on others, so the `as u32`
// casts are needed cross-platform even where clippy sees them as redundant (matches the host).
#![allow(clippy::unnecessary_cast)]

use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use vst3::{uid, Class, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

const PLUGIN_NAME: &str = "VST3 Host Test Synth";
/// Display name of the single-component audio class (factory class index 3).
const SINGLE_PLUGIN_NAME: &str = "VST3 Host Test Synth (Single)";

/// Advertised latency/tail (samples). Nonzero on purpose — host tests assert these exact
/// values to prove the accessors really reach the plugin (in-process and over IPC).
const TEST_LATENCY_SAMPLES: u32 = 32;
const TEST_TAIL_SAMPLES: u32 = 4800;
/// Class id of a deterministic fictional predecessor used to exercise `IRemapParamID`.
/// Canonical text: `4F4C44504C5547494E43494400000001`.
const REPLACED_PLUGIN_UID: TUID = uid(0x4F4C4450, 0x4C554749, 0x4E434944, 0x00000001);
const REPLACED_CUTOFF_PARAM_ID: u32 = 0xCAFE_BABE;

/// Parameter id for the crude low-pass cutoff (normalized 0..1, 1.0 = fully open).
const CUTOFF_PARAM_ID: u32 = 0;
/// Parameter id for the oscillator waveform (stepped: sine / saw / super saw).
const WAVEFORM_PARAM_ID: u32 = 1;
/// Parameter id for the super-saw detune spread (normalized 0..1).
const DETUNE_PARAM_ID: u32 = 2;
/// Parameter id for the super-saw center/side mix (normalized 0..1).
const MIX_PARAM_ID: u32 = 3;
/// Parameter id for the low-pass filter resonance (normalized 0..1, 0.0 = no peak).
const RESONANCE_PARAM_ID: u32 = 4;
/// Amp envelope ADSR parameter ids (attack/decay/release are times, sustain is a level).
const AMP_ATTACK_PARAM_ID: u32 = 5;
const AMP_DECAY_PARAM_ID: u32 = 6;
const AMP_SUSTAIN_PARAM_ID: u32 = 7;
const AMP_RELEASE_PARAM_ID: u32 = 8;
/// Filter envelope ADSR parameter ids.
const FILTER_ATTACK_PARAM_ID: u32 = 9;
const FILTER_DECAY_PARAM_ID: u32 = 10;
const FILTER_SUSTAIN_PARAM_ID: u32 = 11;
const FILTER_RELEASE_PARAM_ID: u32 = 12;
/// How much the filter envelope pushes the cutoff up (normalized; 0 = filter env off).
const FILTER_ENV_AMOUNT_PARAM_ID: u32 = 13;
/// Program-change parameter (kIsProgramChange, tied to the root unit's factory program list).
const PROGRAM_PARAM_ID: u32 = 14;
/// Which factory preset shapes the second timbre (MIDI channel 2). The synth is bitimbral:
/// channel 1 plays the live parameters, channel 2 plays this preset at `Ch2 Level`.
const CH2_PROGRAM_PARAM_ID: u32 = 15;
/// Channel-2 part level (0 = part off — the deterministic default).
const CH2_LEVEL_PARAM_ID: u32 = 16;
/// Channel-1 part level (default 1.0). Lets a host solo the parts, e.g. to render them to
/// separate buses for per-part effects.
const CH1_LEVEL_PARAM_ID: u32 = 17;

const PARAM_COUNT: i32 = 18;

/// Parameter names and defaults, indexed by parameter id.
const PARAM_NAMES: [&str; PARAM_COUNT as usize] = [
    "Cutoff",
    "Waveform",
    "Detune",
    "Mix",
    "Resonance",
    "Amp Attack",
    "Amp Decay",
    "Amp Sustain",
    "Amp Release",
    "Filter Attack",
    "Filter Decay",
    "Filter Sustain",
    "Filter Release",
    "Filter Env Amount",
    "Program",
    "Ch2 Program",
    "Ch2 Level",
    "Ch1 Level",
];
const PARAM_DEFAULTS: [f64; PARAM_COUNT as usize] = [
    1.0, 0.0, 0.3, 0.5, 0.0, // cutoff, waveform, detune, mix, resonance
    0.09, 0.5, 1.0, 0.59, // amp ADSR (~2 ms attack, full sustain, ~90 ms release)
    0.0, 0.65, 0.2, 0.55, // filter ADSR
    0.0,  // filter env amount (off — deterministic for tests)
    0.0,  // program (0 = Init Sine, which matches the defaults above)
    1.0,  // ch2 program (Lush Pad — inert while ch2 level is zero)
    0.0,  // ch2 level (part off — deterministic for tests)
    1.0,  // ch1 level (full — deterministic for tests)
];

/// The root unit's program list id (IUnitInfo).
const PROGRAM_LIST_ID: i32 = 1;

/// Factory programs: name + values for parameters #0..#13. Program 0 must equal
/// `PARAM_DEFAULTS` so the power-on sound stays deterministic for tests.
const PRESETS: [(&str, [f64; 14]); 4] = [
    (
        "Init Sine",
        [
            1.0, 0.0, 0.3, 0.5, 0.0, 0.09, 0.5, 1.0, 0.59, 0.0, 0.65, 0.2, 0.55, 0.0,
        ],
    ),
    (
        "Trance Pluck",
        [
            0.42, 1.0, 0.6, 0.7, 0.3, 0.0, 0.62, 0.12, 0.55, 0.0, 0.62, 0.1, 0.5, 0.55,
        ],
    ),
    (
        "Super Lead",
        [
            0.6, 1.0, 0.6, 0.7, 0.25, 0.09, 0.7, 1.0, 0.62, 0.0, 0.65, 0.2, 0.55, 0.2,
        ],
    ),
    (
        "Lush Pad",
        [
            0.5, 1.0, 0.5, 0.62, 0.1, 0.78, 0.7, 1.0, 0.82, 0.75, 0.8, 0.6, 0.75, 0.25,
        ],
    ),
];

/// State blob header: `TSY1` magic + little-endian param count, then `count` LE f64 values.
const STATE_MAGIC: u32 = 0x5453_5931;
/// Controller-only state: `TSC1` magic followed by a little-endian edit revision.
const CONTROLLER_STATE_MAGIC: u32 = 0x5453_4331;

// --- Editor and state instrumentation ---------------------------------------------------
//
// The editor handshake and the stream metadata a host attaches to `setState` are both invisible
// from a host's safe API: they are pure side effects inside the plugin. So the plugin records
// what it saw and republishes it as **read-only parameters**, which a host test reads back with
// `Plugin::get_parameter`. Ids start at 1000, well clear of the synth parameters (0..=17).
//
// Every encoding divides by a power of two, so the normalized value round-trips exactly through
// f64 and a test can decode it with `(value * SCALE).round()` and no tolerance.
//
// | id   | name            | encoding                                                        |
// |------|-----------------|-----------------------------------------------------------------|
// | 1000 | Editor Attached | 0.0 detached, 1.0 attached                                      |
// | 1001 | Editor Width    | last `onSize` width  / `EDITOR_SIZE_SCALE` (0.0 = no `onSize`)  |
// | 1002 | Editor Height   | last `onSize` height / `EDITOR_SIZE_SCALE`                      |
// | 1003 | Editor Scale    | content scale factor / `EDITOR_SCALE_SCALE` (0.0 = never set)   |
// | 1004 | State Type Seen | `StateType` code / `STATE_PROBE_SCALE`, see `state_type_seen`   |
// | 1005 | State Path Seen | path bitmask / `STATE_PROBE_SCALE`, see `STATE_PATH_*`          |
// | 1010 | State Applies   | `setState`+`setComponentState` count / `STATE_APPLY_SCALE`      |
//
// 1000..=1005 live on the dual synth's controller; 1010 lives on the single-component class.

/// Whether the view is currently attached to a host window.
const EDITOR_ATTACHED_PARAM_ID: u32 = 1000;
/// Width of the most recent `IPlugView::onSize`.
const EDITOR_WIDTH_PARAM_ID: u32 = 1001;
/// Height of the most recent `IPlugView::onSize`.
const EDITOR_HEIGHT_PARAM_ID: u32 = 1002;
/// The most recent `IPlugViewContentScaleSupport::setContentScaleFactor` argument.
const EDITOR_SCALE_PARAM_ID: u32 = 1003;
/// Which `StateType` the last `IComponent::setState` stream advertised.
const STATE_TYPE_PARAM_ID: u32 = 1004;
/// Which file-path metadata the last `IComponent::setState` stream carried.
const STATE_PATH_PARAM_ID: u32 = 1005;
/// How often the single-component class has had component state applied to it.
const STATE_APPLY_PARAM_ID: u32 = 1010;

/// Divisor turning a pixel count into a normalized parameter value (and back).
const EDITOR_SIZE_SCALE: f64 = 4096.0;
/// Divisor turning a content scale factor into a normalized parameter value (and back).
const EDITOR_SCALE_SCALE: f64 = 8.0;
/// Divisor for the two small state-probe codes.
const STATE_PROBE_SCALE: f64 = 4.0;
/// Divisor for the single-component state-apply counter.
const STATE_APPLY_SCALE: f64 = 64.0;

/// `StateType` codes reported through [`STATE_TYPE_PARAM_ID`].
mod state_type_seen {
    /// The stream published no `StateType` attribute (or one this plugin does not know).
    pub const NONE: u32 = 0;
    /// `Steinberg::Vst::StateType::kDefault`.
    pub const DEFAULT: u32 = 1;
    /// `Steinberg::Vst::StateType::kProject`.
    pub const PROJECT: u32 = 2;
    /// `Steinberg::Vst::StateType::kTrackPreset`.
    pub const TRACK_PRESET: u32 = 3;
}

/// [`STATE_PATH_PARAM_ID`] bit 0: the stream published `PresetAttributes::kFilePathStringType`.
const STATE_PATH_ATTRIBUTE: u32 = 0b01;
/// [`STATE_PATH_PARAM_ID`] bit 1: `IStreamAttributes::getFileName` returned a non-empty name.
const STATE_PATH_FILE_NAME: u32 = 0b10;

/// The size `IPlugView::getSize` reports before the host has resized anything.
const EDITOR_SIZE: (i32, i32) = (480, 320);
/// Smallest size `checkSizeConstraint` will agree to.
const EDITOR_MIN_SIZE: (i32, i32) = (240, 160);
/// Largest size `checkSizeConstraint` will agree to.
const EDITOR_MAX_SIZE: (i32, i32) = (960, 640);
/// The one size the view asks the host for through `IPlugFrame::resizeView` after it attaches.
/// Deliberately different from [`EDITOR_SIZE`] so the host's container really has to move.
const EDITOR_SELF_RESIZE: (i32, i32) = (560, 400);

/// The `IPlugView` platform type this build embeds into.
#[cfg(target_os = "macos")]
const HOST_PLATFORM_TYPE: FIDString = kPlatformTypeNSView;
/// The `IPlugView` platform type this build embeds into.
#[cfg(target_os = "windows")]
const HOST_PLATFORM_TYPE: FIDString = kPlatformTypeHWND;
/// The `IPlugView` platform type this build embeds into.
#[cfg(any(target_os = "linux", target_os = "android"))]
const HOST_PLATFORM_TYPE: FIDString = kPlatformTypeX11EmbedWindowID;

/// Does `candidate` name the platform window type this build can embed into?
///
/// # Safety
/// `candidate` must be null or a NUL-terminated C string.
unsafe fn platform_type_matches(candidate: FIDString) -> bool {
    !candidate.is_null() && CStr::from_ptr(candidate) == CStr::from_ptr(HOST_PLATFORM_TYPE)
}

/// What the plugin's editor view saw during the host's embedding handshake.
///
/// Shared between the view (which records) and the controller (which publishes it as read-only
/// parameters). Everything is an atomic on purpose: the host answers `IPlugFrame::resizeView`
/// with `IPlugView::onSize` *in the same callstack*, so the view must never hold a lock across
/// a call into the host.
#[derive(Default)]
struct EditorProbe {
    attached: AtomicBool,
    /// Size of the most recent `onSize`, or `(0, 0)` if there has not been one.
    last_size: (AtomicI32, AtomicI32),
    /// Content scale factor times 256, or 0 when the host never offered one. 256ths keep the
    /// usual factors (1, 1.25, 1.5, 2, 3) exact.
    scale_x256: AtomicU32,
}

impl EditorProbe {
    fn record_size(&self, width: i32, height: i32) {
        self.last_size.0.store(width, Ordering::Release);
        self.last_size.1.store(height, Ordering::Release);
    }

    /// The normalized value of one instrumentation parameter, or `None` if `id` is not one.
    fn parameter(&self, id: u32) -> Option<f64> {
        let value = match id {
            EDITOR_ATTACHED_PARAM_ID => f64::from(self.attached.load(Ordering::Acquire)),
            EDITOR_WIDTH_PARAM_ID => {
                f64::from(self.last_size.0.load(Ordering::Acquire)) / EDITOR_SIZE_SCALE
            }
            EDITOR_HEIGHT_PARAM_ID => {
                f64::from(self.last_size.1.load(Ordering::Acquire)) / EDITOR_SIZE_SCALE
            }
            EDITOR_SCALE_PARAM_ID => {
                f64::from(self.scale_x256.load(Ordering::Acquire)) / 256.0 / EDITOR_SCALE_SCALE
            }
            _ => return None,
        };
        Some(value.clamp(0.0, 1.0))
    }
}

/// The `StateType` the last `IComponent::setState` stream advertised (a `state_type_seen` code).
///
/// Process-global because the dual synth's component and controller are separate COM objects and
/// VST3 gives them no channel for this — the controller has to republish something the component
/// observed. Sound here because the host drives one TestSynth instance at a time in these tests;
/// two concurrent instances would share the observation.
static OBSERVED_STATE_TYPE: AtomicU32 = AtomicU32::new(state_type_seen::NONE);
/// File-path metadata on that same stream: a mask of [`STATE_PATH_ATTRIBUTE`] and
/// [`STATE_PATH_FILE_NAME`]. Process-global for the same reason as [`OBSERVED_STATE_TYPE`].
static OBSERVED_STATE_PATH: AtomicU32 = AtomicU32::new(0);

/// Read one string attribute, or `None` when it is absent or empty.
///
/// # Safety
/// `list` must be a live `IAttributeList` and `key` a NUL-terminated C string.
unsafe fn attribute_string(
    list: &vst3::ComPtr<IAttributeList>,
    key: *const c_char,
) -> Option<String> {
    let mut buf = [0 as TChar; 128];
    let size_in_bytes = std::mem::size_of_val(&buf) as u32;
    if list.getString(key, buf.as_mut_ptr(), size_in_bytes) != kResultOk {
        return None;
    }
    let len = buf.iter().position(|unit| *unit == 0).unwrap_or(buf.len());
    (len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
}

/// Record what the host said about a state stream before reading it, into [`OBSERVED_STATE_TYPE`]
/// and [`OBSERVED_STATE_PATH`].
///
/// A host that hands over an untagged stream is recorded as such (`NONE` / empty mask) rather
/// than leaving a stale reading behind.
///
/// # Safety
/// `stream` must be null or a live `IBStream`.
unsafe fn record_state_stream_context(stream: *mut IBStream) {
    let attributes = ComRef::from_raw(stream).and_then(|s| s.cast::<IStreamAttributes>());
    let Some(attributes) = attributes else {
        OBSERVED_STATE_TYPE.store(state_type_seen::NONE, Ordering::Release);
        OBSERVED_STATE_PATH.store(0, Ordering::Release);
        return;
    };

    let mut state_type = state_type_seen::NONE;
    let mut path_mask = 0u32;
    if let Some(list) = ComRef::from_raw(attributes.getAttributes()).map(|list| list.to_com_ptr()) {
        state_type = match attribute_string(&list, PresetAttributes::kStateType).as_deref() {
            Some("Default") => state_type_seen::DEFAULT,
            Some("Project") => state_type_seen::PROJECT,
            Some("TrackPreset") => state_type_seen::TRACK_PRESET,
            _ => state_type_seen::NONE,
        };
        if attribute_string(&list, PresetAttributes::kFilePathStringType).is_some() {
            path_mask |= STATE_PATH_ATTRIBUTE;
        }
    }
    let mut file_name: String128 = std::mem::zeroed();
    if attributes.getFileName(&mut file_name) == kResultOk && file_name[0] != 0 {
        path_mask |= STATE_PATH_FILE_NAME;
    }

    OBSERVED_STATE_TYPE.store(state_type, Ordering::Release);
    OBSERVED_STATE_PATH.store(path_mask, Ordering::Release);
}

/// Cutoff keyboard tracking: how far the (normalized, 3-decade) cutoff follows the note.
const KEYTRACK: f64 = 0.4;
/// One octave of frequency expressed in the normalized cutoff domain (fc = 20·1000^x).
const OCTAVE_IN_CUTOFF: f64 = 0.100_343;
/// Analogue drift depth as a pitch ratio (~±1.6 cents), supersaw only.
const DRIFT_RATIO: f64 = 1.6 * 0.000_578;

/// Per-oscillator stereo pan positions (-1 = hard left, +1 = hard right). Adjacent detunes
/// alternate sides so the stack spreads without lopsided beating; index 3 (center) stays mono.
const SUPERSAW_PAN: [f64; 7] = [-0.9, 0.55, -0.25, 0.0, 0.25, -0.55, 0.9];

/// PolyBLEP residual: subtract from a naive saw to band-limit its discontinuity.
/// `t` is the phase in [0,1), `dt` the per-sample phase increment.
fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

/// Is this parameter an envelope *time* (displayed in ms/s rather than %)?
fn is_time_param(id: u32) -> bool {
    matches!(
        id,
        AMP_ATTACK_PARAM_ID
            | AMP_DECAY_PARAM_ID
            | AMP_RELEASE_PARAM_ID
            | FILTER_ATTACK_PARAM_ID
            | FILTER_DECAY_PARAM_ID
            | FILTER_RELEASE_PARAM_ID
    )
}

/// A released voice is dropped once its amp envelope decays below this (~ -80 dB).
const ENV_SILENCE: f64 = 1e-4;

/// Envelope time knob (normalized 0..1) → seconds: 1 ms at 0, ~45 ms at 0.5, 2 s at 1.
fn env_time_secs(x: f64) -> f64 {
    0.001 * 2000f64.powf(x)
}

/// ADSR knob settings (all normalized 0..1; times map through `env_time_secs`).
#[derive(Clone, Copy)]
struct AdsrParams {
    a: f64,
    d: f64,
    s: f64,
    r: f64,
}

/// Per-stage one-pole coefficients derived from `AdsrParams` once per block.
#[derive(Clone, Copy)]
struct AdsrCoefs {
    a: f64,
    d: f64,
    s: f64,
    r: f64,
}

impl AdsrCoefs {
    fn new(p: AdsrParams, sr: f64) -> Self {
        // The knob time is the *full stage traversal*, not the one-pole time constant: the
        // exponential covers ~95% of its span in 3τ (decay/release) and the overshooting
        // attack hits 1.0 in ~2.6τ, so divide τ accordingly — a "100 ms" decay sounds like
        // 100 ms, which is what makes short settings actually snap.
        let coef = |x: f64, mult: f64| 1.0 - (-mult / (env_time_secs(x) * sr)).exp();
        Self {
            a: coef(p.a, 2.6),
            d: coef(p.d, 3.0),
            s: p.s,
            r: coef(p.r, 3.0),
        }
    }
}

/// A running ADSR: attack → decay toward sustain while gated, exponential release after.
#[derive(Clone, Copy)]
struct Adsr {
    level: f64,
    attacking: bool,
}

impl Adsr {
    fn new() -> Self {
        Self {
            level: 0.0,
            attacking: true,
        }
    }

    fn next(&mut self, gate: bool, c: &AdsrCoefs) -> f64 {
        if !gate {
            self.level -= c.r * self.level;
        } else if self.attacking {
            // Aim slightly past 1.0 so the (exponential) attack actually arrives.
            self.level += c.a * (1.08 - self.level);
            if self.level >= 1.0 {
                self.level = 1.0;
                self.attacking = false;
            }
        } else {
            self.level += c.d * (c.s - self.level);
        }
        self.level
    }
}

/// Super-saw oscillators: 7 detuned saws, after Adam Szabo's JP-8000 analysis
/// ("How To Emulate The Super Saw", 2010). These are the relative detune offsets of the
/// 7 oscillators (index 3 = center, in tune); the actual offset is scaled by the detune knob.
const SUPERSAW_DETUNE: [f64; 7] = [
    -0.11002313,
    -0.06288439,
    -0.01952356,
    0.0,
    0.01991221,
    0.06216538,
    0.10745242,
];

/// Szabo's empirical detune curve: maps the detune knob `x` (0..1) to the amount that scales the
/// per-oscillator offsets above. An 11th-order polynomial fit of the JP-8000's (non-linear) knob
/// response, so most of the musically useful detune lives in the lower half of the knob.
fn supersaw_detune_curve(x: f64) -> f64 {
    (10028.7312891634 * x.powi(11)) - (50818.8652045924 * x.powi(10))
        + (111363.4808729368 * x.powi(9))
        - (138150.6761080548 * x.powi(8))
        + (106649.6679158292 * x.powi(7))
        - (53046.9642751875 * x.powi(6))
        + (17019.9518580080 * x.powi(5))
        - (3425.0836591318 * x.powi(4))
        + (404.2703938388 * x.powi(3))
        - (24.1878824391 * x.powi(2))
        + (0.6717417634 * x)
        + 0.0030115596
}

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

/// One sounding voice (up to 7 oscillators for the super-saw; index 0 is used for sine/saw).
#[derive(Clone, Copy)]
struct Voice {
    note_id: i32,
    base_freq: f64,
    phases: [f64; 7],
    /// Tuning expression, normalized 0..1 (0.5 = no bend).
    tuning: f64,
    /// Velocity gain, 0..1 (mapped so even the softest note is audible).
    vel: f64,
    amp_env: Adsr,
    filter_env: Adsr,
    /// True from note-on to note-off; the voice lingers until the amp env reaches silence.
    gate: bool,
    /// One-pole low-pass states (L/R) backing the per-voice high-pass at the fundamental
    /// (super saw).
    hp_lp: [f64; 2],
    /// Per-voice 4-pole ladder filter states, one set per channel (the filter envelope needs
    /// a filter per note — a shared one can't pluck; stereo oscillators need one per side).
    flt: [[f64; 4]; 2],
    /// Samples since note-on; clocks the (deterministic) analogue drift LFOs.
    age: f64,
    /// Which timbre this voice plays: 0 = live params (ch 1), 1 = the Ch2 preset part.
    part: usize,
    /// Original note number and channel, for releasing id-less (noteId -1) note-offs.
    pitch: i16,
    channel: i16,
}

struct SynthState {
    sample_rate: f64,
    voices: Vec<Voice>,
    /// Every parameter's normalized value (the DSP derives its block constants from this,
    /// and `getState` serializes it directly).
    params: [f64; PARAM_COUNT as usize],
}

/// An input event with its routing already decoded (offsets are handled by the caller).
enum ParsedEvent {
    NoteOn {
        note_id: i32,
        pitch: i16,
        velocity: f32,
        /// MIDI channel index; channel 1 (index 1) plays the second timbre.
        channel: i16,
    },
    NoteOff {
        note_id: i32,
        pitch: i16,
        channel: i16,
    },
    /// CC 123 (all notes off) / CC 120 (all sound off).
    AllNotesOff,
    Tuning {
        note_id: i32,
        value: f64,
    },
}

impl SynthState {
    /// Route one normalized parameter value; a program change fans out into its preset.
    fn set_param(&mut self, id: u32, value: f64) {
        let v = value.clamp(0.0, 1.0);
        if let Some(slot) = self.params.get_mut(id as usize) {
            *slot = v;
        }
        if id == PROGRAM_PARAM_ID {
            for (slot, pv) in self.params.iter_mut().zip(preset_values(v).iter()) {
                *slot = *pv;
            }
        }
    }

    fn apply_event(&mut self, ev: &ParsedEvent) {
        match *ev {
            ParsedEvent::NoteOn {
                note_id,
                pitch,
                velocity,
                channel,
            } => {
                // The JP-8000's oscillators free-run, so note-on phase is effectively random.
                // A golden-ratio scramble sounds just as decorrelated but stays deterministic
                // for tests; index 0 starts at 0 so sine/saw is phase-exact.
                let mut phases = [0.0; 7];
                for (i, p) in phases.iter_mut().enumerate().skip(1) {
                    *p = (i as f64 * 0.618_033_988_75).fract() * std::f64::consts::TAU;
                }
                // Map velocity so even the softest note stays clearly audible.
                let vel = 0.25 + 0.75 * (velocity as f64).clamp(0.0, 1.0);
                self.voices.push(Voice {
                    note_id,
                    base_freq: note_freq(pitch as f64),
                    phases,
                    tuning: 0.5,
                    vel,
                    amp_env: Adsr::new(),
                    filter_env: Adsr::new(),
                    gate: true,
                    hp_lp: [0.0; 2],
                    flt: [[0.0; 4]; 2],
                    age: 0.0,
                    part: usize::from(channel == 1),
                    pitch,
                    channel,
                });
            }
            ParsedEvent::NoteOff {
                note_id,
                pitch,
                channel,
            } => {
                // VST3 semantics: a note-off with a real id releases that exact note; id -1
                // means the host doesn't track ids, so match by pitch + channel instead.
                // (Treating -1 as all-notes-off — as this synth once did — makes every
                // host-side note-off silence unrelated voices, e.g. a sustained pad part.)
                for v in self.voices.iter_mut() {
                    let matched = if note_id >= 0 {
                        v.note_id == note_id
                    } else {
                        v.pitch == pitch && v.channel == channel
                    };
                    if matched {
                        v.gate = false;
                    }
                }
            }
            ParsedEvent::AllNotesOff => {
                for v in self.voices.iter_mut() {
                    v.gate = false;
                }
            }
            ParsedEvent::Tuning { note_id, value } => {
                for v in self.voices.iter_mut() {
                    if v.note_id == note_id {
                        v.tuning = value;
                    }
                }
            }
        }
    }
}

/// The 14 synth-parameter values for a normalized program-change value.
fn preset_values(normalized: f64) -> [f64; 14] {
    let last = PRESETS.len() - 1;
    let idx = ((normalized.clamp(0.0, 1.0) * last as f64).round() as usize).min(last);
    PRESETS[idx].1
}

/// Everything the render loop needs that is constant across one process block.
struct BlockParams {
    sr: f64,
    /// 0 = sine, 1 = saw, 2 = super saw.
    mode: u8,
    center_gain: f64,
    side_gain: f64,
    detune: f64,
    supersaw_norm: f64,
    /// Per-oscillator equal-power (L, R) gains.
    pan: [[f64; 2]; 7],
    amp_coefs: AdsrCoefs,
    filter_coefs: AdsrCoefs,
    cutoff: f64,
    fenv_amt: f64,
    k_res: f64,
    res_makeup: f64,
    /// Part output level (1.0 for the main part; `Ch2 Level` for the second timbre).
    level: f64,
}

impl BlockParams {
    /// Derive one part's block constants from its 14 synth-parameter values (indexed by the
    /// parameter ids — same layout as `PRESETS` entries and `PARAM_NAMES`).
    fn from_values(p: &[f64; 14], level: f64, sr: f64) -> Self {
        let at = |id: u32| p[id as usize];
        let waveform = at(WAVEFORM_PARAM_ID);
        let mode = if waveform < 1.0 / 3.0 {
            0
        } else if waveform < 2.0 / 3.0 {
            1
        } else {
            2
        };
        // Center/side oscillator gains as a function of the Mix knob (Szabo's curves).
        let mix = at(MIX_PARAM_ID);
        let center_gain = -0.55366 * mix + 0.99785;
        let side_gain = -0.73764 * mix * mix + 1.2841 * mix + 0.044372;
        // Per-oscillator equal-power stereo gains from the fixed pan positions.
        let mut pan = [[0.0f64; 2]; 7];
        for (i, g) in pan.iter_mut().enumerate() {
            let theta = (SUPERSAW_PAN[i] + 1.0) * std::f64::consts::FRAC_PI_4;
            *g = [theta.cos(), theta.sin()];
        }
        let amp_adsr = AdsrParams {
            a: at(AMP_ATTACK_PARAM_ID),
            d: at(AMP_DECAY_PARAM_ID),
            s: at(AMP_SUSTAIN_PARAM_ID),
            r: at(AMP_RELEASE_PARAM_ID),
        };
        let filter_adsr = AdsrParams {
            a: at(FILTER_ATTACK_PARAM_ID),
            d: at(FILTER_DECAY_PARAM_ID),
            s: at(FILTER_SUSTAIN_PARAM_ID),
            r: at(FILTER_RELEASE_PARAM_ID),
        };
        let k_res = 3.7 * at(RESONANCE_PARAM_ID);
        BlockParams {
            sr,
            mode,
            center_gain,
            side_gain,
            detune: supersaw_detune_curve(at(DETUNE_PARAM_ID)),
            // Normalize the 7-osc sum so the super saw sits comparable to a single saw.
            supersaw_norm: 1.0 / (center_gain + 6.0 * side_gain),
            pan,
            amp_coefs: AdsrCoefs::new(amp_adsr, sr),
            filter_coefs: AdsrCoefs::new(filter_adsr, sr),
            cutoff: at(CUTOFF_PARAM_ID),
            // Ladder feedback from the resonance knob: 0 = none, 3.7 = screaming (4 = osc).
            fenv_amt: at(FILTER_ENV_AMOUNT_PARAM_ID),
            k_res,
            // The ladder's passband drops as feedback rises; mostly make it up.
            res_makeup: 1.0 + 0.8 * k_res,
            level,
        }
    }
}

/// One tick of the 24 dB/oct zero-delay-feedback ladder (Zavalishin's TPT form — the
/// four-pole cascade the JP-8000/Virus trance pluck lives on; a 12 dB slope leaks too much
/// top end to ever sound snappy). `z` is one channel's four integrator states.
fn ladder_tick(z: &mut [f64; 4], x: f64, g: f64, big_g: f64, k_res: f64) -> f64 {
    let g2 = big_g * big_g;
    // Zero-delay feedback: solve the loop u = x - k·y4 algebraically...
    let fb_sum = (g2 * big_g * z[0] + g2 * z[1] + big_g * z[2] + z[3]) / (1.0 + g);
    let u = (x - k_res * fb_sum) / (1.0 + k_res * g2 * g2);
    // ...then drive it gently (transistor-ish tanh) before the four cascaded one-poles.
    let mut y = (u * 0.9).tanh() / 0.9;
    for zz in z.iter_mut() {
        let vv = (y - *zz) * big_g;
        let stage = vv + *zz;
        *zz = stage + vv;
        y = stage;
    }
    y
}

/// Render all voices additively into the output channels for samples `[start, end)`.
/// Even channels get left, odd channels right.
///
/// # Safety
/// `out` pointers must be valid channel buffers of at least `end` samples.
unsafe fn render_voices(
    voices: &mut [Voice],
    parts: &[BlockParams; 2],
    out: &[*mut f32],
    start: usize,
    end: usize,
) {
    let tau = std::f64::consts::TAU;
    let amp = 0.25_f64;
    let sr = parts[0].sr;

    for v in voices.iter_mut() {
        let bp = &parts[v.part.min(1)];
        // Tuning: normalized 0..1, 0.5 = center; ±1 octave at the extremes.
        let bend_semitones = (v.tuning - 0.5) * 24.0;
        let freq = v.base_freq * 2f64.powf(bend_semitones / 12.0);

        // Per-oscillator phase increments (only index 0 is used for sine/saw). The super saw
        // adds a slow deterministic "analogue drift" (two incommensurate sines per oscillator,
        // ~±1.6 cents) so held notes shimmer like free-running hardware oscillators.
        let mut incs = [0.0f64; 7];
        if bp.mode == 2 {
            let (w1, w2) = (tau * 0.31 / sr, tau * 0.73 / sr);
            for (i, inc) in incs.iter_mut().enumerate() {
                let drift = 1.0
                    + DRIFT_RATIO
                        * ((v.age * w1 + i as f64 * 1.9).sin()
                            + 0.6 * (v.age * w2 + i as f64 * 4.7).sin());
                *inc = tau * (freq * (1.0 + SUPERSAW_DETUNE[i] * bp.detune) * drift) / sr;
            }
        } else {
            incs[0] = tau * freq / sr;
        }
        // The JP-8000 high-passes the stack at the fundamental: the detuned sides beat
        // against each other below it, and that rumble is what the HPF removes (Szabo).
        let hp_alpha = (1.0 - (-tau * freq / sr).exp()).clamp(0.0, 1.0);
        // Cutoff keytracking, in the normalized (exponential) cutoff domain.
        let keytrack = KEYTRACK * (v.base_freq / 261.625_565).log2() * OCTAVE_IN_CUTOFF;

        for s in start..end {
            // Oscillator(s) → a stereo pair.
            let (osc_l, osc_r) = if bp.mode == 2 {
                // Super saw: 7 band-limited saws, center in tune, sides spread by the detune
                // knob and panned across the field (adjacent detunes alternate sides).
                let (mut l, mut r) = (0.0f64, 0.0f64);
                for (i, (phase, &inc)) in v.phases.iter_mut().zip(incs.iter()).enumerate() {
                    let t = *phase / tau;
                    let saw = 2.0 * t - 1.0 - poly_blep(t, inc / tau);
                    let gain = if i == 3 { bp.center_gain } else { bp.side_gain };
                    l += saw * gain * bp.pan[i][0];
                    r += saw * gain * bp.pan[i][1];
                    *phase += inc;
                    if *phase > tau {
                        *phase -= tau;
                    }
                }
                v.hp_lp[0] += hp_alpha * (l - v.hp_lp[0]);
                v.hp_lp[1] += hp_alpha * (r - v.hp_lp[1]);
                (
                    (l - v.hp_lp[0]) * bp.supersaw_norm,
                    (r - v.hp_lp[1]) * bp.supersaw_norm,
                )
            } else {
                // Single centered oscillator (uses phases[0]): sine, or a band-limited saw.
                let t = v.phases[0] / tau;
                let y = if bp.mode == 1 {
                    2.0 * t - 1.0 - poly_blep(t, incs[0] / tau)
                } else {
                    v.phases[0].sin()
                };
                v.phases[0] += incs[0];
                if v.phases[0] > tau {
                    v.phases[0] -= tau;
                }
                (y, y)
            };

            // Per-voice resonant 24 dB low-pass, cutoff pushed by the filter envelope and
            // following the keyboard.
            let fenv = v.filter_env.next(v.gate, &bp.filter_coefs);
            let eff_cutoff = (bp.cutoff + bp.fenv_amt * fenv + keytrack).clamp(0.0, 1.0);
            let fc = (20.0 * 1000f64.powf(eff_cutoff)).min(sr * 0.45); // ~20 Hz .. ~20 kHz
            let g = (std::f64::consts::PI * fc / sr).tan();
            let big_g = g / (1.0 + g);
            let fl = ladder_tick(&mut v.flt[0], osc_l, g, big_g, bp.k_res) * bp.res_makeup;
            let fr = ladder_tick(&mut v.flt[1], osc_r, g, big_g, bp.k_res) * bp.res_makeup;

            let env = v.amp_env.next(v.gate, &bp.amp_coefs) * v.vel * amp * bp.level;
            let (sl, sr_smp) = ((fl * env) as f32, (fr * env) as f32);
            for (ch, &p) in out.iter().enumerate() {
                *p.add(s) += if ch % 2 == 0 { sl } else { sr_smp };
            }
            v.age += 1.0;
        }
    }
}

struct TestSynthProcessor {
    state: Mutex<SynthState>,
    data_exchange_handler: Mutex<Option<vst3::ComPtr<IDataExchangeHandler>>>,
    data_exchange_handler_ptr: AtomicPtr<IDataExchangeHandler>,
    data_exchange_queue: AtomicU32,
    data_exchange_sequence: AtomicU32,
    processor_ptr: AtomicPtr<IAudioProcessor>,
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
                params: PARAM_DEFAULTS, // sine, sustain 1, env amount 0 — deterministic
            }),
            data_exchange_handler: Mutex::new(None),
            data_exchange_handler_ptr: AtomicPtr::new(ptr::null_mut()),
            data_exchange_queue: AtomicU32::new(InvalidDataExchangeQueueID),
            data_exchange_sequence: AtomicU32::new(0),
            processor_ptr: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

/// Write `bytes` to a VST3 stream, returning success only on a complete write.
unsafe fn stream_write_all(stream: *mut IBStream, bytes: &[u8]) -> bool {
    let Some(s) = ComRef::from_raw(stream) else {
        return false;
    };
    let mut written: i32 = 0;
    s.write(
        bytes.as_ptr() as *mut c_void,
        bytes.len() as i32,
        &mut written,
    ) == kResultOk
        && written as usize == bytes.len()
}

/// Read exactly `len` bytes from a VST3 stream.
unsafe fn stream_read_exact(stream: *mut IBStream, len: usize) -> Option<Vec<u8>> {
    let s = ComRef::from_raw(stream)?;
    let mut buf = vec![0u8; len];
    let mut read: i32 = 0;
    if s.read(buf.as_mut_ptr() as *mut c_void, len as i32, &mut read) != kResultOk
        || read as usize != len
    {
        return None;
    }
    Some(buf)
}

fn encode_state(params: &[f64; PARAM_COUNT as usize]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + params.len() * 8);
    out.extend_from_slice(&STATE_MAGIC.to_le_bytes());
    out.extend_from_slice(&(params.len() as u32).to_le_bytes());
    for v in params {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Read + validate a state blob from a stream, returning the stored parameter values.
unsafe fn read_state_params(stream: *mut IBStream) -> Option<Vec<f64>> {
    let header = stream_read_exact(stream, 8)?;
    if u32::from_le_bytes(header[0..4].try_into().ok()?) != STATE_MAGIC {
        return None;
    }
    let count = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
    if count > 1024 {
        return None; // sanity bound; a corrupt count must not trigger a huge allocation
    }
    let body = stream_read_exact(stream, count * 8)?;
    Some(
        body.chunks_exact(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
            .collect(),
    )
}

/// MIDI note number → frequency (A4=69=440 Hz).
fn note_freq(pitch: f64) -> f64 {
    440.0 * 2f64.powf((pitch - 69.0) / 12.0)
}

/// The bus layout both audio classes present: one stereo audio output and one 16-channel
/// event input.
fn synth_bus_count(media_type: MediaType, dir: BusDirection) -> i32 {
    match media_type as MediaTypes {
        MediaTypes_::kAudio => i32::from(dir as BusDirections == BusDirections_::kOutput),
        MediaTypes_::kEvent => i32::from(dir as BusDirections == BusDirections_::kInput),
        _ => 0,
    }
}

/// Describe one of the buses [`synth_bus_count`] advertises.
///
/// # Safety
/// `bus` must point to a writable `BusInfo`.
unsafe fn synth_bus_info(
    media_type: MediaType,
    dir: BusDirection,
    index: i32,
    bus: *mut BusInfo,
) -> tresult {
    if index != 0 || synth_bus_count(media_type, dir) == 0 {
        return kInvalidArgument;
    }
    let is_audio = media_type as MediaTypes == MediaTypes_::kAudio;
    let bus = &mut *bus;
    bus.mediaType = media_type;
    bus.direction = dir;
    bus.channelCount = if is_audio { 2 } else { 16 };
    copy_wstring(if is_audio { "Output" } else { "Event In" }, &mut bus.name);
    bus.busType = BusTypes_::kMain as BusType;
    bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
    kResultOk
}

/// Render one process block: the engine shared by both exported audio classes.
///
/// Parses the input events (keeping their sample offsets), applies queued parameter changes and
/// echoes them back through `outputParameterChanges`, then splits the block at the event offsets
/// so notes start and stop sample-accurately.
///
/// # Safety
/// `data` must be the host's live `ProcessData` for this call, with valid output buffers.
unsafe fn process_synth_block(state: &Mutex<SynthState>, data: &ProcessData) -> tresult {
    let Ok(mut state) = state.lock() else {
        return kResultOk;
    };

    // Parse input events — note on/off (keyed by noteId) and Tuning note-expression —
    // keeping their sample offsets so they can be applied segment-accurately below.
    let mut events: Vec<(usize, ParsedEvent)> = Vec::new();
    if let Some(in_events) = ComRef::from_raw(data.inputEvents) {
        let count = in_events.getEventCount();
        for i in 0..count {
            let mut ev: Event = std::mem::zeroed();
            if in_events.getEvent(i, &mut ev) != kResultOk {
                continue;
            }
            let offset = ev.sampleOffset.max(0) as usize;
            let parsed = match ev.r#type as u32 {
                t if t == Event_::EventTypes_::kNoteOnEvent as u32 => {
                    let n = ev.__field0.noteOn;
                    ParsedEvent::NoteOn {
                        note_id: n.noteId,
                        pitch: n.pitch,
                        velocity: n.velocity,
                        channel: n.channel,
                    }
                }
                t if t == Event_::EventTypes_::kNoteOffEvent as u32 => {
                    let n = ev.__field0.noteOff;
                    ParsedEvent::NoteOff {
                        note_id: n.noteId,
                        pitch: n.pitch,
                        channel: n.channel,
                    }
                }
                t if t == Event_::EventTypes_::kLegacyMIDICCOutEvent as u32 => {
                    let cc = ev.__field0.midiCCOut;
                    match cc.controlNumber {
                        120 | 123 => ParsedEvent::AllNotesOff,
                        _ => continue,
                    }
                }
                t if t == Event_::EventTypes_::kNoteExpressionValueEvent as u32 => {
                    let nx = ev.__field0.noteExpressionValue;
                    if nx.typeId != NoteExpressionTypeIDs_::kTuningTypeID as u32 {
                        continue;
                    }
                    ParsedEvent::Tuning {
                        note_id: nx.noteId,
                        value: nx.value,
                    }
                }
                _ => continue,
            };
            events.push((offset, parsed));
        }
        // Stable sort: same-offset events keep their input order (note-off before a
        // retriggered note-on, etc).
        events.sort_by_key(|(o, _)| *o);
    }

    // Read parameter changes the host queued, routed through `set_param` (which also
    // fans a program change out into its preset). We take the last point of each queue
    // for the block — parameters stay block-granular; events are sample-accurate.
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
            let param_id = queue.getParameterId();
            state.set_param(param_id, value);

            // Echo the processor-applied value through outputParameterChanges. This is a
            // deterministic host-conformance probe: a host that provides but never drains
            // this list silently loses the feedback.
            if let Some(output) = ComRef::from_raw(data.outputParameterChanges) {
                let mut queue_index = -1;
                let output_queue = output.addParameterData(&param_id, &mut queue_index);
                if let Some(output_queue) = ComRef::from_raw(output_queue) {
                    let mut point_index = -1;
                    output_queue.addPoint(offset, value, &mut point_index);
                }
            }
        }
    }

    let num_samples = data.numSamples as usize;
    if data.numOutputs < 1 || num_samples == 0 {
        // No audio to render this call, but the events still count.
        for (_, ev) in &events {
            state.apply_event(ev);
        }
        state
            .voices
            .retain(|v| v.gate || v.amp_env.level > ENV_SILENCE);
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
    // Two timbres: part 0 plays the live parameters (channel 1), part 1 plays the
    // Ch2 Program preset at Ch2 Level (channel 2).
    let part1: [f64; 14] = state.params[0..14].try_into().unwrap_or([0.0; 14]);
    let parts = [
        BlockParams::from_values(&part1, state.params[CH1_LEVEL_PARAM_ID as usize], sr),
        BlockParams::from_values(
            &preset_values(state.params[CH2_PROGRAM_PARAM_ID as usize]),
            state.params[CH2_LEVEL_PARAM_ID as usize],
            sr,
        ),
    ];

    // Split the block at event offsets so notes start/stop sample-accurately: apply
    // everything due at the segment start, render up to the next event (or block end).
    let mut seg_start = 0usize;
    let mut ev_idx = 0usize;
    while seg_start < num_samples {
        while ev_idx < events.len() && events[ev_idx].0 <= seg_start {
            let (_, ev) = &events[ev_idx];
            state.apply_event(ev);
            ev_idx += 1;
        }
        let seg_end = events
            .get(ev_idx)
            .map(|(o, _)| (*o).min(num_samples))
            .unwrap_or(num_samples)
            .max(seg_start + 1);
        render_voices(&mut state.voices, &parts, &out_ptrs, seg_start, seg_end);
        seg_start = seg_end;
    }
    // Anything scheduled at/after the block end (defensive) still takes effect.
    for (_, ev) in &events[ev_idx..] {
        state.apply_event(ev);
    }

    // Drop voices whose release has decayed to silence.
    state
        .voices
        .retain(|v| v.gate || v.amp_env.level > ENV_SILENCE);
    kResultOk
}

/// Render one parameter's value the way the plugin itself would display it.
fn format_param_value(id: u32, v: f64) -> String {
    if id == WAVEFORM_PARAM_ID {
        (if v < 1.0 / 3.0 {
            "Sine"
        } else if v < 2.0 / 3.0 {
            "Saw"
        } else {
            "Super Saw"
        })
        .to_string()
    } else if id == PROGRAM_PARAM_ID || id == CH2_PROGRAM_PARAM_ID {
        let last = PRESETS.len() - 1;
        let idx = ((v.clamp(0.0, 1.0) * last as f64).round() as usize).min(last);
        PRESETS[idx].0.to_string()
    } else if is_time_param(id) {
        let secs = env_time_secs(v);
        if secs < 1.0 {
            format!("{:.0} ms", secs * 1000.0)
        } else {
            format!("{secs:.2} s")
        }
    } else {
        format!("{:.0}%", v * 100.0)
    }
}

/// Fill `info` with a synth parameter's descriptor (ids 0..[`PARAM_COUNT`]).
///
/// # Safety
/// `info` must point to a writable `ParameterInfo`.
unsafe fn write_synth_parameter_info(id: u32, info: *mut ParameterInfo) -> tresult {
    let Some(name) = PARAM_NAMES.get(id as usize) else {
        return kInvalidArgument;
    };
    let info = &mut *info;
    let automate = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
    info.id = id;
    copy_wstring(name, &mut info.title);
    copy_wstring(name, &mut info.shortTitle);
    copy_wstring("", &mut info.units);
    info.defaultNormalizedValue = PARAM_DEFAULTS[id as usize];
    info.unitId = 0;
    if id == WAVEFORM_PARAM_ID {
        copy_wstring("Wave", &mut info.shortTitle);
        info.stepCount = 2; // three discrete values: Sine / Saw / Super Saw
        info.flags = automate | ParameterInfo_::ParameterFlags_::kIsList as i32;
    } else if id == PROGRAM_PARAM_ID {
        copy_wstring("Prog", &mut info.shortTitle);
        info.stepCount = PRESETS.len() as i32 - 1;
        info.flags = automate
            | ParameterInfo_::ParameterFlags_::kIsList as i32
            | ParameterInfo_::ParameterFlags_::kIsProgramChange as i32;
    } else if id == CH2_PROGRAM_PARAM_ID {
        copy_wstring("Ch2Prg", &mut info.shortTitle);
        info.stepCount = PRESETS.len() as i32 - 1;
        info.flags = automate | ParameterInfo_::ParameterFlags_::kIsList as i32;
    } else {
        info.stepCount = 0; // continuous
        info.flags = automate;
    }
    kResultOk
}

/// Fill `info` with an instrumentation parameter's descriptor: read-only, never automatable,
/// and reported as a plain 0..1 value the test decodes with the scales documented above.
///
/// # Safety
/// `info` must point to a writable `ParameterInfo`.
unsafe fn write_probe_parameter_info(id: u32, title: &str, info: *mut ParameterInfo) -> tresult {
    let info = &mut *info;
    info.id = id;
    copy_wstring(title, &mut info.title);
    copy_wstring(title, &mut info.shortTitle);
    copy_wstring("", &mut info.units);
    info.defaultNormalizedValue = 0.0;
    info.unitId = 0;
    info.stepCount = 0;
    info.flags = ParameterInfo_::ParameterFlags_::kIsReadOnly as i32;
    kResultOk
}

impl IPluginBaseTrait for TestSynthProcessor {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        if let Some(context) = ComRef::from_raw(context) {
            if let Some(handler) = context.cast::<IDataExchangeHandler>() {
                self.data_exchange_handler_ptr
                    .store(handler.as_ptr(), Ordering::Release);
                *self
                    .data_exchange_handler
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(handler);
            }
        }
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
        synth_bus_count(media_type, dir)
    }
    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        synth_bus_info(media_type, dir, index, bus)
    }
    unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setActive(&self, state: TBool) -> tresult {
        if state == 0 {
            let queue = self
                .data_exchange_queue
                .swap(InvalidDataExchangeQueueID, Ordering::AcqRel);
            if queue != InvalidDataExchangeQueueID {
                if let Some(handler) = self
                    .data_exchange_handler
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref()
                {
                    let _ = handler.closeQueue(queue);
                }
            }
        }
        kResultOk
    }
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        // Observe the host's stream tagging before consuming the stream: a project load and a
        // preset load carry different `IStreamAttributes` metadata, and the host's safe API has
        // no other way to prove it sets them.
        record_state_stream_context(stream);
        let Some(values) = read_state_params(stream) else {
            return kResultFalse;
        };
        let Ok(mut state) = self.state.lock() else {
            return kResultFalse;
        };
        for (id, v) in values.iter().enumerate().take(PARAM_COUNT as usize) {
            if id as u32 == PROGRAM_PARAM_ID {
                // Restore the program *value* without re-applying its preset — the individual
                // parameters that follow the program in a saved state are authoritative.
                state.params[id] = v.clamp(0.0, 1.0);
            } else {
                state.set_param(id as u32, *v);
            }
        }
        kResultOk
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        let Ok(state) = self.state.lock() else {
            return kResultFalse;
        };
        let blob = encode_state(&state.params);
        if stream_write_all(stream, &blob) {
            kResultOk
        } else {
            kResultFalse
        }
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
        // Fixed, nonzero advertisement (TestSynth has no real look-ahead): lets host tests
        // assert a real getLatencySamples round trip — a dropped call would read 0.
        TEST_LATENCY_SAMPLES
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if let Ok(mut s) = self.state.lock() {
            s.sample_rate = (*setup).sampleRate;
            s.voices.clear();
        }
        if self.data_exchange_queue.load(Ordering::Acquire) == InvalidDataExchangeQueueID {
            let handler = self
                .data_exchange_handler
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(handler) = handler.as_ref() {
                let mut queue = InvalidDataExchangeQueueID;
                if handler.openQueue(
                    self.processor_ptr.load(Ordering::Acquire),
                    8,
                    4,
                    4,
                    0x5453_0001,
                    &mut queue,
                ) == kResultTrue
                {
                    self.data_exchange_queue.store(queue, Ordering::Release);
                }
            }
        }
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = &*data;
        let queue = self.data_exchange_queue.load(Ordering::Acquire);
        let handler_ptr = self.data_exchange_handler_ptr.load(Ordering::Acquire);
        if queue != InvalidDataExchangeQueueID {
            if let Some(handler) = ComRef::from_raw(handler_ptr) {
                let mut block: vst3::Steinberg::Vst::DataExchangeBlock = std::mem::zeroed();
                if handler.lockBlock(queue, &mut block) == kResultTrue
                    && !block.data.is_null()
                    && block.size >= 8
                {
                    let sequence = self.data_exchange_sequence.fetch_add(1, Ordering::Relaxed);
                    ptr::copy_nonoverlapping(b"DXB1".as_ptr(), block.data.cast::<u8>(), 4);
                    ptr::copy_nonoverlapping(
                        sequence.to_le_bytes().as_ptr(),
                        block.data.cast::<u8>().add(4),
                        4,
                    );
                    let _ = handler.freeBlock(queue, block.blockID, 1);
                }
            }
        }
        process_synth_block(&self.state, data)
    }
    unsafe fn getTailSamples(&self) -> u32 {
        // Fixed, nonzero advertisement, same rationale as getLatencySamples.
        TEST_TAIL_SAMPLES
    }
}

impl IProcessContextRequirementsTrait for TestSynthProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

/// A minimal but protocol-complete `IPlugView`.
///
/// It draws nothing — `attached` just records the parent and returns success, which is all the
/// VST3 embedding contract actually requires of a view. What it *does* do is exercise every
/// negotiation a host has to get right: platform-type matching, `getSize`/`onSize`,
/// `checkSizeConstraint` clamping, content scaling, and one host-side resize request. That makes
/// the host's editor path machine-checkable on all three platforms without any native widgets.
struct TestPlugView {
    probe: Arc<EditorProbe>,
    /// The host's frame, from `setFrame`. VST3 requires the host to set it before `attached`.
    frame: Mutex<Option<vst3::ComPtr<IPlugFrame>>>,
    /// Current view size, reported by `getSize` and updated by `onSize`.
    size: Mutex<(i32, i32)>,
    /// This object's own `IPlugView` pointer. `IPlugFrame::resizeView` takes the view it is
    /// about, and a COM object cannot otherwise name itself. Deliberately not a `ComPtr`: an
    /// owning self-reference would be a refcount cycle the view could never escape.
    self_view: AtomicPtr<IPlugView>,
    /// The post-attach `resizeView` fires exactly once, however often the host re-attaches.
    self_resize_done: AtomicBool,
}

impl Class for TestPlugView {
    type Interfaces = (IPlugView, IPlugViewContentScaleSupport);
}

impl TestPlugView {
    fn new(probe: Arc<EditorProbe>) -> Self {
        Self {
            probe,
            frame: Mutex::new(None),
            size: Mutex::new(EDITOR_SIZE),
            self_view: AtomicPtr::new(ptr::null_mut()),
            self_resize_done: AtomicBool::new(false),
        }
    }

    /// Ask the host — once — to resize the window this view sits in.
    ///
    /// This is the half of the resize protocol only a plugin can start, and the host answers it
    /// with `onSize` in the same callstack before resizing its container. Both halves are then
    /// visible from outside: the request through the host's own resize plumbing, the answer
    /// through this view's `onSize` instrumentation.
    fn request_host_resize(&self) {
        if self.self_resize_done.swap(true, Ordering::AcqRel) {
            return;
        }
        // Clone the frame out and drop the lock first: the host calls straight back into
        // `onSize`, and on some paths into `setFrame`, from inside `resizeView`.
        let frame = self
            .frame
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let Some(frame) = frame else { return };
        let view = self.self_view.load(Ordering::Acquire);
        if view.is_null() {
            return;
        }
        let (width, height) = EDITOR_SELF_RESIZE;
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        // SAFETY: `frame` is the host frame handed to `setFrame`, and `view` is this object's
        // own interface pointer, which cannot outlive `self`.
        unsafe { frame.resizeView(view, &mut rect) };
    }
}

impl IPlugViewTrait for TestPlugView {
    unsafe fn isPlatformTypeSupported(&self, r#type: FIDString) -> tresult {
        if platform_type_matches(r#type) {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn attached(&self, parent: *mut c_void, r#type: FIDString) -> tresult {
        if parent.is_null() || !platform_type_matches(r#type) {
            return kInvalidArgument;
        }
        self.probe.attached.store(true, Ordering::Release);
        self.request_host_resize();
        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        self.probe.attached.store(false, Ordering::Release);
        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _key: char16, _key_code: i16, _modifiers: i16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _key: char16, _key_code: i16, _modifiers: i16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        let Some(size) = size.as_mut() else {
            return kInvalidArgument;
        };
        let (width, height) = *self.size.lock().unwrap_or_else(|p| p.into_inner());
        size.left = 0;
        size.top = 0;
        size.right = width;
        size.bottom = height;
        kResultOk
    }

    unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
        let Some(rect) = new_size.as_ref() else {
            return kInvalidArgument;
        };
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        if width <= 0 || height <= 0 {
            return kInvalidArgument;
        }
        *self.size.lock().unwrap_or_else(|p| p.into_inner()) = (width, height);
        self.probe.record_size(width, height);
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        let frame = ComRef::from_raw(frame).map(|frame| frame.to_com_ptr());
        *self.frame.lock().unwrap_or_else(|p| p.into_inner()) = frame;
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultTrue
    }

    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        let Some(rect) = rect.as_mut() else {
            return kInvalidArgument;
        };
        let width = (rect.right - rect.left).clamp(EDITOR_MIN_SIZE.0, EDITOR_MAX_SIZE.0);
        let height = (rect.bottom - rect.top).clamp(EDITOR_MIN_SIZE.1, EDITOR_MAX_SIZE.1);
        rect.right = rect.left + width;
        rect.bottom = rect.top + height;
        kResultTrue
    }
}

impl IPlugViewContentScaleSupportTrait for TestPlugView {
    unsafe fn setContentScaleFactor(&self, factor: f32) -> tresult {
        if !factor.is_finite() || factor <= 0.0 {
            return kInvalidArgument;
        }
        self.probe.scale_x256.store(
            (f64::from(factor) * 256.0).round() as u32,
            Ordering::Release,
        );
        kResultOk
    }
}

struct TestSynthController {
    values: Mutex<[f64; PARAM_COUNT as usize]>,
    /// Deliberately controller-only persistence probe. Component state does not contain this.
    edit_revision: Mutex<u32>,
    /// What this controller's editor view has seen, republished as read-only parameters.
    editor: Arc<EditorProbe>,
}

impl Class for TestSynthController {
    type Interfaces = (
        IEditController,
        INoteExpressionController,
        IUnitInfo,
        IMidiMapping,
        IRemapParamID,
        IDataExchangeReceiver,
    );
}

impl IDataExchangeReceiverTrait for TestSynthController {
    unsafe fn queueOpened(
        &self,
        _user_context_id: u32,
        _block_size: u32,
        dispatch_on_background_thread: *mut TBool,
    ) {
        if !dispatch_on_background_thread.is_null() {
            *dispatch_on_background_thread = 0;
        }
    }

    unsafe fn queueClosed(&self, _user_context_id: u32) {}

    unsafe fn onDataExchangeBlocksReceived(
        &self,
        _user_context_id: u32,
        _num_blocks: u32,
        _blocks: *mut vst3::Steinberg::Vst::DataExchangeBlock,
        _on_background_thread: TBool,
    ) {
    }
}

impl TestSynthController {
    const CID: TUID = uid(0x54455354, 0x53594E54, 0x4354524C, 0x00000001);

    fn new() -> Self {
        Self {
            values: Mutex::new(PARAM_DEFAULTS),
            edit_revision: Mutex::new(0),
            editor: Arc::new(EditorProbe::default()),
        }
    }
}

/// The read-only instrumentation parameters the dual synth's controller publishes, in the order
/// `getParameterInfo` reports them (right after the [`PARAM_COUNT`] synth parameters).
const PROBE_PARAMS: [(u32, &str); 6] = [
    (EDITOR_ATTACHED_PARAM_ID, "Editor Attached"),
    (EDITOR_WIDTH_PARAM_ID, "Editor Width"),
    (EDITOR_HEIGHT_PARAM_ID, "Editor Height"),
    (EDITOR_SCALE_PARAM_ID, "Editor Scale"),
    (STATE_TYPE_PARAM_ID, "State Type Seen"),
    (STATE_PATH_PARAM_ID, "State Path Seen"),
];

/// Total parameter count the dual synth's controller reports.
const CONTROLLER_PARAM_COUNT: i32 = PARAM_COUNT + PROBE_PARAMS.len() as i32;

impl IPluginBaseTrait for TestSynthController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IRemapParamIDTrait for TestSynthController {
    unsafe fn getCompatibleParamID(
        &self,
        plugin_to_replace_uid: *const TUID,
        old_param_id: u32,
        new_param_id: *mut u32,
    ) -> tresult {
        if plugin_to_replace_uid.is_null() || new_param_id.is_null() {
            return kInvalidArgument;
        }
        if *plugin_to_replace_uid == REPLACED_PLUGIN_UID && old_param_id == REPLACED_CUTOFF_PARAM_ID
        {
            *new_param_id = CUTOFF_PARAM_ID;
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

impl IEditControllerTrait for TestSynthController {
    unsafe fn setComponentState(&self, stream: *mut IBStream) -> tresult {
        // The host hands us the processor's state so the UI side stays in sync.
        let Some(restored) = read_state_params(stream) else {
            return kResultFalse;
        };
        let mut values = self.values.lock().unwrap_or_else(|p| p.into_inner());
        for (slot, v) in values.iter_mut().zip(restored.iter()) {
            *slot = v.clamp(0.0, 1.0);
        }
        kResultOk
    }
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        let Some(bytes) = stream_read_exact(stream, 8) else {
            return kResultFalse;
        };
        if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != CONTROLLER_STATE_MAGIC {
            return kResultFalse;
        }
        let revision = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        *self
            .edit_revision
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = revision;
        kResultOk
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        let revision = *self
            .edit_revision
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut bytes = Vec::with_capacity(8);
        bytes.extend_from_slice(&CONTROLLER_STATE_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&revision.to_le_bytes());
        if stream_write_all(stream, &bytes) {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getParameterCount(&self) -> i32 {
        CONTROLLER_PARAM_COUNT
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if (0..PARAM_COUNT).contains(&index) {
            // Synth parameter ids are contiguous and equal to the index.
            return write_synth_parameter_info(index as u32, info);
        }
        match PROBE_PARAMS.get((index - PARAM_COUNT) as usize) {
            Some((id, title)) if index >= PARAM_COUNT => {
                write_probe_parameter_info(*id, title, info)
            }
            _ => kInvalidArgument,
        }
    }
    unsafe fn getParamStringByValue(&self, id: u32, v: f64, s: *mut String128) -> tresult {
        let text = match id {
            id if id < PARAM_COUNT as u32 => format_param_value(id, v),
            EDITOR_ATTACHED_PARAM_ID => {
                (if v >= 0.5 { "attached" } else { "detached" }).to_string()
            }
            EDITOR_WIDTH_PARAM_ID | EDITOR_HEIGHT_PARAM_ID => {
                format!("{} px", (v * EDITOR_SIZE_SCALE).round())
            }
            EDITOR_SCALE_PARAM_ID => format!("{:.2}x", v * EDITOR_SCALE_SCALE),
            STATE_TYPE_PARAM_ID => match (v * STATE_PROBE_SCALE).round() as u32 {
                state_type_seen::DEFAULT => "Default".to_string(),
                state_type_seen::PROJECT => "Project".to_string(),
                state_type_seen::TRACK_PRESET => "TrackPreset".to_string(),
                _ => "none".to_string(),
            },
            STATE_PATH_PARAM_ID => format!("0b{:02b}", (v * STATE_PROBE_SCALE).round() as u32),
            _ => return kNotImplemented,
        };
        copy_wstring(&text, &mut *s);
        kResultOk
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
        if let Some(value) = self.editor.parameter(id) {
            return value;
        }
        match id {
            STATE_TYPE_PARAM_ID => {
                return f64::from(OBSERVED_STATE_TYPE.load(Ordering::Acquire)) / STATE_PROBE_SCALE
            }
            STATE_PATH_PARAM_ID => {
                return f64::from(OBSERVED_STATE_PATH.load(Ordering::Acquire)) / STATE_PROBE_SCALE
            }
            _ => {}
        }
        let values = self.values.lock().unwrap_or_else(|p| p.into_inner());
        values.get(id as usize).copied().unwrap_or(0.0)
    }
    unsafe fn setParamNormalized(&self, id: u32, v: f64) -> tresult {
        // The instrumentation parameters are read-only: a host that writes one gets told so,
        // and — importantly — it does not bump the controller-only edit revision.
        if id >= PROBE_PARAMS[0].0 {
            return kResultFalse;
        }
        let mut values = self.values.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = values.get_mut(id as usize) {
            *slot = v;
        }
        // A program change fans out into its preset so the UI reflects the loaded sound.
        if id == PROGRAM_PARAM_ID {
            for (slot, pv) in values.iter_mut().zip(preset_values(v).iter()) {
                *slot = *pv;
            }
        }
        drop(values);
        let mut revision = self
            .edit_revision
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *revision = revision.wrapping_add(1);
        kResultOk
    }
    unsafe fn setComponentHandler(&self, _h: *mut IComponentHandler) -> tresult {
        kResultOk
    }
    unsafe fn createView(&self, name: *const c_char) -> *mut IPlugView {
        if !name.is_null() && CStr::from_ptr(name) != c"editor" {
            return ptr::null_mut();
        }
        let wrapper = ComWrapper::new(TestPlugView::new(self.editor.clone()));
        let Some(self_ptr) = wrapper.as_com_ref::<IPlugView>().map(|view| view.as_ptr()) else {
            return ptr::null_mut();
        };
        wrapper.self_view.store(self_ptr, Ordering::Release);
        wrapper
            .to_com_ptr::<IPlugView>()
            .map_or(ptr::null_mut(), |view| view.into_raw())
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

impl IUnitInfoTrait for TestSynthController {
    unsafe fn getUnitCount(&self) -> i32 {
        1
    }
    unsafe fn getUnitInfo(&self, unit_index: i32, info: *mut UnitInfo) -> tresult {
        if unit_index != 0 {
            return kInvalidArgument;
        }
        let info = &mut *info;
        info.id = 0; // kRootUnitId
        info.parentUnitId = -1; // kNoParentUnitId
        copy_wstring("Root", &mut info.name);
        info.programListId = PROGRAM_LIST_ID;
        kResultOk
    }
    unsafe fn getProgramListCount(&self) -> i32 {
        1
    }
    unsafe fn getProgramListInfo(&self, list_index: i32, info: *mut ProgramListInfo) -> tresult {
        if list_index != 0 {
            return kInvalidArgument;
        }
        let info = &mut *info;
        info.id = PROGRAM_LIST_ID;
        copy_wstring("Factory", &mut info.name);
        info.programCount = PRESETS.len() as i32;
        kResultOk
    }
    unsafe fn getProgramName(&self, list_id: i32, index: i32, name: *mut String128) -> tresult {
        if list_id != PROGRAM_LIST_ID || !(0..PRESETS.len() as i32).contains(&index) {
            return kInvalidArgument;
        }
        copy_wstring(PRESETS[index as usize].0, &mut *name);
        kResultOk
    }
    unsafe fn getProgramInfo(
        &self,
        _list_id: i32,
        _index: i32,
        _attribute_id: *const c_char,
        _value: *mut String128,
    ) -> tresult {
        kNotImplemented
    }
    unsafe fn hasProgramPitchNames(&self, _list_id: i32, _index: i32) -> tresult {
        kResultFalse
    }
    unsafe fn getProgramPitchName(
        &self,
        _list_id: i32,
        _index: i32,
        _pitch: i16,
        _name: *mut String128,
    ) -> tresult {
        kNotImplemented
    }
    unsafe fn getSelectedUnit(&self) -> UnitID {
        0
    }
    unsafe fn selectUnit(&self, _unit_id: UnitID) -> tresult {
        kResultOk
    }
    unsafe fn getUnitByBus(
        &self,
        _media_type: MediaType,
        _dir: BusDirection,
        _bus_index: i32,
        _channel: i32,
        unit_id: *mut UnitID,
    ) -> tresult {
        *unit_id = 0;
        kResultOk
    }
    unsafe fn setUnitProgramData(
        &self,
        _list_or_unit_id: i32,
        _program_index: i32,
        _data: *mut IBStream,
    ) -> tresult {
        kNotImplemented
    }
}

impl IMidiMappingTrait for TestSynthController {
    /// Standard MIDI CCs → parameters: mod wheel drives the filter-env pluck depth, plus the
    /// GM2 sound controllers (71 timbre, 72 release, 73 attack, 74 brightness).
    unsafe fn getMidiControllerAssignment(
        &self,
        bus_index: i32,
        _channel: i16,
        midi_cc: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        if bus_index != 0 {
            return kResultFalse;
        }
        let mapped = match midi_cc as u32 {
            1 => FILTER_ENV_AMOUNT_PARAM_ID,
            71 => RESONANCE_PARAM_ID,
            72 => AMP_RELEASE_PARAM_ID,
            73 => AMP_ATTACK_PARAM_ID,
            74 => CUTOFF_PARAM_ID,
            x if x == ControllerNumbers_::kAfterTouch as u32 => FILTER_SUSTAIN_PARAM_ID,
            x if x == ControllerNumbers_::kPitchBend as u32 => MIX_PARAM_ID,
            _ => return kResultFalse,
        };
        *id = mapped;
        kResultOk
    }
}

/// How many parameters the single-component class exposes: the first five synth parameters
/// plus its state-apply counter.
const SINGLE_SYNTH_PARAM_COUNT: i32 = 5;
const SINGLE_PARAM_COUNT: i32 = SINGLE_SYNTH_PARAM_COUNT + 1;

/// The factory's second audio class: **one object** that is both the component and the edit
/// controller.
///
/// Most plugins split those in two (as [`TestSynthProcessor`] / [`TestSynthController`] do), but
/// the single-object form is legal VST3 and takes a different path through a host: there is one
/// state stream instead of two, `getControllerClassId` names nothing, and a host that also
/// pushes the component stream through `setComponentState` would apply the same state twice.
/// [`STATE_APPLY_PARAM_ID`] counts every such application so that last part is observable.
///
/// It reuses this file's DSP, parameter ids and state format wholesale; it only exposes a
/// smaller parameter set and none of the optional interfaces (no units, note expression, MIDI
/// mapping or editor).
struct TestSynthSingle {
    state: Mutex<SynthState>,
    /// Combined `IComponent::setState` + `IEditController::setComponentState` count.
    state_applies: AtomicU32,
}

impl Class for TestSynthSingle {
    type Interfaces = (IComponent, IAudioProcessor, IEditController);
}

impl TestSynthSingle {
    const CID: TUID = uid(0x54455354, 0x53594E54, 0x53494E47, 0x00000001);

    fn new() -> Self {
        Self {
            state: Mutex::new(SynthState {
                sample_rate: 48_000.0,
                voices: Vec::new(),
                params: PARAM_DEFAULTS,
            }),
            state_applies: AtomicU32::new(0),
        }
    }

    /// Apply a component state blob, counting the application.
    ///
    /// # Safety
    /// `stream` must be null or a live `IBStream`.
    unsafe fn apply_component_state(&self, stream: *mut IBStream) -> tresult {
        record_state_stream_context(stream);
        let Some(values) = read_state_params(stream) else {
            return kResultFalse;
        };
        let Ok(mut state) = self.state.lock() else {
            return kResultFalse;
        };
        for (id, v) in values.iter().enumerate().take(PARAM_COUNT as usize) {
            if id as u32 == PROGRAM_PARAM_ID {
                state.params[id] = v.clamp(0.0, 1.0);
            } else {
                state.set_param(id as u32, *v);
            }
        }
        self.state_applies.fetch_add(1, Ordering::AcqRel);
        kResultOk
    }
}

impl IPluginBaseTrait for TestSynthSingle {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for TestSynthSingle {
    unsafe fn getControllerClassId(&self, _class_id: *mut TUID) -> tresult {
        // Single-component: there is no separate controller class to name.
        kNotImplemented
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media_type: MediaType, dir: BusDirection) -> i32 {
        synth_bus_count(media_type, dir)
    }
    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        synth_bus_info(media_type, dir, index, bus)
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
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        self.apply_component_state(stream)
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        let Ok(state) = self.state.lock() else {
            return kResultFalse;
        };
        let blob = encode_state(&state.params);
        if stream_write_all(stream, &blob) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IAudioProcessorTrait for TestSynthSingle {
    unsafe fn setBusArrangements(
        &self,
        _inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins != 0 || num_outs != 1 || *outputs != SpeakerArr::kStereo {
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
        if let Ok(mut state) = self.state.lock() {
            state.sample_rate = (*setup).sampleRate;
            state.voices.clear();
        }
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        process_synth_block(&self.state, &*data)
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IEditControllerTrait for TestSynthSingle {
    unsafe fn setComponentState(&self, stream: *mut IBStream) -> tresult {
        // A well-behaved host does not call this on a single-component plugin — the component
        // it would be syncing *is* this object. Honoring it anyway (and counting it) is what
        // makes a host that double-applies state visible from the outside.
        self.apply_component_state(stream)
    }
    unsafe fn setState(&self, _stream: *mut IBStream) -> tresult {
        // No controller-only state: everything lives in the component stream.
        kNotImplemented
    }
    unsafe fn getState(&self, _stream: *mut IBStream) -> tresult {
        kNotImplemented
    }
    unsafe fn getParameterCount(&self) -> i32 {
        SINGLE_PARAM_COUNT
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        match index {
            0..SINGLE_SYNTH_PARAM_COUNT => write_synth_parameter_info(index as u32, info),
            i if i == SINGLE_SYNTH_PARAM_COUNT => {
                write_probe_parameter_info(STATE_APPLY_PARAM_ID, "State Applies", info)
            }
            _ => kInvalidArgument,
        }
    }
    unsafe fn getParamStringByValue(&self, id: u32, v: f64, s: *mut String128) -> tresult {
        let text = match id {
            id if id < SINGLE_SYNTH_PARAM_COUNT as u32 => format_param_value(id, v),
            STATE_APPLY_PARAM_ID => format!("{}", (v * STATE_APPLY_SCALE).round()),
            _ => return kNotImplemented,
        };
        copy_wstring(&text, &mut *s);
        kResultOk
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
        if id == STATE_APPLY_PARAM_ID {
            return (f64::from(self.state_applies.load(Ordering::Acquire)) / STATE_APPLY_SCALE)
                .clamp(0.0, 1.0);
        }
        if id >= SINGLE_SYNTH_PARAM_COUNT as u32 {
            return 0.0;
        }
        self.state
            .lock()
            .map(|state| state.params[id as usize])
            .unwrap_or(0.0)
    }
    unsafe fn setParamNormalized(&self, id: u32, v: f64) -> tresult {
        if id >= SINGLE_SYNTH_PARAM_COUNT as u32 {
            return kResultFalse;
        }
        let Ok(mut state) = self.state.lock() else {
            return kResultFalse;
        };
        state.set_param(id, v);
        kResultOk
    }
    unsafe fn setComponentHandler(&self, _h: *mut IComponentHandler) -> tresult {
        kResultOk
    }
    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        ptr::null_mut()
    }
}

struct TestSynthCompatibility;

impl TestSynthCompatibility {
    const CID: TUID = uid(0x54455354, 0x53594E54, 0x434F4D50, 0x00000001);
}

impl Class for TestSynthCompatibility {
    type Interfaces = (IPluginCompatibility,);
}

impl IPluginCompatibilityTrait for TestSynthCompatibility {
    unsafe fn getCompatibilityJSON(&self, stream: *mut IBStream) -> tresult {
        // Array form is required by IPluginCompatibility. Comments and trailing commas prove
        // the host takes the same bounded JSON5 path as moduleinfo.json.
        let json = br#"[
            {
                "New": "5445535453594E5450524F4300000001",
                "Old": [
                    "4F4C44504C5547494E43494400000001",
                ],
            },
        ]"#;
        if stream_write_all(stream, json) {
            kResultTrue
        } else {
            kResultFalse
        }
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
        4
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
            2 => {
                info.cid = TestSynthCompatibility::CID;
                info.cardinality = 1;
                copy_cstring("Plugin Compatibility Class", &mut info.category);
                copy_cstring("TestSynth Compatibility", &mut info.name);
                kResultOk
            }
            // The single-component variant deliberately comes last: a host that asks for no
            // particular class must still get the dual-object synth at index 0.
            3 => {
                info.cid = TestSynthSingle::CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
                copy_cstring("Audio Module Class", &mut info.category);
                copy_cstring(SINGLE_PLUGIN_NAME, &mut info.name);
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
            TestSynthProcessor::CID => {
                let wrapper = ComWrapper::new(TestSynthProcessor::new());
                wrapper.processor_ptr.store(
                    wrapper.as_com_ref::<IAudioProcessor>().unwrap().as_ptr(),
                    Ordering::Release,
                );
                Some(wrapper.to_com_ptr::<FUnknown>().unwrap())
            }
            TestSynthController::CID => Some(
                ComWrapper::new(TestSynthController::new())
                    .to_com_ptr::<FUnknown>()
                    .unwrap(),
            ),
            TestSynthSingle::CID => Some(
                ComWrapper::new(TestSynthSingle::new())
                    .to_com_ptr::<FUnknown>()
                    .unwrap(),
            ),
            TestSynthCompatibility::CID => {
                // This class intentionally refuses a generic FUnknown request: the host must
                // instantiate it with the exact IPluginCompatibility IID.
                if *(iid as *const TUID) != IPluginCompatibility_iid {
                    return kNoInterface;
                }
                Some(
                    ComWrapper::new(TestSynthCompatibility)
                        .to_com_ptr::<FUnknown>()
                        .unwrap(),
                )
            }
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

// Linux and Android share the VST3 module convention (ModuleEntry/ModuleExit). Gating these
// in for Android too lets the plugin cross-compile and load on-device (verified on arm64-v8a).
#[cfg(any(target_os = "linux", target_os = "android"))]
#[no_mangle]
extern "system" fn ModuleEntry(handle: *mut c_void) -> bool {
    // On Linux, refuse to load unless the host passed the handle `dlopen` returned for this
    // module, as the SDK's `bool ModuleEntry (void* sharedLibraryHandle)` does: a host that
    // calls ModuleEntry without it then fails here rather than in a plugin that uses it.
    #[cfg(target_os = "linux")]
    let accepted = own_dlopen_handle() == Some(handle);
    #[cfg(target_os = "android")]
    let accepted = {
        let _ = handle;
        true
    };
    accepted
}

/// The handle `dlopen` returns for this module. Compared, never dereferenced, so a wrong value
/// from the host cannot crash the check.
#[cfg(target_os = "linux")]
fn own_dlopen_handle() -> Option<*mut c_void> {
    use std::ffi::c_char;
    #[repr(C)]
    struct DlInfo {
        dli_fname: *const c_char,
        dli_fbase: *mut c_void,
        dli_sname: *const c_char,
        dli_saddr: *mut c_void,
    }
    const RTLD_LAZY: i32 = 0x0001;
    const RTLD_NOLOAD: i32 = 0x0004;
    #[link(name = "dl")]
    extern "C" {
        fn dladdr(address: *const c_void, info: *mut DlInfo) -> i32;
        fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> i32;
    }
    unsafe {
        let mut info = std::mem::zeroed::<DlInfo>();
        if dladdr(ModuleEntry as *const c_void, &mut info) == 0 || info.dli_fname.is_null() {
            return None;
        }
        // RTLD_NOLOAD finds the module already loaded; it still counts a reference.
        let handle = dlopen(info.dli_fname, RTLD_LAZY | RTLD_NOLOAD);
        if handle.is_null() {
            return None;
        }
        dlclose(handle);
        Some(handle)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[no_mangle]
extern "system" fn ModuleExit() -> bool {
    true
}
