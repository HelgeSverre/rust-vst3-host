//! Example: play a real MIDI file (a Nu-NRG riff, embedded) through the `transport::Timeline`
//! into a VST3 synth, with an LPF cutoff sweep + program selection, render it offline to a WAV,
//! and (on macOS) play it.
//!
//!   cargo run --example trance_timeline_demo                 # defaults to Jup-8000 if present
//!   cargo run --example trance_timeline_demo -- "/path/to/Synth.vst3"
//!   VST3_PLUGIN="/path/to/Synth.vst3" cargo run --example trance_timeline_demo
//!   RIFF=nu-nrg cargo run --example trance_timeline_demo     # pick the embedded riff
//!
//! Showcases 0.5.0 program/preset selection + the timeline engine driving a real `.mid`. Two
//! riffs are embedded so the example is self-contained; you only supply a VST3 synth. The filter
//! sweep targets a "Cutoff"/"Filter Type" parameter if the synth exposes one (it degrades
//! gracefully otherwise).

use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use vst3_host::{
    audio::{write_wav, AudioBuffers},
    midi::{MidiChannel, MidiEvent},
    parameters::{AutomationCurve, ParameterAutomation},
    transport::{AutomationLane, MidiClip, Timeline},
    Vst3Host,
};

/// Embedded riffs (name, SMF bytes), so the example needs no external file. Pick with `$RIFF`.
const RIFFS: &[(&str, &[u8])] = &[
    ("moon", include_bytes!("assets/moon-loves-the-sun.mid")),
    ("nu-nrg", include_bytes!("assets/nu-nrg-riff.mid")),
];
/// Default synth if none is passed; override with an arg or `VST3_PLUGIN`.
const DEFAULT_PLUGIN: &str = "/Library/Audio/Plug-Ins/VST3/Jup-8000 V.vst3";

/// Parse the SMF into `(beat, MidiEvent)` note events plus the file's tempo (BPM).
/// Drum channel (MIDI ch 10 / index 9) is skipped; everything else collapses onto Ch1.
fn load_midi(bytes: &[u8]) -> (Vec<(f64, MidiEvent)>, f64) {
    let smf = Smf::parse(bytes).expect("parse embedded midi");
    let tpq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as f64,
        Timing::Timecode(_, _) => 480.0,
    };
    let mut bpm = 138.0;
    let mut events: Vec<(f64, MidiEvent)> = Vec::new();

    for track in &smf.tracks {
        let mut tick: u64 = 0;
        for ev in track {
            tick += ev.delta.as_int() as u64;
            let beat = tick as f64 / tpq;
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us_per_beat)) => {
                    bpm = 60_000_000.0 / us_per_beat.as_int() as f64;
                }
                TrackEventKind::Midi { channel, message } => {
                    if channel.as_int() == 9 {
                        continue; // skip GM drums
                    }
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            events.push((
                                beat,
                                MidiEvent::NoteOn {
                                    channel: MidiChannel::Ch1,
                                    note: key.as_int(),
                                    velocity: vel.as_int(),
                                },
                            ));
                        }
                        MidiMessage::NoteOff { key, .. } | MidiMessage::NoteOn { key, .. } => {
                            events.push((
                                beat,
                                MidiEvent::NoteOff {
                                    channel: MidiChannel::Ch1,
                                    note: key.as_int(),
                                    velocity: 0,
                                },
                            ));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    (events, bpm)
}

