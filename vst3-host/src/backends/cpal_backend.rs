//! CPAL audio backend implementation

use crate::{
    audio::{AudioBackend, AudioConfig, AudioStream},
    error::{Error, Result},
};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    BufferSize, Device, Stream, StreamConfig, SupportedBufferSize,
};

/// Clamp a requested `block_size` into a device-advertised supported buffer range.
///
/// `SupportedBufferSize::Range` → `Fixed(block_size clamped to [min, max])`;
/// `Unknown` → `Default` (the device gives no range, so let it choose). Pure and unit-tested.
fn clamp_block_to_buffer_size(supported: &SupportedBufferSize, block_size: u32) -> BufferSize {
    match supported {
        SupportedBufferSize::Range { min, max } => BufferSize::Fixed(block_size.clamp(*min, *max)),
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

/// Pick a buffer size the device will actually accept, given its advertised config ranges,
/// the requested sample rate / channel count, and the desired `block_size`.
///
/// Many devices (notably CoreAudio on macOS) reject `BufferSize::Fixed` outright, so we only
/// request a fixed size when a matching range is advertised — and clamp into it. Otherwise we
/// fall back to `BufferSize::Default`. The channel count and sample rate are NOT changed here:
/// the bridge interleaves based on the requested channel count, so silently changing it would
/// garble audio.
fn resolve_buffer_size(
    ranges: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    want_sr: u32,
    want_ch: u16,
    block_size: u32,
) -> BufferSize {
    for range in ranges {
        if range.channels() != want_ch {
            continue;
        }
        if want_sr < range.min_sample_rate() || want_sr > range.max_sample_rate() {
            continue;
        }
        return clamp_block_to_buffer_size(range.buffer_size(), block_size);
    }
    BufferSize::Default
}

/// Resolve the output-stream buffer size for `device` and `config`.
fn resolve_output_buffer_size(device: &Device, config: &AudioConfig) -> BufferSize {
    // cpal 0.18: `SampleRate` is a `u32` type alias.
    match device.supported_output_configs() {
        Ok(ranges) => resolve_buffer_size(
            ranges,
            config.sample_rate as u32,
            config.output_channels as u16,
            config.block_size as u32,
        ),
        Err(_) => BufferSize::Default,
    }
}

/// Resolve the input-stream buffer size for `device` and `config`.
///
/// Mirrors [`resolve_output_buffer_size`] for the capture side: an unconditional
/// `BufferSize::Fixed` (what this used to send) is rejected by CoreAudio on many input devices.
fn resolve_input_buffer_size(device: &Device, config: &AudioConfig) -> BufferSize {
    match device.supported_input_configs() {
        Ok(ranges) => resolve_buffer_size(
            ranges,
            config.sample_rate as u32,
            config.input_channels as u16,
            config.block_size as u32,
        ),
        Err(_) => BufferSize::Default,
    }
}

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
            Err(Box::new(std::io::Error::other("Stream has been dropped")))
        }
    }

    fn pause(&self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        if let Some(ref stream) = self.stream {
            stream
                .pause()
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        } else {
            Err(Box::new(std::io::Error::other("Stream has been dropped")))
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
            .map(|d| d.to_string())
            .collect();
        Ok(devices)
    }

    /// List all available input devices
    pub fn list_input_devices(&self) -> Result<Vec<String>> {
        let devices: Vec<String> = self
            .host
            .input_devices()
            .map_err(|e| Error::AudioBackendError(format!("Failed to enumerate devices: {}", e)))?
            .map(|d| d.to_string())
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
            sample_rate: config.sample_rate as u32,
            buffer_size: resolve_output_buffer_size(device, &config),
        };

        let stream = device
            .build_output_stream(
                stream_config,
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
            sample_rate: config.sample_rate as u32,
            buffer_size: resolve_input_buffer_size(device, &config),
        };

        let stream = device
            .build_input_stream(
                stream_config,
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
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self::new().expect("Failed to create CPAL backend")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_within_range_keeps_requested_size() {
        let supported = SupportedBufferSize::Range { min: 64, max: 2048 };
        assert_eq!(
            clamp_block_to_buffer_size(&supported, 512),
            BufferSize::Fixed(512)
        );
    }

    #[test]
    fn clamp_below_min_raises_to_min() {
        let supported = SupportedBufferSize::Range {
            min: 256,
            max: 2048,
        };
        assert_eq!(
            clamp_block_to_buffer_size(&supported, 64),
            BufferSize::Fixed(256)
        );
    }

    #[test]
    fn clamp_above_max_lowers_to_max() {
        let supported = SupportedBufferSize::Range { min: 64, max: 1024 };
        assert_eq!(
            clamp_block_to_buffer_size(&supported, 4096),
            BufferSize::Fixed(1024)
        );
    }

    #[test]
    fn clamp_unknown_range_falls_back_to_default() {
        assert_eq!(
            clamp_block_to_buffer_size(&SupportedBufferSize::Unknown, 512),
            BufferSize::Default
        );
    }
}
