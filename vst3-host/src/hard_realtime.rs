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
//! preallocated storage. Plugin output events and automation use the ordinary bounded output
//! queues. SysEx and text payloads are copied into reusable storage reserved during preparation;
//! the output consumer constructs owned payloads on its calling thread. [`RealtimeProcessReport`]
//! counts writes rejected by the fixed event, payload, and parameter capacities.
//!
//! MIDI 1 controller messages use every snapshotted input assignment, including one-to-many
//! Mapping 2 assignments. Program changes prefer channel mappings and otherwise use the root
//! unit's program list. The current tempo, playing state, and seek position carry into realtime
//! mode; processing advances the same transport, and the reverse transition preserves its endpoint.

use crate::{
    audio::AudioBuffers,
    error::Error,
    midi::MidiEvent,
    plugin::Plugin,
    realtime::{RealtimePluginRunner, RtControl},
    Result,
};

/// Fixed storage bounds prepared before a plugin enters hard-realtime mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealtimeCapacities {
    /// Largest audio block accepted by [`RealtimePlugin::process`].
    pub max_block_frames: usize,
    /// Largest number of raw MIDI messages accepted in one block.
    ///
    /// Every item passed in the `midi` slice consumes this bound, including controller messages
    /// that map to parameter points and unmapped messages that produce no VST3 input. Notes and
    /// poly-aftertouch also consume fixed VST3-event storage. Mapped controllers, pitch bend,
    /// channel aftertouch, and program changes additionally consume parameter-point capacity for
    /// each resolved mapping target.
    pub max_input_events: usize,
    /// Largest number of plugin-emitted events retained for one block.
    pub max_output_events: usize,
    /// Largest copyable payload retained for each plugin-emitted output event.
    ///
    /// A value of zero disables payload-bearing output events. The default runner configuration
    /// reserves 4096 bytes for each event. Storage covers both the current block and the public
    /// 4096-event output queue; each slot's payload allocation is rounded up to two-byte alignment.
    pub max_output_payload_bytes_per_event: usize,
    /// Largest total number of incoming parameter points in one block and, independently, the
    /// largest number of plugin-written output points retained for that block.
    pub max_parameter_changes: usize,
    /// Largest number of distinct parameter ids changed in one block.
    pub max_distinct_parameters: usize,
}

