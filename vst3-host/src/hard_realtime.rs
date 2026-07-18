//! Reversible, exclusively owned hard-realtime VST3 processing.
//!
//! [`Plugin::try_into_realtime`] is an off-audio-thread transition: it may stop the plugin,
//! allocate fixed storage, query parameter metadata, and lock legacy state. After [`RealtimePlugin`]
//! has been started, [`RealtimePlugin::process`] performs no host-side heap allocation,
//! deallocation, logging, or mutex locking. The hosted third-party plugin's own `process` method is
//! outside that guarantee.
//!
//! The transition is reversible. Pause the caller's audio callback, move the realtime owner to a
//! non-realtime thread, and call [`RealtimePlugin::try_into_plugin`] to restore the same ordinary
//! [`Plugin`] instance and synchronize its controller before reopening an editor.
//!
//! The public audio surface stays flat: active buses are concatenated in bus-index/channel order,
//! inactive buses are omitted, and a processor negotiated for 64-bit samples is converted through
//! preallocated storage. Plugin-emitted events and automation live only in the fixed output
//! collections for the current block; [`RealtimeProcessReport`] reports rejected output writes but
//! does not expose their payloads. Ordinary output drains therefore do not replay data emitted
//! while the plugin was sealed in realtime mode.

use crate::{audio::AudioBuffers, error::Error, midi::MidiEvent, plugin::Plugin, Result};

/// Fixed storage bounds prepared before a plugin enters hard-realtime mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealtimeCapacities {
    /// Largest audio block accepted by [`RealtimePlugin::process`].
    pub max_block_frames: usize,
    /// Largest number of incoming messages represented as VST3 events in one block.
    ///
    /// Notes and poly-aftertouch consume this capacity. CC, pitch bend, channel aftertouch, and
    /// program changes use their plugin-provided parameter mappings and consume parameter capacity
    /// instead; unmapped messages consume neither capacity.
    pub max_input_events: usize,
    /// Largest number of plugin-emitted events retained for one block.
    pub max_output_events: usize,
    /// Largest total number of incoming parameter points in one block and, independently, the
    /// largest number of plugin-written output points retained for that block.
    pub max_parameter_changes: usize,
    /// Largest number of distinct parameter ids changed in one block.
    pub max_distinct_parameters: usize,
}

impl RealtimeCapacities {
    fn validate(self, plugin_block_size: usize) -> Result<()> {
        if self.max_block_frames == 0 || self.max_block_frames > plugin_block_size {
            return Err(Error::Other(format!(
                "hard-realtime max block {} must be in 1..={plugin_block_size}",
                self.max_block_frames
            )));
        }
        if self.max_input_events == 0
            || self.max_output_events == 0
            || self.max_parameter_changes == 0
            || self.max_distinct_parameters == 0
            || self.max_distinct_parameters > self.max_parameter_changes
        {
            return Err(Error::Other(
                "hard-realtime event and parameter capacities must be nonzero, and distinct parameters must not exceed total points"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

/// One typed MIDI event scheduled within the next process block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RealtimeMidiEvent {
    /// Parsed VST3-host MIDI event preserving its original channel.
    pub event: MidiEvent,
    /// Zero-based sample offset, clamped to the processed block by the host.
    pub sample_offset: usize,
}

/// One normalized VST3 parameter point scheduled within the next process block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RealtimeParameterChange {
    /// VST3 parameter identifier queried before live mode.
    pub id: u32,
    /// Normalized value. Finite values are clamped to `0.0..=1.0`.
    pub value: f64,
    /// Zero-based sample offset, clamped to the processed block by the host.
    pub sample_offset: usize,
}

/// Allocation-free outcome counters for one successful process call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RealtimeProcessReport {
    /// MIDI messages delivered to the processor as VST3 events.
    pub input_events: usize,
    /// Parameter points delivered to the processor, including mapped channel controllers, pitch
    /// bend, channel aftertouch, and program changes.
    pub parameter_changes: usize,
    /// Distinct parameter queues visible to the processor.
    pub distinct_parameters: usize,
    /// Plugin output events rejected because its fixed output list was full.
    pub output_event_overflows: usize,
    /// Plugin output parameter points/queues rejected because fixed storage was full.
    pub output_parameter_overflows: usize,
}

/// Copy-only failures returned from the hard-realtime process path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RealtimeProcessError {
    /// The backend is process-isolated or does not implement the in-process realtime path.
    UnsupportedBackend,
    /// Processing has not been started.
    NotProcessing,
    /// A previous processor error faulted this owner.
    Faulted,
    /// Requested frames or caller buffer lengths exceed prepared storage.
    InvalidAudioBlock,
    /// Messages routed as VST3 events exceed `max_input_events`.
    InputEventCapacity,
    /// Parameter points exceed `max_parameter_changes`.
    ParameterPointCapacity,
    /// Distinct parameter ids exceed `max_distinct_parameters`.
    DistinctParameterCapacity,
    /// A parameter value was NaN or infinite.
    InvalidParameterValue,
    /// A MIDI data field was outside its 7-bit or 14-bit protocol range.
    InvalidMidiData,
    /// The plugin processor returned a non-success VST3 result code.
    ProcessorFailed(i32),
}

/// Result of synchronizing the controller while leaving live mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerSyncStatus {
    /// A separate controller accepted current component state, including its live parameters.
    StateAndParameters,
    /// Component-state transfer was unavailable; cached explicit parameter values were applied.
    ParametersOnly,
    /// The component and controller are the same object and already share the live state.
    SingleComponent,
    /// The backend has no controller synchronization support.
    Unsupported,
}

