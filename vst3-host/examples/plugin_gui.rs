//! VST3 plugin host with egui interface matching vst3-inspector Processing tab
//!
//! This example demonstrates:
//! - Complete replica of vst3-inspector Processing tab
//! - Audio device initialization and stream management
//! - Real-time VU meters with peak hold
//! - Virtual MIDI keyboard
//! - Plugin GUI window support
//! - Sample rate and block size controls

use eframe::egui;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vst3_host::midi::note_to_name;
use vst3_host::prelude::*;

#[cfg(feature = "cpal-backend")]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use egui::StrokeKind;

struct PluginGuiApp {
    // Core VST3 host
    host: Arc<Mutex<Vst3Host>>,
    plugin: Option<Arc<Mutex<Plugin>>>,
    plugin_info: Option<PluginInfo>,

    // Audio state
    audio_device: Option<cpal::Device>,
    audio_stream: Option<cpal::Stream>,
    is_processing: bool,
    sample_rate: f64,
    block_size: usize,

    // Audio monitoring
    peak_level_left: Arc<Mutex<f32>>,
    peak_level_right: Arc<Mutex<f32>>,
    peak_hold_left: Arc<Mutex<(f32, Instant)>>,
    peak_hold_right: Arc<Mutex<(f32, Instant)>>,

    // MIDI state
    selected_midi_channel: i16,
    active_notes: HashSet<i16>,
    midi_velocity: u8,

    // GUI state
    error_message: Option<String>,
    use_isolation: bool,

    // Plugin window
    plugin_window: Option<PluginWindow>,

    // Shared audio processing state
    shared_audio_state: Option<Arc<Mutex<SharedAudioState>>>,
}

#[derive(Clone)]
struct SharedAudioState {
    sample_rate: f64,
    block_size: usize,
    is_active: bool,
}

impl PluginGuiApp {
    fn new(use_isolation: bool) -> Self {
        let host = Arc::new(Mutex::new(
            Vst3Host::builder()
                .with_process_isolation(use_isolation)
                .sample_rate(48000.0)
                .block_size(512)
                .build()
                .expect("Failed to create host"),
        ));

        Self {
            host,
            plugin: None,
            plugin_info: None,
            audio_device: None,
            audio_stream: None,
            is_processing: false,
            sample_rate: 48000.0,
            block_size: 512,
            peak_level_left: Arc::new(Mutex::new(0.0)),
            peak_level_right: Arc::new(Mutex::new(0.0)),
            peak_hold_left: Arc::new(Mutex::new((0.0, Instant::now()))),
            peak_hold_right: Arc::new(Mutex::new((0.0, Instant::now()))),
            selected_midi_channel: 0, // Channel 1
            active_notes: HashSet::new(),
            midi_velocity: 80,
            error_message: None,
            use_isolation,
            plugin_window: None,
            shared_audio_state: Some(Arc::new(Mutex::new(SharedAudioState {
                sample_rate: 48000.0,
                block_size: 512,
                is_active: true,
            }))),
        }
    }

    fn load_plugin(&mut self, path: std::path::PathBuf) {
        self.error_message = None;

        // Stop existing audio stream
        self.audio_stream = None;
        self.is_processing = false;

        let plugin_result = self.host.lock().unwrap().load_plugin(&path);

        match plugin_result {
            Ok(mut plugin) => {
                // Get initial plugin info
                let mut plugin_info = plugin.info().clone();

                // Start processing
                if let Err(e) = plugin.start_processing() {
                    self.error_message = Some(format!("Failed to start processing: {}", e));
                    return;
                }

                // Update has_gui after plugin is initialized
                plugin_info.has_gui = plugin.has_editor();
                println!(
                    "Plugin loaded: {} - has_editor: {}",
                    plugin_info.name, plugin_info.has_gui
                );
                self.plugin_info = Some(plugin_info);

                self.plugin = Some(Arc::new(Mutex::new(plugin)));
                self.is_processing = true;

                // Initialize audio device if not already done
                if self.audio_device.is_none() {
                    if let Err(e) = self.initialize_audio_device() {
                        self.error_message = Some(format!("Failed to initialize audio: {}", e));
                    }
                }

                // Start audio stream
                if let Err(e) = self.start_audio_stream() {
                    self.error_message = Some(format!("Failed to start audio stream: {}", e));
                }
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to load plugin: {}", e));
            }
        }
    }

