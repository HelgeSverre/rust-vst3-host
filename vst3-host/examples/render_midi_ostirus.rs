//! Render a Standard MIDI File through OsTIrus to a 32-bit float WAV.
//!
//! Defaults are tailored to the licensed Aramanja MIDI in `examples/assets`: the Commerce SV
//! patch (MIDI program 81), two back-to-back passes, and the main stereo output.
//!
//! ```text
//! cargo run -p vst3-host --example render_midi_ostirus --release -- \
//!   [midi-path] [plugin-path] [output-path] [repeat-count]
//! ```
//!
//! The bundled MIDI is embedded in the executable. Use `-` for `midi-path` to keep it
//! while supplying other arguments. OsTIrus is found in standard VST3 locations, or
//! can be selected with `plugin-path` or `VST3_PLUGIN`. Output defaults to the current
//! directory. Install OsTIrus and its required ROM separately.

use std::{env, fs, path::Path};

use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use vst3_host::{
    audio::{write_wav, AudioBuffers},
    midi::{MidiChannel, MidiEvent},
    transport::{MidiClip, Timeline},
    Error, Vst3Host,
};

const DEFAULT_MIDI: &[u8] = include_bytes!("assets/aramanja-memories.mid");
const DEFAULT_OUTPUT: &str = "aramanja-memories-ostirus-commerce-sv-2x.wav";
const COMMERCE_SV_PROGRAM: u8 = 81;
const DEFAULT_REPEATS: usize = 2;
const SAMPLE_RATE: f64 = 48_000.0;
const BLOCK_SIZE: usize = 512;
const TAIL_SECONDS: f64 = 2.0;

#[derive(Debug)]
struct MidiSequence {
    bpm: f64,
    length_beats: f64,
    events: Vec<(f64, MidiEvent)>,
    note_ons: usize,
}

fn main() -> vst3_host::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let midi_bytes = match args.first().map(String::as_str) {
        None | Some("-") => DEFAULT_MIDI.to_vec(),
        Some(path) => {
            fs::read(path).map_err(|error| Error::Other(format!("read MIDI {path}: {error}")))?
        }
    };
    let output_path = args.get(2).map(String::as_str).unwrap_or(DEFAULT_OUTPUT);
    let repeats = args
        .get(3)
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|error| Error::InvalidParameter(format!("invalid repeat count: {error}")))?
        .unwrap_or(DEFAULT_REPEATS);
    if !(1..=16).contains(&repeats) {
        return Err(Error::InvalidParameter(format!(
            "repeat count must be between 1 and 16, got {repeats}"
        )));
    }

    let sequence = load_midi(&midi_bytes)?;
    println!(
        "MIDI: {} note-ons, {:.3} beats at {:.3} BPM",
        sequence.note_ons, sequence.length_beats, sequence.bpm
    );
    println!("patch: Commerce SV (MIDI program {COMMERCE_SV_PROGRAM})");
    println!("arrangement: {repeats} back-to-back passes");

    let mut host = Vst3Host::builder()
        .sample_rate(SAMPLE_RATE)
        .block_size(BLOCK_SIZE)
        .build()?;
    let plugin_path = args
        .get(1)
        .map(std::path::PathBuf::from)
        .or_else(|| env::var_os("VST3_PLUGIN").map(std::path::PathBuf::from))
        .or_else(|| {
            vst3_host::discovery::scan_directories(
                &vst3_host::discovery::scan_standard_paths(),
            )
                .ok()?
                .into_iter()
                .find(|path| path.file_stem().is_some_and(|name| name.eq_ignore_ascii_case("OsTIrus")))
        })
        .ok_or_else(|| Error::PluginNotFound(
            "OsTIrus: install it in a standard VST3 location, pass a plugin path, or set VST3_PLUGIN".into()
        ))?;
    let mut plugin = host.load_plugin(&plugin_path)?;
    println!("loaded: {} by {}", plugin.info().name, plugin.info().vendor);

    plugin.set_tempo(sequence.bpm)?;
    plugin.set_playing(true)?;

    let mut clip = MidiClip::new();
    clip.add(
        0.0,
        MidiEvent::ProgramChange {
            channel: MidiChannel::Ch1,
            program: COMMERCE_SV_PROGRAM,
        },
    );
    for repeat in 0..repeats {
        let beat_offset = repeat as f64 * sequence.length_beats;
        for &(beat, event) in &sequence.events {
            // Program selection is explicit above. Re-sending it at the loop point can reset
            // voices and produce a discontinuity, so embedded program changes are omitted.
            if matches!(event, MidiEvent::ProgramChange { .. }) {
                continue;
            }
            clip.add(beat_offset + beat, event);
        }
    }

    let mut timeline = Timeline::new(SAMPLE_RATE, sequence.bpm).with_clip(clip);
    let arrangement_beats = sequence.length_beats * repeats as f64;
    let arrangement_seconds = arrangement_beats * 60.0 / sequence.bpm;
    let total_seconds = arrangement_seconds + TAIL_SECONDS;
    let total_frames = (total_seconds * SAMPLE_RATE).ceil() as usize;
    let output_channels = plugin.output_channel_count().max(2);
    let mut left = Vec::with_capacity(total_frames);
    let mut right = Vec::with_capacity(total_frames);

    plugin.start_processing()?;
    let render_result = render(
        &mut plugin,
        &mut timeline,
        output_channels,
        total_frames,
        &mut left,
        &mut right,
    );
    let stop_result = plugin.stop_processing();
    render_result?;
    stop_result?;

    if left.iter().chain(&right).any(|sample| !sample.is_finite()) {
        return Err(Error::Other(
            "OsTIrus produced non-finite audio samples".to_string(),
        ));
    }
    let peak = left
        .iter()
        .chain(&right)
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    if peak > 0.98 {
        let gain = 0.98 / peak;
        left.iter_mut()
            .chain(&mut right)
            .for_each(|sample| *sample *= gain);
        println!("peak-safe gain: {gain:.4} ({peak:.3} → 0.980)");
    }

    write_wav(output_path, &[left, right], SAMPLE_RATE as u32)?;
    println!("rendered: {total_seconds:.3}s");
    println!("wrote: {}", Path::new(output_path).display());
    Ok(())
}

