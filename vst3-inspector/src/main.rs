#![allow(deprecated)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use vst3_host::midi::MidiChannel;
use vst3_host::{AudioHandle, Vst3Host};

// Import modules
mod data_structures;

/// Scan for installed VST3 plugin paths via the `vst3-host` library (lightweight —
/// lists `.vst3` bundles without loading them). Replaces the former hand-rolled
/// `plugin_discovery` module so the inspector consumes the library for discovery.
fn discover_vst3_paths(custom_paths: &[String]) -> Vec<String> {
    let mut builder = vst3_host::Vst3Host::builder().scan_default_paths();
    for p in custom_paths {
        builder = builder.add_scan_path(p);
    }
    let host = match builder.build() {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    let mut paths: Vec<String> = host
        .scan_plugin_paths()
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

use data_structures::MidiDirection;

// MIDI note conversion helpers — delegate to the vst3-host library (C3 = MIDI 60).
fn midi_note_to_name(note: u8) -> String {
    vst3_host::midi::note_to_name(note)
}

fn note_name_to_midi(name: &str) -> Option<u8> {
    vst3_host::midi::name_to_note(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_midi_conversions() {
        // Test some known values using C3=60 convention
        assert_eq!(note_name_to_midi("C3"), Some(60)); // User's desired C3
        assert_eq!(note_name_to_midi("C2"), Some(48));
        assert_eq!(note_name_to_midi("A3"), Some(69)); // Concert A
        assert_eq!(note_name_to_midi("C-2"), Some(0));
        assert_eq!(note_name_to_midi("G8"), Some(127));

        // Test reverse conversion
        assert_eq!(midi_note_to_name(60), "C3");
        assert_eq!(midi_note_to_name(48), "C2");
        assert_eq!(midi_note_to_name(69), "A3");
        assert_eq!(midi_note_to_name(0), "C-2");
        assert_eq!(midi_note_to_name(127), "G8");

        // Test accidentals
        assert_eq!(note_name_to_midi("C#3"), Some(61));
        assert_eq!(note_name_to_midi("Db3"), Some(61));
        assert_eq!(note_name_to_midi("F#3"), Some(66));

        // Print for debugging
        println!("C3 = MIDI {}", note_name_to_midi("C3").unwrap());
        println!("C4 = MIDI {}", note_name_to_midi("C4").unwrap());
        println!("C5 = MIDI {}", note_name_to_midi("C5").unwrap());
    }
}

// Default plugin paths — the user can load any discovered plugin. The library handles
// VST3 bundle binary path resolution internally, so the inspector only deals with the
// `.vst3` bundle path.
#[cfg(target_os = "macos")]
const PLUGIN_PATH: &str = "/Library/Audio/Plug-Ins/VST3/HY-MPS3 free.vst3";

#[cfg(target_os = "windows")]
const PLUGIN_PATH: &str = r"C:\Program Files\Common Files\VST3\HY-MPS3 free.vst3";

#[derive(Debug, Clone)]
struct PluginInfo {
    // Accurate plugin-level metadata from the library (name, vendor, version, category,
    // MIDI/audio capability, uid) — surfaced as the identity summary.
    summary: vst3_host::PluginInfo,
    factory_info: FactoryInfo,
    classes: Vec<ClassInfo>,
    component_info: Option<ComponentInfo>,
    controller_info: Option<ControllerInfo>,
    has_gui: bool,
    gui_size: Option<(i32, i32)>,
}

#[derive(Debug, Clone)]
struct FactoryInfo {
    vendor: String,
    url: String,
    email: String,
    flags: i32,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // UI model: not every field is shown yet
struct ClassInfo {
    name: String,
    category: String,
    class_id: String,
    cardinality: i32,
    version: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ComponentInfo {
    bus_count_inputs: i32,
    bus_count_outputs: i32,
    audio_inputs: Vec<BusInfo>,
    audio_outputs: Vec<BusInfo>,
    event_inputs: Vec<BusInfo>,
    event_outputs: Vec<BusInfo>,
    supports_processing: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct BusInfo {
    name: String,
    bus_type: i32,
    flags: i32,
    channel_count: i32,
}

#[derive(Debug, Clone)]
struct ControllerInfo {
    parameter_count: i32,
    parameters: Vec<ParameterInfo>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ParameterInfo {
    id: u32,
    title: String,
    short_title: String,
    units: String,
    step_count: i32,
    default_normalized_value: f64,
    unit_id: i32,
    flags: i32,
    current_value: f64,
}

/// Headless self-test: drive the `vst3-host` library end to end (discover → introspect
/// → load → parameters → play) and report. Lets the inspector's library integration be
/// verified without launching the GUI. Returns a process exit code.
fn run_selftest(path: &str) -> i32 {
    use vst3_host::{midi::MidiChannel, Vst3Host};

    println!("=== vst3-inspector self-test: {path} ===");

    // 1. Discovery via the library (Slice 1).
    match Vst3Host::builder().scan_default_paths().build() {
        Ok(h) => println!(
            "discovery: {} plugin paths found",
            h.scan_plugin_paths().len()
        ),
        Err(e) => {
            eprintln!("FAIL: build host: {e}");
            return 1;
        }
    }

    // 2. Deep introspection (Slice 0).
    let detail = match vst3_host::get_detailed_plugin_info(std::path::Path::new(path)) {
        Ok(d) => {
            println!(
                "introspect: {} by {} — {} classes, {} audio-out bus(es)",
                d.info.name,
                d.factory.vendor,
                d.classes.len(),
                d.buses.audio_outputs.len()
            );
            d
        }
        Err(e) => {
            eprintln!("FAIL: introspect {path}: {e}");
            return 1;
        }
    };

    // 3. Load + parameters + play + observe audio.
    let mut host = match Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            eprintln!("FAIL: build host: {e}");
            return 1;
        }
    };
    let plugin = match host.load_plugin(path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: load {path}: {e}");
            return 1;
        }
    };
    let param_count = plugin.get_parameters().map(|p| p.len()).unwrap_or(0);
    println!("load: {} — {param_count} parameters", plugin.info().name);

    // 3b. JSON export — the "Copy JSON" capability (full report → valid JSON). Reuses the
    // introspection from step 2 plus the loaded plugin's parameters.
    match plugin.get_parameters() {
        Ok(params) => {
            let report = vst3_host::PluginReport::new(detail, params);
            match report.to_json() {
                Ok(json) => {
                    if serde_json::from_str::<serde_json::Value>(&json).is_err() {
                        eprintln!("FAIL: PluginReport produced invalid JSON");
                        return 1;
                    }
                    println!(
                        "export: PluginReport JSON {} bytes, {} params, version={:?} category={:?} midi_in={} midi_out={}",
                        json.len(),
                        report.parameters.len(),
                        report.detailed.info.version,
                        report.detailed.info.category,
                        report.detailed.info.has_midi_input,
                        report.detailed.info.has_midi_output,
                    );
                }
                Err(e) => {
                    eprintln!("FAIL: to_json: {e}");
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("FAIL: get parameters for report: {e}");
            return 1;
        }
    }

    let audio = match host.play(plugin) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("FAIL: play: {e}");
            return 1;
        }
    };
    let _ = audio.lock().send_midi_note(60, 110, MidiChannel::Ch1);
    let mut peak = 0.0f32;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        for c in &audio.lock().get_output_levels().channels {
            peak = peak.max(c.peak);
        }
    }
    let _ = audio.lock().send_midi_note_off(60, MidiChannel::Ch1);
    println!("play: max output peak {peak:.4}");
    println!("SELFTEST OK");
    0
}

/// Apply a Catppuccin Frappé-flavoured dark theme to egui (replaces the `catppuccin-egui`
/// crate, which lags egui's releases).
fn apply_frappe_theme(ctx: &egui::Context) {
    use egui::Color32;
    let base = Color32::from_rgb(0x30, 0x34, 0x46);
    let mantle = Color32::from_rgb(0x29, 0x2c, 0x3c);
    let surface0 = Color32::from_rgb(0x41, 0x45, 0x59);
    let surface1 = Color32::from_rgb(0x51, 0x57, 0x6d);
    let text = Color32::from_rgb(0xc6, 0xd0, 0xf5);
    let blue = Color32::from_rgb(0x8c, 0xaa, 0xee);

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(text);
    v.panel_fill = base;
    v.window_fill = mantle;
    v.extreme_bg_color = mantle;
    v.faint_bg_color = surface0;
    v.hyperlink_color = blue;
    v.widgets.noninteractive.bg_fill = base;
    v.widgets.inactive.bg_fill = surface0;
    v.widgets.hovered.bg_fill = surface1;
    v.widgets.active.bg_fill = surface1;
    v.selection.bg_fill = blue.gamma_multiply(0.4);
    v.selection.stroke = egui::Stroke::new(1.0, blue);
    ctx.set_visuals(v);
}

fn main() {
    // Headless self-test mode: `vst3-inspector --selftest [plugin.vst3]`.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--selftest") {
        let path = args
            .iter()
            .skip_while(|a| a.as_str() != "--selftest")
            .nth(1)
            .cloned()
            .unwrap_or_else(|| "test_plugins/Dexed.vst3".to_string());
        std::process::exit(run_selftest(&path));
    }

    println!("Starting VST3 Host...");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("VST3 Plugin Inspector"),
        ..Default::default()
    };

    let _ = eframe::run_native(
        "VST3 Plugin Inspector",
        options,
        Box::new(|cc| {
            apply_frappe_theme(&cc.egui_ctx);

            let mut inspector = VST3Inspector::from_path(PLUGIN_PATH);

            // Scan for available plugins
            let prefs = Preferences::load();
            inspector.discovered_plugins = discover_vst3_paths(&prefs.custom_plugin_paths);

            // Try to load the default plugin through the library.
            let default_path = inspector.plugin_path.clone();
            if std::path::Path::new(&default_path).exists() {
                inspector.load_plugin(default_path);
            } else {
                println!("ℹDefault plugin not found, none loaded at startup");
            }

            Ok(Box::new(inspector))
        }),
    );
}

#[derive(Debug, Clone)]
struct MidiEvent {
    timestamp: Instant,
    direction: MidiDirection,
    event_type: MidiEventType,
    channel: u8,
    data1: u8,
    data2: u8,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // full MIDI taxonomy; not every variant is produced yet
enum MidiEventType {
    NoteOn {
        pitch: i16,
        velocity: f32,
        channel: i16,
    },
    NoteOff {
        pitch: i16,
        velocity: f32,
        channel: i16,
    },
    ControlChange {
        controller: u8,
        value: u8,
        channel: i16,
    },
    ProgramChange {
        program: u8,
        channel: i16,
    },
    PitchBend {
        value: i16,
        channel: i16,
    },
    Aftertouch,
    ChannelPressure,
    SystemExclusive,
    Clock,
    Start,
    Continue,
    Stop,
    ActiveSensing,
    Reset,
    Other {
        status: u8,
        data1: u8,
        data2: u8,
    },
}

#[derive(Debug, Clone)]
struct MidiEventFilter {
    show_note_events: bool,
    show_cc_events: bool,
    show_program_change: bool,
    show_pitch_bend: bool,
    show_aftertouch: bool,
    show_system_events: bool,
    show_clock_events: bool,
    show_active_sensing: bool,
}

impl Default for MidiEventFilter {
    fn default() -> Self {
        Self {
            show_note_events: true,
            show_cc_events: true,
            show_program_change: true,
            show_pitch_bend: true,
            show_aftertouch: true,
            show_system_events: true,
            show_clock_events: true,
            show_active_sensing: false, // Off by default as it's spammy
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Preferences {
    custom_plugin_paths: Vec<String>,
    last_loaded_plugin: Option<String>,
    auto_start_processing: bool,
    window_size: Option<(f32, f32)>,
}

impl Preferences {
    fn load() -> Self {
        if let Some(config_dir) = directories::ProjectDirs::from("com", "vst-host", "vst-host") {
            let config_path = config_dir.config_dir().join("preferences.json");
            if let Ok(data) = std::fs::read_to_string(config_path) {
                if let Ok(prefs) = serde_json::from_str(&data) {
                    return prefs;
                }
            }
        }
        Self::default()
    }

    fn save(&self) -> Result<(), std::io::Error> {
        if let Some(config_dir) = directories::ProjectDirs::from("com", "vst-host", "vst-host") {
            let config_dir = config_dir.config_dir();
            std::fs::create_dir_all(config_dir)?;
            let config_path = config_dir.join("preferences.json");
            let data = serde_json::to_string_pretty(self)?;
            std::fs::write(config_path, data)?;
        }
        Ok(())
    }
}

struct VST3Inspector {
    plugin_path: String,
    plugin_info: Option<PluginInfo>,
    // Prebuilt JSON export of the current plugin (PluginReport), for the "Copy JSON" button.
    // Built at load time so the button never re-introspects a loaded plugin.
    report_json: Option<String>,
    // The plugin's native editor window while open (standalone; dropped to close).
    plugin_window: Option<vst3_host::PluginWindow>,
    // Plugin discovery
    discovered_plugins: Vec<String>,
    // The `vst3-host` library host (built once, used to load plugins).
    host: Vst3Host,
    // The currently loaded + playing plugin. `Some` when a plugin is loaded; the
    // `Plugin` lives entirely inside this `AudioHandle` for its whole lifetime.
    audio: Option<AudioHandle>,
    // Last user-facing error (e.g. for not-yet-ported features).
    last_error: Option<String>,
    // GUI management
    gui_attached: bool,
    // Parameter editing
    selected_parameter: Option<usize>,
    // Parameter table UI
    parameter_search: String,
    parameter_filter: ParameterFilter,
    show_only_modified: bool,
    table_scroll_to_selected: bool,
    // Pagination
    current_page: usize,
    items_per_page: usize,
    // Tab management
    current_tab: Tab,
    // Inline editing state
    parameter_being_edited: Option<u32>,
    // Whether the loaded plugin is currently processing (cached from the library).
    is_processing: bool,
    // Host configuration
    block_size: i32,
    sample_rate: f64,
    // Virtual keyboard state
    pressed_keys: HashSet<i16>,
    selected_midi_channel: i16, // 0-15 for MIDI channels 1-16
    // MIDI monitoring
    midi_events: Arc<Mutex<Vec<MidiEvent>>>,
    midi_event_filter: MidiEventFilter,
    midi_monitor_paused: Arc<Mutex<bool>>,
    max_midi_events: usize,
    // Preferences
    preferences: Preferences,
    // VU meter
    peak_level_left: Arc<Mutex<f32>>,
    peak_level_right: Arc<Mutex<f32>>,
    // Peak hold
    peak_hold_left: Arc<Mutex<(f32, Instant)>>, // (level, time)
    peak_hold_right: Arc<Mutex<(f32, Instant)>>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tab {
    Plugins,
    Plugin,
    Processing,
    MidiMonitor,
}

#[derive(Debug, Clone, PartialEq)]
enum ParameterFilter {
    All,
    Writable,
    ReadOnly,
    HasSteps,
    HasUnits,
}

impl eframe::App for VST3Inspector {
    // eframe 0.34 made `ui` the required trait method; this app builds its own panels on the
    // `Context` from `update` (still called by eframe each frame), so `ui` is unused.
    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Pull current output levels, plugin-emitted MIDI, and plugin-side parameter edits.
        self.update_vu_meters();
        self.poll_plugin_output_midi();
        self.poll_plugin_parameter_changes();

        // Keep the UI live without busy-looping. egui is reactive by default (repaints only
        // on input), which makes a host UI feel dead between events; an *unbounded*
        // `request_repaint()` instead pegs the render loop at the monitor refresh rate and
        // hammers the plugin mutex (contending with the audio thread → input lag). Capping at
        // ~60 fps with `request_repaint_after` keeps meters/clicks responsive at far lower
        // cost. (Input events still trigger an immediate repaint.)
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
        // Top header panel
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);

            // Plugin info - always shown at top
            ui.horizontal(|ui| {
                // Plugin info - left side
                ui.vertical(|ui| {
                    ui.heading(
                        self.plugin_info
                            .as_ref()
                            .and_then(|p| p.classes.first())
                            .map_or("VST3 Plugin Inspector", |c| &c.name)
                            .to_string(),
                    );
                    ui.label(format!(
                        "by {}",
                        self.plugin_info
                            .as_ref()
                            .map_or("Unknown", |p| &p.factory_info.vendor)
                    ));

                    // Show last error, if any.
                    if let Some(err) = &self.last_error {
                        ui.colored_label(egui::Color32::ORANGE, err.clone());
                    }
                });

                // Push GUI button to the right - only show on Plugin tab
                if self.current_tab != Tab::Plugins {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Large GUI button
                        if self.plugin_info.as_ref().is_some_and(|p| p.has_gui) {
                            if self.gui_attached {
                                if ui
                                    .add_sized([120.0, 40.0], egui::Button::new("Close GUI"))
                                    .clicked()
                                {
                                    self.close_plugin_gui();
                                }
                            } else if ui
                                .add_sized([120.0, 40.0], egui::Button::new("Open GUI"))
                                .clicked()
                            {
                                if let Err(e) = self.create_plugin_gui() {
                                    println!("Failed to create plugin GUI: {}", e);
                                }
                            }
                        } else {
                            // Show disabled button when no GUI is available
                            ui.add_enabled_ui(false, |ui| {
                                ui.add_sized([120.0, 40.0], egui::Button::new("No GUI"));
                            });
                        }
                    });
                }
            });

            ui.separator();
            ui.add_space(4.0);

            // Tab buttons
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.current_tab, Tab::Plugins, "Plugins");
                ui.selectable_value(&mut self.current_tab, Tab::Plugin, "Plugin");
                ui.selectable_value(&mut self.current_tab, Tab::Processing, "Processing");
                ui.selectable_value(&mut self.current_tab, Tab::MidiMonitor, "MIDI Monitor");
            });
            ui.add_space(8.0);
        });

        // Route to appropriate tab content
        match self.current_tab {
            Tab::Plugins => self.show_plugins_tab(ctx),
            Tab::Plugin => self.show_plugin_tab(ctx),
            Tab::Processing => self.show_processing_tab(ctx),
            Tab::MidiMonitor => self.show_midi_monitor_tab(ctx),
        }
    }
}

