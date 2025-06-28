# VST3 Host in Rust 🎵

An experimental VST3 plugin host written in Rust, featuring real-time audio streaming and MIDI processing. This project demonstrates how to implement a functional VST3 host using raw pointers, unsafe Rust, and direct VST3 SDK integration.

## ⚡ Features

- **Real-time Audio Processing**: Streams audio with VST3 plugins in a dedicated audio thread
- **MIDI Support**: Send and receive MIDI events to/from VST3 plugins
- **Plugin GUI Integration**: Native plugin GUI support for macOS and Windows
- **Parameter Control**: View and modify plugin parameters in real-time
- **Thread-safe Architecture**: Proper synchronization between UI and audio threads
- **Plugin Discovery**: Automatic scanning of VST3 plugins on your system

## 🎯 Status: WORKING! 

✅ **Audio streaming works**  
✅ **MIDI events work**  
✅ **Plugin GUIs work**  
✅ **Parameter control works**  
✅ **Real-time processing works**

## 🚀 Quick Start

### Prerequisites

- Rust (latest stable)
- VST3 plugins installed on your system
- Audio interface (built-in audio works too)

### Building

```bash
git clone https://github.com/HelgeSverre/rust-vst3-host.git
cd rust-vst3-host
git submodule update --init --recursive
cargo build --release
```

### Running

```bash
cargo run
```

The application will start and automatically scan for VST3 plugins. Navigate through the tabs to:

1. **Plugins Tab**: Browse and load available VST3 plugins
2. **Plugin Tab**: Control parameters and open plugin GUIs  
3. **Processing Tab**: Test MIDI input and monitor audio output

## 🎹 How It Works

### Architecture Overview

This VST3 host implements a complete audio processing pipeline:

```
[UI Thread] ←→ [Shared State] ←→ [Audio Thread]
     ↓              ↓              ↓
  Parameter      MIDI Events    VST3 Process
  Controls       Queue          Real-time
```

### Key Components

- **Audio Processing**: Real-time audio callback using `cpal` with VST3 processing
- **MIDI Handling**: Thread-safe event queuing with immediate real-time processing
- **COM Implementation**: Custom COM interfaces for VST3 event lists and parameter changes
- **Memory Management**: Careful handling of VST3 pointers and COM object lifecycles

### The "Disgusting Hacks" Approach

This implementation prioritizes **working code over safe code**:

- Extensive use of `unsafe` blocks for VST3 interop
- Raw pointer manipulation for audio buffers
- Direct COM interface implementation
- Manual memory management for VST3 objects

**Why this approach?** The VST3 SDK is inherently unsafe, and creating safe abstractions often introduces overhead and complexity that can interfere with real-time audio processing.

## 🔧 Technical Details

### Thread Safety

The host uses a sophisticated threading model:

- **UI Thread**: Handles GUI, parameter updates, and plugin loading
- **Audio Thread**: Performs real-time VST3 processing with sub-millisecond latency
- **Shared State**: Thread-safe communication via `Arc<Mutex<AudioProcessingState>>`

### VST3 Integration

Direct integration with the VST3 SDK:

```rust
// Example: Real-time audio processing
fn process_audio(&mut self, output: &mut [f32]) -> bool {
    let mut process_data = HostProcessData::new(self.block_size, self.sample_rate);
    process_data.prepare_buffers(&component, self.block_size)?;
    
    // Add MIDI events from UI thread
    for event in self.pending_midi_events.drain(..) {
        process_data.input_events.events.lock().unwrap().push(event);
    }
    
    // Process with VST3 plugin
    processor.process(&mut process_data.process_data)
}
```

### Module Structure

```
src/
├── main.rs                    # Main application and UI
├── audio_processing.rs        # Audio buffers and processing
├── com_implementations.rs     # VST3 COM interfaces  
├── plugin_discovery.rs        # Plugin scanning
├── plugin_loader.rs          # Plugin loading utilities
├── utils.rs                  # Helper functions
└── data_structures.rs        # Shared data types
```

## 🎵 Supported Plugins

Tested with various VST3 plugins including:

- **Synthesizers**: Dexed, Surge XT, Vital
- **Effects**: FabFilter, Valhalla, TAL
- **Utilities**: ShowMIDI, MIDI Monitor plugins

## ⚠️ Experimental Notice

This is an **experimental implementation** for learning and research purposes. Key considerations:

- **Memory Safety**: Extensive use of unsafe code
- **Error Handling**: Minimal error recovery in audio thread
- **Platform Support**: Primarily tested on macOS, Windows support experimental
- **Performance**: Not optimized for production use

## 🛠️ Development

### Building from Source

```bash
# Clone with submodules
git clone --recursive https://github.com/HelgeSverre/rust-vst3-host.git

# Or if already cloned
git submodule update --init --recursive

# Build
cargo build
```

### Dependencies

- `vst3-sys`: VST3 SDK bindings
- `eframe/egui`: GUI framework
- `cpal`: Cross-platform audio I/O
- `libloading`: Dynamic library loading

## 📝 Contributing

This project is experimental and primarily for educational purposes. However, contributions are welcome:

1. Fork the repository
2. Create a feature branch
3. Make your changes (maintaining the "working over safe" philosophy)
4. Test with real VST3 plugins
5. Submit a pull request

## 📄 License

This project is licensed under the MIT License - see the LICENSE file for details.

## 🙏 Acknowledgments

- **VST3 SDK**: Steinberg for the VST3 specification and SDK
- **Rust Audio Community**: For inspiration and audio libraries
- **Plugin Developers**: For creating the amazing plugins this host can run

---

**⚡ "It works!" - The highest praise for experimental audio software**

*Built with raw pointers, unsafe blocks, and a healthy disregard for Rust's safety guarantees. Sometimes you need to break the rules to make music.* 🎶