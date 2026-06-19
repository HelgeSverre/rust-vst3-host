# TODO List for VST3 Host

## Core Features

### Plugin Chaining & Graph Processing
- [ ] Ability to open multiple plugins at once
  - Create `PluginGraph` struct to manage DAG (Directed Acyclic Graph) of plugins
  - Add `ConnectionPoint` struct for audio/MIDI routing between plugins
  - Consider using `petgraph` crate for graph management and topological sorting
- [ ] Chain plugins together (e.g., VST Host → Arp VST → Synth VST → Delay FX VST → VST Host Output)
  - Implement buffer management for inter-plugin communication
  - Add latency compensation (accumulate `getLatencySamples()` from each plugin)
- [ ] Visual representation of signal flow
- [ ] Drag-and-drop to reorder plugin chain

### Audio Processing Improvements
- [ ] Add ability to change sample rate in audio processing
  - Call `setupProcessing()` on all loaded plugins when changing
  - Update all audio stream configurations
- [ ] Add ability to change block size in audio processing
  - Reallocate all buffers in `HostProcessData`
  - Notify all plugins via `setupProcessing()`
- [x] Automatically start processing state when loading plugin
  - Add config option in `VST3Inspector::load_plugin()` to auto-call `startProcessing()`
- [x] Add "Audio Panic" button that kills sound output immediately
  - Zero all output buffers
  - Call `stopProcessing()` on all active plugins
  - Optionally reset audio stream

### MIDI Features
- [x] Add "MIDI Panic" button that sends:
  - "All Notes Off" (CC 123) to all MIDI channels (0-15)
  - "All Sounds Off" (CC 120) to all MIDI channels
  - "Reset All Controllers" (CC 121) to all MIDI channels
  - Implementation: Loop through all 16 channels and send these CCs
- [ ] Add MIDI routing matrix for complex multi-plugin setups
- [ ] Implement per-plugin MIDI filters (channel filter, note range, velocity curve)

### Stability & Error Handling
- [x] Add "plugin crashed" handling so the entire application doesn't crash if a plugin dies
  - [x] Run each plugin in separate thread with panic catching
  - [ ] Implement watchdog timer to monitor processing time
  - [x] Add COM interface validation and null checks
- [x] Implement plugin sandboxing/isolation
  - [x] Process isolation with IPC for audio/MIDI data
  - [x] Memory protection boundaries
- [ ] Add crash recovery mechanism
  - Serialize plugin state before risky operations
  - Attempt to reload from last good state after crash
- [ ] Save state before risky operations
  - Before `setActive()`, `setupProcessing()`, etc.
  - Implement state snapshots with timestamp

### UI/UX Improvements
- [x] Improve "Plugins" tab with ability to add folders to scan for plugins
  - Add "Add Folder" button
  - Store custom paths in preferences
  - Show scan progress
- [x] Add persistent plugin path preferences
  - Save to config file (TOML/JSON)
  - Load on startup
- [x] Remove symbols from labels and prefer text:
  - Replace "active" plugin indicator symbol with text (currently shows as "error square")
  - Audit all UI elements for symbol usage
  - Use descriptive text labels instead
- [x] Add VU meter/level indicator to show plugin audio output
  - RMS and peak level calculation
  - Stereo or multi-channel display
  - Configurable ballistics
- [x] Add peak level indicators with hold time
- [ ] Add clipping indicators (red when signal > 0dB)

## Developer Experience

### Rust API Wrapper
- [ ] Create a more Rust-friendly wrapper/abstraction on top of existing code
  - Hide unsafe COM interactions behind safe API
  - Use RAII patterns for resource management
- [ ] Make loading a plugin easier with safe abstractions:
  ```rust
  let plugin = PluginLoader::new()
      .path("/path/to/plugin.vst3")
      .sample_rate(48000)
      .block_size(512)
      .with_midi_input(true)
      .with_audio_output(2) // stereo
      .load()?;
  ```
- [ ] Add builder pattern for plugin configuration
- [x] Implement proper error types instead of String errors
  - Use `thiserror` crate
  - Specific error variants for each failure mode

### Hooks & Callbacks
- [ ] Add hooks and callback functionality for easier plugin integration
  - `OnLoaded`, `OnActivated`, `OnProcessingStarted` events
  - `OnParameterChanged`, `OnMidiEvent` callbacks
  - `OnCrashed`, `OnError` handlers