    fn initialize_audio_device(&mut self) -> std::result::Result<(), String> {
        #[cfg(feature = "cpal-backend")]
        {
            let host = cpal::default_host();
            self.audio_device = host.default_output_device();

            if self.audio_device.is_none() {
                return Err("No audio output device found".to_string());
            }
        }
        Ok(())
    }

    fn start_audio_stream(&mut self) -> std::result::Result<(), String> {
        #[cfg(feature = "cpal-backend")]
        {
            if let Some(device) = &self.audio_device {
                let config = cpal::StreamConfig {
                    channels: 2,
                    sample_rate: cpal::SampleRate(self.sample_rate as u32),
                    buffer_size: cpal::BufferSize::Fixed(self.block_size as u32),
                };

                // Clone all the Arc references we need
                let plugin = self.plugin.clone();
                let peak_left = self.peak_level_left.clone();
                let peak_right = self.peak_level_right.clone();
                let peak_hold_left = self.peak_hold_left.clone();
                let peak_hold_right = self.peak_hold_right.clone();
                let block_size = self.block_size;

                let stream = device
                    .build_output_stream(
                        &config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            // Clear output buffer
                            for sample in data.iter_mut() {
                                *sample = 0.0;
                            }

                            // Process audio if plugin is available
                            if let Some(plugin) = &plugin {
                                if let Ok(mut plugin) = plugin.lock() {
                                    // Create audio buffers
                                    let sample_rate = 48000.0; // This should come from the app state
                                    let mut buffers = AudioBuffers {
                                        inputs: vec![], // No inputs for instruments
                                        outputs: vec![
                                            vec![0.0; block_size], // Left
                                            vec![0.0; block_size], // Right
                                        ],
                                        sample_rate,
                                        block_size,
                                    };

                                    // Process audio
                                    if plugin.process_audio(&mut buffers).is_ok() {
                                        // Copy processed audio to output
                                        let output_channels = buffers.outputs.len();
                                        if output_channels > 0 {
                                            for (i, sample) in data.iter_mut().enumerate() {
                                                let channel = i % output_channels;
                                                let frame = i / output_channels;
                                                if frame < block_size {
                                                    *sample = buffers.outputs[channel][frame];
                                                }
                                            }

                                            // Update peak levels
                                            if output_channels >= 2 {
                                                let left_peak = buffers.outputs[0]
                                                    .iter()
                                                    .map(|&x| x.abs())
                                                    .fold(0.0f32, |a, b| a.max(b));
                                                let right_peak = buffers.outputs[1]
                                                    .iter()
                                                    .map(|&x| x.abs())
                                                    .fold(0.0f32, |a, b| a.max(b));

                                                *peak_left.lock().unwrap() = left_peak;
                                                *peak_right.lock().unwrap() = right_peak;

                                                // Update peak hold
                                                let now = Instant::now();
                                                if let Ok(mut hold) = peak_hold_left.lock() {
                                                    if left_peak > hold.0
                                                        || now.duration_since(hold.1)
                                                            > Duration::from_secs(2)
                                                    {
                                                        *hold = (left_peak, now);
                                                    }
                                                }
                                                if let Ok(mut hold) = peak_hold_right.lock() {
                                                    if right_peak > hold.0
                                                        || now.duration_since(hold.1)
                                                            > Duration::from_secs(2)
                                                    {
                                                        *hold = (right_peak, now);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        move |err| {
                            eprintln!("Audio stream error: {}", err);
                        },
                        None,
                    )
                    .map_err(|e| format!("Failed to build output stream: {}", e))?;

                stream
                    .play()
                    .map_err(|e| format!("Failed to play stream: {}", e))?;
                self.audio_stream = Some(stream);
            }
        }
        Ok(())
    }

    fn send_midi_note(&mut self, note: i16) {
        if let Some(plugin) = &self.plugin {
            if let Ok(mut plugin) = plugin.lock() {
                let channel = MidiChannel::from_index(self.selected_midi_channel as u8)
                    .unwrap_or(MidiChannel::Ch1);

                if self.active_notes.contains(&note) {
                    // Note off
                    let _ = plugin.send_midi_note_off(note as u8, channel);
                    self.active_notes.remove(&note);
                } else {
                    // Note on
                    let _ = plugin.send_midi_note(note as u8, self.midi_velocity, channel);
                    self.active_notes.insert(note);
                }
            }
        }
    }

    fn send_midi_panic(&mut self) {
        if let Some(plugin) = &self.plugin {
            if let Ok(mut plugin) = plugin.lock() {
                let _ = plugin.midi_panic();
                self.active_notes.clear();
            }
        }
    }

    fn audio_panic(&mut self) {
        // Stop audio stream
        self.audio_stream = None;

        // Clear peak levels
        *self.peak_level_left.lock().unwrap() = 0.0;
        *self.peak_level_right.lock().unwrap() = 0.0;
        *self.peak_hold_left.lock().unwrap() = (0.0, Instant::now());
        *self.peak_hold_right.lock().unwrap() = (0.0, Instant::now());
    }

    fn show_plugin_gui(&mut self) {
        if let Some(plugin) = &self.plugin {
            // Create a new plugin window
            let mut window = PluginWindow::new(plugin.clone());
            match window.open() {
                Ok(()) => {
                    self.plugin_window = Some(window);
                }
                Err(e) => {
                    self.error_message = Some(format!("Failed to open plugin GUI: {}", e));
                }
            }
        }
    }

    fn close_plugin_gui(&mut self) {
        // The PluginWindow will handle cleanup automatically on drop
        self.plugin_window = None;
    }

    fn draw_piano_keyboard(&mut self, ui: &mut egui::Ui) {
        let white_key_width = 24.0;
        let white_key_height = 120.0;
        let black_key_width = 16.0;
        let black_key_height = 80.0;

        // Define notes for 6 octaves (C1 to C6)
        let octave_start = 1;
        let octave_count = 5;

        // Calculate total width needed
        let keys_per_octave = 7;
        let total_white_keys = keys_per_octave * octave_count + 1; // +1 for final C
        let total_width = total_white_keys as f32 * white_key_width;

        // Allocate space for the keyboard
        let (response, painter) = ui.allocate_painter(
            egui::vec2(total_width, white_key_height),
            egui::Sense::click_and_drag(),
        );

        let rect = response.rect;
        let mouse_pos = response.interact_pointer_pos();

        // Track which key is being interacted with
        let mut key_under_mouse: Option<i16> = None;

        // Helper to calculate note number
        let note_for_white_key = |octave: i32, key_in_octave: i32| -> i16 {
            let white_key_offsets = [0, 2, 4, 5, 7, 9, 11]; // C, D, E, F, G, A, B
            let base_note = (octave + 1) * 12; // C0 = 12, C1 = 24, etc.
            (base_note + white_key_offsets[key_in_octave as usize]) as i16
        };

        // Draw white keys first
        for octave in 0..=octave_count {
            let keys_in_octave = if octave == octave_count {
                1
            } else {
                keys_per_octave
            };

            for key in 0..keys_in_octave {
                let x = rect.left() + (octave * keys_per_octave + key) as f32 * white_key_width;
                let key_rect = egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(white_key_width - 1.0, white_key_height),
                );

                let note = note_for_white_key(octave + octave_start, key);
                let is_active = self.active_notes.contains(&note);

                // Check if mouse is over this key
                if let Some(pos) = mouse_pos {
                    if key_rect.contains(pos) {
                        key_under_mouse = Some(note);
                    }
                }

                // Draw white key
                let fill_color = if is_active {
                    egui::Color32::from_rgb(200, 200, 200)
                } else {
                    egui::Color32::WHITE
                };

                painter.rect(
                    key_rect,
                    0.0,
                    fill_color,
                    egui::Stroke::new(1.0, egui::Color32::BLACK),
                    StrokeKind::Inside,
                );

                // Draw note label (C3, D3, etc.) with MIDI note number
                if key == 0 {
                    // Only label C notes
                    let note_name = note_to_name(note as u8);
                    let label_pos = key_rect.center_bottom() - egui::vec2(0.0, 20.0);
                    painter.text(
                        label_pos,
                        egui::Align2::CENTER_CENTER,
                        format!("{}\n{}", note_name, note),
                        egui::FontId::proportional(10.0),
                        egui::Color32::BLACK,
                    );
                }
            }
        }

        // Draw black keys
        let black_key_patterns = [1, 3, 6, 8, 10]; // C#, D#, F#, G#, A#
        for octave in 0..octave_count {
            for &pattern in &black_key_patterns {
                let white_key_index = match pattern {
                    1 => 0,  // C#
                    3 => 1,  // D#
                    6 => 3,  // F#
                    8 => 4,  // G#
                    10 => 5, // A#
                    _ => continue,
                };

                let x = rect.left()
                    + (octave * keys_per_octave + white_key_index) as f32 * white_key_width
                    + white_key_width
                    - black_key_width / 2.0;
                let key_rect = egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(black_key_width, black_key_height),
                );

                let note = ((octave + octave_start + 1) * 12 + pattern) as i16;
                let is_active = self.active_notes.contains(&note);

                // Check if mouse is over this key (black keys take priority)
                if let Some(pos) = mouse_pos {
                    if key_rect.contains(pos) {
                        key_under_mouse = Some(note);
                    }
                }

                // Draw black key
                let fill_color = if is_active {
                    egui::Color32::from_rgb(64, 64, 64)
                } else {
                    egui::Color32::BLACK
                };

                painter.rect(
                    key_rect,
                    0.0,
                    fill_color,
                    egui::Stroke::new(1.0, egui::Color32::DARK_GRAY),
                    StrokeKind::Inside,
                );
            }
        }

        // Handle mouse interaction
        if response.clicked() || response.dragged() {
            if let Some(note) = key_under_mouse {
                if response.clicked() {
                    self.send_midi_note(note);
                }
            }
        }
    }
}

impl eframe::App for PluginGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("VST3 Plugin Processing");

            // Plugin selection
            ui.horizontal(|ui| {
                if ui.button("Select Plugin...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("VST3 Plugin", &["vst3"])
                        .set_title("Select VST3 Plugin")
                        .pick_file()
                    {
                        self.load_plugin(path);
                    }
                }

                if let Some(info) = &self.plugin_info {
                    ui.label(format!("Loaded: {} by {}", info.name, info.vendor));

                    if info.has_gui {
                        ui.horizontal(|ui| {
                            if self.plugin_window.is_none() {
                                if ui.button("Show Plugin GUI").clicked() {
                                    self.show_plugin_gui();
                                }
                            } else {
                                if ui.button("Close Plugin GUI").clicked() {
                                    self.close_plugin_gui();
                                }
                                ui.label("Plugin GUI is open");
                            }
                        });
                    } else {
                        ui.label("Plugin has no GUI");
                    }
                } else {
                    ui.label("No plugin loaded");
                }
            });

            ui.separator();

            // Error display
            if let Some(error) = &self.error_message {
                ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
                ui.separator();
            }

            // Audio Device status
            ui.horizontal(|ui| {
                ui.label("Audio Output:");
                if self.audio_device.is_some() {
                    ui.colored_label(egui::Color32::GREEN, "Initialized");

                    if self.audio_stream.is_some() {
                        ui.colored_label(egui::Color32::GREEN, "Stream Active");
                        if ui.button("Stop Audio").clicked() {
                            self.audio_stream = None;
                        }
                    } else {
                        ui.colored_label(egui::Color32::YELLOW, "Stream Inactive");
                        if ui.button("Start Audio").clicked() {
                            if let Err(e) = self.start_audio_stream() {
                                self.error_message = Some(format!("Failed to start audio: {}", e));
                            }
                        }
                    }
                } else {
                    ui.colored_label(egui::Color32::RED, "Not initialized");
                    if ui.button("Initialize Audio").clicked() {
                        if let Err(e) = self.initialize_audio_device() {
                            self.error_message = Some(format!("Failed to initialize audio: {}", e));
                        }
                    }
                }
            });

            // Audio settings
            ui.horizontal(|ui| {
                ui.label("Sample Rate:");

                let sample_rates = [44100.0, 48000.0, 88200.0, 96000.0];
                let current_rate_text = format!("{} Hz", self.sample_rate as u32);

                egui::ComboBox::from_id_salt("sample_rate_selector")
                    .selected_text(&current_rate_text)
                    .show_ui(ui, |ui| {
                        for &rate in &sample_rates {
                            if ui
                                .selectable_value(
                                    &mut self.sample_rate,
                                    rate,
                                    format!("{} Hz", rate as u32),
                                )
                                .clicked()
                            {
                                // Restart audio stream with new rate
                                self.audio_stream = None;
                                if let Err(e) = self.start_audio_stream() {
                                    self.error_message =
                                        Some(format!("Failed to restart audio: {}", e));
                                }
                            }
                        }
                    });

                ui.separator();
                ui.label("Block Size:");

                let block_sizes = [64, 128, 256, 512, 1024, 2048];
                let current_block_text = format!("{} samples", self.block_size);

                egui::ComboBox::from_id_salt("block_size_selector")
                    .selected_text(&current_block_text)
                    .show_ui(ui, |ui| {
                        for &size in &block_sizes {
                            if ui
                                .selectable_value(
                                    &mut self.block_size,
                                    size,
                                    format!("{} samples", size),
                                )
                                .clicked()
                            {
                                // Restart audio stream with new block size
                                self.audio_stream = None;
                                if let Err(e) = self.start_audio_stream() {
                                    self.error_message =
                                        Some(format!("Failed to restart audio: {}", e));
                                }
                            }
                        }
                    });
            });

            ui.separator();
            ui.add_space(8.0);

            // VU Meter and Panic Controls
            ui.heading("Audio Monitoring & Safety");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                // VU Meter
                ui.group(|ui| {
                    ui.label("Output Levels (VU Meter):");

                    let peak_left = *self.peak_level_left.lock().unwrap();
                    let peak_right = *self.peak_level_right.lock().unwrap();

                    let (peak_hold_left, _) = *self.peak_hold_left.lock().unwrap();
                    let (peak_hold_right, _) = *self.peak_hold_right.lock().unwrap();

                    // Convert to dB
                    const MIN_DB: f32 = -60.0;
                    const SILENCE_THRESHOLD: f32 = 0.00001; // -100 dB

                    let db_left = if peak_left > SILENCE_THRESHOLD {
                        (20.0 * peak_left.log10()).max(MIN_DB)
                    } else {
                        f32::NEG_INFINITY
                    };
                    let db_right = if peak_right > SILENCE_THRESHOLD {
                        (20.0 * peak_right.log10()).max(MIN_DB)
                    } else {
                        f32::NEG_INFINITY
                    };

                    let db_hold_left = if peak_hold_left > SILENCE_THRESHOLD {
                        (20.0 * peak_hold_left.log10()).max(MIN_DB)
                    } else {
                        f32::NEG_INFINITY
                    };
                    let db_hold_right = if peak_hold_right > SILENCE_THRESHOLD {
                        (20.0 * peak_hold_right.log10()).max(MIN_DB)
                    } else {
                        f32::NEG_INFINITY
                    };

                    ui.vertical(|ui| {
                        // Left channel
                        ui.horizontal(|ui| {
                            ui.label("L:");
                            let color = if db_left > -3.0 {
                                egui::Color32::RED
                            } else if db_left > -12.0 {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::GREEN
                            };

                            let bar_value = if db_left.is_finite() {
                                ((db_left - MIN_DB) / -MIN_DB).max(0.0).min(1.0)
                            } else {
                                0.0
                            };

                            let hold_value = if db_hold_left.is_finite() {
                                ((db_hold_left - MIN_DB) / -MIN_DB).max(0.0).min(1.0)
                            } else {
                                0.0
                            };

                            let bar_rect = ui
                                .add(
                                    egui::ProgressBar::new(bar_value)
                                        .desired_width(200.0)
                                        .fill(color),
                                )
                                .rect;

                            // Draw peak hold indicator
                            if hold_value > 0.0 {
                                let hold_x = bar_rect.left() + hold_value * bar_rect.width();
                                ui.painter().vline(
                                    hold_x,
                                    bar_rect.y_range(),
                                    egui::Stroke::new(2.0, egui::Color32::WHITE),
                                );
                            }

                            let db_text = if db_left.is_finite() {
                                format!("{:.1} dB", db_left)
                            } else {
                                "-∞ dB".to_string()
                            };
                            ui.colored_label(color, db_text);
                        });

                        // Right channel
                        ui.horizontal(|ui| {
                            ui.label("R:");
                            let color = if db_right > -3.0 {
                                egui::Color32::RED
                            } else if db_right > -12.0 {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::GREEN
                            };

                            let bar_value = if db_right.is_finite() {
                                ((db_right - MIN_DB) / -MIN_DB).max(0.0).min(1.0)
                            } else {
                                0.0
                            };

                            let hold_value = if db_hold_right.is_finite() {
                                ((db_hold_right - MIN_DB) / -MIN_DB).max(0.0).min(1.0)
                            } else {
                                0.0
                            };

                            let bar_rect = ui
                                .add(
                                    egui::ProgressBar::new(bar_value)
                                        .desired_width(200.0)
                                        .fill(color),
                                )
                                .rect;

                            // Draw peak hold indicator
                            if hold_value > 0.0 {
                                let hold_x = bar_rect.left() + hold_value * bar_rect.width();
                                ui.painter().vline(
                                    hold_x,
                                    bar_rect.y_range(),
                                    egui::Stroke::new(2.0, egui::Color32::WHITE),
                                );
                            }

                            let db_text = if db_right.is_finite() {
                                format!("{:.1} dB", db_right)
                            } else {
                                "-∞ dB".to_string()
                            };
                            ui.colored_label(color, db_text);
                        });
                    });
                });

                ui.add_space(20.0);

                // Panic buttons
                ui.vertical(|ui| {
                    ui.label("Emergency Controls:");

                    if ui.button("🚨 MIDI Panic").clicked() {
                        self.send_midi_panic();
                    }

                    if ui.button("🔇 Audio Panic").clicked() {
                        self.audio_panic();
                    }
                });
            });

            ui.separator();
            ui.add_space(8.0);

            // MIDI Testing
            ui.heading("MIDI Testing");
            ui.add_space(8.0);

            // Virtual keyboard
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label("Virtual MIDI Keyboard:");

                    // MIDI channel selector
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let channel_names: Vec<String> =
                            (1..=16).map(|ch| format!("Channel {}", ch)).collect();
                        let selected_text = &channel_names[self.selected_midi_channel as usize];

                        egui::ComboBox::from_label("MIDI Channel")
                            .selected_text(selected_text)
                            .show_ui(ui, |ui| {
                                for (idx, channel_name) in channel_names.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut self.selected_midi_channel,
                                        idx as i16,
                                        channel_name,
                                    );
                                }
                            });
                    });
                });

                ui.add_space(4.0);
                self.draw_piano_keyboard(ui);

                // Velocity control
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Velocity:");
                    ui.add(egui::Slider::new(&mut self.midi_velocity, 1..=127).show_value(true));
                });
            });
        });

        // Request repaint for VU meter updates
        ctx.request_repaint();
    }
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let use_isolation = args.iter().any(|arg| arg == "--isolated");

    // Initialize logging
    env_logger::init();

    // Create and run the egui app
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 700.0])
            .with_title("VST3 Plugin Host - Processing"),
        ..Default::default()
    };

    eframe::run_native(
        "VST3 Plugin Host",
        options,
        Box::new(|_cc| Ok(Box::new(PluginGuiApp::new(use_isolation)))),
    )?;

    Ok(())
}
