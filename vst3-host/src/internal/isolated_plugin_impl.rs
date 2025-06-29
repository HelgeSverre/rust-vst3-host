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

impl PluginInternal for IsolatedPluginImpl {
    fn set_parameter(&mut self, _id: u32, _value: f64) -> Result<()> {
        // For now, we'll store parameters locally and apply them during processing
        // In a full implementation, this would send a SetParameter command
        // TODO: Implement SetParameter command in the protocol
        Ok(())
    }

    fn get_parameter(&self, _id: u32) -> Result<f64> {
        // TODO: Implement GetParameter command in the protocol
        Ok(0.5) // Default value for now
    }

    fn get_all_parameters(&self) -> Result<Vec<Parameter>> {
        // TODO: Implement GetAllParameters command in the protocol
        // For now, return an empty list
        Ok(Vec::new())
    }

    fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        // Convert input audio to a flat vector
        let input_data: Vec<f32> = buffers
            .inputs
            .iter()
            .flat_map(|channel| channel.iter().copied())
            .collect();

        // Send process command
        let response = self.send_command(HostCommand::Process {
            audio_data: input_data,
        })?;

        // Handle the response
        match response {
            HostResponse::AudioOutput { data } => {
                // Copy output data back to buffers
                let samples_per_channel = self.block_size;
                let num_output_channels = buffers.outputs.len();

                for (ch_idx, output_channel) in buffers.outputs.iter_mut().enumerate() {
                    let start_idx = ch_idx * samples_per_channel;
                    let end_idx = ((ch_idx + 1) * samples_per_channel).min(data.len());

                    if start_idx < data.len() {
                        for (sample_idx, sample) in output_channel.iter_mut().enumerate() {
                            let data_idx = start_idx + sample_idx;
                            if data_idx < end_idx {
                                *sample = data[data_idx];
                            } else {
                                *sample = 0.0;
                            }
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

    fn send_midi_event(&mut self, _event: MidiEvent) -> Result<()> {
        // TODO: Implement MIDI event sending in the protocol
        Ok(())
    }

    fn start_processing(&mut self) -> Result<()> {
        // TODO: Implement StartProcessing command in the protocol
        self.is_processing = true;
        Ok(())
    }

    fn stop_processing(&mut self) -> Result<()> {
        // TODO: Implement StopProcessing command in the protocol
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
}

// Ensure IsolatedPluginImpl is Send
unsafe impl Send for IsolatedPluginImpl {}
