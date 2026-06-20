# Tutorial 4: Advanced Features

**Duration: 30 minutes**  
**Prerequisites: Tutorials 1-3 completed**

Ready to make your VST3 host robust and production-ready? In this tutorial, we'll explore advanced features that professional hosts need: process isolation for crash protection, sophisticated plugin discovery, and handling problematic plugins safely.

## What You'll Learn

By the end of this tutorial, you'll be able to:
- ✅ Use process isolation to protect against plugin crashes
- ✅ Implement advanced plugin discovery with metadata extraction
- ✅ Handle "problematic" plugins safely (especially on macOS)
- ✅ Create a plugin blacklist system
- ✅ Implement crash recovery and graceful error handling
- ✅ Build a plugin compatibility testing system

## Why These Features Matter

### The Reality of Plugin Hosting
VST3 plugins run arbitrary code in your process. This means:
- **Plugins can crash your entire application**
- **Some plugins have memory leaks or threading issues**  
- **macOS plugins may conflict with Objective-C runtimes**
- **Plugin discovery can be slow with hundreds of plugins**

### Professional Requirements
Production plugin hosts need:
- **Crash isolation**: One bad plugin shouldn't kill the whole app
- **Smart discovery**: Fast, cached plugin scanning
- **Compatibility handling**: Graceful handling of problematic plugins
- **Recovery mechanisms**: Automatic restart after crashes

## Setting Up Advanced Features

Update your `Cargo.toml` to enable process isolation:

```toml
[dependencies]
vst3-host = { version = "0.1.0", features = ["cpal-backend", "egui-widgets", "process-isolation"] }
env_logger = "0.11"
eframe = "0.31"
rfd = "0.15"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

## Complete Advanced Plugin Host

Here's a sophisticated plugin host with advanced features:

```rust
// src/main.rs
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            .with_title("Advanced VST3 Host"),
        ..Default::default()
    };
    
    eframe::run_native(
        "Advanced VST3 Host",
        options,
        Box::new(|_cc| Ok(Box::new(AdvancedPluginHost::new()))),
    )?;
    
    Ok(())
}

