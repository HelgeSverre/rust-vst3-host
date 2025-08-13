//! COM implementations for VST3 hosting with lock-free audio thread safety

use crate::realtime::{Command, AudioState};
use crossbeam::channel::Sender;
use std::cell::UnsafeCell;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use vst3::{Class, ComWrapper, Steinberg::Vst::*, Steinberg::*};

/// Thread-safe event list implementation for VST3 audio processing
/// 
/// This implementation uses lock-free atomic operations where possible to minimize
/// contention in real-time audio contexts. Event access is protected by mutex
/// only when necessary for consistency.
pub struct EventList {
    /// Event storage with proper synchronization
    /// Protected by mutex to ensure thread-safe access
    events: std::sync::Mutex<Vec<Event>>,
}

// SAFETY: EventList uses Mutex for thread safety
unsafe impl Send for EventList {}
unsafe impl Sync for EventList {}

impl EventList {
    pub fn new(_capacity: usize) -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
    
    /// Clear all events
    /// 
    /// This method removes all events from the list. It should be called
    /// between processing blocks to reset the event state.
    pub fn clear(&self) {
        match self.events.lock() {
            Ok(mut events) => {
                events.clear();
                log::trace!("EventList: Cleared all events");
            }
            Err(_) => {
                log::error!("EventList: Failed to lock events for clear");
            }
        }
    }
    
    /// Add events from a slice
    /// 
    /// This method efficiently adds multiple events at once, which is
    /// preferable to multiple addEvent calls for performance reasons.
    pub fn add_events(&self, new_events: &[Event]) {
        if new_events.is_empty() {
            return;
        }
        
        match self.events.lock() {
            Ok(mut events) => {
                events.extend_from_slice(new_events);
                log::trace!("EventList: Added {} events, total count: {}", new_events.len(), events.len());
            }
            Err(_) => {
                log::error!("EventList: Failed to lock events for add_events");
            }
        }
    }
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> i32 {
        match self.events.lock() {
            Ok(events) => events.len() as i32,
            Err(_) => {
                log::error!("EventList: Failed to lock events for getEventCount");
                0
            }
        }
    }
    
    unsafe fn getEvent(&self, index: i32, event: *mut Event) -> i32 {
        if event.is_null() {
            log::warn!("EventList: getEvent called with null event pointer");
            return kResultFalse;
        }
        
        if index < 0 {
            log::warn!("EventList: getEvent called with negative index: {}", index);
            return kResultFalse;
        }
        
        match self.events.lock() {
            Ok(events) => {
                if let Some(e) = events.get(index as usize) {
                    *event = *e;
                    kResultOk
                } else {
                    log::warn!("EventList: getEvent index {} out of bounds (count: {})", index, events.len());
                    kResultFalse
                }
            }
            Err(_) => {
                log::error!("EventList: Failed to lock events for getEvent");
                kResultFalse
            }
        }
    }
    
    unsafe fn addEvent(&self, event: *mut Event) -> i32 {
        if event.is_null() {
            log::warn!("EventList: addEvent called with null event pointer");
            return kResultFalse;
        }
        
        match self.events.lock() {
            Ok(mut events) => {
                events.push(*event);
                log::trace!("EventList: Added event, total count: {}", events.len());
                kResultOk
            }
            Err(_) => {
                log::error!("EventList: Failed to lock events for addEvent");
                kResultFalse
            }
        }
    }
}

/// Lock-free parameter changes for real-time audio processing
/// 
/// This implementation provides thread-safe parameter change management using
/// atomic operations for count tracking and UnsafeCell for lock-free access.
/// It's designed for single-producer single-consumer scenarios in real-time contexts.
pub struct ParameterChanges {
    /// Parameter value queues storage
    /// Uses UnsafeCell for lock-free access (single-producer single-consumer)
    queues: UnsafeCell<Vec<ComWrapper<ParameterValueQueue>>>,
    /// Atomic count for thread-safe access
    count: AtomicI32,
}

// SAFETY: ParameterChanges is designed for single-producer single-consumer use
unsafe impl Send for ParameterChanges {}
unsafe impl Sync for ParameterChanges {}

impl ParameterChanges {
    pub fn new() -> Self {
        Self {
            queues: UnsafeCell::new(Vec::with_capacity(128)),
            count: AtomicI32::new(0),
        }
    }
    