/// Nonfatal information returned when the same instance leaves live mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealtimeExitReport {
    /// How the ordinary controller was brought up to date.
    pub controller_sync: ControllerSyncStatus,
}

/// Failed forward transition that retains the original ordinary plugin for retry or teardown.
pub struct RealtimeTransitionFailure {
    plugin: Box<Plugin>,
    error: Error,
}

impl RealtimeTransitionFailure {
    /// Returns the transition error.
    pub fn error(&self) -> &Error {
        &self.error
    }

    /// Recovers the unchanged ordinary plugin and its error.
    pub fn into_parts(self) -> (Plugin, Error) {
        (*self.plugin, self.error)
    }
}

/// Failed reverse transition that retains the exclusive realtime owner.
pub struct RealtimeExitFailure {
    plugin: Box<RealtimePlugin>,
    error: Error,
}

impl RealtimeExitFailure {
    /// Returns the reverse-transition error.
    pub fn error(&self) -> &Error {
        &self.error
    }

    /// Recovers the realtime owner and its error for off-thread retry or teardown.
    pub fn into_parts(self) -> (RealtimePlugin, Error) {
        (*self.plugin, self.error)
    }
}

/// Exclusively owned in-process plugin whose host-side process path uses only fixed storage.
///
/// Process-isolated and other unsupported backends fail the forward transition and return the
/// original [`Plugin`] through [`RealtimeTransitionFailure`]. While sealed, allocation- or
/// lock-backed host callbacks are refused or acknowledged without recording; restart flags are
/// retained atomically for the restored control-thread API.
pub struct RealtimePlugin {
    plugin: Plugin,
    capacities: RealtimeCapacities,
    final_parameters: Vec<(u32, f64)>,
    faulted: bool,
}