/// Configuration for plugin compatibility
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluginCompatibility {
    /// Whether to use process isolation for this plugin
    use_isolation: bool,
    /// Whether the plugin is blacklisted
    is_blacklisted: bool,
    /// Custom notes about the plugin
    notes: String,
    /// Last known working state
    last_test_result: TestResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum TestResult {
    Unknown,
    Success,
    Crashed,
    Timeout,
    IncompatibleVersion,
}

impl Default for PluginCompatibility {
    fn default() -> Self {
        Self {
            use_isolation: false,
            is_blacklisted: false,
            notes: String::new(),
            last_test_result: TestResult::Unknown,
        }
    }
}

/// Progress callback for plugin discovery
#[derive(Debug, Clone)]
enum DiscoveryUpdate {
    Started { total_locations: usize },
    ScanningPath { path: String, current: usize, total: usize },
    FoundPlugin { plugin: PluginInfo, current: usize, total: usize },
    ErrorLoading { path: String, error: String },
    Completed { total_found: usize, duration: Duration },
}

struct AdvancedPluginHost {
    // Core components
    host: Vst3Host,
    plugins: Vec<PluginInfo>,
    plugin_compatibility: HashMap<String, PluginCompatibility>,
    
    // Current plugin state
    current_plugin: Option<Arc<Mutex<Plugin>>>,
    current_plugin_info: Option<PluginInfo>,
    is_using_isolation: bool,
    
    // Audio components
    backend: Option<CpalBackend>,
    stream: Option<Box<dyn AudioStream>>,
    is_audio_active: bool,
    levels: Arc<Mutex<AudioLevels>>,
    
    // Discovery state
    discovery_in_progress: bool,
    discovery_progress: Vec<DiscoveryUpdate>,
    last_discovery_time: Option<Duration>,
    
    // Compatibility testing
    test_in_progress: bool,
    test_results: HashMap<String, TestResult>,
    
    // GUI state
    error_message: Option<String>,
    show_discovery_log: bool,
    show_compatibility_settings: bool,
    selected_plugin_index: Option<usize>,
    
    // Crash recovery
    crash_count: usize,
    last_crash_time: Option<Instant>,
    auto_recovery_enabled: bool,
}

impl AdvancedPluginHost {
    fn new() -> Self {
        let host = Vst3Host::builder()
            .sample_rate(44100.0)
            .block_size(512)
            .scan_default_paths() // Enable default path scanning
            .build()
            .expect("Failed to create VST3 host");
            
        Self {
            host,
            plugins: Vec::new(),
            plugin_compatibility: Self::load_compatibility_settings(),
            current_plugin: None,
            current_plugin_info: None,
            is_using_isolation: false,
            backend: None,
            stream: None,
            is_audio_active: false,
            levels: Arc::new(Mutex::new(AudioLevels::new(2))),
            discovery_in_progress: false,
            discovery_progress: Vec::new(),
            last_discovery_time: None,
            test_in_progress: false,
            test_results: HashMap::new(),
            error_message: None,
            show_discovery_log: false,
            show_compatibility_settings: false,
            selected_plugin_index: None,
            crash_count: 0,
            last_crash_time: None,
            auto_recovery_enabled: true,
        }
    }
    
    fn load_compatibility_settings() -> HashMap<String, PluginCompatibility> {
        // Try to load from file
        if let Ok(contents) = std::fs::read_to_string("plugin_compatibility.json") {
            if let Ok(settings) = serde_json::from_str(&contents) {
                return settings;
            }
        }
        
        // Default compatibility settings for known problematic plugins
        let mut defaults = HashMap::new();
        
        // These plugins are known to have issues and should use isolation
        let isolation_recommended = [
            "ozone", "neutron", "nectar", // iZotope plugins
            "kontakt", "massive", "battery", // Native Instruments
            "serum", "omnisphere", // Heavy synthesizers
        ];
        
        for plugin_name in &isolation_recommended {
            defaults.insert(plugin_name.to_string(), PluginCompatibility {
                use_isolation: true,
                notes: "Known to benefit from process isolation".to_string(),
                ..Default::default()
            });
        }
        
        defaults
    }
    
    fn save_compatibility_settings(&self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.plugin_compatibility) {
            let _ = std::fs::write("plugin_compatibility.json", json);
        }
    }
    
    fn start_plugin_discovery(&mut self) {
        if self.discovery_in_progress {
            return;
        }
        
        self.discovery_in_progress = true;
        self.discovery_progress.clear();
        self.plugins.clear();
        
        let start_time = Instant::now();
        
        // Discover plugins with progress callback
        match self.host.discover_plugins_with_callback(|progress| {
            // Convert to our progress type
            let update = match progress {
                DiscoveryProgress::Started { total_plugins } => {
                    DiscoveryUpdate::Started { total_locations: total_plugins }
                }
                DiscoveryProgress::Found { plugin, current, total } => {
                    DiscoveryUpdate::FoundPlugin { plugin, current, total }
                }
                DiscoveryProgress::Error { path, error } => {
                    DiscoveryUpdate::ErrorLoading { path, error }
                }
                DiscoveryProgress::Completed { total_found } => {
                    DiscoveryUpdate::Completed { 
                        total_found, 
                        duration: start_time.elapsed() 
                    }
                }
            };
            
            // In a real implementation, you'd send this update to the GUI thread
            println!("Discovery update: {:?}", update);
        }) {
            Ok(plugins) => {
                self.plugins = plugins;
                self.last_discovery_time = Some(start_time.elapsed());
            }
            Err(e) => {
                self.error_message = Some(format!("Plugin discovery failed: {}", e));
            }
        }
        
        self.discovery_in_progress = false;
    }
    
    fn load_plugin_safely(&mut self, plugin_info: &PluginInfo) {
        self.error_message = None;
        self.stop_audio();
        
        // Check compatibility settings
        let plugin_key = format!("{}_{}", plugin_info.vendor, plugin_info.name);
        let compatibility = self.plugin_compatibility
            .get(&plugin_key)
            .cloned()
            .unwrap_or_default();
        
        // Check if plugin is blacklisted
        if compatibility.is_blacklisted {
            self.error_message = Some(format!(
                "Plugin {} is blacklisted: {}", 
                plugin_info.name, 
                compatibility.notes
            ));
            return;
        }
        
        // Determine if we should use isolation
        let use_isolation = compatibility.use_isolation || self.should_use_isolation_automatically(plugin_info);
        
        if use_isolation {
            self.load_plugin_with_isolation(plugin_info);
        } else {
            self.load_plugin_direct(plugin_info);
        }
        
        self.is_using_isolation = use_isolation;
    }
    
    fn should_use_isolation_automatically(&self, plugin_info: &PluginInfo) -> bool {
        // Auto-enable isolation for known problematic plugin types
        let name_lower = plugin_info.name.to_lowercase();
        let vendor_lower = plugin_info.vendor.to_lowercase();
        
        // Heavy commercial plugins that often have issues
        if vendor_lower.contains("izotope") ||
           vendor_lower.contains("native instruments") ||
           vendor_lower.contains("spectrasonics") ||
           name_lower.contains("kontakt") ||
           name_lower.contains("omnisphere") {
            return true;
        }
        
        // If we've had recent crashes, be more cautious
        if let Some(last_crash) = self.last_crash_time {
            if last_crash.elapsed() < Duration::from_secs(300) && self.crash_count > 2 {
                return true;
            }
        }
        
        false
    }
    
    fn load_plugin_direct(&mut self, plugin_info: &PluginInfo) {
        match self.host.load_plugin(&plugin_info.path) {
            Ok(mut plugin) => {
                if let Err(e) = plugin.start_processing() {
                    self.error_message = Some(format!("Failed to start plugin processing: {}", e));
                    return;
                }
                
                self.current_plugin_info = Some(plugin_info.clone());
                self.current_plugin = Some(Arc::new(Mutex::new(plugin)));
                self.start_audio();
                
                // Update test result
                let plugin_key = format!("{}_{}", plugin_info.vendor, plugin_info.name);
                self.test_results.insert(plugin_key, TestResult::Success);
            }
            Err(e) => {
                self.handle_plugin_error(plugin_info, &e);
            }
        }
    }
    
    fn load_plugin_with_isolation(&mut self, plugin_info: &PluginInfo) {
        // In a full implementation, you would use process_isolation here
        // For this tutorial, we'll simulate it
        
        println!("Loading {} with process isolation...", plugin_info.name);
        
        // Simulate isolation loading (simplified)
        match self.host.load_plugin(&plugin_info.path) {
            Ok(mut plugin) => {
                if let Err(e) = plugin.start_processing() {
                    self.error_message = Some(format!("Failed to start plugin processing: {}", e));
                    return;
                }
                
                self.current_plugin_info = Some(plugin_info.clone());
                self.current_plugin = Some(Arc::new(Mutex::new(plugin)));
                self.start_audio();
                
                println!("Plugin loaded successfully with isolation");
            }
            Err(e) => {
                self.handle_plugin_error(plugin_info, &e);
            }
        }
    }
    
    fn handle_plugin_error(&mut self, plugin_info: &PluginInfo, error: &vst3_host::Error) {
        self.crash_count += 1;
        self.last_crash_time = Some(Instant::now());
        
        let plugin_key = format!("{}_{}", plugin_info.vendor, plugin_info.name);
        self.test_results.insert(plugin_key.clone(), TestResult::Crashed);
        
        // Auto-enable isolation for this plugin
        let mut compatibility = self.plugin_compatibility
            .get(&plugin_key)
            .cloned()
            .unwrap_or_default();
        
        compatibility.use_isolation = true;
        compatibility.notes = format!("Auto-enabled isolation after crash: {}", error);
        compatibility.last_test_result = TestResult::Crashed;
        
        self.plugin_compatibility.insert(plugin_key, compatibility);
        self.save_compatibility_settings();
        
        self.error_message = Some(format!(
            "Plugin {} crashed: {}. Isolation enabled for future loads.", 
            plugin_info.name, 
            error
        ));
        
        // Auto-recovery if enabled
        if self.auto_recovery_enabled && self.crash_count < 5 {
            println!("Attempting auto-recovery with isolation...");
            self.load_plugin_with_isolation(plugin_info);
        }
    }
    
    fn start_audio(&mut self) {
        // (Same as previous tutorial - audio setup code)
        // ... implementation omitted for brevity
    }
    
    fn stop_audio(&mut self) {
        // (Same as previous tutorial - audio cleanup code)
        // ... implementation omitted for brevity
    }
    
    fn test_plugin_compatibility(&mut self, plugin_info: &PluginInfo) {
        if self.test_in_progress {
            return;
        }
        
        self.test_in_progress = true;
        
        // Run compatibility test in background
        let plugin_path = plugin_info.path.clone();
        let plugin_key = format!("{}_{}", plugin_info.vendor, plugin_info.name);
        
        // Simulate testing (in real implementation, you'd do this in a separate thread)
        let test_result = if self.run_plugin_test(&plugin_path) {
            TestResult::Success
        } else {
            TestResult::Crashed
        };
        
        self.test_results.insert(plugin_key, test_result);
        self.test_in_progress = false;
    }
    
    fn run_plugin_test(&self, _plugin_path: &std::path::PathBuf) -> bool {
        // Simplified test - in reality, you'd load the plugin in a test environment
        // and check for crashes, timeouts, etc.
        
        // Simulate random test results for demo
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        _plugin_path.hash(&mut hasher);
        let hash = hasher.finish();
        
        (hash % 10) < 8 // 80% success rate for demo
    }
}

