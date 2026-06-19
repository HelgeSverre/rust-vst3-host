# VST3 Host Quick Reference

This reference provides quick access to common patterns, APIs, and solutions for VST3 hosting in Rust.

## Quick Start

### Minimal VST3 Host (5 lines)
```rust
use vst3_host::prelude::*;

let mut host = Vst3Host::new()?;
let plugins = host.discover_plugins()?;
let mut plugin = host.load_plugin(&plugins[0].path)?;
plugin.start_processing()?;
```

### With Audio Processing
```rust
use vst3_host::prelude::*;

// Create host with audio backend
let mut host = Vst3Host::builder()
    .sample_rate(44100.0)
    .block_size(512)
    .build()?;

// Load and start plugin
let mut plugin = host.load_plugin("path/to/plugin.vst3")?;
plugin.start_processing()?;

// Create audio stream
let backend = CpalBackend::new()?;
let device = backend.default_output_device().unwrap();
let config = AudioConfig {
    sample_rate: 44100.0,
    block_size: 512,
    input_channels: 0,
    output_channels: 2,
};

let stream = backend.create_output_stream(
    &device,
    config,
    Box::new(move |output: &mut [f32]| {
        // Audio processing callback
    }),
    Box::new(|err| eprintln!("Audio error: {}", err)),
)?;

stream.play()?;
```

## Common Patterns

### Plugin Discovery and Filtering
```rust
// Discover all plugins
let all_plugins = host.discover_plugins()?;

// Filter by category
let instruments: Vec<_> = all_plugins.iter()
    .filter(|p| p.category.contains("Instrument"))
    .collect();

// Filter by capabilities
let midi_instruments: Vec<_> = all_plugins.iter()
    .filter(|p| p.has_midi_input && p.audio_outputs > 0)
    .collect();

// Find by name
let plugin = all_plugins.iter()
    .find(|p| p.name.contains("Reverb"))
    .ok_or("Plugin not found")?;
```

### Safe Plugin Loading
```rust
// Basic loading with error handling
match host.load_plugin(&plugin_path) {
    Ok(mut plugin) => {
        if let Err(e) = plugin.start_processing() {
            eprintln!("Failed to start plugin: {}", e);
        } else {
            println!("Plugin loaded successfully!");
        }
    }
    Err(vst3_host::Error::PluginLoadFailed { path, reason }) => {
        eprintln!("Failed to load {}: {}", path.display(), reason);
    }
    Err(e) => {
        eprintln!("Unexpected error: {}", e);
    }
}

// With process isolation
let host = Vst3Host::builder()
    .with_process_isolation(true)
    .build()?;
```

### Parameter Management
```rust
// Get all parameters
let params = plugin.get_parameters()?;

// Find parameter by name
let gain_param = params.iter()
    .find(|p| p.name.to_lowercase().contains("gain"))
    .ok_or("Gain parameter not found")?;

// Set parameter value (0.0 to 1.0)
plugin.set_parameter(gain_param.id, 0.8)?;

// Parameter automation
for (time, value) in automation_curve {
    plugin.set_parameter(param_id, value)?;
    std::thread::sleep(Duration::from_millis(10));
}
```

### MIDI Handling
```rust
// Send MIDI note
plugin.send_midi_note(60, 127, MidiChannel::Ch1)?; // C4, full velocity

// Send MIDI CC
plugin.send_midi_cc(1, 64, MidiChannel::Ch1)?; // Modulation wheel, center

// Send program change
plugin.send_midi_program_change(5, MidiChannel::Ch1)?;

// MIDI panic (all notes off)
plugin.midi_panic()?;

// Custom MIDI event
let event = MidiEvent::ControlChange {
    channel: MidiChannel::Ch1,
    controller: 7, // Volume
    value: 100,
};
plugin.send_midi_event(event)?;
```

