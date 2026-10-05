//! Batteries-included audio playback: drive a [`Plugin`] from an [`AudioBackend`].
//!
//! This is the glue that turns a loaded plugin into sound. [`play_with_backend`]
//! opens the backend's default output device and pumps the plugin's
//! [`Plugin::process_audio`] from the device callback, returning an [`AudioHandle`]
//! that keeps the stream alive and lets you keep controlling the plugin (send MIDI,
//! change parameters) while it plays.
//!
//! For the common case, prefer [`crate::simple::play`] or [`crate::Vst3Host::play`].

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::{
    audio::{AudioBackend, AudioBuffers, AudioConfig, AudioLevels, AudioStream, ChannelLevel},
    error::{Error, Result},
    midi::MidiEvent,
    plugin::Plugin,
    realtime::{RealtimePluginRunner, RtControl, TransportCommand},
};

/// Capacity of each lock-free side-channel ring between the UI/control thread and the audio
/// callback. Sized for a worst-case control burst and several frames of output MIDI / GUI
/// parameter changes; pushes beyond it are dropped rather than blocking.
const SIDE_CHANNEL_CAPACITY: usize = 4096;

/// A control command queued by a UI/control thread and applied on the audio thread (inside the
/// callback, under the plugin lock it already holds) at the start of the next block.
enum HybridCommand {
    LiveMidi { event: MidiEvent },
    Midi { event: MidiEvent, offset: i32 },
    Param { id: u32, value: f64 },
    Transport(TransportCommand),
    Panic,
}

/// Peak amplitude of one channel buffer, sanitizing non-finite samples to 0.
fn channel_peak(buf: &[f32]) -> f32 {
    buf.iter()
        .map(|&x| if x.is_finite() { x.abs() } else { 0.0 })
        .fold(0.0_f32, f32::max)
}

/// The audio-thread half of the lock-free side channels. Moved into the device callback; it
/// drains queued control before processing and publishes feedback (peaks, output MIDI, GUI
/// parameter changes) after. The control/feedback rings and the level atomics are lock-free;
/// the only lock the callback takes is the plugin mutex it already needs (plus, via
/// `get_parameter_changes`, the plugin's tiny internal component-handler mutex that its editor
/// briefly touches on `performEdit` — bounded, not the UI-thread audio mutex).
struct AudioSideChannels {
    control_rx: Consumer<HybridCommand>,
    out_midi_tx: Producer<MidiEvent>,
    param_tx: Producer<(u32, f64)>,
    /// One `AtomicU32` per output channel holding the max per-block peak (f32 bits) since the
    /// UI last read it. Peaks are non-negative, so `fetch_max` on the bit pattern is a valid
    /// float max.
    levels: Arc<[AtomicU32]>,
}

impl AudioSideChannels {
    /// Apply queued control commands to the plugin. The caller already holds the lock.
    ///
    /// Drains at most one ring's worth per block: a control thread pushing in a tight loop
    /// refills the ring as fast as this drains it, so an unbounded loop would pin the audio
    /// callback indefinitely. Anything still queued is applied on the next block.
    fn apply_control(&mut self, plugin: &mut Plugin) {
        crate::realtime::drain_commands(&mut self.control_rx, |command| match command {
            HybridCommand::LiveMidi { event } => {
                let _ = plugin.send_live_midi_event(event);
            }
            HybridCommand::Midi { event, offset } => {
                let _ = plugin.send_midi_event_at(event, offset);
            }
            HybridCommand::Param { id, value } => {
                let _ = plugin.queue_processor_parameter_at(id, value, 0);
            }
            HybridCommand::Transport(change) => {
                change.apply(plugin);
            }
            HybridCommand::Panic => {
                let _ = plugin.midi_panic();
            }
        });
    }