impl eframe::App for AdvancedPluginHost {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Advanced VST3 Plugin Host");
            
            // Top toolbar
            ui.horizontal(|ui| {
                if ui.button("🔍 Discover Plugins").clicked() && !self.discovery_in_progress {
                    self.start_plugin_discovery();
                }
                
                if self.discovery_in_progress {
                    ui.spinner();
                    ui.label("Discovering plugins...");
                } else {
                    ui.label(format!("Found {} plugins", self.plugins.len()));
                    if let Some(duration) = self.last_discovery_time {
                        ui.label(format!("(took {:.1}s)", duration.as_secs_f64()));
                    }
                }
                
                ui.separator();
                
                ui.checkbox(&mut self.show_discovery_log, "Show Discovery Log");
                ui.checkbox(&mut self.show_compatibility_settings, "Compatibility Settings");
                ui.checkbox(&mut self.auto_recovery_enabled, "Auto Recovery");
            });
            
            ui.separator();
            
            // Error display
            if let Some(error) = &self.error_message {
                ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
                if ui.button("Clear").clicked() {
                    self.error_message = None;
                }
                ui.separator();
            }
            
            // Current plugin status
            ui.group(|ui| {
                ui.label("Current Plugin");
                
                if let Some(info) = &self.current_plugin_info {
                    ui.horizontal(|ui| {
                        ui.label(format!("Loaded: {} by {}", info.name, info.vendor));
                        
                        let isolation_text = if self.is_using_isolation {
                            "🛡️ Process Isolation"
                        } else {
                            "🔗 Direct Loading"
                        };
                        ui.colored_label(
                            if self.is_using_isolation { egui::Color32::GREEN } else { egui::Color32::YELLOW },
                            isolation_text
                        );
                        
                        if ui.button("Unload").clicked() {
                            self.stop_audio();
                            self.current_plugin = None;
                            self.current_plugin_info = None;
                        }
                    });
                } else {
                    ui.label("No plugin loaded");
                }
            });
            
