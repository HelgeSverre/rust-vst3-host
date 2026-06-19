# VST3 Host Library Design Document

## Overview

This document outlines the design for extracting a safe, easy-to-use Rust library for VST3 plugin hosting from our existing VST3 host application. The library prioritizes simplicity, safety, and "works out of the box" functionality.

## Core Philosophy

- **Safe by default**: All unsafe VST3 COM interactions hidden behind safe Rust APIs
- **Simple to use**: Minimal boilerplate, sensible defaults
- **Batteries included**: Comes with CPAL audio backend, ready to make sound
- **Extensible**: Easy integration with custom audio backends and UI frameworks

## Library Architecture

```
vst3-host (main crate)
├── Core functionality (safe wrappers)
├── Plugin discovery & loading
├── Parameter management
├── MIDI event handling
└── Audio processing

vst3-host-cpal (optional feature/subcrate)
└── Ready-to-use CPAL audio backend

vst3-host-egui (optional feature/subcrate)
└── egui integration helpers
```

## Basic Usage Examples

### 1. Minimal Example - Load and Play a Plugin

```rust
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a host with default audio backend (CPAL)
    let mut host = Vst3Host::new()?;
    
    // Discover all plugins on the system
    let plugins = host.discover_plugins()?;
    println!("Found {} plugins", plugins.len());
    
    // Load a plugin
    let mut plugin = host.load_plugin(&plugins[0].path)?;
    
    // Start audio processing
    plugin.start_processing()?;
    
    // Send a MIDI note
    plugin.send_midi_note(60, 127, MidiChannel::Ch1)?;  // Middle C, velocity 127
    
    // Keep playing for 2 seconds
    std::thread::sleep(std::time::Duration::from_secs(2));
    
    // Send note off
    plugin.send_midi_note_off(60, MidiChannel::Ch1)?;
    
    Ok(())
}
```

### 2. Parameter Manipulation

```rust
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Vst3Host::new()?;
    let mut plugin = host.load_plugin("/path/to/plugin.vst3")?;
    
    // Get all parameters
    let params = plugin.get_parameters()?;
    for param in &params {
        println!("{}: {} = {}", param.id, param.name, param.value);
    }
    
    // Set a parameter by name
    plugin.set_parameter_by_name("Cutoff", 0.5)?;
    
    // Listen for parameter changes
    plugin.on_parameter_change(|param_id, value| {
        println!("Parameter {} changed to {}", param_id, value);
    });
    
    Ok(())
}
```

### 3. Plugin Discovery with Metadata

```rust
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = Vst3Host::new()?;
    
    // Add custom scan paths
    host.add_scan_path("/my/custom/vst3/folder")?;
    
    // Discover with progress callback
    let plugins = host.discover_plugins_with_progress(|progress| {
        println!("Scanning: {}%", progress.percentage);
        println!("Current: {}", progress.current_plugin);
    })?;
    
    // Access plugin metadata
    for plugin_info in &plugins {
        println!("Plugin: {} by {}", plugin_info.name, plugin_info.vendor);
        println!("  Category: {}", plugin_info.category);
        println!("  Audio: {} in, {} out", plugin_info.audio_inputs, plugin_info.audio_outputs);
        println!("  MIDI: {}", plugin_info.has_midi_input);
    }
    
    Ok(())
}
```

### 4. Custom Audio Backend Integration

```rust
use vst3_host::prelude::*;

struct MyCustomAudioBackend {
    // Your audio backend implementation
}

impl AudioBackend for MyCustomAudioBackend {
    fn process(&mut self, plugin: &mut Plugin, data: &mut AudioBuffers) -> Result<()> {
        // Fill input buffers with your audio
        // Let plugin process
        plugin.process_audio(data)?;
        // Send output buffers to your audio system
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = MyCustomAudioBackend::new();
    let mut host = Vst3Host::with_backend(backend)?;
    // ... use as normal
    Ok(())
}
```

### 5. egui Integration

```rust
use vst3_host::prelude::*;
use vst3_host_egui::PluginWidget;

fn ui_example(ctx: &egui::Context, plugin: &mut Plugin) {
    egui::Window::new("VST3 Plugin").show(ctx, |ui| {
        // Automatic parameter UI
        ui.add(PluginWidget::new(plugin));
        
        // Or manual control
        if ui.button("Send Middle C").clicked() {
            plugin.send_midi_note(60, 127, MidiChannel::Ch1).ok();
        }
        
        // VU meter widget
        let levels = plugin.get_output_levels();
        ui.add(vst3_host_egui::VuMeter::new(&levels));
    });
}
```

## Core API Types

