//! Lock-free real-time plugin runner.
//!
//! [`Vst3Host::play`](crate::Vst3Host::play) / [`simple::play`](crate::simple::play) are the
//! friendly path: they wrap the plugin in an `Arc<Mutex<Plugin>>` and the audio callback
//! locks it. That's correctness-first but not hard-real-time — a control-thread call can
//! contend with the audio thread for the lock.
//!
//! [`RealtimePluginRunner`] is the serious path *alongside* it. The runner **owns** the
//! plugin on the audio thread; control commands (MIDI, parameter changes) are delivered over
//! a lock-free SPSC ring and applied at the start of each block. The audio callback never
//! takes a lock a control thread could be holding, so it can't be blocked by `set_parameter`
//! or `send_midi`.
//!
//! ```no_run
//! use vst3_host::{simple, realtime::RealtimePluginRunner, midi::MidiChannel, audio::AudioBuffers};
//! # fn main() -> vst3_host::Result<()> {
//! let plugin = simple::load_plugin("/path/synth.vst3")?;
//! let (mut runner, mut control) = RealtimePluginRunner::new(plugin, 1024);
//! runner.start()?;
//!
//! // From any thread: queue control changes without locking the audio thread.
//! control.send_midi(vst3_host::midi::MidiEvent::NoteOn { channel: MidiChannel::Ch1, note: 60, velocity: 100 });
//!
//! // On the audio thread (e.g. your device callback): drain commands + render, no locks.
//! let mut buffers = AudioBuffers::new(0, 2, 512, 48_000.0);
//! runner.process(&mut buffers)?;
//! # Ok(())
//! # }
//! ```

use crate::{audio::AudioBuffers, error::Result, midi::MidiEvent, plugin::Plugin};
use rtrb::{Consumer, Producer, RingBuffer};

/// A control command applied to the plugin on the audio thread.
enum RtCommand {
    /// Deliver a MIDI event on the next block.
    Midi(MidiEvent),
    /// Set a normalized parameter value on the next block.
    Param { id: u32, value: f64 },
}

/// Owns a [`Plugin`] on the audio thread and applies queued control commands before each
/// process block, with no locking on the audio path. Pair with an [`RtControl`] (returned
/// from [`Self::new`]) to drive it from other threads.
pub struct RealtimePluginRunner {
    plugin: Plugin,
    rx: Consumer<RtCommand>,
}

/// A `Send` handle for pushing MIDI and parameter changes to a [`RealtimePluginRunner`]
/// without locking. Lives on the control thread; the runner lives on the audio thread.
pub struct RtControl {
    tx: Producer<RtCommand>,
    /// Count of commands dropped because the queue was full (observability).
    dropped: u64,
}

impl RealtimePluginRunner {
    /// Build a runner that owns `plugin`, plus the [`RtControl`] handle to drive it.
    ///
    /// `command_capacity` is the maximum number of MIDI/parameter commands that can be
    /// queued between two [`process`](Self::process) calls; pushes beyond it are dropped
    /// (reported by the `RtControl` methods returning `false`). Size it for your block rate
    /// and worst-case control burst (e.g. 1024).
    pub fn new(plugin: Plugin, command_capacity: usize) -> (Self, RtControl) {
        let (tx, rx) = RingBuffer::new(command_capacity.max(1));
        (Self { plugin, rx }, RtControl { tx, dropped: 0 })
    }

    /// Begin processing. Call once before the first [`process`](Self::process).
    pub fn start(&mut self) -> Result<()> {
        self.plugin.start_processing()
    }

    /// Stop processing.
    pub fn stop(&mut self) -> Result<()> {
        self.plugin.stop_processing()
    }

    /// Drain all queued control commands and render one block.
    ///
    /// Call this from the audio thread (e.g. inside your device callback). It performs only
    /// the lock-free queue drain plus the plugin's own processing — it never blocks on a lock
    /// a control thread could hold.
    pub fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        while let Ok(cmd) = self.rx.pop() {
            match cmd {
                RtCommand::Midi(event) => {
                    let _ = self.plugin.send_midi_event(event);
                }
                RtCommand::Param { id, value } => {
                    let _ = self.plugin.set_parameter(id, value);
                }
            }
        }
        self.plugin.process_audio(buffers)
    }

    /// Borrow the underlying plugin (e.g. to read parameters or info). Do **not** call this
    /// from the audio thread while another thread might also touch the plugin.
    pub fn plugin(&self) -> &Plugin {
        &self.plugin
    }

    /// Recover the owned plugin, consuming the runner.
    pub fn into_plugin(self) -> Plugin {
        self.plugin
    }
}

impl RtControl {
    /// Queue a MIDI event for the next block. Returns `false` if the command queue is full
    /// (the event is dropped rather than blocking the caller).
    pub fn send_midi(&mut self, event: MidiEvent) -> bool {
        let ok = self.tx.push(RtCommand::Midi(event)).is_ok();
        self.track(ok)
    }

    /// Queue a normalized parameter change (`0.0..=1.0`) for the next block. Returns `false`
    /// if the queue is full.
    pub fn set_parameter(&mut self, id: u32, value: f64) -> bool {
        let ok = self.tx.push(RtCommand::Param { id, value }).is_ok();
        self.track(ok)
    }

    /// Total number of commands dropped because the queue was full since this control was
    /// created. A persistently rising count means the queue capacity is too small for the
    /// control rate.
    pub fn dropped_command_count(&self) -> u64 {
        self.dropped
    }

    fn track(&mut self, ok: bool) -> bool {
        if !ok {
            self.dropped += 1;
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_queue_reports_full_without_blocking() {
        // A tiny capacity makes the drop-on-full behavior observable without a plugin.
        let (tx, _rx) = RingBuffer::<RtCommand>::new(2);
        let mut control = RtControl { tx, dropped: 0 };
        assert!(control.set_parameter(1, 0.5));
        assert!(control.set_parameter(1, 0.6));
        // Third push exceeds capacity (nothing has been drained) → dropped, not blocked.
        assert!(!control.set_parameter(1, 0.7));
        assert!(!control.send_midi(crate::midi::MidiEvent::NoteOn {
            channel: crate::midi::MidiChannel::Ch1,
            note: 60,
            velocity: 100
        }));
        assert_eq!(control.dropped_command_count(), 2);
    }
}