fn load_midi(bytes: &[u8]) -> vst3_host::Result<MidiSequence> {
    let smf = Smf::parse(bytes).map_err(|error| Error::Other(format!("parse MIDI: {error}")))?;
    let ticks_per_beat = match smf.header.timing {
        Timing::Metrical(value) => f64::from(value.as_int()),
        Timing::Timecode(_, _) => {
            return Err(Error::InvalidParameter(
                "SMPTE-timecode MIDI is not supported by this beat timeline".to_string(),
            ));
        }
    };

    let mut tempos = Vec::new();
    let mut events = Vec::new();
    let mut end_tick = 0_u64;
    let mut note_ons = 0_usize;
    for track in &smf.tracks {
        let mut tick = 0_u64;
        for event in track {
            tick += u64::from(event.delta.as_int());
            end_tick = end_tick.max(tick);
            match event.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(micros_per_beat)) => {
                    tempos.push((tick, micros_per_beat.as_int()));
                }
                TrackEventKind::Midi { channel, message } => {
                    let Some(channel) = MidiChannel::from_index(channel.as_int()) else {
                        continue;
                    };
                    if let Some(midi) = convert_message(channel, message) {
                        if matches!(midi, MidiEvent::NoteOn { .. }) {
                            note_ons += 1;
                        }
                        events.push((tick as f64 / ticks_per_beat, midi));
                    }
                }
                _ => {}
            }
        }
    }
    events.sort_by(|left, right| left.0.total_cmp(&right.0));
    tempos.sort_by_key(|(tick, _)| *tick);

    let micros_per_beat = tempos
        .iter()
        .take_while(|(tick, _)| *tick == 0)
        .last()
        .map(|(_, value)| *value)
        .unwrap_or(500_000);
    if ticks_per_beat == 0.0 || micros_per_beat == 0 {
        return Err(Error::InvalidParameter(
            "MIDI timing and tempo must be nonzero".into(),
        ));
    }
    if tempos.iter().any(|(_, value)| *value != micros_per_beat) {
        return Err(Error::InvalidParameter(
            "tempo-changing MIDI is not supported by this constant-tempo renderer".to_string(),
        ));
    }
    let bpm = 60_000_000.0 / f64::from(micros_per_beat);

    Ok(MidiSequence {
        bpm,
        length_beats: end_tick as f64 / ticks_per_beat,
        events,
        note_ons,
    })
}

