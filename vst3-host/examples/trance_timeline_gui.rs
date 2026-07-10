//! LOCAL DEMO GUI (not shipped): a live trance player styled like a 90s hardware synth.
//!
//!   cargo run --example trance_timeline_gui                 # in-repo TestSynth (build with `just test-plugin`)
//!   cargo run --example trance_timeline_gui -- "/path/to/Synth.vst3"
//!
//! Plays classic trance riffs live through the `transport::Timeline` into a VST3 synth (four
//! embedded `.mid`s — a RIFF SELECT LCD with ◀/▶ buttons cycles between them and restarts
//! playback; a PATCH SELECT LCD loads whole front-panel sounds), through a
//! small trance FX chain (3-band EQ → tempo-synced dotted-1/8 ping-pong delay → Dattorro plate
//! reverb, all in `examples/dsp/`, plus a beat-locked kick + sidechain pump with an ON/OFF
//! toggle and LEVEL/PUNCH/DECAY/PUMP shaping). The window is a mock front panel —
//! brushed-texture chrome, knobs grouped into OSC / FILTER / FILTER ENV / AMP ENV / DELAY
//! (with a stepped TIME division knob) / REVERB / KICK sections (drag vertically,
//! double-click to reset), styled after SuperWave Ultimate: slate-blue metal
//! panel, dark inset sections with orange headers, cream knobs with dark pointers, green LED
//! value readouts, the supersaw OSC section highlighted in blue, and the piano roll drawn as
//! a big green backlit LCD. Audio runs
//! on a cpal output stream whose callback drives the timeline sample-synced. Synth-side knobs
//! map onto the plugin's parameters by name (TestSynth exposes them all; other synths degrade
//! gracefully), so pluck→lead morphs are all live knob rides here.

#[path = "dsp/mod.rs"]
mod dsp;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dsp::{EqBand, Kick, PingPong, PlateReverb, ThreeBandEq};
use eframe::egui;
use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use vst3_host::{
    audio::AudioBuffers,
    midi::{MidiChannel, MidiEvent},
    transport::{MidiClip, Timeline},
    Vst3Host,
};

const DEFAULT_PLUGIN: &str = "test_plugins/TestSynth.vst3";

/// The embedded riff collection the RIFF SELECT LCD cycles through.
const RIFFS: &[(&str, &[u8])] = &[
    (
        "HELGEWAVE ANTHEM",
        include_bytes!("assets/helgewave-anthem.mid"),
    ),
    (
        "MOON LOVES THE SUN",
        include_bytes!("assets/moon-loves-the-sun.mid"),
    ),
    ("SLYDER - SCORE", include_bytes!("assets/slyder-score.mid")),
    ("CARTE BLANCHE", include_bytes!("assets/carte-blanche.mid")),
    (
        "SUNBLIND - BELIEVE",
        include_bytes!("assets/sunblind-believe.mid"),
    ),
    (
        "ARAMANJA - MEMORIES",
        include_bytes!("assets/aramanja-memories.mid"),
    ),
];

/// What the audio callback needs to rebuild the timeline when the riff changes.
struct AudioRiff {
    events: Vec<(f64, MidiEvent)>,
    bpm: f64,
    total_beats: f64,
}

/// What the UI needs to draw a riff's piano roll.
struct UiRiff {
    name: &'static str,
    spans: Vec<NoteSpan>,
    total_beats: f64,
    min_pitch: u8,
    max_pitch: u8,
}

/// Parse the embedded riffs into audio- and UI-side halves.
fn load_riffs() -> (Vec<AudioRiff>, Vec<UiRiff>) {
    let mut audio = Vec::new();
    let mut ui = Vec::new();
    for (name, bytes) in RIFFS {
        let (events, spans, bpm) = load_midi(bytes);
        let last_beat = events.iter().map(|(b, _)| *b).fold(0.0, f64::max);
        let total_beats = last_beat + 1.0;
        let min_pitch = spans.iter().map(|s| s.pitch).min().unwrap_or(48);
        let max_pitch = spans.iter().map(|s| s.pitch).max().unwrap_or(72);
        println!(
            "riff \"{name}\": {} notes, {total_beats:.0} beats, {bpm:.0} BPM",
            spans.len()
        );
        audio.push(AudioRiff {
            events,
            bpm,
            total_beats,
        });
        ui.push(UiRiff {
            name,
            spans,
            total_beats,
            min_pitch,
            max_pitch,
        });
    }
    (audio, ui)
}

// ---------------------------------------------------------------------------
// Control surface definition
// ---------------------------------------------------------------------------

/// How a knob's value is displayed in its LED readout.
#[derive(Clone, Copy, PartialEq)]
enum Readout {
    /// Plain 0–100.
    Percent,
    /// Envelope time in ms/s (matches the TestSynth env-time mapping).
    EnvTime,
    /// Stepped tempo division (see `DIVISIONS`).
    Division,
}

/// The delay TIME knob's steps: name + length in beats. Dotted-1/8 is the trance staple.
const DIVISIONS: [(&str, f64); 5] = [
    ("1/16", 0.25),
    ("1/16.", 0.375),
    ("1/8", 0.5),
    ("1/8.", 0.75),
    ("1/4", 1.0),
];

/// Map a normalized knob value onto a `DIVISIONS` index.
fn division_index(v: f32) -> usize {
    ((v.clamp(0.0, 1.0) * (DIVISIONS.len() - 1) as f32).round() as usize).min(DIVISIONS.len() - 1)
}

