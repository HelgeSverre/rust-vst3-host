//! VST3 host with EGUI interface

use eframe::egui;
use std::sync::{Arc, Mutex};
use vst3_host::prelude::*;
use vst3_host::window::PluginWindow;

#[cfg(feature = "cpal-backend")]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

struct App {
    host: Arc<Mutex<Vst3Host>>,
    plugin: Option<Arc<Mutex<Plugin>>>,
    plugin_info: Option<PluginInfo>,
    plugin_path: Option<std::path::PathBuf>,
    is_processing: bool,
    show_parameters: bool,
    midi_channel: u8,
    keyboard_octave: u8,
    last_error: Option<String>,
    // Track which notes are currently pressed
    pressed_notes: std::collections::HashSet<u8>,
    // Audio levels for visualization
    audio_levels: AudioLevels,
    #[cfg(feature = "cpal-backend")]
    audio_stream: Option<cpal::Stream>,
    // Plugin GUI window
    plugin_window: Option<PluginWindow>,
    // Store current parameter values for display
    current_parameter_values: std::collections::HashMap<u32, f64>,
}

impl Default for App {
    fn default() -> Self {
        Self::new(None)
    }
}

impl App {
    fn new(plugin_path: Option<std::path::PathBuf>) -> Self {
        let host = Vst3Host::new().unwrap_or_else(|e| {
            eprintln!("Failed to create VST3 host: {}", e);
            std::process::exit(1);
        });
        
        let mut app = Self {
            host: Arc::new(Mutex::new(host)),
            plugin: None,
            plugin_info: None,
            plugin_path: None,
            is_processing: false,
            show_parameters: false,
            midi_channel: 0,
            keyboard_octave: 4,
            last_error: None,
            pressed_notes: std::collections::HashSet::new(),
            audio_levels: AudioLevels::new(2),
            #[cfg(feature = "cpal-backend")]
            audio_stream: None,
            plugin_window: None,
            current_parameter_values: std::collections::HashMap::new(),
        };

        // Load plugin if path provided
        if let Some(path) = plugin_path {
            println!("Loading plugin: {}", path.display());
            app.load_plugin(&path);
            if app.last_error.is_some() {
                eprintln!("Failed to load plugin: {:?}", app.last_error);
            } else {
                println!("Plugin loaded successfully!");
            }
        }

        app
    }
}

impl App {
    fn send_note_on(&mut self, note: u8, velocity: u8) {
        if let Some(ref mut plugin) = self.plugin {
            if let Ok(mut plugin_lock) = plugin.lock() {
                let channel = match self.midi_channel {
                    0 => MidiChannel::Ch1, 1 => MidiChannel::Ch2, 2 => MidiChannel::Ch3, 3 => MidiChannel::Ch4,
                    4 => MidiChannel::Ch5, 5 => MidiChannel::Ch6, 6 => MidiChannel::Ch7, 7 => MidiChannel::Ch8,
                    8 => MidiChannel::Ch9, 9 => MidiChannel::Ch10, 10 => MidiChannel::Ch11, 11 => MidiChannel::Ch12,
                    12 => MidiChannel::Ch13, 13 => MidiChannel::Ch14, 14 => MidiChannel::Ch15, 15 => MidiChannel::Ch16,
                    _ => MidiChannel::Ch1,
                };
                match plugin_lock.send_midi_note(note, velocity, channel) {
                    Ok(()) => {
                        eprintln!("MIDI Note ON sent: note={}, velocity={}, channel={:?}", note, velocity, channel);
                    }
                    Err(e) => {
                        eprintln!("Failed to send MIDI note: {}", e);
                        self.last_error = Some(format!("Failed to send MIDI note: {}", e));
                    }
                }
            }
        }
    }