- [ ] Event-based architecture for plugin state changes
  - Thread-safe event bus using `crossbeam-channel`
  - Async event handlers with `tokio`
- [ ] Rust-y and type-safe callback system
- [ ] Example implementations for common use cases

### Safe Parameter API
- [ ] Type-safe parameter access:
  ```rust
  let volume = plugin.get_parameter::<f64>("volume")?;
  plugin.set_parameter("volume", 0.5)?;
  ```
- [ ] Batch parameter updates:
  ```rust
  plugin.update_parameters(|params| {
      params.set("volume", 0.5);
      params.set("pan", 0.0);
  })?;
  ```

## Testing & Debugging Features

### Plugin Testing Framework
- [ ] Automated test runner
  - Load plugin with test configuration
  - Send test MIDI sequences
  - Send test audio signals
  - Verify output matches expectations
- [ ] Performance profiling
  - Measure CPU usage per plugin
  - Track memory allocations
  - Monitor processing latency
- [ ] Regression testing
  - Record parameter automation
  - Replay and verify behavior
- [ ] A/B testing
  - Compare plugin output with reference recordings
  - Null testing for identical processing

### Debug Tools
- [ ] VST3 call logger
  - Trace all COM interface calls
  - Add timestamps and thread IDs
  - Optional filtering by interface type
- [ ] Memory leak detector
  - Track COM reference counts
  - Detect unreleased resources
- [ ] Parameter snapshot comparison
  - Save/load plugin states (Note: Basic implementation was removed due to issues)
  - Diff two states
  - Highlight changes
- [ ] MIDI/Audio recording
  - Capture all input/output
  - Save to WAV/MIDI files
  - Useful for debugging

## Additional Features

### Plugin Metadata & Presets
- [ ] Preset management
  - Save plugin states as JSON/XML
  - Load presets with validation
  - Preset browser UI
- [ ] Plugin database
  - SQLite database of discovered plugins
  - Cache metadata for faster loading
  - Search by name, vendor, category
- [ ] Category tags
  - Instrument, Effect, MIDI processor
  - User-defined tags
- [ ] Compatibility matrix
  - Track which plugins work reliably
  - Known issues/workarounds database

### Performance & Resource Management
- [ ] Plugin benchmarking
  - Measure initialization time
  - Profile processing overhead
  - Memory usage tracking
- [ ] Resource pooling
  - Reuse audio buffers
  - Reduce allocations in audio thread
- [ ] Lazy loading
  - Don't fully initialize until needed
  - Background loading queue
- [ ] Background plugin scanning
  - Scan for new plugins without blocking UI
  - Incremental scanning

### Integration Features
- [ ] OSC (Open Sound Control) support
  - Control parameters via network
  - Bidirectional communication
- [ ] MIDI mapping
  - Learn mode for MIDI CC assignments
  - Save/load mapping configurations
- [ ] Scripting API
  - Lua bindings for automation
  - Python integration for testing
- [ ] REST API
  - HTTP interface for remote control
  - WebSocket for real-time updates

## Library Architecture

### Crate Organization
- [ ] Split into multiple crates:
  - `vst3-sys`: Low-level FFI bindings
  - `vst3-host`: High-level safe API
  - `vst3-graph`: Plugin chaining/routing
  - `vst3-test`: Testing utilities
  - `vst3-gui`: Optional GUI components

### Technical Improvements
- [ ] Error handling with `thiserror`
- [ ] Async support with `tokio` for:
  - Plugin discovery
  - Preset loading
  - Network communication
- [ ] Zero-copy audio processing
  - Use `ndarray` for buffer management
  - Minimize allocations in audio thread
- [ ] Comprehensive documentation
  - API documentation with examples
  - Architecture guide
  - Performance best practices

## Documentation & Examples
- [ ] Plugin compatibility list
  - Document known issues
  - Workarounds for specific plugins
- [ ] Example host implementations
  - Minimal CLI host
  - Benchmarking tool
  - MIDI router
  - Effect chain processor
- [ ] Integration test suite
  - Use free/open source VST3 plugins
  - Automated CI testing
- [ ] Performance guidelines
  - Best practices for low-latency
  - Thread priority configuration
  - Buffer size recommendations