/// A front-panel patch: one value per knob, in flat `SECTIONS` order:
/// DETUNE MIX | CUTOFF RESO ENVAMT | F-ENV A D S R | AMP A D S R |
/// DELAY TIME FB MIX | VERB DECAY MIX | KICK LEVEL PUNCH DECAY PUMP.
struct PatchDef {
    name: &'static str,
    values: [f32; 23],
}

#[rustfmt::skip]
const PATCHES: &[PatchDef] = &[
    PatchDef {
        // Tight and clicky, deep filter contrast, rolling dotted-1/8 shimmer.
        name: "TRANCE PLUCK",
        values: [
            0.55, 0.70, 0.50,       // osc (detune, mix, pad level)
            0.38, 0.35, 0.62,       // filter
            0.00, 0.58, 0.05, 0.45, // filter env
            0.00, 0.60, 0.10, 0.50, // amp env
            0.75, 0.50, 0.42,       // delay: 1/8., feedback, mix
            0.70, 0.18,             // reverb
            0.65, 0.55, 0.40, 0.70, // kick
        ],
    },
    PatchDef {
        // The classic full-on wall: sustained, open filter, wide and washed.
        name: "SUPERSAW LEAD",
        values: [
            0.62, 0.72, 0.55,
            0.72, 0.18, 0.15,
            0.10, 0.65, 0.50, 0.55,
            0.12, 0.70, 1.00, 0.62,
            0.75, 0.45, 0.32,       // delay: 1/8.
            0.80, 0.22,
            0.65, 0.55, 0.40, 0.70,
        ],
    },
    PatchDef {
        // Aggressive and dry: cranked resonance, tight gate, straight-1/8 delay,
        // hard clicky kick.
        name: "HARDTRANCE LEAD",
        values: [
            0.72, 0.78, 0.30,
            0.55, 0.55, 0.45,
            0.00, 0.55, 0.25, 0.45,
            0.00, 0.68, 0.62, 0.42,
            0.50, 0.40, 0.28,       // delay: straight 1/8
            0.60, 0.14,
            0.70, 0.75, 0.28, 0.75,
        ],
    },
    PatchDef {
        // Slow bloom, huge reverb, gentle pump — the breakdown wash.
        name: "LUSH PAD",
        values: [
            0.50, 0.62, 0.75,
            0.55, 0.12, 0.25,
            0.75, 0.80, 0.60, 0.75,
            0.78, 0.70, 1.00, 0.82,
            1.00, 0.35, 0.20,       // delay: 1/4
            0.90, 0.38,
            0.65, 0.45, 0.50, 0.50,
        ],
    },
];

/// One front-panel knob: display label, the plugin parameter it drives (`None` = host FX),
/// its default, and how its readout is formatted.
struct ControlDef {
    label: &'static str,
    param: Option<&'static str>,
    default: f32,
    readout: Readout,
}

const fn knob_def(label: &'static str, param: Option<&'static str>, default: f32) -> ControlDef {
    ControlDef {
        label,
        param,
        default,
        readout: Readout::Percent,
    }
}

const fn time_def(label: &'static str, param: &'static str, default: f32) -> ControlDef {
    ControlDef {
        label,
        param: Some(param),
        default,
        readout: Readout::EnvTime,
    }
}

const fn div_def(label: &'static str, default: f32) -> ControlDef {
    ControlDef {
        label,
        param: None,
        default,
        readout: Readout::Division,
    }
}

struct SectionDef {
    name: &'static str,
    /// The reference GUI highlights its supersaw oscillator section in bright blue.
    highlight: bool,
    /// Draw an ON/OFF LED toggle in the plate's top-right corner (the KICK section).
    toggle: bool,
    controls: &'static [ControlDef],
}

/// SuperWave-style palette: slate-blue body, orange silkscreen, green LED digits.
const SW_PANEL: egui::Color32 = egui::Color32::from_rgb(104, 112, 134);
const SW_PLATE: egui::Color32 = egui::Color32::from_rgb(46, 50, 62);
const SW_ORANGE: egui::Color32 = egui::Color32::from_rgb(255, 170, 40);
const SW_BLUE: egui::Color32 = egui::Color32::from_rgb(98, 164, 212);
const SW_LED_GREEN: egui::Color32 = egui::Color32::from_rgb(120, 255, 80);
const SW_LED_BG: egui::Color32 = egui::Color32::from_rgb(10, 30, 8);
/// Playhead red, like the reference's maroon buttons/LEDs.
const SW_RED: egui::Color32 = egui::Color32::from_rgb(216, 70, 50);

const SECTIONS: &[SectionDef] = &[
    SectionDef {
        name: "SUPERSAW OSC",
        highlight: true,
        toggle: false,
        controls: &[
            knob_def("DETUNE", Some("Detune"), 0.6),
            knob_def("MIX", Some("Mix"), 0.7),
            knob_def("PAD", Some("Ch2 Level"), 0.5),
        ],
    },
    SectionDef {
        name: "FILTER",
        highlight: false,
        toggle: false,
        controls: &[
            knob_def("CUTOFF", Some("Cutoff"), 0.5),
            knob_def("RESO", Some("Resonance"), 0.3),
            knob_def("ENV AMT", Some("Filter Env Amount"), 0.5),
        ],
    },
    SectionDef {
        name: "FILTER ENVELOPE",
        highlight: false,
        toggle: false,
        controls: &[
            time_def("A", "Filter Attack", 0.0),
            time_def("D", "Filter Decay", 0.65),
            knob_def("S", Some("Filter Sustain"), 0.15),
            time_def("R", "Filter Release", 0.5),
        ],
    },
    SectionDef {
        name: "AMP ENVELOPE",
        highlight: false,
        toggle: false,
        controls: &[
            time_def("A", "Amp Attack", 0.09),
            time_def("D", "Amp Decay", 0.7),
            knob_def("S", Some("Amp Sustain"), 0.35),
            time_def("R", "Amp Release", 0.59),
        ],
    },
    SectionDef {
        name: "DELAY",
        highlight: false,
        toggle: false,
        controls: &[
            div_def("TIME", 0.75),
            knob_def("FEEDBK", None, 0.45),
            knob_def("MIX", None, 0.35),
        ],
    },
    SectionDef {
        name: "REVERB",
        highlight: false,
        toggle: false,
        controls: &[knob_def("DECAY", None, 0.8), knob_def("MIX", None, 0.22)],
    },
    SectionDef {
        name: "KICK",
        highlight: false,
        toggle: true,
        controls: &[
            knob_def("LEVEL", None, 0.65),
            knob_def("PUNCH", None, 0.55),
            knob_def("DECAY", None, 0.4),
            knob_def("PUMP", None, 0.7),
        ],
    },
];

