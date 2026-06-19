# VST3 Host Documentation

Welcome to the comprehensive documentation for the vst3-host library! This documentation will take you from VST3 hosting beginner to expert through progressive tutorials and practical examples.

## 🚀 Quick Start

New to VST3 hosting? Start here:

```rust
use vst3_host::prelude::*;

// Create a host, discover plugins, and load one
let mut host = Vst3Host::new()?;
let plugins = host.discover_plugins()?;
let mut plugin = host.load_plugin(&plugins[0].path)?;
plugin.start_processing()?;
```

## 📚 Tutorial Series

Follow these tutorials in order to build from basic concepts to production-ready applications:

### [Tutorial 1: Your First VST3 Host](tutorials/01-your-first-vst3-host.md) ⏱️ 5 minutes
**Perfect for beginners**
- Load a VST3 plugin and get basic information
- Understand what VST3 hosting means
- Handle errors gracefully
- Test plugin lifecycle

**What you'll build:** A simple command-line tool that loads a plugin and displays its capabilities.

### [Tutorial 2: Processing Audio](tutorials/02-processing-audio.md) ⏱️ 10 minutes
**Prerequisites: Tutorial 1**
- Set up real-time audio processing with CPAL
- Process audio buffers through plugins
- Generate test signals and monitor levels
- Handle real-time parameter changes

**What you'll build:** A working audio processor that plays test tones through VST3 plugins.

### [Tutorial 3: Building a Simple Plugin Host](tutorials/03-building-simple-plugin-host.md) ⏱️ 20 minutes
**Prerequisites: Tutorials 1-2**
- Create an interactive GUI with egui
- Build real-time parameter controls
- Implement a virtual MIDI keyboard
- Add audio level monitoring

**What you'll build:** A complete GUI plugin host with interactive controls.

### [Tutorial 4: Advanced Features](tutorials/04-advanced-features.md) ⏱️ 30 minutes
**Prerequisites: Tutorials 1-3**
- Use process isolation for crash protection
- Implement sophisticated plugin discovery
- Handle problematic plugins safely
- Build plugin compatibility systems

**What you'll build:** A robust host that can handle any plugin safely.

### [Tutorial 5: Production Ready Host](tutorials/05-production-ready-host.md) ⏱️ 45 minutes
**Prerequisites: Tutorials 1-4**
- Implement session save/load with plugin presets
- Build multi-plugin chains
- Add professional audio features
- Optimize for real-time performance

**What you'll build:** A complete, production-ready VST3 host application.

## 📖 Reference Materials

### [Quick Reference](quick-reference.md)
Fast access to common patterns, APIs, and code snippets for everyday VST3 hosting tasks.

### [Troubleshooting Guide](troubleshooting.md)
Comprehensive guide to diagnosing and solving common issues, platform-specific problems, and performance optimization.

## 🎯 Learning Paths

Choose your path based on your goals:

### **🎵 Music Producer Path**
*"I want to build custom tools for music production"*

1. Tutorial 1 → Tutorial 2 → Tutorial 3
2. Focus on MIDI, parameter automation, and GUI design
3. Study the plugin scanner and GUI examples

### **🔧 Audio Developer Path**
*"I want to build professional audio software"*

1. Complete all tutorials in order
2. Study process isolation and performance optimization
3. Review the advanced host example for production patterns

### **🚀 Plugin Developer Path**
*"I want to test my VST3 plugins during development"*

1. Tutorial 1 → Tutorial 2 → Tutorial 4
2. Focus on compatibility testing and crash protection
3. Build custom test harnesses for your plugins

### **🎮 Game Developer Path**
*"I want to add VST3 support to my game engine"*

1. Tutorial 1 → Tutorial 2 → Performance optimization sections
2. Focus on low-latency processing and resource management
3. Study the real-time audio patterns

## 💡 Examples Overview

The library includes several example applications demonstrating different aspects:

### `examples/test_loading.rs`
**Complexity: Beginner**
- Basic plugin loading without audio
- Plugin information display
- Error handling patterns
- **Best for:** Understanding the basics

### `examples/plugin_scanner.rs`
**Complexity: Intermediate**
- Advanced plugin discovery with progress
- Plugin categorization and filtering
- Metadata extraction
- **Best for:** Building plugin management tools

### `examples/plugin_gui.rs`
**Complexity: Advanced**
- Complete GUI application with egui
- Real-time audio processing
- Virtual MIDI keyboard
- Parameter automation
- **Best for:** Building complete plugin hosts

### `examples/host.rs`
**Complexity: Expert**
- Production-ready multi-plugin host
- Session management
- Advanced error recovery
- Performance monitoring
- **Best for:** Commercial application development

## 🎨 Code Examples by Feature

### Plugin Discovery
```rust
// Basic discovery
let plugins = host.discover_plugins()?;

// With progress callback
let plugins = host.discover_plugins_with_callback(|progress| {
    match progress {
        DiscoveryProgress::Found { plugin, current, total } => {
            println!("[{}/{}] Found: {}", current, total, plugin.name);
        }
        _ => {}
    }
})?;
```

