//! VST3 plugin wrapper with safe API

use crate::{
    audio::{AudioBuffers, AudioLevels},
    error::{Error, Result},
    midi::{MidiChannel, MidiEvent},
    parameters::{Parameter, ParameterUpdate},
};
use std::sync::{Arc, Mutex};

/// Information about a VST3 plugin
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginInfo {
    /// Full path to the VST3 bundle/file
    pub path: std::path::PathBuf,
    /// Plugin name
    pub name: String,
    /// Vendor/manufacturer name
    pub vendor: String,
    /// Plugin version
    pub version: String,
    /// Plugin category (e.g., "Fx", "Instrument")
    pub category: String,
    /// Unique plugin ID
    pub uid: String,
    /// Number of audio input buses
    pub audio_inputs: u32,
    /// Number of audio output buses
    pub audio_outputs: u32,
    /// Whether the plugin accepts MIDI input
    pub has_midi_input: bool,
    /// Whether the plugin produces MIDI output
    pub has_midi_output: bool,
    /// Whether the plugin has a GUI
    pub has_gui: bool,
}

/// VST3 plugin instance
#[allow(clippy::type_complexity)] // callback fields are Box<dyn Fn...>; intrinsic to the API
pub struct Plugin {
    // Internal state is hidden from public API
    pub(crate) info: PluginInfo,
    pub(crate) is_processing: bool,
    /// Configured sample rate (diagnostics / config record)
    #[allow(dead_code)]
    pub(crate) sample_rate: f64,
    /// Configured block size (diagnostics / config record)
    #[allow(dead_code)]
    pub(crate) block_size: usize,
    pub(crate) audio_levels: Arc<Mutex<AudioLevels>>,
    pub(crate) parameter_change_callback: Option<Box<dyn Fn(u32, f64) + Send + 'static>>,
    pub(crate) audio_callback: Option<Box<dyn Fn(&AudioLevels) + Send + 'static>>,

    // These will be populated by the actual implementation
    pub(crate) internal: Option<Box<dyn PluginInternal>>,
}

// Internal trait for hiding implementation details
pub(crate) trait PluginInternal: Send {
    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()>;
    fn get_parameter(&self, id: u32) -> Result<f64>;
    fn get_all_parameters(&self) -> Result<Vec<Parameter>>;
    fn format_parameter(&self, id: u32, normalized: f64) -> Result<String>;
    fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()>;
    fn send_midi_event(&mut self, event: MidiEvent) -> Result<()>;
    fn start_processing(&mut self) -> Result<()>;
    fn stop_processing(&mut self) -> Result<()>;
    fn has_editor(&self) -> bool;
    fn open_editor(&mut self, parent: *mut std::ffi::c_void) -> Result<()>;
    fn close_editor(&mut self) -> Result<()>;
    fn get_editor_size(&self) -> Result<(i32, i32)>;
    fn get_parameter_changes(&self) -> Vec<(u32, f64)>;
    /// Take the MIDI events the plugin has emitted since the last call. Defaults to empty
    /// for implementations that don't capture output MIDI (e.g. process isolation).
    fn take_output_events(&self) -> Vec<MidiEvent> {
        Vec::new()
    }
    /// Serialize the plugin's current state to an opaque byte blob.
    fn save_state(&self) -> Result<Vec<u8>> {
        Err(Error::Other(
            "state save/restore is not supported".to_string(),
        ))
    }
    /// Restore the plugin's state from a blob previously returned by [`Self::save_state`].
    fn load_state(&mut self, _data: &[u8]) -> Result<()> {
        Err(Error::Other(
            "state save/restore is not supported".to_string(),
        ))
    }
    /// OS process id of the isolated helper, if this plugin runs out-of-process.
    fn helper_pid(&self) -> Option<u32> {
        None
    }
    /// Recover from a crashed isolated helper by respawning and reloading. Only meaningful
    /// for process-isolated plugins.
    fn recover(&mut self) -> Result<()> {
        Err(Error::Other(
            "recovery is only supported for process-isolated plugins".to_string(),
        ))
    }
    /// The size the plugin's editor has requested (via `IPlugFrame`) since the last poll.
    fn take_editor_resize_request(&self) -> Option<(i32, i32)> {
        None
    }
    /// Total output audio channels across the plugin's output buses. Defaults to 2.
    fn output_channel_count(&self) -> usize {
        2
    }
}

