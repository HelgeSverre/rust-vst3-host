//! End-to-end acceptance gate for the `transport` timeline: a `Timeline` driving scheduled MIDI
//! (a held note with a NoteOff) plus a parameter-automation lane (cutoff ramp) into a real
//! plugin, rendered offline block by block.
//!
//! Needs the bundled Dexed plugin, so it's `#[ignore]`d by default:
//!   cargo test -p vst3-host --test timeline_tests -- --ignored --nocapture

use vst3_host::{
    audio::AudioBuffers,
    midi::{MidiChannel, MidiEvent},
    parameters::{AutomationCurve, ParameterAutomation},
    transport::{AutomationLane, MidiClip, Timeline},
    Vst3Host,
};

fn block_rms(buf: &AudioBuffers) -> f64 {
    let sumsq: f64 = buf
        .outputs
        .iter()
        .flat_map(|c| c.iter())
        .map(|&s| (s as f64) * (s as f64))
        .sum();
    let n = (buf.block_size * buf.outputs.len().max(1)) as f64;
    (sumsq / n).sqrt()
}

#[test]
#[ignore = "Requires the bundled test plugin"]
fn timeline_renders_scheduled_sequence_offline() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(path).exists() {
        println!("Test plugin not found, skipping");
        return;
    }
    let sr = 48_000.0;
    let block = 512usize;
    let bpm = 120.0; // 1 beat = 0.5s; beat 2 = 1.0s = 48000 frames.

    let mut host = Vst3Host::builder()
        .sample_rate(sr)
        .block_size(block)
        .build()
        .unwrap();
    let mut plugin = host.load_plugin(path).unwrap();
    plugin.start_processing().unwrap();

    let cutoff = plugin
        .get_parameters()
        .unwrap()
        .iter()
        .find(|p| p.name.to_lowercase().contains("cutoff"))
        .map(|p| p.id)
        .expect("Dexed has a cutoff param");

    // A held note from beat 0 to beat 2 (1 second), and a cutoff ramp opening over those 2 beats.
    let clip = MidiClip::new()
        .with(
            0.0,
            MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 110,
            },
        )
        .with(
            2.0,
            MidiEvent::NoteOff {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 0,
            },
        );
    let lane = AutomationLane::new(
        cutoff,
        ParameterAutomation::new()
            .add_point(0.0, 0.05)
            .add_point(2.0, 0.95)
            .with_curve(AutomationCurve::Linear),
        8,
    );
    let mut timeline = Timeline::new(sr, bpm).with_clip(clip).with_lane(lane);

    // Render 2 seconds: the first ~1s holds the note (cutoff ramping open), the second ~1s is
    // after the NoteOff (release tail decaying).
    let blocks_per_sec = sr as usize / block; // ~93
    let total = blocks_per_sec * 2;

    let mut buf = AudioBuffers::new(0, 2, block, sr);
    let (mut held, mut held_n) = (0.0f64, 0u32); // while the note is sounding
    let (mut release, mut release_n) = (0.0f64, 0u32); // after the scheduled NoteOff

    for b in 0..total {
        timeline.drive_block(&mut plugin, &mut buf).unwrap();
        let rms = block_rms(&buf);
        // The note is held over the first second (NoteOff at beat 2 = frame 48000 ≈ block 94).
        // Sample the middle of the held region (clear of the attack and the release).
        if (10..blocks_per_sec - 10).contains(&b) {
            held += rms;
            held_n += 1;
        } else if b >= total - blocks_per_sec / 3 {
            release += rms;
            release_n += 1;
        }
    }
    plugin.stop_processing().ok();

    let held = held / held_n.max(1) as f64;
    let release = release / release_n.max(1) as f64;
    // The automation lane drove the cutoff from 0.05 toward 0.95 over beats 0..2; after the
    // render the playhead is at beat 4, past the curve's end, so it holds the final value.
    let final_cutoff = plugin.get_parameter(cutoff).unwrap();
    println!(
        "timeline render: held_rms={held:.6} release_rms={release:.6} final_cutoff={final_cutoff:.4}"
    );

    // (1) The clock advanced exactly `total` blocks worth of frames.
    assert_eq!(timeline.sample_clock(), (total * block) as u64);
    // (2) The scheduled NoteOn reached the plugin: the held window clearly sounds.
    assert!(
        held > 1e-3,
        "the scheduled note should sound: held_rms={held:.6}"
    );
    // (3) The scheduled NoteOff reached the plugin at the right time: the post-release window
    //     decays far below the held window (Dexed's FM release tails to near silence).
    assert!(
        release < held * 0.1,
        "the scheduled NoteOff should let the note decay: held={held:.6} release={release:.6}"
    );
    // (4) The automation lane reached the plugin: the cutoff parameter ended near the ramp's
    //     final value (0.95), not its start (0.05) — proven by reading the value, not inferred
    //     from amplitude (Dexed's cutoff→loudness response is weak and non-monotonic).
    assert!(
        final_cutoff > 0.8,
        "the automation lane should have driven cutoff toward 0.95: final={final_cutoff:.4}"
    );
}