    /// Publish per-channel output peaks into the atomics. Only meaningful after a successful
    /// render, so the caller gates this on `process_audio` succeeding.
    fn publish_levels(&mut self, outputs: &[Vec<f32>]) {
        for (ch, atomic) in self.levels.iter().enumerate() {
            let peak = outputs.get(ch).map(|b| channel_peak(b)).unwrap_or(0.0);
            atomic.fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
    }

    /// Forward the plugin's drained output MIDI and editor parameter changes into their rings
    /// (drop-on-full). Called every block **regardless of processing state**: the editor can
    /// still emit parameter changes (and a plugin its output MIDI) while processing is stopped,
    /// and the UI must stay in sync.
    fn publish_feedback(&mut self, plugin: &Plugin) {
        for event in plugin.take_output_midi() {
            let _ = self.out_midi_tx.push(event);
        }
        for change in plugin.get_parameter_changes() {
            let _ = self.param_tx.push(change);
        }
    }
}

/// The UI-thread half of the side channels, stored in [`AudioHandle`]. The rtrb endpoints need
/// `&mut` for push/pop, so they live behind `Mutex` to expose `&self` methods; this mutex is
/// only ever touched by the UI/control thread, never the audio callback.
struct UiSideChannels {
    // Shared (`Arc`) so a `Send` [`MidiSink`] can be cloned out and moved to another thread (e.g.
    // a MIDI-input callback) while the non-`Send` `AudioHandle` stays put.
    control_tx: Arc<Mutex<Producer<HybridCommand>>>,
    out_midi_rx: Mutex<Consumer<MidiEvent>>,
    param_rx: Mutex<Consumer<(u32, f64)>>,
    levels: Arc<[AtomicU32]>,
}

/// Push a control command onto the shared lock-free ring, never blocking. Returns `false` if
/// the ring is full (the command is dropped) or if the ring mutex was poisoned.
///
/// The mutex here is only ever contended between control threads — the audio callback holds
/// the consumer end and never touches it.
fn queue_command(tx: &Mutex<Producer<HybridCommand>>, command: HybridCommand) -> bool {
    tx.lock()
        .map(|mut tx| tx.push(command).is_ok())
        .unwrap_or(false)
}

/// Build a fresh set of side channels for `channels` output channels, returning the audio-side
/// half (move into the callback) and the UI-side half (store in the handle).
fn make_side_channels(channels: usize) -> (AudioSideChannels, UiSideChannels) {
    let (control_tx, control_rx) = RingBuffer::<HybridCommand>::new(SIDE_CHANNEL_CAPACITY);
    let (out_midi_tx, out_midi_rx) = RingBuffer::<MidiEvent>::new(SIDE_CHANNEL_CAPACITY);
    let (param_tx, param_rx) = RingBuffer::<(u32, f64)>::new(SIDE_CHANNEL_CAPACITY);
    let levels: Arc<[AtomicU32]> = (0..channels).map(|_| AtomicU32::new(0)).collect();

    let audio = AudioSideChannels {
        control_rx,
        out_midi_tx,
        param_tx,
        levels: Arc::clone(&levels),
    };
    let ui = UiSideChannels {
        control_tx: Arc::new(Mutex::new(control_tx)),
        out_midi_rx: Mutex::new(out_midi_rx),
        param_rx: Mutex::new(param_rx),
        levels,
    };
    (audio, ui)
}

/// A running audio stream driving a [`Plugin`].
///
/// Dropping the handle stops playback (the underlying device stream is released).
/// While it lives, the plugin keeps running on the audio thread; use [`Self::lock`]
/// to send MIDI or change parameters from your control thread.
///
/// # Thread affinity
///
/// `AudioHandle` owns the device stream, which backends make thread-affine (cpal's `Stream`
/// is `!Send` for exactly this reason: open, control and *drop* must happen on one thread).
/// So the handle is `!Send` and has to stay on the thread that started playback:
///
/// ```compile_fail
/// fn assert_send<T: Send>() {}
/// assert_send::<vst3_host::AudioHandle>(); // AudioHandle is deliberately not Send
/// ```
///
/// To drive the plugin from another thread, move a [`MidiSink`] ([`Self::midi_sink`]) or the
/// shared `Arc<Mutex<Plugin>>` ([`Self::plugin`]) there instead — both are `Send`.
pub struct AudioHandle {
    // Boxed as a trait object so `AudioHandle` is not generic over the backend.
    // Kept solely to hold the stream open — dropping it stops audio.
    _stream: Box<dyn AudioStream>,
    // The capture stream for the duplex (effect-hosting) path; `None` for output-only play.
    // Kept alive alongside `_stream`.
    _input_stream: Option<Box<dyn AudioStream>>,
    plugin: Arc<Mutex<Plugin>>,
    // Lock-free side channels to/from the audio callback. Used for the hot path (control +
    // per-frame feedback) so a UI thread never contends with the audio thread for the lock.
    ui: UiSideChannels,
}

/// A cheap, cloneable, `Send` handle for queuing MIDI to a running plugin from another thread.
///
/// Obtained from [`AudioHandle::midi_sink`]. It holds only the (shared) lock-free command ring,
/// not the device stream, so unlike [`AudioHandle`] it is `Send` and can be moved into a
/// background thread or a MIDI input callback. Cloning is cheap (an `Arc` bump).
///
/// ```
/// fn assert_send<T: Send>() {}
/// assert_send::<vst3_host::MidiSink>();
/// ```
#[derive(Clone)]
pub struct MidiSink {
    control_tx: Arc<Mutex<Producer<HybridCommand>>>,
}

impl MidiSink {
    /// Queue a live event, including deferred MIDI learn. Service the owning AudioHandle
    /// on its control thread each UI tick to deliver learn notifications.
    pub fn send_live_midi(&self, event: MidiEvent) -> bool {
        queue_command(&self.control_tx, HybridCommand::LiveMidi { event })
    }

