//! Fixed-storage output events for exclusive realtime processing.
//!
//! A plugin owns the pointer-backed fields of an [`Event`] only for the duration of its
//! `IEventList::addEvent` call. `PreparedEventPool` copies those fields into slots prepared on
//! the control thread. The public output queue then moves slot handles, never payload vectors,
//! across the process/control-thread boundary.

use std::{
    cell::UnsafeCell,
    mem, ptr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use crossbeam_queue::ArrayQueue;
use vst3::Steinberg::Vst::{Event, Event_};

use crate::{
    internal::com_implementations::raw_event_to_plugin_event,
    midi::{MidiEvent, PluginEvent, MAX_EVENT_PAYLOAD_BYTES, MAX_EVENT_TEXT_UNITS},
};

/// A queue entry whose destruction must happen away from the prepared process callback.
pub(crate) enum OutputEventItem {
    Owned(PluginEvent),
    Prepared(PreparedOutputEvent),
}

/// A stable, preallocated copy of one raw VST event.
///
/// The slot is uniquely owned from acquisition until it is either published to the output queue
/// or recycled. A consumer owns it after popping it from that queue.
pub(crate) struct PreparedOutputEvent {
    pool: Arc<PreparedEventPool>,
    slot: usize,
}

impl PreparedOutputEvent {
    /// Returns the raw event while this slot is still checked out.
    pub(crate) fn raw_event(&self) -> Option<Event> {
        self.pool.event(self.slot)
    }

    /// Copies the VST event into a normal owned event and returns the fixed slot to its pool.
    pub(crate) fn into_plugin_event(self) -> Option<PluginEvent> {
        let raw = self.pool.event(self.slot)?;
        // SAFETY: `raw` points only into this uniquely owned slot, which remains checked out
        // until after the converter has copied every pointer-backed payload.
        unsafe { raw_event_to_plugin_event(&raw).ok() }
    }

    /// Converts a channel-voice event, recycling all other prepared events without allocating.
    pub(crate) fn into_midi_event(self) -> Option<MidiEvent> {
        let raw = self.pool.event(self.slot)?;
        if !is_midi_event(&raw) {
            return None;
        }
        // SAFETY: as in `into_plugin_event`, the slot remains checked out through conversion.
        unsafe { raw_event_to_plugin_event(&raw).ok() }.and_then(|event| event.to_midi())
    }

    /// Recycles a slot whose event is intentionally discarded.
    pub(crate) fn recycle(self) {}

    pub(crate) fn pool(&self) -> &Arc<PreparedEventPool> {
        &self.pool
    }
}

impl Drop for PreparedOutputEvent {
    fn drop(&mut self) {
        self.pool.release(self.slot);
    }
}

/// Stable payload storage for prepared VST output events.
pub(crate) struct PreparedEventPool {
    slots: Vec<PreparedEventSlot>,
    free_slots: ArrayQueue<usize>,
    payload_bytes_per_event: usize,
}

// SAFETY: a slot is mutated only while checked out by one producer or consumer. The lock-free
// free list transfers that ownership, and its backing vectors are allocated once at construction.
unsafe impl Sync for PreparedEventPool {}

struct PreparedEventSlot {
    event: UnsafeCell<Event>,
    /// One aligned arena serves byte and UTF-16 payloads. It is fully initialized so raw copies
    /// can address either view without changing a `Vec` length in the process callback.
    payload: UnsafeCell<Box<[u16]>>,
}

impl PreparedEventPool {
    /// Allocates every event and payload slot before entering the prepared processing mode.
    pub(crate) fn new(slot_count: usize, payload_bytes_per_event: usize) -> Arc<Self> {
        let text_capacity = payload_bytes_per_event.div_ceil(mem::size_of::<u16>());
        let slots = (0..slot_count)
            .map(|_| PreparedEventSlot {
                // SAFETY: an all-zero Event is only a placeholder until an acquired slot is
                // populated. It is never exposed to a plugin or consumer in that state.
                event: UnsafeCell::new(unsafe { mem::zeroed() }),
                payload: UnsafeCell::new(vec![0; text_capacity].into_boxed_slice()),
            })
            .collect::<Vec<_>>();
        let free_slots = ArrayQueue::new(slot_count);
        for slot in 0..slot_count {
            // The queue was created with exactly this capacity.
            let _ = free_slots.push(slot);
        }
        Arc::new(Self {
            slots,
            free_slots,
            payload_bytes_per_event,
        })
    }

    /// Copies `source` into a checked-out slot. A `None` result means invalid payload data or a
    /// prepared-capacity overflow; neither case changes the source event.
    /// # Safety
    ///
    /// Any pointer-backed field in `source` must remain readable for the duration of this call.
    /// VST3 provides that guarantee to `IEventList::addEvent` implementations.
    pub(crate) unsafe fn copy_event(
        self: &Arc<Self>,
        source: &Event,
    ) -> Option<PreparedOutputEvent> {
        let slot = self.free_slots.pop()?;
        // SAFETY: upheld by this method's caller contract.
        if !unsafe { self.copy_into_slot(slot, source) } {
            self.release(slot);
            return None;
        }
        Some(PreparedOutputEvent {
            pool: Arc::clone(self),
            slot,
        })
    }

    fn event(&self, slot: usize) -> Option<Event> {
        // SAFETY: callers hold the slot until this copied `Event` has been consumed.
        self.slots
            .get(slot)
            .map(|slot| unsafe { *slot.event.get() })
    }

    fn release(&self, slot: usize) {
        // `PreparedOutputEvent` is the linear ownership token for an acquired slot, so exactly
        // one destructor or explicit failed-copy path returns it. The free ring has one entry per
        // slot and cannot be full for a valid release.
        debug_assert!(self.free_slots.push(slot).is_ok());
    }

    // The SDK exposes these constants with target-dependent aliases; explicit casts keep this
    // comparison portable on Windows where their inferred type differs from `Event::type`.
    #[allow(clippy::unnecessary_cast)]
    unsafe fn copy_into_slot(&self, slot: usize, source: &Event) -> bool {
        let Some(slot) = self.slots.get(slot) else {
            return false;
        };
        let mut copied = *source;

        // SAFETY: this slot was exclusively acquired from `free_slots`; its boxed payload arena
        // never moves, so rewritten VST pointers remain stable until the slot is recycled.
        unsafe {
            use Event_::EventTypes_::*;

            match copied.r#type as u32 {
                kind if kind == kDataEvent as u32 => {
                    let data = copied.__field0.data;
                    let Ok(length) = usize::try_from(data.size) else {
                        return false;
                    };
                    if !copy_bytes(slot, data.bytes, length, self.payload_bytes_per_event) {
                        return false;
                    }
                    copied.__field0.data.bytes = if length == 0 {
                        ptr::null()
                    } else {
                        (&*slot.payload.get()).as_ptr().cast()
                    };
                }
                kind if kind == kNoteExpressionTextEvent as u32 => {
                    let text = copied.__field0.noteExpressionText;
                    let length = match usize::try_from(text.textLen) {
                        Ok(length) => length,
                        Err(_) => return false,
                    };
                    if !copy_text(slot, text.text, length, self.payload_bytes_per_event) {
                        return false;
                    }
                    copied.__field0.noteExpressionText.text = if length == 0 {
                        ptr::null()
                    } else {
                        (&*slot.payload.get()).as_ptr()
                    };
                }
                kind if kind == kChordEvent as u32 => {
                    let chord = copied.__field0.chord;
                    let length = usize::from(chord.textLen);
                    if !copy_text(slot, chord.text, length, self.payload_bytes_per_event) {
                        return false;
                    }
                    copied.__field0.chord.text = if length == 0 {
                        ptr::null()
                    } else {
                        (&*slot.payload.get()).as_ptr()
                    };
                }
                kind if kind == kScaleEvent as u32 => {
                    let scale = copied.__field0.scale;
                    let length = usize::from(scale.textLen);
                    if !copy_text(slot, scale.text, length, self.payload_bytes_per_event) {
                        return false;
                    }
                    copied.__field0.scale.text = if length == 0 {
                        ptr::null()
                    } else {
                        (&*slot.payload.get()).as_ptr()
                    };
                }
                kind if kind == kNoteOnEvent as u32
                    || kind == kNoteOffEvent as u32
                    || kind == kPolyPressureEvent as u32
                    || kind == kNoteExpressionValueEvent as u32
                    || kind == kNoteExpressionIntValueEvent as u32
                    || kind == kLegacyMIDICCOutEvent as u32 => {}
                _ => return false,
            }
            *slot.event.get() = copied;
        }
        true
    }
}

