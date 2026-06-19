//! Internal COM interface implementations for VST3

use std::ptr;
use std::sync::{Arc, Mutex};
use vst3::{Class, ComWrapper, Steinberg::Vst::*, Steinberg::*};

// Host Application implementation.
//
// Many plugins (u-he, Waves, ...) query the context passed to `IComponent::initialize`
// for `IHostApplication` and dereference it. Passing a null context makes them crash.
// Providing a real host-application object that at least answers `getName` lets them
// initialize. (We don't yet vend host-created objects like IMessage/IAttributeList.)
pub struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kResultFalse;
        }
        let dst = &mut *name;
        let mut i = 0;
        for ch in "vst3-host".encode_utf16() {
            if i + 1 >= dst.len() {
                break;
            }
            dst[i] = ch as i16;
            i += 1;
        }
        dst[i] = 0;
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        _cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut std::ffi::c_void,
    ) -> tresult {
        // We don't provide host-created objects; fail cleanly instead of crashing on a
        // null context. Inter-component messaging via the host is simply unavailable.
        if !obj.is_null() {
            *obj = ptr::null_mut();
        }
        kResultFalse
    }
}

/// Create a host-application context to pass to `IComponent::initialize`.
pub fn create_host_application() -> ComWrapper<HostApplication> {
    ComWrapper::new(HostApplication)
}

// Component Handler implementation
pub struct ComponentHandler {
    // Track parameter changes from the plugin
    pub parameter_changes: Arc<Mutex<Vec<(u32, f64)>>>,
}

impl ComponentHandler {
    pub fn new(parameter_changes: Arc<Mutex<Vec<(u32, f64)>>>) -> Self {
        ComponentHandler { parameter_changes }
    }
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: u32) -> i32 {
        log::debug!("Host: Begin edit for parameter {}", id);
        kResultOk
    }

    unsafe fn performEdit(&self, id: u32, value_normalized: f64) -> i32 {
        log::debug!(
            "Host: Perform edit for parameter {} = {}",
            id,
            value_normalized
        );
        // Store the parameter change
        if let Ok(mut changes) = self.parameter_changes.lock() {
            changes.push((id, value_normalized));
        }
        kResultOk
    }

    unsafe fn endEdit(&self, id: u32) -> i32 {
        log::debug!("Host: End edit for parameter {}", id);
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: i32) -> i32 {
        log::debug!("Host: Restart component requested with flags: {}", flags);
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, _state: u8) -> i32 {
        log::debug!("Host: Plugin marked state as dirty (state: {})", _state);
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: *const std::os::raw::c_char) -> i32 {
        log::debug!("Host: Plugin requested editor open");
        kResultOk
    }

    unsafe fn startGroupEdit(&self) -> i32 {
        log::debug!("Host: Plugin started group edit");
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> i32 {
        log::debug!("Host: Plugin finished group edit");
        kResultOk
    }
}

// Event List implementation
pub struct HostEventList {
    pub events: Mutex<Vec<Event>>,
}

impl HostEventList {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    pub fn clear(&self) {
        match self.events.lock() {
            Ok(mut events) => {
                events.clear();
                log::trace!("HostEventList: Cleared all events");
            }
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for clear");
            }
        }
    }

    /// Take all events out of the list, leaving it empty. Used to read the events a
    /// plugin emitted into its output event list during `process()`.
    pub fn drain(&self) -> Vec<Event> {
        match self.events.lock() {
            Ok(mut events) => std::mem::take(&mut *events),
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for drain");
                Vec::new()
            }
        }
    }

    pub fn add_event(&self, event: Event) {
        match self.events.lock() {
            Ok(mut events) => {
                events.push(event);
                log::trace!(
                    "HostEventList: Added event via add_event, total count: {}",
                    events.len()
                );
            }
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for add_event");
            }
        }
    }
}

impl Default for HostEventList {
    fn default() -> Self {
        Self::new()
    }
}

impl Class for HostEventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for HostEventList {
    unsafe fn getEventCount(&self) -> i32 {
        match self.events.lock() {
            Ok(events) => events.len() as i32,
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for getEventCount");
                0
            }
        }
    }

    unsafe fn getEvent(&self, index: i32, event: *mut Event) -> i32 {
        if event.is_null() {
            log::warn!("HostEventList: getEvent called with null event pointer");
            return kResultFalse;
        }

        if index < 0 {
            log::warn!(
                "HostEventList: getEvent called with negative index: {}",
                index
            );
            return kResultFalse;
        }

        match self.events.lock() {
            Ok(events) => {
                if let Some(e) = events.get(index as usize) {
                    *event = *e;
                    kResultOk
                } else {
                    log::warn!(
                        "HostEventList: getEvent index {} out of bounds (count: {})",
                        index,
                        events.len()
                    );
                    kResultFalse
                }
            }
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for getEvent");
                kResultFalse
            }
        }
    }

    unsafe fn addEvent(&self, event: *mut Event) -> i32 {
        if event.is_null() {
            log::warn!("HostEventList: addEvent called with null event pointer");
            return kResultFalse;
        }

        match self.events.lock() {
            Ok(mut events) => {
                events.push(*event);
                log::trace!("HostEventList: Added event, total count: {}", events.len());
                kResultOk
            }
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for addEvent");
                kResultFalse
            }
        }
    }
}