/// Flat indices of the host-FX knobs (everything before them targets plugin params).
/// Order: OSC(3) + FILTER(3) + FILTER ENV(4) + AMP ENV(4) = 14, then DELAY, REVERB, KICK.
const IDX_DELAY_TIME: usize = 14;
const IDX_DELAY_FB: usize = 15;
const IDX_DELAY_MIX: usize = 16;
const IDX_VERB_DECAY: usize = 17;
const IDX_VERB_MIX: usize = 18;
const IDX_KICK_LEVEL: usize = 19;
const IDX_KICK_PUNCH: usize = 20;
const IDX_KICK_DECAY: usize = 21;
const IDX_KICK_PUMP: usize = 22;
/// The OSC section's PAD knob (drives the pad instance's part level, not the lead plugin).
const IDX_PAD_LEVEL: usize = 2;

fn flat_controls() -> impl Iterator<Item = &'static ControlDef> {
    SECTIONS.iter().flat_map(|s| s.controls)
}

/// Live controls shared between the UI thread and the audio callback (f32 stored as bits),
/// one per knob (flat, in `SECTIONS` order) plus the playhead position.
struct Shared {
    values: Vec<AtomicU32>,
    playhead_beats: AtomicU32,
    /// Selected riff index; the audio callback rebuilds its timeline when this changes.
    riff_index: AtomicU32,
    /// Kick + sidechain on/off (the KICK section's LED toggle).
    kick_on: AtomicU32,
    /// Last-selected patch (display only — the knobs hold the actual values).
    patch_index: AtomicU32,
}

impl Shared {
    /// Powers on with patch 0 loaded (the per-knob `default`s remain the double-click reset).
    fn new() -> Self {
        Self {
            values: PATCHES[0]
                .values
                .iter()
                .map(|v| AtomicU32::new(v.to_bits()))
                .collect(),
            playhead_beats: AtomicU32::new(0),
            riff_index: AtomicU32::new(0),
            kick_on: AtomicU32::new(0),
            patch_index: AtomicU32::new(0),
        }
    }
}

fn getf(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}
fn setf(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// MIDI
// ---------------------------------------------------------------------------

/// A note as drawn in the piano roll.
struct NoteSpan {
    on_beat: f64,
    off_beat: f64,
    pitch: u8,
    /// True for channel-2 (pad part) notes, drawn dimmer under the melody.
    pad: bool,
}

/// Parse the SMF into timeline note events, drawable spans, and the tempo (BPM).
fn load_midi(bytes: &[u8]) -> (Vec<(f64, MidiEvent)>, Vec<NoteSpan>, f64) {
    let smf = Smf::parse(bytes).expect("parse midi");
    let tpq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as f64,
        Timing::Timecode(..) => 480.0,
    };
    let mut bpm = 140.0;
    let mut events = Vec::new();
    let mut spans = Vec::new();
    let mut open: Vec<(u8, f64)> = Vec::new(); // (pitch, on_beat) awaiting note-off

    for track in &smf.tracks {
        let mut tick: u64 = 0;
        for ev in track {
            tick += ev.delta.as_int() as u64;
            let beat = tick as f64 / tpq;
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    bpm = 60_000_000.0 / us.as_int() as f64;
                }
                TrackEventKind::Midi { channel, message } if channel.as_int() != 9 => {
                    let ch = MidiChannel::from_index(channel.as_int()).unwrap_or(MidiChannel::Ch1);
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            let note = key.as_int();
                            events.push((
                                beat,
                                MidiEvent::NoteOn {
                                    channel: ch,
                                    note,
                                    velocity: vel.as_int(),
                                },
                            ));
                            open.push((note, beat));
                        }
                        MidiMessage::NoteOff { key, .. } | MidiMessage::NoteOn { key, .. } => {
                            let note = key.as_int();
                            events.push((
                                beat,
                                MidiEvent::NoteOff {
                                    channel: ch,
                                    note,
                                    velocity: 0,
                                },
                            ));
                            if let Some(pos) = open.iter().rposition(|(p, _)| *p == note) {
                                let (pitch, on_beat) = open.remove(pos);
                                spans.push(NoteSpan {
                                    on_beat,
                                    off_beat: beat,
                                    pitch,
                                    pad: ch != MidiChannel::Ch1,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    (events, spans, bpm)
}

fn find_param(plugin: &vst3_host::Plugin, name: &str) -> Option<u32> {
    plugin
        .get_parameters()
        .ok()?
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| p.id)
}

// ---------------------------------------------------------------------------
// Front panel painting
// ---------------------------------------------------------------------------

const KNOB_W: f32 = 54.0;
const SECTION_H: f32 = 100.0;

/// Slate-blue metal panel, SuperWave style: blue-gray base with a vertical sheen (lighter at
/// the top), faint brushed hairlines, and a light dusting of speckles. All deterministic.
fn paint_panel_texture(p: &egui::Painter, rect: egui::Rect) {
    p.rect_filled(rect, 0.0, SW_PANEL);
    // Vertical sheen: translucent white bands fading down, then a darker footer.
    let bands = 12;
    for i in 0..bands {
        let t = i as f32 / bands as f32;
        let band = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.top() + t * rect.height()),
            egui::pos2(
                rect.right(),
                rect.top() + (t + 1.0 / bands as f32) * rect.height() + 1.0,
            ),
        );
        let a = (26.0 * (1.0 - t) - 8.0).max(0.0) as u8; // bright top edge → neutral
        p.rect_filled(
            band,
            0.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, a),
        );
    }
    let mut seed: u32 = 0x9E37_79B9;
    let mut y = rect.top();
    while y < rect.bottom() {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let a = 3 + (seed >> 28) as u8; // 3..18 alpha hairlines
        let bright = seed & 1 == 0;
        let color = if bright {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, a)
        } else {
            egui::Color32::from_rgba_unmultiplied(20, 24, 40, a + 6)
        };
        p.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0_f32, color),
        );
        y += 2.0;
    }
    for i in 0..400u32 {
        let h = i.wrapping_mul(2_654_435_761);
        let x = rect.left() + (h % 9973) as f32 / 9973.0 * rect.width();
        let yy = rect.top() + ((h >> 12) % 9973) as f32 / 9973.0 * rect.height();
        let a = 5 + ((h >> 26) & 12) as u8;
        p.circle_filled(
            egui::pos2(x, yy),
            0.6,
            egui::Color32::from_rgba_unmultiplied(240, 240, 250, a),
        );
    }
}

