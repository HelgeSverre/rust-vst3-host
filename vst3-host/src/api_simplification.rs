//! Proposed API simplification patterns
//! This is a design proposal - not meant to be compiled

/// Module-level convenience functions for common tasks
pub mod simple {
    use crate::{Plugin, AudioConfig, Error, Result};
    use std::path::Path;

    /// Load a plugin with default settings - simplest possible API
    /// 
    /// This handles all the complexity internally:
    /// - Creates a default host
    /// - Configures reasonable audio settings
    /// - Loads and initializes the plugin
    /// 
    /// # Example
    /// ```no_run
    /// let mut plugin = vst3_host::simple::load_plugin("/path/to/plugin.vst3")?;
    /// plugin.start_processing()?;
    /// plugin.send_midi_note(60, 100, vst3_host::MidiChannel::Ch1)?;
    /// # Ok::<(), vst3_host::Error>(())
    /// ```
    pub fn load_plugin<P: AsRef<Path>>(path: P) -> Result<Plugin> {
        load_plugin_with_config(path, AudioConfig::default())
    }

    /// Load a plugin with custom audio configuration
    pub fn load_plugin_with_config<P: AsRef<Path>>(path: P, config: AudioConfig) -> Result<Plugin> {
        let mut host = crate::Vst3Host::builder()
            .sample_rate(config.sample_rate)
            .block_size(config.block_size)
            .input_channels(config.input_channels)
            .output_channels(config.output_channels)
            .build()?;

        host.load_plugin(path)
    }

    /// Load a plugin with process isolation (safer but slower)
    pub fn load_plugin_isolated<P: AsRef<Path>>(path: P) -> Result<Plugin> {
        let mut host = crate::Vst3Host::builder()
            .with_process_isolation(true)
            .build()?;

        host.load_plugin(path)
    }

    /// Quick MIDI test - send a note and automatically turn it off after duration
    pub fn test_plugin_with_note(plugin: &mut Plugin, note: u8, velocity: u8, duration_ms: u64) -> Result<()> {
        use std::thread::sleep;
        use std::time::Duration;
        
        // Ensure plugin is processing
        if !plugin.is_processing() {
            plugin.start_processing()?;
        }

        // Send note on
        plugin.send_midi_note(note, velocity, crate::MidiChannel::Ch1)?;
        
        // Wait
        sleep(Duration::from_millis(duration_ms));
        
        // Send note off
        plugin.send_midi_note_off(note, crate::MidiChannel::Ch1)?;
        
        Ok(())
    }

    /// Discover plugins in standard system locations
    /// Returns paths that can be used with load_plugin()
    pub fn find_plugins() -> Result<Vec<std::path::PathBuf>> {
        let mut host = crate::Vst3Host::builder()
            .scan_default_paths()
            .build()?;
        
        let plugins = host.discover_plugins()?;
        Ok(plugins.into_iter().map(|p| p.path).collect())
    }
}

/// Fluent API for plugin configuration
pub struct PluginBuilder {
    path: std::path::PathBuf,
    config: AudioConfig,
    isolated: bool,
    auto_start: bool,
}

impl PluginBuilder {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            config: AudioConfig::default(),
            isolated: false,
            auto_start: true,
        }
    }

    /// Use process isolation for this plugin
    pub fn isolated(mut self) -> Self {
        self.isolated = true;
        self
    }

    /// Configure audio settings
    pub fn audio_config(mut self, config: AudioConfig) -> Self {
        self.config = config;
        self
    }

    /// Set sample rate
    pub fn sample_rate(mut self, rate: f64) -> Self {
        self.config.sample_rate = rate;
        self
    }

    /// Set buffer size
    pub fn buffer_size(mut self, size: usize) -> Self {
        self.config.block_size = size;
        self
    }

    /// Don't automatically start processing
    pub fn manual_start(mut self) -> Self {
        self.auto_start = false;
        self
    }

    /// Build and load the plugin
    pub fn build(self) -> Result<Plugin> {
        let mut host = crate::Vst3Host::builder()
            .sample_rate(self.config.sample_rate)
            .block_size(self.config.block_size)
            .input_channels(self.config.input_channels)
            .output_channels(self.config.output_channels)
            .with_process_isolation(self.isolated)
            .build()?;

        let mut plugin = host.load_plugin(&self.path)?;
        
        if self.auto_start {
            plugin.start_processing()?;
        }

        Ok(plugin)
    }
}

/// Usage examples of the improved API
#[cfg(any(test, doctest))]
mod examples {
    use super::*;

    /// Example 1: Ultra-simple usage
    fn simple_usage() -> Result<()> {
        // One line to load and start a plugin
        let mut plugin = simple::load_plugin("/path/to/plugin.vst3")?;
        
        // Test it with a note
        simple::test_plugin_with_note(&mut plugin, 60, 100, 1000)?;
        
        Ok(())
    }

    /// Example 2: Fluent configuration
    fn configured_usage() -> Result<()> {
        let mut plugin = PluginBuilder::new("/path/to/plugin.vst3")
            .sample_rate(48000.0)
            .buffer_size(512)
            .isolated() // Use process isolation
            .build()?;

        // Plugin is already started and ready to use
        plugin.send_midi_note(60, 100, crate::MidiChannel::Ch1)?;
        
        Ok(())
    }

    /// Example 3: Plugin discovery
    fn discovery_usage() -> Result<()> {
        let plugin_paths = simple::find_plugins()?;
        
        for path in plugin_paths.iter().take(5) {
            println!("Found plugin: {}", path.display());
        }

        // Load the first one
        if let Some(first_plugin) = plugin_paths.first() {
            let mut plugin = simple::load_plugin(first_plugin)?;
            // Use plugin...
        }

        Ok(())
    }
}

/// Trait for adding convenience methods to Plugin
pub trait PluginExt {
    /// Send a chord (multiple notes at once)
    fn send_chord(&mut self, notes: &[u8], velocity: u8, channel: crate::MidiChannel) -> Result<()>;
    
    /// Send all notes off (panic)
    fn panic(&mut self) -> Result<()>;
    
    /// Set multiple parameters at once
    fn set_parameters(&mut self, params: &[(u32, f64)]) -> Result<()>;
    
    /// Get a parameter by name (convenience method)
    fn get_parameter_by_name(&self, name: &str) -> Result<f64>;
}

impl PluginExt for Plugin {
    fn send_chord(&mut self, notes: &[u8], velocity: u8, channel: crate::MidiChannel) -> Result<()> {
        for &note in notes {
            self.send_midi_note(note, velocity, channel)?;
        }
        Ok(())
    }
    
    fn panic(&mut self) -> Result<()> {
        self.midi_panic()
    }
    
    fn set_parameters(&mut self, params: &[(u32, f64)]) -> Result<()> {
        self.update_parameters(|update| {
            for &(id, value) in params {
                update.set(id, value);
            }
            Ok(())
        })
    }
    
    fn get_parameter_by_name(&self, name: &str) -> Result<f64> {
        let param = self.find_parameter(name)?;
        Ok(param.value)
    }
}