/// A persistent public output queue that can carry ordinary owned events and prepared slots.
pub(crate) struct OutputEventQueue {
    items: ArrayQueue<OutputEventItem>,
    /// The high bit pauses new consumer pops while the control thread rotates entries. The low
    /// bits count pops which had already entered when the pause began.
    pop_gate: AtomicUsize,
}

const POP_GATE_PAUSED: usize = 1usize << (usize::BITS - 1);

struct PopGatePause<'a> {
    gate: &'a AtomicUsize,
}

impl Drop for PopGatePause<'_> {
    fn drop(&mut self) {
        self.gate.store(0, Ordering::Release);
    }
}

impl OutputEventQueue {
    /// Creates the queue at plugin load time, before any particular realtime capacity is known.
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            items: ArrayQueue::new(capacity),
            pop_gate: AtomicUsize::new(0),
        })
    }

    /// Pop an owned event on a control thread.
    pub(crate) fn pop_event(&self) -> Option<PluginEvent> {
        match self.pop_item()? {
            OutputEventItem::Owned(event) => Some(event),
            OutputEventItem::Prepared(event) => event.into_plugin_event(),
        }
    }

    /// Pop MIDI without materializing pointer-backed non-MIDI payloads.
    pub(crate) fn pop_midi(&self) -> Option<MidiEvent> {
        while let Some(item) = self.pop_item() {
            match item {
                OutputEventItem::Owned(event) => {
                    if let Some(midi) = event.to_midi() {
                        return Some(midi);
                    }
                }
                OutputEventItem::Prepared(event) => {
                    if let Some(midi) = event.into_midi_event() {
                        return Some(midi);
                    }
                }
            }
        }
        None
    }

    /// Push an ordinary event with newest-wins semantics.
    ///
    /// The caller decides where a displaced prepared event is retired; dropping the last old
    /// pool reference from a process callback would free its prepared backing store there.
    pub(crate) fn force_push_owned(&self, event: PluginEvent) -> Option<OutputEventItem> {
        self.items.force_push(OutputEventItem::Owned(event))
    }

    /// Push a prepared event with newest-wins semantics.
    ///
    /// A displacement from `current_pool` can be recycled immediately because that pool remains
    /// owned by the active realtime state. An older generation is returned for off-thread
    /// retirement instead.
    pub(crate) fn force_push_prepared(
        &self,
        event: PreparedOutputEvent,
        current_pool: &Arc<PreparedEventPool>,
    ) -> Option<OutputEventItem> {
        match self.items.force_push(OutputEventItem::Prepared(event)) {
            Some(OutputEventItem::Prepared(displaced))
                if Arc::ptr_eq(displaced.pool(), current_pool) =>
            {
                displaced.recycle();
                None
            }
            other => other,
        }
    }

    /// Replaces queued prepared entries with ordinary owned events while audio is stopped.
    ///
    /// This is called before allocating a new prepared generation, so a consumer that has kept
    /// an output handle across realtime sessions cannot make old fixed payload arenas accumulate.
    /// Any entry that cannot be requeued because another producer raced this control-thread
    /// operation is passed to `retire` on this same non-realtime thread.
    pub(crate) fn materialize_prepared_events(&self, mut retire: impl FnMut(OutputEventItem)) {
        let _pause = self.pause_pops();
        let queued = self.items.len();
        for _ in 0..queued {
            let Some(item) = self.items.pop() else {
                break;
            };
            let item = match item {
                OutputEventItem::Owned(event) => OutputEventItem::Owned(event),
                OutputEventItem::Prepared(event) => match event.into_plugin_event() {
                    Some(event) => OutputEventItem::Owned(event),
                    None => continue,
                },
            };
            if let Err(item) = self.items.push(item) {
                retire(item);
            }
        }
    }

    fn pop_item(&self) -> Option<OutputEventItem> {
        let _reader = self.enter_pop()?;
        self.items.pop()
    }

    fn enter_pop(&self) -> Option<PopGateReader<'_>> {
        let mut state = self.pop_gate.load(Ordering::Acquire);
        loop {
            if state & POP_GATE_PAUSED != 0 {
                return None;
            }
            debug_assert!(state < POP_GATE_PAUSED - 1);
            match self.pop_gate.compare_exchange_weak(
                state,
                state + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(PopGateReader {
                        gate: &self.pop_gate,
                    })
                }
                Err(next) => state = next,
            }
        }
    }

    fn pause_pops(&self) -> PopGatePause<'_> {
        let mut state = self.pop_gate.load(Ordering::Acquire);
        loop {
            if state & POP_GATE_PAUSED != 0 {
                std::thread::yield_now();
                state = self.pop_gate.load(Ordering::Acquire);
                continue;
            }
            match self.pop_gate.compare_exchange_weak(
                state,
                state | POP_GATE_PAUSED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(next) => state = next,
            }
        }
        while self.pop_gate.load(Ordering::Acquire) != POP_GATE_PAUSED {
            std::thread::yield_now();
        }
        PopGatePause {
            gate: &self.pop_gate,
        }
    }
}

