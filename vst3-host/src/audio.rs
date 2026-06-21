//! Audio types and utilities for VST3 host

/// Audio buffers for plugin processing
#[derive(Debug)]
pub struct AudioBuffers {
    /// Input audio buffers, indexed `[channel][sample]`.
    pub inputs: Vec<Vec<f32>>,
    /// Output audio buffers, indexed `[channel][sample]`.
    pub outputs: Vec<Vec<f32>>,
    /// Sample rate in Hz
    pub sample_rate: f64,
    /// Number of samples per buffer
    pub block_size: usize,
}

impl AudioBuffers {
    /// Create new audio buffers
    pub fn new(
        input_channels: usize,
        output_channels: usize,
        block_size: usize,
        sample_rate: f64,
    ) -> Self {
        let inputs = vec![vec![0.0; block_size]; input_channels];
        let outputs = vec![vec![0.0; block_size]; output_channels];

        Self {
            inputs,
            outputs,
            sample_rate,
            block_size,
        }
    }

    /// Clear all buffers to silence
    pub fn clear(&mut self) {
        for buffer in &mut self.inputs {
            buffer.fill(0.0);
        }
        for buffer in &mut self.outputs {
            buffer.fill(0.0);
        }
    }

    /// Get the number of input channels
    pub fn input_channels(&self) -> usize {
        self.inputs.len()
    }

    /// Get the number of output channels
    pub fn output_channels(&self) -> usize {
        self.outputs.len()
    }
}

/// Audio level information for a single channel
#[derive(Debug, Clone, Copy)]
pub struct ChannelLevel {
    /// Peak level (0.0 to 1.0, where 1.0 = 0dB)
    pub peak: f32,
    /// RMS level (0.0 to 1.0)
    pub rms: f32,
    /// Peak hold level (0.0 to 1.0)
    pub peak_hold: f32,
}

impl Default for ChannelLevel {
    fn default() -> Self {
        Self {
            peak: 0.0,
            rms: 0.0,
            peak_hold: 0.0,
        }
    }
}

impl ChannelLevel {
    /// Convert peak level to decibels
    pub fn peak_db(&self) -> f32 {
        if self.peak <= 0.0 {
            -f32::INFINITY
        } else {
            20.0 * self.peak.log10()
        }
    }

    /// Convert RMS level to decibels
    pub fn rms_db(&self) -> f32 {
        if self.rms <= 0.0 {
            -f32::INFINITY
        } else {
            20.0 * self.rms.log10()
        }
    }

    /// Check if the signal is clipping (> 0dB)
    pub fn is_clipping(&self) -> bool {
        self.peak > 1.0
    }
}

/// Audio level information for all channels
#[derive(Debug, Clone)]
pub struct AudioLevels {
    /// Level information for each channel
    pub channels: Vec<ChannelLevel>,
}

impl AudioLevels {
    /// Create new audio levels for the given number of channels
    pub fn new(channel_count: usize) -> Self {
        Self {
            channels: vec![ChannelLevel::default(); channel_count],
        }
    }

    /// Update levels from audio buffers
    pub fn update_from_buffers(&mut self, buffers: &[Vec<f32>]) {
        for (i, buffer) in buffers.iter().enumerate() {
            if i >= self.channels.len() {
                break;
            }

            // Calculate peak
            let peak = buffer.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);

            // Calculate RMS
            let sum_squares: f32 = buffer.iter().map(|&x| x * x).sum();
            let rms = (sum_squares / buffer.len() as f32).sqrt();

            // Update channel levels
            let channel = &mut self.channels[i];
            channel.peak = peak;
            channel.rms = rms;

            // Update peak hold if necessary
            if peak > channel.peak_hold {
                channel.peak_hold = peak;
            }
        }
    }

    /// Reset peak hold values
    pub fn reset_peak_hold(&mut self) {
        for channel in &mut self.channels {
            channel.peak_hold = channel.peak;
        }
    }

    /// Check if any channel is clipping
    pub fn is_clipping(&self) -> bool {
        self.channels.iter().any(|ch| ch.is_clipping())
    }
}

/// Audio processing configuration
#[derive(Debug, Clone, Copy)]
pub struct AudioConfig {
    /// Sample rate in Hz
    pub sample_rate: f64,
    /// Block size in samples
    pub block_size: usize,
    /// Number of input channels
    pub input_channels: usize,
    /// Number of output channels
    pub output_channels: usize,
    /// Transport tempo in beats per minute, advertised to plugins in the host
    /// `ProcessContext` (drives tempo-synced DSP such as LFOs and synced delays).
    pub tempo: f64,
    /// Time signature numerator (beats per bar), advertised in the `ProcessContext`.
    pub time_sig_numerator: i32,
    /// Time signature denominator (note value of one beat), advertised in the
    /// `ProcessContext`.
    pub time_sig_denominator: i32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 44100.0,
            block_size: 512,
            input_channels: 0,
            output_channels: 2,
            tempo: 120.0,
            time_sig_numerator: 4,
            time_sig_denominator: 4,
        }
    }
}

