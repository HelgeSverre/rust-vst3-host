# vst3-host

A Rust library for hosting VST3 plugins with a safe API.

## Features

- Safe API - No unsafe code required by library users
- Simple to use - Minimal boilerplate for common tasks
- CPAL audio backend included
- Process isolation for plugin crash protection
- Full MIDI support
- Parameter control and automation
- Real-time audio level monitoring

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

## Examples

Run the examples with:

```bash
# Basic host example
cargo run --example basic_host --features cpal-backend

# Plugin scanner
cargo run --example plugin_scanner

# MIDI keyboard (coming soon)
cargo run --example midi_keyboard --features cpal-backend
```

## API Overview

### Plugin Discovery

```rust
// Discover all plugins
let plugins = host.discover_plugins()?;

// Discover with progress updates
let plugins = host.discover_plugins_with_progress(|progress| {
    println!("Scanning: {}", progress.current_plugin);
})?;

// Add custom scan paths
host.add_scan_path("/my/custom/vst3/folder")?;
```

### Plugin Loading and Control

```rust
// Load a plugin
let mut plugin = host.load_plugin("/path/to/plugin.vst3")?;

// Start/stop processing
plugin.start_processing()?;
plugin.stop_processing()?;

// Get plugin info
println!("Plugin: {} by {}", plugin.info().name, plugin.info().vendor);
```

### MIDI

```rust
// Send note on/off
plugin.send_midi_note(60, 100, MidiChannel::Ch1)?;
plugin.send_midi_note_off(60, MidiChannel::Ch1)?;

// Send control change
plugin.send_midi_cc(1, 64, MidiChannel::Ch1)?;  // Mod wheel to 64

// Send pitch bend
plugin.send_midi_event(MidiEvent::PitchBend {
    channel: MidiChannel::Ch1,
    value: 8192,  // Center position
})?;

// MIDI panic - all notes/sounds off
plugin.midi_panic()?;
```

### Parameters

```rust
// Get all parameters
let params = plugin.get_parameters()?;

// Set parameter by ID
plugin.set_parameter(param_id, 0.5)?;

// Set by name
plugin.set_parameter_by_name("Cutoff", 0.8)?;

// Batch updates
plugin.update_parameters(|update| {
    update.set(1, 0.5)
          .set(2, 0.3)
          .set(3, 0.9);
    Ok(())
})?;

// Monitor changes
plugin.on_parameter_change(|id, value| {
    println!("Parameter {} = {}", id, value);
});
```

### Audio Monitoring

```rust
// Get current levels
let levels = plugin.get_output_levels();
for (i, channel) in levels.channels.iter().enumerate() {
    println!("Ch{}: {:.1} dB", i, channel.peak_db());
}

// Real-time monitoring
plugin.on_audio_process(|levels| {
    // Called after each audio buffer
    if levels.is_clipping() {
        println!("CLIPPING!");
    }
});
```

## Safety

This library prioritizes safety:

- No `unsafe` code in the public API
- All COM/VST3 interactions are wrapped
- Process isolation prevents plugin crashes from affecting the host
- Automatic resource cleanup via RAII
- Thread-safe parameter access

## License

MIT OR Apache-2.0