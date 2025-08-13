//! Memory-based IBStream implementation for VST3 state transfer

use std::cell::RefCell;
use std::cmp;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use vst3::{Class, ComWrapper, Steinberg::*, Steinberg::Vst::*};

/// Memory-based stream for VST3 state transfer
pub struct MemoryStream {
    buffer: RefCell<Cursor<Vec<u8>>>,
}

impl MemoryStream {
    pub fn new() -> Self {
        Self {
            buffer: RefCell::new(Cursor::new(Vec::new())),
        }
    }
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut std::ffi::c_void, num_bytes: i32, num_bytes_read: *mut i32) -> i32 {
        if buffer.is_null() || num_bytes < 0 {
            return kResultFalse;
        }
        
        let slice = std::slice::from_raw_parts_mut(buffer as *mut u8, num_bytes as usize);
        match self.buffer.borrow_mut().read(slice) {
            Ok(n) => {
                if !num_bytes_read.is_null() {
                    *num_bytes_read = n as i32;
                }
                kResultOk
            }
            Err(_) => kResultFalse,
        }
    }
    
    unsafe fn write(&self, buffer: *mut std::ffi::c_void, num_bytes: i32, num_bytes_written: *mut i32) -> i32 {
        if buffer.is_null() || num_bytes < 0 {
            return kResultFalse;
        }
        
        let slice = std::slice::from_raw_parts(buffer as *const u8, num_bytes as usize);
        match self.buffer.borrow_mut().write(slice) {
            Ok(n) => {
                if !num_bytes_written.is_null() {
                    *num_bytes_written = n as i32;
                }
                kResultOk
            }
            Err(_) => kResultFalse,
        }
    }
    
    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> i32 {
        let seek_from = match mode {
            0 => SeekFrom::Start(pos as u64), // kIBSeekSet
            1 => SeekFrom::Current(pos),       // kIBSeekCur
            2 => SeekFrom::End(pos),          // kIBSeekEnd
            _ => return kResultFalse,
        };
        
        match self.buffer.borrow_mut().seek(seek_from) {
            Ok(new_pos) => {
                if !result.is_null() {
                    *result = new_pos as i64;
                }
                kResultOk
            }
            Err(_) => kResultFalse,
        }
    }
    
    unsafe fn tell(&self, pos: *mut i64) -> i32 {
        if pos.is_null() {
            return kResultFalse;
        }
        
        match self.buffer.borrow_mut().stream_position() {
            Ok(position) => {
                *pos = position as i64;
                kResultOk
            }
            Err(_) => kResultFalse,
        }
    }
}