### Audio Buffer Management
```rust
// Create audio buffers
let mut buffers = AudioBuffers::new(
    input_channels: 2,
    output_channels: 2,
    block_size: 512,
    sample_rate: 44100.0,
);

// Fill input buffers with test signal
for (i, sample) in buffers.inputs[0].iter_mut().enumerate() {
    *sample = (i as f32 * 440.0 * 2.0 * PI / 44100.0).sin() * 0.5;
}

// Process through plugin
plugin.process_audio(&mut buffers)?;

// Access output
let left_output = &buffers.outputs[0];
let right_output = &buffers.outputs[1];
```

### Plugin State Management
```rust
// Save plugin state
let state = plugin.get_state()?;
let state_json = serde_json::to_string(&state)?;
std::fs::write("plugin_preset.json", state_json)?;

// Load plugin state
let state_json = std::fs::read_to_string("plugin_preset.json")?;
let state: serde_json::Value = serde_json::from_str(&state_json)?;
plugin.set_state(&state)?;
```

### Multi-Plugin Chains
```rust
struct PluginChain {
    plugins: Vec<Plugin>,
}

impl PluginChain {
    fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        for plugin in &mut self.plugins {
            plugin.process_audio(buffers)?;
        }
        Ok(())
    }
    
    fn add_plugin(&mut self, plugin: Plugin) {
        self.plugins.push(plugin);
    }
    
    fn remove_plugin(&mut self, index: usize) {
        if index < self.plugins.len() {
            self.plugins.remove(index);
        }
    }
}
```

### Thread-Safe Plugin Access
```rust
use std::sync::{Arc, Mutex};

let plugin = Arc::new(Mutex::new(plugin));
let plugin_clone = plugin.clone();

// Audio thread
std::thread::spawn(move || {
    loop {
        if let Ok(mut plugin) = plugin_clone.try_lock() {
            // Process audio - don't block if locked
            let _ = plugin.process_audio(&mut buffers);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
});

// GUI thread
if let Ok(mut plugin) = plugin.lock() {
    plugin.set_parameter(param_id, new_value)?;
}
```

## Audio Configuration

### Common Sample Rates
```rust
let configs = [
    (44100.0, "CD Quality"),
    (48000.0, "Professional"),
    (88200.0, "High Resolution"),
    (96000.0, "Studio"),
    (192000.0, "Ultra High"),
];
```

### Buffer Sizes and Latency
```rust
fn calculate_latency(block_size: usize, sample_rate: f64) -> Duration {
    Duration::from_secs_f64(block_size as f64 / sample_rate)
}

// Common buffer sizes
let buffer_configs = [
    (64, "Ultra Low Latency"),    // ~1.5ms @ 44.1kHz
    (128, "Low Latency"),         // ~2.9ms @ 44.1kHz
    (256, "Normal"),              // ~5.8ms @ 44.1kHz
    (512, "Stable"),              // ~11.6ms @ 44.1kHz
    (1024, "High Stability"),     // ~23.2ms @ 44.1kHz
];
```

### Audio Device Selection
```rust
let backend = CpalBackend::new()?;

// List available devices
let output_devices = backend.enumerate_output_devices()?;
for (i, device) in output_devices.iter().enumerate() {
    println!("{}: {}", i, device.name().unwrap_or("Unknown"));
}

// Use specific device
let device = &output_devices[device_index];
let stream = backend.create_output_stream(device, config, callback, error_callback)?;
```

## Error Handling

### Common Error Types
```rust
match error {
    vst3_host::Error::PluginNotFound(path) => {
        eprintln!("Plugin not found: {}", path);
    }
    vst3_host::Error::PluginLoadFailed { path, reason } => {
        eprintln!("Failed to load {}: {}", path.display(), reason);
    }
    vst3_host::Error::AudioBackendError(msg) => {
        eprintln!("Audio system error: {}", msg);
    }
    vst3_host::Error::ParameterError { param_id, reason } => {
        eprintln!("Parameter {} error: {}", param_id, reason);
    }
    vst3_host::Error::MidiError(msg) => {
        eprintln!("MIDI error: {}", msg);
    }
    vst3_host::Error::ProcessingError(msg) => {
        eprintln!("Processing error: {}", msg);
    }
}
```

