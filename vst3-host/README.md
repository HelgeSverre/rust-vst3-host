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

- **Discovery** — scan standard locations; read plugin metadata; export a full report as JSON
  (`PluginReport`).
- **Audio** — a bundled CPAL backend drives a plugin to the default output (or bring your own
  backend), with two paths: the easy mutex-based `play` and a lock-free `play_realtime`
  (`RealtimePluginRunner`) that takes no lock on the audio thread.
- **Parameters** — list, read, set (applied to the audio processor), and format them as the
  plugin itself displays them.
- **MIDI** — send notes, CC, pitch bend, aftertouch; capture MIDI the plugin emits
  (`take_output_midi`).
- **State** — save/restore a plugin's state (`save_state`/`load_state`), in-process or isolated.
- **Crash isolation** — optionally run a plugin in a separate process (`process-isolation`),
  with typed `Error::PluginCrashed` + `Plugin::recover()`.
- **Native editors** — open a plugin's own GUI in a standalone window, or embed it in your
  egui app (`EmbeddedEditor`, macOS). Windows/Linux window code compiles but isn't yet
  runtime-verified.

## Feature flags

| Flag | Default | Enables |
| --- | --- | --- |
| `cpal-backend` | yes | The bundled CPAL backend and `simple::play` / `Vst3Host::play`. |
| `process-isolation` | yes | Out-of-process hosting + the `vst3-host-helper` binary. |
| `egui-widgets` | no | `EmbeddedEditor` — embed a plugin editor in an egui/eframe window (macOS). |

```toml
[dependencies]
vst3-host = "0.1"
```

## Status & caveats

The core is working and exercised against real plugins on macOS. Honest limits:

- The default `play` audio path is correctness-first (it locks on the audio callback). For a
  lock-free path use `play_realtime` / `RealtimePluginRunner`; even that isn't a fully
  RT-audited (zero-allocation) engine yet.
- Process isolation is opt-in (`Vst3Host::builder().with_process_isolation(true)`); crashes
  surface as `Error::PluginCrashed` and `Plugin::recover()` reloads, but there is no
  GUI-across-the-boundary yet.
- `MidiEvent::ProgramChange` is unsupported (VST3 routes programs through `IUnitInfo`).
- Windows/Linux build and test in CI but aren't interactively exercised (no plugin run or
  editor opened) — macOS is the exercised platform.
- Not yet published to crates.io: building depends on the VST3 SDK via `VST3_SDK_DIR`
  (see [Building from source](#building-from-source)).

## Building from source

The `vst3` dependency generates bindings from the Steinberg VST3 SDK headers at build time,
so it needs the SDK and the `VST3_SDK_DIR` environment variable pointing at it (plus
`libclang` for bindgen).

**In this repository** the SDK is the `vst3sdk` git submodule and `.cargo/config.toml`
already sets `VST3_SDK_DIR`, so no extra setup is needed:

```bash
git submodule update --init --recursive
cargo build --release
```

**As a dependency in your own project**, you must provide the SDK yourself — clone the
[VST3 SDK](https://github.com/steinbergmedia/vst3sdk) and set `VST3_SDK_DIR` to its path
before building (e.g. in your crate's `.cargo/config.toml` or the environment). This is why
the crate isn't on crates.io yet; the Steinberg SDK's license prevents bundling its headers.

## Documentation

Full guides (Diátaxis: tutorials, how-to, reference, explanation) are in the
[`docs/`](https://github.com/HelgeSverre/rust-vst3-host/tree/main/docs) directory of the
repository. API reference is on [docs.rs](https://docs.rs/vst3-host).

## License

MIT OR Apache-2.0
