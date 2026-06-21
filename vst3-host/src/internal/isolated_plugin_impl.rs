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
use std::sync::Mutex;

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
    ) -> Self {
        Self {
            process: Mutex::new(process),
            info,
            sample_rate,
            block_size,
            tempo,
            time_sig_numerator,
            time_sig_denominator,
            is_processing: false,
            has_open_editor: false,
            editor_size: None,
            output_channels,
            output_midi: Mutex::new(Vec::new()),
        }
    }

    /// Send a command and get response.
    ///
    /// Maps a dead/crashed/hung helper to a typed [`Error::PluginCrashed`] /
    /// [`Error::PluginTimeout`] (the host process stays alive); the caller can then call
    /// [`PluginInternal::recover`] to respawn. Recovery is deliberately *not* inline here:
    /// `process()` runs on the audio thread, where a synchronous respawn+reload would stall
    /// it for hundreds of milliseconds.
    fn send_command(&self, command: HostCommand) -> Result<HostResponse> {
        let mut process = self
            .process
            .lock()
            .map_err(|e| Error::Other(format!("Failed to lock process: {}", e)))?;

        process
            .send_command(command)
            .map_err(|e| classify_ipc_error(&e))
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

        let response = self.send_command(HostCommand::Process {
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

    fn recover(&mut self) -> Result<()> {
        let mut process = self
            .process
            .lock()
            .map_err(|e| Error::Other(format!("Failed to lock process: {}", e)))?;

        // Spawn a fresh helper and reload the plugin from the original path + settings.
        let mut fresh = PluginHostProcess::new()
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

        // Restore processing state (parameter values are NOT replayed; see Plugin::recover).
        if self.is_processing {
            let _ = fresh.send_command(HostCommand::StartProcessing);
        }

        *process = fresh;
        Ok(())
    }
}

// Ensure IsolatedPluginImpl is Send
unsafe impl Send for IsolatedPluginImpl {}
