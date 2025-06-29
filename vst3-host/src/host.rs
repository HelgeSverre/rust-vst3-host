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
    /// Whether to use process isolation for plugin loading
    pub(crate) use_process_isolation: bool,
    /// Whether to scan default system paths for plugins
    pub(crate) scan_default_paths: bool,
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
            return Err(Error::Other(format!(
                "Path does not exist: {}",
                path.display()
            )));
        }
        self.custom_paths.push(path.to_path_buf());
        Ok(())
    }

    /// Discover all VST3 plugins on the system
    pub fn discover_plugins(&self) -> Result<Vec<PluginInfo>> {
        self.discover_plugins_with_callback(|_| {})
    }

    /// Discover VST3 plugins with progress callback
    pub fn discover_plugins_with_callback<F>(&self, mut callback: F) -> Result<Vec<PluginInfo>>
    where
        F: FnMut(DiscoveryProgress),
    {
        // Get all paths to scan
        let mut all_paths = Vec::new();

        // Only include standard paths if requested
        if self.scan_default_paths {
            all_paths.extend(crate::discovery::scan_standard_paths());
        }

        all_paths.extend(self.custom_paths.clone());

        // Find all VST3 files
        let plugin_paths = crate::discovery::scan_directories(&all_paths)?;
        let total = plugin_paths.len();

        // Notify start
        callback(DiscoveryProgress::Started {
            total_plugins: total,
        });

        // Get info for each plugin
        let mut plugins = Vec::new();
        for (index, path) in plugin_paths.iter().enumerate() {
            match crate::discovery::get_plugin_info(path) {
                Ok(info) => {
                    callback(DiscoveryProgress::Found {
                        plugin: info.clone(),
                        current: index + 1,
                        total,
                    });
                    plugins.push(info);
                }
                Err(e) => {
                    callback(DiscoveryProgress::Error {
                        path: path.display().to_string(),
                        error: e.to_string(),
                    });
                }
            }
        }

        // Notify completion
        callback(DiscoveryProgress::Completed {
            total_found: plugins.len(),
        });

        Ok(plugins)
    }

    /// Load a VST3 plugin
    pub fn load_plugin<P: AsRef<Path>>(&mut self, path: P) -> Result<Plugin> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(Error::PluginNotFound(path.display().to_string()));
        }

        // Get plugin info first
        let info = crate::discovery::get_plugin_info(path)?;

        // Use process isolation if enabled
        if self.use_process_isolation {
            self.load_plugin_isolated(path, info)
        } else {
            self.load_plugin_internal(path, info)
        }
    }

    /// Load a plugin in-process
    fn load_plugin_internal(&mut self, path: &Path, info: PluginInfo) -> Result<Plugin> {
        // Get the binary path
        let binary_path = crate::discovery::get_vst3_binary_path(path)?;

        // Load the plugin implementation
        let plugin_impl =
            crate::internal::plugin_impl::PluginImpl::load(&binary_path, info.clone())?;

        // Get the updated info from the plugin implementation (has_gui might have been updated)
        let updated_info = plugin_impl.info.clone();

        // Create the plugin wrapper
        let output_channels = if updated_info.audio_outputs > 0 {
            updated_info.audio_outputs as usize * 2 // Assume stereo buses
        } else {
            2
        };

        let plugin = Plugin {
            info: updated_info,
            is_processing: false,
            sample_rate: self.config.sample_rate,
            block_size: self.config.block_size,
            audio_levels: Arc::new(Mutex::new(crate::audio::AudioLevels::new(output_channels))),
            parameter_change_callback: None,
            audio_callback: None,
            internal: Some(Box::new(plugin_impl)),
        };

        // Note: We can't track plugins in a Vec since they're not cloneable
        // This would require a different design (e.g., using handles/IDs)

        Ok(plugin)
    }

    /// Load a plugin in an isolated process
    fn load_plugin_isolated(&mut self, path: &Path, info: PluginInfo) -> Result<Plugin> {
        use crate::process_isolation::{HostCommand, HostResponse, PluginHostProcess};

        // Create and start the isolated plugin process
        let mut process = PluginHostProcess::new()
            .map_err(|e| Error::Other(format!("Failed to create isolated process: {}", e)))?;

        // Load the plugin in the isolated process
        let response = process
            .send_command(HostCommand::LoadPlugin {
                path: path.display().to_string(),
            })
            .map_err(|e| Error::Other(format!("Failed to load plugin in isolation: {}", e)))?;

        // Verify the plugin loaded successfully
        let loaded_info = match response {
            HostResponse::PluginInfo {
                vendor,
                name,
                version,
                has_gui,
                audio_inputs,
                audio_outputs,
            } => {
                PluginInfo {
                    path: path.to_path_buf(),
                    name,
                    vendor,
                    version,
                    category: info.category.clone(), // Use the original category
                    uid: info.uid.clone(),           // Use the original UID
                    has_gui,
                    audio_inputs: audio_inputs as u32,
                    audio_outputs: audio_outputs as u32,
                    has_midi_input: info.has_midi_input, // Use the original MIDI info
                    has_midi_output: info.has_midi_output,
                }
            }
            HostResponse::Error { message } => {
                return Err(Error::Other(format!("Failed to load plugin: {}", message)));
            }
            _ => {
                return Err(Error::Other(
                    "Unexpected response from helper process".to_string(),
                ));
            }
        };

        // Create the isolated plugin implementation
        let plugin_impl = crate::internal::isolated_plugin_impl::IsolatedPluginImpl::new(
            process,
            loaded_info.clone(),
            self.config.sample_rate,
            self.config.block_size,
        );

        // Create the plugin wrapper
        let output_channels = if loaded_info.audio_outputs > 0 {
            loaded_info.audio_outputs as usize * 2 // Assume stereo buses
        } else {
            2
        };

        let plugin = Plugin {
            info: loaded_info,
            is_processing: false,
            sample_rate: self.config.sample_rate,
            block_size: self.config.block_size,
            audio_levels: Arc::new(Mutex::new(crate::audio::AudioLevels::new(output_channels))),
            parameter_change_callback: None,
            audio_callback: None,
            internal: Some(Box::new(plugin_impl)),
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
            use_process_isolation: false,
            scan_default_paths: true, // Default to true for backward compatibility
        }
    }
}

