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

/// A saved plugin preset: the plugin's identity plus its opaque state blob.
///
/// Written/read by [`Plugin::save_preset`] / [`Plugin::load_preset`]. The `uid` lets a
/// loader reject a preset that belongs to a different plugin (whose state bytes would be
/// meaningless or harmful).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginPreset {
    /// The originating plugin's unique class id ([`PluginInfo::uid`]).
    pub uid: String,
    /// The originating plugin's display name (for friendly mismatch messages).
    pub plugin_name: String,
    /// The plugin's opaque serialized state (from [`Plugin::save_state`]).
    pub state: Vec<u8>,
}

/// A plugin unit (from `IUnitInfo`) and its program list, if any.
///
/// Units form a hierarchy (via [`parent_id`](Self::parent_id)); a unit may carry a named
/// program list (e.g. a synth's factory patches). Query with [`Plugin::get_units`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginUnit {
    /// Unit id (unique within the plugin; the root unit is conventionally `0`).
    pub id: i32,
    /// Parent unit id, or `-1` for the root.
    pub parent_id: i32,
    /// Unit display name.
    pub name: String,
    /// Program names in this unit's program list (empty if the unit has none).
    pub programs: Vec<String>,
}

/// How the plugin should run: real-time (live playback) or offline (faster-than-real-time
/// bounce/render). Maps to VST3 `kRealtime` / `kOffline`; plugins may switch quality or
/// look-ahead accordingly. Defaults to [`ProcessMode::Realtime`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ProcessMode {
    /// Real-time / live processing (the default; `kRealtime`).
    #[default]
    Realtime,
    /// Offline / non-real-time processing such as a render or bounce (`kOffline`).
    Offline,
}

