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
    /// Current sample rate
    sample_rate: f64,
    /// Current block size
    block_size: usize,
    /// Whether the plugin is currently processing
    is_processing: bool,
    /// Whether the plugin has an open editor
    has_open_editor: bool,
}

impl IsolatedPluginImpl {
    /// Create a new isolated plugin implementation
    pub fn new(
        process: PluginHostProcess,
        info: PluginInfo,
        sample_rate: f64,
        block_size: usize,
    ) -> Self {
        Self {
            process: Mutex::new(process),
            info,
            sample_rate,
            block_size,
            is_processing: false,
            has_open_editor: false,
        }
    }

    /// Send a command and get response
    fn send_command(&self, command: HostCommand) -> Result<HostResponse> {
        let mut process = self
            .process
            .lock()
            .map_err(|e| Error::Other(format!("Failed to lock process: {}", e)))?;

        process
            .send_command(command)
            .map_err(|e| Error::Other(format!("IPC error: {}", e)))
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
            HostResponse::AudioOutput { outputs } => {
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
        // TODO: Query the isolated process for editor size
        Ok((800, 600))
    }

    fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
        // Parameter changes not supported in process isolation mode
        Vec::new()
    }
}

// Ensure IsolatedPluginImpl is Send
unsafe impl Send for IsolatedPluginImpl {}
