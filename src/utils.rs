use std::ffi::CStr;
use vst3::{ComWrapper, Steinberg::Vst::*};

// String conversion utilities
pub fn c_str_to_string(c_str: &[i8]) -> String {
    unsafe { CStr::from_ptr(c_str.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

pub fn utf16_to_string_i16(utf16: &[i16]) -> String {
    let len = utf16.iter().position(|&c| c == 0).unwrap_or(utf16.len());
    String::from_utf16_lossy(&utf16[..len].iter().map(|&c| c as u16).collect::<Vec<_>>())
}

#[cfg(target_os = "windows")]
pub fn win32_string(value: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

// Plugin path utilities
pub fn get_plugin_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

// Helper function to find the correct binary path in VST3 bundle
pub fn get_vst3_binary_path(bundle_path: &str) -> Result<String, String> {
    let path = std::path::Path::new(bundle_path);

    // If it's already pointing to the binary, use it
    if path.is_file() {
        return Ok(bundle_path.to_string());
    }

    // Platform-specific VST3 bundle handling
    #[cfg(target_os = "macos")]
    {
        // macOS: .vst3 bundle structure
        if bundle_path.ends_with(".vst3") {
            let contents_path = path.join("Contents").join("MacOS");
            if let Ok(entries) = std::fs::read_dir(&contents_path) {
                for entry in entries {
                    if let Ok(entry) = entry {
                        let file_path = entry.path();
                        if file_path.is_file() {
                            if let Some(name) = file_path.file_name() {
                                if let Some(name_str) = name.to_str() {
                                    // Skip hidden files and common non-binary files
                                    if !name_str.starts_with('.')
                                        && !name_str.ends_with(".plist")
                                        && !name_str.ends_with(".txt")
                                    {
                                        return Ok(file_path.to_string_lossy().to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            return Err(format!("No binary found in VST3 bundle: {}", bundle_path));
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Windows: .vst3 bundle structure
        if bundle_path.ends_with(".vst3") {
            let contents_path = path.join("Contents");
            let arch_path = if cfg!(target_arch = "x86_64") {
                contents_path.join("x86_64-win")
            } else {
                contents_path.join("x86-win")
            };

            // Look for .vst3 file inside the architecture folder
            if let Ok(entries) = std::fs::read_dir(&arch_path) {
                for entry in entries {
                    if let Ok(entry) = entry {
                        let file_path = entry.path();
                        if file_path.is_file() && file_path.extension() == Some(std::ffi::OsStr::new("vst3")) {
                            return Ok(file_path.to_string_lossy().to_string());
                        }
                    }
                }
            }
            return Err(format!("No binary found in VST3 bundle: {}", bundle_path));
        }
    }

    Err(format!("Not a valid VST3 bundle: {}", bundle_path))
}

// MIDI event printing utility
pub fn print_midi_events(event_list: &ComWrapper<crate::com_implementations::MyEventList>) {
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