impl VST3Inspector {
    /// Drain MIDI the plugin emitted during processing (arpeggiators, MPE, step
    /// sequencers, ...) and log it in the MIDI monitor as Output. The plugin only emits
    /// while it is processing audio, so this is a no-op when nothing is playing.
    fn poll_plugin_output_midi(&mut self) {
        use vst3_host::midi::MidiEvent;
        let events = match &self.audio {
            Some(a) => a.lock().take_output_midi(),
            None => return,
        };
        for ev in events {
            let (ty, ch, d1, d2): (u16, u8, u8, u8) = match ev {
                MidiEvent::NoteOn {
                    channel,
                    note,
                    velocity,
                } => (0, channel.as_index(), note, velocity),
                MidiEvent::NoteOff {
                    channel,
                    note,
                    velocity,
                } => (1, channel.as_index(), note, velocity),
                MidiEvent::ControlChange {
                    channel,
                    controller,
                    value,
                } => (3, channel.as_index(), controller, value),
                MidiEvent::ProgramChange { channel, program } => {
                    (4, channel.as_index(), program, 0)
                }
                MidiEvent::ChannelAftertouch { channel, pressure } => {
                    (2, channel.as_index(), pressure, 0)
                }
                MidiEvent::PolyAftertouch {
                    channel,
                    note,
                    pressure,
                } => (2, channel.as_index(), note, pressure),
                // PitchBend and any future variants aren't shown in the monitor's note grid.
                _ => continue,
            };
            self.log_midi_event(MidiDirection::Output, ty, ch, d1, d2);
        }
    }

    /// Reflect parameter changes the plugin made through its own editor (turning a knob in
    /// the plugin GUI calls back via the component handler) into the inspector's parameter
    /// list, so the displayed values stay in sync with the plugin's editor.
    fn poll_plugin_parameter_changes(&mut self) {
        let changes = match &self.audio {
            Some(a) => a.lock().get_parameter_changes(),
            None => return,
        };
        if changes.is_empty() {
            return;
        }
        if let Some(plugin_info) = &mut self.plugin_info {
            if let Some(controller_info) = &mut plugin_info.controller_info {
                for (id, value) in changes {
                    if let Some(p) = controller_info.parameters.iter_mut().find(|p| p.id == id) {
                        p.current_value = value;
                    }
                }
            }
        }
    }

