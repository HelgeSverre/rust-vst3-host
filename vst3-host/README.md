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

    // Play middle C (MIDI note 60) for one second.
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

## Examples

**Discover and inspect installed plugins.** `discover_plugins` scans the standard VST3
locations and returns metadata without loading any DSP:

```rust
use vst3_host::simple;

fn main() -> vst3_host::Result<()> {
    for info in simple::discover_plugins()? {
        println!(
            "{} v{} — {} | {} audio out | MIDI in: {} | GUI: {}",
            info.name, info.version, info.category,
            info.audio_outputs, info.has_midi_input, info.has_gui,
        );
    }
    Ok(())
}
```

**Read, format, and set parameters.** Parameters are normalized to `0.0..=1.0`;
`format_parameter` renders the plugin's own display string (e.g. `"8.2 kHz"`):

```rust
use vst3_host::simple;

fn main() -> vst3_host::Result<()> {
    let mut plugin = simple::load_plugin("/Library/Audio/Plug-Ins/VST3/Dexed.vst3")?;

    for p in plugin.get_parameters()?.iter().take(5) {
        println!("{:>4}  {:<24} {}", p.id, p.name, plugin.format_parameter(p.id, p.value)?);
    }

    // Set the first parameter to 75% and read it back.
    let id = plugin.get_parameters()?[0].id;
    plugin.set_parameter(id, 0.75)?;
    assert_eq!(plugin.get_parameter(id)?, 0.75);
    Ok(())
}
```

**Save and restore a plugin's state** (its own serialized preset/patch blob):

```rust
use vst3_host::simple;

fn main() -> vst3_host::Result<()> {
    let mut plugin = simple::load_plugin("/path/synth.vst3")?;

    let snapshot: Vec<u8> = plugin.save_state()?;   // serialize current state
    plugin.set_parameter(0, 0.1)?;                  // change something...
    plugin.load_state(&snapshot)?;                  // ...and restore it exactly
    Ok(())
}
```

**Contain crashes with process isolation.** Run the plugin in a child process; a crash
surfaces as `Error::PluginCrashed` instead of killing your app, and `recover()` respawns
and reloads it:

```rust
use vst3_host::{Vst3Host, Error, midi::MidiChannel};

fn main() -> vst3_host::Result<()> {
    let mut host = Vst3Host::builder().with_process_isolation(true).build()?;
    let mut plugin = host.load_plugin("/path/sketchy.vst3")?;

    if let Err(Error::PluginCrashed) = plugin.send_midi_note(60, 100, MidiChannel::Ch1) {
        plugin.recover()?; // respawn helper + reload + restart processing
    }
    Ok(())
}
```

**Real-time, lock-free playback.** `play_realtime` hands the plugin to the audio thread and
sends control over a lock-free SPSC ring, so the callback never blocks on your thread:

```rust
use vst3_host::{Vst3Host, midi::{MidiEvent, MidiChannel}};

fn main() -> vst3_host::Result<()> {
    let mut host = Vst3Host::new()?;
    let plugin = host.load_plugin("/path/synth.vst3")?;

    let mut audio = host.play_realtime(plugin, 1024)?; // 1024 = command-queue capacity
    audio.control().send_midi(MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 100,
    });
    std::thread::sleep(std::time::Duration::from_secs(1));
    Ok(())
}
```

### Putting it all together

A complete offline workflow: configure an isolated host, discover an instrument, tweak a
parameter, snapshot its state, render a held note while measuring the peak, recover if the
plugin's process crashes, capture any emitted MIDI, then restore the original state.

```rust
use vst3_host::{audio::AudioBuffers, midi::MidiChannel, Error, Vst3Host};

fn main() -> vst3_host::Result<()> {
    // 1. Configure the host: 48 kHz, 512-sample blocks, and run the plugin in its own
    //    process so a crash can't take the whole app down.
    let mut host = Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(512)
        .with_process_isolation(true)
        .build()?;

    // 2. Discover what's installed and pick the first instrument.
    let info = host
        .discover_plugins()?
        .into_iter()
        .find(|p| p.has_midi_input && p.audio_outputs > 0)
        .ok_or_else(|| Error::Other("no instrument found".into()))?;
    println!("Loading {} v{} by {}", info.name, info.version, info.vendor);

    // 3. Load it (in the isolated helper process).
    let mut plugin = host.load_plugin(&info.path)?;

    // 4. Tweak the first automatable parameter, shown the way the plugin displays it.
    if let Some(p) = plugin.get_parameters()?.into_iter().find(|p| p.can_automate) {
        plugin.set_parameter(p.id, 0.6)?;
        println!("set {} -> {}", p.name, plugin.format_parameter(p.id, 0.6)?);
    }

    // 5. Snapshot the state so we can restore it later.
    let preset = plugin.save_state()?;

    // 6. Render one second of a held note offline, measuring the peak. If the plugin
    //    crashes its process, recover and reload instead of crashing ourselves.
    plugin.start_processing()?;
    plugin.send_midi_note(60, 100, MidiChannel::Ch1)?;

    let mut buffer = AudioBuffers::new(0, 2, 512, 48_000.0);
    let mut peak = 0.0f32;
    for _ in 0..(48_000 / 512) {
        match plugin.process_audio(&mut buffer) {
            Ok(()) => {
                for ch in &buffer.outputs {
                    peak = peak.max(ch.iter().fold(0.0, |m, &s| m.max(s.abs())));
                }
            }
            Err(Error::PluginCrashed) => {
                eprintln!("plugin crashed - recovering");
                plugin.recover()?;
                plugin.load_state(&preset)?;
                break;
            }
            Err(e) => return Err(e),
        }
    }
    plugin.stop_processing()?;
    println!("rendered peak amplitude: {peak:.3}");

    // 7. Capture any MIDI the plugin emitted (arpeggiators, MPE, ...).
    let emitted = plugin.take_output_midi();
    println!("plugin emitted {} MIDI event(s)", emitted.len());

    // 8. Restore the original state.
    plugin.load_state(&preset)?;
    Ok(())
}
```

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

## Status & known limitations

The core is working and exercised against real plugins on macOS. What to be aware of:

- The default `play` audio path is correctness-first (it locks on the audio callback). For a
  lock-free path use `play_realtime` / `RealtimePluginRunner`; even that isn't a fully
  RT-audited (zero-allocation) engine yet.
- Process isolation is opt-in (`Vst3Host::builder().with_process_isolation(true)`); crashes
  surface as `Error::PluginCrashed` and `Plugin::recover()` reloads, but there is no
  GUI-across-the-boundary yet.
- `MidiEvent::ProgramChange` is unsupported (VST3 routes programs through `IUnitInfo`).
- Windows/Linux build and test in CI but aren't interactively exercised (no plugin run or
  editor opened) — macOS is the exercised platform.

## Building from source

No VST3 SDK or extra setup is required. The `vst3` dependency (0.3) ships pre-generated
bindings, so building is just:

```bash
cargo build --release
```

The only build-time native dependency is `libclang`, used by `cpal`'s `coreaudio-sys`
(macOS) and `alsa-sys` (Linux); on Linux you also need the ALSA and libxcb dev headers
(`libasound2-dev`, `libxcb1-dev`, `libxcb-util-dev`).

## Documentation

Full guides (Diátaxis: tutorials, how-to, reference, explanation) are in the
[`docs/`](https://github.com/HelgeSverre/rust-vst3-host/tree/main/docs) directory of the
repository. API reference is on [docs.rs](https://docs.rs/vst3-host).

## License

MIT OR Apache-2.0