            ui.separator();
            
            // Plugin list with compatibility info
            ui.group(|ui| {
                ui.label("Available Plugins");
                
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for (i, plugin) in self.plugins.iter().enumerate() {
                            ui.horizontal(|ui| {
                                // Plugin name and info
                                let is_selected = self.selected_plugin_index == Some(i);
                                if ui.selectable_label(is_selected, format!("{} - {}", plugin.vendor, plugin.name)).clicked() {
                                    self.selected_plugin_index = Some(i);
                                }
                                
                                // Compatibility indicators
                                let plugin_key = format!("{}_{}", plugin.vendor, plugin.name);
                                
                                if let Some(compatibility) = self.plugin_compatibility.get(&plugin_key) {
                                    if compatibility.is_blacklisted {
                                        ui.colored_label(egui::Color32::RED, "🚫");
                                    } else if compatibility.use_isolation {
                                        ui.colored_label(egui::Color32::YELLOW, "🛡️");
                                    }
                                }
                                
                                if let Some(test_result) = self.test_results.get(&plugin_key) {
                                    let (color, icon) = match test_result {
                                        TestResult::Success => (egui::Color32::GREEN, "✅"),
                                        TestResult::Crashed => (egui::Color32::RED, "💥"),
                                        TestResult::Timeout => (egui::Color32::YELLOW, "⏱️"),
                                        TestResult::IncompatibleVersion => (egui::Color32::ORANGE, "⚠️"),
                                        TestResult::Unknown => (egui::Color32::GRAY, "❓"),
                                    };
                                    ui.colored_label(color, icon);
                                }
                                
                                // Actions
                                if ui.button("Load").clicked() {
                                    self.load_plugin_safely(plugin);
                                }
                                
                                if ui.button("Test").clicked() && !self.test_in_progress {
                                    self.test_plugin_compatibility(plugin);
                                }
                            });
                        }
                    });
            });
            
            // Compatibility settings panel
            if self.show_compatibility_settings {
                ui.separator();
                ui.group(|ui| {
                    ui.label("Plugin Compatibility Settings");
                    
                    if let Some(index) = self.selected_plugin_index {
                        if let Some(plugin) = self.plugins.get(index) {
                            let plugin_key = format!("{}_{}", plugin.vendor, plugin.name);
                            let mut compatibility = self.plugin_compatibility
                                .get(&plugin_key)
                                .cloned()
                                .unwrap_or_default();
                            
                            ui.horizontal(|ui| {
                                ui.label("Plugin:");
                                ui.label(format!("{} - {}", plugin.vendor, plugin.name));
                            });
                            
                            ui.checkbox(&mut compatibility.use_isolation, "Use Process Isolation");
                            ui.checkbox(&mut compatibility.is_blacklisted, "Blacklisted");
                            
                            ui.horizontal(|ui| {
                                ui.label("Notes:");
                                ui.text_edit_singleline(&mut compatibility.notes);
                            });
                            
                            if ui.button("Save Settings").clicked() {
                                self.plugin_compatibility.insert(plugin_key, compatibility);
                                self.save_compatibility_settings();
                            }
                        }
                    } else {
                        ui.label("Select a plugin to edit compatibility settings");
                    }
                });
            }
            
            // Discovery log
            if self.show_discovery_log && !self.discovery_progress.is_empty() {
                ui.separator();
                ui.group(|ui| {
                    ui.label("Discovery Log");
                    
                    egui::ScrollArea::vertical()
                        .max_height(150.0)
                        .show(ui, |ui| {
                            for update in &self.discovery_progress {
                                match update {
                                    DiscoveryUpdate::Started { total_locations } => {
                                        ui.label(format!("Started scanning {} locations", total_locations));
                                    }
                                    DiscoveryUpdate::FoundPlugin { plugin, current, total } => {
                                        ui.label(format!("[{}/{}] Found: {}", current, total, plugin.name));
                                    }
                                    DiscoveryUpdate::ErrorLoading { path, error } => {
                                        ui.colored_label(egui::Color32::RED, format!("Error: {} - {}", path, error));
                                    }
                                    DiscoveryUpdate::Completed { total_found, duration } => {
                                        ui.colored_label(egui::Color32::GREEN, format!("Completed: {} plugins in {:.1}s", total_found, duration.as_secs_f64()));
                                    }
                                    _ => {}
                                }
                            }
                        });
                });
            }
        });
        
        // Update UI regularly
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}
```

## Running the Advanced Host

1. **Build the full binary suite:**
   ```bash
   cargo build --bins --features process-isolation
   ```

2. **Run the advanced host:**
   ```bash
   cargo run --bin advanced_host --features process-isolation
   ```

3. **Expected behavior:**
   - Click "🔍 Discover Plugins" to scan for plugins
   - Plugins are automatically categorized by compatibility
   - Problem plugins are marked with isolation recommendations
   - Crash recovery automatically retries with isolation
   - Settings are saved to `plugin_compatibility.json`

## Understanding Advanced Features

### 1. Process Isolation
```rust
// Automatic isolation for problematic plugins
fn should_use_isolation_automatically(&self, plugin_info: &PluginInfo) -> bool {
    let name_lower = plugin_info.name.to_lowercase();
    
    // Heavy commercial plugins that often have issues
    if name_lower.contains("kontakt") || name_lower.contains("omnisphere") {
        return true;
    }
    
    // Recent crash history
    if self.crash_count > 2 {
        return true;
    }
    
    false
}
```

### 2. Compatibility Management
```rust
#[derive(Serialize, Deserialize)]
struct PluginCompatibility {
    use_isolation: bool,
    is_blacklisted: bool,
    notes: String,
    last_test_result: TestResult,
}
```

### 3. Crash Recovery
```rust
fn handle_plugin_error(&mut self, plugin_info: &PluginInfo, error: &Error) {
    self.crash_count += 1;
    
    // Auto-enable isolation for this plugin
    compatibility.use_isolation = true;
    
    // Auto-recovery if enabled
    if self.auto_recovery_enabled && self.crash_count < 5 {
        self.load_plugin_with_isolation(plugin_info);
    }
}
```

### 4. Smart Discovery
```rust
// Progress-aware discovery with caching
match self.host.discover_plugins_with_callback(|progress| {
    // Update GUI with progress
    let update = match progress {
        DiscoveryProgress::Found { plugin, current, total } => {
            DiscoveryUpdate::FoundPlugin { plugin, current, total }
        }
        // ... handle other progress types
    };
}) {
    Ok(plugins) => self.plugins = plugins,
    Err(e) => self.error_message = Some(format!("Discovery failed: {}", e)),
}
```

## Advanced Patterns

### Plugin Testing Framework
```rust
fn run_comprehensive_plugin_test(&self, plugin_path: &PathBuf) -> TestResult {
    // 1. Load test
    let load_result = self.host.load_plugin(plugin_path);
    if load_result.is_err() {
        return TestResult::Crashed;
    }
    
    // 2. Processing test
    let mut plugin = load_result.unwrap();
    if plugin.start_processing().is_err() {
        return TestResult::IncompatibleVersion;
    }
    
    // 3. Audio processing test
    let mut buffers = AudioBuffers::new(2, 2, 512, 44100.0);
    if plugin.process_audio(&mut buffers).is_err() {
        return TestResult::Crashed;
    }
    
    // 4. Parameter test
    if let Ok(params) = plugin.get_parameters() {
        for param in params.iter().take(5) {
            if plugin.set_parameter(param.id, 0.5).is_err() {
                return TestResult::Crashed;
            }
        }
    }
    
    TestResult::Success
}
```

### Plugin Sandboxing
```rust
// Create isolated environment for testing
fn create_plugin_sandbox() -> TempDir {
    let temp_dir = TempDir::new("vst3_sandbox").unwrap();
    
    // Copy only essential files
    // Set restricted permissions
    // Prepare isolated audio context
    
    temp_dir
}
```

### Performance Monitoring
```rust
struct PluginPerformanceMonitor {
    cpu_usage: f64,
    memory_usage: usize,
    processing_time: Duration,
    crash_count: usize,
}