impl Plugin {
    /// Converts this same in-process plugin instance into reversible hard-realtime mode.
    ///
    /// Call only after pausing any thread that was processing or editing `self`. The transition may
    /// allocate, stop processing, query parameters and MIDI mappings, and rebuild process
    /// structures. Controller-derived mappings are snapshotted for the complete realtime tenure;
    /// a plugin-requested mapping restart is retained but takes effect only after the plugin is
    /// restored and serviced on its control thread. On failure the returned
    /// [`RealtimeTransitionFailure`] owns the ordinary plugin so no instance is lost.
    pub fn try_into_realtime(
        mut self,
        capacities: RealtimeCapacities,
    ) -> std::result::Result<RealtimePlugin, RealtimeTransitionFailure> {
        if let Err(error) = capacities.validate(self.block_size) {
            return Err(RealtimeTransitionFailure {
                plugin: Box::new(self),
                error,
            });
        }
        if let Err(error) = self.stop_processing() {
            return Err(RealtimeTransitionFailure {
                plugin: Box::new(self),
                error,
            });
        }
        let mut final_parameters: Vec<(u32, f64)> = self
            .get_parameters()
            .unwrap_or_default()
            .into_iter()
            .map(|parameter| (parameter.id, parameter.value))
            .collect();
        final_parameters.sort_unstable_by_key(|(id, _)| *id);
        let result = self
            .internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))
            .and_then(|internal| internal.prepare_hard_realtime(capacities));
        match result {
            Ok(()) => Ok(RealtimePlugin {
                plugin: self,
                capacities,
                final_parameters,
                faulted: false,
            }),
            Err(error) => Err(RealtimeTransitionFailure {
                plugin: Box::new(self),
                error,
            }),
        }
    }
}

impl RealtimePlugin {
    /// Starts the plugin after the off-thread transition has prepared all fixed storage.
    pub fn start(&mut self) -> Result<()> {
        self.plugin.start_processing()?;
        self.faulted = false;
        Ok(())
    }

    /// Stops processing. This is a lifecycle operation and is not realtime-safe.
    pub fn stop(&mut self) -> Result<()> {
        self.plugin.stop_processing()
    }

    /// Processes one block without host-side allocation, deallocation, logging, or mutex locking.
    ///
    /// `buffers` must retain its channel allocations and contain channel lengths at least equal to
    /// `buffers.block_size`. Channels address the flattened active buses in bus-index/channel
    /// order; missing inputs are zero-filled and missing outputs are discarded. The maximum frame,
    /// event, and parameter limits are those supplied at transition, after controller-style MIDI
    /// has been translated to parameter points. A processor error faults this owner; subsequent
    /// calls return [`RealtimeProcessError::Faulted`] and clear the caller's output channels.
    pub fn process(
        &mut self,
        buffers: &mut AudioBuffers,
        midi: &[RealtimeMidiEvent],
        parameters: &[RealtimeParameterChange],
    ) -> std::result::Result<RealtimeProcessReport, RealtimeProcessError> {
        let clear_outputs = |buffers: &mut AudioBuffers| {
            for output in &mut buffers.outputs {
                output.fill(0.0);
            }
        };
        if self.faulted {
            clear_outputs(buffers);
            return Err(RealtimeProcessError::Faulted);
        }
        if !self.plugin.is_processing {
            clear_outputs(buffers);
            return Err(RealtimeProcessError::NotProcessing);
        }
        let frames = buffers.block_size;
        if frames == 0
            || frames > self.capacities.max_block_frames
            || buffers.inputs.iter().any(|channel| channel.len() < frames)
            || buffers.outputs.iter().any(|channel| channel.len() < frames)
        {
            clear_outputs(buffers);
            return Err(RealtimeProcessError::InvalidAudioBlock);
        }
        if parameters.len() > self.capacities.max_parameter_changes {
            clear_outputs(buffers);
            return Err(RealtimeProcessError::ParameterPointCapacity);
        }
        if parameters.iter().any(|change| !change.value.is_finite()) {
            clear_outputs(buffers);
            return Err(RealtimeProcessError::InvalidParameterValue);
        }
        if midi
            .iter()
            .any(|scheduled| !realtime_midi_data_is_valid(scheduled.event))
        {
            clear_outputs(buffers);
            return Err(RealtimeProcessError::InvalidMidiData);
        }
        let mut distinct = 0usize;
        for (index, parameter) in parameters.iter().enumerate() {
            if !parameters[..index]
                .iter()
                .any(|seen| seen.id == parameter.id)
            {
                distinct += 1;
            }
        }
        if distinct > self.capacities.max_distinct_parameters {
            clear_outputs(buffers);
            return Err(RealtimeProcessError::DistinctParameterCapacity);
        }
        let result = self
            .plugin
            .internal
            .as_mut()
            .ok_or(RealtimeProcessError::UnsupportedBackend)
            .and_then(|internal| internal.process_hard_realtime(buffers, midi, parameters));
        match result {
            Ok(report) => {
                for parameter in parameters {
                    if let Ok(index) = self
                        .final_parameters
                        .binary_search_by_key(&parameter.id, |(id, _)| *id)
                    {
                        self.final_parameters[index].1 = parameter.value.clamp(0.0, 1.0);
                    }
                }
                Ok(report)
            }
            Err(error) => {
                clear_outputs(buffers);
                if matches!(error, RealtimeProcessError::ProcessorFailed(_)) {
                    self.faulted = true;
                }
                Err(error)
            }
        }
    }