impl RealtimeCapacities {
    pub(crate) fn validate(self, plugin_block_size: usize) -> Result<()> {
        if self.max_block_frames == 0 || self.max_block_frames > plugin_block_size {
            return Err(Error::Other(format!(
                "hard-realtime max block {} must be in 1..={plugin_block_size}",
                self.max_block_frames
            )));
        }
        if self.max_output_payload_bytes_per_event > crate::midi::MAX_EVENT_PAYLOAD_BYTES {
            return Err(Error::Other(format!(
                "hard-realtime output payload capacity {} exceeds maximum {}",
                self.max_output_payload_bytes_per_event,
                crate::midi::MAX_EVENT_PAYLOAD_BYTES
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
    /// Plugin output events rejected because event storage was full, their payload exceeded its
    /// prepared byte budget, or the event/payload was unsupported or invalid.
    pub output_event_overflows: usize,
    /// Plugin output parameter points/queues rejected because fixed storage was full.
    pub output_parameter_overflows: usize,
}

/// Copy-only failures returned from the hard-realtime process path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RealtimeProcessError {
    /// The backend is process-isolated or does not implement the in-process realtime path.
    #[error("the plugin backend does not support prepared realtime processing")]
    UnsupportedBackend,
    /// Processing has not been started.
    #[error("the plugin is not processing")]
    NotProcessing,
    /// A previous processor error faulted this owner.
    #[error("the prepared realtime owner is faulted")]
    Faulted,
    /// Requested frames or caller buffer lengths exceed prepared storage.
    #[error("the audio block exceeds prepared realtime storage")]
    InvalidAudioBlock,
    /// Raw MIDI messages exceed `max_input_events`.
    #[error("realtime raw MIDI-message capacity was exceeded")]
    InputEventCapacity,
    /// Parameter points exceed `max_parameter_changes`.
    #[error("realtime parameter-point capacity was exceeded")]
    ParameterPointCapacity,
    /// Distinct parameter ids exceed `max_distinct_parameters`.
    #[error("realtime distinct-parameter capacity was exceeded")]
    DistinctParameterCapacity,
    /// A parameter value was NaN or infinite.
    #[error("a realtime parameter value was not finite")]
    InvalidParameterValue,
    /// A MIDI data field was outside its 7-bit or 14-bit protocol range.
    #[error("realtime MIDI data was invalid")]
    InvalidMidiData,
    /// The plugin processor returned a non-success VST3 result code.
    #[error("the plugin processor failed: {0:#x}")]
    ProcessorFailed(i32),
}

/// Result of synchronizing the controller while leaving live mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerSyncStatus {
    /// A separate controller accepted current component state, including its live parameters.
    StateAndParameters,
    /// Component-state transfer was unavailable; bounded parameter feedback updated the controller.
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

/// Compatibility wrapper around the canonical prepared [`RealtimePluginRunner`].
///
/// New hosts can use [`RealtimePluginRunner::start_with_capacities`] and
/// [`RealtimePluginRunner::process_direct`] directly. This type preserves the original
/// reversible ownership API.
pub struct RealtimePlugin {
    runner: RealtimePluginRunner,
    // This private handle owns the control-thread teardown receiver. It is intentionally kept
    // with the wrapper so an accidental audio-thread drop remains non-destructive.
    _control: RtControl,
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
        self,
        capacities: RealtimeCapacities,
    ) -> std::result::Result<RealtimePlugin, RealtimeTransitionFailure> {
        let (mut runner, control) =
            RealtimePluginRunner::new(self, capacities.max_parameter_changes);
        if let Err(error) = runner.start_with_capacities(capacities) {
            let plugin = runner.into_plugin();
            return Err(RealtimeTransitionFailure {
                plugin: Box::new(plugin),
                error,
            });
        }
        Ok(RealtimePlugin {
            runner,
            _control: control,
        })
    }
}

impl RealtimePlugin {
    /// Ensures processing is started after the off-thread transition.
    ///
    /// [`Plugin::try_into_realtime`] starts the processor transactionally before returning, so
    /// this is normally a no-op. It remains available for an off-thread retry after an explicit
    /// [`Self::stop`].
    pub fn start(&mut self) -> Result<()> {
        self.runner.start()
    }

    /// Stops processing. This is a lifecycle operation and is not realtime-safe.
    pub fn stop(&mut self) -> Result<()> {
        self.runner.stop()
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
        self.runner.process_direct(buffers, midi, parameters)
    }

    /// Returns the fixed capacities prepared for this owner.
    pub fn capacities(&self) -> RealtimeCapacities {
        self.runner
            .prepared_capacities()
            .expect("RealtimePlugin always owns a prepared runner")
    }

    /// Reverses live mode and returns the same ordinary plugin instance.
    ///
    /// Pause audio and move this value to a non-realtime thread first. This operation may stop the
    /// processor, allocate component-state streams, lock ordinary controller state, and free fixed
    /// realtime storage. Controller synchronization failures are represented in the successful
    /// report and do not prevent the editor-capable plugin from being returned. Output event and
    /// automation payloads produced while sealed are not replayed into the ordinary drains.
    pub fn try_into_plugin(
        self,
    ) -> std::result::Result<(Plugin, RealtimeExitReport), RealtimeExitFailure> {
        let Self { runner, _control } = self;
        match runner.try_restore_prepared() {
            Ok((plugin, controller_sync)) => Ok((plugin, RealtimeExitReport { controller_sync })),
            Err(error) => {
                let (runner, error) = *error;
                Err(RealtimeExitFailure {
                    plugin: Box::new(RealtimePlugin { runner, _control }),
                    error,
                })
            }
        }
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
            max_output_payload_bytes_per_event: 0,
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
        let calls = Arc::new(AtomicUsize::new(0));
        let plugin = plugin_with_internal(FakeInternal {
            start_fails: false,
            process_fails: false,
            process_calls: calls,
        });
        let mut realtime = match plugin.try_into_realtime(capacities()) {
            Ok(realtime) => realtime,
            Err(_) => panic!("fake backend supports prepared realtime"),
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

    /// The raw MIDI budget bounds caller work before routing. In particular, unmapped
    /// controllers cannot bypass the bound merely because they would not consume a VST3 event
    /// or a parameter point.
    #[test]
    fn raw_midi_capacity_rejects_unmapped_messages_before_processing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let plugin = plugin_with_internal(FakeInternal {
            start_fails: false,
            process_fails: false,
            process_calls: calls.clone(),
        });
        let mut capacities = capacities();
        capacities.max_input_events = 1;
        let mut realtime = match plugin.try_into_realtime(capacities) {
            Ok(realtime) => realtime,
            Err(_) => panic!("fake backend supports prepared realtime"),
        };
        let mut buffers = AudioBuffers::new(0, 2, 64, 48_000.0);
        for output in &mut buffers.outputs {
            output.fill(1.0);
        }
        let midi = [
            RealtimeMidiEvent {
                event: MidiEvent::ControlChange {
                    channel: MidiChannel::Ch1,
                    controller: 7,
                    value: 100,
                },
                sample_offset: 0,
            },
            RealtimeMidiEvent {
                event: MidiEvent::ControlChange {
                    channel: MidiChannel::Ch1,
                    controller: 7,
                    value: 99,
                },
                sample_offset: 1,
            },
        ];
        assert_eq!(
            realtime.process(&mut buffers, &midi, &[]),
            Err(RealtimeProcessError::InputEventCapacity)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(buffers
            .outputs
            .iter()
            .flatten()
            .all(|sample| *sample == 0.0));
    }

    #[test]
    fn output_payload_capacity_cannot_exceed_the_protocol_limit() {
        let mut capacities = capacities();
        capacities.max_output_payload_bytes_per_event = crate::midi::MAX_EVENT_PAYLOAD_BYTES + 1;
        assert!(matches!(capacities.validate(64), Err(Error::Other(_))));
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
        realtime.stop().unwrap();
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
