# vst3-host

A safe, simple, and lightweight Rust library for hosting VST3 plugins with real-time audio processing, MIDI support, and advanced plugin compatibility features.

## 🚀 Quick Start (5 minutes)

```rust
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Simple plugin loading
    let mut plugin = vst3_host::simple::load_plugin("/path/to/plugin.vst3")?;
    
    // Start audio processing
    plugin.start_processing()?;
    
    // Send a MIDI note
    plugin.send_midi_note(60, 127, MidiChannel::Ch1)?;  // Middle C
    
    Ok(())
}
```

## 📚 Learning Paths

### 🎵 **For Music Producers**
Want to build custom audio tools? Start here:
1. [**5-min Tutorial**: Your First VST3 Host](docs/tutorials/01-first-host.md)
2. [**10-min Tutorial**: Processing Audio](docs/tutorials/02-processing-audio.md)
3. [**Example**: Interactive Parameter Automation](examples/parameter_automation.rs)

### 👨‍💻 **For Rust Developers**
New to audio programming? Follow this path:
1. [**Quick Reference**: Common Patterns](docs/QUICK_REFERENCE.md)
2. [**20-min Tutorial**: Building a Simple Plugin Host](docs/tutorials/03-simple-host.md)
3. [**30-min Tutorial**: Advanced Features](docs/tutorials/04-advanced-features.md)

### 🔧 **For Plugin Developers**
Testing your VST3 plugins? Use these tools:
1. [**Example**: Plugin Scanner](examples/plugin_scanner.rs) - Test plugin loading
2. [**Example**: Test Loading](examples/test_loading.rs) - Detailed compatibility testing
3. [**Guide**: Troubleshooting](docs/TROUBLESHOOTING.md) - Common plugin issues

### 🏭 **For Production Use**
Building commercial software? Learn best practices:
1. [**45-min Tutorial**: Production Ready Host](docs/tutorials/05-production-ready.md)
2. [**Guide**: Performance Optimization](docs/QUICK_REFERENCE.md#performance)
3. [**Guide**: Cross-Platform Deployment](docs/QUICK_REFERENCE.md#deployment)

## ✨ Key Features

- **🛡️ Crash Protection** - Process isolation prevents plugin crashes from affecting your app
- **🎯 Simple API** - No unsafe code required, sensible defaults, minimal boilerplate
- **🎹 Full MIDI Support** - Real-time MIDI input/output with virtual keyboard
- **🎚️ Parameter Control** - Real-time automation and preset management
- **📊 Audio Monitoring** - Built-in level meters and clipping detection
- **🔄 Cross-Platform** - Windows, macOS, and Linux support
- **⚡ High Performance** - Optimized for real-time audio processing

## 🛠️ Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
vst3-host = "0.1"

# For audio I/O
vst3-host = { version = "0.1", features = ["cpal-backend"] }

# For GUI applications  
vst3-host = { version = "0.1", features = ["cpal-backend", "egui-widgets"] }

# For crash protection
vst3-host = { version = "0.1", features = ["process-isolation"] }
```

## 🎮 Examples

Run these examples to see the library in action:

```bash
# 1. Interactive GUI host with virtual keyboard
cargo run --example host --features "cpal-backend,egui-widgets"

# 2. Discover plugins on your system
cargo run --example plugin_scanner

# 3. Test plugin loading and compatibility
cargo run --example test_loading -- "/path/to/plugin.vst3"

# 4. Interactive parameter automation
cargo run --example parameter_automation --features "cpal-backend"
```

## 📖 API Overview

### Simple Plugin Loading
```rust
// Load and play immediately
let mut plugin = vst3_host::simple::load_plugin("/path/to/synth.vst3")?;
plugin.send_midi_note(60, 100, MidiChannel::Ch1)?;
```

### Advanced Configuration
```rust
// Full control over the host
let mut host = Vst3Host::builder()
    .sample_rate(44100.0)
    .block_size(512)
    .with_process_isolation(true)  // Crash protection
    .add_scan_path("./my-plugins")
    .build()?;

let mut plugin = host.load_plugin("/path/to/plugin.vst3")?;
```

### Real-time Audio Processing
```rust
// Set up audio callback
plugin.start_processing()?;

// Monitor audio levels
plugin.on_audio_process(|levels| {
    if levels.is_clipping() {
        println!("Audio clipping detected!");
    }
});
```

### Parameter Automation
```rust
// Automate parameters over time
plugin.update_parameters(|update| {
    update.set(1, 0.5)  // Cutoff frequency
          .set(2, 0.8)  // Resonance
          .set(3, 0.2); // Filter envelope
    Ok(())
})?;
```

## 🔧 Development Setup

### Prerequisites
- Rust 1.70 or later
- CMake (for VST3 SDK)
- Platform-specific audio dependencies (see [Troubleshooting](#troubleshooting))

### Build Commands
```bash
# Clone with VST3 SDK
git clone --recursive https://github.com/your-repo/vst3-host.git
cd vst3-host

# Quick development checks
make check      # Type checking and linting
make test       # Run all tests  
make examples   # Build all examples
make fix        # Auto-fix formatting and simple issues

# Or use cargo directly
cargo build --features "cpal-backend,egui-widgets"
cargo test --features "cpal-backend"
cargo run --example host --features "cpal-backend,egui-widgets"
```

## 🛡️ Safety & Reliability

This library prioritizes safety and reliability:

- **Memory Safety**: No unsafe code in public API, automatic resource cleanup
- **Crash Protection**: Process isolation prevents plugin crashes from affecting your application
- **Thread Safety**: Safe concurrent access to plugin parameters and audio processing
- **Error Handling**: Comprehensive error types with actionable error messages
- **Platform Compatibility**: Handles platform-specific edge cases (e.g., macOS Objective-C conflicts)

## 🌐 Platform Support

| Platform | Status | Notes |
|----------|--------|-------|
| **macOS** | ✅ Full | Includes Objective-C conflict resolution for Waves plugins |
| **Windows** | ✅ Full | WASAPI and DirectSound support via CPAL |
| **Linux** | ✅ Full | ALSA, PulseAudio, and JACK support via CPAL |

## 📊 Performance

- **Low Latency**: Optimized for real-time audio (< 10ms latency possible)
- **Memory Efficient**: Minimal allocations in audio thread
- **CPU Efficient**: SIMD optimizations where available
- **Scalable**: Handle multiple plugins simultaneously

## 🤝 Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## 📄 License

Licensed under either of:
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

## 🆘 Troubleshooting

### Build Issues
```bash
# Missing VST3 SDK
git submodule update --init --recursive

# Missing CMake
brew install cmake  # macOS
sudo apt install cmake  # Ubuntu

# Audio system dependencies
sudo apt install libasound2-dev  # Linux
```

### Runtime Issues
- **No plugins found**: Check VST3 installation paths in [troubleshooting guide](docs/TROUBLESHOOTING.md)
- **Audio device errors**: Ensure audio device isn't in use by another application
- **Plugin crashes**: Enable process isolation with `.with_process_isolation(true)`

For detailed troubleshooting, see the [complete troubleshooting guide](docs/TROUBLESHOOTING.md).

---

**Need Help?** 
- 📖 [Full Documentation](docs/README.md)
- 🐛 [Report Issues](https://github.com/your-repo/vst3-host/issues)  
- 💬 [Community Discord](https://discord.gg/your-discord)