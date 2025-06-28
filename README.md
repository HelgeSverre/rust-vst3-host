# VST3 Host in Rust

Experimental VST3 plugin host written in Rust. Uses unsafe code and raw pointers for direct VST3 SDK integration.

## What works

- Audio streaming with VST3 plugins
- MIDI input/output
- Plugin GUIs (macOS/Windows)
- Parameter control
- Plugin discovery

## Building

```bash
git clone https://github.com/HelgeSverre/rust-vst3-host.git
cd rust-vst3-host
git submodule update --init --recursive
cargo build --release
cargo run
```

## Implementation Notes

This host prioritizes working code over safe code:

- Extensive `unsafe` blocks for VST3 interop
- Raw pointer manipulation for audio buffers  
- Direct COM interface implementation
- Manual memory management

The VST3 SDK is inherently unsafe, so safe abstractions add complexity without much benefit for this experimental implementation.

## Architecture

```
[UI Thread] ←→ [Shared State] ←→ [Audio Thread]
     ↓              ↓              ↓
  Parameters     MIDI Queue    VST3 Process
```

- UI thread handles GUI and plugin loading
- Audio thread processes VST3 in real-time
- Thread-safe communication via `Arc<Mutex<AudioProcessingState>>`

## Warning

This is experimental code for learning purposes. Extensive use of unsafe Rust, minimal error handling, and not optimized for production use.