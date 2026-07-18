//! Fixed-capacity VST3 COM collections for the exclusive realtime processing mode.
//!
//! VST3 collection callbacks receive `&self`, but the SDK calls the input/output event and
//! parameter containers synchronously from the one thread executing `IAudioProcessor::process`.
//! These types use `UnsafeCell` only to express that ABI-mandated interior mutability. They must
//! never be exposed to concurrent callers. Capacity is allocated before live mode; every mutation
//! checks the configured bound before touching a `Vec`, so no live operation can grow or shrink the
//! heap allocation.

use std::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
    sync::Arc,
};

use vst3::{Class, ComWrapper, Steinberg::Vst::*, Steinberg::*};

const NONE: usize = usize::MAX;

/// Fixed-capacity implementation of `IEventList` used only by an exclusively owned processor.
pub(crate) struct RealtimeEventList {
    events: UnsafeCell<Vec<Event>>,
    capacity: usize,
    overflows: AtomicUsize,
}

// SAFETY: the VST3 process contract gives the list to one processor invocation at a time. The
// public realtime owner is `&mut self`-driven and never shares these objects with another thread.
unsafe impl Sync for RealtimeEventList {}

impl RealtimeEventList {
    /// Allocates storage for at most `capacity` events before live processing starts.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            events: UnsafeCell::new(Vec::with_capacity(capacity)),
            capacity,
            overflows: AtomicUsize::new(0),
        }
    }

    /// Clears the current block while retaining the backing allocation.
    pub(crate) fn clear(&self) {
        // SAFETY: callers obey the exclusive process-thread contract documented on the type.
        unsafe { (&mut *self.events.get()).clear() };
    }

    /// Adds one host-created event without exceeding the prepared capacity.
    pub(crate) fn push(&self, event: Event) -> bool {
        // SAFETY: callers obey the exclusive process-thread contract documented on the type.
        let events = unsafe { &mut *self.events.get() };
        if events.len() == self.capacity {
            self.overflows.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        events.push(event);
        true
    }

    /// Returns the number of rejected pushes since construction.
    pub(crate) fn overflow_count(&self) -> usize {
        self.overflows.load(Ordering::Relaxed)
    }
}

impl Class for RealtimeEventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for RealtimeEventList {
    unsafe fn getEventCount(&self) -> i32 {
        (&*self.events.get()).len().min(i32::MAX as usize) as i32
    }

