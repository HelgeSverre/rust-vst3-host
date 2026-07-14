//! Process-isolated plugin implementation
//!
//! This module provides a PluginInternal implementation that forwards all
//! operations to a plugin running in a separate process via IPC.

use crate::{
    audio::AudioBuffers,
    error::{Error, Result},
    midi::MidiEvent,
    parameters::Parameter,
    plugin::{PluginInfo, PluginInternal},
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
    output_midi: Mutex<Vec<MidiEvent>>,
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
}

/// Cap on buffered output MIDI, matching the in-process path's MAX_OUTPUT_MIDI.
const MAX_OUTPUT_MIDI: usize = 4096;

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
            is_processing: false,
            has_open_editor: false,
            editor_size: None,
            output_channels,
            output_midi: Mutex::new(Vec::new()),
            helper_path,
            response_timeout,
            auto_recover,
            auto_recover_max_retries,
            recovery_count: std::sync::atomic::AtomicU64::new(0),
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
    fn send_command(&self, command: HostCommand) -> Result<HostResponse> {
        if !self.auto_recover {
            return self.send_command_once(command);
        }
        let mut attempt: u32 = 0;
        loop {
            match self.send_command_once(command.clone()) {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    let recoverable = matches!(e, Error::PluginCrashed | Error::PluginTimeout);
                    if !recoverable || attempt >= self.auto_recover_max_retries {
                        return Err(e);
                    }
                    attempt += 1;
                    log::warn!(
                        "isolated plugin crashed/hung ({e}); auto-recover attempt {attempt}/{}",
                        self.auto_recover_max_retries
                    );
                    // Best-effort respawn+reload; on failure surface the original error.
                    if self.recover_locked().is_err() {
                        return Err(e);
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
}

impl PluginInternal for IsolatedPluginImpl {
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
        self.expect_success(HostCommand::SetTempo { bpm }, "SetTempo")
    }

    fn set_time_signature(&mut self, numerator: i32, denominator: i32) -> Result<()> {
        self.expect_success(
            HostCommand::SetTimeSignature {
                numerator,
                denominator,
            },
            "SetTimeSignature",
        )
    }

    fn set_playing(&mut self, playing: bool) -> Result<()> {
        self.expect_success(HostCommand::SetPlaying { playing }, "SetPlaying")
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
                output_midi,
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
                if !output_midi.is_empty() {
                    if let Ok(mut buf) = self.output_midi.lock() {
                        buf.extend(output_midi);
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
        )
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

    fn latency_samples(&self) -> u32 {
        match self.send_command(HostCommand::LatencySamples) {
            Ok(HostResponse::LatencySamples { samples }) => samples,
            _ => 0,
        }
    }

    fn tail_samples(&self) -> u32 {
        match self.send_command(HostCommand::TailSamples) {
            Ok(HostResponse::TailSamples { samples }) => samples,
            _ => 0,
        }
    }

    fn midi_cc_to_parameter(&self, bus: i32, channel: i16, cc: u16) -> Option<u32> {
        match self.send_command(HostCommand::MidiCcToParameter { bus, channel, cc }) {
            Ok(HostResponse::MidiParameterMapping { id }) => id,
            _ => None,
        }
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
        // Parameter changes not supported in process isolation mode
        Vec::new()
    }

    fn take_parameter_edits(&mut self) -> Vec<crate::plugin::ParameterEdit> {
        // Pulled on demand across the boundary, like the value-change drain: the helper's
        // in-process plugin accumulates gestures from its editor and hands them back here.
        match self.send_command(HostCommand::TakeParameterEdits) {
            Ok(HostResponse::ParameterEdits { edits }) => edits,
            _ => Vec::new(),
        }
    }

    fn save_state(&self) -> Result<Vec<u8>> {
        match self.send_command(HostCommand::SaveState)? {
            HostResponse::State { data } => Ok(data),
            HostResponse::Error { message } => Err(Error::Other(format!("SaveState: {message}"))),
            _ => Err(Error::Other("SaveState: unexpected response".to_string())),
        }
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        self.expect_success(
            HostCommand::LoadState {
                data: data.to_vec(),
            },
            "LoadState",
        )
    }

    fn take_output_events(&self) -> Vec<MidiEvent> {
        self.output_midi
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
