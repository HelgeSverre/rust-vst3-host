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

/// A single-channel peak meter with falling ballistics and a timed peak-hold marker —
/// the behaviour a level meter UI wants but [`AudioLevels`]'s sticky `peak_hold` doesn't give.
///
/// Time is **injected** ([`push`](Self::push) takes `now: Instant`) so the meter is
/// deterministic and independent of any clock — pass `Instant::now()` from real code, or
/// synthetic instants in tests. Feed it the per-block peak amplitude; read [`level`](Self::level)
/// for the falling meter value and [`peak_hold`](Self::peak_hold) for the held marker.
///
/// ```
/// use std::time::{Duration, Instant};
/// use vst3_host::audio::PeakMeter;
///
/// let mut meter = PeakMeter::new(20.0, Duration::from_secs(2)); // 20 dB/s fall, 2 s hold
/// let t0 = Instant::now();
/// meter.push(0.8, t0);
/// assert_eq!(meter.level(), 0.8);
/// // After silence the displayed level falls but the hold marker stays put (within the window).
/// meter.push(0.0, t0 + Duration::from_millis(100));
/// assert!(meter.level() < 0.8 && meter.level() > 0.0);
/// assert_eq!(meter.peak_hold(), 0.8);
/// ```
#[derive(Debug, Clone)]
pub struct PeakMeter {
    fall_db_per_sec: f32,
    hold: std::time::Duration,
    level: f32,
    peak_hold: f32,
    peak_hold_at: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
}

impl PeakMeter {
    /// Below this the level snaps to exactly 0.0 (≈ -100 dB), so a meter fully empties
    /// instead of asymptotically approaching zero forever.
    const SILENCE: f32 = 1e-5;

    /// Create a meter that falls at `fall_db_per_sec` decibels per second and holds the peak
    /// marker for `hold` before it, too, begins to fall. A typical UI meter uses ~20 dB/s and
    /// a 1–3 second hold.
    pub fn new(fall_db_per_sec: f32, hold: std::time::Duration) -> Self {
        Self {
            fall_db_per_sec: fall_db_per_sec.max(0.0),
            hold,
            level: 0.0,
            peak_hold: 0.0,
            peak_hold_at: None,
            last: None,
        }
    }

    /// Linear gain after falling for `dt`, e.g. `10^(-(dB/s · dt)/20)`.
    fn decay(&self, dt: std::time::Duration) -> f32 {
        let db = self.fall_db_per_sec * dt.as_secs_f32();
        10f32.powf(-db / 20.0)
    }

    /// Update with a new block's peak amplitude (`0.0..`) observed at `now`. The displayed
    /// level rises instantly to a louder peak and decays toward quieter input; the hold marker
    /// latches the loudest value and only starts falling once `hold` has elapsed since it was set.
    pub fn push(&mut self, block_peak: f32, now: std::time::Instant) {
        let block_peak = block_peak.max(0.0);
        let decay = match self.last {
            Some(prev) => self.decay(now.saturating_duration_since(prev)),
            None => 1.0,
        };

        self.level = (self.level * decay).max(block_peak);
        if self.level < Self::SILENCE {
            self.level = 0.0;
        }

        if block_peak >= self.peak_hold {
            // New loudest value — latch it and restart the hold timer.
            self.peak_hold = block_peak;
            self.peak_hold_at = Some(now);
        } else if self
            .peak_hold_at
            .is_some_and(|at| now.saturating_duration_since(at) > self.hold)
        {
            // Hold window expired — the marker falls at the same ballistic, never below `level`.
            self.peak_hold = (self.peak_hold * decay).max(self.level);
            if self.peak_hold < Self::SILENCE {
                self.peak_hold = 0.0;
            }
        }

        self.last = Some(now);
    }

    /// The current falling-meter level (`0.0..`).
    pub fn level(&self) -> f32 {
        self.level
    }

    /// The held peak marker (`0.0..`).
    pub fn peak_hold(&self) -> f32 {
        self.peak_hold
    }

    /// Reset the meter to silence.
    pub fn reset(&mut self) {
        self.level = 0.0;
        self.peak_hold = 0.0;
        self.peak_hold_at = None;
        self.last = None;
    }
}

/// A moving-window RMS estimator over the most recent `N` samples.
///
/// Unlike [`AudioLevels`]'s per-block RMS (which resets every buffer), this gives a smooth
/// level over a fixed time window regardless of block size — feed it samples or whole blocks
/// and read [`rms`](Self::rms). The window length in samples is `window_secs · sample_rate`.
///
/// ```
/// use vst3_host::audio::RmsWindow;
///
/// let mut rms = RmsWindow::new(4);
/// for _ in 0..4 { rms.push_sample(0.5); }
/// assert!((rms.rms() - 0.5).abs() < 1e-6); // constant 0.5 → RMS 0.5
/// ```
#[derive(Debug, Clone)]
pub struct RmsWindow {
    capacity: usize,
    squares: std::collections::VecDeque<f32>,
    sum: f32,
}

