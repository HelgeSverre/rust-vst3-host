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
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
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
        0
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if let Ok(mut s) = self.state.lock() {
            s.sample_rate = (*setup).sampleRate;
            s.voices.clear();
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
                state.set_param(queue.getParameterId(), value);
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
    values: Mutex<[f64; PARAM_COUNT as usize]>,
}

impl Class for TestSynthController {
    type Interfaces = (
        IEditController,
        INoteExpressionController,
        IUnitInfo,
        IMidiMapping,
    );
}

impl TestSynthController {
    const CID: TUID = uid(0x54455354, 0x53594E54, 0x4354524C, 0x00000001);

    fn new() -> Self {
        Self {
            values: Mutex::new(PARAM_DEFAULTS),
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
    unsafe fn setState(&self, _s: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _s: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> i32 {
        PARAM_COUNT
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if !(0..PARAM_COUNT).contains(&index) {
            return kInvalidArgument;
        }
        let id = index as u32; // parameter ids are contiguous and equal to the index
        let info = &mut *info;
        let automate = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
        info.id = id;
        copy_wstring(PARAM_NAMES[index as usize], &mut info.title);
        copy_wstring(PARAM_NAMES[index as usize], &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.defaultNormalizedValue = PARAM_DEFAULTS[index as usize];
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
    unsafe fn getParamStringByValue(&self, id: u32, v: f64, s: *mut String128) -> tresult {
        if id >= PARAM_COUNT as u32 {
            return kNotImplemented;
        }
        let text = if id == WAVEFORM_PARAM_ID {
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
        let values = self.values.lock().unwrap_or_else(|p| p.into_inner());
        values.get(id as usize).copied().unwrap_or(0.0)
    }
    unsafe fn setParamNormalized(&self, id: u32, v: f64) -> tresult {
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
            _ => return kResultFalse,
        };
        *id = mapped;
        kResultOk
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