/// VST3 plugin instance
#[allow(clippy::type_complexity)] // callback fields are Box<dyn Fn...>; intrinsic to the API
pub struct Plugin {
    // Internal state is hidden from public API
    pub(crate) info: PluginInfo,
    pub(crate) is_processing: bool,
    /// Configured sample rate (exposed via [`Plugin::sample_rate`]).
    pub(crate) sample_rate: f64,
    /// Configured max block size (exposed via [`Plugin::block_size`]).
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
    /// Schedule a parameter change at a sample offset within the next process block.
    /// Defaults to a block-start change (ignores the offset) for implementations that don't
    /// support sample-accurate scheduling (e.g. process isolation).
    fn set_parameter_at(&mut self, id: u32, value: f64, _sample_offset: i32) -> Result<()> {
        self.set_parameter(id, value)
    }
    fn get_parameter(&self, id: u32) -> Result<f64>;
    fn get_all_parameters(&self) -> Result<Vec<Parameter>>;
    fn format_parameter(&self, id: u32, normalized: f64) -> Result<String>;
    fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()>;
    /// Re-run `setupProcessing` for a new sample rate / block size. Defaults to unsupported
    /// (e.g. process isolation, where reconfigure isn't marshalled across the boundary).
    fn reconfigure(&mut self, _sample_rate: f64, _block_size: usize) -> Result<()> {
        Err(Error::Other(
            "runtime reconfigure is not supported for this plugin".to_string(),
        ))
    }
    /// Switch the plugin's process mode (real-time vs offline), re-running `setupProcessing`.
    /// Defaults to unsupported (e.g. process isolation, where it isn't marshalled).
    fn set_process_mode(&mut self, _mode: crate::plugin::ProcessMode) -> Result<()> {
        Err(Error::Other(
            "process mode switching is not supported for this plugin".to_string(),
        ))
    }
    /// Query each audio bus's current speaker arrangement. Defaults to unsupported.
    fn bus_arrangements(&self) -> Result<crate::audio::BusArrangements> {
        Err(Error::Other(
            "bus arrangement query is not supported for this plugin".to_string(),
        ))
    }
    /// Request specific speaker arrangements for the audio buses (re-runs `setupProcessing`).
    /// Defaults to unsupported (e.g. process isolation, where it isn't marshalled).
    fn set_bus_arrangements(
        &mut self,
        _inputs: &[crate::audio::SpeakerArrangement],
        _outputs: &[crate::audio::SpeakerArrangement],
    ) -> Result<()> {
        Err(Error::Other(
            "bus arrangement negotiation is not supported for this plugin".to_string(),
        ))
    }
    /// Activate or deactivate a single bus (`IComponent::activateBus`). Defaults to
    /// unsupported.
    fn set_bus_active(
        &mut self,
        _media_type: crate::audio::MediaType,
        _direction: crate::audio::BusDirection,
        _bus_index: i32,
        _active: bool,
    ) -> Result<()> {
        Err(Error::Other(
            "bus activation is not supported for this plugin".to_string(),
        ))
    }
    fn send_midi_event(&mut self, event: MidiEvent) -> Result<()>;
    /// Schedule a MIDI event at a sample offset within the next process block.
    /// Defaults to a block-start event (ignores the offset) for implementations that don't
    /// support sample-accurate scheduling (e.g. process isolation).
    fn send_midi_event_at(&mut self, event: MidiEvent, _sample_offset: i32) -> Result<()> {
        self.send_midi_event(event)
    }
    /// Start a note and return a per-voice [`NoteId`] for targeting note-expression. Default:
    /// unsupported (process isolation does not yet marshal note expression, so it errors).
    fn note_on(
        &mut self,
        _channel: MidiChannel,
        _note: u8,
        _velocity: u8,
        _sample_offset: i32,
    ) -> Result<crate::midi::NoteId> {
        Err(Error::Other(
            "per-note expression is not supported for this plugin".to_string(),
        ))
    }
    /// Release a note started with [`Self::note_on`]. Default: unsupported.
    fn note_off(&mut self, _id: crate::midi::NoteId, _sample_offset: i32) -> Result<()> {
        Err(Error::Other(
            "per-note expression is not supported for this plugin".to_string(),
        ))
    }
    /// Send a per-note expression value (normalized 0..1) for a voice. Default: unsupported.
    fn send_note_expression(
        &mut self,
        _id: crate::midi::NoteId,
        _kind: crate::midi::NoteExpressionType,
        _value: f64,
        _sample_offset: i32,
    ) -> Result<()> {
        Err(Error::Other(
            "per-note expression is not supported for this plugin".to_string(),
        ))
    }
    /// Enumerate the per-note expressions the plugin advertises (`INoteExpressionController`).
    /// Defaults to empty.
    fn note_expressions(
        &self,
        _bus: i32,
        _channel: i16,
    ) -> Result<Vec<crate::midi::NoteExpressionInfo>> {
        Ok(Vec::new())
    }
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
    /// Enumerate the plugin's units and their program lists (`IUnitInfo`). Defaults to empty
    /// for implementations that don't query it yet (e.g. process isolation).
    fn get_units(&self) -> Result<Vec<PluginUnit>> {
        Ok(Vec::new())
    }
    /// Select a program in a unit's program list. Defaults to unsupported (e.g. plugins
    /// without `IUnitInfo`); implementations resolve the unit's program-change parameter and
    /// set it to the index's normalized value.
    fn select_program(&mut self, _unit_id: i32, _program_index: i32) -> Result<()> {
        Err(Error::Other(
            "program selection is not supported for this plugin".to_string(),
        ))
    }
    /// Processing latency in samples (`IAudioProcessor::getLatencySamples`). Defaults to 0.
    fn latency_samples(&self) -> u32 {
        0
    }
    /// Tail length in samples (`IAudioProcessor::getTailSamples`). Defaults to 0.
    fn tail_samples(&self) -> u32 {
        0
    }
    /// Resolve a MIDI controller `(bus, channel, cc)` to a parameter id via `IMidiMapping`.
    /// Defaults to `None` (plugin doesn't implement the interface, or no mapping / isolation).
    fn midi_cc_to_parameter(&self, _bus: i32, _channel: i16, _cc: u16) -> Option<u32> {
        None
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
    /// Number of times this plugin has been recovered (respawned + reloaded). Defaults to 0
    /// for non-isolated plugins.
    fn recovery_count(&self) -> u64 {
        0
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

    /// The sample rate (Hz) this plugin was configured with at load.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// The maximum block size (frames per `process_audio` call) configured at load.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Reconfigure the plugin for a new sample rate and/or maximum block size, re-running the
    /// plugin's `setupProcessing` and rebuilding its audio buffers.
    ///
    /// Use this when the audio device's sample rate changes mid-session instead of reloading.
    /// The plugin must **not** be processing: call [`Self::stop_processing`] first, reconfigure,
    /// then [`Self::start_processing`] again. Returns an error if called while processing, on an
    /// invalid sample rate / zero block size, or under process isolation (not yet marshalled).
    pub fn reconfigure(&mut self, sample_rate: f64, block_size: usize) -> Result<()> {
        if self.is_processing {
            return Err(Error::Other(
                "cannot reconfigure while processing; call stop_processing() first".to_string(),
            ));
        }
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(Error::InvalidParameter(format!(
                "sample rate must be finite and positive, got {sample_rate}"
            )));
        }
        if block_size == 0 {
            return Err(Error::InvalidParameter(
                "block size must be greater than 0".to_string(),
            ));
        }

        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .reconfigure(sample_rate, block_size)?;

        self.sample_rate = sample_rate;
        self.block_size = block_size;
        Ok(())
    }

    /// Switch the plugin between real-time and offline processing, re-running the plugin's
    /// `setupProcessing` so it can adjust quality / look-ahead for a faster-than-real-time
    /// bounce.
    ///
    /// Like [`Self::reconfigure`], the plugin must **not** be processing: call
    /// [`Self::stop_processing`] first. Returns an error if called while processing, or under
    /// process isolation (not marshalled across the boundary).
    pub fn set_process_mode(&mut self, mode: ProcessMode) -> Result<()> {
        if self.is_processing {
            return Err(Error::Other(
                "cannot set process mode while processing; call stop_processing() first"
                    .to_string(),
            ));
        }
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .set_process_mode(mode)
    }

    /// Query the current speaker arrangement of each audio input/output bus.
    pub fn bus_arrangements(&self) -> Result<crate::audio::BusArrangements> {
        self.internal
            .as_ref()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .bus_arrangements()
    }

    /// Request specific speaker arrangements for the audio buses (e.g. force stereo, or a
    /// surround layout). The slices give one [`SpeakerArrangement`](crate::audio::SpeakerArrangement)
    /// per input bus and per output bus, in bus-index order.
    ///
    /// Re-runs the plugin's `setupProcessing`, so the plugin must **not** be processing (call
    /// [`Self::stop_processing`] first). A plugin may decline a requested layout and keep its
    /// own; re-query with [`Self::bus_arrangements`] to see what was actually applied. Errors
    /// while processing or under process isolation (not marshalled).
    pub fn set_bus_arrangements(
        &mut self,
        inputs: &[crate::audio::SpeakerArrangement],
        outputs: &[crate::audio::SpeakerArrangement],
    ) -> Result<()> {
        if self.is_processing {
            return Err(Error::Other(
                "cannot set bus arrangements while processing; call stop_processing() first"
                    .to_string(),
            ));
        }
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .set_bus_arrangements(inputs, outputs)
    }

    /// Activate or deactivate a single bus on the plugin (`IComponent::activateBus`).
    ///
    /// Hosts must explicitly activate the buses they intend to use; a plugin's secondary
    /// buses (sidechain / aux inputs, extra outputs) commonly start **inactive** and only
    /// receive/produce audio once activated. (The load sequence already activates the main
    /// audio and event buses, so call this to enable the rest.)
    ///
    /// `media_type` selects audio vs event buses and `direction` selects input vs output;
    /// `bus_index` is the 0-based index within that `(media_type, direction)` group (the
    /// same indexing as [`crate::discovery::BusLayout`]). `active` true activates, false
    /// deactivates.
    ///
    /// VST3 requires bus activation to happen while the component is **inactive** — i.e.
    /// before processing starts. This therefore returns an error if called while the plugin
    /// is processing; call [`Self::stop_processing`] first, activate the bus, then
    /// [`Self::start_processing`] again. Returns an error for an out-of-range `bus_index`,
    /// and under process isolation activation marshals across the boundary.
    pub fn set_bus_active(
        &mut self,
        media_type: crate::audio::MediaType,
        direction: crate::audio::BusDirection,
        bus_index: i32,
        active: bool,
    ) -> Result<()> {
        if self.is_processing {
            return Err(Error::Other(
                "cannot activate a bus while processing; call stop_processing() first".to_string(),
            ));
        }
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .set_bus_active(media_type, direction, bus_index, active)
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

    /// Set a parameter value at a specific sample offset within the next process block.
    ///
    /// This is the sample-accurate building block for automation: call it once per
    /// sub-block point (e.g. from [`ParameterAutomation::points_for_block`]) and the plugin
    /// receives the changes at their offsets in the next `process_audio`. Like
    /// [`Self::set_parameter`], `value` is normalized `0.0..=1.0`.
    ///
    /// `sample_offset` is clamped to the block. Under process isolation the offset **is** now
    /// carried across the boundary and applied by the helper's in-process plugin.
    ///
    /// [`ParameterAutomation::points_for_block`]: crate::parameters::ParameterAutomation::points_for_block
    pub fn set_parameter_at(&mut self, id: u32, value: f64, sample_offset: i32) -> Result<()> {
        if !(0.0..=1.0).contains(&value) {
            return Err(Error::InvalidParameter(format!(
                "Value {} is out of range [0.0, 1.0]",
                value
            )));
        }
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .set_parameter_at(id, value, sample_offset)
    }

    /// Enumerate the plugin's units and their program lists (`IUnitInfo`).
    ///
    /// Returns an empty list for plugins that don't implement `IUnitInfo`, and (for now) for
    /// plugins running under process isolation. The root unit (id `0`) is typically present.
    pub fn get_units(&self) -> Result<Vec<PluginUnit>> {
        self.internal
            .as_ref()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .get_units()
    }

    /// Select a program (preset) in a unit's program list (`IUnitInfo`).
    ///
    /// `unit_id` is a [`PluginUnit::id`] from [`get_units`](Self::get_units) (the root unit is
    /// `0`); `program_index` is a 0-based index into that unit's [`PluginUnit::programs`].
    /// Internally this locates the unit's program-change parameter (the controller parameter
    /// tied to the unit with the VST3 `kIsProgramChange` flag) and sets it to the normalized
    /// value `program_index / max(1, program_count - 1)`, driving both the controller (for the
    /// editor/display) and the processor (for the audio DSP).
    ///
    /// Returns an error for an unknown unit, a unit with no program list, an out-of-range
    /// index, a plugin that doesn't implement `IUnitInfo`, or a plugin running under process
    /// isolation only if the helper cannot resolve the unit. Works both in-process and across
    /// the isolation boundary.
    pub fn select_program(&mut self, unit_id: i32, program_index: i32) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .select_program(unit_id, program_index)
    }

    /// The plugin's reported processing latency in samples (e.g. from look-ahead or
    /// oversampling), via `IAudioProcessor::getLatencySamples`. Use it to delay-compensate
    /// when aligning the plugin's output with other signals. `0` if it reports none, or for
    /// plugins running under process isolation (not bridged).
    pub fn latency_samples(&self) -> u32 {
        self.internal
            .as_ref()
            .map(|i| i.latency_samples())
            .unwrap_or(0)
    }

    /// The plugin's reported tail length in samples (how long it keeps producing output
    /// after input stops — e.g. reverb/delay), via `IAudioProcessor::getTailSamples`. `0`
    /// means no tail; `u32::MAX` means an infinite tail. `0` for isolated plugins (not
    /// bridged).
    pub fn tail_samples(&self) -> u32 {
        self.internal
            .as_ref()
            .map(|i| i.tail_samples())
            .unwrap_or(0)
    }

    /// Resolve a MIDI controller to the parameter it's mapped to, via the plugin's
    /// `IMidiMapping` (`getMidiControllerAssignment`).
    ///
    /// `bus` is the event input bus index (usually `0`), `channel` the 0-based MIDI channel,
    /// and `cc` the MIDI controller number (`0–127`, or the VST3 specials such as `128`
    /// aftertouch / `129` pitch-bend). Returns the parameter id the controller drives, or
    /// `None` if the plugin doesn't implement `IMidiMapping`, the controller is unmapped, or
    /// the plugin is process-isolated (not bridged).
    pub fn midi_cc_to_parameter(&self, bus: i32, channel: i16, cc: u16) -> Option<u32> {
        // VST3 controller numbers are 0..130 (0–127 MIDI CCs + the specials up to pitch-bend).
        // Reject out-of-range values rather than forwarding a meaningless controller number.
        if cc > 129 {
            return None;
        }
        self.internal
            .as_ref()?
            .midi_cc_to_parameter(bus, channel, cc)
    }

    /// Get a parameter value by ID
    pub fn get_parameter(&self, id: u32) -> Result<f64> {
        self.internal
            .as_ref()
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

    /// Schedule a MIDI event at a sample offset within the **next** [`process_audio`] block.
    ///
    /// Use this for sample-accurate sequencing: an event sent with `sample_offset = N` takes
    /// effect `N` frames into the next processed block, rather than at its start. Keep the
    /// offset within the upcoming block's frame count ([`Plugin::block_size`] is the maximum);
    /// a negative offset is treated as 0, and an offset past the block end is plugin-defined.
    ///
    /// Under process isolation the offset is not marshalled across the boundary — the event is
    /// delivered at block start (offset 0), same as [`Self::send_midi_event`].
    ///
    /// [`process_audio`]: Self::process_audio
    pub fn send_midi_event_at(&mut self, event: MidiEvent, sample_offset: i32) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .send_midi_event_at(event, sample_offset)
    }

    /// Start a note and get a per-voice [`NoteId`](crate::midi::NoteId) handle for sending
    /// per-note (MPE-style) expression to that exact voice via
    /// [`send_note_expression`](Self::send_note_expression).
    ///
    /// Unlike [`send_midi_note`](Self::send_midi_note) (which uses a shared note id and can't be
    /// individually expressed), this allocates a unique voice id. Pair it with
    /// [`note_off`](Self::note_off). Per-note expression works both in-process and under
    /// process isolation — the calls marshal across the boundary.
    pub fn note_on(
        &mut self,
        channel: MidiChannel,
        note: u8,
        velocity: u8,
    ) -> Result<crate::midi::NoteId> {
        self.note_on_at(channel, note, velocity, 0)
    }

    /// [`note_on`](Self::note_on) scheduled at a sample offset within the next block.
    pub fn note_on_at(
        &mut self,
        channel: MidiChannel,
        note: u8,
        velocity: u8,
        sample_offset: i32,
    ) -> Result<crate::midi::NoteId> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .note_on(channel, note, velocity, sample_offset)
    }

