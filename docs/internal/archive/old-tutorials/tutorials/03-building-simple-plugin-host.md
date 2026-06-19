# Tutorial 3: Building a Simple Plugin Host

**Duration: 20 minutes**  
**Prerequisites: Tutorials 1 and 2 completed**

Now let's build an interactive GUI for your VST3 host! In this tutorial, you'll create a simple but complete plugin host with real-time parameter controls, MIDI input, and audio monitoring.

## What You'll Learn

By the end of this tutorial, you'll be able to:
- ✅ Create a GUI application with egui
- ✅ Load plugins interactively with file dialogs
- ✅ Control plugin parameters in real-time
- ✅ Send MIDI notes from a virtual keyboard
- ✅ Monitor audio levels with VU meters
- ✅ Handle plugin GUIs (if supported)
- ✅ Implement emergency controls for audio safety

## GUI Development Concepts

### Why egui?
We're using **egui** because it's:
- **Immediate mode**: Simple to learn and use
- **Cross-platform**: Works on Windows, macOS, and Linux  
- **Audio-friendly**: Doesn't interfere with real-time audio
- **Lightweight**: Minimal overhead

### Key GUI Patterns
- **Immediate Mode**: UI is described every frame, not built once
- **Thread Safety**: GUI runs on main thread, audio on audio thread
- **State Management**: Share data between threads safely

## Setting Up

Update your `Cargo.toml`:

```toml
[dependencies]
vst3-host = { version = "0.1.0", features = ["cpal-backend", "egui-widgets"] }
env_logger = "0.11"
eframe = "0.31"
rfd = "0.15"  # For file dialogs
```

## Complete Simple Plugin Host

Here's our complete simple plugin host:

```rust
// src/main.rs
use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("Simple VST3 Host"),
        ..Default::default()
    };
    
    eframe::run_native(
        "Simple VST3 Host",
        options,
        Box::new(|_cc| Ok(Box::new(SimplePluginHost::new()))),
    )?;
    
    Ok(())
}

struct SimplePluginHost {
    // Core VST3 components
    host: Vst3Host,
    plugin: Option<Arc<Mutex<Plugin>>>,
    plugin_info: Option<PluginInfo>,
    
    // Audio components
    backend: Option<CpalBackend>,
    stream: Option<Box<dyn AudioStream>>,
    is_audio_active: bool,
    
    // Audio monitoring
    levels: Arc<Mutex<AudioLevels>>,
    peak_hold_timer: Instant,
    
    // Parameter control
    parameters: Vec<Parameter>,
    parameter_changes: Vec<(u32, f32)>, // (param_id, new_value)
    
    // MIDI control
    midi_channel: MidiChannel,
    midi_velocity: u8,
    
    // GUI state
    error_message: Option<String>,
    show_parameters: bool,
    show_midi_keyboard: bool,
}

impl SimplePluginHost {
    fn new() -> Self {
        let host = Vst3Host::builder()
            .sample_rate(44100.0)
            .block_size(512)
            .build()
            .expect("Failed to create VST3 host");
            
        Self {
            host,
            plugin: None,
            plugin_info: None,
            backend: None,
            stream: None,
            is_audio_active: false,
            levels: Arc::new(Mutex::new(AudioLevels::new(2))),
            peak_hold_timer: Instant::now(),
            parameters: Vec::new(),
            parameter_changes: Vec::new(),
            midi_channel: MidiChannel::Ch1,
            midi_velocity: 100,
            error_message: None,
            show_parameters: true,
            show_midi_keyboard: true,
        }
    }
    
    fn load_plugin(&mut self, path: std::path::PathBuf) {
        self.error_message = None;
        self.stop_audio();
        
        match self.host.load_plugin(&path) {
            Ok(mut plugin) => {
                // Start plugin processing
                if let Err(e) = plugin.start_processing() {
                    self.error_message = Some(format!("Failed to start plugin processing: {}", e));
                    return;
                }
                
                // Get plugin info and parameters
                let info = plugin.info().clone();
                let params = plugin.get_parameters().unwrap_or_default();
                
                self.plugin_info = Some(info);
                self.parameters = params;
                self.plugin = Some(Arc::new(Mutex::new(plugin)));
                
                // Start audio
                self.start_audio();
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to load plugin: {}", e));
            }
        }
    }
    
    fn start_audio(&mut self) {
        if self.plugin.is_none() {
            return;
        }
        
        // Create backend if needed
        if self.backend.is_none() {
            match CpalBackend::new() {
                Ok(backend) => self.backend = Some(backend),
                Err(e) => {
                    self.error_message = Some(format!("Failed to create audio backend: {}", e));
                    return;
                }
            }
        }
        
        let backend = self.backend.as_ref().unwrap();
        let device = match backend.default_output_device() {
            Some(device) => device,
            None => {
                self.error_message = Some("No audio output device found".to_string());
                return;
            }
        };
        
        let config = AudioConfig {
            sample_rate: 44100.0,
            block_size: 512,
            input_channels: 0,
            output_channels: 2,
        };
        
        // Clone references for audio thread
        let plugin = self.plugin.clone().unwrap();
        let levels = self.levels.clone();
        
        match backend.create_output_stream(
            &device,
            config,
            Box::new(move |output_buffer: &mut [f32]| {
                audio_callback(output_buffer, &plugin, &levels, config);
            }),
            Box::new(|error| {
                eprintln!("Audio error: {}", error);
            }),
        ) {
            Ok(stream) => {
                if let Err(e) = stream.play() {
                    self.error_message = Some(format!("Failed to start audio stream: {}", e));
                    return;
                }
                self.stream = Some(stream);
                self.is_audio_active = true;
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to create audio stream: {}", e));
            }
        }
    }
    
    fn stop_audio(&mut self) {
        if let Some(stream) = &self.stream {
            let _ = stream.pause();
        }
        self.stream = None;
        self.is_audio_active = false;
        
        // Clear levels
        if let Ok(mut levels) = self.levels.lock() {
            for channel in &mut levels.channels {
                channel.peak = 0.0;
                channel.rms = 0.0;
                channel.peak_hold = 0.0;
            }
        }
    }
    
    fn send_midi_note(&mut self, note: u8, velocity: u8) {
        if let Some(plugin) = &self.plugin {
            if let Ok(mut plugin) = plugin.try_lock() {
                let _ = plugin.send_midi_note(note, velocity, self.midi_channel);
            }
        }
    }
    
    fn send_midi_note_off(&mut self, note: u8) {
        if let Some(plugin) = &self.plugin {
            if let Ok(mut plugin) = plugin.try_lock() {
                let _ = plugin.send_midi_note_off(note, self.midi_channel);
            }
        }
    }
    
    fn set_parameter(&mut self, param_id: u32, value: f32) {
        if let Some(plugin) = &self.plugin {
            if let Ok(mut plugin) = plugin.try_lock() {
                let _ = plugin.set_parameter(param_id, value);
            }
        }
    }
    
    fn draw_level_meter(&self, ui: &mut egui::Ui, level: f32, label: &str) {
        ui.horizontal(|ui| {
            ui.label(label);
            
            // Convert to dB
            let db = if level > 0.0001 {
                20.0 * level.log10()
            } else {
                -60.0
            };
            
            // Color based on level
            let color = if db > -3.0 {
                egui::Color32::RED
            } else if db > -12.0 {
                egui::Color32::YELLOW
            } else {
                egui::Color32::GREEN
            };
            
            // Normalize for progress bar (0.0 to 1.0)
            let normalized = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            
            ui.add(
                egui::ProgressBar::new(normalized)
                    .desired_width(200.0)
                    .fill(color)
            );
            
            ui.label(format!("{:.1} dB", db));
        });
    }
    
    fn draw_piano_key(&self, ui: &mut egui::Ui, note: u8, note_name: &str, is_black: bool) -> egui::Response {
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
        
        let stroke = egui::Stroke::new(1.0, egui::Color32::GRAY);
        
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        
        ui.painter().rect(rect, 2.0, color, stroke);
        
        // Draw note name on white keys
        if !is_black {
            ui.painter().text(
                rect.center_bottom() - egui::vec2(0.0, 10.0),
                egui::Align2::CENTER_CENTER,
                note_name,
                egui::FontId::monospace(8.0),
                egui::Color32::BLACK,
            );
        }
        
        response
    }
}

impl eframe::App for SimplePluginHost {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Apply parameter changes
        for (param_id, value) in self.parameter_changes.drain(..) {
            self.set_parameter(param_id, value);
        }
        
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Simple VST3 Plugin Host");
            
            // Plugin loading section
            ui.group(|ui| {
                ui.label("Plugin Management");
                
                ui.horizontal(|ui| {
                    if ui.button("Load Plugin...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("VST3 Plugin", &["vst3"])
                            .pick_file()
                        {
                            self.load_plugin(path);
                        }
                    }
                    
                    if let Some(info) = &self.plugin_info {
                        ui.label(format!("Loaded: {} by {}", info.name, info.vendor));
                        
                        if info.has_gui {
                            if ui.button("Show Plugin GUI").clicked() {
                                // In a real implementation, you'd open the plugin's native GUI here
                                self.error_message = Some("Plugin GUI not implemented in this tutorial".to_string());
                            }
                        }
                    } else {
                        ui.label("No plugin loaded");
                    }
                });
            });
            
            ui.separator();
            
            // Error display
            if let Some(error) = &self.error_message {
                ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
                if ui.button("Clear Error").clicked() {
                    self.error_message = None;
                }
                ui.separator();
            }
            
            // Audio control section
            ui.group(|ui| {
                ui.label("Audio Control");
                
                ui.horizontal(|ui| {
                    let status_text = if self.is_audio_active {
                        "Audio: Active"
                    } else {
                        "Audio: Inactive"
                    };
                    
                    let status_color = if self.is_audio_active {
                        egui::Color32::GREEN
                    } else {
                        egui::Color32::RED
                    };
                    
                    ui.colored_label(status_color, status_text);
                    
                    if self.is_audio_active {
                        if ui.button("Stop Audio").clicked() {
                            self.stop_audio();
                        }
                    } else if self.plugin.is_some() {
                        if ui.button("Start Audio").clicked() {
                            self.start_audio();
                        }
                    }
                    
                    if ui.button("🔇 Audio Panic").clicked() {
                        self.stop_audio();
                    }
                });
                
                // Audio levels
                if self.is_audio_active {
                    ui.separator();
                    ui.label("Output Levels:");
                    
                    if let Ok(levels) = self.levels.lock() {
                        if levels.channels.len() >= 2 {
                            self.draw_level_meter(ui, levels.channels[0].peak, "L");
                            self.draw_level_meter(ui, levels.channels[1].peak, "R");
                        }
                    }
                }
            });
            
            ui.separator();
            
            // Parameter control section
            if self.plugin.is_some() {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Plugin Parameters");
                        ui.checkbox(&mut self.show_parameters, "Show");
                    });
                    
                    if self.show_parameters && !self.parameters.is_empty() {
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .show(ui, |ui| {
                                for param in &self.parameters {
                                    ui.horizontal(|ui| {
                                        ui.label(&param.name);
                                        
                                        let mut value = param.value;
                                        if ui.add(
                                            egui::Slider::new(&mut value, 0.0..=1.0)
                                                .show_value(true)
                                        ).changed() {
                                            self.parameter_changes.push((param.id, value));
                                        }
                                        
                                        ui.label(format!("{:.3}", value));
                                    });
                                }
                            });
                    } else if self.parameters.is_empty() {
                        ui.label("No parameters available");
                    }
                });
                
                ui.separator();
                
                // MIDI control section
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("MIDI Control");
                        ui.checkbox(&mut self.show_midi_keyboard, "Show Keyboard");
                    });
                    
                    if self.show_midi_keyboard {
                        ui.horizontal(|ui| {
                            ui.label("Channel:");
                            let mut channel_index = self.midi_channel.to_index() as usize;
                            if ui.add(egui::Slider::new(&mut channel_index, 0..=15)).changed() {
                                self.midi_channel = MidiChannel::from_index(channel_index as u8)
                                    .unwrap_or(MidiChannel::Ch1);
                            }
                            
                            ui.label("Velocity:");
                            ui.add(egui::Slider::new(&mut self.midi_velocity, 1..=127));
                        });
                        
                        // Simple piano keyboard (just C major scale)
                        ui.label("Virtual Keyboard (C4-B4):");
                        ui.horizontal(|ui| {
                            let notes = [
                                (60, "C4", false), (61, "C#4", true), (62, "D4", false), (63, "D#4", true),
                                (64, "E4", false), (65, "F4", false), (66, "F#4", true), (67, "G4", false),
                                (68, "G#4", true), (69, "A4", false), (70, "A#4", true), (71, "B4", false),
                            ];
                            
                            for (note, name, is_black) in notes {
                                if !is_black { // Only show white keys for simplicity
                                    if self.draw_piano_key(ui, note, name, is_black).clicked() {
                                        self.send_midi_note(note, self.midi_velocity);
                                        
                                        // Auto note-off after a short delay (simplified)
                                        std::thread::spawn({
                                            let plugin = self.plugin.clone();
                                            let channel = self.midi_channel;
                                            move || {
                                                std::thread::sleep(std::time::Duration::from_millis(500));
                                                if let Some(plugin) = plugin {
                                                    if let Ok(mut plugin) = plugin.lock() {
                                                        let _ = plugin.send_midi_note_off(note, channel);
                                                    }
                                                }
                                            }
                                        });
                                    }
                                }
                            }
                        });
                    }
                });
            }
        });
        
        // Request repaint for real-time updates
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

fn audio_callback(
    output_buffer: &mut [f32],
    plugin: &Arc<Mutex<Plugin>>,
    levels: &Arc<Mutex<AudioLevels>>,
    config: AudioConfig,
) {
    // Clear output
    output_buffer.fill(0.0);
    
    // Process through plugin
    if let Ok(mut plugin) = plugin.try_lock() {
        // Create audio buffers
        let mut audio_buffers = AudioBuffers::new(
            config.input_channels,
            config.output_channels,
            config.block_size,
            config.sample_rate,
        );
        
        // Process audio
        if plugin.process_audio(&mut audio_buffers).is_ok() {
            // Copy to output buffer (interleaved format)
            for (frame_idx, output_frame) in output_buffer.chunks_mut(config.output_channels).enumerate() {
                if frame_idx >= config.block_size {
                    break;
                }
                
                for (ch_idx, output_sample) in output_frame.iter_mut().enumerate() {
                    if ch_idx < audio_buffers.outputs.len() && frame_idx < audio_buffers.outputs[ch_idx].len() {
                        *output_sample = audio_buffers.outputs[ch_idx][frame_idx];
                    }
                }
            }
            
            // Update levels
            if let Ok(mut levels) = levels.try_lock() {
                levels.update_from_buffers(&audio_buffers.outputs);
            }
        }
    }
}
```