    /// Pull the latest output levels from the playing plugin and feed the VU meter
    /// (peak + peak-hold) caches that the Processing tab reads.
    fn update_vu_meters(&mut self) {
        let levels = match &self.audio {
            Some(a) => a.lock().get_output_levels(),
            None => return,
        };

        let peak_left = levels.channels.first().map(|c| c.peak).unwrap_or(0.0);
        let peak_right = levels.channels.get(1).map(|c| c.peak).unwrap_or(peak_left);

        const SILENCE_THRESHOLD: f32 = 0.00001; // -100 dB
        const PEAK_HOLD_TIME: f64 = 3.0;
        let now = Instant::now();

        if let Ok(mut level) = self.peak_level_left.lock() {
            *level = (*level * 0.95).max(peak_left);
            if *level < SILENCE_THRESHOLD {
                *level = 0.0;
            }
        }
        if let Ok(mut level) = self.peak_level_right.lock() {
            *level = (*level * 0.95).max(peak_right);
            if *level < SILENCE_THRESHOLD {
                *level = 0.0;
            }
        }
        if let Ok(mut hold) = self.peak_hold_left.lock() {
            if peak_left > hold.0 || now.duration_since(hold.1).as_secs_f64() > PEAK_HOLD_TIME {
                *hold = (peak_left, now);
            }
        }
        if let Ok(mut hold) = self.peak_hold_right.lock() {
            if peak_right > hold.0 || now.duration_since(hold.1).as_secs_f64() > PEAK_HOLD_TIME {
                *hold = (peak_right, now);
            }
        }
    }

