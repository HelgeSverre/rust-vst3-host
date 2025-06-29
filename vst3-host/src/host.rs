//! VST3 host implementation

use crate::{
    audio::AudioConfig,
    error::{Error, Result},
    plugin::{Plugin, PluginInfo},
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// VST3 host instance
pub struct Vst3Host {
    /// Audio configuration
    pub(crate) config: AudioConfig,
    /// Custom plugin scan paths
    pub(crate) custom_paths: Vec<PathBuf>,
    /// Whether process isolation is enabled
    pub(crate) use_process_isolation: bool,
    /// Loaded plugins
    pub(crate) plugins: Vec<Plugin>,
}

impl Vst3Host {
    /// Create a new VST3 host with default settings
    pub fn new() -> Result<Self> {
        Self::builder().build()
    }
    
    /// Create a new VST3 host builder
    pub fn builder() -> Vst3HostBuilder {
        Vst3HostBuilder::default()
    }
    
    /// Add a custom path to scan for VST3 plugins
    pub fn add_scan_path<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(Error::Other(format!("Path does not exist: {}", path.display())));
        }
        self.custom_paths.push(path.to_path_buf());
        Ok(())
    }
    
    /// Discover all VST3 plugins on the system
    pub fn discover_plugins(&self) -> Result<Vec<PluginInfo>> {
        self.discover_plugins_with_callback(|_| {})
    }
    
    /// Discover VST3 plugins with progress callback
    pub fn discover_plugins_with_progress<F>(&self, mut callback: F) -> Result<Vec<PluginInfo>>
    where
        F: FnMut(DiscoveryProgress),
    {
        self.discover_plugins_with_callback(|info| {
            callback(DiscoveryProgress {
                percentage: 0, // TODO: Calculate actual percentage
                current_plugin: info.name.clone(),
                current_path: info.path.clone(),
            });
        })
    }
    
    /// Internal discovery implementation
    pub(crate) fn discover_plugins_with_callback<F>(&self, mut callback: F) -> Result<Vec<PluginInfo>>
    where
        F: FnMut(&PluginInfo),
    {
        // Get all paths to scan
        let mut all_paths = crate::discovery::scan_standard_paths();
        all_paths.extend(self.custom_paths.clone());
        
        // Find all VST3 files
        let plugin_paths = crate::discovery::scan_directories(&all_paths)?;
        
        // Get info for each plugin
        let mut plugins = Vec::new();
        for path in plugin_paths {
            match crate::discovery::get_plugin_info(&path) {
                Ok(info) => {
                    callback(&info);
                    plugins.push(info);
                }
                Err(e) => {
                    // Log error but continue scanning
                    eprintln!("Failed to get info for {}: {}", path.display(), e);
                }
            }
        }
        
        Ok(plugins)
    }
    
    /// Load a VST3 plugin
    pub fn load_plugin<P: AsRef<Path>>(&mut self, path: P) -> Result<Plugin> {
        let path = path.as_ref();
        
        if !path.exists() {
            return Err(Error::PluginNotFound(path.display().to_string()));
        }
        
        // This will be implemented to actually load the plugin
        // For now, create a dummy plugin
        let info = PluginInfo {
            path: path.to_path_buf(),
            name: "Test Plugin".to_string(),
            vendor: "Test Vendor".to_string(),
            version: "1.0.0".to_string(),
            category: "Effect".to_string(),
            uid: "test-uid".to_string(),
            audio_inputs: 2,
            audio_outputs: 2,
            has_midi_input: true,
            has_midi_output: false,
            has_gui: false,
        };
        
        let plugin = Plugin {
            info,
            is_processing: false,
            sample_rate: self.config.sample_rate,
            block_size: self.config.block_size,
            audio_levels: Arc::new(Mutex::new(crate::audio::AudioLevels::new(2))),
            parameter_change_callback: None,
            audio_callback: None,
            internal: None,
        };
        
        Ok(plugin)
    }
    
    /// Get audio configuration
    pub fn config(&self) -> &AudioConfig {
        &self.config
    }
}

impl Default for Vst3Host {
    fn default() -> Self {
        Self {
            config: AudioConfig::default(),
            custom_paths: Vec::new(),
            use_process_isolation: true,
            plugins: Vec::new(),
        }
    }
}

/// Builder for VST3 host configuration
pub struct Vst3HostBuilder {
    config: AudioConfig,
    use_process_isolation: bool,
    custom_paths: Vec<PathBuf>,
}

impl Default for Vst3HostBuilder {
    fn default() -> Self {
        Self {
            config: AudioConfig::default(),
            use_process_isolation: true,
            custom_paths: Vec::new(),
        }
    }
}

impl Vst3HostBuilder {
    /// Set the sample rate
    pub fn sample_rate(mut self, rate: f64) -> Self {
        self.config.sample_rate = rate;
        self
    }
    
    /// Set the block size
    pub fn block_size(mut self, size: usize) -> Self {
        self.config.block_size = size;
        self
    }
    
    /// Set the number of input channels
    pub fn input_channels(mut self, channels: usize) -> Self {
        self.config.input_channels = channels;
        self
    }
    
    /// Set the number of output channels
    pub fn output_channels(mut self, channels: usize) -> Self {
        self.config.output_channels = channels;
        self
    }
    
    /// Enable or disable process isolation
    pub fn with_process_isolation(mut self, enabled: bool) -> Self {
        self.use_process_isolation = enabled;
        self
    }
    
    /// Add a custom plugin scan path
    pub fn add_scan_path<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.custom_paths.push(path.as_ref().to_path_buf());
        self
    }
    
    /// Build the VST3 host
    pub fn build(self) -> Result<Vst3Host> {
        Ok(Vst3Host {
            config: self.config,
            custom_paths: self.custom_paths,
            use_process_isolation: self.use_process_isolation,
            plugins: Vec::new(),
        })
    }
}

/// Plugin discovery progress information
pub struct DiscoveryProgress {
    /// Progress percentage (0-100)
    pub percentage: u8,
    /// Currently scanning plugin
    pub current_plugin: String,
    /// Current path being scanned
    pub current_path: PathBuf,
}

#[cfg(feature = "cpal-backend")]
impl Vst3Host {
    /// Create a host with CPAL audio backend
    pub fn with_cpal_backend() -> Result<Self> {
        use crate::backends::CpalBackend;
        
        let backend = CpalBackend::new()?;
        Self::with_backend(backend)
    }
    
    /// Create a host with a custom audio backend
    pub fn with_backend<B: crate::backends::AudioBackend>(backend: B) -> Result<Self> {
        // This will be implemented when we create the AudioBackend trait
        Self::new()
    }
}