## Running Your Simple Plugin Host

1. **Build and run:**
   ```bash
   cargo run
   ```

2. **Expected behavior:**
   - A GUI window opens with controls for loading plugins
   - Click "Load Plugin..." to select a VST3 plugin
   - The plugin loads and audio starts automatically
   - Parameter sliders appear for controlling the plugin
   - Virtual keyboard sends MIDI notes to the plugin
   - VU meters show real-time audio levels

## Understanding the Key Components

### 1. Application Structure
```rust
struct SimplePluginHost {
    // Core components
    host: Vst3Host,
    plugin: Option<Arc<Mutex<Plugin>>>,
    
    // Audio system
    backend: Option<CpalBackend>,
    stream: Option<Box<dyn AudioStream>>,
    
    // Real-time data
    levels: Arc<Mutex<AudioLevels>>,
    parameters: Vec<Parameter>,
}
```

### 2. Thread-Safe Communication
```rust
// Audio levels are shared between GUI and audio threads
let levels = Arc<Mutex<AudioLevels>>;

// Parameter changes are queued and applied safely
parameter_changes: Vec<(u32, f32)>
```

### 3. Real-time Audio Processing
```rust
fn audio_callback(output_buffer: &mut [f32], /* ... */) {
    // This runs in the audio thread - keep it fast!
    if let Ok(mut plugin) = plugin.try_lock() {
        plugin.process_audio(&mut audio_buffers)?;
    }
}
```

