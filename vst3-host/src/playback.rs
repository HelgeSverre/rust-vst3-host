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
    realtime::{RealtimePluginRunner, RtControl},
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
    // The capture stream for the duplex (effect-hosting) path; `None` for output-only play.
    // Kept alive alongside `_stream`.
    _input_stream: Option<Box<dyn AudioStream>>,
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
        _input_stream: None,
        plugin,
    })
}

/// Drive a plugin with **live audio input** (effect hosting): capture from the default input
/// device, process it through the plugin, and play the result on the default output device.
///
/// cpal has no true duplex stream, so this opens a separate input and output stream bridged
/// by a lock-free ring: the input callback pushes captured frames, the output callback pops
/// them into the plugin's input buffers, processes, and writes the output. `config`'s
/// `input_channels`/`output_channels`/`sample_rate` define the streams. Like
/// [`play_with_backend`], control the plugin via the returned [`AudioHandle`].
///
/// Note: the two device clocks are independent; this uses a small bridge buffer and tolerates
/// drift by dropping/zero-filling at the edges. Suitable for monitoring/auditioning effects.
pub fn play_with_input_backend<B: AudioBackend>(
    backend: &B,
    plugin: Plugin,
    config: AudioConfig,
) -> Result<AudioHandle> {
    let in_device = backend
        .default_input_device()
        .ok_or_else(|| Error::AudioBackendError("No default input device available".into()))?;
    let out_device = backend
        .default_output_device()
        .ok_or_else(|| Error::AudioBackendError("No default output device available".into()))?;

    let in_channels = config.input_channels.max(1);
    let out_channels = config.output_channels;
    let sample_rate = config.sample_rate;

    let plugin = Arc::new(Mutex::new(plugin));
    plugin
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .start_processing()?;

    // SPSC bridge: input callback (producer) -> output callback (consumer). Hold a few
    // blocks of interleaved input so the independent device clocks don't starve immediately.
    let ring_cap = (config.block_size * in_channels * 8).max(2048);
    let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(ring_cap);

    let in_data_cb = Box::new(move |data: &[f32]| {
        // Drop on full (output side fell behind) rather than block the capture callback.
        for &s in data {
            let _ = producer.push(s);
        }
    });
    let in_err_cb = Box::new(|e: B::Error| log::error!("input stream error: {}", e));
    let input_stream = backend
        .create_input_stream(&in_device, config, in_data_cb, in_err_cb)
        .map_err(|e| Error::AudioBackendError(format!("Failed to create input stream: {}", e)))?;

    let plugin_cb = Arc::clone(&plugin);
    let mut scratch = AudioBuffers::new(in_channels, out_channels, config.block_size, sample_rate);
    let out_data_cb = Box::new(move |data: &mut [f32]| {
        data.fill(0.0);
        if out_channels == 0 {
            return;
        }
        let frames = data.len() / out_channels;
        prepare_scratch(&mut scratch, frames);
        // Deinterleave captured input from the ring into the plugin's input buffers
        // (interleaved frame-major order matches the input callback's push order).
        for f in 0..frames {
            for ch in scratch.inputs.iter_mut() {
                ch[f] = consumer.pop().unwrap_or(0.0);
            }
        }
        if let Ok(mut p) = plugin_cb.lock() {
            if p.process_audio(&mut scratch).is_ok() {
                interleave_outputs(&scratch.outputs, data, out_channels);
            }
        }
    });
    let out_err_cb = Box::new(|e: B::Error| log::error!("output stream error: {}", e));
    let output_stream = backend
        .create_output_stream(&out_device, config, out_data_cb, out_err_cb)
        .map_err(|e| Error::AudioBackendError(format!("Failed to create output stream: {}", e)))?;

    input_stream
        .play()
        .map_err(|e| Error::AudioBackendError(format!("Failed to start input stream: {}", e)))?;
    output_stream
        .play()
        .map_err(|e| Error::AudioBackendError(format!("Failed to start output stream: {}", e)))?;

    Ok(AudioHandle {
        _stream: Box::new(output_stream),
        _input_stream: Some(Box::new(input_stream)),
        plugin,
    })
}

/// A running real-time audio stream (the [`RealtimePluginRunner`] variant of
/// [`AudioHandle`]). Holds the device stream open and exposes the lock-free [`RtControl`];
/// dropping it stops playback.
pub struct RtAudioHandle {
    _stream: Box<dyn AudioStream>,
    control: RtControl,
}

impl RtAudioHandle {
    /// The lock-free control handle — queue MIDI and parameter changes without locking the
    /// audio thread.
    pub fn control(&mut self) -> &mut RtControl {
        &mut self.control
    }

    /// Stop playback now (equivalent to dropping the handle).
    pub fn stop(self) {}
}

/// Like [`play_with_backend`], but drives the plugin through a [`RealtimePluginRunner`] so the
/// audio callback takes **no lock** — control changes flow over a lock-free queue. Returns an
/// [`RtAudioHandle`] that keeps the stream alive and exposes the [`RtControl`].
///
/// `command_capacity` bounds how many MIDI/parameter commands can queue between callbacks.
pub fn play_realtime_with_backend<B: AudioBackend>(
    backend: &B,
    plugin: Plugin,
    config: AudioConfig,
    command_capacity: usize,
) -> Result<RtAudioHandle> {
    let device = backend
        .default_output_device()
        .ok_or_else(|| Error::AudioBackendError("No default output device available".into()))?;

    let channels = config.output_channels;
    let sample_rate = config.sample_rate;

    let (mut runner, control) = RealtimePluginRunner::new(plugin, command_capacity);
    runner.start()?;

    // Reusable scratch buffer so the steady-state callback does not allocate.
    let mut scratch = AudioBuffers::new(0, channels, config.block_size, sample_rate);

    let data_cb = Box::new(move |data: &mut [f32]| {
        data.fill(0.0);
        if channels == 0 {
            return;
        }
        let frames = data.len() / channels;
        prepare_scratch(&mut scratch, frames);

        // No lock: the runner owns the plugin and drains its command queue here.
        if runner.process(&mut scratch).is_ok() {
            interleave_outputs(&scratch.outputs, data, channels);
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

    Ok(RtAudioHandle {
        _stream: Box::new(stream),
        control,
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
