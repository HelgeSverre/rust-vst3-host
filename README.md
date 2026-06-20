# vst3-host

A safe Rust library for hosting VST3 plugins: discover them, load them, play audio
through them, control parameters, send MIDI, and isolate crashes — without writing any
`unsafe` code yourself.

```rust
use vst3_host::{simple, midi::MidiChannel};

fn main() -> vst3_host::Result<()> {
    // Load a synth and start it playing through the default audio device.
    let plugin = simple::load_plugin("/Library/Audio/Plug-Ins/VST3/Dexed.vst3")?;
    let audio = simple::play(plugin)?;

    // Play middle C (MIDI note 60) for one second.
    audio.lock().send_midi_note(60, 100, MidiChannel::Ch1)?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    Ok(())
}
```

## What it does

- **Discovery** — find installed VST3 plugins and read their metadata.
- **Audio** — a bundled CPAL backend drives a plugin to your speakers; or plug in your own backend.
- **Parameters** — list, read, set, and format parameters as the plugin itself displays them.
- **MIDI** — notes, control changes, pitch bend, and aftertouch.
- **State** — save and restore a plugin's own state (`save_state` / `load_state`).
- **Crash isolation** — run a plugin in a separate process so a crash can't take down your
  app; crashes surface as `Error::PluginCrashed` and `Plugin::recover()` reloads it.
- **Native plugin editors** — open a plugin's own GUI in a standalone window, or embed it in
  your egui app (`EmbeddedEditor`, macOS).

All VST3 COM interaction is contained behind a safe API. The public surface has no `unsafe`.

## Status

The core is working and exercised against real plugins on macOS. Known limitations — see
[Platform support](docs/reference/platform-support.md) and each guide: the default audio path
is correctness-first (a lock-free `play_realtime` path also exists, though neither is a fully
RT-audited engine yet), process isolation is opt-in, and editor embedding into egui
(`EmbeddedEditor`) works on macOS — the Windows/Linux window code compiles but isn't yet
runtime-verified.

## Install

```toml
[dependencies]
vst3-host = "0.1"
```

No VST3 SDK or extra setup is required — the `vst3` dependency ships pre-generated bindings.
Building from source is just:

```bash
git clone https://github.com/HelgeSverre/rust-vst3-host.git
cd rust-vst3-host
cargo build --release
```

(`libclang` must be installed for `cpal`'s bindgen-based audio deps on macOS/Linux.)

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