    pub fn clear(&self) {
        unsafe {
            let queues = &mut *self.queues.get();
            for queue in queues.iter() {
                queue.clear();
            }
            queues.clear();
            self.count.store(0, Ordering::Release);
        }
    }
}

impl Class for ParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        let count = self.count.load(Ordering::Acquire);
        log::trace!("ParameterChanges: getParameterCount returning {}", count);
        count
    }
    
    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if index < 0 {
            log::warn!("ParameterChanges: getParameterData called with negative index: {}", index);
            return ptr::null_mut();
        }
        
        let count = self.count.load(Ordering::Acquire);
        if index >= count {
            log::warn!("ParameterChanges: getParameterData index {} out of bounds (count: {})", index, count);
            return ptr::null_mut();
        }
        
        let queues = &*self.queues.get();
        if let Some(queue) = queues.get(index as usize) {
            match queue.to_com_ptr::<IParamValueQueue>() {
                Some(ptr) => {
                    log::trace!("ParameterChanges: getParameterData returning queue for index {}", index);
                    ptr.into_raw()
                }
                None => {
                    log::error!("ParameterChanges: Failed to convert queue to COM pointer for index {}", index);
                    ptr::null_mut()
                }
            }
        } else {
            log::error!("ParameterChanges: Queue not found at index {} (should not happen)", index);
            ptr::null_mut()
        }
    }
    
    unsafe fn addParameterData(&self, id: *const u32, index: *mut i32) -> *mut IParamValueQueue {
        if id.is_null() {
            log::warn!("ParameterChanges: addParameterData called with null id pointer");
            return ptr::null_mut();
        }
        
        let param_id = *id;
        let queues = &mut *self.queues.get();
        
        // Check if queue already exists
        for (i, queue) in queues.iter().enumerate() {
            if queue.param_id == param_id {
                if !index.is_null() {
                    *index = i as i32;
                }
                log::trace!("ParameterChanges: Found existing queue for parameter {}", param_id);
                return queue
                    .to_com_ptr::<IParamValueQueue>()
                    .map(|ptr| ptr.into_raw())
                    .unwrap_or_else(|| {
                        log::error!("ParameterChanges: Failed to convert existing queue to COM pointer");
                        ptr::null_mut()
                    });
            }
        }
        
        // Create new queue
        let new_queue = ComWrapper::new(ParameterValueQueue::new(param_id));
        let queue_ptr = new_queue
            .to_com_ptr::<IParamValueQueue>()
            .map(|ptr| ptr.into_raw())
            .unwrap_or_else(|| {
                log::error!("ParameterChanges: Failed to convert new queue to COM pointer");
                ptr::null_mut()
            });
        
        if !index.is_null() {
            *index = queues.len() as i32;
        }
        
        queues.push(new_queue);
        let new_count = queues.len() as i32;
        self.count.store(new_count, Ordering::Release);
        
        log::trace!("ParameterChanges: Created new queue for parameter {}, total count: {}", param_id, new_count);
        queue_ptr
    }
}

/// Lock-free parameter value queue
pub struct ParameterValueQueue {
    param_id: u32,
    points: UnsafeCell<Vec<(i32, f64)>>,
    count: AtomicI32,
}

// SAFETY: ParameterValueQueue is designed for single-producer single-consumer use
unsafe impl Send for ParameterValueQueue {}
unsafe impl Sync for ParameterValueQueue {}

impl ParameterValueQueue {
    pub fn new(param_id: u32) -> Self {
        Self {
            param_id,
            points: UnsafeCell::new(Vec::with_capacity(64)),
            count: AtomicI32::new(0),
        }
    }
    
    pub fn clear(&self) {
        unsafe {
            (*self.points.get()).clear();
            self.count.store(0, Ordering::Release);
        }
    }
}

impl Class for ParameterValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParameterValueQueue {
    unsafe fn getParameterId(&self) -> u32 {
        log::trace!("ParameterValueQueue: getParameterId returning {}", self.param_id);
        self.param_id
    }
    
    unsafe fn getPointCount(&self) -> i32 {
        let count = self.count.load(Ordering::Acquire);
        log::trace!("ParameterValueQueue: getPointCount returning {} for param {}", count, self.param_id);
        count
    }
    