struct PopGateReader<'a> {
    gate: &'a AtomicUsize,
}

impl Drop for PopGateReader<'_> {
    fn drop(&mut self) {
        self.gate.fetch_sub(1, Ordering::Release);
    }
}

#[allow(clippy::unnecessary_cast)]
fn is_midi_event(event: &Event) -> bool {
    use Event_::EventTypes_::*;

    matches!(
        event.r#type as u32,
        kind
            if kind == kNoteOnEvent as u32
                || kind == kNoteOffEvent as u32
                || kind == kPolyPressureEvent as u32
                || kind == kLegacyMIDICCOutEvent as u32
    )
}

unsafe fn copy_bytes(
    slot: &PreparedEventSlot,
    source: *const u8,
    length: usize,
    capacity: usize,
) -> bool {
    if length > capacity || length > MAX_EVENT_PAYLOAD_BYTES || (length != 0 && source.is_null()) {
        return false;
    }
    if length != 0 {
        // SAFETY: caller owns the slot; the checked length fits its initialized payload arena.
        // The non-null source is read before `addEvent` returns.
        unsafe {
            ptr::copy_nonoverlapping(
                source,
                (&mut *slot.payload.get()).as_mut_ptr().cast(),
                length,
            )
        };
    }
    true
}

unsafe fn copy_text(
    slot: &PreparedEventSlot,
    source: *const u16,
    length: usize,
    capacity_bytes: usize,
) -> bool {
    let Some(length_bytes) = length.checked_mul(mem::size_of::<u16>()) else {
        return false;
    };
    if length_bytes > capacity_bytes
        || length > MAX_EVENT_TEXT_UNITS
        || (length != 0 && source.is_null())
        || (length != 0 && (source as usize) % mem::align_of::<u16>() != 0)
    {
        return false;
    }
    if length != 0 {
        // SAFETY: caller owns the slot; checked byte length fits the initialized UTF-16 arena.
        // The non-null source is copied synchronously during `addEvent`.
        unsafe {
            ptr::copy_nonoverlapping(source, (&mut *slot.payload.get()).as_mut_ptr(), length)
        };
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::PluginEventData;
    use std::sync::Arc;
    use vst3::Steinberg::Vst::{DataEvent, Event_, NoteExpressionTextEvent};

    fn data_event(bytes: *const u8, size: u32) -> Event {
        let mut event: Event = unsafe { mem::zeroed() };
        event.r#type = Event_::EventTypes_::kDataEvent as u16;
        event.__field0.data = DataEvent {
            r#type: 0,
            size,
            bytes,
        };
        event
    }

    fn text_event(text: *const u16, text_len: u32) -> Event {
        let mut event: Event = unsafe { mem::zeroed() };
        event.r#type = Event_::EventTypes_::kNoteExpressionTextEvent as u16;
        event.__field0.noteExpressionText = NoteExpressionTextEvent {
            typeId: 7,
            noteId: 12,
            textLen: text_len,
            text,
        };
        event
    }

    #[test]
    fn copies_payload_before_plugin_reuses_its_source() {
        let pool = PreparedEventPool::new(1, 4);
        let mut source = [1, 2, 3, 4];
        let event = data_event(source.as_ptr(), source.len() as u32);
        let copied = unsafe { pool.copy_event(&event) }.expect("within prepared capacity");
        source.fill(9);

        let owned = copied.into_plugin_event().expect("known VST event");
        assert_eq!(
            owned.data,
            PluginEventData::Data {
                data_type: 0,
                bytes: vec![1, 2, 3, 4],
            }
        );
    }

    #[test]
    fn rejects_null_or_oversized_payload_without_losing_the_slot() {
        let pool = PreparedEventPool::new(1, 3);
        assert!(unsafe { pool.copy_event(&data_event(ptr::null(), 1)) }.is_none());
        assert!(unsafe { pool.copy_event(&data_event([1, 2, 3, 4].as_ptr(), 4)) }.is_none());
        assert!(unsafe { pool.copy_event(&data_event([1, 2, 3].as_ptr(), 3)) }.is_some());
    }

    #[test]
    fn copies_utf16_at_the_exact_byte_capacity() {
        let pool = PreparedEventPool::new(1, 4);
        let mut source = [0x41_u16, 0x42];
        let copied = unsafe { pool.copy_event(&text_event(source.as_ptr(), 2)) }
            .expect("two UTF-16 units use exactly four bytes");
        source.fill(0x58);

        let owned = copied.into_plugin_event().expect("known VST event");
        assert_eq!(
            owned.data,
            PluginEventData::NoteExpressionText {
                type_id: 7,
                note_id: 12,
                text: vec![0x41, 0x42],
            }
        );
    }

    #[test]
    fn rejects_unknown_event_kinds_and_global_payload_limits() {
        let pool = PreparedEventPool::new(1, MAX_EVENT_PAYLOAD_BYTES + 1);
        let mut unknown: Event = unsafe { mem::zeroed() };
        unknown.r#type = 999;
        assert!(unsafe { pool.copy_event(&unknown) }.is_none());

        let oversized = vec![0_u8; MAX_EVENT_PAYLOAD_BYTES + 1];
        assert!(unsafe {
            pool.copy_event(&data_event(oversized.as_ptr(), oversized.len() as u32))
        }
        .is_none());
        assert!(unsafe { pool.copy_event(&text_event(ptr::null(), 1)) }.is_none());
        let bytes = [0_u8; 3];
        // SAFETY: the pointer is never dereferenced: the pool rejects its invalid alignment.
        let unaligned_text = unsafe { bytes.as_ptr().add(1).cast::<u16>() };
        assert!(unsafe { pool.copy_event(&text_event(unaligned_text, 1)) }.is_none());
    }

    #[test]
    fn recycles_slots_across_blocks() {
        let pool = PreparedEventPool::new(1, 4);
        let first = unsafe { pool.copy_event(&data_event([1].as_ptr(), 1)) }.expect("first slot");
        first.recycle();
        assert!(unsafe { pool.copy_event(&data_event([2].as_ptr(), 1)) }.is_some());
    }

    #[test]
    fn concurrent_consumers_own_their_slots() {
        let pool = PreparedEventPool::new(2, 4);
        let queue = OutputEventQueue::new(2);
        for byte in [1_u8, 2] {
            let event =
                unsafe { pool.copy_event(&data_event([byte].as_ptr(), 1)) }.expect("prepared slot");
            assert!(queue.force_push_prepared(event, &pool).is_none());
        }
        let first = Arc::clone(&queue);
        let second = Arc::clone(&queue);
        let first = std::thread::spawn(move || first.pop_event());
        let second = std::thread::spawn(move || second.pop_event());
        let values = [
            first.join().expect("thread"),
            second.join().expect("thread"),
        ]
        .into_iter()
        .flatten()
        .filter_map(|event| match event.data {
            PluginEventData::Data { bytes, .. } => bytes.first().copied(),
            _ => None,
        })
        .collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values.contains(&1));
        assert!(values.contains(&2));
    }

    #[test]
    fn materializing_preserves_order_and_pauses_consumers() {
        let pool = PreparedEventPool::new(3, 1);
        let queue = OutputEventQueue::new(3);
        for byte in [1_u8, 2, 3] {
            let event =
                unsafe { pool.copy_event(&data_event([byte].as_ptr(), 1)) }.expect("prepared slot");
            assert!(queue.force_push_prepared(event, &pool).is_none());
        }

        let pause = queue.pause_pops();
        assert!(queue.pop_event().is_none());
        drop(pause);
        queue.materialize_prepared_events(drop);

        let values = std::iter::from_fn(|| queue.pop_event())
            .filter_map(|event| match event.data {
                PluginEventData::Data { bytes, .. } => bytes.first().copied(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(values, [1, 2, 3]);
    }
}