    /// Release a note started with [`note_on`](Self::note_on).
    pub fn note_off(&mut self, id: crate::midi::NoteId) -> Result<()> {
        self.note_off_at(id, 0)
    }

    /// [`note_off`](Self::note_off) scheduled at a sample offset within the next block.
    pub fn note_off_at(&mut self, id: crate::midi::NoteId, sample_offset: i32) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .note_off(id, sample_offset)
    }

    /// Send a per-note expression value for a voice (normalized `0.0..=1.0`; bipolar dimensions
    /// like [`Tuning`](crate::midi::NoteExpressionType::Tuning) center at `0.5`). The plugin
    /// must implement `INoteExpressionController` and the dimension must be one it advertises
    /// (see [`note_expressions`](Self::note_expressions)).
    pub fn send_note_expression(
        &mut self,
        id: crate::midi::NoteId,
        kind: crate::midi::NoteExpressionType,
        value: f64,
    ) -> Result<()> {
        self.send_note_expression_at(id, kind, value, 0)
    }

    /// [`send_note_expression`](Self::send_note_expression) scheduled at a sample offset.
    pub fn send_note_expression_at(
        &mut self,
        id: crate::midi::NoteId,
        kind: crate::midi::NoteExpressionType,
        value: f64,
        sample_offset: i32,
    ) -> Result<()> {
        if !(0.0..=1.0).contains(&value) {
            return Err(Error::InvalidParameter(format!(
                "note-expression value {value} out of range [0.0, 1.0]"
            )));
        }
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .send_note_expression(id, kind, value, sample_offset)
    }

    /// Enumerate the per-note expression dimensions the plugin advertises for the given event
    /// bus / channel (defaults: bus 0, channel 0), via `INoteExpressionController`. Empty if the
    /// plugin doesn't implement it.
    pub fn note_expressions(&self) -> Result<Vec<crate::midi::NoteExpressionInfo>> {
        self.internal
            .as_ref()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .note_expressions(0, 0)
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
    /// (e.g. each UI frame) while the plugin is playing; an empty vector means the plugin
    /// emitted nothing. This works for process-isolated plugins too — emitted events are
    /// marshalled back alongside each processed block.
    ///
    /// The buffer is capped at 4096 events: if you never poll while a chatty plugin keeps
    /// emitting, the oldest events are dropped (silently) to bound memory.
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
    /// Works both in-process and across process isolation (the state blob is marshalled over
    /// the IPC boundary). Returns an error for plugins that don't implement state saving.
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

    /// Save this plugin's state to a file as a [`PluginPreset`] (JSON: the plugin's `uid`
    /// and name plus the opaque state blob). The embedded `uid` lets [`Self::load_preset`]
    /// reject a preset saved from a different plugin.
    pub fn save_preset<P: AsRef<std::path::Path>>(&self, path: P) -> Result<()> {
        let info = self.info();
        let preset = PluginPreset {
            uid: info.uid.clone(),
            plugin_name: info.name.clone(),
            state: self.save_state()?,
        };
        let json = serde_json::to_vec_pretty(&preset)
            .map_err(|e| Error::Other(format!("serialize preset: {e}")))?;
        std::fs::write(path, json).map_err(|e| Error::Other(format!("write preset: {e}")))?;
        Ok(())
    }

    /// Load a [`PluginPreset`] file written by [`Self::save_preset`] and apply its state.
    /// Returns an error if the preset's `uid` doesn't match this plugin (loading another
    /// plugin's state is undefined).
    pub fn load_preset<P: AsRef<std::path::Path>>(&mut self, path: P) -> Result<()> {
        let bytes = std::fs::read(path).map_err(|e| Error::Other(format!("read preset: {e}")))?;
        let preset: PluginPreset = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Other(format!("parse preset: {e}")))?;
        if preset.uid != self.info().uid {
            return Err(Error::Other(format!(
                "preset is for a different plugin ({}, expected {})",
                preset.plugin_name,
                self.info().name
            )));
        }
        self.load_state(&preset.state)
    }

    /// Save this plugin's state to a standard Steinberg `.vstpreset` file.
    ///
    /// Unlike [`Self::save_preset`] (a JSON wrapper specific to this library), the
    /// `.vstpreset` container is the interchange format shared by VST3 hosts and plugins, so
    /// the file can be read by other hosts (and by the plugin's own preset browser). It wraps
    /// the same opaque bytes from [`Self::save_state`] in a single `"Comp"` (component state)
    /// chunk, tagged with this plugin's class id ([`PluginInfo::uid`]) so a loader can reject
    /// presets from a different plugin. Call this on the main thread.
    pub fn save_vstpreset<P: AsRef<std::path::Path>>(&self, path: P) -> Result<()> {
        let state = self.save_state()?;
        let bytes = vstpreset::build(&self.info().uid, &state)?;
        std::fs::write(path, bytes).map_err(|e| Error::Other(format!("write vstpreset: {e}")))?;
        Ok(())
    }

    /// Load a Steinberg `.vstpreset` file and apply its component state to this plugin.
    ///
    /// Parses the `.vstpreset` container written by [`Self::save_vstpreset`] (or another VST3
    /// host), extracts the `"Comp"` (component state) chunk and passes it to
    /// [`Self::load_state`]. Returns an error if the file's magic is invalid, or if its class
    /// id doesn't match this plugin (loading another plugin's state is undefined). Call this
    /// on the main thread.
    pub fn load_vstpreset<P: AsRef<std::path::Path>>(&mut self, path: P) -> Result<()> {
        let bytes =
            std::fs::read(path).map_err(|e| Error::Other(format!("read vstpreset: {e}")))?;
        let parsed = vstpreset::parse(&bytes)?;
        if parsed.class_id != self.info().uid {
            return Err(Error::Other(format!(
                "vstpreset is for a different plugin (class id {}, expected {})",
                parsed.class_id,
                self.info().uid
            )));
        }
        self.load_state(&parsed.component_state)
    }

    /// The OS process id of the isolated helper hosting this plugin, or `None` if it runs
    /// in-process. Useful for monitoring an isolated plugin's resource use.
    pub fn isolation_pid(&self) -> Option<u32> {
        self.internal.as_ref().and_then(|i| i.helper_pid())
    }

    /// How many times this plugin has been recovered (helper respawned + reloaded), via either
    /// [`Self::recover`] or automatic recovery ([`Vst3HostBuilder::auto_recover_plugins`]).
    ///
    /// A recovery reloads the plugin from defaults — parameter values and loaded state are NOT
    /// replayed. With auto-recover on, a crash is otherwise invisible (the call returns `Ok`),
    /// so poll this count to detect that a reset happened and re-apply a saved
    /// [`save_state`](Self::save_state) snapshot.
    ///
    /// [`Vst3HostBuilder::auto_recover_plugins`]: crate::Vst3HostBuilder::auto_recover_plugins
    pub fn recovery_count(&self) -> u64 {
        self.internal
            .as_ref()
            .map(|i| i.recovery_count())
            .unwrap_or(0)
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

/// Build and parse the standard Steinberg `.vstpreset` container format.
///
/// Layout (all multi-byte integers little-endian, matching the SDK's `PresetFile`):
///
/// - Header (48 bytes): magic `b"VST3"` (4) + version `i32` = 1 (4) + 32-char ASCII class
///   id (the plugin's FUID hex) (32) + `i64` byte offset from the start of the file to the
///   chunk list (8).
/// - Body: the chunk payloads, written back to back after the header. We write a single
///   `"Comp"` (component state) chunk.
/// - Chunk list (at the header's list offset): magic `b"List"` (4) + entry count `i32` (4),
///   then per entry: 4-byte chunk id + `i64` absolute offset + `i64` size.
mod vstpreset {
    use crate::error::{Error, Result};

    const MAGIC: &[u8; 4] = b"VST3";
    const LIST_MAGIC: &[u8; 4] = b"List";
    const COMPONENT_CHUNK: &[u8; 4] = b"Comp";
    const VERSION: i32 = 1;
    const CLASS_ID_LEN: usize = 32;
    const HEADER_SIZE: usize = 4 + 4 + CLASS_ID_LEN + 8;

    /// A parsed `.vstpreset` container.
    pub(super) struct Parsed {
        /// The 32-char ASCII class id from the header.
        pub class_id: String,
        /// The bytes of the `"Comp"` (component state) chunk.
        pub component_state: Vec<u8>,
    }

    /// Build a `.vstpreset` file wrapping `component_state` in a single component chunk,
    /// tagged with `class_id` (a 32-char ASCII FUID hex string).
    pub(super) fn build(class_id: &str, component_state: &[u8]) -> Result<Vec<u8>> {
        let class_bytes = class_id.as_bytes();
        if class_bytes.len() != CLASS_ID_LEN || !class_id.is_ascii() {
            return Err(Error::Other(format!(
                "vstpreset class id must be {CLASS_ID_LEN} ASCII chars, got {:?}",
                class_id
            )));
        }

        let comp_offset = HEADER_SIZE as i64;
        let comp_size = component_state.len() as i64;
        let list_offset = HEADER_SIZE + component_state.len();

        let mut out = Vec::with_capacity(list_offset + 8 + 24);
        // Header.
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(class_bytes);
        out.extend_from_slice(&(list_offset as i64).to_le_bytes());
        // Body.
        out.extend_from_slice(component_state);
        // Chunk list.
        out.extend_from_slice(LIST_MAGIC);
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(COMPONENT_CHUNK);
        out.extend_from_slice(&comp_offset.to_le_bytes());
        out.extend_from_slice(&comp_size.to_le_bytes());

        Ok(out)
    }

    /// Parse a `.vstpreset` file, extracting the class id and the component-state chunk.
    pub(super) fn parse(bytes: &[u8]) -> Result<Parsed> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Other("vstpreset too short for header".to_string()));
        }
        if &bytes[0..4] != MAGIC {
            return Err(Error::Other(format!(
                "bad vstpreset magic: expected {:?}, got {:?}",
                MAGIC,
                &bytes[0..4]
            )));
        }
        let version = read_i32(&bytes[4..8]);
        if version != VERSION {
            return Err(Error::Other(format!(
                "unsupported vstpreset version {version} (expected {VERSION})"
            )));
        }
        let class_id = String::from_utf8(bytes[8..8 + CLASS_ID_LEN].to_vec())
            .map_err(|e| Error::Other(format!("vstpreset class id not UTF-8: {e}")))?;
        let list_offset = read_i64(&bytes[8 + CLASS_ID_LEN..HEADER_SIZE]);
        if list_offset < HEADER_SIZE as i64 || list_offset as usize > bytes.len() {
            return Err(Error::Other(format!(
                "vstpreset chunk-list offset {list_offset} out of bounds (len {})",
                bytes.len()
            )));
        }
        let list = &bytes[list_offset as usize..];
        if list.len() < 8 || &list[0..4] != LIST_MAGIC {
            return Err(Error::Other(
                "vstpreset chunk list missing or malformed".to_string(),
            ));
        }
        let count = read_i32(&list[4..8]);
        if count < 0 {
            return Err(Error::Other("vstpreset negative entry count".to_string()));
        }
        let mut cursor = 8;
        for _ in 0..count {
            if list.len() < cursor + 20 {
                return Err(Error::Other(
                    "vstpreset chunk-list entry truncated".to_string(),
                ));
            }
            let id = &list[cursor..cursor + 4];
            let offset = read_i64(&list[cursor + 4..cursor + 12]);
            let size = read_i64(&list[cursor + 12..cursor + 20]);
            cursor += 20;
            if id == COMPONENT_CHUNK {
                if offset < 0 || size < 0 {
                    return Err(Error::Other(
                        "vstpreset component chunk has negative offset/size".to_string(),
                    ));
                }
                let start = offset as usize;
                let end = start
                    .checked_add(size as usize)
                    .ok_or_else(|| Error::Other("vstpreset chunk size overflow".to_string()))?;
                if end > bytes.len() {
                    return Err(Error::Other(format!(
                        "vstpreset component chunk [{start}..{end}] out of bounds (len {})",
                        bytes.len()
                    )));
                }
                return Ok(Parsed {
                    class_id,
                    component_state: bytes[start..end].to_vec(),
                });
            }
        }
        Err(Error::Other(
            "vstpreset has no component (\"Comp\") chunk".to_string(),
        ))
    }

    fn read_i32(b: &[u8]) -> i32 {
        i32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    fn read_i64(b: &[u8]) -> i64 {
        i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    }
}

