use std::cell::RefCell;
use std::ptr;
use vst3::{Class, ComWrapper, Steinberg::{self, *}, Steinberg::Vst::*};

// Component Handler implementation
pub struct ComponentHandler {
    // Empty for now, but can be extended
}

impl ComponentHandler {
    pub fn new() -> Self {
        ComponentHandler {}
    }
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: u32) -> i32 {
        println!("🎛️ Host: Begin edit for parameter {}", id);
        kResultOk
    }

    unsafe fn performEdit(&self, id: u32, value_normalized: f64) -> i32 {
        println!(
            "🎛️ Host: Perform edit for parameter {} = {}",
            id, value_normalized
        );
        kResultOk
    }

    unsafe fn endEdit(&self, id: u32) -> i32 {
        println!("🎛️ Host: End edit for parameter {}", id);
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: i32) -> i32 {
        println!("🎛️ Host: Restart component requested with flags: {}", flags);
        kResultOk
    }
}

// Event List implementation
pub struct MyEventList {
    pub events: RefCell<Vec<Event>>,
}

impl MyEventList {
    pub fn new() -> Self {
        Self {
            events: RefCell::new(Vec::new()),
        }
    }
}

impl Class for MyEventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for MyEventList {
    unsafe fn getEventCount(&self) -> i32 {
        let count = self.events.borrow().len() as i32;
        println!("[DEBUG] Plugin calling getEventCount, returning: {}", count);
        count
    }
    
    unsafe fn getEvent(&self, index: i32, event: *mut Event) -> i32 {
        if let Some(e) = self.events.borrow().get(index as usize) {
            *event = *e;
            kResultOk
        } else {
            kResultFalse
        }
    }
    
    unsafe fn addEvent(&self, event: *mut Event) -> i32 {
        if !event.is_null() {
            println!("[DEBUG] Plugin calling addEvent!");
            self.events.borrow_mut().push(*event);
            kResultOk
        } else {
            kResultFalse
        }
    }
}

pub fn create_event_list() -> ComWrapper<MyEventList> {
    ComWrapper::new(MyEventList::new())
}

// Parameter Changes implementation
#[derive(Default)]
pub struct ParameterChanges {
    pub queues: RefCell<Vec<ComWrapper<ParameterValueQueue>>>,
}

impl Class for ParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        self.queues.borrow().len() as i32
    }
    
    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if let Some(queue) = self.queues.borrow().get(index as usize) {
            queue.to_com_ptr::<IParamValueQueue>()
                .map(|ptr| ptr.into_raw())
                .unwrap_or(ptr::null_mut())
        } else {
            ptr::null_mut()
        }
    }
    
    unsafe fn addParameterData(&self, id: *const u32, index: *mut i32) -> *mut IParamValueQueue {
        if id.is_null() {
            return ptr::null_mut();
        }
        
        let param_id = *id;
        let mut queues = self.queues.borrow_mut();
        
        // Check if queue for this parameter already exists
        for (i, queue) in queues.iter().enumerate() {
            if queue.param_id == param_id {
                if !index.is_null() {
                    *index = i as i32;
                }
                return queue.to_com_ptr::<IParamValueQueue>()
                    .map(|ptr| ptr.into_raw())
                    .unwrap_or(ptr::null_mut());
            }
        }
        
        // Create new queue
        let new_queue = ComWrapper::new(ParameterValueQueue::new(param_id));
        let queue_ptr = new_queue.to_com_ptr::<IParamValueQueue>()
            .map(|ptr| ptr.into_raw())
            .unwrap_or(ptr::null_mut());
        
        if !index.is_null() {
            *index = queues.len() as i32;
        }
        
        queues.push(new_queue);
        queue_ptr
    }
}

// Parameter Value Queue implementation
pub struct ParameterValueQueue {
    pub param_id: u32,
    pub points: RefCell<Vec<(i32, f64)>>, // sample offset, value
}

impl ParameterValueQueue {
    pub fn new(param_id: u32) -> Self {
        Self {
            param_id,
            points: RefCell::new(Vec::new()),
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
        self.points.borrow().len() as i32
    }
    
    unsafe fn getPoint(&self, index: i32, sample_offset: *mut i32, value: *mut f64) -> i32 {
        if let Some((offset, val)) = self.points.borrow().get(index as usize) {
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
        let mut points = self.points.borrow_mut();
        
        // Find insertion point
        let insert_pos = points.iter().position(|(offset, _)| *offset > sample_offset)
            .unwrap_or(points.len());
        
        points.insert(insert_pos, (sample_offset, value));
        
        if !index.is_null() {
            *index = insert_pos as i32;
        }
        
        kResultOk
    }
}