//! Simplified API for common VST3 hosting tasks.
//!
//! This module provides convenience functions that make it easy to get started
//! with VST3 plugin hosting without needing to understand all the configuration
//! options and complex APIs.
//!
//! ## Quick Examples
//!
//! ### Load and play a plugin
//! ```no_run
//! use vst3_host::simple;
//! use vst3_host::midi::MidiChannel;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Load a plugin with sensible defaults
//! let mut plugin = simple::load_plugin("/path/to/synth.vst3")?;
//!
//! // Start processing audio
//! plugin.start_processing()?;
//!
//! // Play a note
//! plugin.send_midi_note(60, 127, MidiChannel::Ch1)?; // Middle C
//! # Ok(())
//! # }
//! ```
//!
//! ### Discover plugins easily
//! ```no_run
//! use vst3_host::simple;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Find all plugins on the system
//! let plugins = simple::discover_plugins()?;
//!
//! for plugin in plugins {
//!     println!("Found: {} by {}", plugin.name, plugin.vendor);
//! }
//! # Ok(())
//! # }
//! ```

use crate::{
    error::{Error, Result},
    host::Vst3Host,
    plugin::{Plugin, PluginInfo},
};
use std::path::Path;

/// Load a VST3 plugin with sensible defaults.
///
/// This function creates a host with default audio settings and loads the
/// specified plugin. It's the quickest way to get started with plugin hosting.
///
/// # Default Settings
/// - Sample rate: 44100 Hz
/// - Block size: 512 samples
/// - Input channels: 2 (stereo)
/// - Output channels: 2 (stereo)
/// - Process isolation: disabled (in-process). Use [`load_plugin_isolated`] to opt in.
///
/// # Arguments
/// * `path` - Path to the VST3 plugin (.vst3 file or directory)
///
/// # Returns
/// A loaded and configured plugin ready for audio processing.
///
/// # Examples
/// ```no_run
/// use vst3_host::simple;
/// use vst3_host::midi::MidiChannel;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut plugin = simple::load_plugin("/Applications/Dexed.vst3")?;
/// plugin.start_processing()?;
/// plugin.send_midi_note(60, 100, MidiChannel::Ch1)?;
/// # Ok(())
/// # }
/// ```
pub fn load_plugin<P: AsRef<Path>>(path: P) -> Result<Plugin> {
    let mut host = Vst3Host::builder()
        .sample_rate(44100.0)
        .block_size(512)
        .input_channels(2)
        .output_channels(2)
        .build()?;

    host.load_plugin(path)
}

/// Load a plugin with custom audio settings.
///
/// This provides a middle ground between the fully automatic `load_plugin()`
/// and the full control of the host builder pattern.
///
/// # Arguments
/// * `path` - Path to the VST3 plugin
/// * `sample_rate` - Audio sample rate in Hz (typically 44100 or 48000)
/// * `block_size` - Audio buffer size (typically 512 or 1024)
///
/// # Examples
/// ```no_run
/// use vst3_host::simple;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Load with professional audio settings
/// let mut plugin = simple::load_plugin_with_settings(
///     "/path/to/plugin.vst3",
///     48000.0,  // 48kHz sample rate
///     256       // Small buffer for low latency
/// )?;
/// # Ok(())
/// # }
/// ```
pub fn load_plugin_with_settings<P: AsRef<Path>>(
    path: P,
    sample_rate: f64,
    block_size: usize,
) -> Result<Plugin> {
    let mut host = Vst3Host::builder()
        .sample_rate(sample_rate)
        .block_size(block_size)
        .input_channels(2)
        .output_channels(2)
        .build()?;

    host.load_plugin(path)
}

/// Load a plugin with crash protection enabled.
///
/// This loads the plugin in a separate process, which prevents plugin crashes
/// from affecting your application. Use this for untested or problematic plugins.
///
/// # Arguments
/// * `path` - Path to the VST3 plugin
///
/// # Examples
/// ```no_run
/// use vst3_host::simple;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Load a potentially unstable plugin safely
/// let mut plugin = simple::load_plugin_isolated("/path/to/sketchy_plugin.vst3")?;
/// # Ok(())
/// # }
/// ```
pub fn load_plugin_isolated<P: AsRef<Path>>(path: P) -> Result<Plugin> {
    let mut host = Vst3Host::builder()
        .sample_rate(44100.0)
        .block_size(512)
        .input_channels(2)
        .output_channels(2)
        .with_process_isolation(true) // Force process isolation
        .build()?;

    host.load_plugin(path)
}

