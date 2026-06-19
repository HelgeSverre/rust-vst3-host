//! Internal utility functions

use std::os::raw::c_char;
use vst3::Steinberg::Vst::String128;

/// Convert a C-style string to Rust String.
///
/// Takes `c_char` (not `i8`) because `c_char` is unsigned on some platforms (e.g. ARM
/// Linux) and signed on others (macOS, x86) — the VST3 bindings use `c_char`.
// The `c as u8` cast is required on platforms where `c_char` is `i8`; on platforms where
// `c_char` is already `u8` clippy sees it as redundant. Keep it for portability.
#[allow(clippy::unnecessary_cast)]
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