/// Format a knob value for its readout.
fn knob_readout(def: &ControlDef, v: f32) -> String {
    match def.readout {
        Readout::EnvTime => {
            let secs = 0.001 * 2000f32.powf(v); // matches the TestSynth env-time mapping
            if secs < 1.0 {
                format!("{:.0}ms", secs * 1000.0)
            } else {
                format!("{secs:.1}s")
            }
        }
        Readout::Division => DIVISIONS[division_index(v)].0.to_string(),
        Readout::Percent => format!("{:.0}", v * 100.0),
    }
}

/// One knob, SuperWave style: white label on top, cream plastic cap with a dark pointer in
/// the middle, green LED value readout below. Drag vertically to turn, double-click to reset.
/// `flat_idx` keys the egui id (labels repeat across sections).
fn draw_knob(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    def: &ControlDef,
    flat_idx: usize,
    value: &AtomicU32,
) {
    let resp = ui.interact(
        rect,
        egui::Id::new(("panel-knob", flat_idx)),
        egui::Sense::click_and_drag(),
    );
    let mut v = getf(value);
    if resp.dragged() {
        v = (v - resp.drag_delta().y / 160.0).clamp(0.0, 1.0);
        setf(value, v);
    }
    if resp.double_clicked() {
        v = def.default;
        setf(value, v);
    }

    let p = ui.painter();
    let hovered = resp.hovered() || resp.dragged();
    // Label on top, panel silkscreen.
    p.text(
        egui::pos2(rect.center().x, rect.top() + 5.0),
        egui::Align2::CENTER_CENTER,
        def.label,
        egui::FontId::proportional(8.5),
        if hovered {
            SW_ORANGE
        } else {
            egui::Color32::from_gray(225)
        },
    );

    let center = egui::pos2(rect.center().x, rect.top() + 30.0);
    let r = 13.0;
    // Dark recess, then the cream cap with a soft bottom shadow inside.
    p.circle_filled(center, r + 2.5, egui::Color32::from_rgb(24, 26, 32));
    p.circle_filled(center, r, egui::Color32::from_rgb(232, 228, 218));
    p.circle_filled(
        center + egui::vec2(0.0, 2.0),
        r - 2.0,
        egui::Color32::from_rgb(214, 209, 197),
    );
    p.circle_filled(
        center - egui::vec2(0.0, 2.0),
        r - 4.5,
        egui::Color32::from_rgb(240, 237, 229),
    );
    p.circle_stroke(
        center,
        r,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(70)),
    );
    // Pointer: dark line, like the printed line on a plastic cap. 270° sweep from 7 o'clock.
    let a = (-135.0 + 270.0 * v).to_radians();
    let dir = egui::vec2(a.sin(), -a.cos());
    p.line_segment(
        [center + dir * 2.0, center + dir * (r - 1.5)],
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(40, 42, 48)),
    );

    // Green LED readout in a dark bezel below.
    let led = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.bottom() - 9.0),
        egui::vec2(38.0, 13.0),
    );
    p.rect_filled(led, 2.0, SW_LED_BG);
    p.rect_stroke(
        led,
        2.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(20)),
        egui::StrokeKind::Inside,
    );
    p.text(
        led.center(),
        egui::Align2::CENTER_CENTER,
        knob_readout(def, v),
        egui::FontId::monospace(9.0),
        SW_LED_GREEN,
    );
}

