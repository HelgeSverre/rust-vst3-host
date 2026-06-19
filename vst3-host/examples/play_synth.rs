//! Load a VST3 instrument and play a note through the default audio device.
//!
//! This is the "batteries-included" happy path end to end:
//!
//! ```text
//! cargo run --example play_synth --features cpal-backend -- /path/to/synth.vst3
//! ```
//!
//! Pass a `.vst3` path, or run with no args to play the first discovered plugin.

use std::time::Duration;
use vst3_host::{midi::MidiChannel, simple};

fn main() -> vst3_host::Result<()> {
    env_logger::init();

    let path = std::env::args().nth(1);

    let plugin = match path {
        Some(p) => {
            println!("Loading {p}");
            simple::load_plugin(&p)?
        }
        None => {
            println!("No path given; discovering plugins...");
            let mut found = simple::discover_plugins()?;
            let info = found
                .drain(..)
                .next()
                .ok_or_else(|| vst3_host::Error::Other("No VST3 plugins found".into()))?;
            println!("Playing first discovered: {} by {}", info.name, info.vendor);
            simple::load_plugin(&info.path)?
        }
    };

    println!(
        "Loaded: {} ({} in / {} out, midi_in: {})",
        plugin.info().name,
        plugin.info().audio_inputs,
        plugin.info().audio_outputs,
        plugin.info().has_midi_input,
    );

    // Start streaming audio to the default output device.
    let audio = simple::play(plugin)?;
    println!("Audio stream started. Sending middle C...");

    // Play a short C-major arpeggio so an instrument makes audible sound, sampling
    // the output meters *while* each note rings (peak decays once notes stop).
    let mut max_peak = 0.0f32;
    for note in [60u8, 64, 67, 72] {
        audio.lock().send_midi_note(note, 110, MidiChannel::Ch1)?;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(25));
            let levels = audio.lock().get_output_levels();
            for c in &levels.channels {
                max_peak = max_peak.max(c.peak);
            }
        }
        audio.lock().send_midi_note_off(note, MidiChannel::Ch1)?;
    }

    println!("Done. Max output peak while playing: {max_peak:.4}");
    if max_peak > 0.0 {
        println!("✅ Plugin produced audio through the CPAL backend.");
    } else {
        println!("⚠️  No output observed (effect fed silence, or no active output device).");
    }

    Ok(())
}
