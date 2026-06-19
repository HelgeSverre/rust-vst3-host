# vst3-host

A safe Rust library for hosting VST3 plugins: discover them, load them, play audio through
them, control parameters, send MIDI, and isolate crashes — without writing any `unsafe`
code yourself. All VST3 COM interaction is contained behind a safe API.

```rust
use vst3_host::{simple, midi::MidiChannel};

fn main() -> vst3_host::Result<()> {
    // Load a synth and start it playing through the default audio device.
    let plugin = simple::load_plugin("/Library/Audio/Plug-Ins/VST3/Dexed.vst3")?;
    let audio = simple::play(plugin)?;

    // Play middle C for one second.
    audio.lock().send_midi_note(60, 100, MidiChannel::Ch1)?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    Ok(())
}
```

## Features

- **Discovery** — scan standard locations; read plugin metadata.
- **Audio** — a bundled CPAL backend drives a plugin to the default output, or bring your
  own backend.
- **Parameters** — list, read, set, and format them as the plugin itself displays them.
- **MIDI** — notes, control change, pitch bend, aftertouch.
- **Crash isolation** — optionally run a plugin in a separate process (`process-isolation`).
- **Native editors** — open a plugin's own GUI in a window (macOS/Windows).

## Feature flags

| Flag | Default | Enables |
| --- | --- | --- |
| `cpal-backend` | yes | The bundled CPAL backend and `simple::play` / `Vst3Host::play`. |
| `process-isolation` | yes | Out-of-process hosting + the `vst3-host-helper` binary. |
| `egui-widgets` | no | egui helpers (planned — not yet a usable widget). |

```toml
[dependencies]
vst3-host = "0.1"
```

## Status & caveats

The core is working and exercised against real plugins on macOS. Honest limits:

- The audio path is correctness-first — not yet tuned for the lowest latency (it locks on
  the audio callback).
- Process isolation is opt-in (`Vst3Host::builder().with_process_isolation(true)`); it has
  no GUI-across-the-boundary and no auto-respawn yet.
- `MidiEvent::ProgramChange` is unsupported (VST3 routes programs through `IUnitInfo`).
- Windows/Linux are implemented but less exercised than macOS.

## Building from source

Requires the VST3 SDK (a git submodule in the repository):

```bash
git submodule update --init --recursive
cargo build --release
```

## Documentation

Full guides (Diátaxis: tutorials, how-to, reference, explanation) are in the
[`docs/`](https://github.com/HelgeSverre/rust-vst3-host/tree/main/docs) directory of the
repository. API reference is on [docs.rs](https://docs.rs/vst3-host).

## License

MIT OR Apache-2.0