/// One panel section: rounded plate, accent header, a row of knobs. Returns the number of
/// controls it consumed from the flat value array.
fn draw_section(ui: &mut egui::Ui, sec: &SectionDef, shared: &Shared, base_idx: usize) -> usize {
    let n = sec.controls.len();
    let size = egui::vec2(n as f32 * KNOB_W + 14.0, SECTION_H);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter();
    // Inset plate: dark navy recess — or the bright blue highlight plate for the star section.
    let (fill, header_color) = if sec.highlight {
        (SW_BLUE, egui::Color32::from_rgb(18, 30, 52))
    } else {
        (SW_PLATE, SW_ORANGE)
    };
    p.rect_filled(rect, 5.0, fill);
    p.rect_stroke(
        rect,
        5.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(110)),
        egui::StrokeKind::Inside,
    );
    // Header: uppercase silkscreen with divider lines flanking it, SuperWave style.
    let galley = p.layout_no_wrap(
        sec.name.to_string(),
        egui::FontId::proportional(9.5),
        header_color,
    );
    let text_w = galley.size().x;
    p.galley(
        egui::pos2(rect.center().x - text_w / 2.0, rect.top() + 5.0),
        galley,
        header_color,
    );
    let line_y = rect.top() + 10.5;
    let line_color = if sec.highlight {
        egui::Color32::from_rgba_unmultiplied(18, 30, 52, 140)
    } else {
        egui::Color32::from_gray(120)
    };
    p.line_segment(
        [
            egui::pos2(rect.left() + 7.0, line_y),
            egui::pos2(rect.center().x - text_w / 2.0 - 5.0, line_y),
        ],
        egui::Stroke::new(1.0_f32, line_color),
    );
    p.line_segment(
        [
            egui::pos2(rect.center().x + text_w / 2.0 + 5.0, line_y),
            egui::pos2(rect.right() - 7.0, line_y),
        ],
        egui::Stroke::new(1.0_f32, line_color),
    );
    // ON/OFF LED toggle (KICK section): red LED lit when active, click anywhere on it.
    if sec.toggle {
        let led_rect = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 26.0, rect.top() + 10.0),
            egui::vec2(30.0, 14.0),
        );
        let resp = ui.interact(
            led_rect,
            egui::Id::new(("section-toggle", sec.name)),
            egui::Sense::click(),
        );
        let mut on = shared.kick_on.load(Ordering::Relaxed) != 0;
        if resp.clicked() {
            on = !on;
            shared.kick_on.store(on as u32, Ordering::Relaxed);
        }
        let p = ui.painter();
        p.circle_filled(
            egui::pos2(led_rect.left() + 5.0, led_rect.center().y),
            3.5,
            if on {
                SW_RED
            } else {
                egui::Color32::from_gray(45)
            },
        );
        p.text(
            egui::pos2(led_rect.left() + 12.0, led_rect.center().y),
            egui::Align2::LEFT_CENTER,
            "ON",
            egui::FontId::proportional(8.0),
            egui::Color32::from_gray(215),
        );
    }
    for (i, def) in sec.controls.iter().enumerate() {
        let knob_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 7.0 + i as f32 * KNOB_W, rect.top() + 18.0),
            egui::vec2(KNOB_W, SECTION_H - 22.0),
        );
        draw_knob(
            ui,
            knob_rect,
            def,
            base_idx + i,
            &shared.values[base_idx + i],
        );
    }
    n
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct App {
    shared: Arc<Shared>,
    riffs: Vec<UiRiff>,
    _stream: cpal::Stream, // kept alive for the lifetime of the window
}

impl App {
    /// The RIFF SELECT unit: ◀/▶ buttons around a green LCD showing the current riff. Clicking
    /// either button cycles the riff; the audio callback notices and restarts from the top.
    fn riff_picker(&self, ui: &mut egui::Ui) {
        let n = self.riffs.len() as u32;
        let current = self.shared.riff_index.load(Ordering::Relaxed) % n;
        lcd_picker(
            ui,
            "RIFF SELECT",
            self.riffs[current as usize].name,
            190.0,
            |delta| {
                let next = (current as i32 + delta).rem_euclid(n as i32) as u32;
                self.shared.riff_index.store(next, Ordering::Relaxed);
            },
        );
    }

    /// Selecting a patch writes every knob's value, so the panel *is* the loaded sound and
    /// the audio callback's change-detection pushes it all to the plugin/FX.
    fn patch_picker(&self, ui: &mut egui::Ui) {
        let n = PATCHES.len() as u32;
        let current = self.shared.patch_index.load(Ordering::Relaxed) % n;
        lcd_picker(
            ui,
            "PATCH",
            PATCHES[current as usize].name,
            150.0,
            |delta| {
                let idx = (current as i32 + delta).rem_euclid(n as i32) as usize;
                self.shared.patch_index.store(idx as u32, Ordering::Relaxed);
                for (slot, v) in self.shared.values.iter().zip(PATCHES[idx].values.iter()) {
                    setf(slot, *v);
                }
            },
        );
    }
}