impl Plugin {
    /// Get plugin information
    pub fn info(&self) -> &PluginInfo {
        &self.info
    }

    /// Get all parameters
    pub fn get_parameters(&self) -> Result<Vec<Parameter>> {
        self.internal
            .as_ref()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .get_all_parameters()
    }

    /// Set a parameter value by ID
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        if !(0.0..=1.0).contains(&value) {
            return Err(Error::InvalidParameter(format!(
                "Value {} is out of range [0.0, 1.0]",
                value
            )));
        }

        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .set_parameter(id, value)?;

        // Trigger callback if set
        if let Some(ref callback) = self.parameter_change_callback {
            callback(id, value);
        }

        Ok(())
    }

    /// Get a parameter value by ID
    pub fn get_parameter(&mut self, id: u32) -> Result<f64> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .get_parameter(id)
    }

    /// Format a parameter value as the plugin itself would display it.
    ///
    /// VST3 keeps all parameter values normalized (0.0–1.0) and delegates
    /// human-readable formatting to the plugin's controller. This asks the plugin to
    /// render `normalized` for parameter `id`, returning exactly what its own UI would
    /// show — e.g. `"440.00 Hz"`, `"-6.0 dB"`, `"Sine"`. Prefer this over
    /// [`Parameter::format_value`], which can only approximate without the plugin's
    /// internal mapping.
    pub fn format_parameter(&self, id: u32, normalized: f64) -> Result<String> {
        self.internal
            .as_ref()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .format_parameter(id, normalized)
    }

    /// Set a parameter by name
    pub fn set_parameter_by_name(&mut self, name: &str, value: f64) -> Result<()> {
        let params = self.get_parameters()?;
        let param = params
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| Error::InvalidParameter(format!("Parameter '{}' not found", name)))?;

        self.set_parameter(param.id, value)
    }

    /// Find a parameter by name
    pub fn find_parameter(&self, name: &str) -> Result<Parameter> {
        let params = self.get_parameters()?;
        params
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| Error::InvalidParameter(format!("Parameter '{}' not found", name)))
    }

    /// Send a MIDI note on event
    pub fn send_midi_note(&mut self, note: u8, velocity: u8, channel: MidiChannel) -> Result<()> {
        if note > 127 {
            return Err(Error::MidiError(format!("Invalid note number: {}", note)));
        }
        if velocity > 127 {
            return Err(Error::MidiError(format!("Invalid velocity: {}", velocity)));
        }

        let event = MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        };
        self.send_midi_event(event)
    }

    /// Send a MIDI note off event
    pub fn send_midi_note_off(&mut self, note: u8, channel: MidiChannel) -> Result<()> {
        if note > 127 {
            return Err(Error::MidiError(format!("Invalid note number: {}", note)));
        }

        let event = MidiEvent::NoteOff {
            channel,
            note,
            velocity: 0,
        };
        self.send_midi_event(event)
    }

    /// Send a MIDI control change event
    pub fn send_midi_cc(&mut self, controller: u8, value: u8, channel: MidiChannel) -> Result<()> {
        if controller > 127 {
            return Err(Error::MidiError(format!(
                "Invalid controller number: {}",
                controller
            )));
        }
        if value > 127 {
            return Err(Error::MidiError(format!("Invalid CC value: {}", value)));
        }

        let event = MidiEvent::ControlChange {
            channel,
            controller,
            value,
        };
        self.send_midi_event(event)
    }

    /// Send a generic MIDI event
    pub fn send_midi_event(&mut self, event: MidiEvent) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .send_midi_event(event)
    }

    /// Start audio processing
    pub fn start_processing(&mut self) -> Result<()> {
        if self.is_processing {
            return Ok(());
        }

        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .start_processing()?;

        self.is_processing = true;
        Ok(())
    }

    /// Stop audio processing
    pub fn stop_processing(&mut self) -> Result<()> {
        if !self.is_processing {
            return Ok(());
        }

        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .stop_processing()?;

        self.is_processing = false;
        Ok(())
    }

    /// Process audio buffers
    pub fn process_audio(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        if !self.is_processing {
            return Err(Error::Other("Plugin is not processing".to_string()));
        }

        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .process(buffers)?;

        // Update audio levels
        if let Ok(mut levels) = self.audio_levels.lock() {
            levels.update_from_buffers(&buffers.outputs);

            // Trigger audio callback if set
            if let Some(ref callback) = self.audio_callback {
                callback(&levels);
            }
        }

        Ok(())
    }

    /// Get current output levels.
    ///
    /// Recovers automatically if the audio thread panicked while holding the lock
    /// (poisoned mutex) rather than propagating the panic to the caller — metering
    /// must never take down a UI thread polling it.
    pub fn get_output_levels(&self) -> AudioLevels {
        self.audio_levels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Check if the plugin is currently processing
    pub fn is_processing(&self) -> bool {
        self.is_processing
    }

    /// Set a callback for parameter changes
    pub fn on_parameter_change<F>(&mut self, callback: F)
    where
        F: Fn(u32, f64) + Send + 'static,
    {
        self.parameter_change_callback = Some(Box::new(callback));
    }

    /// Set a callback for audio processing (called after each process cycle)
    pub fn on_audio_process<F>(&mut self, callback: F)
    where
        F: Fn(&AudioLevels) + Send + 'static,
    {
        self.audio_callback = Some(Box::new(callback));
    }

    /// Check if the plugin has an editor GUI
    pub fn has_editor(&self) -> bool {
        self.internal
            .as_ref()
            .map(|i| i.has_editor())
            .unwrap_or(false)
    }

    /// Open the plugin editor window
    pub fn open_editor(&mut self, parent: WindowHandle) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .open_editor(parent.0)
    }

    /// Close the plugin editor window
    pub fn close_editor(&mut self) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .close_editor()
    }

    /// Get the preferred editor size
    pub fn get_editor_size(&self) -> Result<(i32, i32)> {
        self.internal
            .as_ref()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .get_editor_size()
    }

    /// Create a batch parameter update
    pub fn update_parameters<F>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(&mut ParameterUpdate) -> Result<()>,
    {
        let mut update = ParameterUpdate::new(self);
        f(&mut update)?;
        update.apply()
    }

    /// Send MIDI panic (all notes off, all sounds off, reset controllers)
    pub fn midi_panic(&mut self) -> Result<()> {
        for i in 0..16 {
            if let Some(channel) = MidiChannel::from_index(i) {
                // All Notes Off
                self.send_midi_cc(123, 0, channel)?;
                // All Sounds Off
                self.send_midi_cc(120, 0, channel)?;
                // Reset All Controllers
                self.send_midi_cc(121, 0, channel)?;
            }
        }
        Ok(())
    }

    /// Get parameter changes from plugin GUI
    /// Returns a vector of (parameter_id, normalized_value) pairs
    /// This should be called regularly to pick up parameter changes made through the plugin's GUI
    pub fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
        self.internal
            .as_ref()
            .map(|i| i.get_parameter_changes())
            .unwrap_or_default()
    }

    /// Take the MIDI events the plugin has emitted (e.g. from an arpeggiator or MPE
    /// controller) since the last call, draining the internal buffer.
    ///
    /// Output MIDI is captured while the plugin processes audio, so poll this regularly
    /// (e.g. each UI frame) while the plugin is playing. Returns an empty vector if the
    /// plugin emits nothing, or for plugins running under process isolation (output MIDI
    /// across the boundary is not captured yet).
    pub fn take_output_midi(&self) -> Vec<MidiEvent> {
        self.internal
            .as_ref()
            .map(|i| i.take_output_events())
            .unwrap_or_default()
    }

    /// Save the plugin's current state (parameters, internal settings, loaded preset) to
    /// an opaque byte blob.
    ///
    /// The bytes are the plugin's own serialized state — treat them as opaque and pair them
    /// with the plugin's identity ([`PluginInfo::uid`]); they only mean something to the
    /// same plugin. Persist them to restore a patch later with [`Self::load_state`], or to
    /// snapshot a session. Call this on the main thread (see the
    /// [threading model](https://docs.rs/vst3-host)).
    ///
    /// Returns an error for plugins that don't implement state saving, or when running
    /// under process isolation (not yet bridged across the boundary).
    pub fn save_state(&self) -> Result<Vec<u8>> {
        self.internal
            .as_ref()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .save_state()
    }

    /// Restore plugin state from a blob produced by [`Self::save_state`] on the *same*
    /// plugin. Applies to both the processor and the controller, so parameter values and
    /// the editor reflect the restored state.
    ///
    /// Passing bytes from a different plugin has undefined results (the plugin decides what
    /// to do with bytes it doesn't recognize). Call this on the main thread.
    pub fn load_state(&mut self, data: &[u8]) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .load_state(data)
    }

    /// The OS process id of the isolated helper hosting this plugin, or `None` if it runs
    /// in-process. Useful for monitoring an isolated plugin's resource use.
    pub fn isolation_pid(&self) -> Option<u32> {
        self.internal.as_ref().and_then(|i| i.helper_pid())
    }

    /// Total number of output audio channels across the plugin's output buses.
    ///
    /// Reflects the plugin's actual bus layout (mono / stereo / surround / multi-bus), not a
    /// stereo assumption — useful for sizing meters or output buffers. Returns 2 if unknown.
    pub fn output_channel_count(&self) -> usize {
        self.internal
            .as_ref()
            .map(|i| i.output_channel_count())
            .unwrap_or(2)
    }

    /// Poll for an editor resize the plugin requested via VST3's `IPlugFrame` since the last
    /// call, as `(width, height)` in pixels, or `None`.
    ///
    /// Plugins with resizable editors call back to ask the host to resize the window hosting
    /// their view. Poll this on your UI thread (e.g. each frame) while the editor is open and
    /// resize your editor container to match. Only the in-process editor path reports this.
    pub fn take_editor_resize_request(&self) -> Option<(i32, i32)> {
        self.internal
            .as_ref()
            .and_then(|i| i.take_editor_resize_request())
    }

    /// Recover a process-isolated plugin whose helper has crashed.
    ///
    /// When an isolated plugin's helper process dies, calls return [`Error::PluginCrashed`]
    /// and the host itself stays alive. This respawns the helper and reloads the plugin
    /// from the same path and audio settings, restarting processing if it was running.
    ///
    /// **The reloaded plugin starts from its default state** — parameter values and any
    /// loaded preset are lost. Snapshot with [`Self::save_state`] beforehand and
    /// [`Self::load_state`] after recovering to preserve them. Returns an error for
    /// in-process plugins (an in-process crash takes down the whole host) and if the
    /// reload itself fails.
    pub fn recover(&mut self) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .recover()
    }
}

