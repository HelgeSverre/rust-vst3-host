//! VST3 host implementation

use crate::{
    audio::AudioConfig,
    error::{Error, Result},
    plugin::{Plugin, PluginInfo, PluginInternal},
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
    /// Whether to automatically isolate plugins known to be crash-prone in-process
    /// (e.g. Waves), so a crash can't take down the host.
    pub(crate) auto_isolate_problematic: bool,
    /// Whether to scan default system paths for plugins
    pub(crate) scan_default_paths: bool,
}

impl Vst3Host {
    /// Create a new VST3 host with default settings.
    ///
    /// Discovery scans the standard system VST3 directories (consistent with
    /// [`Vst3Host::default`]). For explicit control use [`Vst3Host::builder`]; the builder
    /// does **not** scan system paths unless you opt in with
    /// [`Vst3HostBuilder::scan_default_paths`].
    pub fn new() -> Result<Self> {
        Self::builder().scan_default_paths().build()
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

    /// Discover VST3 plugins in configured scan paths
    pub fn discover_plugins(&mut self) -> Result<Vec<PluginInfo>> {
        let mut all_paths = self.custom_paths.clone();

        // Add system paths if enabled
        if self.scan_default_paths {
            all_paths.extend(crate::discovery::scan_standard_paths());
        }

        // Scan directories for VST3 plugins
        let plugin_paths = crate::discovery::scan_directories(&all_paths)?;

        // Get plugin info for each found plugin
        let mut plugins = Vec::new();
        for path in plugin_paths {
            match crate::discovery::get_plugin_info(&path) {
                Ok(info) => plugins.push(info),
                Err(e) => {
                    log::warn!("Failed to get info for plugin {}: {}", path.display(), e);
                    // Continue with other plugins
                }
            }
        }

        Ok(plugins)
    }

    /// List VST3 bundle paths in the configured scan locations **without loading them**.
    ///
    /// Fast and safe: unlike [`Self::discover_plugins`] (which loads and initializes
    /// every plugin to read its metadata, and can be slow or crash-prone in-process),
    /// this only walks the filesystem. Use it when you just need the list of available
    /// `.vst3` paths (e.g. to populate a picker) and will load on demand.
    pub fn scan_plugin_paths(&self) -> Vec<std::path::PathBuf> {
        let mut all_paths = self.custom_paths.clone();
        if self.scan_default_paths {
            all_paths.extend(crate::discovery::scan_standard_paths());
        }
        crate::discovery::scan_directories(&all_paths).unwrap_or_default()
    }

    /// Discover VST3 plugins, reporting progress through a callback.
    ///
    /// The callback receives [`DiscoveryProgress`] events: one `Started` at the
    /// beginning, a `Found` or `Error` per candidate, and a final `Completed`.
    /// Returns the successfully-inspected plugins, same as [`Self::discover_plugins`].
    pub fn discover_plugins_with_callback<F>(
        &mut self,
        mut on_progress: F,
    ) -> Result<Vec<PluginInfo>>
    where
        F: FnMut(DiscoveryProgress),
    {
        let mut all_paths = self.custom_paths.clone();

        if self.scan_default_paths {
            all_paths.extend(crate::discovery::scan_standard_paths());
        }

        let plugin_paths = crate::discovery::scan_directories(&all_paths)?;
        let total = plugin_paths.len();

        on_progress(DiscoveryProgress::Started {
            total_plugins: total,
        });

        let mut plugins = Vec::new();
        for (index, path) in plugin_paths.into_iter().enumerate() {
            match crate::discovery::get_plugin_info(&path) {
                Ok(info) => {
                    on_progress(DiscoveryProgress::Found {
                        plugin: info.clone(),
                        current: index + 1,
                        total,
                    });
                    plugins.push(info);
                }
                Err(e) => {
                    log::warn!("Failed to get info for plugin {}: {}", path.display(), e);
                    on_progress(DiscoveryProgress::Error {
                        path: path.display().to_string(),
                        error: e.to_string(),
                    });
                }
            }
        }

        on_progress(DiscoveryProgress::Completed {
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

        // Use process isolation if explicitly enabled, or automatically for plugins
        // known to be crash-prone in-process (e.g. Waves) when auto-isolation is on.
        let isolate = self.use_process_isolation
            || (self.auto_isolate_problematic
                && crate::internal::module_loader::has_objc_conflicts(path));
        if isolate {
            self.load_plugin_isolated(path)
        } else {
            self.load_plugin_internal(path)
        }
    }

    /// Probe whether a plugin loads safely, **without risking the host process** — it is
    /// loaded in an isolated helper, so a crash is contained. This is the "validate
    /// plugins" operation a scanner uses to blacklist bad plugins.
    ///
    /// Requires the `process-isolation` feature.
    #[cfg(feature = "process-isolation")]
    pub fn probe_plugin<P: AsRef<Path>>(&self, path: P) -> ProbeResult {
        use crate::process_isolation::{HostCommand, HostResponse, PluginHostProcess};

        let path = path.as_ref();
        if !path.exists() {
            return ProbeResult::Failed("plugin path does not exist".to_string());
        }
        let mut process = match PluginHostProcess::new() {
            Ok(p) => p,
            Err(e) => return ProbeResult::Failed(format!("helper unavailable: {e}")),
        };
        match process.send_command(HostCommand::LoadPlugin {
            path: path.display().to_string(),
            sample_rate: self.config.sample_rate,
            block_size: self.config.block_size as u32,
            tempo: self.config.tempo,
            time_sig_numerator: self.config.time_sig_numerator,
            time_sig_denominator: self.config.time_sig_denominator,
        }) {
            Ok(HostResponse::PluginInfo { .. }) => ProbeResult::Ok,
            Ok(HostResponse::Error { message }) => ProbeResult::Failed(message),
            Ok(_) => ProbeResult::Failed("unexpected response from helper".to_string()),
            Err(e) if e.to_lowercase().contains("crash") => ProbeResult::Crashed,
            Err(e) if e.to_lowercase().contains("timed out") => ProbeResult::TimedOut,
            Err(e) => ProbeResult::Failed(e),
        }
    }

    /// Load a plugin in-process
    fn load_plugin_internal(&mut self, path: &Path) -> Result<Plugin> {
        // Load the plugin implementation directly - it will handle path resolution
        let mut plugin_impl = crate::internal::plugin_impl::PluginImpl::load(path)?;

        // Thread the configured transport into the plugin's host ProcessContext so
        // tempo-synced DSP sees the host tempo / time signature.
        plugin_impl.set_transport(
            self.config.tempo,
            self.config.time_sig_numerator,
            self.config.time_sig_denominator,
        );

        // Get the updated info from the plugin implementation (has_gui might have been updated)
        let updated_info = plugin_impl.info.clone();

        // Size meters to the plugin's real output channel count (bus-aware), not a stereo
        // assumption; fall back to 2 only when the plugin reports no output channels.
        let output_channels = match plugin_impl.output_channel_count() {
            0 => 2,
            n => n,
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
    fn load_plugin_isolated(&mut self, path: &Path) -> Result<Plugin> {
        use crate::process_isolation::{HostCommand, HostResponse, PluginHostProcess};

        // Create and start the isolated plugin process
        let mut process = PluginHostProcess::new()
            .map_err(|e| Error::Other(format!("Failed to create isolated process: {}", e)))?;

        // Load the plugin in the isolated process
        let response = process
            .send_command(HostCommand::LoadPlugin {
                path: path.display().to_string(),
                sample_rate: self.config.sample_rate,
                block_size: self.config.block_size as u32,
                tempo: self.config.tempo,
                time_sig_numerator: self.config.time_sig_numerator,
                time_sig_denominator: self.config.time_sig_denominator,
            })
            .map_err(|e| Error::Other(format!("Failed to load plugin in isolation: {}", e)))?;

        // Verify the plugin loaded successfully. Metadata comes straight from the helper's
        // accurate introspection, so the isolated path matches the in-process one.
        let (loaded_info, output_channels) = match response {
            HostResponse::PluginInfo {
                vendor,
                name,
                version,
                category,
                uid,
                has_gui,
                audio_inputs,
                audio_outputs,
                output_channels,
                has_midi_input,
                has_midi_output,
            } => {
                let info = PluginInfo {
                    path: path.to_path_buf(),
                    name,
                    vendor,
                    version,
                    category,
                    uid,
                    has_gui,
                    audio_inputs: audio_inputs as u32,
                    audio_outputs: audio_outputs as u32,
                    has_midi_input,
                    has_midi_output,
                };
                let channels = if output_channels > 0 {
                    output_channels as usize
                } else {
                    2
                };
                (info, channels)
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
            self.config.tempo,
            self.config.time_sig_numerator,
            self.config.time_sig_denominator,
            output_channels,
        );

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
            auto_isolate_problematic: false,
            scan_default_paths: true, // Default to true for backward compatibility
        }
    }
}

/// Builder for VST3 host configuration
///
/// All fields default to their type defaults; notably `scan_default_paths` defaults to
/// `false`, requiring explicit opt-in (unlike `Vst3Host`, which defaults it to `true`).
#[derive(Default)]
pub struct Vst3HostBuilder {
    config: AudioConfig,
    custom_paths: Vec<PathBuf>,
    use_process_isolation: bool,
    auto_isolate_problematic: bool,
    scan_default_paths: bool,
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

    /// Set the transport tempo (beats per minute) advertised to plugins in the host
    /// `ProcessContext`. Drives tempo-synced DSP (LFOs, synced delays, arpeggiators).
    /// Defaults to `120.0`. Non-finite or non-positive values are ignored (a tempo of 0 or
    /// less would freeze/reverse the derived musical playhead), keeping the previous tempo.
    pub fn tempo(mut self, bpm: f64) -> Self {
        if bpm.is_finite() && bpm > 0.0 {
            self.config.tempo = bpm;
        }
        self
    }

    /// Set the transport time signature advertised to plugins in the host
    /// `ProcessContext` (`num`/`den`, e.g. `4, 4`). Defaults to `4/4`. Non-positive values
    /// are ignored (a malformed time signature), keeping the previous setting.
    pub fn time_signature(mut self, num: i32, den: i32) -> Self {
        if num > 0 && den > 0 {
            self.config.time_sig_numerator = num;
            self.config.time_sig_denominator = den;
        }
        self
    }

    /// Enable or disable process isolation for plugin loading
    pub fn with_process_isolation(mut self, enabled: bool) -> Self {
        self.use_process_isolation = enabled;
        self
    }

    /// Automatically load known crash-prone plugins (e.g. Waves/WaveShell) in an isolated
    /// process so a crash is contained instead of taking down the host. Plugins that load
    /// fine in-process are unaffected. Requires the `process-isolation` feature at runtime
    /// (the helper binary must be present).
    pub fn auto_isolate_problematic(mut self, enabled: bool) -> Self {
        self.auto_isolate_problematic = enabled;
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
            auto_isolate_problematic: self.auto_isolate_problematic,
            scan_default_paths: self.scan_default_paths,
        })
    }
}

/// The outcome of [`Vst3Host::probe_plugin`] — whether a plugin can be loaded safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    /// The plugin loaded successfully in an isolated process.
    Ok,
    /// The plugin crashed the isolated helper while loading (do not load in-process).
    Crashed,
    /// The plugin did not respond within the timeout.
    TimedOut,
    /// Loading failed with an error (not a crash) — message included.
    Failed(String),
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
    /// Load a plugin and immediately start playing it through the default audio
    /// output device, using the host's configured sample rate and block size.
    ///
    /// This is the "batteries-included" path: it wires a [`CpalBackend`] to the
    /// plugin and pumps audio for you. The returned [`AudioHandle`] keeps the stream
    /// alive — drop it to stop — and lets you keep sending MIDI / changing parameters
    /// while it plays:
    ///
    /// ```no_run
    /// # use vst3_host::Vst3Host;
    /// # use vst3_host::midi::MidiChannel;
    /// # fn main() -> vst3_host::Result<()> {
    /// let mut host = Vst3Host::new()?;
    /// let plugin = host.load_plugin("/path/to/synth.vst3")?;
    /// let audio = host.play(plugin)?;
    /// audio.lock().send_midi_note(60, 100, MidiChannel::Ch1)?;
    /// std::thread::sleep(std::time::Duration::from_secs(1));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`CpalBackend`]: crate::backends::CpalBackend
    /// [`AudioHandle`]: crate::AudioHandle
    pub fn play(&self, plugin: Plugin) -> Result<crate::AudioHandle> {
        let backend = crate::backends::CpalBackend::new()?;
        let config = crate::audio::AudioConfig {
            output_channels: 2,
            input_channels: 0,
            ..self.config
        };
        crate::playback::play_with_backend(&backend, plugin, config)
    }

    /// Play a plugin through the default device using the **lock-free** real-time path
    /// (a [`RealtimePluginRunner`]) instead of the mutex-based [`Self::play`].
    ///
    /// The audio callback takes no lock; queue MIDI and parameter changes through the
    /// returned handle's [`RtControl`](crate::RtControl):
    ///
    /// ```no_run
    /// # use vst3_host::{Vst3Host, midi::MidiEvent, midi::MidiChannel};
    /// # fn main() -> vst3_host::Result<()> {
    /// let mut host = Vst3Host::new()?;
    /// let plugin = host.load_plugin("/path/synth.vst3")?;
    /// let mut audio = host.play_realtime(plugin, 1024)?;
    /// audio.control().send_midi(MidiEvent::NoteOn { channel: MidiChannel::Ch1, note: 60, velocity: 100 });
    /// std::thread::sleep(std::time::Duration::from_secs(1));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`RealtimePluginRunner`]: crate::RealtimePluginRunner
    pub fn play_realtime(
        &self,
        plugin: Plugin,
        command_capacity: usize,
    ) -> Result<crate::playback::RtAudioHandle> {
        let backend = crate::backends::CpalBackend::new()?;
        let config = crate::audio::AudioConfig {
            output_channels: 2,
            input_channels: 0,
            ..self.config
        };
        crate::playback::play_realtime_with_backend(&backend, plugin, config, command_capacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_defaults_to_120_bpm_4_4() {
        let host = Vst3HostBuilder::default().build().unwrap();
        assert_eq!(host.config().tempo, 120.0);
        assert_eq!(host.config().time_sig_numerator, 4);
        assert_eq!(host.config().time_sig_denominator, 4);
    }

    #[test]
    fn builder_threads_tempo_and_time_signature_into_config() {
        let host = Vst3HostBuilder::default()
            .tempo(140.0)
            .time_signature(7, 8)
            .build()
            .unwrap();
        assert_eq!(host.config().tempo, 140.0);
        assert_eq!(host.config().time_sig_numerator, 7);
        assert_eq!(host.config().time_sig_denominator, 8);
    }
}
