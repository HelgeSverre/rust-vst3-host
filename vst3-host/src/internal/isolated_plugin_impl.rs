//! Process-isolated plugin implementation
//!
//! This module provides a PluginInternal implementation that forwards all
//! operations to a plugin running in a separate process via IPC.

use crate::{
    audio::{AudioBuffers, AudioBusConfig, AudioBusLayout, BusAudioBuffers},
    error::{Error, Result},
    midi::{MidiEvent, PluginEvent},
    parameters::Parameter,
    plugin::{PluginInfo, PluginInternal, StateContext},
    process_isolation::{HostCommand, HostResponse, PluginHostProcess},
};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// Plugin implementation that communicates with an isolated process
pub struct IsolatedPluginImpl {
    /// The process managing the isolated plugin
    process: Mutex<PluginHostProcess>,
    /// Plugin information
    info: PluginInfo,
    /// Current sample rate (also used to reload after a crash)
    sample_rate: f64,
    /// Current block size
    block_size: usize,
    /// Current process mode (replayed on post-crash reload, which resets the helper's
    /// plugin to the `Realtime` default).
    process_mode: crate::plugin::ProcessMode,
    /// Transport tempo (BPM) advertised in the helper's host `ProcessContext`
    /// (also used to reload after a crash).
    tempo: f64,
    /// Time signature numerator advertised in the helper's host `ProcessContext`.
    time_sig_numerator: i32,
    /// Time signature denominator advertised in the helper's host `ProcessContext`.
    time_sig_denominator: i32,
    /// Whether the transport is playing in the helper's host `ProcessContext`. A freshly
    /// loaded plugin starts out playing (the in-process default), so only a stopped transport
    /// needs replaying after a crash.
    is_playing: bool,
    /// Whether the plugin is currently processing
    is_processing: bool,
    /// Whether the plugin has an open editor
    has_open_editor: bool,
    /// Editor size reported by the helper when the GUI was created (helper-owned window).
    editor_size: Option<(i32, i32)>,
    /// Total output audio channels (reported by the helper's introspection).
    output_channels: usize,
    /// MIDI the plugin has emitted across the boundary, buffered for the host to poll
    /// (mirrors PluginImpl::output_midi). Capped to bound growth if never read.
    output_events: Mutex<Vec<PluginEvent>>,
    /// Explicit helper-binary path override (re-used when respawning after a crash).
    helper_path: Option<PathBuf>,
    /// Per-command IPC response timeout (re-used when respawning after a crash).
    response_timeout: Duration,
    /// When true, a crashed/hung helper is transparently respawned+reloaded and the command
    /// retried (on the control plane only — never on the audio-thread `process()` path).
    auto_recover: bool,
    /// Max respawn+retry cycles per command when `auto_recover` is on.
    auto_recover_max_retries: u32,
    /// Count of successful recoveries (manual or automatic). Lets a caller detect that the
    /// plugin was respawned+reloaded (and thus reset to defaults) even when auto-recover
    /// swallowed the crash and returned `Ok`.
    recovery_count: std::sync::atomic::AtomicU64,
    /// Latched while [`Self::poll`] is failing, so a host polling every UI frame logs the
    /// helper's death once rather than once per frame. Cleared by the next successful poll.
    poll_error_logged: std::sync::atomic::AtomicBool,
}

/// Cap on buffered output MIDI, matching the in-process path's MAX_OUTPUT_MIDI.
const MAX_OUTPUT_MIDI: usize = 4096;

/// Pause between auto-recover attempts whose respawn+reload itself failed, so a helper that
/// cannot start at all is not hammered for the whole retry budget.
const RECOVERY_RETRY_BACKOFF: Duration = Duration::from_millis(250);

