//! Internal COM interface implementations for VST3

use std::collections::HashMap;
use std::ffi::CStr;
use std::ptr;
use std::sync::{Arc, Mutex};
use vst3::{Class, ComWrapper, Interface, Steinberg::Vst::*, Steinberg::*};

// Host Application implementation.
//
// Many plugins (u-he, Waves, ...) query the context passed to `IComponent::initialize`
// for `IHostApplication` and dereference it. Passing a null context makes them crash.
// Providing a real host-application object that at least answers `getName` lets them
// initialize. (We don't yet vend host-created objects like IMessage/IAttributeList.)
pub struct HostApplication;

impl Class for HostApplication {
    // The standard SDK host context implements both IHostApplication and
    // IPlugInterfaceSupport; plugins query the context for either.
    type Interfaces = (IHostApplication, IPlugInterfaceSupport);
}

impl IPlugInterfaceSupportTrait for HostApplication {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        if iid.is_null() {
            return kResultFalse;
        }
        // Advertise the host-side interfaces we genuinely provide (the component handler
        // installed on the controller); decline the rest so plugins use their defaults.
        let bytes = std::slice::from_raw_parts(iid as *const u8, 16);
        if bytes == &IComponentHandler::IID[..] || bytes == &IComponentHandler2::IID[..] {
            kResultTrue
        } else {
            kResultFalse
        }
    }
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
            dst[i] = ch;
            i += 1;
        }
        dst[i] = 0;
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut std::ffi::c_void,
    ) -> tresult {
        // Vend the host-created objects plugins ask for (the SDK's HostApplication does
        // this): IMessage and IAttributeList, used to pass data between a plugin's
        // component and controller halves. Anything else fails cleanly.
        if obj.is_null() || cid.is_null() {
            return kResultFalse;
        }
        *obj = ptr::null_mut();

        // Compare the requested class id to a known IID by raw bytes (TUID element type
        // is platform-dependent, so avoid a typed array compare).
        let cid_bytes = std::slice::from_raw_parts(cid as *const u8, 16);
        let matches = |iid: &[u8; 16]| cid_bytes == &iid[..];

        if matches(&IMessage::IID) {
            if let Some(p) = create_host_message().to_com_ptr::<IMessage>() {
                *obj = p.into_raw() as *mut std::ffi::c_void;
                return kResultTrue;
            }
        } else if matches(&IAttributeList::IID) {
            if let Some(p) = create_host_attribute_list().to_com_ptr::<IAttributeList>() {
                *obj = p.into_raw() as *mut std::ffi::c_void;
                return kResultTrue;
            }
        }
        kResultFalse
    }
}

/// Create a host-application context to pass to `IComponent::initialize`.
pub fn create_host_application() -> ComWrapper<HostApplication> {
    ComWrapper::new(HostApplication)
}

// A host-side IAttributeList: a typed key/value bag plugins use (via the host's
// createInstance) to pass data between their component and controller halves.
#[derive(Debug, Clone, PartialEq)]
enum AttrValue {
    Int(i64),
    Float(f64),
    /// UTF-16 (TChar) string, not null-terminated.
    Str(Vec<u16>),
    Bin(Vec<u8>),
}

/// Host implementation of `IAttributeList`.
#[derive(Default)]
pub struct HostAttributeList {
    attrs: Mutex<HashMap<String, AttrValue>>,
}

impl HostAttributeList {
    pub fn new() -> Self {
        Self::default()
    }

    // Safe inner API (also the unit-test surface).
    fn put(&self, key: String, value: AttrValue) {
        if let Ok(mut m) = self.attrs.lock() {
            m.insert(key, value);
        }
    }
    fn get_value(&self, key: &str) -> Option<AttrValue> {
        self.attrs.lock().ok().and_then(|m| m.get(key).cloned())
    }
}

/// Decode an `AttrID` (a C string) into an owned key.
unsafe fn attr_key(id: *const std::os::raw::c_char) -> String {
    if id.is_null() {
        return String::new();
    }
    CStr::from_ptr(id).to_string_lossy().into_owned()
}