### 4. Immediate Mode GUI
```rust
impl eframe::App for SimplePluginHost {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Describe the entire UI every frame
        ui.heading("Simple VST3 Plugin Host");
        if ui.button("Load Plugin...").clicked() {
            // Handle button click
        }
    }
}
```

## Key Patterns and Best Practices

### Safe Parameter Updates
```rust
// Queue parameter changes in GUI thread
if slider.changed() {
    self.parameter_changes.push((param.id, new_value));
}

// Apply changes at safe points
for (param_id, value) in self.parameter_changes.drain(..) {
    self.set_parameter(param_id, value);
}
```

### Error Handling
```rust
match self.host.load_plugin(&path) {
    Ok(plugin) => {
        // Success path
    }
    Err(e) => {
        self.error_message = Some(format!("Failed to load plugin: {}", e));
    }
}
```

### Audio Safety
```rust
if ui.button("🔇 Audio Panic").clicked() {
    self.stop_audio(); // Immediately stop all audio
}
```

### MIDI Note Management
```rust
// Send note on
self.send_midi_note(note, velocity);

// Auto note-off after delay
std::thread::spawn(move || {
    std::thread::sleep(Duration::from_millis(500));
    // Send note off
});
```

## Extending Your Host

### Add Plugin Preset Management
```rust
// Save preset
let preset_data = plugin.get_state()?;
std::fs::write("preset.json", serde_json::to_string(&preset_data)?)?;

// Load preset  
let preset_data = std::fs::read_to_string("preset.json")?;
let data: serde_json::Value = serde_json::from_str(&preset_data)?;
plugin.set_state(&data)?;
```

