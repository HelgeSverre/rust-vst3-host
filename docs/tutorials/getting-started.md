# Getting started

By the end of this tutorial you'll have a Rust program that loads a VST3 synth and plays
a note through your speakers.

## Prerequisites

- Rust (stable) and a working audio output device.
- One VST3 instrument installed. [Dexed](https://asb2m10.github.io/dexed/) is a good free
  FM synth; on macOS it installs to `/Library/Audio/Plug-Ins/VST3/Dexed.vst3`.

## 1. Create a project and add the dependency

```bash
cargo new vst3-demo
cd vst3-demo
cargo add vst3-host
```

## 2. Load and play

Replace `src/main.rs` with:

```rust
use std::time::Duration;
use vst3_host::{midi::MidiChannel, simple};

fn main() -> vst3_host::Result<()> {
    // Adjust this path to a synth you have installed.
    let plugin = simple::load_plugin("/Library/Audio/Plug-Ins/VST3/Dexed.vst3")?;
    println!("Loaded: {}", plugin.info().name);

    // Open the default audio output and start the plugin running.
    let audio = simple::play(plugin)?;

    // Play a C-major chord for one second.
    for note in [60, 64, 67] {
        audio.lock().send_midi_note(note, 100, MidiChannel::Ch1)?;
    }
    std::thread::sleep(Duration::from_secs(1));
    for note in [60, 64, 67] {
        audio.lock().send_midi_note_off(note, MidiChannel::Ch1)?;
    }

    Ok(())
}
```

## 3. Run it

```bash
cargo run
```

You should hear a chord. The program prints the plugin name, opens an audio stream, plays
for a second, and exits.

## What just happened

- `simple::load_plugin` loaded the plugin in-process and returned a [`Plugin`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html).
- `simple::play` opened the default output device (via the bundled CPAL backend), started
  the plugin processing, and returned an [`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html).
  The handle owns the running audio stream — when it's dropped, audio stops.
- `audio.lock()` gives you the running plugin so you can keep controlling it (send MIDI,
  change parameters) while it plays.
- MIDI note `60` is middle C. See [Send MIDI](../how-to/send-midi.md) for the rest.

> The note convention is C3 = 60. `vst3_host::midi::note_to_name(60)` returns `"C3"`.

## Next steps

- [Build a mini host](build-a-mini-host.md) — list parameters and read output levels.
- [Control parameters](../how-to/control-parameters.md) — change a plugin's cutoff, etc.
- Don't have a plugin path handy? [Discover installed plugins](../how-to/discover-plugins.md).