/// Audio stream trait for controlling playback
pub trait AudioStream: Send {
    /// Start playback
    fn play(&self) -> Result<(), Box<dyn std::error::Error>>;

    /// Pause playback
    fn pause(&self) -> Result<(), Box<dyn std::error::Error>>;
}

/// Audio backend trait for creating audio streams
#[allow(clippy::type_complexity)] // Box<dyn FnMut...> callbacks are intrinsic to the API
pub trait AudioBackend: Send + Sync {
    /// The stream type this backend produces
    type Stream: AudioStream + Send + 'static;
    /// The device type this backend uses
    type Device: Send + Sync;
    /// The error type this backend returns
    type Error: std::error::Error + Send + Sync + 'static;

    /// Enumerate available output devices
    fn enumerate_output_devices(&self) -> Result<Vec<Self::Device>, Self::Error>;

    /// Enumerate available input devices
    fn enumerate_input_devices(&self) -> Result<Vec<Self::Device>, Self::Error>;

    /// Get the default output device
    fn default_output_device(&self) -> Option<Self::Device>;

    /// Get the default input device
    fn default_input_device(&self) -> Option<Self::Device>;

    /// Create an output stream
    fn create_output_stream(
        &self,
        device: &Self::Device,
        config: AudioConfig,
        data_callback: Box<dyn FnMut(&mut [f32]) + Send>,
        error_callback: Box<dyn FnMut(Self::Error) + Send>,
    ) -> Result<Self::Stream, Self::Error>;

    /// Create an input stream
    fn create_input_stream(
        &self,
        device: &Self::Device,
        config: AudioConfig,
        data_callback: Box<dyn FnMut(&[f32]) + Send>,
        error_callback: Box<dyn FnMut(Self::Error) + Send>,
    ) -> Result<Self::Stream, Self::Error>;

    /// Create a duplex stream (input and output)
    fn create_duplex_stream(
        &self,
        input_device: &Self::Device,
        output_device: &Self::Device,
        config: AudioConfig,
        data_callback: Box<dyn FnMut(&[f32], &mut [f32]) + Send>,
        error_callback: Box<dyn FnMut(Self::Error) + Send>,
    ) -> Result<Self::Stream, Self::Error>;
}

/// Write deinterleaved channel buffers to a 32-bit float WAV file (`WAVE_FORMAT_IEEE_FLOAT`).
///
/// `channels[ch][frame]`; all channels must be the same length. Used by offline rendering
/// (e.g. [`crate::simple::render_to_wav`]) and audio export. No external dependency.
pub fn write_wav<P: AsRef<std::path::Path>>(
    path: P,
    channels: &[Vec<f32>],
    sample_rate: u32,
) -> crate::error::Result<()> {
    use crate::error::Error;
    use std::io::Write;

    let num_channels = channels.len().max(1) as u16;
    let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    let bits_per_sample: u16 = 32;
    let block_align = num_channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * block_align as u32;
    let data_size = (frames * num_channels as usize * (bits_per_sample / 8) as usize) as u32;

    let mut buf: Vec<u8> = Vec::with_capacity(44 + data_size as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_size).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    buf.extend_from_slice(&num_channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    // Interleave channels frame by frame.
    for f in 0..frames {
        for ch in channels {
            buf.extend_from_slice(&ch[f].to_le_bytes());
        }
    }

    let mut file =
        std::fs::File::create(path).map_err(|e| Error::Other(format!("create wav: {e}")))?;
    file.write_all(&buf)
        .map_err(|e| Error::Other(format!("write wav: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod wav_tests {
    use super::*;

    #[test]
    fn write_wav_has_correct_header_and_size() {
        let ch = vec![vec![0.0f32, 0.5, -0.5, 1.0], vec![0.1, 0.2, 0.3, 0.4]];
        let path = std::env::temp_dir().join("vh_write_wav_test.wav");
        write_wav(&path, &ch, 48_000).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 3); // IEEE float
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 2); // channels
        assert_eq!(
            u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            48_000
        );
        // 4 frames * 2 ch * 4 bytes = 32 bytes of data; file = 44-byte header + 32.
        assert_eq!(bytes.len(), 44 + 32);
    }
}