### Add Multiple Plugin Support
```rust
struct PluginSlot {
    plugin: Arc<Mutex<Plugin>>,
    info: PluginInfo,
    parameters: Vec<Parameter>,
    is_bypassed: bool,
}

struct SimplePluginHost {
    plugin_slots: Vec<PluginSlot>,
    active_slot: usize,
}
```

### Add Audio Recording
```rust
struct AudioRecorder {
    samples: Vec<f32>,
    is_recording: bool,
}

// In audio callback
if recorder.is_recording {
    recorder.samples.extend_from_slice(output_buffer);
}
```

## Troubleshooting

### GUI Not Responding
- Check for blocking operations in the GUI thread
- Use `try_lock()` instead of `lock()` to avoid deadlocks

### Audio Dropouts
- Keep audio callback minimal and fast
- Avoid memory allocations in audio thread

### Parameter Changes Not Working
- Ensure parameter IDs are correct
- Check that plugin supports parameter automation

## What's Next?

You now have a functional plugin host with GUI! In the next tutorial, we'll add advanced features like process isolation and better plugin discovery.

**Coming up in Tutorial 4**: Advanced features including process isolation for problematic plugins and sophisticated plugin discovery.

## Key Concepts Learned

- **Immediate Mode GUI**: Describing UI every frame vs. building once
- **Thread Safety**: Safe communication between GUI and audio threads
- **Real-time Constraints**: Keeping audio thread fast and predictable
- **Event Handling**: Managing user interactions and audio events
- **Error Management**: Graceful handling of plugin and audio errors
- **MIDI Integration**: Virtual keyboards and note management

You've built a complete, interactive VST3 plugin host! Next, we'll make it production-ready with advanced features.