/// Builder for VST3 host configuration
pub struct Vst3HostBuilder {
    config: AudioConfig,
    custom_paths: Vec<PathBuf>,
    use_process_isolation: bool,
    scan_default_paths: bool,
}

impl Default for Vst3HostBuilder {
    fn default() -> Self {
        Self {
            config: AudioConfig::default(),
            custom_paths: Vec::new(),
            use_process_isolation: false,
            scan_default_paths: false, // Default to false - require explicit opt-in
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

    /// Enable or disable process isolation for plugin loading
    pub fn with_process_isolation(mut self, enabled: bool) -> Self {
        self.use_process_isolation = enabled;
        self
    }

    /// Add a custom plugin scan path
    pub fn add_scan_path<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.custom_paths.push(path.as_ref().to_path_buf());
        self
    }

    /// Enable scanning of default system VST3 paths
    pub fn scan_default_paths(mut self) -> Self {
        self.scan_default_paths = true;
        self
    }

    /// Build the VST3 host
    pub fn build(self) -> Result<Vst3Host> {
        Ok(Vst3Host {
            config: self.config,
            custom_paths: self.custom_paths,
            use_process_isolation: self.use_process_isolation,
            scan_default_paths: self.scan_default_paths,
        })
    }
}

/// Plugin discovery progress information
#[derive(Debug, Clone)]
pub enum DiscoveryProgress {
    /// Discovery has started
    Started {
        /// Total number of plugins to scan
        total_plugins: usize,
    },
    /// A plugin was found
    Found {
        /// The plugin information
        plugin: PluginInfo,
        /// Current plugin index
        current: usize,
        /// Total number of plugins
        total: usize,
    },
    /// An error occurred while scanning a plugin
    Error {
        /// Path that failed
        path: String,
        /// Error message
        error: String,
    },
    /// Discovery completed
    Completed {
        /// Total number of plugins found
        total_found: usize,
    },
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
    pub fn with_backend<B: crate::audio::AudioBackend>(_backend: B) -> Result<Self> {
        // This will be implemented when we create the AudioBackend trait
        Self::new()
    }
}
