//! Batteries-included audio playback: drive a [`Plugin`] from an [`AudioBackend`].
//!
//! This is the glue that turns a loaded plugin into sound. [`play_with_backend`]
//! opens the backend's default output device and pumps the plugin's
//! [`Plugin::process_audio`] from the device callback, returning an [`AudioHandle`]
//! that keeps the stream alive and lets you keep controlling the plugin (send MIDI,
//! change parameters) while it plays.
//!
//! For the common case, prefer [`crate::simple::play`] or [`crate::Vst3Host::play`].

use std::sync::{Arc, Mutex, MutexGuard};

use crate::{
    audio::{AudioBackend, AudioBuffers, AudioConfig, AudioStream},
    error::{Error, Result},
    plugin::Plugin,
};

/// A running audio stream driving a [`Plugin`].
///
/// Dropping the handle stops playback (the underlying device stream is released).
/// While it lives, the plugin keeps running on the audio thread; use [`Self::lock`]
/// to send MIDI or change parameters from your control thread.
pub struct AudioHandle {
    // Boxed as a trait object so `AudioHandle` is not generic over the backend.
    // Kept solely to hold the stream open — dropping it stops audio.
    _stream: Box<dyn AudioStream>,
    plugin: Arc<Mutex<Plugin>>,
}

impl AudioHandle {
    /// Lock the running plugin to send MIDI, change parameters, etc.
    ///
    /// Recovers automatically if the audio thread previously panicked while holding
    /// the lock (poisoned mutex), so control calls keep working.
    pub fn lock(&self) -> MutexGuard<'_, Plugin> {
        self.plugin
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A shared handle to the plugin, e.g. to move into another thread.
    pub fn plugin(&self) -> Arc<Mutex<Plugin>> {
        Arc::clone(&self.plugin)
    }

    /// Stop playback now (equivalent to dropping the handle).
    pub fn stop(self) {}
}

/// Interleave per-channel plugin output into a device's interleaved buffer.
///
/// `out` is laid out as `[frame0_ch0, frame0_ch1, ..., frame1_ch0, ...]` with
/// `out.len() == frames * channels`. Channels the plugin didn't produce are left
/// untouched (callers should pre-fill `out` with silence); plugin channels beyond
/// `channels` are ignored.
pub(crate) fn interleave_outputs(outputs: &[Vec<f32>], out: &mut [f32], channels: usize) {
    if channels == 0 {
        return;
    }
    let frames = out.len() / channels;
    for ch in 0..channels.min(outputs.len()) {
        let src = &outputs[ch];
        for frame in 0..frames.min(src.len()) {
            out[frame * channels + ch] = src[frame];
        }
    }
}

/// Resize a scratch buffer's output channels to exactly `frames`, clearing them.
fn prepare_scratch(scratch: &mut AudioBuffers, frames: usize) {
    for ch in &mut scratch.outputs {
        if ch.len() != frames {
            ch.resize(frames, 0.0);
        }
        ch.fill(0.0);
    }
    for ch in &mut scratch.inputs {
        if ch.len() != frames {
            ch.resize(frames, 0.0);
        }
        ch.fill(0.0);
    }
    scratch.block_size = frames;
}

/// Start streaming `plugin` through `backend`'s default output device.
///
/// The plugin is moved behind a shared lock so it can keep being controlled while
/// the audio thread pulls blocks. Playback starts immediately and continues until
/// the returned [`AudioHandle`] is dropped.
///
/// `config.output_channels` and `config.sample_rate` define the stream; the device
/// callback may request varying block sizes, which the bridge accommodates.
pub fn play_with_backend<B: AudioBackend>(
    backend: &B,
    plugin: Plugin,
    config: AudioConfig,
) -> Result<AudioHandle> {
    let device = backend
        .default_output_device()
        .ok_or_else(|| Error::AudioBackendError("No default output device available".into()))?;

    let channels = config.output_channels;
    let sample_rate = config.sample_rate;

    let plugin = Arc::new(Mutex::new(plugin));
    // Ensure the plugin is armed before the first callback fires.
    plugin
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .start_processing()?;

    let plugin_cb = Arc::clone(&plugin);
    // Reusable scratch buffer so the steady-state callback does not allocate.
    let mut scratch = AudioBuffers::new(0, channels, config.block_size, sample_rate);

    let data_cb = Box::new(move |data: &mut [f32]| {
        // Start from silence so unproduced channels/frames are quiet.
        data.fill(0.0);
        if channels == 0 {
            return;
        }
        let frames = data.len() / channels;
        prepare_scratch(&mut scratch, frames);

        if let Ok(mut p) = plugin_cb.lock() {
            if p.process_audio(&mut scratch).is_ok() {
                interleave_outputs(&scratch.outputs, data, channels);
            }
        }
    });

    let err_cb = Box::new(|e: B::Error| {
        log::error!("audio stream error: {}", e);
    });

    let stream = backend
        .create_output_stream(&device, config, data_cb, err_cb)
        .map_err(|e| Error::AudioBackendError(format!("Failed to create output stream: {}", e)))?;

    stream
        .play()
        .map_err(|e| Error::AudioBackendError(format!("Failed to start stream: {}", e)))?;

    Ok(AudioHandle {
        _stream: Box::new(stream),
        plugin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaves_two_channels() {
        // outputs[ch][frame]
        let outputs = vec![vec![1.0, 2.0, 3.0], vec![-1.0, -2.0, -3.0]];
        let mut out = vec![0.0; 6]; // 3 frames * 2 channels
        interleave_outputs(&outputs, &mut out, 2);
        assert_eq!(out, vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    }

    #[test]
    fn ignores_extra_plugin_channels() {
        // Plugin produced 3 channels but the device only has 2.
        let outputs = vec![vec![1.0, 2.0], vec![3.0, 4.0], vec![9.0, 9.0]];
        let mut out = vec![0.0; 4];
        interleave_outputs(&outputs, &mut out, 2);
        assert_eq!(out, vec![1.0, 3.0, 2.0, 4.0]);
    }

    #[test]
    fn leaves_missing_channels_as_silence() {
        // Device wants 2 channels but plugin produced only 1 (mono).
        let outputs = vec![vec![0.5, 0.6]];
        let mut out = vec![0.0; 4];
        interleave_outputs(&outputs, &mut out, 2);
        // ch1 stays at the pre-filled silence.
        assert_eq!(out, vec![0.5, 0.0, 0.6, 0.0]);
    }

    #[test]
    fn zero_channels_is_a_noop() {
        let outputs = vec![vec![1.0, 2.0]];
        let mut out = vec![7.0, 7.0];
        interleave_outputs(&outputs, &mut out, 0);
        assert_eq!(out, vec![7.0, 7.0]);
    }

    #[test]
    fn prepare_scratch_resizes_and_clears() {
        let mut scratch = AudioBuffers::new(1, 2, 4, 48000.0);
        scratch.outputs[0][0] = 9.0;
        prepare_scratch(&mut scratch, 8);
        assert_eq!(scratch.block_size, 8);
        assert!(scratch.outputs.iter().all(|c| c.len() == 8));
        assert!(scratch.inputs.iter().all(|c| c.len() == 8));
        assert!(scratch.outputs.iter().flatten().all(|&s| s == 0.0));
    }
}