### Graceful Error Recovery
```rust
// Plugin loading with fallback
fn load_plugin_with_fallback(host: &Vst3Host, primary_path: &Path, fallback_path: &Path) -> Result<Plugin> {
    host.load_plugin(primary_path)
        .or_else(|_| {
            eprintln!("Primary plugin failed, trying fallback...");
            host.load_plugin(fallback_path)
        })
}

// Audio processing with error recovery
fn safe_audio_process(plugin: &mut Plugin, buffers: &mut AudioBuffers) {
    if let Err(e) = plugin.process_audio(buffers) {
        eprintln!("Audio processing failed: {}", e);
        // Clear output to prevent noise
        for channel in &mut buffers.outputs {
            channel.fill(0.0);
        }
    }
}
```

## Performance Optimization

### CPU Usage Monitoring
```rust
use std::time::Instant;

fn monitor_plugin_performance(plugin: &mut Plugin, buffers: &mut AudioBuffers) -> f32 {
    let start = Instant::now();
    let _ = plugin.process_audio(buffers);
    let processing_time = start.elapsed();
    
    // Calculate CPU usage as percentage of available time
    let buffer_duration = Duration::from_secs_f64(buffers.block_size as f64 / buffers.sample_rate);
    (processing_time.as_secs_f64() / buffer_duration.as_secs_f64() * 100.0) as f32
}
```

### Memory Management
```rust
// Pre-allocate buffers to avoid real-time allocations
struct BufferPool {
    buffers: Vec<AudioBuffers>,
    current: usize,
}

impl BufferPool {
    fn new(count: usize, channels: usize, block_size: usize, sample_rate: f64) -> Self {
        let buffers = (0..count)
            .map(|_| AudioBuffers::new(channels, channels, block_size, sample_rate))
            .collect();
        Self { buffers, current: 0 }
    }
    
    fn get(&mut self) -> &mut AudioBuffers {
        let buffer = &mut self.buffers[self.current];
        self.current = (self.current + 1) % self.buffers.len();
        buffer.clear();
        buffer
    }
}
```

### Real-time Safe Operations
```rust
// Good: Non-blocking operations
if let Ok(mut plugin) = plugin.try_lock() {
    plugin.process_audio(buffers)?;
}

// Bad: Blocking operations in audio thread
let mut plugin = plugin.lock(); // Can block!
plugin.process_audio(buffers)?;

// Good: Pre-allocated data structures
let mut params = Vec::with_capacity(128);

// Bad: Dynamic allocation in audio thread
let mut params = Vec::new(); // Can allocate!
```

## GUI Integration with egui

### Basic Plugin Control Panel
```rust
fn draw_plugin_controls(ui: &mut egui::Ui, plugin: &Arc<Mutex<Plugin>>) {
    if let Ok(plugin) = plugin.try_lock() {
        if let Ok(params) = plugin.get_parameters() {
            for param in params {
                ui.horizontal(|ui| {
                    ui.label(&param.name);
                    
                    let mut value = param.value;
                    if ui.add(egui::Slider::new(&mut value, 0.0..=1.0)).changed() {
                        let _ = plugin.set_parameter(param.id, value);
                    }
                    
                    ui.label(format!("{:.3}", value));
                });
            }
        }
    }
}
```

### Real-time Audio Levels
```rust
fn draw_level_meter(ui: &mut egui::Ui, level: f32, label: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        
        let db = if level > 0.0001 { 20.0 * level.log10() } else { -60.0 };
        let color = if db > -3.0 { 
            egui::Color32::RED 
        } else if db > -12.0 { 
            egui::Color32::YELLOW 
        } else { 
            egui::Color32::GREEN 
        };
        
        let normalized = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        ui.add(egui::ProgressBar::new(normalized).fill(color));
        ui.label(format!("{:.1} dB", db));
    });
}
```