    fn show_plugins_tab(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("Available VST3 Plugins");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(format!("Found {} plugins", self.discovered_plugins.len()));
                if ui.button("Refresh").clicked() {
                    self.discovered_plugins =
                        discover_vst3_paths(&self.preferences.custom_plugin_paths);
                }

                // Add custom path button
                if ui.button("Add Folder...").clicked() {
                    if let Some(folder) = rfd::FileDialog::new()
                        .set_title("Select VST3 Plugin Folder")
                        .pick_folder()
                    {
                        let folder_path = folder.to_string_lossy().to_string();
                        if !self.preferences.custom_plugin_paths.contains(&folder_path) {
                            self.preferences.custom_plugin_paths.push(folder_path);
                            if let Err(e) = self.preferences.save() {
                                println!("Failed to save preferences: {}", e);
                            }
                            // Refresh plugin list
                            self.discovered_plugins =
                                discover_vst3_paths(&self.preferences.custom_plugin_paths);
                        }
                    }
                }
            });

            ui.add_space(8.0);

            // Show custom plugin paths if any exist
            if !self.preferences.custom_plugin_paths.is_empty() {
                ui.collapsing("Custom Plugin Paths", |ui| {
                    let mut paths_to_remove = Vec::new();

                    for (idx, path) in self.preferences.custom_plugin_paths.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(path);
                            if ui.small_button("Remove").clicked() {
                                paths_to_remove.push(idx);
                            }
                        });
                    }

                    // Remove paths marked for deletion
                    for idx in paths_to_remove.into_iter().rev() {
                        self.preferences.custom_plugin_paths.remove(idx);
                        if let Err(e) = self.preferences.save() {
                            println!("Failed to save preferences: {}", e);
                        }
                        // Refresh plugin list
                        self.discovered_plugins =
                            discover_vst3_paths(&self.preferences.custom_plugin_paths);
                    }
                });

                ui.add_space(8.0);
            }

            // Plugin table
            self.show_plugins_table(ui);
        });
    }

    fn show_plugins_table(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        TableBuilder::new(ui)
            .striped(true)
            .resizable(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center)) // Plugin Name
            .column(Column::remainder().at_least(200.0))
            .column(Column::remainder().at_least(300.0)) // Directory
            .column(Column::auto().at_least(80.0)) // Actions
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Plugin Name");
                });
                header.col(|ui| {
                    ui.strong("Directory");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|mut body| {
                for plugin_path in &self.discovered_plugins.clone() {
                    let plugin_name = get_plugin_name_from_path(plugin_path);
                    let directory = std::path::Path::new(plugin_path)
                        .parent()
                        .and_then(|p| p.to_str())
                        .unwrap_or("Unknown");
                    let is_current = self.plugin_path == *plugin_path;

                    // Check if this plugin is from a custom path
                    let is_custom = self
                        .preferences
                        .custom_plugin_paths
                        .iter()
                        .any(|custom_path| directory.starts_with(custom_path));

                    body.row(25.0, |mut row| {
                        // Plugin Name
                        row.col(|ui| {
                            let mut label = plugin_name.clone();
                            if is_current {
                                label = format!("[ACTIVE] {}", label);
                            }
                            if is_custom {
                                label = format!("{} [Custom]", label);
                            }

                            if is_current {
                                ui.colored_label(egui::Color32::GREEN, label);
                            } else if is_custom {
                                ui.colored_label(egui::Color32::from_rgb(100, 149, 237), label);
                            // Cornflower blue
                            } else {
                                ui.label(label);
                            }
                        });

                        // Directory
                        row.col(|ui| {
                            ui.label(plugin_path);
                        });

                        // Actions
                        row.col(|ui| {
                            if is_current {
                                ui.label("Current");
                            } else if ui.button("Load").clicked() {
                                self.load_plugin(plugin_path.clone());
                                self.current_tab = Tab::Plugin; // Switch to plugin tab after loading
                            }
                        });
                    });
                }
            });
    }

    fn show_plugin_tab(&mut self, ctx: &egui::Context) {
        // Left sidebar for plugin information
        egui::SidePanel::left("plugin_info_panel")
            .resizable(true)
            .default_width(300.0)
            .min_width(250.0)
            .max_width(500.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);

                ui.heading("Plugin Information");
                ui.add_space(4.0);

                // Export the full plugin report (metadata + bus layout + parameters) as JSON.
                ui.add_enabled_ui(self.report_json.is_some(), |ui| {
                    if ui
                        .button("Copy JSON")
                        .on_hover_text(
                            "Copy this plugin's full report (metadata, buses, parameters) as JSON",
                        )
                        .clicked()
                    {
                        if let Some(json) = &self.report_json {
                            ctx.copy_text(json.clone());
                        }
                    }
                });
                ui.add_space(8.0);

                // Make the plugin information section scrollable
                egui::ScrollArea::vertical()
                    .id_salt("plugin_info_scroll")
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        if let Some(plugin_info) = &self.plugin_info {
                            // Plugin identity summary — accurate library metadata.
                            let s = &plugin_info.summary;
                            ui.label(egui::RichText::new("Plugin").strong());
                            ui.add_space(2.0);
                            egui::Grid::new("plugin_summary_grid")
                                .num_columns(2)
                                .spacing([10.0, 4.0])
                                .show(ui, |ui| {
                                    let dash = |t: &str| {
                                        if t.is_empty() {
                                            "—".to_string()
                                        } else {
                                            t.to_string()
                                        }
                                    };
                                    ui.label("Name:");
                                    ui.label(&s.name);
                                    ui.end_row();
                                    ui.label("Vendor:");
                                    ui.label(&s.vendor);
                                    ui.end_row();
                                    ui.label("Version:");
                                    ui.label(dash(&s.version));
                                    ui.end_row();
                                    ui.label("Category:");
                                    ui.label(dash(&s.category));
                                    ui.end_row();
                                    ui.label("Audio I/O:");
                                    ui.label(format!(
                                        "{} in / {} out",
                                        s.audio_inputs, s.audio_outputs
                                    ));
                                    ui.end_row();
                                    let yn = |b: bool| if b { "yes" } else { "no" };
                                    ui.label("MIDI:");
                                    ui.label(format!(
                                        "in: {}   out: {}",
                                        yn(s.has_midi_input),
                                        yn(s.has_midi_output),
                                    ));
                                    ui.end_row();
                                    ui.label("Editor:");
                                    ui.label(yn(s.has_gui));
                                    ui.end_row();
                                    ui.label("UID:");
                                    ui.label(egui::RichText::new(&s.uid).monospace().small());
                                    ui.end_row();
                                });
                            ui.add_space(8.0);

                            // Factory Information - collapsible
                            egui::CollapsingHeader::new("Factory Information")
                                .id_source("factory_info_header")
                                .show(ui, |ui| {
                                    ui.add_space(4.0);
                                    egui::Grid::new("factory_info_grid")
                                        .num_columns(2)
                                        .spacing([10.0, 4.0])
                                        .show(ui, |ui| {
                                            ui.label("Vendor:");
                                            ui.label(&plugin_info.factory_info.vendor);
                                            ui.end_row();

                                            ui.label("URL:");
                                            ui.label(&plugin_info.factory_info.url);
                                            ui.end_row();

                                            ui.label("Email:");
                                            ui.label(&plugin_info.factory_info.email);
                                            ui.end_row();

                                            ui.label("Flags:");
                                            ui.label(format!(
                                                "0x{:x}",
                                                plugin_info.factory_info.flags
                                            ));
                                            ui.end_row();
                                        });
                                    ui.add_space(4.0);
                                });

                            ui.add_space(8.0);

                            // Plugin Classes - collapsible
                            ui.collapsing("Plugin Classes", |ui| {
                                if plugin_info.classes.is_empty() {
                                    ui.label("No classes found.");
                                } else {
                                    for (i, class) in plugin_info.classes.iter().enumerate() {
                                        ui.group(|ui| {
                                            ui.strong(format!("Class {}: {}", i, class.name));
                                            ui.separator();
                                            egui::Grid::new(format!("class_grid_{}", i))
                                                .num_columns(2)
                                                .spacing([10.0, 2.0])
                                                .show(ui, |ui| {
                                                    ui.label("Category:");
                                                    ui.label(&class.category);
                                                    ui.end_row();

                                                    ui.label("Flags:");
                                                    ui.label(format!("0x{:x}", class.cardinality));
                                                    ui.end_row();
                                                });
                                        });
                                        ui.add_space(4.0);
                                    }
                                }
                                ui.add_space(4.0);
                            });

                            ui.add_space(8.0);

                            // Component Information - collapsible
                            if let Some(ref info) = plugin_info.component_info {
                                egui::CollapsingHeader::new("Component Information")
                                    .id_source("component_info_header")
                                    .show(ui, |ui| {
                                        ui.strong("Bus Counts");
                                        egui::Grid::new("component_bus_counts_grid")
                                            .num_columns(2)
                                            .spacing([10.0, 4.0])
                                            .show(ui, |ui| {
                                                ui.label("Audio Inputs:");
                                                ui.label(info.audio_inputs.len().to_string());
                                                ui.end_row();

                                                ui.label("Audio Outputs:");
                                                ui.label(info.audio_outputs.len().to_string());
                                                ui.end_row();

                                                ui.label("Event Inputs:");
                                                ui.label(info.event_inputs.len().to_string());
                                                ui.end_row();

                                                ui.label("Event Outputs:");
                                                ui.label(info.event_outputs.len().to_string());
                                                ui.end_row();
                                            });

                                        ui.add_space(8.0);

                                        if !info.audio_inputs.is_empty() {
                                            ui.strong("Audio Inputs");
                                            for bus in info.audio_inputs.iter() {
                                                ui.label(format!(
                                                    "  {} - {} channels",
                                                    bus.name, bus.channel_count
                                                ));
                                            }
                                            ui.add_space(4.0);
                                        }

                                        if !info.audio_outputs.is_empty() {
                                            ui.strong("Audio Outputs");
                                            for bus in info.audio_outputs.iter() {
                                                ui.label(format!(
                                                    "  {} - {} channels",
                                                    bus.name, bus.channel_count
                                                ));
                                            }
                                            ui.add_space(4.0);
                                        }

                                        ui.add_space(4.0);
                                    });
                            }

                            // GUI Information - collapsible
                            egui::CollapsingHeader::new("GUI Information")
                                .id_source("gui_info_header")
                                .show(ui, |ui| {
                                    ui.add_space(4.0);
                                    egui::Grid::new("gui_information_grid")
                                        .num_columns(2)
                                        .spacing([10.0, 4.0])
                                        .show(ui, |ui| {
                                            ui.label("Has GUI:");
                                            if plugin_info.has_gui {
                                                ui.colored_label(egui::Color32::GREEN, "Yes");
                                            } else {
                                                ui.colored_label(egui::Color32::GRAY, "No");
                                            }
                                            ui.end_row();

                                            if let Some((width, height)) = plugin_info.gui_size {
                                                ui.label("GUI Size:");
                                                ui.label(format!("{}x{}", width, height));
                                                ui.end_row();
                                            }
                                        });
                                    ui.add_space(4.0);
                                });
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(50.0);
                                ui.label("No plugin loaded");
                                ui.add_space(10.0);
                                ui.label("Load a VST3 plugin to view its information");
                            });
                        }
                    });
            });

        // Central panel for parameters
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("Parameter Control");
            ui.add_space(8.0);

            // Clone the plugin info to avoid borrowing issues
            let plugin_info_clone = self.plugin_info.clone();

            if let Some(plugin_info) = plugin_info_clone {
                if let Some(ref info) = plugin_info.controller_info {
                    // Get filtered parameters first
                    let filtered_params = self.get_filtered_parameters(&info.parameters);

                    // Parameter editor (shown prominently at top when selected)
                    if let Some(selected_index) = self.selected_parameter {
                        if let Some((_, selected_param)) = filtered_params
                            .iter()
                            .find(|(idx, _)| *idx == selected_index)
                        {
                            ui.group(|ui| {
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.heading("Parameter Editor");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.button("Close").clicked() {
                                                self.selected_parameter = None;
                                            }
                                        },
                                    );
                                });
                                ui.separator();
                                ui.add_space(4.0);
                                self.show_parameter_editor(ui, selected_param);
                                ui.add_space(8.0);
                            });
                            ui.add_space(8.0);
                        }
                    }

                    // Control panel
                    ui.group(|ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            // Stats
                            ui.vertical(|ui| {
                                ui.strong(format!("{} Parameters Total", info.parameter_count));
                                if filtered_params.len() != info.parameters.len() {
                                    ui.label(format!("{} Filtered", filtered_params.len()));
                                }
                            });

                            ui.separator();

                            // Actions
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    if ui.button("Refresh Values").clicked() {
                                        if let Err(e) = self.refresh_parameter_values() {
                                            println!("Failed to refresh parameters: {}", e);
                                        }
                                    }
                                });
                            });
                        });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);

                        // Search and filter controls
                        ui.horizontal(|ui| {
                            ui.label("Search:");
                            let search_response =
                                ui.text_edit_singleline(&mut self.parameter_search);
                            if search_response.changed() {
                                self.current_page = 0;
                                self.table_scroll_to_selected = true;
                            }

                            if ui.button("Clear").clicked() {
                                self.parameter_search.clear();
                                self.current_page = 0;
                            }

                            ui.separator();

                            ui.label("Filter:");
                            let filter_changed = egui::ComboBox::from_label("")
                                .selected_text(format!("{:?}", self.parameter_filter))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.parameter_filter,
                                        ParameterFilter::All,
                                        "All Parameters",
                                    )
                                    .clicked()
                                        || ui
                                            .selectable_value(
                                                &mut self.parameter_filter,
                                                ParameterFilter::Writable,
                                                "Writable Only",
                                            )
                                            .clicked()
                                        || ui
                                            .selectable_value(
                                                &mut self.parameter_filter,
                                                ParameterFilter::ReadOnly,
                                                "Read-Only",
                                            )
                                            .clicked()
                                        || ui
                                            .selectable_value(
                                                &mut self.parameter_filter,
                                                ParameterFilter::HasSteps,
                                                "Has Steps",
                                            )
                                            .clicked()
                                        || ui
                                            .selectable_value(
                                                &mut self.parameter_filter,
                                                ParameterFilter::HasUnits,
                                                "Has Units",
                                            )
                                            .clicked()
                                })
                                .inner
                                .unwrap_or(false);

                            if filter_changed {
                                self.current_page = 0;
                            }

                            let modified_changed =
                                ui.checkbox(&mut self.show_only_modified, "Modified Only");
                            if modified_changed.changed() {
                                self.current_page = 0;
                            }
                        });
                        ui.add_space(4.0);
                    });

                    ui.add_space(8.0);

                    // Pagination and table
                    if !filtered_params.is_empty() {
                        let total_pages = filtered_params.len().div_ceil(self.items_per_page);
                        let start_idx = self.current_page * self.items_per_page;
                        let end_idx = (start_idx + self.items_per_page).min(filtered_params.len());

                        // Pagination controls
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "Page {} of {} - Showing {}-{} of {} parameters",
                                    self.current_page + 1,
                                    total_pages,
                                    start_idx + 1,
                                    end_idx,
                                    filtered_params.len()
                                ));

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        // Items per page
                                        egui::ComboBox::from_label("Items per page")
                                            .selected_text(self.items_per_page.to_string())
                                            .show_ui(ui, |ui| {
                                                for &size in &[25, 50, 100, 200] {
                                                    if ui
                                                        .selectable_value(
                                                            &mut self.items_per_page,
                                                            size,
                                                            size.to_string(),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.current_page = 0;
                                                    }
                                                }
                                            });

                                        ui.separator();

                                        // Navigation
                                        ui.add_enabled_ui(
                                            self.current_page + 1 < total_pages,
                                            |ui| {
                                                if ui.button("Next >>").clicked() {
                                                    self.current_page += 1;
                                                }
                                            },
                                        );

                                        ui.add_enabled_ui(self.current_page > 0, |ui| {
                                            if ui.button("<< Previous").clicked() {
                                                self.current_page -= 1;
                                            }
                                        });
                                    },
                                );
                            });
                        });

                        ui.add_space(8.0);

                        // Get current page parameters
                        let page_params: Vec<_> = filtered_params
                            .iter()
                            .skip(start_idx)
                            .take(self.items_per_page)
                            .cloned()
                            .collect();

                        self.show_parameter_table(ui, &page_params);
                    } else if !info.parameters.is_empty() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(50.0);
                            ui.label("No parameters match the current filter criteria.");
                            ui.add_space(10.0);
                            ui.label("Try adjusting your search or filter settings.");
                        });
                    } else {
                        ui.vertical_centered(|ui| {
                            ui.add_space(50.0);
                            ui.label("No parameters found");
                        });
                    }
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(50.0);
                        ui.label("No controller information available");
                    });
                }
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(100.0);
                    ui.heading("VST3 Plugin Inspector");
                    ui.add_space(20.0);
                    ui.label("Load a VST3 plugin to begin inspection");
                });
            }
        });
    }

    fn show_parameter_table(
        &mut self,
        ui: &mut egui::Ui,
        filtered_params: &[(usize, &ParameterInfo)],
    ) {
        use egui_extras::{Column, TableBuilder};

        TableBuilder::new(ui)
            .striped(true)
            .resizable(false)
            .animate_scrolling(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::auto().at_least(40.0)) // Index
            .column(Column::auto().at_least(60.0)) // ID
            .column(Column::remainder().at_least(180.0)) // Title
            .column(Column::auto().at_least(150.0)) // Current Value (Slider)
            .column(Column::auto().at_least(70.0)) // Default
            .column(Column::auto().at_least(50.0)) // Units
            .column(Column::auto().at_least(50.0)) // Steps
            .column(Column::auto().at_least(80.0)) // Actions
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Index");
                });
                header.col(|ui| {
                    ui.strong("ID");
                });
                header.col(|ui| {
                    ui.strong("Parameter Name");
                });
                header.col(|ui| {
                    ui.strong("Value");
                });
                header.col(|ui| {
                    ui.strong("Default");
                });
                header.col(|ui| {
                    ui.strong("Units");
                });
                header.col(|ui| {
                    ui.strong("Steps");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|mut body| {
                for (original_index, param) in filtered_params {
                    let is_selected = self.selected_parameter == Some(*original_index);
                    let is_modified =
                        (param.current_value - param.default_normalized_value).abs() > 0.001;
                    let is_read_only = (param.flags & 0x1) != 0;

                    body.row(30.0, |mut row| {
                        // Index
                        row.col(|ui| {
                            if is_selected {
                                ui.colored_label(
                                    egui::Color32::YELLOW,
                                    format!("> {}", original_index),
                                );
                            } else {
                                ui.label(original_index.to_string());
                            }
                        });

                        // ID
                        row.col(|ui| {
                            ui.label(param.id.to_string());
                        });

                        // Title
                        row.col(|ui| {
                            if is_modified {
                                ui.colored_label(egui::Color32::LIGHT_GREEN, &param.title);
                            } else {
                                ui.label(&param.title);
                            }
                        });

                        // Current Value - Inline Editor
                        row.col(|ui| {
                            if is_read_only {
                                // Read-only parameters - just show the value
                                ui.add_enabled(false, |ui: &mut egui::Ui| {
                                    ui.label(format!("{:.3}", param.current_value))
                                });
                            } else {
                                // Editable parameters - show slider or drag value
                                let mut new_value = param.current_value as f32;
                                let step_size = if param.step_count > 0 {
                                    1.0 / param.step_count as f32
                                } else {
                                    0.001
                                };

                                let is_being_edited = self.parameter_being_edited == Some(param.id);

                                ui.horizontal(|ui| {
                                    let _response = if param.step_count > 0
                                        && param.step_count <= 10
                                    {
                                        // For parameters with few steps, use a combo box
                                        let current_step =
                                            (param.current_value * param.step_count as f64).round()
                                                as i32;
                                        let mut selected_step = current_step;

                                        let combo_response = egui::ComboBox::from_id_source(
                                            format!("param_{}", param.id),
                                        )
                                        .selected_text(format!("{}", current_step))
                                        .width(60.0)
                                        .show_ui(ui, |ui| {
                                            let mut changed = false;
                                            for step in 0..=param.step_count {
                                                if ui
                                                    .selectable_value(
                                                        &mut selected_step,
                                                        step,
                                                        format!("{}", step),
                                                    )
                                                    .clicked()
                                                {
                                                    changed = true;
                                                }
                                            }
                                            changed
                                        });

                                        if combo_response.inner.unwrap_or(false) {
                                            new_value =
                                                selected_step as f32 / param.step_count as f32;
                                            self.parameter_being_edited = Some(param.id);
                                            if let Err(e) =
                                                self.set_parameter_value(param.id, new_value as f64)
                                            {
                                                println!("Failed to set parameter: {}", e);
                                            }
                                        }
                                        combo_response.response
                                    } else {
                                        // For continuous parameters, use a compact slider
                                        let slider_response = ui.add_sized(
                                            [100.0, 20.0],
                                            egui::Slider::new(&mut new_value, 0.0..=1.0)
                                                .step_by(step_size as f64)
                                                .show_value(false),
                                        );

                                        if slider_response.changed() {
                                            self.parameter_being_edited = Some(param.id);
                                            if let Err(e) =
                                                self.set_parameter_value(param.id, new_value as f64)
                                            {
                                                println!("Failed to set parameter: {}", e);
                                            }
                                        }

                                        if slider_response.drag_stopped() {
                                            self.parameter_being_edited = None;
                                        }

                                        slider_response
                                    };

                                    // Show numeric value with enhanced visual feedback
                                    let color = if is_being_edited {
                                        egui::Color32::YELLOW
                                    } else if is_modified {
                                        egui::Color32::LIGHT_GREEN
                                    } else {
                                        ui.style().visuals.text_color()
                                    };
                                    ui.colored_label(color, format!("{:.3}", param.current_value));
                                });
                            }
                        });

                        // Default Value
                        row.col(|ui| {
                            ui.label(format!("{:.3}", param.default_normalized_value));
                        });

                        // Units
                        row.col(|ui| {
                            ui.label(&param.units);
                        });

                        // Steps
                        row.col(|ui| {
                            if param.step_count > 0 {
                                ui.label(param.step_count.to_string());
                            } else {
                                ui.label("∞");
                            }
                        });

                        // Actions
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                if is_modified
                                    && ui
                                        .small_button("Reset")
                                        .on_hover_text("Reset to default")
                                        .clicked()
                                {
                                    if let Err(e) = self.set_parameter_value(
                                        param.id,
                                        param.default_normalized_value,
                                    ) {
                                        println!("Failed to reset parameter: {}", e);
                                    }
                                }

                                if ui
                                    .small_button("Edit")
                                    .on_hover_text("Show detailed editor")
                                    .clicked()
                                {
                                    self.selected_parameter = Some(*original_index);
                                    self.table_scroll_to_selected = true;
                                }
                            });
                        });
                    });
                }
            });
    }

    fn show_parameter_editor(&mut self, ui: &mut egui::Ui, param: &ParameterInfo) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.strong(format!("Editing: {}", param.title));
                    ui.label(format!("ID: {} | Range: 0.0 - 1.0", param.id));
                    if !param.units.is_empty() {
                        ui.label(format!("Units: {}", param.units));
                    }
                });

                ui.separator();

                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Value:");

                        let mut new_value = param.current_value as f32;
                        let step_size = if param.step_count > 0 {
                            1.0 / param.step_count as f32
                        } else {
                            0.001
                        };

                        let slider_response = ui.add(
                            egui::Slider::new(&mut new_value, 0.0..=1.0)
                                .step_by(step_size as f64)
                                .show_value(true),
                        );

                        if slider_response.changed() {
                            if let Err(e) = self.set_parameter_value(param.id, new_value as f64) {
                                println!("Failed to set parameter: {}", e);
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        if ui.button("Reset to Default").clicked() {
                            if let Err(e) =
                                self.set_parameter_value(param.id, param.default_normalized_value)
                            {
                                println!("Failed to reset parameter: {}", e);
                            }
                        }

                        if ui.button("Set to 0.0").clicked() {
                            if let Err(e) = self.set_parameter_value(param.id, 0.0) {
                                println!("Failed to set parameter: {}", e);
                            }
                        }

                        if ui.button("Set to 1.0").clicked() {
                            if let Err(e) = self.set_parameter_value(param.id, 1.0) {
                                println!("Failed to set parameter: {}", e);
                            }
                        }

                        if ui.button("Close Editor").clicked() {
                            self.selected_parameter = None;
                        }
                    });
                });
            });
        });
    }

    fn show_processing_tab(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("Audio & MIDI Processing");
            ui.add_space(8.0);

            if self.plugin_info.is_none() {
                ui.label("No plugin loaded. Please load a plugin first.");
                return;
            }

            // Processing controls
            ui.horizontal(|ui| {
                ui.label("Processing State:");
                if self.is_processing {
                    ui.colored_label(egui::Color32::GREEN, "Active");
                    if ui.button("Stop Processing").clicked() {
                        self.stop_processing();
                    }
                } else {
                    ui.colored_label(egui::Color32::RED, "Stopped");
                    if ui.button("Start Processing").clicked() {
                        if let Err(e) = self.start_processing() {
                            println!("Failed to start processing: {}", e);
                        }
                    }
                }
            });

            ui.separator();

            // Audio Output — the library opens the default device and starts the audio
            // stream as part of `Vst3Host::play()`, so "running" simply means a plugin
            // is loaded and playing.
            ui.horizontal(|ui| {
                ui.label("Audio Output:");
                if self.audio.is_some() {
                    ui.colored_label(egui::Color32::GREEN, "Running");
                } else {
                    ui.colored_label(egui::Color32::RED, "Not running (no plugin loaded)");
                }
            });

            // Audio settings — these reflect the host configuration chosen at startup.
            // The library fixes sample rate / block size when the host is built, so these
            // are shown for reference and apply to subsequently loaded plugins.
            ui.horizontal(|ui| {
                ui.label("Sample Rate:");
                let sample_rates = [44100.0, 48000.0, 88200.0, 96000.0, 176400.0, 192000.0];
                let current_rate_text = format!("{} Hz", self.sample_rate as u32);
                egui::ComboBox::from_id_source("sample_rate_selector")
                    .selected_text(&current_rate_text)
                    .show_ui(ui, |ui| {
                        for &rate in &sample_rates {
                            let rate_text = format!("{} Hz", rate as u32);
                            ui.selectable_value(&mut self.sample_rate, rate, &rate_text);
                        }
                    });

                ui.separator();
                ui.label("Block Size:");
                let block_sizes = [64, 128, 256, 512, 1024, 2048, 4096];
                let current_block_text = format!("{} samples", self.block_size);
                egui::ComboBox::from_id_source("block_size_selector")
                    .selected_text(&current_block_text)
                    .show_ui(ui, |ui| {
                        for &size in &block_sizes {
                            let size_text = format!("{} samples", size);
                            ui.selectable_value(&mut self.block_size, size, &size_text);
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
                                egui::Color32::RED // Clipping warning
                            } else if db_left > -12.0 {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::GREEN
                            };

                            // VU meter bar with peak hold indicator
                            let bar_value = if db_left.is_finite() {
                                ((db_left - MIN_DB) / -MIN_DB).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };

                            // Calculate peak hold position
                            let hold_value = if db_hold_left.is_finite() {
                                ((db_hold_left - MIN_DB) / -MIN_DB).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };

                            // Draw the VU meter bar
                            let bar_rect = ui
                                .add(
                                    egui::ProgressBar::new(bar_value)
                                        .desired_width(200.0)
                                        .fill(color),
                                )
                                .rect;

                            // Draw peak hold indicator as a vertical line
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
                                egui::Color32::RED // Clipping warning
                            } else if db_right > -12.0 {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::GREEN
                            };

                            // VU meter bar with peak hold indicator
                            let bar_value = if db_right.is_finite() {
                                ((db_right - MIN_DB) / -MIN_DB).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };

                            // Calculate peak hold position
                            let hold_value = if db_hold_right.is_finite() {
                                ((db_hold_right - MIN_DB) / -MIN_DB).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };

                            // Draw the VU meter bar
                            let bar_rect = ui
                                .add(
                                    egui::ProgressBar::new(bar_value)
                                        .desired_width(200.0)
                                        .fill(color),
                                )
                                .rect;

                            // Draw peak hold indicator as a vertical line
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

                    if ui.button("MIDI Panic").clicked() {
                        self.send_midi_panic();
                    }

                    if ui.button("Audio Panic").clicked() {
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
                        // Create channel options
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
            });

            ui.separator();
            ui.add_space(8.0);

            // Bus information
            if let Some(info) = &self.plugin_info {
                if let Some(comp_info) = &info.component_info {
                    ui.heading("Audio Buses");

                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label("Input Buses:");
                            for (i, bus) in comp_info.audio_inputs.iter().enumerate() {
                                ui.label(format!(
                                    "  {} [{}]: {} channels",
                                    i, bus.name, bus.channel_count
                                ));
                            }
                            if comp_info.audio_inputs.is_empty() {
                                ui.label("  None");
                            }
                        });

                        ui.separator();

                        ui.vertical(|ui| {
                            ui.label("Output Buses:");
                            for (i, bus) in comp_info.audio_outputs.iter().enumerate() {
                                ui.label(format!(
                                    "  {} [{}]: {} channels",
                                    i, bus.name, bus.channel_count
                                ));
                            }
                            if comp_info.audio_outputs.is_empty() {
                                ui.label("  None");
                            }
                        });
                    });

                    ui.add_space(8.0);

                    ui.heading("Event Buses");

                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label("Event Input Buses:");
                            for (i, bus) in comp_info.event_inputs.iter().enumerate() {
                                ui.label(format!(
                                    "  {} [{}]: {} channels",
                                    i, bus.name, bus.channel_count
                                ));
                            }
                            if comp_info.event_inputs.is_empty() {
                                ui.label("  None");
                            }
                        });

                        ui.separator();

                        ui.vertical(|ui| {
                            ui.label("Event Output Buses:");
                            for (i, bus) in comp_info.event_outputs.iter().enumerate() {
                                ui.label(format!(
                                    "  {} [{}]: {} channels",
                                    i, bus.name, bus.channel_count
                                ));
                            }
                            if comp_info.event_outputs.is_empty() {
                                ui.label("  None");
                            }
                        });
                    });
                }
            }
        });
    }

    fn show_midi_monitor_tab(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("MIDI Monitor");
            ui.add_space(8.0);

            // Controls
            ui.horizontal(|ui| {
                let is_paused = *self.midi_monitor_paused.lock().unwrap();
                if is_paused {
                    if ui.button("[Resume]").clicked() {
                        *self.midi_monitor_paused.lock().unwrap() = false;
                    }
                } else if ui.button("[Pause]").clicked() {
                    *self.midi_monitor_paused.lock().unwrap() = true;
                }

                if ui.button("Clear").clicked() {
                    self.midi_events.lock().unwrap().clear();
                }

                ui.separator();
                let event_count = self.midi_events.lock().unwrap().len();
                ui.label(format!("Events: {}", event_count));

                if event_count >= self.max_midi_events {
                    ui.colored_label(egui::Color32::YELLOW, "(buffer full)");
                }
            });

            ui.separator();

            // Filters
            ui.collapsing("Filters", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.midi_event_filter.show_note_events, "Note On/Off");
                    ui.checkbox(&mut self.midi_event_filter.show_cc_events, "Control Change");
                    ui.checkbox(
                        &mut self.midi_event_filter.show_program_change,
                        "Program Change",
                    );
                    ui.checkbox(&mut self.midi_event_filter.show_pitch_bend, "Pitch Bend");
                    ui.checkbox(&mut self.midi_event_filter.show_aftertouch, "Aftertouch");
                    ui.checkbox(&mut self.midi_event_filter.show_system_events, "System");
                    ui.checkbox(
                        &mut self.midi_event_filter.show_clock_events,
                        "Clock/Timing",
                    );
                    ui.checkbox(
                        &mut self.midi_event_filter.show_active_sensing,
                        "Active Sensing",
                    );
                });

                ui.horizontal(|ui| {
                    if ui.button("Show All").clicked() {
                        self.midi_event_filter = MidiEventFilter {
                            show_note_events: true,
                            show_cc_events: true,
                            show_program_change: true,
                            show_pitch_bend: true,
                            show_aftertouch: true,
                            show_system_events: true,
                            show_clock_events: true,
                            show_active_sensing: true,
                        };
                    }
                    if ui.button("Hide All").clicked() {
                        self.midi_event_filter = MidiEventFilter {
                            show_note_events: false,
                            show_cc_events: false,
                            show_program_change: false,
                            show_pitch_bend: false,
                            show_aftertouch: false,
                            show_system_events: false,
                            show_clock_events: false,
                            show_active_sensing: false,
                        };
                    }
                });
            });

            ui.separator();

            // Event list using proper table
            use egui_extras::{Column, TableBuilder};

            // Get events and calculate start time
            let events = self.midi_events.lock().unwrap().clone();
            let start_time = events
                .first()
                .map(|e| e.timestamp)
                .unwrap_or_else(Instant::now);

            // Filter events
            let filtered_events: Vec<_> = events
                .iter()
                .rev() // Show newest first
                .filter(|event| self.should_show_event(event))
                .collect();

            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(80.0)) // Time
                .column(Column::exact(50.0)) // Direction
                .column(Column::exact(100.0)) // Type
                .column(Column::exact(40.0)) // Channel
                .column(Column::exact(80.0)) // Data
                .column(Column::remainder()) // Description
                .header(20.0, |mut header| {
                    header.col(|ui| {
                        ui.strong("Time");
                    });
                    header.col(|ui| {
                        ui.strong("Dir");
                    });
                    header.col(|ui| {
                        ui.strong("Type");
                    });
                    header.col(|ui| {
                        ui.strong("Ch");
                    });
                    header.col(|ui| {
                        ui.strong("Data");
                    });
                    header.col(|ui| {
                        ui.strong("Description");
                    });
                })
                .body(|mut body| {
                    for event in filtered_events {
                        body.row(20.0, |mut row| {
                            // Time
                            row.col(|ui| {
                                let elapsed =
                                    event.timestamp.duration_since(start_time).as_secs_f64();
                                ui.monospace(format!("{:8.3}", elapsed));
                            });

                            // Direction
                            row.col(|ui| {
                                let dir_color = match event.direction {
                                    MidiDirection::Input => egui::Color32::from_rgb(100, 200, 100),
                                    MidiDirection::Output => egui::Color32::from_rgb(100, 150, 200),
                                };
                                ui.colored_label(
                                    dir_color,
                                    match event.direction {
                                        MidiDirection::Input => "In",
                                        MidiDirection::Output => "Out",
                                    },
                                );
                            });

                            // Type
                            row.col(|ui| {
                                ui.monospace(self.event_type_name(&event.event_type));
                            });

                            // Channel
                            row.col(|ui| {
                                let channel = match &event.event_type {
                                    MidiEventType::NoteOn { channel, .. }
                                    | MidiEventType::NoteOff { channel, .. }
                                    | MidiEventType::ControlChange { channel, .. }
                                    | MidiEventType::ProgramChange { channel, .. }
                                    | MidiEventType::PitchBend { channel, .. } => *channel + 1,
                                    _ => event.channel as i16 + 1,
                                };
                                ui.monospace(format!("{:2}", channel));
                            });

                            // Data
                            row.col(|ui| {
                                ui.monospace(format!("{:3} {:3}", event.data1, event.data2));
                            });

                            // Description
                            row.col(|ui| {
                                ui.label(self.format_event_description(event));
                            });
                        });
                    }
                });
        });
    }

    // Open the plugin's native editor in a standalone window (via the library's PluginWindow).
    // In-process plugins only — editors across process isolation aren't bridged yet.
    fn create_plugin_gui(&mut self) -> Result<(), String> {
        let Some(audio) = self.audio.as_ref() else {
            return Err("No plugin loaded".into());
        };
        let mut window = vst3_host::PluginWindow::new(audio.plugin());
        if let Err(e) = window.open() {
            let msg = format!("Failed to open editor: {e}");
            self.last_error = Some(msg.clone());
            return Err(msg);
        }
        self.plugin_window = Some(window);
        self.gui_attached = true;
        Ok(())
    }

    fn close_plugin_gui(&mut self) {
        // Dropping the window closes the editor and the native window.
        self.plugin_window = None;
        self.gui_attached = false;
    }

    fn set_parameter_value(&mut self, param_id: u32, normalized_value: f64) -> Result<(), String> {
        let audio = match &self.audio {
            Some(a) => a,
            None => return Err("No plugin loaded".to_string()),
        };

        audio
            .lock()
            .set_parameter(param_id, normalized_value)
            .map_err(|e| format!("Failed to set parameter: {e}"))?;

        // Update our cached parameter info for display.
        if let Some(ref mut plugin_info) = self.plugin_info {
            if let Some(ref mut controller_info) = plugin_info.controller_info {
                if let Some(param) = controller_info
                    .parameters
                    .iter_mut()
                    .find(|p| p.id == param_id)
                {
                    param.current_value = normalized_value;
                }
            }
        }
        Ok(())
    }

    fn refresh_parameter_values(&mut self) -> Result<(), String> {
        let audio = match &self.audio {
            Some(a) => a,
            None => return Err("No plugin loaded".to_string()),
        };

        let plugin = audio.lock();
        if let Some(ref mut plugin_info) = self.plugin_info {
            if let Some(ref mut controller_info) = plugin_info.controller_info {
                for param in &mut controller_info.parameters {
                    if let Ok(v) = plugin.get_parameter(param.id) {
                        param.current_value = v;
                    }
                }
            }
        }
        Ok(())
    }

    fn get_filtered_parameters<'a>(
        &self,
        parameters: &'a [ParameterInfo],
    ) -> Vec<(usize, &'a ParameterInfo)> {
        parameters
            .iter()
            .enumerate()
            .filter(|(_, param)| {
                // Search filter
                if !self.parameter_search.is_empty() {
                    let search_lower = self.parameter_search.to_lowercase();
                    let title_match = param.title.to_lowercase().contains(&search_lower);
                    let id_match = param.id.to_string().contains(&search_lower);
                    let units_match = param.units.to_lowercase().contains(&search_lower);

                    if !(title_match || id_match || units_match) {
                        return false;
                    }
                }

                // Type filter
                let type_matches = match self.parameter_filter {
                    ParameterFilter::All => true,
                    ParameterFilter::Writable => (param.flags & 0x1) == 0, // Not read-only
                    ParameterFilter::ReadOnly => (param.flags & 0x1) != 0, // Read-only
                    ParameterFilter::HasSteps => param.step_count > 0,
                    ParameterFilter::HasUnits => !param.units.is_empty(),
                };

                // Modified filter
                let modified_matches = !self.show_only_modified
                    || (param.current_value - param.default_normalized_value).abs() > 0.001;

                type_matches && modified_matches
            })
            .collect()
    }

    /// Load a plugin through the `vst3-host` library and start playing it.
    ///
    /// The loaded `Plugin` lives inside `self.audio` (an `AudioHandle`) for its whole
    /// lifetime; all parameter / MIDI / processing access goes through `self.audio.lock()`.
    fn load_plugin(&mut self, plugin_path: String) {
        println!("Loading plugin: {}", plugin_path);

        // Drop any previously playing plugin first (stops audio, releases the device).
        self.audio = None;
        self.plugin_info = None;
        self.selected_parameter = None;
        self.current_page = 0;
        self.plugin_window = None; // close any open editor from the previous plugin
        self.gui_attached = false;
        self.is_processing = false;
        self.last_error = None;

        // Build the inspector's detailed PluginInfo from the library's introspection.
        let detail = match vst3_host::get_detailed_plugin_info(std::path::Path::new(&plugin_path)) {
            Ok(d) => d,
            Err(e) => {
                let msg = format!("Failed to introspect plugin: {e}");
                println!("{msg}");
                self.last_error = Some(msg);
                return;
            }
        };

        // Load + play the plugin via the library.
        let plugin = match self.host.load_plugin(&plugin_path) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("Failed to load plugin: {e}");
                println!("{msg}");
                self.last_error = Some(msg);
                return;
            }
        };

        // Read the parameter list before the plugin is moved into `play`.
        let params = plugin.get_parameters().unwrap_or_default();

        let audio = match self.host.play(plugin) {
            Ok(a) => a,
            Err(e) => {
                let msg = format!("Failed to start audio playback: {e}");
                println!("{msg}");
                self.last_error = Some(msg);
                return;
            }
        };

        // Prebuild the JSON export (full report) so "Copy JSON" never re-introspects a
        // plugin that's currently loaded.
        self.report_json = vst3_host::PluginReport::new(detail.clone(), params.clone())
            .to_json()
            .ok();

        self.plugin_info = Some(Self::build_plugin_info(&detail, &params));
        self.is_processing = audio.lock().is_processing();
        self.audio = Some(audio);
        self.plugin_path = plugin_path;

        // Auto-start processing if enabled in preferences.
        if self.preferences.auto_start_processing {
            if let Err(e) = self.start_processing() {
                println!("Failed to auto-start processing: {}", e);
            }
        }

        println!("Plugin loaded successfully!");
    }

    /// Map the library's `DetailedPluginInfo` + parameter list into the inspector's own
    /// `PluginInfo` (which drives the existing UI rendering).
    fn build_plugin_info(
        detail: &vst3_host::DetailedPluginInfo,
        params: &[vst3_host::parameters::Parameter],
    ) -> PluginInfo {
        let map_buses = |buses: &[vst3_host::BusInfo]| -> Vec<BusInfo> {
            buses
                .iter()
                .map(|b| BusInfo {
                    name: b.name.clone(),
                    bus_type: b.bus_type,
                    flags: b.flags,
                    channel_count: b.channel_count,
                })
                .collect()
        };

        let component_info = ComponentInfo {
            bus_count_inputs: detail.buses.audio_inputs.len() as i32,
            bus_count_outputs: detail.buses.audio_outputs.len() as i32,
            audio_inputs: map_buses(&detail.buses.audio_inputs),
            audio_outputs: map_buses(&detail.buses.audio_outputs),
            event_inputs: map_buses(&detail.buses.event_inputs),
            event_outputs: map_buses(&detail.buses.event_outputs),
            supports_processing: true,
        };

        let parameters: Vec<ParameterInfo> = params
            .iter()
            .map(|p| ParameterInfo {
                id: p.id,
                title: p.name.clone(),
                short_title: String::new(),
                units: p.unit.clone(),
                step_count: p.step_count,
                default_normalized_value: p.default,
                unit_id: 0,
                flags: p.flags as i32,
                current_value: p.value,
            })
            .collect();

        PluginInfo {
            summary: detail.info.clone(),
            factory_info: FactoryInfo {
                vendor: detail.factory.vendor.clone(),
                url: detail.factory.url.clone(),
                email: detail.factory.email.clone(),
                flags: detail.factory.flags,
            },
            classes: detail
                .classes
                .iter()
                .map(|c| ClassInfo {
                    name: c.name.clone(),
                    category: c.category.clone(),
                    class_id: c.class_id.clone(),
                    cardinality: c.cardinality,
                    version: c.version.clone(),
                })
                .collect(),
            component_info: Some(component_info),
            controller_info: Some(ControllerInfo {
                parameter_count: parameters.len() as i32,
                parameters,
            }),
            has_gui: detail.info.has_gui,
            gui_size: None,
        }
    }

    fn stop_processing(&mut self) {
        if let Some(audio) = &self.audio {
            if let Err(e) = audio.lock().stop_processing() {
                println!("stop_processing failed: {e}");
            }
        }
        self.is_processing = false;
    }

    fn start_processing(&mut self) -> Result<(), String> {
        let audio = match &self.audio {
            Some(a) => a,
            None => return Err("No plugin loaded".to_string()),
        };
        audio
            .lock()
            .start_processing()
            .map_err(|e| format!("Failed to start processing: {e}"))?;
        self.is_processing = true;
        Ok(())
    }

    fn current_midi_channel(&self) -> MidiChannel {
        MidiChannel::from_index(self.selected_midi_channel as u8).unwrap_or(MidiChannel::Ch1)
    }

    /// Send a MIDI Note On event to the plugin (velocity 0.0..=1.0).
    fn send_midi_note_on(&mut self, channel: i16, pitch: i16, velocity: f32) -> Result<(), String> {
        // Log to the MIDI monitor (events the app sends).
        self.log_midi_event(
            MidiDirection::Input,
            0, // Note On
            channel as u8,
            pitch as u8,
            (velocity * 127.0) as u8,
        );

        let ch = self.current_midi_channel();
        let audio = match &self.audio {
            Some(a) => a,
            None => return Err("No plugin loaded".to_string()),
        };
        audio
            .lock()
            .send_midi_note(pitch as u8, (velocity * 127.0) as u8, ch)
            .map_err(|e| format!("Failed to send note on: {e}"))
    }

    /// Send a MIDI Note Off event.
    fn send_midi_note_off(
        &mut self,
        channel: i16,
        pitch: i16,
        velocity: f32,
    ) -> Result<(), String> {
        self.log_midi_event(
            MidiDirection::Input,
            1, // Note Off
            channel as u8,
            pitch as u8,
            (velocity * 127.0) as u8,
        );

        let ch = self.current_midi_channel();
        let audio = match &self.audio {
            Some(a) => a,
            None => return Err("No plugin loaded".to_string()),
        };
        audio
            .lock()
            .send_midi_note_off(pitch as u8, ch)
            .map_err(|e| format!("Failed to send note off: {e}"))
    }

    #[allow(dead_code)]
    fn send_midi_cc(&mut self, channel: i16, controller: u8, value: u8) -> Result<(), String> {
        self.log_midi_event(
            MidiDirection::Input,
            3, // Control Change
            channel as u8,
            controller,
            value,
        );

        let ch =
            MidiChannel::from_index(channel as u8).unwrap_or_else(|| self.current_midi_channel());
        let audio = match &self.audio {
            Some(a) => a,
            None => return Err("No plugin loaded".to_string()),
        };
        audio
            .lock()
            .send_midi_cc(controller, value, ch)
            .map_err(|e| format!("Failed to send CC: {e}"))
    }

    // MIDI Panic — uses the library's dedicated all-notes-off / all-sounds-off.
    fn send_midi_panic(&mut self) {
        println!("Sending MIDI Panic...");
        if let Some(audio) = &self.audio {
            if let Err(e) = audio.lock().midi_panic() {
                println!("MIDI panic failed: {e}");
            } else {
                println!("MIDI Panic sent");
            }
        } else {
            println!("Cannot send MIDI Panic: no plugin loaded");
        }
    }

    // Audio Panic — stop processing and clear the VU meters.
    fn audio_panic(&mut self) {
        println!("Audio Panic - stopping processing");
        if let Some(audio) = &self.audio {
            let mut p = audio.lock();
            let _ = p.midi_panic();
            let _ = p.stop_processing();
        }
        self.is_processing = false;

        if let Ok(mut level) = self.peak_level_left.lock() {
            *level = 0.0;
        }
        if let Ok(mut level) = self.peak_level_right.lock() {
            *level = 0.0;
        }
        let now = Instant::now();
        if let Ok(mut hold) = self.peak_hold_left.lock() {
            *hold = (0.0, now);
        }
        if let Ok(mut hold) = self.peak_hold_right.lock() {
            *hold = (0.0, now);
        }
        println!("Audio panic complete");
    }

    fn should_show_event(&self, event: &MidiEvent) -> bool {
        match &event.event_type {
            MidiEventType::NoteOn { .. } | MidiEventType::NoteOff { .. } => {
                self.midi_event_filter.show_note_events
            }
            MidiEventType::ControlChange { .. } => self.midi_event_filter.show_cc_events,
            MidiEventType::ProgramChange { .. } => self.midi_event_filter.show_program_change,
            MidiEventType::PitchBend { .. } => self.midi_event_filter.show_pitch_bend,
            MidiEventType::Aftertouch | MidiEventType::ChannelPressure => {
                self.midi_event_filter.show_aftertouch
            }
            MidiEventType::SystemExclusive | MidiEventType::Reset => {
                self.midi_event_filter.show_system_events
            }
            MidiEventType::Clock
            | MidiEventType::Start
            | MidiEventType::Continue
            | MidiEventType::Stop => self.midi_event_filter.show_clock_events,
            MidiEventType::ActiveSensing => self.midi_event_filter.show_active_sensing,
            MidiEventType::Other { .. } => true,
        }
    }

    fn event_type_name(&self, event_type: &MidiEventType) -> &'static str {
        match event_type {
            MidiEventType::NoteOn { .. } => "Note On",
            MidiEventType::NoteOff { .. } => "Note Off",
            MidiEventType::ControlChange { .. } => "CC",
            MidiEventType::ProgramChange { .. } => "Prog Change",
            MidiEventType::PitchBend { .. } => "Pitch Bend",
            MidiEventType::Aftertouch => "Aftertouch",
            MidiEventType::ChannelPressure => "Ch Pressure",
            MidiEventType::SystemExclusive => "SysEx",
            MidiEventType::Clock => "Clock",
            MidiEventType::Start => "Start",
            MidiEventType::Continue => "Continue",
            MidiEventType::Stop => "Stop",
            MidiEventType::ActiveSensing => "Active Sense",
            MidiEventType::Reset => "Reset",
            MidiEventType::Other { .. } => "Other",
        }
    }

    fn format_event_description(&self, event: &MidiEvent) -> String {
        match &event.event_type {
            MidiEventType::NoteOn {
                pitch, velocity, ..
            } => {
                let note_name = self.note_number_to_name(*pitch as u8);
                format!("{} velocity {}", note_name, (*velocity * 127.0) as u8)
            }
            MidiEventType::NoteOff {
                pitch, velocity, ..
            } => {
                let note_name = self.note_number_to_name(*pitch as u8);
                format!("{} velocity {}", note_name, (*velocity * 127.0) as u8)
            }
            MidiEventType::ControlChange {
                controller, value, ..
            } => {
                format!("CC {} = {}", controller, value)
            }
            MidiEventType::ProgramChange { program, .. } => {
                format!("Program {}", program)
            }
            MidiEventType::PitchBend { value, .. } => {
                format!("Value: {} ({})", value, value - 8192)
            }
            MidiEventType::Aftertouch => {
                format!("Key {} pressure {}", event.data1, event.data2)
            }
            MidiEventType::ChannelPressure => {
                format!("Pressure {}", event.data1)
            }
            _ => String::new(),
        }
    }

    fn note_number_to_name(&self, note: u8) -> String {
        midi_note_to_name(note)
    }

    fn log_midi_event(
        &self,
        direction: MidiDirection,
        event_type: u16,
        channel: u8,
        data1: u8,
        data2: u8,
    ) {
        if let Ok(is_paused) = self.midi_monitor_paused.lock() {
            if *is_paused {
                return;
            }
        }

        let midi_type = match event_type as u32 {
            0 => match data2 {
                0 => MidiEventType::NoteOff {
                    pitch: data1 as i16,
                    velocity: 0.0,
                    channel: channel as i16,
                },
                _ => MidiEventType::NoteOn {
                    pitch: data1 as i16,
                    velocity: data2 as f32 / 127.0,
                    channel: channel as i16,
                },
            },
            1 => MidiEventType::NoteOff {
                pitch: data1 as i16,
                velocity: data2 as f32 / 127.0,
                channel: channel as i16,
            },
            2 => MidiEventType::Aftertouch,
            3 => MidiEventType::ControlChange {
                controller: data1,
                value: data2,
                channel: channel as i16,
            },
            4 => MidiEventType::ProgramChange {
                program: data1,
                channel: channel as i16,
            },
            5 => MidiEventType::ChannelPressure,
            6 => MidiEventType::PitchBend {
                value: ((data2 as i16) << 7) | (data1 as i16),
                channel: channel as i16,
            },
            _ => MidiEventType::Other {
                status: event_type as u8,
                data1,
                data2,
            },
        };

        let event = MidiEvent {
            timestamp: Instant::now(),
            direction,
            event_type: midi_type,
            channel,
            data1,
            data2,
        };

        if let Ok(mut events) = self.midi_events.lock() {
            // Keep buffer size under control
            if events.len() >= self.max_midi_events {
                events.remove(0);
            }
            events.push(event);
        }
    }

    fn draw_piano_keyboard(&mut self, ui: &mut egui::Ui) {
        let white_key_width = 24.0;
        let white_key_height = 120.0;
        let black_key_width = 16.0;
        let black_key_height = 80.0;

        // Define notes for 6 octaves (C0 to C6)
        let octave_start = 0;
        let octave_count = 6;

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
            let _white_key_offsets = [0, 2, 4, 5, 7, 9, 11]; // C, D, E, F, G, A, B
            let note_names = ["C", "D", "E", "F", "G", "A", "B"];

            // Generate the note name (e.g., "C3")
            let note_name = format!("{}{}", note_names[key_in_octave as usize], octave);

            // Convert to MIDI note using our helper
            note_name_to_midi(&note_name).unwrap_or(0) as i16
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

                let note = note_for_white_key(octave_start + octave, key);
                let is_pressed = self.pressed_keys.contains(&note);

                // Check if mouse is over this key
                let mut is_hover = false;
                if let Some(pos) = mouse_pos {
                    if key_rect.contains(pos) && key_under_mouse.is_none() {
                        key_under_mouse = Some(note);
                        is_hover = true;
                    }
                }

                // Draw the key
                let color = if is_pressed {
                    egui::Color32::GRAY
                } else if is_hover {
                    egui::Color32::from_gray(240)
                } else {
                    egui::Color32::WHITE
                };

                painter.rect_filled(key_rect, egui::Rounding::ZERO, color);
                painter.rect_stroke(
                    key_rect,
                    egui::Rounding::ZERO,
                    egui::Stroke::new(1.0, egui::Color32::BLACK),
                    egui::epaint::StrokeKind::Middle,
                );

                // Draw note label
                let note_names = ["C", "D", "E", "F", "G", "A", "B"];
                let label = format!("{}{}", note_names[key as usize], octave_start + octave);
                painter.text(
                    egui::pos2(x + white_key_width / 2.0, rect.bottom() - 20.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::default(),
                    egui::Color32::BLACK,
                );

                // Draw MIDI number
                let midi_num = format!("{}", note);
                painter.text(
                    egui::pos2(x + white_key_width / 2.0, rect.bottom() - 8.0),
                    egui::Align2::CENTER_CENTER,
                    midi_num,
                    egui::FontId::new(10.0, egui::FontFamily::Proportional),
                    egui::Color32::from_gray(100),
                );
            }
        }

        // Draw black keys
        for octave in 0..octave_count {
            // Black keys positions within an octave (after C, D, F, G, A)
            let black_key_positions = [(0, 1), (1, 3), (3, 6), (4, 8), (5, 10)]; // (white_key_index, semitone_offset)

            for (i, (white_idx, _semitone)) in black_key_positions.iter().enumerate() {
                let x = rect.left()
                    + (octave * keys_per_octave + white_idx) as f32 * white_key_width
                    + white_key_width
                    - black_key_width / 2.0;

                let key_rect = egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(black_key_width, black_key_height),
                );

                // Use our helper to convert the note name to MIDI
                let black_note_names = ["C#", "D#", "F#", "G#", "A#"];
                let note_name = format!("{}{}", black_note_names[i], octave_start + octave);
                let note = note_name_to_midi(&note_name).unwrap_or(0) as i16;
                let is_pressed = self.pressed_keys.contains(&note);

                // Check if mouse is over this key (black keys take priority)
                let mut is_hover = false;
                if let Some(pos) = mouse_pos {
                    if key_rect.contains(pos) {
                        key_under_mouse = Some(note);
                        is_hover = true;
                    }
                }

                // Draw the key
                let color = if is_pressed {
                    egui::Color32::from_gray(60)
                } else if is_hover {
                    egui::Color32::from_gray(40)
                } else {
                    egui::Color32::BLACK
                };

                painter.rect_filled(key_rect, egui::Rounding::ZERO, color);
                painter.rect_stroke(
                    key_rect,
                    egui::Rounding::ZERO,
                    egui::Stroke::new(1.0, egui::Color32::DARK_GRAY),
                    egui::epaint::StrokeKind::Middle,
                );

                // Draw MIDI number on black key
                let text_color = if is_pressed {
                    egui::Color32::from_gray(200)
                } else {
                    egui::Color32::from_gray(150)
                };
                let midi_num = format!("{}", note);
                painter.text(
                    egui::pos2(x + black_key_width / 2.0, key_rect.bottom() - 8.0),
                    egui::Align2::CENTER_CENTER,
                    midi_num,
                    egui::FontId::new(9.0, egui::FontFamily::Proportional),
                    text_color,
                );
            }
        }

        // Handle mouse interactions
        if let Some(note) = key_under_mouse {
            if response.drag_started()
                || (response.is_pointer_button_down_on() && !self.pressed_keys.contains(&note))
            {
                // Mouse down - send note on
                if !self.pressed_keys.contains(&note) {
                    self.pressed_keys.insert(note);
                    if let Err(e) = self.send_midi_note_on(self.selected_midi_channel, note, 0.8) {
                        println!("Failed to send note on: {}", e);
                    }
                }
            }
        }

        // Check for released keys
        if response.drag_stopped() || !response.is_pointer_button_down_on() {
            // Mouse up - send note off for all pressed keys
            for &note in self.pressed_keys.clone().iter() {
                if let Err(e) = self.send_midi_note_off(self.selected_midi_channel, note, 0.0) {
                    println!("Failed to send note off: {}", e);
                }
            }
            self.pressed_keys.clear();
        }
    }
}