    /// Queue a MIDI event for the plugin, applied at the start of the next audio block.
    ///
    /// Lock-free and non-blocking (the same path as [`AudioHandle::send_midi`]). Returns `false`
    /// if the command ring is full (the event is dropped).
    pub fn send_midi(&self, event: MidiEvent) -> bool {
        self.send_midi_at(event, 0)
    }

    /// Queue a MIDI event scheduled at `sample_offset` samples into the next block, for
    /// sample-accurate sequencing. A negative offset is floored to `0`. Returns `false` if the
    /// ring is full.
    pub fn send_midi_at(&self, event: MidiEvent, sample_offset: i32) -> bool {
        queue_command(
            &self.control_tx,
            HybridCommand::Midi {
                event,
                offset: sample_offset.max(0),
            },
        )
    }
}

impl AudioHandle {
    /// Queue live MIDI (as opposed to sequenced input) for DSP and MIDI learn.
    pub fn send_live_midi(&self, event: MidiEvent) -> bool {
        self.midi_sink().send_live_midi(event)
    }

    /// Service deferred MIDI learn and plugin restart requests on the plugin's control
    /// thread. Call once per UI tick; this briefly locks out audio processing.
    pub fn service_host_requests(&self) -> Result<crate::plugin::RestartFlags> {
        self.lock().service_host_requests()
    }

    /// Queue a seek for the next processing block. Returns false for a non-finite musical
    /// position or a full command ring.
    pub fn set_transport_position(&self, position: crate::plugin::TransportPosition) -> bool {
        position.quarter_notes.is_finite()
            && queue_command(
                &self.ui.control_tx,
                HybridCommand::Transport(TransportCommand::Position(position)),
            )
    }

    /// Lock the running plugin to send MIDI, change parameters, etc.
    ///
    /// Recovers automatically if the audio thread previously panicked while holding
    /// the lock (poisoned mutex), so control calls keep working.
    pub fn lock(&self) -> MutexGuard<'_, Plugin> {
        self.plugin
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Try to lock the plugin without blocking, returning `None` if the audio
    /// callback currently holds the lock (it is held for the duration of each
    /// `process_audio` call).
    ///
    /// Use this on a UI/render thread for best-effort, per-frame reads (VU
    /// meters, output-MIDI drain, parameter sync): skipping a frame when the
    /// audio thread is mid-block is invisible, and it keeps the UI thread from
    /// stalling on the (unfair) mutex — which otherwise shows up as input lag.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, Plugin>> {
        match self.plugin.try_lock() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    /// Queue a MIDI event for the plugin without locking the audio thread.
    ///
    /// The event is pushed onto a lock-free ring and applied at the start of the next audio
    /// block. Prefer this over `lock().send_midi_event(..)` on a UI thread — it never blocks
    /// on the audio mutex. Returns `false` if the ring is full (the event is dropped).
    pub fn send_midi(&self, event: MidiEvent) -> bool {
        self.send_midi_at(event, 0)
    }