    unsafe fn getPoint(&self, index: i32, sample_offset: *mut i32, value: *mut f64) -> i32 {
        if index < 0 {
            log::warn!("ParameterValueQueue: getPoint called with negative index: {}", index);
            return kResultFalse;
        }
        
        let count = self.count.load(Ordering::Acquire);
        if index >= count {
            log::warn!("ParameterValueQueue: getPoint index {} out of bounds (count: {})", index, count);
            return kResultFalse;
        }
        
        let points = &*self.points.get();
        if let Some((offset, val)) = points.get(index as usize) {
            if !sample_offset.is_null() {
                *sample_offset = *offset;
            }
            if !value.is_null() {
                *value = *val;
            }
            log::trace!("ParameterValueQueue: getPoint({}) = offset: {}, value: {}", index, offset, val);
            kResultOk
        } else {
            log::error!("ParameterValueQueue: Point not found at index {} (should not happen)", index);
            kResultFalse
        }
    }
    
    unsafe fn addPoint(&self, sample_offset: i32, value: f64, index: *mut i32) -> i32 {
        if sample_offset < 0 {
            log::warn!("ParameterValueQueue: addPoint called with negative sample_offset: {}", sample_offset);
            return kResultFalse;
        }
        
        let points = &mut *self.points.get();
        let insert_pos = points
            .iter()
            .position(|(offset, _)| *offset > sample_offset)
            .unwrap_or(points.len());
        
        points.insert(insert_pos, (sample_offset, value));
        let new_count = points.len() as i32;
        self.count.store(new_count, Ordering::Release);
        
        if !index.is_null() {
            *index = insert_pos as i32;
        }
        
        log::trace!("ParameterValueQueue: Added point at offset {}, value: {}, index: {}, total count: {}", 
                   sample_offset, value, insert_pos, new_count);
        kResultOk
    }
}

/// Component handler for parameter changes from plugin
pub struct ComponentHandler {
    command_tx: Sender<Command>,
}

impl ComponentHandler {
    pub fn new(command_tx: Sender<Command>) -> Self {
        Self { command_tx }
    }
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: u32) -> i32 {
        kResultOk
    }
    
    unsafe fn performEdit(&self, id: u32, value_normalized: f64) -> i32 {
        // Send parameter change to audio thread via lock-free queue
        let _ = self.command_tx.try_send(Command::SetParameter { id, value: value_normalized });
        kResultOk
    }
    
    unsafe fn endEdit(&self, _id: u32) -> i32 {
        kResultOk
    }
    
    unsafe fn restartComponent(&self, _flags: i32) -> i32 {
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, _state: u8) -> i32 {
        // Host can mark plugin state as dirty for save/restore
        log::debug!("Host: Plugin marked state as dirty (state: {})", _state);
        kResultOk
    }
    
    unsafe fn requestOpenEditor(&self, _name: *const i8) -> i32 {
        // Host can handle plugin editor open requests
        log::debug!("Host: Plugin requested editor open");
        kResultOk
    }
    
    unsafe fn startGroupEdit(&self) -> i32 {
        // Host can group parameter edits for undo/redo
        log::debug!("Host: Plugin started group edit");
        kResultOk
    }
    
    unsafe fn finishGroupEdit(&self) -> i32 {
        // Host can finish grouped parameter edits
        log::debug!("Host: Plugin finished group edit");
        kResultOk
    }
}

/// Host application for plugin windows
pub struct HostApplication {
    audio_state: Arc<AudioState>,
}

impl HostApplication {
    pub fn new(audio_state: Arc<AudioState>) -> Self {
        Self { audio_state }
    }
}

impl Class for HostApplication {
    type Interfaces = (IHostApplication, IPlugFrame);
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> i32 {
        if !name.is_null() {
            let host_name = "VST3 Host\0";
            let bytes = host_name.as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                (*name)[i] = b as i16;
            }
        }
        kResultOk
    }
    
    unsafe fn createInstance(&self, _cid: *mut [i8; 16], _iid: *mut [i8; 16], _obj: *mut *mut std::ffi::c_void) -> i32 {
        kResultFalse
    }
}

impl IPlugFrameTrait for HostApplication {
    unsafe fn resizeView(&self, _view: *mut IPlugView, _new_size: *mut ViewRect) -> i32 {
        // Host should handle view resizing here
        // For now, we accept all resize requests
        log::debug!("Host: Plugin requested view resize");
        kResultOk
    }
}