impl VST3Inspector {
    fn from_path(path: &str) -> Self {
        let sample_rate = 48000.0;
        let block_size = 512;

        // Build the library host once. If this fails we still construct a usable (but
        // plugin-less) inspector so the GUI can launch and surface the error.
        let host = Vst3Host::builder()
            .sample_rate(sample_rate)
            .block_size(block_size as usize)
            // Contain crash-prone plugins (e.g. Waves/WaveShell) in an isolated process so
            // selecting one can't take down the inspector.
            .auto_isolate_problematic(true)
            .build()
            .unwrap_or_else(|e| {
                eprintln!("Failed to build Vst3Host: {e}");
                // Fall back to a default host; if that also fails, panic is acceptable
                // since the app cannot function without it.
                Vst3Host::new().expect("failed to build a default Vst3Host")
            });

        Self {
            plugin_path: path.to_string(),
            plugin_info: None,
            report_json: None,
            plugin_window: None,
            discovered_plugins: Vec::new(),
            host,
            audio: None,
            last_error: None,
            gui_attached: false,
            selected_parameter: None,
            parameter_search: String::new(),
            parameter_filter: ParameterFilter::All,
            show_only_modified: false,
            table_scroll_to_selected: false,
            current_page: 0,
            items_per_page: 50,
            current_tab: Tab::Plugins,
            parameter_being_edited: None,
            is_processing: false,
            block_size,
            sample_rate,
            pressed_keys: HashSet::new(),
            selected_midi_channel: 0, // Default to channel 1 (0-based)
            midi_events: Arc::new(Mutex::new(Vec::new())),
            midi_event_filter: MidiEventFilter::default(),
            midi_monitor_paused: Arc::new(Mutex::new(false)),
            max_midi_events: 1000,
            preferences: Preferences::load(),
            peak_level_left: Arc::new(Mutex::new(0.0)),
            peak_level_right: Arc::new(Mutex::new(0.0)),
            peak_hold_left: Arc::new(Mutex::new((0.0, Instant::now()))),
            peak_hold_right: Arc::new(Mutex::new((0.0, Instant::now()))),
        }
    }
}

// Plugin discovery is now handled by plugin_discovery module

fn get_plugin_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}
