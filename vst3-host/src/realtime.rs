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
//!
//! // Stop the audio side, then perform thread-affine COM teardown here.
//! runner.stop()?;
//! drop(runner);
//! let _destroyed = control.service_teardown();
//! # Ok(())
//! # }
//! ```

use crate::{audio::AudioBuffers, error::Result, midi::MidiEvent, plugin::Plugin};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    mem::ManuallyDrop,
    sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, ThreadId},
};

/// Whether `value` is a usable normalized parameter value: finite and within `0.0..=1.0`.
///
/// Checked on the control thread before a parameter change is queued. A bad value that reached
/// the ring would be rejected inside the plugin on the *audio* thread, where the rejection
/// allocates an error string and is then discarded — long after the caller was told the change
/// had been accepted.
pub(crate) fn is_normalized(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

/// Drain at most one ring's worth of queued commands from `rx`, handing each to `apply`.
/// Returns how many were applied.
///
/// The bound is what makes this safe to call from an audio callback: the ring never fills while
/// a drain keeps pace with it, so an unbounded `while let Ok(..) = pop()` lets a control thread
/// pushing in a tight loop pin the callback for as long as it keeps pushing. Commands still
/// queued when the budget runs out are applied on the next block.
pub(crate) fn drain_commands<T>(rx: &mut Consumer<T>, mut apply: impl FnMut(T)) -> usize {
    let budget = rx.buffer().capacity();
    let mut applied = 0;
    for _ in 0..budget {
        let Ok(command) = rx.pop() else {
            break;
        };
        apply(command);
        applied += 1;
    }
    applied
}

/// A runtime transport change applied to the plugin's host `ProcessContext` on the audio
/// thread, taking effect on the next block. Shared by the lock-free runner and the
/// mutex-based playback path so both apply transport mutation the same way.
#[derive(Clone, Copy)]
pub(crate) enum TransportCommand {
    /// Explicit project position for the next block.
    Position(crate::plugin::TransportPosition),
    /// Set the transport tempo (BPM).
    Tempo(f64),
    /// Set the transport time signature (`numerator`, `denominator`).
    TimeSignature(i32, i32),
    /// Toggle the transport playing state.
    Playing(bool),
}

impl TransportCommand {
    /// Apply this transport change to the plugin, ignoring errors as the audio thread does for
    /// all queued control. The value was validated on the control thread before being queued.
    pub(crate) fn apply(self, plugin: &mut Plugin) {
        match self {
            TransportCommand::Position(position) => {
                let _ = plugin.set_transport_position(position);
            }
            TransportCommand::Tempo(bpm) => {
                let _ = plugin.set_tempo(bpm);
            }
            TransportCommand::TimeSignature(num, den) => {
                let _ = plugin.set_time_signature(num, den);
            }
            TransportCommand::Playing(playing) => {
                let _ = plugin.set_playing(playing);
            }
        }
    }
}

/// A control command applied to the plugin on the audio thread.
enum RtCommand {
    /// Deliver a MIDI event at `offset` samples into the next block.
    Midi { event: MidiEvent, offset: i32 },
    /// Set a normalized parameter value on the next block.
    ///
    /// Applying this only adds a point to the processor's next input-parameter queue.
    /// `IEditController::setParamNormalized` is never invoked from the audio callback.
    Param { id: u32, value: f64 },
    /// Apply a transport change (tempo / time signature / playing) on the next block.
    Transport(TransportCommand),
}

/// Owns a [`Plugin`] on the audio thread and applies queued control commands before each
/// process block. Pair with an [`RtControl`] (returned from [`Self::new`]) to drive it from
/// other threads.
///
/// # Real-time safety
///
/// In steady state [`process`](Self::process) is **allocation-free and `Drop`-free**: once
/// warmed up it performs no heap allocation, reallocation, or free per block, even while
/// parameter changes and MIDI (in and out) are flowing. This holds under two conditions:
///
/// - **Fixed buffer size** — pass an [`AudioBuffers`] sized to the configured block size and
///   don't resize it between calls (a smaller block is fine; growth reallocates).
/// - **In-process** — the runner hosts the plugin in-process; the process-isolation path
///   marshals audio over IPC and is not allocation-free.
///
/// This is verified by `tests/alloc_tests.rs` (a counting global allocator asserts zero
/// alloc/realloc/free over a steady-state run driving parameters and MIDI). The host cannot
/// guarantee the *plugin's* own `process()` is allocation-free — that is the plugin's
/// responsibility; the guarantee is about the host code around it.
///
/// # Threading model
///
/// Queued parameter and mapped-MIDI commands populate the processor's input parameter queues on
/// the audio thread and park the same values for `IEditController`, which is a main-thread-domain
/// interface: the plugin applies them when a control thread next touches it (see
/// [`RtControl::set_parameter`]). No controller call is ever made from this runner.
///
/// It is **not yet fully lock-free**: `process` still takes a few short, uncontended mutexes
/// per block (the parameter-change and event queues, and the level meter). They are uncontended
/// while the runner owns the plugin, but a hard-real-time deployment should treat lock removal
/// as pending work. Output MIDI is already lock-free, though: take a
/// [`OutputMidiConsumer`](crate::OutputMidiConsumer) via
/// [`Plugin::output_midi_handle`](crate::Plugin::output_midi_handle) before moving the plugin
/// into the runner, then drain emitted events from your UI thread while the audio thread pushes.
pub struct RealtimePluginRunner {
    plugin: Option<Plugin>,
    rx: Consumer<RtCommand>,
    teardown_tx: SyncSender<ManuallyDrop<Plugin>>,
}

/// A `Send` handle for pushing MIDI and parameter changes to a [`RealtimePluginRunner`]
/// without locking. The runner lives on the audio thread; this handle may move between threads,
/// but plugin teardown is serviced only on the thread where [`RealtimePluginRunner::new`] created
/// it.
pub struct RtControl {
    tx: Producer<RtCommand>,
    /// Count of commands dropped because the queue was full (observability).
    dropped: u64,
    teardown: OwnerThreadTeardown<Plugin>,
}

/// The receive half of a one-slot teardown handoff.
///
/// Values are wrapped in `ManuallyDrop` before they enter the channel. This is important:
/// destroying a disconnected receiver normally drops queued values on whichever thread drops
/// the receiver. Here an off-owner drop leaks queued values instead, preserving the plugin's COM
/// and module-unload thread affinity.
struct OwnerThreadTeardown<T> {
    rx: Receiver<ManuallyDrop<T>>,
    owner_thread: ThreadId,
}

impl<T> OwnerThreadTeardown<T> {
    fn service_one(&mut self) -> bool {
        if thread::current().id() != self.owner_thread {
            return false;
        }
        match self.rx.try_recv() {
            Ok(value) => {
                drop(ManuallyDrop::into_inner(value));
                true
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => false,
        }
    }
}

impl<T> Drop for OwnerThreadTeardown<T> {
    fn drop(&mut self) {
        if thread::current().id() != self.owner_thread {
            return;
        }
        while self.service_one() {}
    }
}

fn teardown_handoff<T>() -> (SyncSender<ManuallyDrop<T>>, OwnerThreadTeardown<T>) {
    let (tx, rx) = sync_channel(1);
    (
        tx,
        OwnerThreadTeardown {
            rx,
            owner_thread: thread::current().id(),
        },
    )
}

/// Hand a value to its owner thread without blocking. Both error variants deliberately leak the
/// `ManuallyDrop` payload when the receiver is gone or the one-slot queue is occupied.
fn try_handoff_teardown<T>(tx: &SyncSender<ManuallyDrop<T>>, value: T) -> bool {
    match tx.try_send(ManuallyDrop::new(value)) {
        Ok(()) => true,
        Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
    }
}

impl RealtimePluginRunner {
    /// Build a runner that owns `plugin`, plus the [`RtControl`] handle to drive it.
    ///
    /// `command_capacity` is the maximum number of MIDI/parameter commands that can be
    /// queued between two [`process`](Self::process) calls; pushes beyond it are dropped
    /// (reported by the `RtControl` methods returning `false`). Size it for your block rate
    /// and worst-case control burst (e.g. 1024).
    ///
    /// Call this on the same control thread that loaded the plugin. If the runner is later
    /// dropped on an audio thread, the plugin is handed back to the returned [`RtControl`] for
    /// destruction on this thread. Call [`RtControl::service_teardown`] after the runner has
    /// stopped, or drop the control on this thread.
    pub fn new(plugin: Plugin, command_capacity: usize) -> (Self, RtControl) {
        let (tx, rx) = RingBuffer::new(command_capacity.max(1));
        let (teardown_tx, teardown) = teardown_handoff();
        (
            Self {
                plugin: Some(plugin),
                rx,
                teardown_tx,
            },
            RtControl {
                tx,
                dropped: 0,
                teardown,
            },
        )
    }

    /// Begin processing. Call once before the first [`process`](Self::process).
    pub fn start(&mut self) -> Result<()> {
        self.plugin
            .as_mut()
            .expect("runner plugin missing")
            .start_processing()
    }

    /// Stop processing.
    pub fn stop(&mut self) -> Result<()> {
        self.plugin
            .as_mut()
            .expect("runner plugin missing")
            .stop_processing()
    }

    /// Drain queued control commands and render one block.
    ///
    /// Call this from the audio thread (e.g. inside your device callback). It performs only
    /// the lock-free queue drain plus the plugin's own processing — it never blocks on a lock
    /// a control thread could hold.
    ///
    /// The drain is bounded by the command queue's capacity. A control thread pushing in a
    /// tight loop refills the queue as fast as this drains it, so an unbounded drain would pin
    /// the audio callback; anything still queued is applied on the next block instead.
    pub fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        let plugin = self.plugin.as_mut().expect("runner plugin missing");
        let rx = &mut self.rx;
        drain_commands(rx, |command| match command {
            RtCommand::Midi { event, offset } => {
                let _ = plugin.send_midi_event_at(event, offset);
            }
            RtCommand::Param { id, value } => {
                let _ = plugin.queue_processor_parameter_at(id, value, 0);
            }
            RtCommand::Transport(change) => {
                change.apply(plugin);
            }
        });
        plugin.process_audio(buffers)
    }

    /// Borrow the underlying plugin (e.g. to read parameters or info). Do **not** call this
    /// from the audio thread while another thread might also touch the plugin.
    pub fn plugin(&self) -> &Plugin {
        self.plugin.as_ref().expect("runner plugin missing")
    }

    /// Recover the owned plugin, consuming the runner.
    pub fn into_plugin(mut self) -> Plugin {
        self.plugin.take().expect("runner plugin missing")
    }
}

impl Drop for RealtimePluginRunner {
    fn drop(&mut self) {
        let Some(plugin) = self.plugin.take() else {
            return;
        };
        // Never destroy the plugin on this (possibly real-time) thread. The bounded handoff is
        // nonblocking; a disconnected/full queue leaks rather than running COM termination or
        // unloading executable code here.
        let _ = try_handoff_teardown(&self.teardown_tx, plugin);
    }
}

impl RtControl {
    /// Queue a seek; returns false for invalid musical position or a full ring.
    pub fn set_transport_position(&mut self, position: crate::plugin::TransportPosition) -> bool {
        if !position.quarter_notes.is_finite() {
            return false;
        }
        let ok = self
            .tx
            .push(RtCommand::Transport(TransportCommand::Position(position)))
            .is_ok();
        self.track(ok)
    }

    /// Destroy a plugin handed back by a dropped [`RealtimePluginRunner`].
    ///
    /// This call never waits for the runner. It returns `true` only when a pending plugin was
    /// destroyed. It must be called on the thread where [`RealtimePluginRunner::new`] created
    /// this control; calls from any other thread return `false` and leave the handoff queued.
    ///
    /// Dropping `RtControl` on its creation thread services any pending handoff automatically.
    /// Dropping it elsewhere deliberately leaks a pending plugin rather than releasing COM
    /// objects and unloading the plugin bundle on the wrong thread.
    pub fn service_teardown(&mut self) -> bool {
        self.teardown.service_one()
    }

    /// Queue a MIDI event for the next block (at block start). Returns `false` if the command
    /// queue is full (the event is dropped rather than blocking the caller).
    pub fn send_midi(&mut self, event: MidiEvent) -> bool {
        self.send_midi_at(event, 0)
    }

    /// Queue a MIDI event scheduled at `sample_offset` samples into the next block, for
    /// sample-accurate sequencing. A negative offset is floored to `0`; `process()` clamps it
    /// into the actual (possibly shorter) block. Returns `false` if the queue is full.
    pub fn send_midi_at(&mut self, event: MidiEvent, sample_offset: i32) -> bool {
        let ok = self
            .tx
            .push(RtCommand::Midi {
                event,
                offset: sample_offset.max(0),
            })
            .is_ok();
        self.track(ok)
    }

    /// Queue a normalized parameter change for the next block. `value` must be finite and
    /// within `0.0..=1.0`; an invalid value is rejected here (returns `false`) rather than
    /// queued, so the caller learns about it instead of the audio thread silently discarding
    /// it. Returns `false` if the queue is full.
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
    pub fn set_parameter(&mut self, id: u32, value: f64) -> bool {
        if !is_normalized(value) {
            return false;
        }
        let ok = self.tx.push(RtCommand::Param { id, value }).is_ok();
        self.track(ok)
    }

    /// Queue a transport tempo change (BPM) for the next block. `bpm` must be finite and
    /// greater than `0`; an invalid value is rejected (returns `false`) rather than queued.
    /// Returns `false` if the queue is full.
    pub fn set_tempo(&mut self, bpm: f64) -> bool {
        if !(bpm.is_finite() && bpm > 0.0) {
            return false;
        }
        let ok = self
            .tx
            .push(RtCommand::Transport(TransportCommand::Tempo(bpm)))
            .is_ok();
        self.track(ok)
    }

    /// Queue a transport time-signature change for the next block. `denominator` must be one
    /// of `1, 2, 4, 8, 16` and `numerator` must be positive; an invalid value is rejected
    /// (returns `false`). Returns `false` if the queue is full.
    pub fn set_time_signature(&mut self, numerator: i32, denominator: i32) -> bool {
        if numerator <= 0 || !matches!(denominator, 1 | 2 | 4 | 8 | 16) {
            return false;
        }
        let ok = self
            .tx
            .push(RtCommand::Transport(TransportCommand::TimeSignature(
                numerator,
                denominator,
            )))
            .is_ok();
        self.track(ok)
    }

    /// Queue a transport playing-state toggle for the next block. Returns `false` if the queue
    /// is full.
    pub fn set_playing(&mut self, playing: bool) -> bool {
        let ok = self
            .tx
            .push(RtCommand::Transport(TransportCommand::Playing(playing)))
            .is_ok();
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
    use crate::midi::MidiChannel;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    fn test_control(tx: Producer<RtCommand>) -> RtControl {
        let (_teardown_tx, teardown) = teardown_handoff();
        RtControl {
            tx,
            dropped: 0,
            teardown,
        }
    }

    #[test]
    fn control_queue_reports_full_without_blocking() {
        // A tiny capacity makes the drop-on-full behavior observable without a plugin.
        let (tx, _rx) = RingBuffer::<RtCommand>::new(2);
        let mut control = test_control(tx);
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

    #[test]
    fn transport_commands_round_trip_through_the_ring() {
        let (tx, mut rx) = RingBuffer::<RtCommand>::new(8);
        let mut control = test_control(tx);

        assert!(control.set_tempo(140.0));
        assert!(control.set_time_signature(7, 8));
        assert!(control.set_playing(false));

        // The three transport commands arrive in order, carrying their payloads intact.
        match rx.pop().expect("tempo queued") {
            RtCommand::Transport(TransportCommand::Tempo(bpm)) => assert_eq!(bpm, 140.0),
            _ => panic!("expected tempo transport command"),
        }
        match rx.pop().expect("time sig queued") {
            RtCommand::Transport(TransportCommand::TimeSignature(n, d)) => {
                assert_eq!((n, d), (7, 8))
            }
            _ => panic!("expected time-signature transport command"),
        }
        match rx.pop().expect("playing queued") {
            RtCommand::Transport(TransportCommand::Playing(p)) => assert!(!p),
            _ => panic!("expected playing transport command"),
        }
    }

    #[test]
    fn midi_offset_round_trips_through_the_ring() {
        let (tx, mut rx) = RingBuffer::<RtCommand>::new(8);
        let mut control = test_control(tx);

        assert!(control.send_midi_at(
            MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 100,
            },
            128,
        ));
        // send_midi is the offset-0 convenience.
        assert!(control.send_midi(MidiEvent::NoteOff {
            channel: MidiChannel::Ch1,
            note: 60,
            velocity: 0,
        }));

        match rx.pop().expect("scheduled note queued") {
            RtCommand::Midi { offset, .. } => assert_eq!(offset, 128),
            _ => panic!("expected a MIDI command"),
        }
        match rx.pop().expect("block-start note queued") {
            RtCommand::Midi { offset, .. } => assert_eq!(offset, 0),
            _ => panic!("expected a MIDI command"),
        }
    }

    /// `set_parameter` documents normalized values. A NaN or a `7.3` that reached the ring is
    /// rejected on the *audio* thread — allocating an error string there and vanishing silently
    /// after the caller was told the change succeeded.
    #[test]
    fn out_of_range_parameter_values_are_rejected_not_queued() {
        let (tx, mut rx) = RingBuffer::<RtCommand>::new(8);
        let mut control = test_control(tx);

        for bad in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.1,
            1.000_001,
            7.3,
        ] {
            assert!(!control.set_parameter(1, bad), "{bad} must be rejected");
        }
        assert!(rx.pop().is_err(), "no invalid value reached the ring");
        // Rejected on validation, not because the queue was full.
        assert_eq!(control.dropped_command_count(), 0);

        // Both endpoints of the normalized range are valid.
        assert!(control.set_parameter(1, 0.0));
        assert!(control.set_parameter(1, 1.0));
        assert_eq!(rx.slots(), 2);
    }

    #[test]
    fn is_normalized_accepts_exactly_the_unit_interval() {
        assert!(is_normalized(0.0) && is_normalized(0.5) && is_normalized(1.0));
        for bad in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -1e-9,
            1.0 + 1e-9,
        ] {
            assert!(!is_normalized(bad), "{bad} is not normalized");
        }
    }

    /// An unbounded `while let Ok(..) = pop()` on the audio thread can be pinned indefinitely by
    /// a control thread that pushes as fast as the callback drains: the ring never fills, so the
    /// loop never ends. The drain is capped at one ring's worth per block instead.
    #[test]
    fn drain_commands_stops_after_one_ring_even_while_the_producer_refills() {
        let (mut tx, mut rx) = RingBuffer::<u32>::new(4);
        for i in 0..4 {
            tx.push(i).expect("ring holds 4");
        }

        // Refill from inside the drain, standing in for the tight-loop control thread.
        let mut seen = Vec::new();
        let applied = drain_commands(&mut rx, |command| {
            seen.push(command);
            let _ = tx.push(100 + command);
        });

        assert_eq!(applied, 4, "exactly one ring's worth per call");
        assert_eq!(seen, vec![0, 1, 2, 3]);
        assert_eq!(
            rx.slots(),
            4,
            "the commands pushed during the drain wait for the next block"
        );
    }

    #[test]
    fn drain_commands_stops_early_on_an_empty_ring() {
        let (mut tx, mut rx) = RingBuffer::<u32>::new(64);
        tx.push(7).expect("room");
        let mut seen = Vec::new();
        assert_eq!(drain_commands(&mut rx, |c| seen.push(c)), 1);
        assert_eq!(seen, vec![7]);
        assert_eq!(drain_commands(&mut rx, |c| seen.push(c)), 0);
    }

    #[test]
    fn invalid_transport_values_are_rejected_not_queued() {
        let (tx, _rx) = RingBuffer::<RtCommand>::new(8);
        let mut control = test_control(tx);
        // Non-positive / non-finite tempo and malformed time signatures never reach the ring.
        assert!(!control.set_tempo(0.0));
        assert!(!control.set_tempo(f64::NAN));
        assert!(!control.set_time_signature(0, 4));
        assert!(!control.set_time_signature(4, 3));
        // Rejected on validation, not because the queue was full.
        assert_eq!(control.dropped_command_count(), 0);
    }

    struct DropProbe {
        drops: Arc<AtomicUsize>,
        threads: Arc<Mutex<Vec<ThreadId>>>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
            self.threads
                .lock()
                .expect("drop thread log")
                .push(thread::current().id());
        }
    }

    fn drop_probe() -> (DropProbe, Arc<AtomicUsize>, Arc<Mutex<Vec<ThreadId>>>) {
        let drops = Arc::new(AtomicUsize::new(0));
        let threads = Arc::new(Mutex::new(Vec::new()));
        (
            DropProbe {
                drops: Arc::clone(&drops),
                threads: Arc::clone(&threads),
            },
            drops,
            threads,
        )
    }

    #[test]
    fn teardown_is_serviced_only_on_the_captured_owner_thread() {
        let owner = thread::current().id();
        let (teardown_tx, teardown) = teardown_handoff();
        let (probe, drops, threads) = drop_probe();
        assert!(try_handoff_teardown(&teardown_tx, probe));

        let mut teardown = thread::spawn(move || {
            let mut teardown = teardown;
            assert!(!teardown.service_one());
            teardown
        })
        .join()
        .expect("non-owner service thread");

        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(teardown.service_one());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(*threads.lock().expect("drop thread log"), vec![owner]);
    }

    #[test]
    fn dropping_teardown_receiver_off_owner_leaks_queued_value() {
        let (teardown_tx, teardown) = teardown_handoff();
        let (probe, drops, _threads) = drop_probe();
        assert!(try_handoff_teardown(&teardown_tx, probe));

        thread::spawn(move || drop(teardown))
            .join()
            .expect("off-owner drop thread");
        drop(teardown_tx);

        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "thread-affine value must not be destroyed off its owner thread"
        );
    }

    #[test]
    fn disconnected_or_full_handoff_leaks_instead_of_dropping_the_value() {
        let (disconnected_tx, disconnected_rx) = teardown_handoff();
        drop(disconnected_rx);
        let (disconnected_probe, disconnected_drops, _) = drop_probe();
        assert!(!try_handoff_teardown(&disconnected_tx, disconnected_probe));
        assert_eq!(disconnected_drops.load(Ordering::SeqCst), 0);

        let (full_tx, mut full_rx) = teardown_handoff();
        let (queued_probe, queued_drops, _) = drop_probe();
        let (overflow_probe, overflow_drops, _) = drop_probe();
        assert!(try_handoff_teardown(&full_tx, queued_probe));
        assert!(!try_handoff_teardown(&full_tx, overflow_probe));
        assert_eq!(overflow_drops.load(Ordering::SeqCst), 0);
        assert!(full_rx.service_one());
        assert_eq!(queued_drops.load(Ordering::SeqCst), 1);
    }
}