fn convert_message(channel: MidiChannel, message: MidiMessage) -> Option<MidiEvent> {
    match message {
        MidiMessage::NoteOff { key, vel } => Some(MidiEvent::NoteOff {
            channel,
            note: key.as_int(),
            velocity: vel.as_int(),
        }),
        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => Some(MidiEvent::NoteOn {
            channel,
            note: key.as_int(),
            velocity: vel.as_int(),
        }),
        MidiMessage::NoteOn { key, .. } => Some(MidiEvent::NoteOff {
            channel,
            note: key.as_int(),
            velocity: 0,
        }),
        MidiMessage::Aftertouch { key, vel } => Some(MidiEvent::PolyAftertouch {
            channel,
            note: key.as_int(),
            pressure: vel.as_int(),
        }),
        MidiMessage::Controller { controller, value } => Some(MidiEvent::ControlChange {
            channel,
            controller: controller.as_int(),
            value: value.as_int(),
        }),
        MidiMessage::ProgramChange { program } => Some(MidiEvent::ProgramChange {
            channel,
            program: program.as_int(),
        }),
        MidiMessage::ChannelAftertouch { vel } => Some(MidiEvent::ChannelAftertouch {
            channel,
            pressure: vel.as_int(),
        }),
        MidiMessage::PitchBend { bend } => Some(MidiEvent::PitchBend {
            channel,
            value: bend.0.as_int(),
        }),
    }
}

fn render(
    plugin: &mut vst3_host::Plugin,
    timeline: &mut Timeline,
    output_channels: usize,
    total_frames: usize,
    left: &mut Vec<f32>,
    right: &mut Vec<f32>,
) -> vst3_host::Result<()> {
    let mut rendered = 0_usize;
    while rendered < total_frames {
        let frames = BLOCK_SIZE.min(total_frames - rendered);
        let mut buffers = AudioBuffers::new(0, output_channels, frames, SAMPLE_RATE);
        timeline.drive_block(plugin, &mut buffers)?;

        let main_left = buffers
            .outputs
            .first()
            .ok_or_else(|| Error::Other("OsTIrus exposed no main output".to_string()))?;
        let main_right = buffers.outputs.get(1).unwrap_or(main_left);
        left.extend_from_slice(main_left);
        right.extend_from_slice(main_right);
        rendered += frames;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_midi_parses_without_a_runtime_file() {
        let sequence = load_midi(DEFAULT_MIDI).expect("embedded MIDI");
        assert!(sequence.note_ons > 0);
        assert!(sequence.length_beats > 0.0);
        assert!(sequence.bpm.is_finite() && sequence.bpm > 0.0);
    }

    fn tempo_midi(delta: u8, tempo: [u8; 3]) -> Vec<u8> {
        let mut bytes = b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk\0\0\0\x0b".to_vec();
        bytes.extend_from_slice(&[delta, 0xff, 0x51, 3]);
        bytes.extend_from_slice(&tempo);
        bytes.extend_from_slice(&[0, 0xff, 0x2f, 0]);
        bytes
    }

    #[test]
    fn rejects_a_tempo_change_after_the_default_initial_tempo() {
        assert!(load_midi(&tempo_midi(96, [0x06, 0x1a, 0x80])).is_err());
        assert_eq!(
            load_midi(&tempo_midi(0, [0x06, 0x1a, 0x80])).unwrap().bpm,
            150.0
        );
    }

    #[test]
    fn rejects_zero_tempo() {
        assert!(load_midi(&tempo_midi(0, [0, 0, 0])).is_err());
    }
}
