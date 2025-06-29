# vst3-host

A Rust library for hosting VST3 plugins with a safe API.

## Building and Running

### Prerequisites

- Rust 1.70 or later
- VST3 SDK (included as submodule)
- CMake (for building VST3 SDK)

### Building the Library

```bash
# Clone the repository (if not already done)
git clone <repository-url>
cd vst-host

# Initialize and update submodules (for VST3 SDK)
git submodule update --init --recursive

# Build the library
cd vst3-host
cargo build --release

# Build with CPAL audio backend
cargo build --release --features cpal-backend

# Run tests
cargo test
```

### Building the Main Application

```bash
# From the root directory
cd ..
cargo build --release

# Run the main VST3 host application
cargo run --release

# Or run the built binary directly
./target/release/vst-host

# The build also creates a helper binary for process isolation
# This is built automatically: ./target/release/vst-host-helper
```

### Building and Running Examples

The library comes with several examples demonstrating different features:

```bash
# From the vst3-host directory
cd vst3-host

# 1. Plugin scanner - discover all VST3 plugins on your system
cargo run --example plugin_scanner
cargo run --example plugin_scanner -- --isolated  # With process isolation

# 2. Plugin GUI - egui interface with file picker and virtual MIDI keyboard
cargo run --example plugin_gui --features cpal-backend

# 3. MIDI keyboard - terminal-based interactive MIDI input
cargo run --example midi_keyboard --features cpal-backend

# Build all examples at once
cargo build --examples --features cpal-backend
```

### Running Tests

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test module
cargo test midi_tests
cargo test parameter_tests
cargo test audio_tests

# Run tests with CPAL backend
cargo test --features cpal-backend
```

### Quick Command Reference

```bash
# Development workflow
cargo check              # Quick syntax/type check
cargo clippy            # Linting
cargo fmt               # Format code
cargo doc --open        # Build and view documentation

# Release builds
cargo build --release --features cpal-backend
cargo test --release

# Run specific example in release mode
cargo run --release --example plugin_scanner
```

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
    // Create a host that scans system directories
    let mut host = Vst3Host::builder()
        .scan_default_paths()  // Opt-in to scanning system VST3 directories
        .build()?;
    
    // Or create a host that only scans specific directories
    let mut host = Vst3Host::builder()
        .add_scan_path("./my-plugins")
        .build()?;
    
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

## Usage Examples

The library includes three focused examples:

1. **plugin_scanner** - Discovers and lists VST3 plugins with optional process isolation
   ```bash
   cargo run --example plugin_scanner -- --help
   cargo run --example plugin_scanner -- --isolated  # Run with process isolation
   ```

2. **plugin_gui** - Complete host with egui interface, file picker and virtual MIDI keyboard
   ```bash
   cargo run --example plugin_gui --features cpal-backend
   cargo run --example plugin_gui --features cpal-backend -- --isolated  # With process isolation
   ```

3. **midi_keyboard** - Terminal-based MIDI keyboard for testing plugins
   ```bash
   cargo run --example midi_keyboard --features cpal-backend
   ```

## API Overview

### Plugin Discovery

```rust
// Create host with custom scan paths only (recommended)
let mut host = Vst3Host::builder()
    .add_scan_path("./my-plugins")
    .add_scan_path("/custom/vst3/folder")
    .build()?;

// Or include system directories
let mut host = Vst3Host::builder()
    .scan_default_paths()  // Opt-in to system paths
    .add_scan_path("./my-plugins")  // Plus custom paths
    .build()?;

// Discover all plugins in configured paths
let plugins = host.discover_plugins()?;

// Discover with progress updates
let plugins = host.discover_plugins_with_callback(|progress| {
    match progress {
        DiscoveryProgress::Found { plugin, current, total } => {
            println!("[{}/{}] Found: {}", current, total, plugin.name);
        }
        _ => {}
    }
})?;

// Add paths after creation
host.add_scan_path("/another/vst3/folder")?;
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

## Troubleshooting

### Build Issues

If you encounter build errors:

1. **VST3 SDK not found**: Make sure submodules are initialized:
   ```bash
   git submodule update --init --recursive
   ```

2. **CMake errors**: The VST3 SDK requires CMake. Install it:
   ```bash
   # macOS
   brew install cmake
   
   # Ubuntu/Debian
   sudo apt-get install cmake
   
   # Windows
   # Download from https://cmake.org/download/
   ```

3. **CPAL backend errors**: Install system audio dependencies:
   ```bash
   # Ubuntu/Debian
   sudo apt-get install libasound2-dev
   
   # Fedora
   sudo dnf install alsa-lib-devel
   ```

### Runtime Issues

1. **No plugins found**: Make sure you have VST3 plugins installed in standard locations:
   - macOS: `/Library/Audio/Plug-Ins/VST3` or `~/Library/Audio/Plug-Ins/VST3`
   - Windows: `C:\Program Files\Common Files\VST3`
   - Linux: `/usr/lib/vst3` or `~/.vst3`

2. **Audio device errors**: Check that your audio device is properly configured and not in use by another application.

3. **Plugin crashes**: The library includes process isolation to handle plugin crashes gracefully. If a plugin consistently crashes, it may be incompatible.

## License

MIT OR Apache-2.0