//! CPAL audio backend implementation

use crate::{
    audio::{AudioBackend, AudioConfig, AudioStream},
    error::{Error, Result},
};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    BufferSize, Device, SampleRate, Stream, StreamConfig,
};
use std::sync::Arc;

/// CPAL stream wrapper
pub struct CpalStream {
    // We use Option to allow moving the stream in drop
    stream: Option<Stream>,
}

// Manually implement Send for CpalStream
// This is safe because we only use the stream for play/pause operations
unsafe impl Send for CpalStream {}

impl AudioStream for CpalStream {
    fn play(&self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        if let Some(ref stream) = self.stream {
            stream
                .play()
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        } else {
            Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Stream has been dropped",
            )))
        }
    }

    fn pause(&self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        if let Some(ref stream) = self.stream {
            stream
                .pause()
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        } else {
            Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Stream has been dropped",
            )))
        }
    }
}

impl Drop for CpalStream {
    fn drop(&mut self) {
        // Drop the stream
        self.stream.take();
    }
}

/// CPAL-based audio backend
pub struct CpalBackend {
    host: cpal::Host,
}

impl CpalBackend {
    /// Create a new CPAL backend
    pub fn new() -> Result<Self> {
        Ok(Self {
            host: cpal::default_host(),
        })
    }

    /// List all available output devices
    pub fn list_output_devices(&self) -> Result<Vec<String>> {
        let devices: Vec<String> = self
            .host
            .output_devices()
            .map_err(|e| Error::AudioBackendError(format!("Failed to enumerate devices: {}", e)))?
            .filter_map(|d| d.name().ok())
            .collect();
        Ok(devices)
    }

    /// List all available input devices
    pub fn list_input_devices(&self) -> Result<Vec<String>> {
        let devices: Vec<String> = self
            .host
            .input_devices()
            .map_err(|e| Error::AudioBackendError(format!("Failed to enumerate devices: {}", e)))?
            .filter_map(|d| d.name().ok())
            .collect();
        Ok(devices)
    }
}

impl AudioBackend for CpalBackend {
    type Stream = CpalStream;
    type Device = Device;
    type Error = Error;

    fn enumerate_output_devices(&self) -> Result<Vec<Self::Device>> {
        let devices: Vec<Device> = self
            .host
            .output_devices()
            .map_err(|e| {
                Error::AudioBackendError(format!("Failed to enumerate output devices: {}", e))
            })?
            .collect();
        Ok(devices)
    }

    fn enumerate_input_devices(&self) -> Result<Vec<Self::Device>> {
        let devices: Vec<Device> = self
            .host
            .input_devices()
            .map_err(|e| {
                Error::AudioBackendError(format!("Failed to enumerate input devices: {}", e))
            })?
            .collect();
        Ok(devices)
    }

    fn default_output_device(&self) -> Option<Self::Device> {
        self.host.default_output_device()
    }

    fn default_input_device(&self) -> Option<Self::Device> {
        self.host.default_input_device()
    }

    fn create_output_stream(
        &self,
        device: &Self::Device,
        config: AudioConfig,
        mut data_callback: Box<dyn FnMut(&mut [f32]) + Send>,
        mut error_callback: Box<dyn FnMut(Self::Error) + Send>,
    ) -> Result<Self::Stream> {
        let stream_config = StreamConfig {
            channels: config.output_channels as u16,
            sample_rate: SampleRate(config.sample_rate as u32),
            buffer_size: BufferSize::Fixed(config.block_size as u32),
        };

        let stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    data_callback(data);
                },
                move |err| {
                    error_callback(Error::AudioBackendError(format!("Stream error: {}", err)));
                },
                None,
            )
            .map_err(|e| {
                Error::AudioBackendError(format!("Failed to build output stream: {}", e))
            })?;

        Ok(CpalStream {
            stream: Some(stream),
        })
    }

    fn create_input_stream(
        &self,
        device: &Self::Device,
        config: AudioConfig,
        mut data_callback: Box<dyn FnMut(&[f32]) + Send>,
        mut error_callback: Box<dyn FnMut(Self::Error) + Send>,
    ) -> Result<Self::Stream> {
        let stream_config = StreamConfig {
            channels: config.input_channels as u16,
            sample_rate: SampleRate(config.sample_rate as u32),
            buffer_size: BufferSize::Fixed(config.block_size as u32),
        };

        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    data_callback(data);
                },
                move |err| {
                    error_callback(Error::AudioBackendError(format!("Stream error: {}", err)));
                },
                None,
            )
            .map_err(|e| {
                Error::AudioBackendError(format!("Failed to build input stream: {}", e))
            })?;

        Ok(CpalStream {
            stream: Some(stream),
        })
    }

    fn create_duplex_stream(
        &self,
        _input_device: &Self::Device,
        output_device: &Self::Device,
        config: AudioConfig,
        mut data_callback: Box<dyn FnMut(&[f32], &mut [f32]) + Send>,
        mut error_callback: Box<dyn FnMut(Self::Error) + Send>,
    ) -> Result<Self::Stream> {
        // CPAL doesn't directly support duplex streams, so we'll create an output stream
        // and assume the callback handles both input and output
        let stream_config = StreamConfig {
            channels: config.output_channels as u16,
            sample_rate: SampleRate(config.sample_rate as u32),
            buffer_size: BufferSize::Fixed(config.block_size as u32),
        };

        // Create a dummy input buffer
        let input_buffer = Arc::new(std::sync::Mutex::new(vec![
            0.0f32;
            config.block_size
                * config.input_channels
        ]));
        let input_buffer_clone = input_buffer.clone();

        let stream = output_device
            .build_output_stream(
                &stream_config,
                move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let input = input_buffer_clone.lock().unwrap();
                    data_callback(&input, output);
                },
                move |err| {
                    error_callback(Error::AudioBackendError(format!("Stream error: {}", err)));
                },
                None,
            )
            .map_err(|e| {
                Error::AudioBackendError(format!("Failed to build duplex stream: {}", e))
            })?;

        Ok(CpalStream {
            stream: Some(stream),
        })
    }
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self::new().expect("Failed to create CPAL backend")
    }
}