    fn send_note_off(&mut self, note: u8) {
        if let Some(ref mut plugin) = self.plugin {
            if let Ok(mut plugin_lock) = plugin.lock() {
                let channel = match self.midi_channel {
                    0 => MidiChannel::Ch1, 1 => MidiChannel::Ch2, 2 => MidiChannel::Ch3, 3 => MidiChannel::Ch4,
                    4 => MidiChannel::Ch5, 5 => MidiChannel::Ch6, 6 => MidiChannel::Ch7, 7 => MidiChannel::Ch8,
                    8 => MidiChannel::Ch9, 9 => MidiChannel::Ch10, 10 => MidiChannel::Ch11, 11 => MidiChannel::Ch12,
                    12 => MidiChannel::Ch13, 13 => MidiChannel::Ch14, 14 => MidiChannel::Ch15, 15 => MidiChannel::Ch16,
                    _ => MidiChannel::Ch1,
                };
                match plugin_lock.send_midi_note_off(note, channel) {
                    Ok(()) => {
                        eprintln!("MIDI Note OFF sent: note={}, channel={:?}", note, channel);
                    }
                    Err(e) => {
                        eprintln!("Failed to send MIDI note off: {}", e);
                        self.last_error = Some(format!("Failed to send MIDI note off: {}", e));
                    }
                }
            }
        }
    }

    fn load_plugin(&mut self, path: &std::path::Path) {
        let plugin_result = {
            if let Ok(mut host) = self.host.lock() {
                host.load_plugin(path)
            } else {
                return;
            }
        };
        
        match plugin_result {
            Ok(plugin) => {
                self.plugin_info = Some(plugin.info().clone());
                
                // Start processing if plugin was loaded successfully
                let plugin_wrapped = Arc::new(Mutex::new(plugin));
                if let Ok(mut plugin_lock) = plugin_wrapped.lock() {
                    if let Err(e) = plugin_lock.start_processing() {
                        self.last_error = Some(format!("Failed to start processing: {}", e));
                    } else {
                        self.is_processing = true;
                    }
                }
                self.plugin = Some(plugin_wrapped);
                self.plugin_path = Some(path.to_path_buf());
                
                // Start audio stream for real-time processing
                self.start_audio_processing();
            }
            Err(e) => {
                self.last_error = Some(format!("Failed to load plugin: {}", e));
            }
        }
    }

    fn get_available_plugins(&self) -> Vec<PluginInfo> {
        if let Ok(host) = self.host.lock() {
            host.discover_plugins().unwrap_or_else(|e| {
                eprintln!("Failed to discover plugins: {}", e);
                Vec::new()
            })
        } else {
            Vec::new()
        }
    }

    fn open_plugin_gui(&mut self) {
        if let Some(ref plugin) = self.plugin {
            // Close existing window if any
            if self.plugin_window.is_some() {
                self.plugin_window = None;
            }
            
            // Create new plugin window
            let mut window = PluginWindow::new(plugin.clone());
            match window.open() {
                Ok(()) => {
                    self.plugin_window = Some(window);
                    println!("Plugin GUI opened successfully");
                }
                Err(e) => {
                    self.last_error = Some(format!("Failed to open plugin GUI: {}", e));
                    eprintln!("Failed to open plugin GUI: {}", e);
                }
            }
        } else {
            self.last_error = Some("No plugin loaded".to_string());
        }
    }

    fn close_plugin_gui(&mut self) {
        if let Some(mut window) = self.plugin_window.take() {
            window.close();
            println!("Plugin GUI closed");
        }
    }

    fn update_parameter_values(&mut self) {
        if let Some(ref plugin) = self.plugin {
            if let Ok(plugin_lock) = plugin.lock() {
                // Get parameter changes from plugin GUI
                let changes = plugin_lock.get_parameter_changes();
                for (param_id, value) in changes {
                    self.current_parameter_values.insert(param_id, value);
                    println!("Parameter {} changed to {} via GUI", param_id, value);
                }
            }
        }
    }

