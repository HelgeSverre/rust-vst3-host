# Build a mini host

This tutorial builds on [Getting started](getting-started.md). You'll write a small
command-line host that discovers a plugin, prints its parameters, plays a short phrase,
and reports the output level it measured.

## Prerequisites

- You've completed [Getting started](getting-started.md).
- At least one VST3 instrument installed.

## The program

```rust
use std::time::Duration;
use vst3_host::{midi::MidiChannel, Vst3Host};

fn main() -> vst3_host::Result<()> {
    // A host configured for 48 kHz / 512-sample blocks.
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .scan_default_paths()
        .build()?;

    // Find a plugin (first discovered, or pass a path as the first argument).
    let path = match std::env::args().nth(1) {
        Some(p) => p.into(),
        None => host
            .discover_plugins()?
            .into_iter()
            .next()
            .ok_or_else(|| vst3_host::Error::Other("no plugins found".into()))?
            .path,
    };

    let plugin = host.load_plugin(&path)?;
    println!("Loaded {} ({} params)", plugin.info().name, plugin.get_parameters()?.len());

    // Print the first few parameters, formatted the way the plugin displays them.
    for p in plugin.get_parameters()?.iter().take(5) {
        let shown = plugin.format_parameter(p.id, p.value).unwrap_or_default();
        println!("  {:<24} {}", p.name, shown);
    }

    // Start audio and play an arpeggio, sampling the output meter while it rings.
    let audio = host.play(plugin)?;
    let mut peak = 0.0f32;
    for note in [60, 64, 67, 72] {
        audio.lock().send_midi_note(note, 110, MidiChannel::Ch1)?;
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(25));
            for ch in &audio.lock().get_output_levels().channels {
                peak = peak.max(ch.peak);
            }
        }
        audio.lock().send_midi_note_off(note, MidiChannel::Ch1)?;
    }

    println!("Peak output level while playing: {peak:.3}");
    Ok(())
}
```

Run it (optionally pass a plugin path):

```bash
cargo run -- "/Library/Audio/Plug-Ins/VST3/Dexed.vst3"
```

## What's new here

- **`Vst3Host::builder()`** configures sample rate and block size before loading. The
  `simple` functions use defaults (44.1 kHz, 512); the builder is the way to change them.
- **`discover_plugins()`** loads each plugin to read its metadata. For a fast path list
  that doesn't load anything, use `scan_plugin_paths()` instead (see
  [Discover installed plugins](../how-to/discover-plugins.md)).
- **`format_parameter(id, value)`** asks the plugin to render a value the way its own UI
  would (e.g. `"440.00 Hz"`, `"Sine"`), rather than the raw normalized `0.0–1.0` number.
- **`get_output_levels()`** returns per-channel peak/RMS, updated as the plugin processes.

## Next steps

- [Control parameters](../how-to/control-parameters.md) — set values, not just read them.
- [Isolate plugin crashes](../how-to/isolate-plugin-crashes.md) — load untrusted plugins safely.
- The full program above is essentially `vst3-host/examples/play_synth.rs` — run it with `just play`.