### Audio Processing
```rust
// Create audio stream with plugin processing
let stream = backend.create_output_stream(
    &device,
    config,
    Box::new(move |output: &mut [f32]| {
        let mut buffers = AudioBuffers::new(0, 2, 512, 44100.0);
        plugin.process_audio(&mut buffers).unwrap();
        // Copy buffers to output...
    }),
    Box::new(|err| eprintln!("Audio error: {}", err)),
)?;
```

### Parameter Control
```rust
// Get and modify parameters
let params = plugin.get_parameters()?;
for param in params {
    println!("{}: {}", param.name, param.value);
    plugin.set_parameter(param.id, 0.5)?; // Set to 50%
}
```

### MIDI Input
```rust
// Send MIDI notes
plugin.send_midi_note(60, 127, MidiChannel::Ch1)?; // C4, full velocity
plugin.send_midi_cc(1, 64, MidiChannel::Ch1)?;     // Mod wheel, center
```

### Process Isolation
```rust
// Enable process isolation for crash protection
let host = Vst3Host::builder()
    .with_process_isolation(true)
    .build()?;
```

## 🏗️ Architecture Overview

Understanding the library architecture helps you make the right design decisions:

```
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   Your App      │◄──►│   vst3-host      │◄──►│   VST3 Plugin   │
│                 │    │                  │    │                 │
│ ┌─────────────┐ │    │ ┌──────────────┐ │    │ ┌─────────────┐ │
│ │ GUI Thread  │ │    │ │ Host Manager │ │    │ │ Component   │ │
│ └─────────────┘ │    │ └──────────────┘ │    │ └─────────────┘ │
│ ┌─────────────┐ │    │ ┌──────────────┐ │    │ ┌─────────────┐ │
│ │Audio Thread │ │◄──►│ │Plugin Wrapper│ │◄──►│ │ Controller  │ │
│ └─────────────┘ │    │ └──────────────┘ │    │ └─────────────┘ │
│ ┌─────────────┐ │    │ ┌──────────────┐ │    │ ┌─────────────┐ │
│ │MIDI Handler │ │    │ │ Audio Backend│ │    │ │ Editor      │ │
│ └─────────────┘ │    │ └──────────────┘ │    │ └─────────────┘ │
└─────────────────┘    └──────────────────┘    └─────────────────┘
```

### Key Components

- **Host Manager**: Discovers and loads plugins
- **Plugin Wrapper**: Provides safe, Rust-friendly plugin interface
- **Audio Backend**: Handles real-time audio I/O (CPAL, JACK, etc.)
- **Process Isolation**: Optional crash protection for problematic plugins

## 🔧 Development Setup

### Prerequisites
- Rust 1.70+ with Cargo
- Platform-specific audio drivers (ASIO/CoreAudio/ALSA)
- VST3 plugins for testing

### Quick Setup
```bash
# Clone and build
git clone https://github.com/your-repo/vst3-host
cd vst3-host
cargo build --examples

# Run plugin scanner
cargo run --example plugin_scanner

# Run GUI host
cargo run --example plugin_gui --features egui-widgets
```

### Feature Flags
```toml
[dependencies]
vst3-host = { 
    version = "0.1.0", 
    features = [
        "cpal-backend",      # Real-time audio with CPAL
        "egui-widgets",      # GUI widgets for plugin controls
        "process-isolation", # Crash protection
    ]
}
```

## 🤝 Contributing

This documentation is a living resource! Help improve it:

- **Found a bug in a tutorial?** Open an issue
- **Have a better example?** Submit a PR
- **Want to add a tutorial?** Follow the existing format
- **Need help?** Ask in Discussions

## 📚 Additional Resources

### VST3 Specification
- [Official VST3 Documentation](https://developer.steinberg.help/display/VST)
- [VST3 SDK on GitHub](https://github.com/steinbergmedia/vst3sdk)

### Audio Programming
- [Real-Time Audio Programming](http://www.rossbencina.com/code/real-time-audio-programming-101-time-waits-for-nothing)
- [CPAL Documentation](https://docs.rs/cpal/)

### Rust Audio Ecosystem
- [RustAudio GitHub Organization](https://github.com/RustAudio)
- [Awesome Rust Audio](https://github.com/rust-unofficial/awesome-rust#audio-and-music)

## 🎉 What's Next?

After completing these tutorials, you'll have the knowledge to:

- **Build custom DAW software** with full VST3 support
- **Create specialized audio tools** for specific workflows  
- **Develop plugin testing frameworks** for plugin developers
- **Add VST3 support** to existing applications
- **Contribute to the Rust audio ecosystem**

Ready to start? Head to [Tutorial 1: Your First VST3 Host](tutorials/01-your-first-vst3-host.md)!

---

*This documentation covers vst3-host library version 0.1.0. For the latest updates, check the [repository](https://github.com/your-repo/vst3-host).*