    /// Returns the fixed capacities prepared for this owner.
    pub const fn capacities(&self) -> RealtimeCapacities {
        self.capacities
    }

    /// Reverses live mode and returns the same ordinary plugin instance.
    ///
    /// Pause audio and move this value to a non-realtime thread first. This operation may stop the
    /// processor, allocate component-state streams, lock ordinary controller state, and free fixed
    /// realtime storage. Controller synchronization failures are represented in the successful
    /// report and do not prevent the editor-capable plugin from being returned. Output event and
    /// automation payloads produced while sealed are not replayed into the ordinary drains.
    pub fn try_into_plugin(
        mut self,
    ) -> std::result::Result<(Plugin, RealtimeExitReport), RealtimeExitFailure> {
        if let Err(error) = self.stop() {
            return Err(RealtimeExitFailure {
                plugin: Box::new(self),
                error,
            });
        }
        let leave = self
            .plugin
            .internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))
            .and_then(|internal| internal.leave_hard_realtime());
        if let Err(error) = leave {
            return Err(RealtimeExitFailure {
                plugin: Box::new(self),
                error,
            });
        }
        let controller_sync = self
            .plugin
            .internal
            .as_mut()
            .map(|internal| internal.sync_controller_after_realtime(&self.final_parameters))
            .unwrap_or(ControllerSyncStatus::Unsupported);
        Ok((self.plugin, RealtimeExitReport { controller_sync }))
    }
}