    /// Queue a MIDI event scheduled at `sample_offset` samples into the next block, for
    /// sample-accurate sequencing, without locking the audio thread. A negative offset is
    /// floored to `0`. Returns `false` if the ring is full.
    pub fn send_midi_at(&self, event: MidiEvent, sample_offset: i32) -> bool {
        queue_command(
            &self.ui.control_tx,
            HybridCommand::Midi {
                event,
                offset: sample_offset.max(0),
            },
        )
    }

    /// Obtain a [`MidiSink`]: a cheap, cloneable, `Send` handle that can queue MIDI to this
    /// running plugin from another thread.
    ///
    /// Unlike [`AudioHandle`] itself (which is not `Send`, as it owns the device stream), the
    /// sink can be moved into a background thread or callback — e.g. a MIDI input device
    /// callback (see [`crate::midi_input`]). It shares the same lock-free command ring as
    /// [`Self::send_midi`].
    pub fn midi_sink(&self) -> MidiSink {
        MidiSink {
            control_tx: Arc::clone(&self.ui.control_tx),
        }
    }

    /// Queue a normalized parameter change without locking the audio thread; applied at the
    /// start of the next block. `value` must be finite and within `0.0..=1.0` — an invalid
    /// value is rejected here (returns `false`) rather than queued, so the caller learns about
    /// it instead of the audio thread silently discarding it. Returns `false` if the ring is
    /// full.
    ///
    /// # The editor catches up later
    ///
    /// The audio thread applies the value to the plugin's DSP, but `IEditController` belongs to
    /// the main-thread domain, so the plugin's *own editor* (and
    /// [`Plugin::get_parameter`](crate::Plugin::get_parameter),
    /// [`format_parameter`](crate::Plugin::format_parameter) and saved state) is updated from
    /// the control thread instead. That happens the next time the control thread touches the
    /// plugin — reading a parameter, draining
    /// [`Plugin::get_parameter_changes`](crate::Plugin::get_parameter_changes), or calling
    /// [`Plugin::service_host_requests`](crate::Plugin::service_host_requests). A host that
    /// polls the plugin every UI frame (the usual editor loop) never notices the gap; a host
    /// that never calls back in will see a stale editor. The queue is bounded and drops its
    /// oldest entry when full, so the newest value for a parameter always wins.
    pub fn set_parameter(&self, id: u32, value: f64) -> bool {
        crate::realtime::is_normalized(value)
            && queue_command(&self.ui.control_tx, HybridCommand::Param { id, value })
    }

    /// Queue a transport tempo change (BPM) without locking the audio thread; applied at the
    /// start of the next block. `bpm` must be finite and greater than `0` (an invalid value is
    /// rejected, returning `false`). Returns `false` if the ring is full.
    pub fn set_tempo(&self, bpm: f64) -> bool {
        if !(bpm.is_finite() && bpm > 0.0) {
            return false;
        }
        queue_command(
            &self.ui.control_tx,
            HybridCommand::Transport(TransportCommand::Tempo(bpm)),
        )
    }

    /// Queue a transport time-signature change without locking the audio thread; applied at the
    /// start of the next block. `denominator` must be one of `1, 2, 4, 8, 16` and `numerator`
    /// must be positive (an invalid value is rejected, returning `false`). Returns `false` if
    /// the ring is full.
    pub fn set_time_signature(&self, numerator: i32, denominator: i32) -> bool {
        if numerator <= 0 || !matches!(denominator, 1 | 2 | 4 | 8 | 16) {
            return false;
        }
        queue_command(
            &self.ui.control_tx,
            HybridCommand::Transport(TransportCommand::TimeSignature(numerator, denominator)),
        )
    }