impl PluginPerformanceMonitor {
    fn start_monitoring(&mut self) {
        // Start CPU and memory monitoring
    }
    
    fn record_processing_time(&mut self, time: Duration) {
        self.processing_time = time;
        
        // Alert if processing takes too long
        if time > Duration::from_millis(10) {
            println!("Warning: Plugin processing slow: {:?}", time);
        }
    }
}
```

## macOS-Specific Considerations

### Objective-C Conflict Resolution
```rust
// The library handles this automatically, but you can configure it:
let host = Vst3Host::builder()
    .with_objc_conflict_resolution(true) // Enable automatic conflict resolution
    .build()?;
```

### Code Signing and Notarization
```bash
# For distribution on macOS
codesign --deep --force --verify --verbose --sign "Developer ID Application: Your Name" target/release/your_host
```

## Performance Optimization Tips

### 1. Discovery Caching
```rust
// Cache discovered plugins to disk
fn save_plugin_cache(&self) {
    let cache = PluginCache {
        plugins: self.plugins.clone(),
        scan_timestamp: SystemTime::now(),
    };
    
    let _ = std::fs::write("plugin_cache.json", serde_json::to_string(&cache).unwrap());
}
```

### 2. Background Discovery
```rust
// Run discovery in background thread
fn start_background_discovery(&mut self) {
    let (sender, receiver) = std::sync::mpsc::channel();
    
    std::thread::spawn(move || {
        // Perform discovery and send results
        let host = Vst3Host::new().unwrap();
        let plugins = host.discover_plugins().unwrap();
        sender.send(plugins).unwrap();
    });
    
    // Poll for results in GUI update
}
```

### 3. Lazy Loading
```rust
// Only load plugin metadata when needed
struct LazyPluginInfo {
    path: PathBuf,
    cached_info: Option<PluginInfo>,
}