pub fn create_event_list() -> ComWrapper<HostEventList> {
    ComWrapper::new(HostEventList::new())
}

// Parameter Changes implementation
pub struct ParameterChanges {
    pub queues: Mutex<Vec<ComWrapper<ParameterValueQueue>>>,
}

impl Default for ParameterChanges {
    fn default() -> Self {
        Self {
            queues: Mutex::new(Vec::new()),
        }
    }
}

impl Class for ParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        match self.queues.lock() {
            Ok(queues) => {
                let count = queues.len() as i32;
                log::trace!(
                    "Internal ParameterChanges: getParameterCount returning {}",
                    count
                );
                count
            }
            Err(_) => {
                log::error!(
                    "Internal ParameterChanges: Failed to lock queues for getParameterCount"
                );
                0
            }
        }
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if index < 0 {
            log::warn!(
                "Internal ParameterChanges: getParameterData called with negative index: {}",
                index
            );
            return ptr::null_mut();
        }

        match self.queues.lock() {
            Ok(queues) => {
                if let Some(queue) = queues.get(index as usize) {
                    match queue.to_com_ptr::<IParamValueQueue>() {
                        Some(ptr) => {
                            log::trace!("Internal ParameterChanges: getParameterData returning queue for index {}", index);
                            ptr.into_raw()
                        }
                        None => {
                            log::error!("Internal ParameterChanges: Failed to convert queue to COM pointer for index {}", index);
                            ptr::null_mut()
                        }
                    }
                } else {
                    log::warn!("Internal ParameterChanges: getParameterData index {} out of bounds (count: {})", index, queues.len());
                    ptr::null_mut()
                }
            }
            Err(_) => {
                log::error!(
                    "Internal ParameterChanges: Failed to lock queues for getParameterData"
                );
                ptr::null_mut()
            }
        }
    }

    unsafe fn addParameterData(&self, id: *const u32, index: *mut i32) -> *mut IParamValueQueue {
        if id.is_null() {
            log::warn!("Internal ParameterChanges: addParameterData called with null id pointer");
            return ptr::null_mut();
        }

        let param_id = *id;

        match self.queues.lock() {
            Ok(mut queues) => {
                // Check if queue for this parameter already exists
                for (i, queue) in queues.iter().enumerate() {
                    if queue.param_id == param_id {
                        if !index.is_null() {
                            *index = i as i32;
                        }
                        log::trace!(
                            "Internal ParameterChanges: Found existing queue for parameter {}",
                            param_id
                        );
                        return queue
                            .to_com_ptr::<IParamValueQueue>()
                            .map(|ptr| ptr.into_raw())
                            .unwrap_or_else(|| {
                                log::error!("Internal ParameterChanges: Failed to convert existing queue to COM pointer");
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
                        log::error!(
                            "Internal ParameterChanges: Failed to convert new queue to COM pointer"
                        );
                        ptr::null_mut()
                    });

                if !index.is_null() {
                    *index = queues.len() as i32;
                }

                queues.push(new_queue);
                let new_count = queues.len();
                log::trace!("Internal ParameterChanges: Created new queue for parameter {}, total count: {}", param_id, new_count);
                queue_ptr
            }
            Err(_) => {
                log::error!(
                    "Internal ParameterChanges: Failed to lock queues for addParameterData"
                );
                ptr::null_mut()
            }
        }
    }
}

// Parameter Value Queue implementation
pub struct ParameterValueQueue {
    pub param_id: u32,
    pub points: Mutex<Vec<(i32, f64)>>, // sample offset, value
}

impl ParameterValueQueue {
    pub fn new(param_id: u32) -> Self {
        Self {
            param_id,
            points: Mutex::new(Vec::new()),
        }
    }
}

impl Class for ParameterValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParameterValueQueue {
    unsafe fn getParameterId(&self) -> u32 {
        self.param_id
    }

    unsafe fn getPointCount(&self) -> i32 {
        self.points.lock().unwrap().len() as i32
    }

    unsafe fn getPoint(&self, index: i32, sample_offset: *mut i32, value: *mut f64) -> i32 {
        if let Some((offset, val)) = self.points.lock().unwrap().get(index as usize) {
            if !sample_offset.is_null() {
                *sample_offset = *offset;
            }
            if !value.is_null() {
                *value = *val;
            }
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn addPoint(&self, sample_offset: i32, value: f64, index: *mut i32) -> i32 {
        let mut points = self.points.lock().unwrap();

        // Find insertion point
        let insert_pos = points
            .iter()
            .position(|(offset, _)| *offset > sample_offset)
            .unwrap_or(points.len());

        points.insert(insert_pos, (sample_offset, value));

        if !index.is_null() {
            *index = insert_pos as i32;
        }

        kResultOk
    }
}