/// A SuperWave-style picker unit: silkscreen label, ◀/▶ buttons, green LCD. `step` gets
/// `-1` for ◀ and `+1` for ▶.
fn lcd_picker(
    ui: &mut egui::Ui,
    label: &str,
    display: &str,
    lcd_width: f32,
    mut step: impl FnMut(i32),
) {
    let button = |ui: &mut egui::Ui, glyph: &str, step: &mut dyn FnMut()| {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 20.0), egui::Sense::click());
        let fill = if resp.hovered() {
            egui::Color32::from_rgb(72, 78, 94)
        } else {
            SW_PLATE
        };
        let p = ui.painter();
        p.rect_filled(rect, 3.0, fill);
        p.rect_stroke(
            rect,
            3.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(130)),
            egui::StrokeKind::Inside,
        );
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(10.0),
            egui::Color32::from_gray(230),
        );
        if resp.clicked() {
            step();
        }
    };
    ui.label(egui::RichText::new(label).size(9.0).color(SW_ORANGE));
    button(ui, "◀", &mut || step(-1));
    let (lcd, _) = ui.allocate_exact_size(egui::vec2(lcd_width, 20.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(lcd, 2.0, SW_LED_BG);
    p.rect_stroke(
        lcd,
        2.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(20)),
        egui::StrokeKind::Inside,
    );
    p.text(
        lcd.center(),
        egui::Align2::CENTER_CENTER,
        display,
        egui::FontId::monospace(9.5),
        SW_LED_GREEN,
    );
    button(ui, "▶", &mut || step(1));
}

impl eframe::App for App {
    // eframe 0.34 requires `ui`; we build everything on the `Context` in `update` instead.
    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // egui 0.34: a single root `Ui` that panels are shown inside (see the inspector).
        let mut root_ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("trance_gui_root"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(ctx.content_rect()),
        );
        root_ui.set_clip_rect(ctx.content_rect());