impl IsolatedPluginImpl {
    /// Create a new isolated plugin implementation
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        process: PluginHostProcess,
        info: PluginInfo,
        sample_rate: f64,
        block_size: usize,
        tempo: f64,
        time_sig_numerator: i32,
        time_sig_denominator: i32,
        output_channels: usize,
        helper_path: Option<PathBuf>,
        response_timeout: Duration,
        auto_recover: bool,
        auto_recover_max_retries: u32,
    ) -> Self {
        Self {
            process: Mutex::new(process),
            info,
            sample_rate,
            block_size,
            process_mode: crate::plugin::ProcessMode::Realtime,
            tempo,
            time_sig_numerator,
            time_sig_denominator,
            is_playing: true,
            is_processing: false,
            has_open_editor: false,
            editor_size: None,
            output_channels,
            output_events: Mutex::new(Vec::new()),
            helper_path,
            response_timeout,
            auto_recover,
            auto_recover_max_retries,
            recovery_count: std::sync::atomic::AtomicU64::new(0),
            poll_error_logged: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Send a command once, with NO recovery.
    ///
    /// Maps a dead/crashed/hung helper to a typed [`Error::PluginCrashed`] /
    /// [`Error::PluginTimeout`] (the host process stays alive). This is the path used by
    /// `process()` on the audio thread, where a synchronous respawn+reload would stall it for
    /// hundreds of milliseconds — so it never recovers inline.
    fn send_command_once(&self, command: HostCommand) -> Result<HostResponse> {
        let mut process = self
            .process
            .lock()
            .map_err(|e| Error::Other(format!("Failed to lock process: {}", e)))?;

        process
            .send_command(command)
            .map_err(|e| classify_ipc_error(&e))
    }

    /// Send a command, transparently respawning + reloading the helper and retrying on a
    /// crash/timeout when `auto_recover` is enabled (control-plane commands only).
    ///
    /// On its own (auto-recover off) this is just [`Self::send_command_once`]; the caller can
    /// still recover manually via [`PluginInternal::recover`].
    ///
    /// Recovery holds the process mutex across the whole respawn + reload, which takes as
    /// long as loading the plugin binary does (tens to hundreds of milliseconds, sometimes
    /// more). An audio callback calling `process()` concurrently blocks on that mutex for the
    /// duration and will glitch — recovery is a control-plane operation, and there is no way
    /// to swap the helper underneath a caller that is mid-command.
    fn send_command(&self, command: HostCommand) -> Result<HostResponse> {
        if !self.auto_recover {
            return self.send_command_once(command);
        }
        // A timed-out load or state command already cost a full (long) deadline and a SIGKILL;
        // replaying it against each fresh helper would just repeat that, so only crashes are
        // worth retrying for those.
        let slow = crate::process_isolation::is_slow_command(&command);
        let mut attempt: u32 = 0;
        loop {
            match self.send_command_once(command.clone()) {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    let recoverable = match e {
                        Error::PluginCrashed => true,
                        Error::PluginTimeout => !slow,
                        _ => false,
                    };
                    if !recoverable || attempt >= self.auto_recover_max_retries {
                        return Err(e);
                    }
                    attempt += 1;
                    log::warn!(
                        "isolated plugin crashed/hung ({e}); auto-recover attempt {attempt}/{}",
                        self.auto_recover_max_retries
                    );
                    if let Err(recovery_error) = self.recover_locked() {
                        // A respawn whose reload dies inside the plugin's own initialization
                        // is the same flake the original crash was, so it spends one of the
                        // caller's attempts rather than ending the loop outright. Only an
                        // exhausted budget gives up — and it reports the crash the caller
                        // actually hit, not the recovery's echo of it.
                        log::warn!(
                            "isolated plugin recovery attempt {attempt}/{} failed \
                             ({recovery_error})",
                            self.auto_recover_max_retries
                        );
                        if attempt >= self.auto_recover_max_retries {
                            return Err(e);
                        }
                        std::thread::sleep(RECOVERY_RETRY_BACKOFF);
                    }
                }
            }
        }
    }
}

/// Classify a low-level IPC error string into a typed library error.
fn classify_ipc_error(message: &str) -> Error {
    let lo = message.to_lowercase();
    if lo.contains("timed out") {
        Error::PluginTimeout
    } else if lo.contains("crash")
        || lo.contains("no longer running")
        || lo.contains("gone")
        || lo.contains("exited")
        || lo.contains("not running")
    {
        Error::PluginCrashed
    } else {
        Error::Other(format!("IPC error: {message}"))
    }
}

impl IsolatedPluginImpl {
    /// Expect a `Success` response, mapping anything else to an error.
    fn expect_success(&self, command: HostCommand, what: &str) -> Result<()> {
        match self.send_command(command)? {
            HostResponse::Success { .. } => Ok(()),
            HostResponse::Error { message } => Err(Error::Other(format!("{what}: {message}"))),
            _ => Err(Error::Other(format!("{what}: unexpected response"))),
        }
    }

    /// Run a poll command whose `PluginInternal` signature has no way to report failure.
    ///
    /// These accessors return "nothing to report" (an empty `Vec`, default flags) on the
    /// happy path, which is indistinguishable from a helper that just died — so the transport
    /// error is logged here instead of vanishing. It is logged **once per death**: the flag
    /// latches on the first failure and is cleared by the next successful poll, so a host
    /// polling every UI frame gets one line, not sixty a second.
    ///
    /// The death itself is not swallowed. [`PluginHostProcess`] latches its `dead` flag on the
    /// failing exchange and answers every later command with "Helper process is no longer
    /// running", which [`classify_ipc_error`] maps to [`Error::PluginCrashed`] — so the next
    /// fallible command (or [`PluginInternal::recover`]) still surfaces it. With auto-recover
    /// enabled, a poll is itself a control-plane command and will respawn+reload the helper.
    fn poll<T>(
        &self,
        command: HostCommand,
        what: &str,
        extract: impl FnOnce(HostResponse) -> Option<T>,
    ) -> Option<T> {
        let outcome = match self.send_command(command) {
            Ok(response) => extract(response).ok_or_else(|| "unexpected response".to_string()),
            Err(error) => Err(error.to_string()),
        };
        use std::sync::atomic::Ordering;
        match outcome {
            Ok(value) => {
                self.poll_error_logged.store(false, Ordering::Relaxed);
                Some(value)
            }
            Err(reason) => {
                if !self.poll_error_logged.swap(true, Ordering::Relaxed) {
                    log::warn!(
                        "isolated {what} reported nothing: {reason}. The helper is not \
                         answering; the next fallible command will surface the failure."
                    );
                }
                None
            }
        }
    }
}

