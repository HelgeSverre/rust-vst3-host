//! Load and drive a VST3 plugin in a separate (isolated) process.
//!
//! ```text
//! cargo build --bin vst3-host-helper --features process-isolation
//! cargo run --example isolated_host --features process-isolation -- test_plugins/Dexed.vst3
//! ```
//!
//! A crash in the plugin takes down only the helper process, not this one.

use vst3_host::{midi::MidiChannel, AudioBuffers, Vst3Host};

fn main() -> vst3_host::Result<()> {
    env_logger::init();

    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "test_plugins/Dexed.vst3".to_string());

    println!("Loading {path} in an ISOLATED process...");
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .with_process_isolation(true)
        .build()?;

    let mut plugin = host.load_plugin(&path)?;
    let info = plugin.info().clone();
    println!(
        "Loaded (isolated): {} by {} ({} out, midi_in: {})",
        info.name, info.vendor, info.audio_outputs, info.has_midi_input
    );

    // Parameter round-trip across the process boundary.
    let params = plugin.get_parameters()?;
    println!("Parameters reported across IPC: {}", params.len());
    if let Some(p) = params.first() {
        plugin.set_parameter(p.id, 0.5)?;
        let got = plugin.get_parameter(p.id)?;
        let formatted = plugin
            .format_parameter(p.id, got)
            .unwrap_or_else(|_| "<n/a>".into());
        println!("  set {} = 0.5 -> read back {got:.3} ({formatted})", p.name);
    }

    // Start processing and feed a note, pulling audio blocks across IPC.
    plugin.start_processing()?;
    plugin.send_midi_note(60, 110, MidiChannel::Ch1)?;

    let mut max_peak = 0.0f32;
    for _ in 0..20 {
        let mut buffers = AudioBuffers::new(0, 2, 512, 48000.0);
        plugin.process_audio(&mut buffers)?;
        for ch in &buffers.outputs {
            for &s in ch {
                max_peak = max_peak.max(s.abs());
            }
        }
    }
    plugin.send_midi_note_off(60, MidiChannel::Ch1)?;
    plugin.stop_processing()?;

    println!("Max output peak from isolated plugin: {max_peak:.4}");
    if max_peak > 0.0 {
        println!("Isolated plugin produced audio across the process boundary.");
    } else {
        println!("No output (effect fed silence, or plugin produced none).");
    }

    Ok(())
}