### Plugin Discovery

```rust
pub struct PluginInfo {
    pub path: PathBuf,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub category: String,
    pub uid: String,
    pub audio_inputs: u32,
    pub audio_outputs: u32,
    pub has_midi_input: bool,
    pub has_midi_output: bool,
    pub has_gui: bool,
}
```

### Plugin Instance

```rust
pub struct Plugin {
    // Opaque handle to plugin internals
}

impl Plugin {
    // Parameter access
    pub fn get_parameters(&self) -> Result<Vec<Parameter>>;
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()>;
    pub fn set_parameter_by_name(&mut self, name: &str, value: f64) -> Result<()>;
    
    // MIDI
    pub fn send_midi_note(&mut self, note: u8, velocity: u8, channel: MidiChannel) -> Result<()>;
    pub fn send_midi_cc(&mut self, cc: u8, value: u8, channel: MidiChannel) -> Result<()>;
    pub fn send_midi_event(&mut self, event: MidiEvent) -> Result<()>;
    
    // Audio
    pub fn start_processing(&mut self) -> Result<()>;
    pub fn stop_processing(&mut self) -> Result<()>;
    pub fn process_audio(&mut self, buffers: &mut AudioBuffers) -> Result<()>;
    
    // Monitoring
    pub fn get_output_levels(&self) -> AudioLevels;
    pub fn is_processing(&self) -> bool;
    
    // GUI (if available)
    pub fn has_editor(&self) -> bool;
    pub fn open_editor(&mut self, parent: WindowHandle) -> Result<()>;
}
```

### Audio Types

```rust
pub struct AudioBuffers {
    pub inputs: Vec<Vec<f32>>,   // [channel][sample]
    pub outputs: Vec<Vec<f32>>,  // [channel][sample]
    pub sample_rate: f64,
    pub block_size: usize,
}

pub struct AudioLevels {
    pub channels: Vec<ChannelLevel>,
}

pub struct ChannelLevel {
    pub peak: f32,      // 0.0 to 1.0
    pub rms: f32,       // 0.0 to 1.0
    pub peak_hold: f32, // 0.0 to 1.0
}
```

## Integration Guides

### With CPAL (Default)

The library comes with CPAL integration out of the box:

```rust
// Automatic CPAL setup with sensible defaults
let host = Vst3Host::new()?;

// Or with custom configuration
let host = Vst3Host::builder()
    .sample_rate(48000)
    .block_size(512)
    .build()?;
```

### With JACK

```rust
use vst3_host::prelude::*;
use vst3_host_jack::JackBackend;

let backend = JackBackend::new("MyApp")?;
let host = Vst3Host::with_backend(backend)?;
```

### With egui

```rust
// In your egui app
use vst3_host_egui::prelude::*;

struct MyApp {
    host: Vst3Host,
    plugin: Option<Plugin>,
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // Plugin selector
            if ui.button("Load Plugin").clicked() {
                if let Ok(plugins) = self.host.discover_plugins() {
                    // Show plugin selector dialog
                }
            }
            
            // Plugin UI
            if let Some(plugin) = &mut self.plugin {
                ui.add(PluginControlPanel::new(plugin));
            }
        });
    }
}
```

### With tauri

```rust
// In your Tauri command
#[tauri::command]
fn load_plugin(path: String, state: State<AppState>) -> Result<PluginInfo, String> {
    let mut host = state.host.lock().unwrap();
    let plugin = host.load_plugin(&path).map_err(|e| e.to_string())?;
    Ok(plugin.get_info())
}
```

## Safety Features

1. **Crash Protection**: Plugins run in separate processes by default
2. **Timeout Protection**: Automatic timeout for hung plugins
3. **Memory Safety**: All COM interactions wrapped in safe Rust
4. **Thread Safety**: Safe concurrent access to plugin instances
5. **Resource Management**: Automatic cleanup with RAII

## Performance Considerations

- Zero-copy audio processing where possible
- Lock-free audio thread operations
- Efficient parameter caching
- Background plugin scanning
- Process pooling for multiple plugins

## Error Handling

```rust
use vst3_host::Error;

match host.load_plugin(path) {
    Ok(plugin) => { /* use plugin */ },
    Err(Error::PluginNotFound) => { /* handle */ },
    Err(Error::PluginLoadFailed(msg)) => { /* handle */ },
    Err(Error::PluginCrashed) => { /* handle */ },
    Err(e) => { /* handle other errors */ },
}
```

## Future Extensions

- Plugin preset management
- Plugin state serialization
- DAW-style plugin chains
- Network-transparent plugin hosting
- Plugin UI embedding helpers for more frameworks