impl PluginInternal for IsolatedPluginImpl {
    fn midi_controller_assignments(
        &self,
        direction: crate::audio::BusDirection,
    ) -> Result<Vec<crate::midi::MidiControllerAssignment>> {
        match self.send_command(HostCommand::MidiControllerAssignments { direction })? {
            HostResponse::MidiControllerAssignments { assignments } => Ok(assignments),
            HostResponse::Error { message } => Err(Error::Other(message)),
            _ => Err(Error::Other(
                "unexpected MidiControllerAssignments response".into(),
            )),
        }
    }
    fn send_midi_controller_at(
        &mut self,
        bus: i32,
        channel: crate::midi::MidiChannel,
        controller: crate::midi::MidiController,
        value: f64,
        sample_offset: i32,
    ) -> Result<()> {
        self.expect_success(
            HostCommand::SendMidiControllerAt {
                bus,
                channel,
                controller,
                value,
                sample_offset,
            },
            "SendMidiControllerAt",
        )
    }
    fn notify_live_midi_controller(
        &mut self,
        bus: i32,
        channel: crate::midi::MidiChannel,
        controller: crate::midi::MidiController,
    ) -> Result<bool> {
        match self.send_command(HostCommand::NotifyLiveMidiController {
            bus,
            channel,
            controller,
        })? {
            HostResponse::NotifyLiveMidiController { accepted } => Ok(accepted),
            HostResponse::Error { message } => Err(Error::Other(message)),
            _ => Err(Error::Other(
                "unexpected NotifyLiveMidiController response".into(),
            )),
        }
    }
    fn send_live_midi_event(&mut self, event: crate::midi::MidiEvent) -> Result<()> {
        self.expect_success(
            HostCommand::SendLiveMidiEvent { event },
            "SendLiveMidiEvent",
        )
    }
    fn keyswitches(
        &self,
        bus: i32,
        channel: crate::midi::MidiChannel,
    ) -> Result<Vec<crate::midi::KeyswitchInfo>> {
        match self.send_command(HostCommand::Keyswitches { bus, channel })? {
            HostResponse::Keyswitches { keyswitches } => Ok(keyswitches),
            HostResponse::Error { message } => Err(Error::Other(message)),
            _ => Err(Error::Other("unexpected Keyswitches response".into())),
        }
    }