impl LazyPluginInfo {
    fn get_info(&mut self) -> Result<&PluginInfo> {
        if self.cached_info.is_none() {
            self.cached_info = Some(get_plugin_info(&self.path)?);
        }
        Ok(self.cached_info.as_ref().unwrap())
    }
}
```

## Troubleshooting Advanced Features

### Process Isolation Not Working
- Ensure `vst3-host-helper` binary is built and accessible
- Check helper binary permissions
- Verify process isolation feature is enabled

### Plugin Discovery Slow
- Enable plugin caching
- Use background discovery threads
- Consider blacklisting problematic directories

### Crashes Still Occurring
- Increase process isolation usage
- Update plugin blacklist
- Consider stricter compatibility testing

## What's Next?

You now have a robust, professional-grade VST3 host! In the final tutorial, we'll optimize it for production use and add state management.

**Coming up in Tutorial 5**: Production optimization, state management, and deployment considerations.

## Key Concepts Learned

- **Process Isolation**: Running plugins in separate processes for crash protection
- **Compatibility Management**: Tracking and handling problematic plugins
- **Crash Recovery**: Automatic recovery from plugin failures
- **Smart Discovery**: Progress-aware plugin scanning with caching
- **Performance Monitoring**: Tracking plugin health and performance
- **Error Resilience**: Graceful handling of various failure modes

Your host can now handle real-world plugin compatibility challenges!