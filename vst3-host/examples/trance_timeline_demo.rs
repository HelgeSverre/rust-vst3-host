//! Example: play a real MIDI file (a Nu-NRG riff, embedded) through the `transport::Timeline`
//! into a VST3 synth, with an LPF cutoff sweep + program selection, render it offline through a
//! small trance FX chain (3-band EQ → tempo-synced ping-pong delay → Dattorro plate reverb, all
//! in `examples/dsp/`) to a WAV, and (on macOS) play it.
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

#[path = "dsp/mod.rs"]
mod dsp;

use dsp::{EqBand, PingPong, PlateReverb, ThreeBandEq};
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
    ("anthem", include_bytes!("assets/helgewave-anthem.mid")),
];
/// Default synth if none is passed; override with an arg or `VST3_PLUGIN`.
const DEFAULT_PLUGIN: &str = "/Library/Audio/Plug-Ins/VST3/Jup-8000 V.vst3";

/// Parse the SMF into `(beat, MidiEvent)` note events plus the file's tempo (BPM).
/// Drum channel (MIDI ch 10 / index 9) is skipped; other channels are preserved (the
/// bitimbral TestSynth plays channel 2 with its pad part).
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
                    let ch = MidiChannel::from_index(channel.as_int()).unwrap_or(MidiChannel::Ch1);
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            events.push((
                                beat,
                                MidiEvent::NoteOn {
                                    channel: ch,
                                    note: key.as_int(),
                                    velocity: vel.as_int(),
                                },
                            ));
                        }
                        MidiMessage::NoteOff { key, .. } | MidiMessage::NoteOn { key, .. } => {
                            events.push((
                                beat,
                                MidiEvent::NoteOff {
                                    channel: ch,
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

/// Resolve a delay-division name to its length in beats. Trance staples: dotted-1/8 (rolling
/// lead) and 1/16 / dotted-1/16 (fast supersaw-pluck shimmer).
fn delay_beats(name: &str) -> Option<f64> {
    match name {
        "off" => None,
        "quarter" => Some(1.0),
        "dotted8" => Some(0.75),
        "8" => Some(0.5),
        "dotted16" => Some(0.375),
        "16" => Some(0.25),
        _ => Some(0.75), // default: dotted 1/8, the rolling trance staple
    }
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
    // The anthem is written as a sustained lead piece — it skips the pluck→lead morph below
    // and plays a fully open supersaw throughout.
    let is_anthem = name == "anthem";
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
        plugin.set_parameter(detune.id, 0.6)?;
        println!("detune → 0.6 on #{} \"{}\"", detune.id, detune.name);
    }
    // Bright center/side blend (Szabo's curves put the classic JP-8000 mix around 0.7).
    if let Some(mix) = find("Mix") {
        plugin.set_parameter(mix.id, 0.7)?;
        println!("mix → 0.7 on #{} \"{}\"", mix.id, mix.name);
    }
    // Envelope base settings (TestSynth ADSRs). Default arrangement: a tight pluck that the
    // automation lanes below morph into the sustained lead. The anthem instead runs fully
    // open from bar one.
    let base_settings: &[(&str, f64)] = if is_anthem {
        &[
            ("Amp Sustain", 1.0),
            ("Amp Release", 0.62),
            ("Cutoff", 1.0), // wide open, no sweep — the anthem stays bright throughout
            ("Filter Env Amount", 0.1),
            ("Resonance", 0.18),
            ("Ch2 Program", 1.0),
        ]
    } else {
        &[
            ("Amp Decay", 0.6), // ~95 ms — tighter than the riff's 16th grid, so notes gate
            ("Filter Decay", 0.62), // ~110 ms filter fall
            ("Filter Sustain", 0.1),
            ("Filter Release", 0.5),
            ("Ch2 Program", 1.0), // second timbre = Lush Pad...
            ("Ch2 Level", 0.5),   // ...playing whatever the riff has on MIDI channel 2
        ]
    };
    let mut env_set = 0;
    for (name, v) in base_settings {
        if let Some(p) = find(name) {
            plugin.set_parameter(p.id, *v)?;
            env_set += 1;
        }
    }
    if env_set > 0 {
        println!(
            "envelopes: {} settings on {env_set} params",
            if is_anthem {
                "open supersaw lead"
            } else {
                "pluck base"
            }
        );
    }

    // Build the timeline straight from the MIDI events (kept around — the anthem's second
    // render pass rebuilds a fresh timeline from them).
    let mut clip = MidiClip::new();
    for &(beat, ev) in &events {
        clip.add(beat, ev);
    }
    let mut timeline = Timeline::new(sr, bpm).with_clip(clip);

    // The arrangement: a gated pluck for the first half that morphs into the sustained
    // supersaw lead — amp sustain ramps up, the filter envelope hands over to an open cutoff.
    let span = last_beat.max(8.0);
    let (morph_from, morph_to) = (span * 0.45, span * 0.6);
    if let Some(p) = cutoff.as_ref().filter(|_| !is_anthem) {
        let sweep = ParameterAutomation::new()
            .add_point(0.0, 0.32)
            .add_point(morph_from, 0.40)
            .add_point(morph_to, 0.75)
            .add_point(span * 0.8, 0.95)
            .add_point(span, 0.6)
            .with_curve(AutomationCurve::Linear);
        timeline.add_lane(AutomationLane::new(p.id, sweep, 16));
    }
    if let Some(p) = find("Amp Sustain").filter(|_| !is_anthem) {
        let morph = ParameterAutomation::new()
            .add_point(0.0, 0.12)
            .add_point(morph_from, 0.12)
            .add_point(morph_to, 1.0)
            .add_point(span, 1.0)
            .with_curve(AutomationCurve::Linear);
        timeline.add_lane(AutomationLane::new(p.id, morph, 16));
        println!("amp sustain: pluck (12%) → lead (100%) over beats {morph_from:.0}–{morph_to:.0}");
    }
    if let Some(p) = find("Filter Env Amount").filter(|_| !is_anthem) {
        let morph = ParameterAutomation::new()
            .add_point(0.0, 0.60)
            .add_point(morph_from, 0.60)
            .add_point(morph_to, 0.15)
            .add_point(span, 0.15)
            .with_curve(AutomationCurve::Linear);
        timeline.add_lane(AutomationLane::new(p.id, morph, 16));
        println!("filter env amount: pluck (60%) → lead (15%)");
    }

    // Render to the end of the riff + a release tail.
    let total_secs = (last_beat + 1.0) * 60.0 / bpm + 1.5;
    let total_blocks = (total_secs * sr / block as f64).ceil() as usize;

    let mut buf = AudioBuffers::new(0, 2, block, sr);
    let mut render = |timeline: &mut Timeline, plugin: &mut vst3_host::Plugin| {
        let mut l = Vec::with_capacity(total_blocks * block);
        let mut r = Vec::with_capacity(total_blocks * block);
        for _ in 0..total_blocks {
            timeline.drive_block(plugin, &mut buf)?;
            l.extend_from_slice(&buf.outputs[0]);
            let ri = if buf.outputs.len() > 1 { 1 } else { 0 };
            r.extend_from_slice(&buf.outputs[ri]);
        }
        vst3_host::Result::Ok((l, r))
    };

    // The anthem renders its two parts to separate buses (via the Ch1/Ch2 Level params), so
    // the lead can take delay + reverb while the pad stays dry and unmuddied.
    let mut pad_bus: Option<(Vec<f32>, Vec<f32>)> = None;
    let (mut left, mut right) = if is_anthem {
        let set = |plugin: &mut vst3_host::Plugin, name: &str, v: f64| {
            if let Some(id) = params.iter().find(|p| p.name == name).map(|p| p.id) {
                let _ = plugin.set_parameter(id, v);
            }
        };
        set(&mut plugin, "Ch2 Level", 0.0); // pass 1: lead only
        let lead = render(&mut timeline, &mut plugin)?;
        set(&mut plugin, "Ch1 Level", 0.0); // pass 2: pad only
        set(&mut plugin, "Ch2 Level", 0.6);
        let mut clip = MidiClip::new();
        for &(beat, ev) in &events {
            clip.add(beat, ev);
        }
        let mut pad_timeline = Timeline::new(sr, bpm).with_clip(clip);
        pad_bus = Some(render(&mut pad_timeline, &mut plugin)?);
        println!("anthem: lead and pad rendered to separate buses");
        lead
    } else {
        render(&mut timeline, &mut plugin)?
    };
    plugin.stop_processing().ok();

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
    eq.process(&mut left, &mut right);
    println!("EQ: low shelf 120 Hz -1.5 dB, peak 2.5 kHz +1 dB, high shelf 8 kHz +2.5 dB");

    // Pad a few seconds of silence so the delay echoes and reverb tail ring out.
    let tail = (4.0 * sr) as usize;
    left.resize(left.len() + tail, 0.0);
    right.resize(right.len() + tail, 0.0);

    // Tempo-synced ping-pong delay (classic trance). Pick the division with $DELAY.
    // The anthem sends the lead bus hotter into it.
    let delay_name = std::env::var("DELAY").unwrap_or_else(|_| "dotted8".into());
    if let Some(beats) = delay_beats(&delay_name) {
        let delay_secs = beats * 60.0 / bpm;
        let (feedback, wet) = if is_anthem { (0.52, 0.5) } else { (0.45, 0.35) };
        let mut delay = PingPong::new((delay_secs * sr) as usize, sr as f32);
        delay.process(&mut left, &mut right, feedback, wet);
        println!(
            "ping-pong delay: {delay_name} ({:.0} ms, feedback {feedback}, wet {wet})",
            delay_secs * 1000.0
        );
    }

    // Dattorro plate reverb (disable with REVERB=off) — on the anthem this only touches the
    // lead bus, bigger and wetter; the pad joins afterwards, dry.
    if std::env::var("REVERB").map(|v| v != "off").unwrap_or(true) {
        let mut reverb = PlateReverb::new(sr, 20.0);
        let wet = if is_anthem {
            reverb.decay = 0.85;
            reverb.damping = 0.3;
            0.38
        } else {
            reverb.decay = 0.8;
            reverb.damping = 0.35;
            0.22
        };
        reverb.process(&mut left, &mut right, wet);
        println!(
            "plate reverb: decay {}, wet {wet}, pre-delay 20 ms{}",
            reverb.decay,
            if is_anthem { " (lead bus only)" } else { "" }
        );
    }

    // Sum the dry pad bus back under the lead.
    if let Some((pl, pr)) = pad_bus {
        for (dst, src) in left.iter_mut().zip(pl.iter()) {
            *dst += src;
        }
        for (dst, src) in right.iter_mut().zip(pr.iter()) {
            *dst += src;
        }
    }

    // Four-on-the-floor kick + sidechain pump — off by default so the riff itself stays the
    // star; opt in with KICK=on. Ducks the synth bus as if keyed by the kick, then lays the
    // kick on top unducked.
    if std::env::var("KICK").map(|v| v == "on").unwrap_or(false) {
        let samples_per_beat = 60.0 / bpm * sr;
        let kicked = (((last_beat + 1.0) * samples_per_beat) as usize).min(left.len());
        dsp::sidechain_duck(&mut left[..kicked], &mut right[..kicked], sr, bpm, 0.55);
        let mut kick = dsp::Kick::new(sr as f32);
        let mut beat = 0.0;
        while beat <= last_beat {
            let pos = (beat * samples_per_beat) as usize;
            let end = (pos + (sr * 0.4) as usize).min(left.len());
            kick.trigger();
            kick.process(&mut left[pos..end], &mut right[pos..end], 0.65, 0.55, 0.4);
            beat += 1.0;
        }
        println!("kick + sidechain: 4-on-the-floor, duck depth 0.55");
    }

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