    fn set_transport_position(&mut self, position: crate::plugin::TransportPosition) -> Result<()> {
        self.expect_success(
            HostCommand::SetTransportPosition { position },
            "SetTransportPosition",
        )
    }
    fn transport_position(&self) -> Result<crate::plugin::TransportPosition> {
        self.poll(
            HostCommand::TransportPosition,
            "TransportPosition",
            |response| match response {
                HostResponse::TransportPosition { position } => Some(position),
                _ => None,
            },
        )
        .ok_or_else(|| Error::Other("TransportPosition: unavailable".into()))
    }

    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        self.expect_success(HostCommand::SetParameter { id, value }, "SetParameter")
    }

    fn set_parameter_at(&mut self, id: u32, value: f64, sample_offset: i32) -> Result<()> {
        self.expect_success(
            HostCommand::SetParameterAt {
                id,
                value,
                offset: sample_offset,
            },
            "SetParameterAt",
        )
    }

    fn set_tempo(&mut self, bpm: f64) -> Result<()> {
        self.expect_success(HostCommand::SetTempo { bpm }, "SetTempo")?;
        // Track the accepted transport state so a post-crash reload replays it instead of the
        // load-time settings. Only on success, so a rejected change can't desync the copy.
        self.tempo = bpm;
        Ok(())
    }

    fn set_time_signature(&mut self, numerator: i32, denominator: i32) -> Result<()> {
        self.expect_success(
            HostCommand::SetTimeSignature {
                numerator,
                denominator,
            },
            "SetTimeSignature",
        )?;
        self.time_sig_numerator = numerator;
        self.time_sig_denominator = denominator;
        Ok(())
    }

    fn set_playing(&mut self, playing: bool) -> Result<()> {
        self.expect_success(HostCommand::SetPlaying { playing }, "SetPlaying")?;
        self.is_playing = playing;
        Ok(())
    }

    fn get_parameter(&self, id: u32) -> Result<f64> {
        match self.send_command(HostCommand::GetParameter { id })? {
            HostResponse::ParameterValue { value } => Ok(value),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("GetParameter: {message}")))
            }
            _ => Err(Error::Other(
                "GetParameter: unexpected response".to_string(),
            )),
        }
    }

    fn get_all_parameters(&self) -> Result<Vec<Parameter>> {
        match self.send_command(HostCommand::GetAllParameters)? {
            HostResponse::Parameters { params } => Ok(params),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("GetAllParameters: {message}")))
            }
            _ => Err(Error::Other(
                "GetAllParameters: unexpected response".to_string(),
            )),
        }
    }

    fn format_parameter(&self, id: u32, normalized: f64) -> Result<String> {
        match self.send_command(HostCommand::FormatParameter { id, normalized })? {
            HostResponse::ParameterString { value } => Ok(value),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("FormatParameter: {message}")))
            }
            _ => Err(Error::Other(
                "FormatParameter: unexpected response".to_string(),
            )),
        }
    }

    fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        let frames = buffers
            .outputs
            .first()
            .map(|c| c.len())
            .unwrap_or(self.block_size);

        // Audio-thread path: never auto-recover inline (a respawn would stall the callback).
        let response = self.send_command_once(HostCommand::Process {
            inputs: buffers.inputs.clone(),
            frames: frames as u32,
        })?;

        match response {
            HostResponse::AudioOutput {
                outputs,
                output_events,
            } => {
                for (ch_idx, output_channel) in buffers.outputs.iter_mut().enumerate() {
                    if let Some(src) = outputs.get(ch_idx) {
                        let n = output_channel.len().min(src.len());
                        output_channel[..n].copy_from_slice(&src[..n]);
                        for s in &mut output_channel[n..] {
                            *s = 0.0;
                        }
                    } else {
                        output_channel.fill(0.0);
                    }
                }
                // Buffer any MIDI the plugin emitted this block for the host to poll.
                if !output_events.is_empty() {
                    if let Ok(mut buf) = self.output_events.lock() {
                        buf.extend(output_events);
                        if buf.len() > MAX_OUTPUT_MIDI {
                            let drop = buf.len() - MAX_OUTPUT_MIDI;
                            buf.drain(0..drop);
                        }
                    }
                }
                Ok(())
            }
            HostResponse::Error { message } => {
                Err(Error::ProcessError(format!("Process error: {}", message)))
            }
            _ => Err(Error::Other(
                "Unexpected response from process command".to_string(),
            )),
        }
    }

    fn audio_bus_layout(&self) -> Result<AudioBusLayout> {
        match self.send_command(HostCommand::AudioBusLayout)? {
            HostResponse::AudioBusLayout { layout } => Ok(layout),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("AudioBusLayout: {message}")))
            }
            _ => Err(Error::Other(
                "AudioBusLayout: unexpected response".to_string(),
            )),
        }
    }

    fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> Result<()> {
        let frames = buffers
            .outputs
            .iter()
            .chain(&buffers.inputs)
            .flat_map(|bus| &bus.channels)
            .map(Vec::len)
            .next()
            .unwrap_or(buffers.block_size);
        let outputs = buffers
            .outputs
            .iter()
            .map(|bus| AudioBusConfig {
                channel_count: bus.channels.len(),
                active: bus.active,
            })
            .collect();
        let response = self.send_command_once(HostCommand::ProcessBuses {
            inputs: buffers.inputs.clone(),
            outputs,
            frames: frames as u32,
        })?;
        match response {
            HostResponse::BusAudioOutput {
                outputs,
                output_events,
            } => {
                for (destination_bus, source_bus) in buffers.outputs.iter_mut().zip(&outputs) {
                    for (destination, source) in destination_bus
                        .channels
                        .iter_mut()
                        .zip(&source_bus.channels)
                    {
                        let count = destination.len().min(source.len());
                        destination[..count].copy_from_slice(&source[..count]);
                        destination[count..].fill(0.0);
                    }
                }
                for destination_bus in buffers.outputs.iter_mut().skip(outputs.len()) {
                    for destination in &mut destination_bus.channels {
                        destination.fill(0.0);
                    }
                }
                if !output_events.is_empty() {
                    if let Ok(mut queued) = self.output_events.lock() {
                        queued.extend(output_events);
                        if queued.len() > MAX_OUTPUT_MIDI {
                            let drop_count = queued.len() - MAX_OUTPUT_MIDI;
                            queued.drain(0..drop_count);
                        }
                    }
                }
                Ok(())
            }
            HostResponse::Error { message } => Err(Error::ProcessError(format!(
                "ProcessBuses error: {message}"
            ))),
            _ => Err(Error::Other(
                "ProcessBuses: unexpected response".to_string(),
            )),
        }
    }

    fn send_midi_event(&mut self, event: MidiEvent) -> Result<()> {
        self.expect_success(HostCommand::SendMidi { event }, "SendMidi")
    }

    fn send_midi_event_at(&mut self, event: MidiEvent, sample_offset: i32) -> Result<()> {
        self.expect_success(
            HostCommand::SendMidiAt {
                event,
                sample_offset,
            },
            "SendMidiAt",
        )
    }

    fn send_plugin_event(&mut self, event: PluginEvent) -> Result<()> {
        self.expect_success(HostCommand::SendPluginEvent { event }, "SendPluginEvent")
    }

    fn midi_panic(&mut self) -> Result<()> {
        self.expect_success(HostCommand::MidiPanic, "MidiPanic")
    }

    fn set_bus_active(
        &mut self,
        media_type: crate::audio::MediaType,
        direction: crate::audio::BusDirection,
        bus_index: i32,
        active: bool,
    ) -> Result<()> {
        self.expect_success(
            HostCommand::SetBusActive {
                media_type,
                direction,
                bus_index,
                active,
            },
            "SetBusActive",
        )
    }

    fn bus_arrangements(&self) -> Result<crate::audio::BusArrangements> {
        match self.send_command(HostCommand::BusArrangements)? {
            HostResponse::BusArrangements { arrangements } => Ok(arrangements),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("BusArrangements: {message}")))
            }
            _ => Err(Error::Other(
                "BusArrangements: unexpected response".to_string(),
            )),
        }
    }

    fn set_bus_arrangements(
        &mut self,
        inputs: &[crate::audio::SpeakerArrangement],
        outputs: &[crate::audio::SpeakerArrangement],
    ) -> Result<()> {
        self.expect_success(
            HostCommand::SetBusArrangements {
                inputs: inputs.to_vec(),
                outputs: outputs.to_vec(),
            },
            "SetBusArrangements",
        )?;
        // The negotiated layout may have changed the channel count: refresh the cached
        // total from what the helper's plugin actually applied (it may decline a request
        // and keep its own arrangement), so output_channel_count() stays live like the
        // in-process implementation's getBusInfo query.
        if let Ok(HostResponse::BusArrangements { arrangements }) =
            self.send_command(HostCommand::BusArrangements)
        {
            self.output_channels = arrangements.outputs.iter().map(|a| a.channel_count()).sum();
        }
        Ok(())
    }

    fn note_on(
        &mut self,
        channel: crate::midi::MidiChannel,
        note: u8,
        velocity: u8,
        sample_offset: i32,
    ) -> Result<crate::midi::NoteId> {
        // The helper owns the real plugin, so it allocates the NoteId; we wrap the raw id back.
        match self.send_command(HostCommand::NoteOn {
            channel: channel.as_index(),
            note,
            velocity,
            sample_offset,
        })? {
            HostResponse::NoteStarted { note_id } => Ok(crate::midi::NoteId(note_id)),
            HostResponse::Error { message } => Err(Error::Other(format!("NoteOn: {message}"))),
            _ => Err(Error::Other("NoteOn: unexpected response".to_string())),
        }
    }

    fn note_off(&mut self, id: crate::midi::NoteId, sample_offset: i32) -> Result<()> {
        self.expect_success(
            HostCommand::NoteOff {
                note_id: id.raw(),
                sample_offset,
            },
            "NoteOff",
        )
    }

    fn send_note_expression(
        &mut self,
        id: crate::midi::NoteId,
        kind: crate::midi::NoteExpressionType,
        value: f64,
        sample_offset: i32,
    ) -> Result<()> {
        self.expect_success(
            HostCommand::SendNoteExpression {
                note_id: id.raw(),
                kind,
                value,
                sample_offset,
            },
            "SendNoteExpression",
        )
    }

    fn note_expressions(
        &self,
        bus: i32,
        channel: i16,
    ) -> Result<Vec<crate::midi::NoteExpressionInfo>> {
        match self.send_command(HostCommand::NoteExpressions { bus, channel })? {
            HostResponse::NoteExpressions { expressions } => Ok(expressions),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("NoteExpressions: {message}")))
            }
            _ => Err(Error::Other(
                "NoteExpressions: unexpected response".to_string(),
            )),
        }
    }

    fn select_program(&mut self, unit_id: i32, program_index: i32) -> Result<()> {
        self.expect_success(
            HostCommand::SelectProgram {
                unit_id,
                program_index,
            },
            "SelectProgram",
        )
    }

    fn get_units(&self) -> Result<Vec<crate::plugin::PluginUnit>> {
        match self.send_command(HostCommand::GetUnits)? {
            HostResponse::Units { units } => Ok(units),
            HostResponse::Error { message } => Err(Error::Other(format!("GetUnits: {message}"))),
            _ => Err(Error::Other("GetUnits: unexpected response".to_string())),
        }
    }

    fn selected_unit(&self) -> Result<Option<i32>> {
        match self.send_command(HostCommand::GetSelectedUnit)? {
            HostResponse::SelectedUnit { unit_id } => Ok(unit_id),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("GetSelectedUnit: {message}")))
            }
            _ => Err(Error::Other(
                "GetSelectedUnit: unexpected response".to_string(),
            )),
        }
    }

    fn select_unit(&mut self, unit_id: i32) -> Result<()> {
        self.expect_success(HostCommand::SelectUnit { unit_id }, "SelectUnit")
    }

    fn program_pitch_names(
        &self,
        program_list_id: i32,
        program_index: i32,
    ) -> Result<Vec<crate::plugin::ProgramPitchName>> {
        match self.send_command(HostCommand::ProgramPitchNames {
            program_list_id,
            program_index,
        })? {
            HostResponse::ProgramPitchNames { names } => Ok(names),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("ProgramPitchNames: {message}")))
            }
            _ => Err(Error::Other(
                "ProgramPitchNames: unexpected response".to_string(),
            )),
        }
    }

    fn get_program_data(
        &self,
        program_list_id: i32,
        program_index: i32,
    ) -> Result<Option<Vec<u8>>> {
        match self.send_command(HostCommand::GetProgramData {
            program_list_id,
            program_index,
        })? {
            HostResponse::OpaqueData { supported, data } => Ok(supported.then_some(data)),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("GetProgramData: {message}")))
            }
            _ => Err(Error::Other(
                "GetProgramData: unexpected response".to_string(),
            )),
        }
    }

    fn set_program_data(
        &mut self,
        program_list_id: i32,
        program_index: i32,
        data: &[u8],
    ) -> Result<()> {
        self.expect_success(
            HostCommand::SetProgramData {
                program_list_id,
                program_index,
                data: data.to_vec(),
            },
            "SetProgramData",
        )
    }

    fn get_unit_data(&self, unit_id: i32) -> Result<Option<Vec<u8>>> {
        match self.send_command(HostCommand::GetUnitData { unit_id })? {
            HostResponse::OpaqueData { supported, data } => Ok(supported.then_some(data)),
            HostResponse::Error { message } => Err(Error::Other(format!("GetUnitData: {message}"))),
            _ => Err(Error::Other("GetUnitData: unexpected response".to_string())),
        }
    }

    fn set_unit_data(&mut self, unit_id: i32, data: &[u8]) -> Result<()> {
        self.expect_success(
            HostCommand::SetUnitData {
                unit_id,
                data: data.to_vec(),
            },
            "SetUnitData",
        )
    }

    fn begin_host_edit(&mut self, parameter_id: u32) -> Result<()> {
        self.expect_success(HostCommand::BeginHostEdit { parameter_id }, "BeginHostEdit")
    }

    fn end_host_edit(&mut self, parameter_id: u32) -> Result<()> {
        self.expect_success(HostCommand::EndHostEdit { parameter_id }, "EndHostEdit")
    }

    fn send_midi_learn(&mut self, bus: i32, channel: i16, controller: u16) -> Result<()> {
        self.expect_success(
            HostCommand::SendMidiLearn {
                bus,
                channel,
                controller,
            },
            "SendMidiLearn",
        )
    }

    fn set_automation_state(&mut self, state: crate::plugin::AutomationState) -> Result<()> {
        self.expect_success(
            HostCommand::SetAutomationState { state },
            "SetAutomationState",
        )
    }

    fn remap_parameter_id(&self, old_plugin_uid: &str, old_param_id: u32) -> Result<Option<u32>> {
        if crate::internal::utils::parse_class_uid(old_plugin_uid).is_none() {
            return Err(Error::InvalidParameter(
                "plugin UID must contain exactly 32 hexadecimal characters".to_string(),
            ));
        }
        match self.send_command(HostCommand::RemapParameterId {
            old_plugin_uid: old_plugin_uid.to_string(),
            old_param_id,
        })? {
            HostResponse::RemappedParameter { id } => Ok(id),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("RemapParameterId: {message}")))
            }
            _ => Err(Error::Other(
                "RemapParameterId: unexpected response".to_string(),
            )),
        }
    }

    fn latency_samples(&self) -> u32 {
        self.poll(
            HostCommand::LatencySamples,
            "LatencySamples",
            |response| match response {
                HostResponse::LatencySamples { samples } => Some(samples),
                _ => None,
            },
        )
        .unwrap_or(0)
    }

    fn tail_samples(&self) -> u32 {
        self.poll(
            HostCommand::TailSamples,
            "TailSamples",
            |response| match response {
                HostResponse::TailSamples { samples } => Some(samples),
                _ => None,
            },
        )
        .unwrap_or(0)
    }

    fn midi_cc_to_parameter(&self, bus: i32, channel: i16, cc: u16) -> Option<u32> {
        // The inner `Option` is the plugin's own answer ("this controller is not mapped");
        // the outer one is whether the helper answered at all.
        self.poll(
            HostCommand::MidiCcToParameter { bus, channel, cc },
            "MidiCcToParameter",
            |response| match response {
                HostResponse::MidiParameterMapping { id } => Some(id),
                _ => None,
            },
        )
        .flatten()
    }

    fn start_processing(&mut self) -> Result<()> {
        self.expect_success(HostCommand::StartProcessing, "StartProcessing")?;
        self.is_processing = true;
        Ok(())
    }

    fn stop_processing(&mut self) -> Result<()> {
        self.expect_success(HostCommand::StopProcessing, "StopProcessing")?;
        self.is_processing = false;
        Ok(())
    }

    fn reconfigure(&mut self, sample_rate: f64, block_size: usize) -> Result<()> {
        self.expect_success(
            HostCommand::Reconfigure {
                sample_rate,
                block_size: block_size as u32,
            },
            "Reconfigure",
        )?;
        // Track the new config so a post-crash reload uses it.
        self.sample_rate = sample_rate;
        self.block_size = block_size;
        Ok(())
    }

    fn set_process_mode(&mut self, mode: crate::plugin::ProcessMode) -> Result<()> {
        self.expect_success(HostCommand::SetProcessMode { mode }, "SetProcessMode")?;
        // Track the mode so a post-crash reload restores it.
        self.process_mode = mode;
        Ok(())
    }

    fn has_editor(&self) -> bool {
        self.info.has_gui
    }

    fn open_editor(&mut self, _parent: *mut std::ffi::c_void) -> Result<()> {
        let response = self.send_command(HostCommand::CreateGui)?;

        match response {
            // The helper owns the window and reports its real size.
            HostResponse::GuiCreated { width, height } => {
                self.editor_size = Some((width, height));
                self.has_open_editor = true;
                Ok(())
            }
            HostResponse::Success { .. } => {
                self.has_open_editor = true;
                Ok(())
            }
            HostResponse::Error { message } => {
                Err(Error::Other(format!("Failed to open editor: {}", message)))
            }
            _ => Err(Error::Other(
                "Unexpected response from CreateGui command".to_string(),
            )),
        }
    }

    fn close_editor(&mut self) -> Result<()> {
        if !self.has_open_editor {
            return Ok(());
        }

        let response = self.send_command(HostCommand::CloseGui)?;

        match response {
            HostResponse::Success { .. } => {
                self.has_open_editor = false;
                Ok(())
            }
            HostResponse::Error { message } => {
                Err(Error::Other(format!("Failed to close editor: {}", message)))
            }
            _ => Err(Error::Other(
                "Unexpected response from CloseGui command".to_string(),
            )),
        }
    }

    fn get_editor_size(&self) -> Result<(i32, i32)> {
        // The real size is learned when the helper creates the editor (GuiCreated);
        // fall back to a sensible default before the GUI has been opened.
        Ok(self.editor_size.unwrap_or((800, 600)))
    }

    fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
        self.poll(
            HostCommand::TakeParameterChanges,
            "TakeParameterChanges",
            |response| match response {
                HostResponse::ParameterChanges { changes } => Some(changes),
                _ => None,
            },
        )
        .unwrap_or_default()
    }

    fn take_parameter_edits(&mut self) -> Vec<crate::plugin::ParameterEdit> {
        // Pulled on demand across the boundary, like the value-change drain: the helper's
        // in-process plugin accumulates gestures from its editor and hands them back here.
        self.poll(
            HostCommand::TakeParameterEdits,
            "TakeParameterEdits",
            |response| match response {
                HostResponse::ParameterEdits { edits } => Some(edits),
                _ => None,
            },
        )
        .unwrap_or_default()
    }

    fn take_host_notifications(&mut self) -> Vec<crate::plugin::HostNotification> {
        self.poll(
            HostCommand::TakeHostNotifications,
            "TakeHostNotifications",
            |response| match response {
                HostResponse::HostNotifications { notifications } => Some(notifications),
                _ => None,
            },
        )
        .unwrap_or_default()
    }

    fn take_data_exchange_blocks(&mut self) -> Vec<crate::plugin::DataExchangeBlock> {
        self.poll(
            HostCommand::TakeDataExchangeBlocks,
            "TakeDataExchangeBlocks",
            |response| match response {
                HostResponse::DataExchangeBlocks { blocks } => Some(blocks),
                _ => None,
            },
        )
        .unwrap_or_default()
    }

    fn execute_context_menu_item(&mut self, menu_id: u64, item_id: u32) -> Result<()> {
        match self.send_command(HostCommand::ExecuteContextMenuItem { menu_id, item_id })? {
            HostResponse::Success { .. } => Ok(()),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("ExecuteContextMenuItem: {message}")))
            }
            _ => Err(Error::Other(
                "ExecuteContextMenuItem: unexpected response".to_string(),
            )),
        }
    }

    fn dismiss_context_menu(&mut self, menu_id: u64) -> Result<()> {
        match self.send_command(HostCommand::DismissContextMenu { menu_id })? {
            HostResponse::Success { .. } => Ok(()),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("DismissContextMenu: {message}")))
            }
            _ => Err(Error::Other(
                "DismissContextMenu: unexpected response".to_string(),
            )),
        }
    }

    fn take_restart_flags(&mut self) -> crate::plugin::RestartFlags {
        self.poll(
            HostCommand::TakeRestartFlags,
            "TakeRestartFlags",
            |response| match response {
                HostResponse::RestartFlags { bits } => {
                    Some(crate::plugin::RestartFlags::from_bits(bits))
                }
                _ => None,
            },
        )
        .unwrap_or_default()
    }

    fn service_host_requests(&mut self) -> Result<crate::plugin::RestartFlags> {
        match self.send_command(HostCommand::ServiceHostRequests)? {
            HostResponse::RestartFlags { bits } => Ok(crate::plugin::RestartFlags::from_bits(bits)),
            HostResponse::Error { message } => {
                Err(Error::Other(format!("ServiceHostRequests: {message}")))
            }
            _ => Err(Error::Other(
                "ServiceHostRequests: unexpected response".to_string(),
            )),
        }
    }

    fn save_state(&self) -> Result<Vec<u8>> {
        match self.send_command(HostCommand::SaveState)? {
            HostResponse::State { data } => Ok(data),
            HostResponse::Error { message } => Err(Error::Other(format!("SaveState: {message}"))),
            _ => Err(Error::Other("SaveState: unexpected response".to_string())),
        }
    }

    fn load_state_with_context(&mut self, data: &[u8], context: &StateContext) -> Result<()> {
        self.expect_success(
            HostCommand::LoadState {
                data: data.to_vec(),
                context: context.clone(),
            },
            "LoadState",
        )
    }

    fn take_output_events(&self) -> Vec<PluginEvent> {
        self.output_events
            .lock()
            .map(|mut o| std::mem::take(&mut *o))
            .unwrap_or_default()
    }

    fn output_channel_count(&self) -> usize {
        self.output_channels
    }

    fn helper_pid(&self) -> Option<u32> {
        self.process.lock().ok().and_then(|p| p.helper_pid())
    }

    fn recovery_count(&self) -> u64 {
        self.recovery_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn recover(&mut self) -> Result<()> {
        self.recover_locked()
    }
}

