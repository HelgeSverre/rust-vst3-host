# vst3-host

[![Crates.io](https://img.shields.io/crates/v/vst3-host.svg)](https://crates.io/crates/vst3-host)
[![Docs.rs](https://docs.rs/vst3-host/badge.svg)](https://docs.rs/vst3-host)
[![CI](https://github.com/HelgeSverre/rust-vst3-host/actions/workflows/ci.yml/badge.svg)](https://github.com/HelgeSverre/rust-vst3-host/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/vst3-host.svg)](#license)

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
- **Audio** — a bundled CPAL backend drives a plugin to your speakers; or plug in your own
  backend. Host effects on live audio input, or render offline to a WAV file.
- **Parameters** — list, read, set, and format parameters as the plugin itself displays them,
  with sample-accurate automation (`set_parameter_at`).
- **MIDI** — notes, control changes, pitch bend, and aftertouch, with sample-accurate
  scheduling; per-note expression / MPE (`note_on` / `send_note_expression`, in-process).
- **State & presets** — save/restore a plugin's own state, and read/write `.vstpreset` files.
- **Metering** — peak/RMS levels with ready-made UI ballistics (`PeakMeter`, `RmsWindow`).
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
vst3-host = "0.4"
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

MIT — see [LICENSE](LICENSE).