    /// Queue a transport playing-state toggle without locking the audio thread; applied at the
    /// start of the next block. Returns `false` if the ring is full.
    pub fn set_playing(&self, playing: bool) -> bool {
        queue_command(
            &self.ui.control_tx,
            HybridCommand::Transport(TransportCommand::Playing(playing)),
        )
    }

    /// Queue an all-notes-off "panic" (CC 123/120/121 on every channel) without locking the
    /// audio thread. Returns `false` if the ring is full.
    pub fn midi_panic(&self) -> bool {
        queue_command(&self.ui.control_tx, HybridCommand::Panic)
    }

    /// Read the latest per-channel output peak levels without locking the audio thread.
    ///
    /// Each channel reports the maximum peak observed since the previous call (the read resets
    /// the accumulator), so polling at UI frame rate never misses a transient between frames.
    /// `rms` is not tracked on this path and is reported as 0; `peak_hold` mirrors `peak`
    /// (drive your own ballistics, e.g. [`crate::audio::PeakMeter`], from the peak).
    pub fn output_levels(&self) -> AudioLevels {
        let channels = self
            .ui
            .levels
            .iter()
            .map(|atomic| {
                let peak = f32::from_bits(atomic.swap(0, Ordering::Relaxed));
                ChannelLevel {
                    peak,
                    rms: 0.0,
                    peak_hold: peak,
                }
            })
            .collect();
        AudioLevels { channels }
    }

    /// Drain MIDI the plugin emitted during processing (arpeggiators, MPE, …) without locking
    /// the audio thread. Returns the events queued since the last call.
    pub fn drain_output_midi(&self) -> Vec<MidiEvent> {
        let mut out = Vec::new();
        if let Ok(mut rx) = self.ui.out_midi_rx.lock() {
            while let Ok(event) = rx.pop() {
                out.push(event);
            }
        }
        out
    }