#[cfg(test)]
mod vstpreset_tests {
    use super::vstpreset;

    const TEST_CLASS_ID: &str = "0123456789ABCDEF0123456789ABCDEF";

    #[test]
    fn build_parse_round_trip() {
        let state = b"opaque plugin state \x00\x01\x02\xff bytes".to_vec();
        let bytes = vstpreset::build(TEST_CLASS_ID, &state).expect("build");

        // Sanity-check the header layout.
        assert_eq!(&bytes[0..4], b"VST3");
        assert_eq!(
            i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            1
        );
        assert_eq!(&bytes[8..40], TEST_CLASS_ID.as_bytes());

        let parsed = vstpreset::parse(&bytes).expect("parse");
        assert_eq!(parsed.class_id, TEST_CLASS_ID);
        assert_eq!(parsed.component_state, state);
    }

    #[test]
    fn round_trip_empty_state() {
        let bytes = vstpreset::build(TEST_CLASS_ID, &[]).expect("build");
        let parsed = vstpreset::parse(&bytes).expect("parse");
        assert_eq!(parsed.class_id, TEST_CLASS_ID);
        assert!(parsed.component_state.is_empty());
    }

    #[test]
    fn build_rejects_wrong_length_class_id() {
        assert!(vstpreset::build("short", b"x").is_err());
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let mut bytes = vstpreset::build(TEST_CLASS_ID, b"x").expect("build");
        bytes[0] = b'X';
        assert!(vstpreset::parse(&bytes).is_err());
    }

    #[test]
    fn parse_rejects_truncated_header() {
        assert!(vstpreset::parse(b"VST3").is_err());
    }

    #[test]
    fn parse_rejects_out_of_bounds_list_offset() {
        let mut bytes = vstpreset::build(TEST_CLASS_ID, b"hello").expect("build");
        // Corrupt the list offset (bytes 40..48) to point past the end.
        let bad = (bytes.len() as i64 + 100).to_le_bytes();
        bytes[40..48].copy_from_slice(&bad);
        assert!(vstpreset::parse(&bytes).is_err());
    }
}
