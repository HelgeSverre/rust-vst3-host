# vst3-host

A safe Rust library for hosting VST3 plugins: discover them, load them, play audio
through them, control parameters, send MIDI, and isolate crashes — without writing any
`unsafe` code yourself.

```rust
use vst3_host::{simple, midi::MidiChannel};

# fn main() -> vst3_host::Result<()> {
// Load a synth and start it playing through the default audio device.
let plugin = simple::load_plugin("/Library/Audio/Plug-Ins/VST3/Dexed.vst3")?;
let audio = simple::play(plugin)?;

// Play middle C for one second.
audio.lock().send_midi_note(60, 100, MidiChannel::Ch1)?;
std::thread::sleep(std::time::Duration::from_secs(1));
# Ok(())
# }
```

## What it does

- **Discovery** — find installed VST3 plugins and read their metadata.
- **Audio** — a bundled CPAL backend drives a plugin to your speakers; or plug in your own backend.
- **Parameters** — list, read, set, and format parameters as the plugin itself displays them.
- **MIDI** — notes, control changes, pitch bend, and aftertouch.
- **Crash isolation** — run a plugin in a separate process so a crash can't take down your app.
- **Native plugin editors** — open a plugin's own GUI in a window (macOS/Windows).

All VST3 COM interaction is contained behind a safe API. The public surface has no `unsafe`.

## Status

The core is working and exercised against real plugins on macOS. Some things are still in
progress — see [Platform support](docs/reference/platform-support.md) and the honest caveats
in each guide. Notably: the audio path is correctness-first (not yet tuned for the lowest
latency), process isolation is opt-in, and plugin-editor embedding into egui is planned.

## Install

```toml
[dependencies]
vst3-host = "0.1"
```

Building from source requires the VST3 SDK (included as a submodule):

```bash
git clone --recursive https://github.com/HelgeSverre/rust-vst3-host.git
cd rust-vst3-host
cargo build --release
```

## Documentation

Start with the [documentation index](docs/README.md). It's organized by what you're doing:

- **[Getting started](docs/tutorials/getting-started.md)** — load and hear a plugin, step by step.
- **[How-to guides](docs/how-to/)** — focused recipes: discover, play, parameters, MIDI, isolation.
- **[Reference](docs/reference/)** — feature flags, platform support, API map. Full API on [docs.rs](https://docs.rs/vst3-host).
- **[Explanation](docs/explanation/)** — how the library is built and why.

## Workspace layout

- `vst3-host/` — the library.
- `vst3-inspector/` — a GUI app (egui) built entirely on the library's public API; a worked example of consuming it.

Common tasks are wrapped in a [`justfile`](justfile): `just build`, `just test`, `just play`, `just lint`.

## License

MIT OR Apache-2.0