impl RmsWindow {
    /// Create a window holding the most recent `window_samples` samples (minimum 1).
    pub fn new(window_samples: usize) -> Self {
        let capacity = window_samples.max(1);
        Self {
            capacity,
            squares: std::collections::VecDeque::with_capacity(capacity),
            sum: 0.0,
        }
    }

    /// Create a window sized for `window_secs` of audio at `sample_rate` Hz.
    pub fn from_duration(window_secs: f32, sample_rate: f64) -> Self {
        Self::new((window_secs.max(0.0) as f64 * sample_rate).round() as usize)
    }

    /// Add one sample, evicting the oldest if the window is full.
    pub fn push_sample(&mut self, sample: f32) {
        let sq = sample * sample;
        if self.squares.len() == self.capacity {
            if let Some(old) = self.squares.pop_front() {
                self.sum -= old;
            }
        }
        self.squares.push_back(sq);
        self.sum += sq;
    }

    /// Add a whole block of samples.
    pub fn push_block(&mut self, block: &[f32]) {
        for &s in block {
            self.push_sample(s);
        }
    }

    /// Current RMS over the samples in the window (`0.0` when empty).
    pub fn rms(&self) -> f32 {
        if self.squares.is_empty() {
            return 0.0;
        }
        // Guard against tiny negative drift from float subtraction.
        (self.sum.max(0.0) / self.squares.len() as f32).sqrt()
    }

    /// Number of samples currently in the window.
    pub fn len(&self) -> usize {
        self.squares.len()
    }

    /// Whether the window holds no samples yet.
    pub fn is_empty(&self) -> bool {
        self.squares.is_empty()
    }

    /// Drop all samples.
    pub fn clear(&mut self) {
        self.squares.clear();
        self.sum = 0.0;
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

#[cfg(test)]
mod meter_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn peak_meter_rises_instantly_and_holds() {
        let mut m = PeakMeter::new(20.0, Duration::from_secs(2));
        let t0 = Instant::now();
        m.push(0.7, t0);
        assert_eq!(m.level(), 0.7);
        assert_eq!(m.peak_hold(), 0.7);

        // A louder block snaps both up immediately.
        m.push(0.9, t0 + Duration::from_millis(10));
        assert_eq!(m.level(), 0.9);
        assert_eq!(m.peak_hold(), 0.9);
    }

    #[test]
    fn peak_meter_level_falls_but_hold_latches() {
        let mut m = PeakMeter::new(20.0, Duration::from_secs(3));
        let t0 = Instant::now();
        m.push(1.0, t0);

        // 0.5 s of silence: 20 dB/s → -10 dB ≈ 0.316 linear. Level fell; hold latched.
        m.push(0.0, t0 + Duration::from_millis(500));
        let lvl = m.level();
        assert!(
            lvl < 1.0 && lvl > 0.0,
            "level should be mid-fall, got {lvl}"
        );
        assert!((lvl - 0.316).abs() < 0.02, "≈-10 dB expected, got {lvl}");
        assert_eq!(m.peak_hold(), 1.0, "hold must latch within its window");
    }

    #[test]
    fn peak_meter_hold_falls_after_window() {
        let mut m = PeakMeter::new(20.0, Duration::from_secs(1));
        let t0 = Instant::now();
        m.push(1.0, t0);
        // Past the 1 s hold window, with continued silence the marker starts falling too.
        m.push(0.0, t0 + Duration::from_millis(1500));
        assert!(
            m.peak_hold() < 1.0,
            "hold should fall after the window expired, got {}",
            m.peak_hold()
        );
    }

    #[test]
    fn peak_meter_reaches_silence_floor() {
        let mut m = PeakMeter::new(60.0, Duration::from_millis(0));
        let t0 = Instant::now();
        m.push(0.5, t0);
        // A long gap of silence fully empties the meter (snaps to exactly 0).
        m.push(0.0, t0 + Duration::from_secs(10));
        assert_eq!(m.level(), 0.0);
        assert_eq!(m.peak_hold(), 0.0);
    }

    #[test]
    fn rms_window_constant_signal() {
        let mut r = RmsWindow::new(8);
        for _ in 0..8 {
            r.push_sample(0.5);
        }
        assert!((r.rms() - 0.5).abs() < 1e-6);
        assert_eq!(r.len(), 8);
    }

    #[test]
    fn rms_window_slides_and_evicts() {
        let mut r = RmsWindow::new(3);
        r.push_block(&[1.0, 1.0, 1.0]);
        assert!((r.rms() - 1.0).abs() < 1e-6);
        // Push three zeros: the loud samples are evicted, RMS returns to 0.
        r.push_block(&[0.0, 0.0, 0.0]);
        assert_eq!(r.len(), 3);
        assert!(
            r.rms() < 1e-6,
            "window should have slid to silence, got {}",
            r.rms()
        );
    }

    #[test]
    fn rms_window_empty_is_zero() {
        let r = RmsWindow::new(16);
        assert!(r.is_empty());
        assert_eq!(r.rms(), 0.0);
    }

    #[test]
    fn rms_window_from_duration_sizes_correctly() {
        // 10 ms at 48 kHz = 480 samples.
        let r = RmsWindow::from_duration(0.01, 48_000.0);
        assert_eq!(r.capacity, 480);
    }
}