### Virtual MIDI Keyboard
```rust
fn draw_midi_keyboard(ui: &mut egui::Ui, plugin: &Arc<Mutex<Plugin>>) {
    ui.horizontal(|ui| {
        for note in 60..73 { // C4 to C5
            let note_name = note_to_name(note);
            let is_black = note_name.contains('#');
            
            let size = if is_black { 
                egui::vec2(20.0, 60.0) 
            } else { 
                egui::vec2(30.0, 100.0) 
            };
            
            let color = if is_black { 
                egui::Color32::BLACK 
            } else { 
                egui::Color32::WHITE 
            };
            
            if ui.add_sized(size, egui::Button::new("").fill(color)).clicked() {
                if let Ok(mut plugin) = plugin.try_lock() {
                    let _ = plugin.send_midi_note(note, 100, MidiChannel::Ch1);
                }
            }
        }
    });
}
```

## Testing and Debugging

### Plugin Validation
```rust
fn validate_plugin(plugin_path: &Path) -> Result<PluginInfo> {
    let mut host = Vst3Host::new()?;
    let mut plugin = host.load_plugin(plugin_path)?;
    
    // Test basic operations
    plugin.start_processing()?;
    
    let mut buffers = AudioBuffers::new(2, 2, 512, 44100.0);
    plugin.process_audio(&mut buffers)?;
    
    plugin.stop_processing()?;
    
    Ok(plugin.info().clone())
}
```

### Audio Testing
```rust
fn test_audio_processing() {
    let mut buffers = AudioBuffers::new(2, 2, 512, 44100.0);
    
    // Generate test signal
    for (i, sample) in buffers.inputs[0].iter_mut().enumerate() {
        *sample = (i as f32 * 440.0 * 2.0 * PI / 44100.0).sin() * 0.5;
    }
    
    // Process and validate
    plugin.process_audio(&mut buffers).unwrap();
    
    // Check for NaN or infinity
    for channel in &buffers.outputs {
        for &sample in channel {
            assert!(sample.is_finite(), "Plugin produced invalid audio");
        }
    }
}
```

### Performance Benchmarking
```rust
use std::time::{Duration, Instant};

fn benchmark_plugin(plugin: &mut Plugin, iterations: usize) -> Duration {
    let mut buffers = AudioBuffers::new(2, 2, 512, 44100.0);
    
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = plugin.process_audio(&mut buffers);
    }
    start.elapsed()
}
```

## Deployment

### Cross-Platform Builds
```bash
# Install targets
rustup target add x86_64-pc-windows-gnu
rustup target add x86_64-apple-darwin
rustup target add x86_64-unknown-linux-gnu

# Build for different platforms
cargo build --release --target x86_64-pc-windows-gnu
cargo build --release --target x86_64-apple-darwin
cargo build --release --target x86_64-unknown-linux-gnu
```

### Feature Configuration
```toml
[features]
default = ["cpal-backend"]
cpal-backend = ["cpal"]
process-isolation = []
egui-widgets = ["egui"]
jack-backend = ["jack"]

# Conditional compilation
[target.'cfg(target_os = "macos")'.dependencies]
cocoa = "0.26"

[target.'cfg(target_os = "windows")'.dependencies]
winapi = "0.3"
```

### Plugin Compatibility Database
```rust
// plugins.json
{
  "plugins": [
    {
      "name": "Heavy Synth",
      "vendor": "SomeCompany",
      "recommended_isolation": true,
      "max_cpu_usage": 25.0,
      "notes": "Can cause crashes with certain parameter combinations"
    }
  ]
}
```

This quick reference covers the most common patterns and use cases. For more detailed information, refer to the full tutorial series!