impl Class for HostAttributeList {
    type Interfaces = (IAttributeList,);
}

impl IAttributeListTrait for HostAttributeList {
    unsafe fn setInt(&self, id: *const std::os::raw::c_char, value: i64) -> tresult {
        self.put(attr_key(id), AttrValue::Int(value));
        kResultOk
    }
    unsafe fn getInt(&self, id: *const std::os::raw::c_char, value: *mut i64) -> tresult {
        match self.get_value(&attr_key(id)) {
            Some(AttrValue::Int(v)) if !value.is_null() => {
                *value = v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setFloat(&self, id: *const std::os::raw::c_char, value: f64) -> tresult {
        self.put(attr_key(id), AttrValue::Float(value));
        kResultOk
    }
    unsafe fn getFloat(&self, id: *const std::os::raw::c_char, value: *mut f64) -> tresult {
        match self.get_value(&attr_key(id)) {
            Some(AttrValue::Float(v)) if !value.is_null() => {
                *value = v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setString(&self, id: *const std::os::raw::c_char, string: *const u16) -> tresult {
        if string.is_null() {
            return kResultFalse;
        }
        let mut buf = Vec::new();
        let mut p = string;
        while *p != 0 {
            buf.push(*p);
            p = p.add(1);
        }
        self.put(attr_key(id), AttrValue::Str(buf));
        kResultOk
    }
    unsafe fn getString(
        &self,
        id: *const std::os::raw::c_char,
        string: *mut u16,
        size_in_bytes: u32,
    ) -> tresult {
        match self.get_value(&attr_key(id)) {
            Some(AttrValue::Str(v)) if !string.is_null() => {
                // Copy up to capacity-1 chars, then null-terminate.
                let cap_chars = (size_in_bytes as usize / 2).saturating_sub(1);
                let n = v.len().min(cap_chars);
                for (i, &ch) in v.iter().take(n).enumerate() {
                    *string.add(i) = ch;
                }
                *string.add(n) = 0;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setBinary(
        &self,
        id: *const std::os::raw::c_char,
        data: *const std::ffi::c_void,
        size_in_bytes: u32,
    ) -> tresult {
        if data.is_null() {
            return kResultFalse;
        }
        let bytes = std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize).to_vec();
        self.put(attr_key(id), AttrValue::Bin(bytes));
        kResultOk
    }
    unsafe fn getBinary(
        &self,
        id: *const std::os::raw::c_char,
        data: *mut *const std::ffi::c_void,
        size_in_bytes: *mut u32,
    ) -> tresult {
        // Note: returns a pointer into the stored buffer; valid until the entry is
        // replaced. VST3 plugins read it synchronously during init, which is safe here.
        if data.is_null() || size_in_bytes.is_null() {
            return kResultFalse;
        }
        if let Ok(m) = self.attrs.lock() {
            if let Some(AttrValue::Bin(v)) = m.get(&attr_key(id)) {
                *data = v.as_ptr() as *const std::ffi::c_void;
                *size_in_bytes = v.len() as u32;
                return kResultOk;
            }
        }
        kResultFalse
    }
}

/// Create a host attribute list.
pub fn create_host_attribute_list() -> ComWrapper<HostAttributeList> {
    ComWrapper::new(HostAttributeList::new())
}

/// Host implementation of `IMessage` (an id + an attribute list), used for
/// component<->controller communication that plugins allocate via the host.
pub struct HostMessage {
    id: Mutex<Option<std::ffi::CString>>,
    attributes: ComWrapper<HostAttributeList>,
}

impl Default for HostMessage {
    fn default() -> Self {
        Self {
            id: Mutex::new(None),
            attributes: create_host_attribute_list(),
        }
    }
}

impl HostMessage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        // Pointer to the stored id (valid until replaced); null if unset.
        if let Ok(g) = self.id.lock() {
            if let Some(ref s) = *g {
                return s.as_ptr();
            }
        }
        ptr::null()
    }
    unsafe fn setMessageID(&self, id: FIDString) {
        if id.is_null() {
            return;
        }
        let owned = CStr::from_ptr(id).to_owned();
        if let Ok(mut g) = self.id.lock() {
            *g = Some(owned);
        }
    }
    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // Borrowed pointer to the message's own attribute list (kept alive by `self`).
        self.attributes
            .to_com_ptr::<IAttributeList>()
            .map(|p| p.as_ptr())
            .unwrap_or(ptr::null_mut())
    }
}

/// Create a host message.
pub fn create_host_message() -> ComWrapper<HostMessage> {
    ComWrapper::new(HostMessage::new())
}

// Host-side in-memory `IBStream`. Plugins serialize their state into a stream the host
// provides (`IComponent::getState`) and restore from one the host fills
// (`IComponent::setState`). This backs both with a growable byte buffer plus a cursor.
struct MemBuf {
    data: Vec<u8>,
    pos: usize,
}

/// Host implementation of `IBStream` over an in-memory buffer.
pub struct MemoryStream {
    inner: Mutex<MemBuf>,
}

impl MemoryStream {
    fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Mutex::new(MemBuf { data, pos: 0 }),
        }
    }

    /// A copy of everything written to the stream (used after `getState`).
    pub fn to_vec(&self) -> Vec<u8> {
        self.inner
            .lock()
            .map(|b| b.data.clone())
            .unwrap_or_default()
    }

    // Safe inner ops — also the unit-test surface for the read/write/seek logic.
    fn write_at_cursor(&self, src: &[u8]) -> usize {
        if let Ok(mut b) = self.inner.lock() {
            let end = b.pos + src.len();
            if end > b.data.len() {
                b.data.resize(end, 0);
            }
            let pos = b.pos;
            b.data[pos..end].copy_from_slice(src);
            b.pos = end;
            src.len()
        } else {
            0
        }
    }

    fn read_at_cursor(&self, n: usize) -> Vec<u8> {
        if let Ok(mut b) = self.inner.lock() {
            let start = b.pos.min(b.data.len());
            let end = (start + n).min(b.data.len());
            let out = b.data[start..end].to_vec();
            b.pos = end;
            out
        } else {
            Vec::new()
        }
    }

    fn seek_to(&self, pos: i64, mode: u32) -> i64 {
        if let Ok(mut b) = self.inner.lock() {
            let base = match mode {
                SEEK_CUR => b.pos as i64,
                SEEK_END => b.data.len() as i64,
                _ => 0, // SEEK_SET
            };
            let new = (base + pos).max(0);
            b.pos = new as usize;
            new
        } else {
            0
        }
    }

    fn position(&self) -> i64 {
        self.inner.lock().map(|b| b.pos as i64).unwrap_or(0)
    }
}

// IBStream seek modes — fixed by the VST3 ABI. kIBSeekSet (0) is the `_` arm in seek_to.
const SEEK_CUR: u32 = 1; // kIBSeekCur
const SEEK_END: u32 = 2; // kIBSeekEnd

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(
        &self,
        buffer: *mut std::ffi::c_void,
        num_bytes: i32,
        num_bytes_read: *mut i32,
    ) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kResultFalse;
        }
        let bytes = self.read_at_cursor(num_bytes as usize);
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len());
        if !num_bytes_read.is_null() {
            *num_bytes_read = bytes.len() as i32;
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut std::ffi::c_void,
        num_bytes: i32,
        num_bytes_written: *mut i32,
    ) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kResultFalse;
        }
        let src = std::slice::from_raw_parts(buffer as *const u8, num_bytes as usize);
        let written = self.write_at_cursor(src);
        if !num_bytes_written.is_null() {
            *num_bytes_written = written as i32;
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> tresult {
        let new = self.seek_to(pos, mode as u32);
        if !result.is_null() {
            *result = new;
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        if pos.is_null() {
            return kResultFalse;
        }
        *pos = self.position();
        kResultOk
    }
}

/// Create an empty stream for the plugin to write its state into (`getState`).
pub fn create_memory_stream() -> ComWrapper<MemoryStream> {
    ComWrapper::new(MemoryStream::new(Vec::new()))
}

/// Create a stream seeded with bytes for the plugin to read its state from (`setState`).
pub fn create_memory_stream_from(data: Vec<u8>) -> ComWrapper<MemoryStream> {
    ComWrapper::new(MemoryStream::new(data))
}

// Host implementation of `IPlugFrame`. A plugin editor calls `resizeView` to ask the host
// to resize the window hosting its view. We record the requested size; the host polls it
// (take_editor_resize_request) and resizes its container on the UI thread.
pub struct HostPlugFrame {
    requested: Arc<Mutex<Option<(i32, i32)>>>,
}

impl HostPlugFrame {
    pub fn new(requested: Arc<Mutex<Option<(i32, i32)>>>) -> Self {
        Self { requested }
    }
}

impl Class for HostPlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for HostPlugFrame {
    unsafe fn resizeView(&self, _view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kResultFalse;
        }
        let r = &*new_size;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if let Ok(mut slot) = self.requested.lock() {
            *slot = Some((w, h));
        }
        // The host resizes its container to (w, h) from take_editor_resize_request on the
        // UI thread; acknowledging here is enough for the plugin to proceed.
        kResultOk
    }
}