    fn start_audio_processing(&mut self) {
        #[cfg(feature = "cpal-backend")]
        {
            if self.audio_stream.is_some() {
                return; // Already started
            }

            let cpal_host = cpal::default_host();
            let device = cpal_host.default_output_device().expect("no output device available");
            let config = device.default_output_config().expect("Failed to get default output config");
            
            let plugin_clone = self.plugin.clone();
            let sample_rate = config.sample_rate().0 as f64;
            let channels = config.channels() as usize;
            
            let stream_config = cpal::StreamConfig {
                channels: config.channels(),
                sample_rate: config.sample_rate(),
                buffer_size: cpal::BufferSize::Default,
            };

            let stream = device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    static mut COUNTER: usize = 0;
                    unsafe { 
                        COUNTER += 1;
                        if COUNTER % 1000 == 0 {
                            eprintln!("Audio callback #{}, buffer size: {}", COUNTER, data.len());
                        }
                    }
                    
                    // Clear output buffer first
                    data.fill(0.0);
                    
                    if let Some(ref plugin) = plugin_clone {
                        if let Ok(mut plugin_lock) = plugin.lock() {
                            // Create audio buffers for processing
                            let frames = data.len() / channels;
                            let mut audio_buffers = AudioBuffers::new(0, channels, frames, sample_rate);
                            
                            // Debug buffer setup
                            unsafe {
                                static mut DEBUG_COUNT: usize = 0;
                                DEBUG_COUNT += 1;
                                if DEBUG_COUNT % 2000 == 0 {
                                    eprintln!("Buffer setup: {} channels, {} frames, sample_rate: {}, inputs: {}, outputs: {}", 
                                        channels, frames, sample_rate, audio_buffers.inputs.len(), audio_buffers.outputs.len());
                                }
                            }
                            
                            // Process audio through the plugin
                            if let Err(e) = plugin_lock.process_audio(&mut audio_buffers) {
                                eprintln!("Audio processing error: {}", e);
                                return;
                            }
                            
                            // Check if we got any non-zero audio BEFORE copying
                            let max_sample = audio_buffers.outputs.iter()
                                .flat_map(|ch| ch.iter())
                                .map(|&sample| sample.abs())
                                .fold(0.0f32, f32::max);
                            
                            if max_sample > 0.001 {
                                eprintln!("Got audio output from plugin! Max sample: {:.6}", max_sample);
                            }
                            
                            // Copy processed audio to output buffer
                            for channel in 0..channels.min(audio_buffers.outputs.len()) {
                                for (frame, sample) in audio_buffers.outputs[channel].iter().enumerate() {
                                    if frame * channels + channel < data.len() {
                                        data[frame * channels + channel] = *sample;
                                    }
                                }
                            }
                        }
                    }
                },
                |err| {
                    eprintln!("Audio stream error: {}", err);
                },
                None,
            ).expect("Failed to create audio stream");

