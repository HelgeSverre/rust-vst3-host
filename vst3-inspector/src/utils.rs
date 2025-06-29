use vst3::{ComWrapper, Steinberg::Vst::*};

#[cfg(target_os = "windows")]
pub fn win32_string(value: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

// MIDI event printing utility
pub fn print_midi_events(event_list: &ComWrapper<crate::com_implementations::HostEventList>) {
    for event in event_list.events.lock().unwrap().iter() {
        match event.r#type as u32 {
            Event_::EventTypes_::kNoteOnEvent => {
                let note_on = unsafe { event.__field0.noteOn };
                println!(
                    "[MIDI OUT] Note On: channel={}, pitch={}, velocity={}",
                    note_on.channel, note_on.pitch, note_on.velocity
                );
            }
            Event_::EventTypes_::kNoteOffEvent => {
                let note_off = unsafe { event.__field0.noteOff };
                println!(
                    "[MIDI OUT] Note Off: channel={}, pitch={}, velocity={}",
                    note_off.channel, note_off.pitch, note_off.velocity
                );
            }
            _ => {
                println!("[MIDI OUT] Other event type: {}", event.r#type);
            }
        }
    }
}
