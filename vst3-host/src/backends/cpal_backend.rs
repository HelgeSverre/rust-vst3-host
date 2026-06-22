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
    // Prefer a range matching both the channel count and sample rate. Many devices (notably
    // pro multichannel interfaces) advertise their config ranges only at their native channel
    // count, so a stereo request would never find an exact match and silently lose the
    // configured block size. The buffer-size range is a device-level property independent of
    // the stream's channel count, so fall back to any range that covers the sample rate.
    let mut sr_only_fallback: Option<BufferSize> = None;
    for range in ranges {
        if want_sr < range.min_sample_rate() || want_sr > range.max_sample_rate() {
            continue;
        }
        let resolved = clamp_block_to_buffer_size(range.buffer_size(), block_size);
        if range.channels() == want_ch {
            return resolved; // exact channel + sample-rate match wins
        }
        sr_only_fallback.get_or_insert(resolved);
    }
    sr_only_fallback.unwrap_or(BufferSize::Default)
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
/// `BufferSize::Fixed` is rejected by CoreAudio on many input devices.
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
    use cpal::{SampleFormat, SupportedStreamConfigRange};

    /// Build a config range with the given channels, sample-rate bounds, and fixed buffer range.
    fn range(
        channels: u16,
        min_sr: u32,
        max_sr: u32,
        buf_min: u32,
        buf_max: u32,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(
            channels,
            min_sr,
            max_sr,
            SupportedBufferSize::Range {
                min: buf_min,
                max: buf_max,
            },
            SampleFormat::F32,
        )
    }

    #[test]
    fn resolve_exact_channel_and_sr_match_clamps() {
        let ranges = vec![range(2, 44_100, 48_000, 64, 2048)];
        assert_eq!(
            resolve_buffer_size(ranges.into_iter(), 48_000, 2, 512),
            BufferSize::Fixed(512)
        );
    }

    #[test]
    fn resolve_channel_mismatch_falls_back_to_sample_rate_match() {
        // Device advertises only an 8-channel range (e.g. a pro interface); a stereo request
        // must still honor the device's buffer-size range rather than dropping to Default.
        let ranges = vec![range(8, 44_100, 96_000, 128, 1024)];
        assert_eq!(
            resolve_buffer_size(ranges.into_iter(), 48_000, 2, 4096),
            BufferSize::Fixed(1024) // clamped into the device range
        );
    }

    #[test]
    fn resolve_prefers_exact_channel_over_sr_only() {
        // sr-only candidate first, exact match second — exact must win.
        let ranges = vec![
            range(8, 44_100, 96_000, 128, 1024),
            range(2, 44_100, 96_000, 64, 2048),
        ];
        assert_eq!(
            resolve_buffer_size(ranges.into_iter(), 48_000, 2, 512),
            BufferSize::Fixed(512)
        );
    }

    #[test]
    fn resolve_sample_rate_out_of_range_is_default() {
        let ranges = vec![range(2, 44_100, 48_000, 64, 2048)];
        assert_eq!(
            resolve_buffer_size(ranges.into_iter(), 96_000, 2, 512),
            BufferSize::Default
        );
    }

    #[test]
    fn resolve_no_ranges_is_default() {
        let ranges: Vec<SupportedStreamConfigRange> = vec![];
        assert_eq!(
            resolve_buffer_size(ranges.into_iter(), 48_000, 2, 512),
            BufferSize::Default
        );
    }

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
