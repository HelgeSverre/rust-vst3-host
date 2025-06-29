# TODO List for VST3 Host

## Core Features

### Plugin Chaining
- [ ] Ability to open multiple plugins at once
- [ ] Chain plugins together (e.g., VST Host → Arp VST → Synth VST → Delay FX VST → VST Host Output)
- [ ] Visual representation of signal flow
- [ ] Drag-and-drop to reorder plugin chain

### Audio Processing Improvements
- [ ] Add ability to change sample rate in audio processing
- [ ] Add ability to change block size in audio processing
- [ ] Automatically start processing state when loading plugin
- [ ] Add "Audio Panic" button that kills sound output immediately

### MIDI Features
- [ ] Add "MIDI Panic" button that sends:
  - "All Notes Off" (CC 123) to all MIDI channels
  - "All Sounds Off" (CC 120) to all MIDI channels
  - Reset all controllers (CC 121) to all MIDI channels

### Stability & Error Handling
- [ ] Add "plugin crashed" handling so the entire application doesn't crash if a plugin dies
- [ ] Implement plugin sandboxing/isolation
- [ ] Add crash recovery mechanism
- [ ] Save state before risky operations

### UI/UX Improvements
- [ ] Improve "Plugins" tab with ability to add folders to scan for plugins
- [ ] Add persistent plugin path preferences
- [ ] Remove symbols from labels and prefer text:
  - Replace "active" plugin indicator symbol with text (currently shows as "error square")
  - Audit all UI elements for symbol usage
- [ ] Add VU meter/level indicator to show plugin audio output
- [ ] Add peak level indicators
- [ ] Add clipping indicators

## Developer Experience

### Rust API Wrapper
- [ ] Create a more Rust-friendly wrapper/abstraction on top of existing code
- [ ] Make loading a plugin easier with safe abstractions
- [ ] Add builder pattern for plugin configuration
- [ ] Implement proper error types instead of String errors

### Hooks & Callbacks
- [ ] Add hooks and callback functionality for easier plugin integration
- [ ] Event-based architecture for plugin state changes
- [ ] Rust-y and type-safe callback system
- [ ] Example implementations for common use cases

## Future Enhancements

### Performance
- [ ] Multi-threaded plugin processing
- [ ] Plugin processing optimization
- [ ] Memory usage optimization

### Advanced Features
- [ ] Plugin preset management
- [ ] A/B comparison between settings
- [ ] Parameter automation recording/playback
- [ ] MIDI learn functionality
- [ ] Plugin bypass with proper tail handling