    unsafe fn getEvent(&self, index: i32, event: *mut Event) -> i32 {
        if index < 0 || event.is_null() {
            return kResultFalse;
        }
        match (&*self.events.get()).get(index as usize) {
            Some(value) => {
                *event = *value;
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn addEvent(&self, event: *mut Event) -> i32 {
        if event.is_null() {
            return kResultFalse;
        }
        if self.push(*event) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

/// One point in the shared fixed parameter arena.
#[derive(Clone, Copy)]
struct RealtimeParameterPoint {
    sample_offset: i32,
    value: f64,
    next: usize,
}

/// Shared point storage for every parameter queue in one `IParameterChanges` object.
struct RealtimeParameterArena {
    points: UnsafeCell<Vec<RealtimeParameterPoint>>,
    capacity: usize,
}

// SAFETY: the arena is reachable only through queues belonging to one exclusive process call.
unsafe impl Sync for RealtimeParameterArena {}

impl RealtimeParameterArena {
    fn new(capacity: usize) -> Self {
        Self {
            points: UnsafeCell::new(Vec::with_capacity(capacity)),
            capacity,
        }
    }

    fn clear(&self) {
        // SAFETY: the owning `RealtimeParameterChanges` is process-thread exclusive.
        unsafe { (&mut *self.points.get()).clear() };
    }

    fn push(&self, sample_offset: i32, value: f64) -> Option<usize> {
        // SAFETY: the owning `RealtimeParameterChanges` is process-thread exclusive.
        let points = unsafe { &mut *self.points.get() };
        if points.len() == self.capacity {
            return None;
        }
        let index = points.len();
        points.push(RealtimeParameterPoint {
            sample_offset,
            value,
            next: NONE,
        });
        Some(index)
    }

    fn get(&self, index: usize) -> Option<RealtimeParameterPoint> {
        // SAFETY: the owning `RealtimeParameterChanges` is process-thread exclusive.
        unsafe { (&*self.points.get()).get(index).copied() }
    }

    fn set_next(&self, index: usize, next: usize) {
        // SAFETY: the owning `RealtimeParameterChanges` is process-thread exclusive.
        if let Some(point) = unsafe { (&mut *self.points.get()).get_mut(index) } {
            point.next = next;
        }
    }
}

/// Fixed queue descriptor backed by a shared point arena.
pub(crate) struct RealtimeParameterValueQueue {
    param_id: AtomicU32,
    head: AtomicUsize,
    count: AtomicUsize,
    arena: Arc<RealtimeParameterArena>,
}

impl RealtimeParameterValueQueue {
    fn new(arena: Arc<RealtimeParameterArena>) -> Self {
        Self {
            param_id: AtomicU32::new(0),
            head: AtomicUsize::new(NONE),
            count: AtomicUsize::new(0),
            arena,
        }
    }

    fn reset(&self, param_id: u32) {
        self.param_id.store(param_id, Ordering::Relaxed);
        self.head.store(NONE, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
    }

    fn add_point(&self, sample_offset: i32, value: f64) -> Option<i32> {
        let new_index = self.arena.push(sample_offset, value)?;
        let mut previous = NONE;
        let mut current = self.head.load(Ordering::Relaxed);
        let mut logical_index = 0usize;
        while let Some(point) = self.arena.get(current) {
            if point.sample_offset > sample_offset {
                break;
            }
            previous = current;
            current = point.next;
            logical_index += 1;
        }
        self.arena.set_next(new_index, current);
        if previous == NONE {
            self.head.store(new_index, Ordering::Relaxed);
        } else {
            self.arena.set_next(previous, new_index);
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        Some(logical_index.min(i32::MAX as usize) as i32)
    }

    fn point_at(&self, index: usize) -> Option<RealtimeParameterPoint> {
        let mut current = self.head.load(Ordering::Relaxed);
        for _ in 0..index {
            current = self.arena.get(current)?.next;
        }
        self.arena.get(current)
    }
}

impl Class for RealtimeParameterValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for RealtimeParameterValueQueue {
    unsafe fn getParameterId(&self) -> u32 {
        self.param_id.load(Ordering::Relaxed)
    }

    unsafe fn getPointCount(&self) -> i32 {
        self.count.load(Ordering::Relaxed).min(i32::MAX as usize) as i32
    }

    unsafe fn getPoint(&self, index: i32, sample_offset: *mut i32, value: *mut f64) -> i32 {
        if index < 0 {
            return kResultFalse;
        }
        match self.point_at(index as usize) {
            Some(point) => {
                if !sample_offset.is_null() {
                    *sample_offset = point.sample_offset;
                }
                if !value.is_null() {
                    *value = point.value;
                }
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn addPoint(&self, sample_offset: i32, value: f64, index: *mut i32) -> i32 {
        match self.add_point(sample_offset, value) {
            Some(inserted) => {
                if !index.is_null() {
                    *index = inserted;
                }
                kResultOk
            }
            None => kResultFalse,
        }
    }
}

/// Fixed `IParameterChanges` implementation with precreated queues and one flat point arena.
pub(crate) struct RealtimeParameterChanges {
    queues: Vec<ComWrapper<RealtimeParameterValueQueue>>,
    arena: Arc<RealtimeParameterArena>,
    used: AtomicUsize,
    overflows: AtomicUsize,
}

impl RealtimeParameterChanges {
    /// Allocates every queue descriptor and point slot before live processing starts.
    pub(crate) fn new(max_distinct_parameters: usize, max_points: usize) -> Self {
        let arena = Arc::new(RealtimeParameterArena::new(max_points));
        let queues = (0..max_distinct_parameters)
            .map(|_| ComWrapper::new(RealtimeParameterValueQueue::new(Arc::clone(&arena))))
            .collect();
        Self {
            queues,
            arena,
            used: AtomicUsize::new(0),
            overflows: AtomicUsize::new(0),
        }
    }

    /// Clears this block while retaining every queue and arena allocation.
    pub(crate) fn clear(&self) {
        self.used.store(0, Ordering::Relaxed);
        self.arena.clear();
    }

    fn queue_for(
        &self,
        param_id: u32,
    ) -> Option<(usize, &ComWrapper<RealtimeParameterValueQueue>)> {
        let used = self.used.load(Ordering::Relaxed);
        if let Some((index, queue)) = self.queues[..used]
            .iter()
            .enumerate()
            .find(|(_, queue)| queue.param_id.load(Ordering::Relaxed) == param_id)
        {
            return Some((index, queue));
        }
        let queue = self.queues.get(used)?;
        queue.reset(param_id);
        self.used.store(used + 1, Ordering::Relaxed);
        Some((used, queue))
    }

    /// Adds one point, returning false rather than growing either prepared bound.
    pub(crate) fn enqueue(&self, param_id: u32, sample_offset: i32, value: f64) -> bool {
        let Some((_, queue)) = self.queue_for(param_id) else {
            self.overflows.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        if queue.add_point(sample_offset, value).is_none() {
            self.overflows.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Returns the active distinct-parameter count for this block.
    #[cfg(test)]
    pub(crate) fn distinct_count(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    /// Returns the number of rejected queues or points since construction.
    pub(crate) fn overflow_count(&self) -> usize {
        self.overflows.load(Ordering::Relaxed)
    }
}

impl Class for RealtimeParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for RealtimeParameterChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        self.used.load(Ordering::Relaxed).min(i32::MAX as usize) as i32
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if index < 0 || index as usize >= self.used.load(Ordering::Relaxed) {
            return ptr::null_mut();
        }
        self.queues[index as usize]
            .as_com_ref::<IParamValueQueue>()
            .map(|value| value.as_ptr())
            .unwrap_or(ptr::null_mut())
    }

    unsafe fn addParameterData(&self, id: *const u32, index: *mut i32) -> *mut IParamValueQueue {
        if id.is_null() {
            return ptr::null_mut();
        }
        let Some((queue_index, queue)) = self.queue_for(*id) else {
            self.overflows.fetch_add(1, Ordering::Relaxed);
            return ptr::null_mut();
        };
        if !index.is_null() {
            *index = queue_index.min(i32::MAX as usize) as i32;
        }
        queue
            .as_com_ref::<IParamValueQueue>()
            .map(|value| value.as_ptr())
            .unwrap_or(ptr::null_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_list_rejects_growth_and_reuses_capacity() {
        let list = RealtimeEventList::new(1);
        let event: Event = unsafe { std::mem::zeroed() };
        assert!(list.push(event));
        assert!(!list.push(event));
        assert_eq!(list.overflow_count(), 1);
        list.clear();
        assert!(list.push(event));
    }

    #[test]
    fn parameter_arena_groups_orders_and_clears_blocks() {
        let changes = RealtimeParameterChanges::new(2, 3);
        assert!(changes.enqueue(7, 64, 0.9));
        assert!(changes.enqueue(7, 0, 0.1));
        assert!(changes.enqueue(3, 8, 0.5));
        assert_eq!(changes.distinct_count(), 2);
        let q = &changes.queues[0];
        assert_eq!(q.point_at(0).map(|p| p.sample_offset), Some(0));
        assert_eq!(q.point_at(1).map(|p| p.sample_offset), Some(64));
        assert!(!changes.enqueue(3, 9, 0.6));
        changes.clear();
        assert_eq!(changes.distinct_count(), 0);
        assert!(changes.enqueue(9, 1, 0.2));
    }
}