impl IsolatedPluginImpl {
    /// Respawn the helper and reload the plugin. Takes `&self` (it locks `self.process`
    /// internally and only reads immutable fields), so the auto-recover retry path in
    /// `send_command` — which has only `&self` — can call it too.
    ///
    /// The process mutex is held for the whole spawn + reload + state replay, so any other
    /// caller — including an audio callback in `process()` — blocks until the new helper is
    /// ready. See [`Self::send_command`].
    fn recover_locked(&self) -> Result<()> {
        let mut process = self
            .process
            .lock()
            .map_err(|e| Error::Other(format!("Failed to lock process: {}", e)))?;

        // Spawn a fresh helper and reload the plugin from the original path + settings.
        let mut fresh = PluginHostProcess::new(self.helper_path.clone(), self.response_timeout)
            .map_err(|e| Error::ProcessError(format!("Failed to respawn helper: {e}")))?;
        match fresh.send_command(HostCommand::LoadPlugin {
            path: self.info.path.display().to_string(),
            sample_rate: self.sample_rate,
            block_size: self.block_size as u32,
            tempo: self.tempo,
            time_sig_numerator: self.time_sig_numerator,
            time_sig_denominator: self.time_sig_denominator,
            // Reload the exact class selected originally, not merely the bundle's first class.
            class_id: Some(self.info.uid.clone()),
        }) {
            Ok(HostResponse::PluginInfo { .. }) => {}
            Ok(HostResponse::Error { message }) => {
                return Err(Error::PluginLoadFailed(format!("reload failed: {message}")))
            }
            Ok(_) => return Err(Error::Other("unexpected response while reloading".into())),
            // The reload itself crashed the fresh helper — the plugin is unrecoverable.
            Err(e) => return Err(classify_ipc_error(&e)),
        }

        // Re-apply a non-default process mode before (re)starting processing — the fresh
        // helper's plugin comes up in `Realtime`. Best-effort, like the rest of the replay.
        if self.process_mode != crate::plugin::ProcessMode::Realtime {
            let _ = fresh.send_command(HostCommand::SetProcessMode {
                mode: self.process_mode,
            });
        }

        // Tempo and time signature travel with `LoadPlugin` above; the transport's playing
        // state does not, and a fresh plugin comes up playing.
        if !self.is_playing {
            let _ = fresh.send_command(HostCommand::SetPlaying { playing: false });
        }

        // Restore processing state (parameter values are NOT replayed; see Plugin::recover).
        if self.is_processing {
            let _ = fresh.send_command(HostCommand::StartProcessing);
        }

        *process = fresh;
        self.recovery_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

// Ensure IsolatedPluginImpl is Send
unsafe impl Send for IsolatedPluginImpl {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    /// Response deadline for the fake helper below.
    ///
    /// Deliberately far above the library's production default. The fake helper is a `/bin/sh`
    /// loop, so its turnaround is bounded by how soon the OS schedules another process — which
    /// on a machine running the suite alongside a build is nowhere near instant. These tests
    /// assert on *what* crossed the boundary, never on how quickly, so a deadline tight enough
    /// to be tripped by scheduling latency only makes them flaky. The deadline behaviour itself
    /// is covered by `process_isolation`'s own timeout test, which sets its own short one.
    const FAKE_HELPER_TIMEOUT: Duration = Duration::from_secs(120);

    /// A stand-in helper: logs every request it receives and answers each one, so a test can
    /// assert on exactly what the host sent across the boundary.
    struct FakeHelper {
        dir: std::path::PathBuf,
        script: PathBuf,
        log: PathBuf,
    }

    impl FakeHelper {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "vst3_iso_{name}_{}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            let script = dir.join("fake-helper");
            let log = dir.join("requests.log");
            let mut f = std::fs::File::create(&script).expect("script");
            write!(
                f,
                concat!(
                    "#!/bin/sh\n",
                    "while IFS= read -r line; do\n",
                    "  printf '%s\\n' \"$line\" >> '{log}'\n",
                    "  case \"$line\" in\n",
                    "    *LoadPlugin*) printf '%s\\n' '{{\"PluginInfo\":{{\"vendor\":\"v\",\"name\":\"n\",\"version\":\"1\",\"category\":\"\",\"uid\":\"u\",\"has_gui\":false,\"audio_inputs\":0,\"audio_outputs\":1,\"output_channels\":2,\"has_midi_input\":true,\"has_midi_output\":false}}}}' ;;\n",
                    "    *) printf '%s\\n' '{{\"Success\":{{\"message\":\"ok\"}}}}' ;;\n",
                    "  esac\n",
                    "done\n"
                ),
                log = log.display()
            )
            .expect("write script");
            drop(f);
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            Self { dir, script, log }
        }

        fn requests(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    impl Drop for FakeHelper {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn isolated(fake: &FakeHelper) -> IsolatedPluginImpl {
        let process = PluginHostProcess::new(Some(fake.script.clone()), FAKE_HELPER_TIMEOUT)
            .expect("spawn fake helper");
        let info = PluginInfo {
            path: PathBuf::from("/tmp/fake.vst3"),
            name: "n".to_string(),
            vendor: "v".to_string(),
            version: "1".to_string(),
            category: String::new(),
            uid: "u".to_string(),
            audio_inputs: 0,
            audio_outputs: 1,
            has_midi_input: true,
            has_midi_output: false,
            has_gui: false,
        };
        IsolatedPluginImpl::new(
            process,
            info,
            44100.0,
            512,
            120.0,
            4,
            4,
            2,
            Some(fake.script.clone()),
            FAKE_HELPER_TIMEOUT,
            false,
            0,
        )
    }

    #[test]
    fn recovery_replays_the_current_transport_state_not_the_load_time_one() {
        let fake = FakeHelper::new("transport");
        let mut plugin = isolated(&fake);

        plugin.set_tempo(140.0).expect("set_tempo");
        plugin.set_time_signature(7, 8).expect("set_time_signature");
        plugin.set_playing(false).expect("set_playing");
        plugin.recover().expect("recover");

        let requests = fake.requests();
        let load_index = requests
            .iter()
            .rposition(|r| r.contains("LoadPlugin"))
            .expect("the reload sends LoadPlugin");
        let load = &requests[load_index];
        assert!(
            load.contains("\"tempo\":140.0"),
            "reload must carry the tempo set at runtime, got {load}"
        );
        assert!(
            load.contains("\"time_sig_numerator\":7")
                && load.contains("\"time_sig_denominator\":8"),
            "reload must carry the time signature set at runtime, got {load}"
        );
        assert!(
            requests[load_index..]
                .iter()
                .any(|r| r.contains("SetPlaying")),
            "a stopped transport must be replayed after the reload, got {requests:?}"
        );
    }

    /// An isolated plugin's `setState` must see the same stream attributes an in-process one
    /// would, so the restore context has to survive the boundary rather than defaulting to a
    /// project restore on the helper's side.
    #[test]
    fn a_state_restore_carries_its_context_across_the_boundary() {
        let fake = FakeHelper::new("state_context");
        let mut plugin = isolated(&fake);

        plugin
            .load_state_with_context(&[1, 2, 3], &StateContext::Project)
            .expect("project restore");
        plugin
            .load_state_with_context(
                &[1, 2, 3],
                &StateContext::preset_from_path("/Users/me/Presets/Big Lead.vstpreset"),
            )
            .expect("preset restore");
        plugin
            .load_state_with_context(&[1, 2, 3], &StateContext::preset())
            .expect("pathless preset restore");

        let loads: Vec<String> = fake
            .requests()
            .into_iter()
            .filter(|r| r.contains("LoadState"))
            .collect();
        assert_eq!(
            loads.len(),
            3,
            "expected three LoadState commands: {loads:?}"
        );
        assert!(
            loads[0].contains(r#""context":"Project""#),
            "a project restore must say so on the wire, got {}",
            loads[0]
        );
        assert!(
            loads[1].contains(r#""Preset""#)
                && loads[1].contains("/Users/me/Presets/Big Lead.vstpreset"),
            "a preset restore must carry its source path, got {}",
            loads[1]
        );
        assert!(
            loads[2].contains(r#""Preset""#) && loads[2].contains(r#""path":null"#),
            "a pathless preset restore must still say it is a preset, got {}",
            loads[2]
        );
    }

    /// The plain `load_state` entry point keeps meaning "session restore", so hosts that never
    /// heard of a restore context see exactly the behaviour they had before.
    #[test]
    fn a_context_free_state_restore_still_says_project() {
        let fake = FakeHelper::new("state_default");
        let mut plugin = isolated(&fake);

        plugin.load_state(&[9]).expect("load_state");

        let load = fake
            .requests()
            .into_iter()
            .find(|r| r.contains("LoadState"))
            .expect("a LoadState command");
        assert!(
            load.contains(r#""context":"Project""#),
            "load_state must default to a project restore, got {load}"
        );
    }
}