        egui::Panel::top("controls").show_inside(&mut root_ui, |ui| {
            paint_panel_texture(ui.painter(), ui.max_rect().expand(8.0));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                // Two-tone wordmark, SuperWave style: white name + orange model.
                ui.label(
                    egui::RichText::new("HELGEWAVE")
                        .size(17.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                );
                ui.label(
                    egui::RichText::new("TRANCE STATION")
                        .size(17.0)
                        .strong()
                        .color(SW_ORANGE),
                );
                ui.add_space(24.0);
                self.riff_picker(ui);
                ui.add_space(18.0);
                self.patch_picker(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("v0.5 · drag knobs · double-click resets")
                            .size(9.0)
                            .color(egui::Color32::from_gray(220)),
                    );
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let mut base = 0;
                for sec in SECTIONS {
                    base += draw_section(ui, sec, &self.shared, base);
                    ui.add_space(6.0);
                }
            });
            ui.add_space(8.0);
        });

        egui::CentralPanel::default().show_inside(&mut root_ui, |ui| {
            let (resp, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::hover());
            let outer = resp.rect;

            // The roll is a big green backlit LCD set into the panel, like the reference's
            // patch display: slate metal around it, dark bezel, dark-green glass, green pixels.
            paint_panel_texture(&painter, outer);
            let bezel = outer.shrink(8.0);
            painter.rect_filled(bezel, 6.0, egui::Color32::from_rgb(22, 24, 28));
            let rect = bezel.shrink(6.0);
            painter.rect_filled(rect, 3.0, egui::Color32::from_rgb(16, 36, 12));
            painter.rect_stroke(
                rect,
                3.0,
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(42, 64, 36)),
                egui::StrokeKind::Inside,
            );

            let riff = &self.riffs
                [(self.shared.riff_index.load(Ordering::Relaxed) as usize) % self.riffs.len()];
            let pitch_range = (riff.max_pitch - riff.min_pitch).max(1) as f32;
            let row_h = rect.height() / (pitch_range + 1.0);
            let beat_to_x =
                |b: f64| rect.left() + (b / riff.total_beats.max(1.0)) as f32 * rect.width();
            let pitch_to_y = |p: u8| rect.bottom() - (p - riff.min_pitch) as f32 * row_h - row_h;

            // Beat grid (brighter every 4 beats — the bar lines), faint LCD segments.
            let mut beat = 0.0;
            while beat <= riff.total_beats {
                let x = beat_to_x(beat);
                let a = if (beat as u32) % 4 == 0 { 44 } else { 16 };
                painter.line_segment(
                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                    egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(130, 210, 130, a),
                    ),
                );
                beat += 1.0;
            }

            // Notes: lit LCD-green pixels with a soft backlight bloom; pad-part notes
            // (channel 2) glow dimmer underneath the melody.
            for s in &riff.spans {
                let x0 = beat_to_x(s.on_beat);
                let x1 = beat_to_x(s.off_beat).max(x0 + 2.0);
                let y = pitch_to_y(s.pitch);
                let note =
                    egui::Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, y + row_h * 0.9));
                if s.pad {
                    painter.rect_filled(
                        note,
                        1.5,
                        egui::Color32::from_rgba_unmultiplied(90, 170, 90, 110),
                    );
                } else {
                    painter.rect_filled(
                        note.expand(2.0),
                        3.0,
                        egui::Color32::from_rgba_unmultiplied(120, 230, 120, 34),
                    );
                    painter.rect_filled(note, 1.5, egui::Color32::from_rgb(150, 235, 130));
                }
            }

            // Playhead: the one red accent on the display, like the reference's red LEDs.
            let px = beat_to_x(getf(&self.shared.playhead_beats) as f64);
            painter.line_segment(
                [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
                egui::Stroke::new(
                    5.0_f32,
                    egui::Color32::from_rgba_unmultiplied(216, 70, 50, 50),
                ),
            );
            painter.line_segment(
                [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
                egui::Stroke::new(2.0_f32, SW_RED),
            );
        });

        ctx.request_repaint(); // keep the playhead moving
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let plugin_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_PLUGIN.to_string());
    if !std::path::Path::new(&plugin_path).exists() {
        eprintln!("Synth not found: {plugin_path}");
        eprintln!("Build the bundled synth with `just test-plugin`, or pass a VST3 synth path.");
        return Ok(());
    }

    // Audio device config drives the sample rate the plugin is set up at.
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("no default output device")?;
    let supported = device.default_output_config()?;
    let sr = supported.sample_rate() as f64; // cpal 0.18: SampleRate is a u32 alias
    let channels = supported.channels() as usize;
    let max_block = 8192usize;

    let (audio_riffs, ui_riffs) = load_riffs();
    assert_eq!(
        flat_controls().count(),
        PATCHES[0].values.len(),
        "PATCHES must have one value per panel knob"
    );

    let mut host_builder = Vst3Host::builder()
        .sample_rate(sr)
        .block_size(max_block)
        .build()?;
    let mut plugin = host_builder.load_plugin(&plugin_path)?;
    println!(
        "loaded: {} by {} @ {sr} Hz",
        plugin.info().name,
        plugin.info().vendor
    );

    // A second instance renders the pad part to its own bus, so the delay + reverb can stay
    // on the lead while the pad joins the mix dry (reverbed pads mud up fast). The lead
    // instance mutes its channel-2 part; the pad instance mutes channel 1 and plays the
    // Lush Pad preset on channel 2. Both receive the same timeline events.
    let mut pad_plugin = host_builder.load_plugin(&plugin_path)?;
    let has_parts = find_param(&plugin, "Ch1 Level").is_some();
    if has_parts {
        if let Some(p) = find_param(&plugin, "Ch2 Level") {
            let _ = plugin.set_parameter(p, 0.0);
        }
        if let Some(p) = find_param(&pad_plugin, "Ch1 Level") {
            let _ = pad_plugin.set_parameter(p, 0.0);
        }
        if let Some(p) = find_param(&pad_plugin, "Ch2 Program") {
            let _ = pad_plugin.set_parameter(p, 1.0);
        }
        println!("pad part on its own bus (dry) via a second plugin instance");
    }
    // Prefer a super-saw waveform if the synth offers one; every panel knob then drives its
    // plugin parameter by name (missing params are skipped, so any synth still plays).
    if let Some(w) = find_param(&plugin, "Waveform") {
        let _ = plugin.set_parameter(w, 1.0);
    }
    // The PAD knob drives the pad instance's part level; all other knobs drive the lead.
    let pad_level_id = find_param(&pad_plugin, "Ch2 Level");
    let param_ids: Vec<Option<u32>> = flat_controls()
        .enumerate()
        .map(|(i, c)| {
            if i == IDX_PAD_LEVEL {
                None
            } else {
                c.param.and_then(|name| find_param(&plugin, name))
            }
        })
        .collect();
    let mapped = param_ids.iter().flatten().count();
    println!("panel: {mapped} knobs mapped to plugin parameters");
    plugin.start_processing()?;
    pad_plugin.start_processing()?;

    // Build the timeline for a riff: fresh clip, tempo, and a re-synced dotted-1/8 delay.
    let make_timeline = move |riff: &AudioRiff| {
        let mut clip = MidiClip::new();
        for &(beat, ev) in &riff.events {
            clip.add(beat, ev);
        }
        let timeline = Timeline::new(sr, riff.bpm).with_clip(clip);
        let total_frames = timeline.beat_to_frame(riff.total_beats).max(1);
        // Buffer one full beat — the TIME knob's largest division; the knob retunes the tap.
        let delay = PingPong::new((60.0 / riff.bpm * sr) as usize, sr as f32);
        let samples_per_beat = sr * 60.0 / riff.bpm;
        (timeline, total_frames, delay, samples_per_beat)
    };
    let mut current_riff = 0usize;
    let (mut timeline, mut total_frames, mut delay, mut samples_per_beat) =
        make_timeline(&audio_riffs[current_riff]);

    let shared = Arc::new(Shared::new());
    let shared_audio = shared.clone();
    let mut reverb = PlateReverb::new(sr, 20.0);
    let mut kick = Kick::new(sr as f32);
    let mut next_kick = 0.0f64; // absolute sample position of the next kick trigger
                                // Channel EQ before the sends: trim the mud, add presence, lift the sparkle.
    let mut eq = ThreeBandEq::new(
        sr as f32,
        EqBand {
            hz: 120.0,
            gain_db: -1.5,
            q: 0.7,
        },
        EqBand {
            hz: 2500.0,
            gain_db: 1.0,
            q: 1.0,
        },
        EqBand {
            hz: 8000.0,
            gain_db: 2.5,
            q: 0.7,
        },
    );
    // Only push knob values to the plugin when they actually change.
    let mut last_sent: Vec<f32> = vec![f32::NAN; param_ids.len()];

    let config = cpal::StreamConfig {
        channels: channels as u16,
        sample_rate: sr as u32,
        buffer_size: cpal::BufferSize::Default,
    };
    let stream = device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / channels.max(1);
            if frames == 0 {
                return;
            }
            // Riff switch from the RIFF SELECT buttons: rebuild the timeline (new tempo,
            // re-synced delay), silence held notes, and restart from the top.
            let want = shared_audio.riff_index.load(Ordering::Relaxed) as usize % audio_riffs.len();
            if want != current_riff {
                current_riff = want;
                (timeline, total_frames, delay, samples_per_beat) =
                    make_timeline(&audio_riffs[current_riff]);
                next_kick = 0.0;
                for p in [&mut plugin, &mut pad_plugin] {
                    let _ = p.send_midi_event(MidiEvent::ControlChange {
                        channel: MidiChannel::Ch1,
                        controller: 123,
                        value: 0,
                    }); // CC 123 = all notes off
                }
            }
            // Loop the riff.
            if timeline.sample_clock() >= total_frames {
                timeline.seek_frame(0);
                next_kick = 0.0;
                for p in [&mut plugin, &mut pad_plugin] {
                    let _ = p.send_midi_event(MidiEvent::ControlChange {
                        channel: MidiChannel::Ch1,
                        controller: 123,
                        value: 0,
                    }); // CC 123 = all notes off
                }
            }
            // Live knobs → plugin params (by name-resolved id, change-detected).
            for (i, id) in param_ids.iter().enumerate() {
                if let Some(id) = id {
                    let v = getf(&shared_audio.values[i]);
                    if (v - last_sent[i]).abs() > 1e-5 || last_sent[i].is_nan() {
                        let _ = plugin.set_parameter(*id, v as f64);
                        last_sent[i] = v;
                    }
                }
            }
            if let Some(id) = pad_level_id {
                let v = getf(&shared_audio.values[IDX_PAD_LEVEL]);
                if (v - last_sent[IDX_PAD_LEVEL]).abs() > 1e-5 || last_sent[IDX_PAD_LEVEL].is_nan()
                {
                    let _ = pad_plugin.set_parameter(id, v as f64);
                    last_sent[IDX_PAD_LEVEL] = v;
                }
            }
            // Drive the timeline for this block.
            let block_start = timeline.sample_clock() as f64;
            let block = timeline.advance_block(frames);
            for (ev, off) in block.midi {
                let _ = plugin.send_midi_event_at(ev, off);
                let _ = pad_plugin.send_midi_event_at(ev, off);
            }
            let mut buf = AudioBuffers::new(0, 2, frames, sr);
            let mut pad_buf = AudioBuffers::new(0, 2, frames, sr);
            let pad_ok = pad_plugin.process_audio(&mut pad_buf).is_ok();
            if plugin.process_audio(&mut buf).is_ok() {
                let (mut l, mut r) = (buf.outputs[0].clone(), buf.outputs[1].clone());
                eq.process(&mut l, &mut r);
                let division =
                    DIVISIONS[division_index(getf(&shared_audio.values[IDX_DELAY_TIME]))].1;
                delay.set_delay((division * samples_per_beat) as usize);
                delay.process(
                    &mut l,
                    &mut r,
                    getf(&shared_audio.values[IDX_DELAY_FB]),
                    getf(&shared_audio.values[IDX_DELAY_MIX]),
                );
                reverb.decay = getf(&shared_audio.values[IDX_VERB_DECAY]);
                reverb.process(&mut l, &mut r, getf(&shared_audio.values[IDX_VERB_MIX]));
                // The pad bus joins here — dry (post lead FX), but before the sidechain so
                // it pumps with everything else.
                if pad_ok {
                    for (dst, src) in l.iter_mut().zip(pad_buf.outputs[0].iter()) {
                        *dst += src;
                    }
                    for (dst, src) in r.iter_mut().zip(pad_buf.outputs[1].iter()) {
                        *dst += src;
                    }
                }
                if shared_audio.kick_on.load(Ordering::Relaxed) != 0 {
                    // Sidechain pump keyed to the beat grid: duck the synth bus, then lay the
                    // kick on top unducked (same order as the offline demo).
                    let depth = getf(&shared_audio.values[IDX_KICK_PUMP]) * 0.8;
                    for (i, (ls, rs)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
                        let t = ((block_start + i as f64) % samples_per_beat) / sr;
                        let g = 1.0 - depth * (-(t / 0.085)).exp() as f32;
                        *ls *= g;
                        *rs *= g;
                    }
                    let (level, punch, kdecay) = (
                        getf(&shared_audio.values[IDX_KICK_LEVEL]),
                        getf(&shared_audio.values[IDX_KICK_PUNCH]),
                        getf(&shared_audio.values[IDX_KICK_DECAY]),
                    );
                    // Render the kick, splitting the block at beat boundaries so triggers
                    // land sample-accurately.
                    let mut i = 0usize;
                    while i < frames {
                        let abs = block_start + i as f64;
                        if next_kick <= abs {
                            kick.trigger();
                            next_kick += samples_per_beat;
                            continue;
                        }
                        let span = (((next_kick - abs).ceil() as usize).max(1)).min(frames - i);
                        kick.process(
                            &mut l[i..i + span],
                            &mut r[i..i + span],
                            level,
                            punch,
                            kdecay,
                        );
                        i += span;
                    }
                } else {
                    // Keep the trigger armed on the next beat boundary while switched off.
                    next_kick = ((block_start + frames as f64) / samples_per_beat).ceil()
                        * samples_per_beat;
                }
                for (i, frame) in data.chunks_mut(channels.max(1)).enumerate() {
                    // Soft clip: resonance + delay feedback can push peaks past full scale.
                    let (lv, rv) = (
                        l.get(i).copied().unwrap_or(0.0).tanh(),
                        r.get(i).copied().unwrap_or(0.0).tanh(),
                    );
                    for (ch, out) in frame.iter_mut().enumerate() {
                        *out = if ch % 2 == 0 { lv } else { rv };
                    }
                }
            } else {
                data.fill(0.0);
            }
            let beats = timeline.sample_clock() as f64 / samples_per_beat;
            setf(&shared_audio.playhead_beats, beats as f32);
        },
        move |err| eprintln!("audio stream error: {err}"),
        None,
    )?;
    stream.play()?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 620.0])
            .with_title("HS-8000 — Supersaw Trance Station"),
        ..Default::default()
    };
    eframe::run_native(
        "HS-8000 — Supersaw Trance Station",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(App {
                shared,
                riffs: ui_riffs,
                _stream: stream,
            }))
        }),
    )?;
    Ok(())
}