fn main() -> vst3_host::Result<()> {
    let sr = 48_000.0;
    let block = 512usize;

    // Plugin: CLI arg, else $VST3_PLUGIN, else the default. Fail with guidance if absent.
    let plugin_path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("VST3_PLUGIN").ok())
        .unwrap_or_else(|| DEFAULT_PLUGIN.to_string());
    if !std::path::Path::new(&plugin_path).exists() {
        eprintln!("Synth not found: {plugin_path}");
        eprintln!("Pass a VST3 synth path: cargo run --example trance_timeline_demo -- \"/path/to/Synth.vst3\"");
        eprintln!("(or set VST3_PLUGIN). Any polyphonic VST3 synth works; a \"Cutoff\" param enables the filter sweep.");
        return Ok(());
    }

    // Pick the embedded riff (default: the first), selectable with `$RIFF`.
    let riff = std::env::var("RIFF").unwrap_or_default();
    let (name, bytes) = RIFFS
        .iter()
        .find(|(n, _)| *n == riff)
        .copied()
        .unwrap_or(RIFFS[0]);
    println!("riff: {name}");
    let (events, bpm) = load_midi(bytes);
    let last_beat = events.iter().map(|(b, _)| *b).fold(0.0, f64::max);
    let note_ons = events
        .iter()
        .filter(|(_, e)| matches!(e, MidiEvent::NoteOn { .. }))
        .count();
    println!(
        "MIDI: {} note events ({note_ons} note-ons), {last_beat:.1} beats, {bpm:.1} BPM",
        events.len()
    );

    let mut host = Vst3Host::builder()
        .sample_rate(sr)
        .block_size(block)
        .build()?;
    let mut plugin = host.load_plugin(&plugin_path)?;
    println!("loaded: {} by {}", plugin.info().name, plugin.info().vendor);

    // Preset/program selection (0.5.0 IUnitInfo program lists).
    if let Ok(units) = plugin.get_units() {
        if let Some(u) = units.iter().find(|u| !u.programs.is_empty()) {
            let pick = u
                .programs
                .iter()
                .position(|p| {
                    let n = p.to_lowercase();
                    ["saw", "super", "trance", "lead", "pluck"]
                        .iter()
                        .any(|k| n.contains(k))
                })
                .unwrap_or(0);
            println!(
                "program: unit {} \"{}\" → #{pick} \"{}\"",
                u.id, u.name, u.programs[pick]
            );
            let _ = plugin.select_program(u.id, pick as i32);
        }
    }

    plugin.start_processing()?;

    // Find the main synth filter to sweep (a synth like Jup-8000 has thousands of params).
    let params = plugin.get_parameters()?;
    let find = |name: &str| {
        params
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .cloned()
    };

    // The main synth filter is "Cutoff" (#40), with "Filter Type" (#38) / "Resonance" (#41).
    let cutoff = find("Cutoff").or_else(|| {
        params
            .iter()
            .find(|p| {
                let n = p.name.to_lowercase();
                n.contains("cutoff") && !n.contains("noise") && !n.contains("fx")
            })
            .cloned()
    });
    match &cutoff {
        Some(p) => println!("cutoff sweep on #{} \"{}\"", p.id, p.name),
        None => println!("no cutoff param found — no filter sweep"),
    }

    // Force the filter to low-pass: probe the "Filter Type" display strings for the LP mode.
    if let Some(ft) = find("Filter Type") {
        let mut chosen = None;
        for i in 0..=16 {
            let v = i as f64 / 16.0;
            if let Ok(label) = plugin.format_parameter(ft.id, v) {
                let l = label.to_lowercase();
                if l.contains("lp") || l.contains("low") {
                    chosen = Some((v, label));
                    break;
                }
            }
        }
        if let Some((v, label)) = chosen {
            plugin.set_parameter(ft.id, v)?;
            println!("filter type → \"{label}\" (LPF)");
        } else {
            println!("could not identify an LP mode on \"Filter Type\" — leaving default");
        }
    }
    // A little resonance makes the sweep sing.
    if let Some(res) = find("Resonance") {
        plugin.set_parameter(res.id, 0.35)?;
        println!("resonance → 0.35 on #{} \"{}\"", res.id, res.name);
    }
    // Prefer the richest waveform the synth offers (super saw > saw), and widen the detune.
    if let Some(wave) = find("Waveform") {
        plugin.set_parameter(wave.id, 1.0)?;
        let label = plugin
            .format_parameter(wave.id, 1.0)
            .unwrap_or_else(|_| "saw".into());
        println!("waveform → \"{label}\" on #{} \"{}\"", wave.id, wave.name);
    }
    if let Some(detune) = find("Detune") {
        plugin.set_parameter(detune.id, 0.5)?;
        println!("detune → 0.5 on #{} \"{}\"", detune.id, detune.name);
    }

    // Build the timeline straight from the MIDI events.
    let mut clip = MidiClip::new();
    for (beat, ev) in events {
        clip.add(beat, ev);
    }
    let mut timeline = Timeline::new(sr, bpm).with_clip(clip);

    // A trance filter sweep over the whole riff.
    if let Some(p) = &cutoff {
        let span = last_beat.max(8.0);
        let sweep = ParameterAutomation::new()
            .add_point(0.0, 0.30)
            .add_point(span * 0.25, 0.95)
            .add_point(span * 0.5, 0.40)
            .add_point(span * 0.75, 1.0)
            .add_point(span, 0.5)
            .with_curve(AutomationCurve::Linear);
        timeline.add_lane(AutomationLane::new(p.id, sweep, 16));
    }

    // Render to the end of the riff + a release tail.
    let total_secs = (last_beat + 1.0) * 60.0 / bpm + 1.5;
    let total_blocks = (total_secs * sr / block as f64).ceil() as usize;

    let mut buf = AudioBuffers::new(0, 2, block, sr);
    let mut left = Vec::with_capacity(total_blocks * block);
    let mut right = Vec::with_capacity(total_blocks * block);
    for _ in 0..total_blocks {
        timeline.drive_block(&mut plugin, &mut buf)?;
        left.extend_from_slice(&buf.outputs[0]);
        let r = if buf.outputs.len() > 1 { 1 } else { 0 };
        right.extend_from_slice(&buf.outputs[r]);
    }
    plugin.stop_processing().ok();

    let peak = left
        .iter()
        .chain(right.iter())
        .fold(0.0f32, |m, &s| m.max(s.abs()));
    println!("rendered {total_secs:.1}s, peak amplitude {peak:.3}");

    let out = std::env::temp_dir().join("trance_timeline_demo.wav");
    write_wav(&out, &[left, right], sr as u32)?;
    println!("wrote {}", out.display());

    #[cfg(target_os = "macos")]
    {
        println!("playing… (afplay)");
        let _ = std::process::Command::new("afplay").arg(&out).status();
    }
    Ok(())
}