/// Create a host plug-frame backed by a shared resize-request slot.
pub fn create_host_plug_frame(
    requested: Arc<Mutex<Option<(i32, i32)>>>,
) -> ComWrapper<HostPlugFrame> {
    ComWrapper::new(HostPlugFrame::new(requested))
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

impl ParameterChanges {
    /// Host-side: queue a parameter change point for the next process block. The processor
    /// reads these from `inputParameterChanges` during `process()`. Points for the same id
    /// share one queue and are kept ordered by sample offset.
    pub fn enqueue(&self, id: u32, sample_offset: i32, value: f64) {
        if let Ok(mut queues) = self.queues.lock() {
            if let Some(idx) = queues.iter().position(|q| q.param_id == id) {
                queues[idx].insert_point(sample_offset, value);
            } else {
                let q = ComWrapper::new(ParameterValueQueue::new(id));
                q.insert_point(sample_offset, value);
                queues.push(q);
            }
        }
    }

    /// Drop all queued changes. Call after each `process()` block so values don't re-stick.
    pub fn clear_all(&self) {
        if let Ok(mut queues) = self.queues.lock() {
            queues.clear();
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

    /// Insert a point keeping sample-offset order (safe host-side population, mirroring the
    /// COM `addPoint`).
    fn insert_point(&self, sample_offset: i32, value: f64) {
        if let Ok(mut points) = self.points.lock() {
            let pos = points
                .iter()
                .position(|(off, _)| *off > sample_offset)
                .unwrap_or(points.len());
            points.insert(pos, (sample_offset, value));
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
        // These run as COM FFI callbacks; a panic (e.g. from `.unwrap()` on a poisoned
        // lock) would unwind across the C++ boundary — UB. Recover the lock instead.
        self.points.lock().unwrap_or_else(|p| p.into_inner()).len() as i32
    }

    unsafe fn getPoint(&self, index: i32, sample_offset: *mut i32, value: *mut f64) -> i32 {
        if let Some((offset, val)) = self
            .points
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(index as usize)
        {
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
        let mut points = self.points.lock().unwrap_or_else(|p| p.into_inner());

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

#[cfg(test)]
mod host_attr_tests {
    use super::*;

    #[test]
    fn attribute_list_round_trips_each_type() {
        let list = HostAttributeList::new();
        list.put("i".into(), AttrValue::Int(42));
        list.put("f".into(), AttrValue::Float(1.5));
        list.put("s".into(), AttrValue::Str(vec![72, 105])); // "Hi"
        list.put("b".into(), AttrValue::Bin(vec![1, 2, 3]));

        assert_eq!(list.get_value("i"), Some(AttrValue::Int(42)));
        assert_eq!(list.get_value("f"), Some(AttrValue::Float(1.5)));
        assert_eq!(list.get_value("s"), Some(AttrValue::Str(vec![72, 105])));
        assert_eq!(list.get_value("b"), Some(AttrValue::Bin(vec![1, 2, 3])));
        assert_eq!(list.get_value("missing"), None);
    }
}

#[cfg(test)]
mod plug_frame_tests {
    use super::*;

    #[test]
    fn records_requested_size_from_view_rect() {
        let slot = Arc::new(Mutex::new(None));
        let frame = HostPlugFrame::new(slot.clone());
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 640,
            bottom: 480,
        };
        let r = unsafe { frame.resizeView(std::ptr::null_mut(), &mut rect) };
        assert_eq!(r, kResultOk);
        assert_eq!(*slot.lock().unwrap(), Some((640, 480)));

        // Null size is rejected and leaves the slot unchanged.
        let r = unsafe { frame.resizeView(std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(r, kResultFalse);
        assert_eq!(*slot.lock().unwrap(), Some((640, 480)));
    }
}

#[cfg(test)]
mod parameter_changes_tests {
    use super::*;

    #[test]
    fn enqueue_groups_by_id_orders_by_offset_and_clears() {
        let pc = ParameterChanges::default();
        pc.enqueue(7, 64, 0.9);
        pc.enqueue(7, 0, 0.5); // earlier offset, same id → must sort before the 64 point
        pc.enqueue(3, 0, 0.1);

        // Two distinct parameter ids → two queues; the processor reads this count.
        assert_eq!(unsafe { pc.getParameterCount() }, 2);
        {
            let queues = pc.queues.lock().unwrap();
            let q7 = queues.iter().find(|q| q.param_id == 7).unwrap();
            assert_eq!(*q7.points.lock().unwrap(), vec![(0, 0.5), (64, 0.9)]);
            let q3 = queues.iter().find(|q| q.param_id == 3).unwrap();
            assert_eq!(*q3.points.lock().unwrap(), vec![(0, 0.1)]);
        }

        // After a block the host clears it so values don't re-stick.
        pc.clear_all();
        assert_eq!(unsafe { pc.getParameterCount() }, 0);
    }
}

#[cfg(test)]
mod memory_stream_tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips_from_start() {
        let s = MemoryStream::new(Vec::new());
        assert_eq!(s.write_at_cursor(&[1, 2, 3, 4]), 4);
        assert_eq!(s.position(), 4);
        assert_eq!(s.to_vec(), vec![1, 2, 3, 4]);

        // Rewind (mode 0 = SEEK_SET) and read it all back.
        assert_eq!(s.seek_to(0, 0), 0);
        assert_eq!(s.read_at_cursor(4), vec![1, 2, 3, 4]);
    }

    #[test]
    fn read_past_end_is_clamped() {
        let s = MemoryStream::new(vec![9, 8]);
        assert_eq!(s.read_at_cursor(10), vec![9, 8]);
        // Cursor now at end; further reads yield nothing.
        assert_eq!(s.read_at_cursor(10), Vec::<u8>::new());
    }

    #[test]
    fn seek_modes_and_overwrite() {
        let s = MemoryStream::new(vec![0, 0, 0, 0]);
        // SEEK_END then write appends.
        assert_eq!(s.seek_to(0, SEEK_END), 4);
        s.write_at_cursor(&[5]);
        assert_eq!(s.to_vec(), vec![0, 0, 0, 0, 5]);
        // SEEK_SET to 1 then overwrite in place.
        assert_eq!(s.seek_to(1, 0), 1);
        s.write_at_cursor(&[7, 7]);
        assert_eq!(s.to_vec(), vec![0, 7, 7, 0, 5]);
        // SEEK_CUR is relative.
        assert_eq!(s.seek_to(-3, SEEK_CUR), 0);
    }
}