/// Validate every data field without formatting an error or allocating on the process thread.
fn realtime_midi_data_is_valid(event: MidiEvent) -> bool {
    match event {
        MidiEvent::NoteOn { note, velocity, .. } | MidiEvent::NoteOff { note, velocity, .. } => {
            note <= 127 && velocity <= 127
        }
        MidiEvent::ControlChange {
            controller, value, ..
        } => controller <= 127 && value <= 127,
        MidiEvent::ProgramChange { program, .. } => program <= 127,
        MidiEvent::PitchBend { value, .. } => value <= 16_383,
        MidiEvent::ChannelAftertouch { pressure, .. } => pressure <= 127,
        MidiEvent::PolyAftertouch { note, pressure, .. } => note <= 127 && pressure <= 127,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audio::AudioLevels,
        midi::MidiChannel,
        parameters::Parameter,
        plugin::{PluginInfo, PluginInternal},
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    struct FakeInternal {
        start_fails: bool,
        process_fails: bool,
        process_calls: Arc<AtomicUsize>,
    }

    impl PluginInternal for FakeInternal {
        fn set_parameter(&mut self, _id: u32, _value: f64) -> Result<()> {
            Ok(())
        }

        fn get_parameter(&self, _id: u32) -> Result<f64> {
            Ok(0.0)
        }

        fn get_all_parameters(&self) -> Result<Vec<Parameter>> {
            Ok(Vec::new())
        }

        fn format_parameter(&self, _id: u32, normalized: f64) -> Result<String> {
            Ok(normalized.to_string())
        }

        fn process(&mut self, _buffers: &mut AudioBuffers) -> Result<()> {
            Ok(())
        }

        fn prepare_hard_realtime(&mut self, _capacities: RealtimeCapacities) -> Result<()> {
            Ok(())
        }

        fn process_hard_realtime(
            &mut self,
            _buffers: &mut AudioBuffers,
            _midi: &[RealtimeMidiEvent],
            _parameters: &[RealtimeParameterChange],
        ) -> std::result::Result<RealtimeProcessReport, RealtimeProcessError> {
            self.process_calls.fetch_add(1, Ordering::Relaxed);
            if self.process_fails {
                Err(RealtimeProcessError::ProcessorFailed(-1))
            } else {
                Ok(RealtimeProcessReport::default())
            }
        }

        fn sync_controller_after_realtime(
            &mut self,
            _final_parameters: &[(u32, f64)],
        ) -> ControllerSyncStatus {
            ControllerSyncStatus::ParametersOnly
        }

        fn send_midi_event(&mut self, _event: MidiEvent) -> Result<()> {
            Ok(())
        }

        fn start_processing(&mut self) -> Result<()> {
            if self.start_fails {
                Err(Error::Other("synthetic startup failure".to_string()))
            } else {
                Ok(())
            }
        }

        fn stop_processing(&mut self) -> Result<()> {
            Ok(())
        }

        fn has_editor(&self) -> bool {
            false
        }

        fn open_editor(&mut self, _parent: *mut std::ffi::c_void) -> Result<()> {
            Err(Error::Other("no editor".to_string()))
        }

        fn close_editor(&mut self) -> Result<()> {
            Ok(())
        }

        fn get_editor_size(&self) -> Result<(i32, i32)> {
            Err(Error::Other("no editor".to_string()))
        }

        fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
            Vec::new()
        }
    }

    fn unloaded_plugin(processing: bool) -> Plugin {
        Plugin {
            info: PluginInfo {
                path: "/none.vst3".into(),
                name: "None".to_string(),
                vendor: String::new(),
                version: String::new(),
                category: String::new(),
                uid: "none".to_string(),
                audio_inputs: 0,
                audio_outputs: 2,
                has_midi_input: true,
                has_midi_output: false,
                has_gui: false,
            },
            compatibility: Vec::new(),
            is_processing: processing,
            sample_rate: 48_000.0,
            block_size: 64,
            audio_levels: Arc::new(Mutex::new(AudioLevels::new(2))),
            parameter_change_callback: None,
            audio_callback: None,
            internal: None::<Box<dyn PluginInternal>>,
        }
    }

    fn plugin_with_internal(internal: FakeInternal) -> Plugin {
        let mut plugin = unloaded_plugin(false);
        plugin.internal = Some(Box::new(internal));
        plugin
    }

    fn capacities() -> RealtimeCapacities {
        RealtimeCapacities {
            max_block_frames: 64,
            max_input_events: 8,
            max_output_events: 8,
            max_parameter_changes: 8,
            max_distinct_parameters: 4,
        }
    }

    /// A failed forward transition must return the same ordinary owner rather than dropping it.
    #[test]
    fn transition_failure_preserves_plugin_ownership() {
        let plugin = unloaded_plugin(false);
        let failure = match plugin.try_into_realtime(capacities()) {
            Err(failure) => failure,
            Ok(_) => panic!("an unloaded backend cannot enter realtime mode"),
        };
        let (plugin, error) = failure.into_parts();
        assert_eq!(plugin.info().uid, "none");
        assert!(matches!(error, Error::Other(_)));
    }

    /// Invalid protocol fields are rejected by a Copy-only error before any backend call, and
    /// every caller output is silenced on that error path.
    #[test]
    fn invalid_midi_data_is_rejected_and_clears_outputs() {
        let mut realtime = RealtimePlugin {
            plugin: unloaded_plugin(true),
            capacities: capacities(),
            final_parameters: Vec::new(),
            faulted: false,
        };
        let mut buffers = AudioBuffers::new(0, 2, 64, 48_000.0);
        for output in &mut buffers.outputs {
            output.fill(1.0);
        }
        let midi = [RealtimeMidiEvent {
            event: MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 255,
                velocity: 100,
            },
            sample_offset: 0,
        }];
        assert_eq!(
            realtime.process(&mut buffers, &midi, &[]),
            Err(RealtimeProcessError::InvalidMidiData)
        );
        assert!(buffers
            .outputs
            .iter()
            .flatten()
            .all(|sample| *sample == 0.0));
    }

    /// Startup belongs to the ownership transaction: a backend start error returns the ordinary
    /// plugin, with no half-started realtime value exposed to the caller.
    #[test]
    fn startup_failure_returns_the_ordinary_owner() {
        let calls = Arc::new(AtomicUsize::new(0));
        let plugin = plugin_with_internal(FakeInternal {
            start_fails: true,
            process_fails: false,
            process_calls: calls,
        });
        let failure = match plugin.try_into_realtime(capacities()) {
            Err(failure) => failure,
            Ok(_) => panic!("synthetic startup failure must abort the transition"),
        };
        let (plugin, error) = failure.into_parts();
        assert_eq!(plugin.info().uid, "none");
        assert!(!plugin.is_processing());
        assert!(matches!(error, Error::Other(message) if message == "synthetic startup failure"));
    }

    /// The same owner can stop, restart, process, and return to the ordinary surface with its
    /// controller synchronization result intact.
    #[test]
    fn lifecycle_is_reversible_on_the_same_plugin() {
        let calls = Arc::new(AtomicUsize::new(0));
        let plugin = plugin_with_internal(FakeInternal {
            start_fails: false,
            process_fails: false,
            process_calls: calls.clone(),
        });
        let mut realtime = match plugin.try_into_realtime(capacities()) {
            Ok(realtime) => realtime,
            Err(_) => panic!("fake backend supports realtime mode"),
        };
        assert!(realtime.plugin.is_processing());
        realtime.stop().unwrap();
        assert!(!realtime.plugin.is_processing());
        realtime.start().unwrap();
        let mut buffers = AudioBuffers::new(0, 2, 64, 48_000.0);
        realtime.process(&mut buffers, &[], &[]).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let (plugin, report) = match realtime.try_into_plugin() {
            Ok(result) => result,
            Err(_) => panic!("fake backend leaves realtime mode"),
        };
        assert_eq!(plugin.info().uid, "none");
        assert!(!plugin.is_processing());
        assert_eq!(report.controller_sync, ControllerSyncStatus::ParametersOnly);
    }

    /// A processor failure faults the owner once; later calls fail before entering the backend,
    /// and both paths silence every caller output.
    #[test]
    fn processor_failure_faults_owner_and_clears_outputs() {
        let calls = Arc::new(AtomicUsize::new(0));
        let plugin = plugin_with_internal(FakeInternal {
            start_fails: false,
            process_fails: true,
            process_calls: calls.clone(),
        });
        let mut realtime = match plugin.try_into_realtime(capacities()) {
            Ok(realtime) => realtime,
            Err(_) => panic!("fake backend supports realtime mode"),
        };
        let mut buffers = AudioBuffers::new(0, 2, 64, 48_000.0);
        for output in &mut buffers.outputs {
            output.fill(1.0);
        }
        assert_eq!(
            realtime.process(&mut buffers, &[], &[]),
            Err(RealtimeProcessError::ProcessorFailed(-1))
        );
        assert!(buffers
            .outputs
            .iter()
            .flatten()
            .all(|sample| *sample == 0.0));
        for output in &mut buffers.outputs {
            output.fill(1.0);
        }
        assert_eq!(
            realtime.process(&mut buffers, &[], &[]),
            Err(RealtimeProcessError::Faulted)
        );
        assert!(buffers
            .outputs
            .iter()
            .flatten()
            .all(|sample| *sample == 0.0));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