            if let Err(e) = stream.play() {
                eprintln!("Failed to start audio stream: {}", e);
            } else {
                self.audio_stream = Some(stream);
                println!("Audio processing started");
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Update parameter values from plugin GUI
        self.update_parameter_values();
        
        // Release all notes if mouse button is released globally
        if ctx.input(|i| i.pointer.primary_released()) && !self.pressed_notes.is_empty() {
            for &note in self.pressed_notes.clone().iter() {
                self.send_note_off(note);
            }
            self.pressed_notes.clear();
        }
        
        // Request repaint periodically
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("VST3 Host");
            
            ui.separator();
            
            // Error display
            if let Some(ref error) = self.last_error {
                ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
                if ui.button("Clear Error").clicked() {
                    self.last_error = None;
                }
                ui.separator();
            }
            
            // Plugin selection
            ui.horizontal(|ui| {
                ui.label("Plugin:");
                if ui.button("Browse...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("VST3", &["vst3"])
                        .pick_file()
                    {
                        self.load_plugin(&path);
                    }
                }
                
                if ui.button("Discover Plugins").clicked() {
                    // Just update to trigger discovery
                    ctx.request_repaint();
                }
                
                if let Some(path) = &self.plugin_path {
                    ui.label(path.file_name().unwrap_or_default().to_string_lossy());
                }
            });
            
            // Available plugins list
            let available_plugins = self.get_available_plugins();
            if !available_plugins.is_empty() {
                ui.collapsing("Available Plugins", |ui| {
                    for plugin_info in available_plugins.iter().take(10) { // Show first 10
                        if ui.button(&plugin_info.name).clicked() {
                            self.load_plugin(&plugin_info.path);
                        }
                        ui.small(format!("by {}", plugin_info.vendor));
                    }
                });
            }
            
            // Plugin info
            if let Some(ref info) = self.plugin_info {
                ui.horizontal(|ui| {
                    ui.label(format!("Plugin: {}", info.name));
                    ui.separator();
                    ui.label(format!("Vendor: {}", info.vendor));
                    ui.separator();
                    ui.label(format!("I/O: {}/{}", info.audio_inputs, info.audio_outputs));
                });
            }
            
            ui.separator();
            
            // Transport controls
            ui.horizontal(|ui| {
                if self.is_processing {
                    if ui.button("Stop Audio").clicked() {
                        #[cfg(feature = "cpal-backend")]
                        {
                            self.audio_stream = None;
                        }
                        if let Some(ref plugin) = self.plugin {
                            if let Ok(mut plugin_lock) = plugin.lock() {
                                let _ = plugin_lock.stop_processing();
                            }
                        }
                        self.is_processing = false;
                    }
                    ui.colored_label(egui::Color32::GREEN, "Processing");
                } else {
                    if ui.button("Start Audio").clicked() {
                        self.start_audio_processing();
                    }
                    ui.colored_label(egui::Color32::RED, "Stopped");
                }
                
                ui.separator();
                
                // Plugin GUI controls
                if let Some(ref plugin) = self.plugin {
                    let has_gui = plugin.lock().map(|p| p.has_editor()).unwrap_or(false);
                    if has_gui {
                        if self.plugin_window.is_some() {
                            if ui.button("Close GUI").clicked() {
                                self.close_plugin_gui();
                            }
                            ui.colored_label(egui::Color32::GREEN, "GUI Open");
                        } else {
                            if ui.button("Open GUI").clicked() {
                                self.open_plugin_gui();
                            }
                            ui.colored_label(egui::Color32::GRAY, "GUI Closed");
                        }
                    } else {
                        ui.colored_label(egui::Color32::DARK_GRAY, "No GUI");
                    }
                }
            });
            
            ui.separator();
            
            // Main content area
            ui.columns(2, |columns| {
                // Left column - Virtual keyboard
                columns[0].group(|ui| {
                    ui.heading("Virtual Keyboard");
                    
                    ui.horizontal(|ui| {
                        ui.label("MIDI Channel:");
                        ui.add(egui::Slider::new(&mut self.midi_channel, 0..=15));
                    });
                    
                    ui.horizontal(|ui| {
                        ui.label("Octave:");
                        if ui.button("-").clicked() && self.keyboard_octave > 0 {
                            self.keyboard_octave -= 1;
                        }
                        ui.label(format!("C{}", self.keyboard_octave));
                        if ui.button("+").clicked() && self.keyboard_octave < 8 {
                            self.keyboard_octave += 1;
                        }
                    });
                    
                    ui.separator();
                    
                    // Piano keys
                    let base_note = (self.keyboard_octave + 1) * 12; // C
                    let notes = [
                        ("C", 0, false),
                        ("C#", 1, true),
                        ("D", 2, false),
                        ("D#", 3, true),
                        ("E", 4, false),
                        ("F", 5, false),
                        ("F#", 6, true),
                        ("G", 7, false),
                        ("G#", 8, true),
                        ("A", 9, false),
                        ("A#", 10, true),
                        ("B", 11, false),
                    ];
                    
                    // White keys
                    ui.horizontal(|ui| {
                        for (name, offset, is_black) in notes.iter() {
                            if !is_black {
                                let note = base_note + offset;
                                let is_pressed = self.pressed_notes.contains(&note);
                                
                                let button = egui::Button::new(*name)
                                    .min_size(egui::vec2(40.0, 100.0))
                                    .selected(is_pressed);
                                
                                let response = ui.add(button);
                                
                                // Handle mouse down - only send if not already pressed
                                if (response.drag_started() || (response.hovered() && ui.input(|i| i.pointer.primary_pressed()))) && !is_pressed {
                                    self.send_note_on(note, 100);
                                    self.pressed_notes.insert(note);
                                }
                                
                                // Handle mouse up - only send if currently pressed
                                if is_pressed && (response.drag_stopped() || !response.hovered() || ui.input(|i| i.pointer.primary_released())) {
                                    self.send_note_off(note);
                                    self.pressed_notes.remove(&note);
                                }
                            }
                        }
                    });
                    
                    // Black keys (simplified)
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        for (name, offset, is_black) in notes.iter() {
                            if *is_black {
                                let note = base_note + offset;
                                let is_pressed = self.pressed_notes.contains(&note);
                                
                                let button = egui::Button::new(*name)
                                    .min_size(egui::vec2(30.0, 60.0))
                                    .fill(if is_pressed { 
                                        egui::Color32::from_gray(80) 
                                    } else { 
                                        egui::Color32::from_gray(40) 
                                    });
                                
                                let response = ui.add(button);
                                
                                // Handle mouse down - only send if not already pressed
                                if (response.drag_started() || (response.hovered() && ui.input(|i| i.pointer.primary_pressed()))) && !is_pressed {
                                    self.send_note_on(note, 100);
                                    self.pressed_notes.insert(note);
                                }
                                
                                // Handle mouse up - only send if currently pressed
                                if is_pressed && (response.drag_stopped() || !response.hovered() || ui.input(|i| i.pointer.primary_released())) {
                                    self.send_note_off(note);
                                    self.pressed_notes.remove(&note);
                                }
                                ui.add_space(10.0);
                            }
                        }
                    });
                });
                
                // Right column - Meters and parameters
                columns[1].group(|ui| {
                    ui.heading("Audio Meters");
                    
                    // Peak meters
                    for (i, channel) in self.audio_levels.channels.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("Ch{}:", i + 1));
                            let peak = channel.peak;
                            let db = if peak > 0.0001 {
                                20.0 * peak.log10()
                            } else {
                                -80.0
                            };
                            ui.add(
                                egui::ProgressBar::new(peak.min(1.0))
                                    .text(format!("{:.1} dB", db))
                            );
                        });
                    }
                    
                    ui.separator();
                    
                    // Parameters toggle
                    ui.checkbox(&mut self.show_parameters, "Show Parameters");
                    
                    if self.show_parameters {
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .show(ui, |ui| {
                                if let Some(ref plugin) = self.plugin {
                                    if let Ok(plugin_lock) = plugin.lock() {
                                        if let Ok(parameters) = plugin_lock.get_parameters() {
                                            drop(plugin_lock); // Release lock before UI loop
                                            for param in parameters.iter() {
                                                ui.horizontal(|ui| {
                                                    ui.label(&param.name);
                                                    // Use current value from GUI changes if available, otherwise use plugin's value
                                                    let mut value = self.current_parameter_values
                                                        .get(&param.id)
                                                        .copied()
                                                        .unwrap_or(param.value);
                                                    let value_text = format!("{:.3}", value);
                                                    let response = ui.add(
                                                        egui::Slider::new(&mut value, 0.0..=1.0)
                                                            .text(value_text)
                                                    );
                                                    if response.changed() {
                                                        // Update local cache
                                                        self.current_parameter_values.insert(param.id, value);
                                                        // Send to plugin (this is host -> plugin direction)
                                                        if let Ok(mut plugin_lock) = plugin.lock() {
                                                            let _ = plugin_lock.set_parameter(param.id, value);
                                                        }
                                                    }
                                                });
                                            }
                                        }
                                    }
                                }
                            });
                    }
                });
            });
            
            ui.separator();
            
            // Status bar
            ui.horizontal(|ui| {
                ui.label("Status:");
                if let Some(error) = &self.last_error {
                    ui.colored_label(egui::Color32::RED, error);
                    if ui.button("Clear").clicked() {
                        self.last_error = None;
                    }
                } else if self.plugin_path.is_some() {
                    ui.colored_label(egui::Color32::GREEN, "Plugin loaded");
                } else {
                    ui.colored_label(egui::Color32::YELLOW, "No plugin loaded");
                }
            });
        });
    }
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let plugin_path = if args.len() > 1 {
        let path = std::path::PathBuf::from(&args[1]);
        if !path.exists() {
            eprintln!("Error: Plugin path does not exist: {}", path.display());
            std::process::exit(1);
        }
        if !path.to_string_lossy().ends_with(".vst3") {
            eprintln!("Error: Path must be a .vst3 file: {}", path.display());
            std::process::exit(1);
        }
        Some(path)
    } else {
        None
    };

    if let Some(ref path) = plugin_path {
        println!("VST3 Host starting with plugin: {}", path.display());
    } else {
        println!("VST3 Host starting (no plugin specified)");
        println!("Usage: {} <path_to_vst3_plugin>", args[0]);
    }

    let app = App::new(plugin_path);
    
    // Run GUI
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("VST3 Host"),
        ..Default::default()
    };
    
    eframe::run_native(
        "vst3_host",
        options,
        Box::new(|_| Ok(Box::new(app))),
    ).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    
    Ok(())
}