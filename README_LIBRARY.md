# vst3-host

A Rust library for hosting VST3 plugins with a safe API.

> **Status (in active development).** The in-process load path, parameters, MIDI,
> and discovery work today on macOS. The "batteries-included" audio output
> (`CpalBackend` → plugin), process isolation by default, lock-free audio, and the
> egui widgets are **still being built** — see [ROADMAP.md](ROADMAP.md). Bullets
> below marked _(planned)_ are not implemented yet.

## Features

- Safe API - No unsafe code required by library users
- Simple to use - Minimal boilerplate for common tasks
- Plugin discovery with metadata (working)
- Full MIDI support
- Parameter control and automation
- CPAL audio backend bundled — _(planned: automatic device wiring; today you drive `process_audio` yourself)_
- Process isolation for plugin crash protection — _(opt-in; load/unload work, full control protocol planned)_
- Real-time audio level monitoring — _(populated while a stream drives the plugin)_

## Quick Start

```rust
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a host with default settings
    let mut host = Vst3Host::new()?;
    
    // Discover and load a plugin
    let plugins = host.discover_plugins()?;
    let mut plugin = host.load_plugin(&plugins[0].path)?;
    
    // Start audio processing
    plugin.start_processing()?;
    
    // Send a MIDI note
    plugin.send_midi_note(60, 127, MidiChannel::Ch1)?;  // Middle C
    
    Ok(())
}
```

> Note: `start_processing()` arms the plugin, but the library does not yet open an
> audio device for you — to actually hear sound today you must drive
> `plugin.process_audio(&mut buffers)` from your own audio callback (see
> `examples/parameter_automation.rs`). Automatic `CpalBackend` wiring is Phase 1 on
> the [roadmap](ROADMAP.md).

## Installation

```toml
[dependencies]
vst3-host = "0.1"

# Optional features
vst3-host = { version = "0.1", features = ["egui-widgets"] }
```

## Supported Platforms

- ✅ Windows (x64)
- ✅ macOS (x64, ARM64) 
- ✅ Linux (x64)

## Examples

### Load a specific plugin
```rust
let mut plugin = host.load_plugin("/path/to/plugin.vst3")?;
```

### Discover plugins with metadata
```rust
let plugins = host.discover_plugins()?;
for p in &plugins {
    println!("{} by {} ({} ins, {} outs)", 
             p.name, p.vendor, p.audio_inputs, p.audio_outputs);
}
```

### Control parameters
```rust
// By name
plugin.set_parameter_by_name("Cutoff", 0.5)?;

// By ID with change callback
plugin.on_parameter_change(|id, value| {
    println!("Param {} = {}", id, value);
});
```

### Send MIDI events
```rust
// Note on/off
plugin.send_midi_note(60, 100, MidiChannel::Ch1)?;
plugin.send_midi_note_off(60, MidiChannel::Ch1)?;

// Control change
plugin.send_midi_cc(1, 64, MidiChannel::Ch1)?;  // Mod wheel

// Custom MIDI event
let event = MidiEvent::PitchBend { 
    channel: MidiChannel::Ch1, 
    value: 8192  // Center 
};
plugin.send_midi_event(event)?;
```

### Monitor audio levels
```rust
plugin.on_audio_process(|levels| {
    for (i, ch) in levels.channels.iter().enumerate() {
        println!("Ch{}: {:.1} dB", i, 20.0 * ch.peak.log10());
    }
});
```

## GUI Integration

### With egui _(planned)_

A dedicated egui widget (`PluginWidget`) under the `egui-widgets` feature is on the
[roadmap](ROADMAP.md). For now, embed the plugin's native editor and build controls
from `get_parameters()` / `set_parameter()` yourself (see `examples/plugin_gui.rs`).

### Native plugin GUI
```rust
if plugin.has_editor() {
    plugin.open_editor(window_handle)?;
}
```

## Safety

This library prioritizes safety:

- **No unsafe in public API** - You never need to write unsafe code
- **Automatic cleanup** - Resources are properly released via RAII
- **Process isolation** _(opt-in)_ - Enable with `Vst3Host::builder().with_process_isolation(true)`
  (requires building the helper binary with the `process-isolation` feature). It is
  **not** on by default yet, and currently isolates load/unload only — see the
  [roadmap](ROADMAP.md).
- **Timeout protection** _(planned)_ - Detecting and terminating hung plugins is not
  implemented yet.

## Performance

Current state is correctness-first, not yet real-time-optimized:

- Background plugin scanning (working)
- _(planned)_ Lock-free audio-thread operations — the audio path currently uses
  mutexes and per-block allocations
- _(planned)_ Zero-copy audio processing, parameter-change caching, process pooling

## License

MIT OR Apache-2.0

## Contributing

Contributions are welcome! Please read our contributing guidelines and code of conduct.

## Acknowledgments

Built on top of the `vst3-sys` bindings. Special thanks to the VST3 SDK contributors.