/// Load a plugin and immediately start playing it through the default audio device.
///
/// The quickest way to actually hear a synth: load, then `play`. The returned
/// [`AudioHandle`](crate::AudioHandle) keeps audio running until dropped, and lets
/// you control the plugin while it plays.
///
/// # Examples
/// ```no_run
/// use vst3_host::{simple, midi::MidiChannel};
///
/// # fn main() -> vst3_host::Result<()> {
/// let plugin = simple::load_plugin("/path/to/synth.vst3")?;
/// let audio = simple::play(plugin)?;
/// audio.lock().send_midi_note(60, 100, MidiChannel::Ch1)?; // middle C
/// std::thread::sleep(std::time::Duration::from_secs(2));
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "cpal-backend")]
pub fn play(plugin: Plugin) -> Result<crate::AudioHandle> {
    let backend = crate::backends::CpalBackend::new()?;
    let config = crate::audio::AudioConfig {
        output_channels: 2,
        input_channels: 0,
        ..Default::default()
    };
    crate::playback::play_with_backend(&backend, plugin, config)
}

/// Discover all VST3 plugins in the standard system locations.
///
/// Scans the platform's standard VST3 directories and returns metadata for each
/// plugin found. For progress reporting during a long scan, use
/// [`Vst3Host::discover_plugins_with_callback`](crate::Vst3Host::discover_plugins_with_callback).
///
/// # Examples
/// ```no_run
/// use vst3_host::simple;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// for info in simple::discover_plugins()? {
///     println!("Found: {} by {}", info.name, info.vendor);
/// }
/// # Ok(())
/// # }
/// ```
pub fn discover_plugins() -> Result<Vec<PluginInfo>> {
    let mut host = Vst3Host::builder()
        .scan_default_paths() // Enable scanning system directories
        .build()?;

    host.discover_plugins()
}

/// Discover plugins in a specific directory.
///
/// # Arguments
/// * `path` - Directory to scan for VST3 plugins
///
/// # Examples
/// ```no_run
/// use vst3_host::simple;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let plugins = simple::discover_plugins_in("/my/custom/vst3/folder")?;
/// println!("{} plugins found", plugins.len());
/// # Ok(())
/// # }
/// ```
pub fn discover_plugins_in<P: AsRef<Path>>(path: P) -> Result<Vec<PluginInfo>> {
    let mut host = Vst3Host::builder()
        .add_scan_path(path)
        .build()?;

    host.discover_plugins()
}

/// Get information about a specific plugin without loading it.
///
/// This is useful for checking plugin compatibility or displaying plugin
/// information before deciding whether to load it.
///
/// # Arguments
/// * `path` - Path to the VST3 plugin
///
/// # Returns
/// Plugin information including name, vendor, version, and capabilities.
///
/// # Examples
/// ```no_run
/// use vst3_host::simple;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let info = simple::get_plugin_info("/path/to/plugin.vst3")?;
/// println!("Plugin: {} v{} by {}", info.name, info.version, info.vendor);
/// println!("Has GUI: {}", info.has_gui);
/// println!("Audio I/O: {} in, {} out", info.audio_inputs, info.audio_outputs);
/// # Ok(())
/// # }
/// ```
pub fn get_plugin_info<P: AsRef<Path>>(path: P) -> Result<PluginInfo> {
    let path = path.as_ref();
    
    if !path.exists() {
        return Err(Error::PluginNotFound(path.display().to_string()));
    }

    // Create a minimal host just for info gathering
    let mut host = Vst3Host::builder().build()?;
    
    // Load plugin to get info, then immediately drop it
    let plugin = host.load_plugin(path)?;
    Ok(plugin.info().clone())
}

/// Check if a plugin path is valid and loadable.
///
/// This performs basic validation without actually loading the plugin.
/// Useful for filtering plugin lists or validating user input.
///
/// # Arguments
/// * `path` - Path to check
///
/// # Returns
/// `true` if the path appears to be a valid VST3 plugin, `false` otherwise.
///
/// # Examples
/// ```no_run
/// use vst3_host::simple;
///
/// if simple::is_valid_plugin("/path/to/plugin.vst3") {
///     println!("Plugin path looks valid");
/// } else {
///     println!("Not a valid VST3 plugin path");
/// }
/// ```
pub fn is_valid_plugin<P: AsRef<Path>>(path: P) -> bool {
    let path = path.as_ref();
    
    // Basic checks
    if !path.exists() {
        return false;
    }
    
    // Check for .vst3 extension
    if let Some(extension) = path.extension() {
        if extension.to_string_lossy().to_lowercase() == "vst3" {
            return true;
        }
    }
    
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_plugin() {
        // Test non-existent path
        assert!(!is_valid_plugin("/nonexistent/path.vst3"));
        
        // Test wrong extension
        assert!(!is_valid_plugin("plugin.dll"));
        assert!(!is_valid_plugin("plugin.so"));
        
        // Would need actual plugin files to test positive cases
    }

    #[test]
    fn test_host_creation() {
        // Test that we can create hosts with different configurations
        let host1 = Vst3Host::builder()
            .sample_rate(44100.0)
            .block_size(512)
            .build();
        assert!(host1.is_ok());
        
        let host2 = Vst3Host::builder()
            .sample_rate(48000.0)
            .block_size(256)
            .with_process_isolation(true)
            .build();
        assert!(host2.is_ok());
    }
}