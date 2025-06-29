# vst3-host

A safe, simple, and lightweight Rust library for hosting VST3 plugins.

## Features

- 🛡️ **Safe by Default** - All unsafe VST3 COM interactions are hidden behind a safe Rust API
- 🚀 **Simple to Use** - Load and play a plugin in under 10 lines of code
- 🔊 **Batteries Included** - Comes with CPAL audio backend, ready to make sound
- 💪 **Crash Resistant** - Plugin crashes won't take down your host (process isolation)
- 🎹 **Full MIDI Support** - Send notes, CCs, and other MIDI events
- 🎛️ **Parameter Control** - Get, set, and automate plugin parameters
- 📊 **Real-time Monitoring** - VU meters, peak levels, and parameter change callbacks

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

### With egui
```rust
use vst3_host_egui::PluginWidget;

ui.add(PluginWidget::new(&mut plugin));
```

### Native plugin GUI
```rust
if plugin.has_editor() {
    plugin.open_editor(window_handle)?;
}
```

## Safety

This library prioritizes safety:

- **No unsafe in public API** - You never need to write unsafe code
- **Process isolation** - Plugins run in separate processes by default
- **Automatic cleanup** - Resources are properly released via RAII
- **Thread safe** - Plugin instances can be safely shared between threads
- **Timeout protection** - Hung plugins are automatically terminated

## Performance

- Zero-copy audio processing where possible
- Lock-free operations in audio thread
- Efficient parameter caching
- Background plugin scanning
- Process pooling for multiple plugins

## License

MIT OR Apache-2.0

## Contributing

Contributions are welcome! Please read our contributing guidelines and code of conduct.

## Acknowledgments

Built on top of the `vst3-sys` bindings. Special thanks to the VST3 SDK contributors.