    /// Drain parameter changes the plugin made through its own editor without locking the
    /// audio thread. Returns `(id, normalized_value)` pairs queued since the last call.
    pub fn drain_parameter_changes(&self) -> Vec<(u32, f64)> {
        let mut out = Vec::new();
        if let Ok(mut rx) = self.ui.param_rx.lock() {
            while let Ok(change) = rx.pop() {
                out.push(change);
            }
        }
        out
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

/// Push interleaved capture frames onto the duplex bridge ring, **whole frames only**.
///
/// The ring carries interleaved samples but its meaning is frame-major: sample *n* belongs to
/// channel `n % channels`. Dropping an individual sample when the ring is full would shift
/// every later sample by one channel — stereo would come out L/R-swapped for the rest of the
/// stream, with nothing to resync it. So a frame is either pushed in full or dropped in full.
///
/// Returns the number of frames pushed. `data` is interleaved; a trailing partial frame (a
/// device handing over a non-multiple of `channels`) is ignored.
fn push_capture_frames(producer: &mut Producer<f32>, data: &[f32], channels: usize) -> usize {
    if channels == 0 {
        return 0;
    }
    // `slots()` is a conservative estimate (never an over-count), so every push below fits.
    let room_frames = producer.slots() / channels;
    let mut pushed = 0;
    for frame in data.chunks_exact(channels).take(room_frames) {
        for &sample in frame {
            let _ = producer.push(sample);
        }
        pushed += 1;
    }
    pushed
}

/// Pop whole interleaved frames off the duplex bridge ring into per-channel buffers.
///
/// Mirrors [`push_capture_frames`]: consuming a partial frame (the producer may be mid-frame
/// when this runs) would leave the ring's head on a non-channel-0 sample and permanently swap
/// the channel assignment. Frames beyond what the ring can supply are left untouched — the
/// caller has already cleared the buffers, so an underrun reads as silence.
///
/// Returns the number of frames written.
fn pop_capture_frames(
    consumer: &mut Consumer<f32>,
    inputs: &mut [Vec<f32>],
    frames: usize,
) -> usize {
    let channels = inputs.len();
    if channels == 0 {
        return 0;
    }
    let available = (consumer.slots() / channels).min(frames);
    for f in 0..available {
        for ch in inputs.iter_mut() {
            let sample = consumer.pop().unwrap_or(0.0);
            if let Some(slot) = ch.get_mut(f) {
                *slot = sample;
            }
        }
    }
    available
}

/// Capacity, in interleaved samples, of the duplex input→output bridge ring: about eight
/// device blocks of headroom so the two independent device clocks don't starve each other.
///
/// Sized in **frames** and multiplied up, so the capacity is always a whole number of frames —
/// one that wasn't would strand a partial frame at the wrap point, exactly the split the
/// push/pop pair goes out of its way to avoid. Bounded at both ends: a floor so a tiny block
/// size still buys real headroom, and a ceiling so an absurd configured block size can't ask
/// for a multi-gigabyte allocation.
fn bridge_ring_capacity(block_size: usize, channels: usize) -> usize {
    const MIN_BRIDGE_FRAMES: usize = 1024;
    const MAX_BRIDGE_FRAMES: usize = 1 << 18; // ~5.5 s at 48 kHz
    let frames = block_size
        .saturating_mul(8)
        .clamp(MIN_BRIDGE_FRAMES, MAX_BRIDGE_FRAMES);
    frames.saturating_mul(channels.max(1))
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
    // Lock-free side channels: UI control in, feedback (peaks / output MIDI / param changes) out.
    let (mut side, ui) = make_side_channels(channels);
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

        // Recover from poison so queued control keeps flowing even after an audio-thread panic
        // (matches AudioHandle::lock): the callback re-attempts processing rather than going
        // permanently silent.
        let mut p = match plugin_cb.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Apply queued control before rendering; render; then forward feedback. Levels need a
        // successful render, but MIDI/param feedback is published even when stopped so the UI
        // stays in sync.
        side.apply_control(&mut p);
        if p.process_audio(&mut scratch).is_ok() {
            interleave_outputs(&scratch.outputs, data, channels);
            side.publish_levels(&scratch.outputs);
        }
        side.publish_feedback(&p);
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
        ui,
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
    let (mut producer, mut consumer) =
        rtrb::RingBuffer::<f32>::new(bridge_ring_capacity(config.block_size, in_channels));

    let in_data_cb = Box::new(move |data: &[f32]| {
        // Drop on full (output side fell behind) rather than block the capture callback —
        // whole frames at a time, so overflow never shifts the interleave boundary.
        push_capture_frames(&mut producer, data, in_channels);
    });
    let in_err_cb = Box::new(|e: B::Error| log::error!("input stream error: {}", e));
    let input_stream = backend
        .create_input_stream(&in_device, config, in_data_cb, in_err_cb)
        .map_err(|e| Error::AudioBackendError(format!("Failed to create input stream: {}", e)))?;

    let plugin_cb = Arc::clone(&plugin);
    // Lock-free side channels (same as the output-only path) so effect hosting is also
    // controllable without locking the audio thread.
    let (mut side, ui) = make_side_channels(out_channels);
    let mut scratch = AudioBuffers::new(in_channels, out_channels, config.block_size, sample_rate);
    let out_data_cb = Box::new(move |data: &mut [f32]| {
        data.fill(0.0);
        if out_channels == 0 {
            return;
        }
        let frames = data.len() / out_channels;
        prepare_scratch(&mut scratch, frames);
        // Deinterleave captured input from the ring into the plugin's input buffers
        // (interleaved frame-major order matches the input callback's push order). Frames the
        // ring can't supply stay at the silence `prepare_scratch` just wrote.
        pop_capture_frames(&mut consumer, &mut scratch.inputs, frames);
        let mut p = match plugin_cb.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        side.apply_control(&mut p);
        if p.process_audio(&mut scratch).is_ok() {
            interleave_outputs(&scratch.outputs, data, out_channels);
            side.publish_levels(&scratch.outputs);
        }
        side.publish_feedback(&p);
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
        ui,
    })
}

/// A running real-time audio stream (the [`RealtimePluginRunner`] variant of
/// [`AudioHandle`]). Holds the device stream open and exposes the lock-free [`RtControl`];
/// dropping it stops playback.
///
/// Like [`AudioHandle`], it owns the thread-affine device stream and is therefore `!Send`.
/// Build the runner and its [`RtControl`] yourself ([`RealtimePluginRunner::new`]) if you need
/// the control half on a different thread than the stream.
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
    fn hybrid_midi_carries_sample_offset() {
        // The control ring (and apply_control) must preserve the scheduled offset so the mutex
        // playback path schedules MIDI sample-accurately, like the lock-free path.
        let (mut tx, mut rx) = RingBuffer::<HybridCommand>::new(4);
        tx.push(HybridCommand::Midi {
            event: MidiEvent::NoteOn {
                channel: crate::midi::MidiChannel::Ch1,
                note: 60,
                velocity: 100,
            },
            offset: 200,
        })
        .expect("queue scheduled note");
        match rx.pop().expect("note queued") {
            HybridCommand::Midi { offset, .. } => assert_eq!(offset, 200),
            _ => panic!("expected a MIDI command"),
        }
    }

    #[test]
    fn channel_peak_is_max_abs_and_sanitizes_non_finite() {
        assert_eq!(channel_peak(&[0.1, -0.5, 0.3]), 0.5);
        assert_eq!(channel_peak(&[]), 0.0);
        // NaN / inf are treated as 0 so they never poison the meter or the atomic.
        assert_eq!(channel_peak(&[f32::NAN, 0.2, f32::INFINITY]), 0.2);
    }

    #[test]
    fn nonneg_f32_bits_are_monotonic_so_fetch_max_is_float_max() {
        // The level atomics rely on this: for non-negative finite floats, a < b implies
        // a.to_bits() < b.to_bits(), so AtomicU32::fetch_max on the bit pattern is a float max.
        let peaks = [0.0_f32, 1e-6, 0.01, 0.25, 0.5, 0.999, 1.0];
        for w in peaks.windows(2) {
            assert!(w[0].to_bits() < w[1].to_bits(), "{} vs {}", w[0], w[1]);
        }
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

    /// Push more than the ring holds, then drain it: every frame that survived must still be a
    /// whole frame. Dropping a single *sample* on overflow shifts the interleave boundary, and
    /// the ring never resyncs — stereo comes out L/R-swapped for the rest of the stream.
    #[test]
    fn bridge_overflow_drops_whole_frames_never_splits_one() {
        const CH: usize = 2;
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(4 * CH);

        // 8 stereo frames into a 4-frame ring: frame f is (f, -f) so a swap is visible.
        let data: Vec<f32> = (0..8).flat_map(|f| [f as f32, -(f as f32)]).collect();
        let pushed = push_capture_frames(&mut producer, &data, CH);
        assert_eq!(pushed, 4, "only the frames that fit are pushed");

        let mut inputs = vec![vec![0.0f32; 8], vec![0.0f32; 8]];
        let popped = pop_capture_frames(&mut consumer, &mut inputs, 8);
        assert_eq!(popped, 4);
        // Left channel holds the non-negative half of each frame, right the negative half —
        // i.e. no frame was split. Frames past the underrun stay at the pre-cleared silence.
        assert_eq!(inputs[0], vec![0.0, 1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(inputs[1], vec![-0.0, -1.0, -2.0, -3.0, 0.0, 0.0, 0.0, 0.0]);
    }

    /// After an overflow the bridge must still be frame-aligned for every later block: this is
    /// the permanent-channel-swap regression, observed across successive push/pop rounds.
    #[test]
    fn bridge_stays_frame_aligned_across_overflow_rounds() {
        const CH: usize = 2;
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(4 * CH);

        for round in 0..5 {
            // Over-produce every round so the ring is permanently in overflow.
            let base = round * 100;
            let data: Vec<f32> = (0..6)
                .flat_map(|f| [(base + f) as f32, -((base + f) as f32)])
                .collect();
            push_capture_frames(&mut producer, &data, CH);

            let mut inputs = vec![vec![0.0f32; 3], vec![0.0f32; 3]];
            let popped = pop_capture_frames(&mut consumer, &mut inputs, 3);
            for f in 0..popped {
                assert_eq!(
                    inputs[1][f], -inputs[0][f],
                    "round {round} frame {f} lost channel alignment: {:?}",
                    inputs
                );
            }
        }
    }

    /// A partial trailing frame (a device handing over a non-multiple of the channel count) is
    /// ignored rather than half-pushed, for the same alignment reason.
    #[test]
    fn bridge_ignores_a_partial_trailing_frame() {
        const CH: usize = 3;
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(8 * CH);
        // Two whole frames plus two stray samples.
        let data = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        assert_eq!(push_capture_frames(&mut producer, &data, CH), 2);
        assert_eq!(consumer.slots() % CH, 0, "ring holds whole frames only");

        let mut inputs = vec![vec![0.0f32; 2], vec![0.0f32; 2], vec![0.0f32; 2]];
        assert_eq!(pop_capture_frames(&mut consumer, &mut inputs, 2), 2);
        assert_eq!(inputs[0], vec![1.0, 4.0]);
        assert_eq!(inputs[1], vec![2.0, 5.0]);
        assert_eq!(inputs[2], vec![3.0, 6.0]);
    }

    /// The consumer must not take a partial frame either: with an odd number of samples parked
    /// in the ring (the producer caught mid-frame), popping sample-by-sample would leave the
    /// head on a channel-1 sample and swap every later frame.
    #[test]
    fn bridge_consumer_leaves_an_incomplete_frame_alone() {
        const CH: usize = 2;
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(4 * CH);
        // Simulate the producer being pre-empted between the two samples of a frame.
        producer.push(1.0).expect("room for the first sample");

        let mut inputs = vec![vec![0.0f32; 2], vec![0.0f32; 2]];
        assert_eq!(pop_capture_frames(&mut consumer, &mut inputs, 2), 0);
        assert_eq!(consumer.slots(), 1, "the half frame is still queued");

        // Once the frame is complete it is consumed as a unit, channel 0 first.
        producer.push(-1.0).expect("room for the second sample");
        assert_eq!(pop_capture_frames(&mut consumer, &mut inputs, 2), 1);
        assert_eq!(inputs[0][0], 1.0);
        assert_eq!(inputs[1][0], -1.0);
    }

    /// The bridge ring must hold a whole number of frames, or a partial frame is stranded at
    /// the wrap point. `(block_size * in_channels * 8).max(2048)` did not: with 3 channels and
    /// a small block it clamped to 2048, which is not a multiple of 3.
    #[test]
    fn bridge_capacity_is_a_whole_number_of_frames() {
        for block_size in [0usize, 1, 32, 64, 128, 512, 1024, usize::MAX] {
            for channels in 1..=8usize {
                let cap = bridge_ring_capacity(block_size, channels);
                assert_eq!(
                    cap % channels,
                    0,
                    "block {block_size} x {channels} ch: capacity {cap} splits a frame"
                );
                assert!(
                    cap >= channels,
                    "block {block_size} x {channels} ch: capacity {cap} holds no frame"
                );
            }
        }
        // The old expression, for contrast: `(32 * 3 * 8).max(2048)` clamps up to 2048 samples,
        // which across 3 channels is 682.67 frames — the ring wraps mid-frame.
        assert_ne!(2048 % 3, 0);
    }

    /// A zero channel count would make the frame arithmetic divide by zero.
    #[test]
    fn bridge_capacity_survives_zero_channels() {
        assert!(bridge_ring_capacity(512, 0) > 0);
    }

    /// `queue_command` is the single push path behind every `AudioHandle` / `MidiSink` control
    /// method: never blocking, dropping on a full ring and reporting that as `false`.
    #[test]
    fn queue_command_drops_on_a_full_ring() {
        let (tx, mut rx) = RingBuffer::<HybridCommand>::new(2);
        let tx = Mutex::new(tx);
        assert!(queue_command(&tx, HybridCommand::Panic));
        assert!(queue_command(&tx, HybridCommand::Panic));
        assert!(!queue_command(&tx, HybridCommand::Panic), "ring is full");
        assert_eq!(rx.slots(), 2);
        assert!(rx.pop().is_ok());
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
