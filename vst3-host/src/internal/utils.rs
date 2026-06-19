//! Internal utility functions

use std::os::raw::c_char;
use vst3::Steinberg::Vst::String128;

/// Convert a C-style string to Rust String.
///
/// Takes `c_char` (not `i8`) because `c_char` is unsigned on some platforms (e.g. ARM
/// Linux) and signed on others (macOS, x86) — the VST3 bindings use `c_char`.
pub fn c_str_to_string(c_str: &[c_char]) -> String {
    let end = c_str.iter().position(|&c| c == 0).unwrap_or(c_str.len());
    let bytes: Vec<u8> = c_str[..end].iter().map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).to_string()
}

/// Convert VST3 String128 (UTF-16) to Rust String
pub fn vst_string_to_string(vst_str: &String128) -> String {
    let mut utf16_vec = Vec::new();

    for &ch in vst_str.iter() {
        if ch == 0 {
            break;
        }
        utf16_vec.push(ch as u16);
    }

    String::from_utf16_lossy(&utf16_vec)
}

/// Convert Rust String to VST3 String128
pub fn string_to_vst_string(s: &str) -> String128 {
    let mut result = [0i16; 128];
    let utf16: Vec<u16> = s.encode_utf16().collect();
    let len = utf16.len().min(127);

    for (i, &ch) in utf16.iter().take(len).enumerate() {
        result[i] = ch as i16;
    }

    result
}
