# VST3 Host Library Implementation Plan

## Phase 1: Core Extraction

### 1.1 Create Safe Wrappers
- [ ] Extract plugin discovery code into `discovery.rs`
- [ ] Create `Plugin` struct that wraps all unsafe COM operations
- [ ] Implement `PluginInfo` for metadata without loading
- [ ] Create safe parameter access API
- [ ] Wrap MIDI event handling

### 1.2 Error Types
- [ ] Define comprehensive error enum using `thiserror`
- [ ] Convert all String errors to proper types
- [ ] Add context to errors for debugging

### 1.3 Core Types
```rust
// In lib.rs
pub mod prelude {
    pub use crate::{
        Vst3Host, Plugin, PluginInfo, Parameter,
        Error, Result,
        MidiEvent, MidiChannel, AudioBuffers,
    };
}
```

## Phase 2: Audio Backend Abstraction

### 2.1 Define AudioBackend Trait
```rust
pub trait AudioBackend: Send + 'static {
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn process(&mut self, callback: &mut dyn FnMut(&mut AudioBuffers)) -> Result<()>;
}
```

### 2.2 CPAL Implementation
- [ ] Move current CPAL code to `backends/cpal.rs`
- [ ] Implement AudioBackend for CpalBackend
- [ ] Add configuration options
- [ ] Handle device selection

## Phase 3: Safety & Stability

### 3.1 Process Isolation
- [ ] Move helper process code to library
- [ ] Make process isolation optional but default
- [ ] Add configuration for isolation level

### 3.2 Thread Safety
- [ ] Ensure Plugin is Send + Sync where appropriate
- [ ] Add proper locking for shared state
- [ ] Document thread safety guarantees

## Phase 4: Feature Flags

```toml
[features]
default = ["cpal-backend", "process-isolation"]
cpal-backend = ["cpal"]
process-isolation = []
egui-widgets = ["egui"]
```

## Phase 5: Testing

### 5.1 Unit Tests
- [ ] Test plugin discovery
- [ ] Test parameter manipulation
- [ ] Test MIDI event creation
- [ ] Test error conditions

### 5.2 Integration Tests
- [ ] Test with free VST3 plugins
- [ ] Test crash recovery
- [ ] Test concurrent plugin loading
- [ ] Test audio processing

## Phase 6: Documentation

### 6.1 API Documentation
- [ ] Document all public types
- [ ] Add examples to each method
- [ ] Create module-level documentation

### 6.2 Examples
- [ ] Basic plugin host
- [ ] MIDI keyboard
- [ ] Parameter automation
- [ ] Multi-plugin router

## Migration Strategy

### Current Code Structure
```
src/
├── main.rs (4500+ lines) → Will become example/demo
├── audio_processing.rs → Move to lib
├── com_implementations.rs → Hide in implementation
├── crash_protection.rs → Move to lib
├── data_structures.rs → Refactor into public API
├── plugin_discovery.rs → Move to lib
├── plugin_host_process.rs → Move to lib
└── utils.rs → Some public, some private
```

### New Library Structure
```
vst3-host/
├── Cargo.toml
├── src/
│   ├── lib.rs (public API)
│   ├── plugin.rs (Plugin struct and methods)
│   ├── host.rs (Vst3Host struct)
│   ├── discovery.rs (plugin discovery)
│   ├── parameters.rs (parameter handling)
│   ├── midi.rs (MIDI types and handling)
│   ├── audio.rs (audio types)
│   ├── error.rs (error types)
│   ├── backends/
│   │   ├── mod.rs
│   │   └── cpal.rs
│   └── internal/ (private implementation details)
│       ├── com.rs
│       ├── process_isolation.rs
│       └── utils.rs
├── examples/
│   ├── basic_host.rs
│   ├── midi_keyboard.rs
│   └── plugin_scanner.rs
└── tests/
    └── integration_tests.rs
```

## Key Decisions

1. **Hide COM Complexity**: Users should never see ComPtr, IComponent, etc.
2. **Sensible Defaults**: 
   - 44.1kHz sample rate
   - 512 sample block size
   - Stereo output
   - Process isolation enabled
3. **Progressive Disclosure**: Simple API for common cases, advanced API available
4. **Zero Unsafe in Public API**: All unsafe code contained in internal modules

## Success Criteria

- [ ] Can load and play a VST3 plugin in under 10 lines of code
- [ ] No unsafe code required by library users
- [ ] Plugin crashes don't crash the host
- [ ] Works on Windows, macOS, and Linux
- [ ] Performance within 5% of native implementation
- [ ] Comprehensive documentation with examples