/// Platform-specific window handle
pub struct WindowHandle(pub(crate) *mut std::ffi::c_void);

impl WindowHandle {
    /// Create from a raw window handle
    ///
    /// # Safety
    /// The pointer must be a valid window handle for the platform
    pub unsafe fn from_raw(handle: *mut std::ffi::c_void) -> Self {
        Self(handle)
    }
}

// Safe Send implementation - the window handle is platform-specific
unsafe impl Send for WindowHandle {}

#[cfg(target_os = "macos")]
impl WindowHandle {
    /// Create from an NSView pointer on macOS
    pub fn from_nsview(view: *mut std::ffi::c_void) -> Self {
        Self(view)
    }
}

#[cfg(target_os = "windows")]
impl WindowHandle {
    /// Create from an HWND on Windows
    pub fn from_hwnd(hwnd: *mut std::ffi::c_void) -> Self {
        Self(hwnd)
    }
}

#[cfg(target_os = "linux")]
impl WindowHandle {
    /// Create from an X11 window id on Linux (for VST3 `X11EmbedWindowID`).
    ///
    /// The VST3 X11 platform type expects the window id itself as the handle value,
    /// not a pointer to it.
    pub fn from_x11(window_id: u32) -> Self {
        Self(window_id as usize as *mut std::ffi::c_void)
    }
}
