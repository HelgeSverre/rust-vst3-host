//! Show a plugin's editor across the process-isolation boundary.
//!
//! The plugin runs in the `vst3-host-helper` child process, and on macOS that helper now
//! owns a native window and runs its own UI event loop — so the editor appears (and is
//! interactive) even though the plugin lives in a different process from this one. Audio
//! keeps flowing concurrently.
//!
//! Usage:
//!   cargo run -p vst3-host --example isolated_gui --features cpal-backend,process-isolation
//!     [-- <plugin.vst3>]
//!
//! macOS only for now (the helper's window/run-loop is macOS). On other platforms the editor
//! request reports "not supported" and only audio runs.

use std::io::Read;
use vst3_host::{midi::MidiChannel, Vst3Host, WindowHandle};

fn main() -> vst3_host::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "test_plugins/Dexed.vst3".to_string());

    let mut host = Vst3Host::builder()
        .with_process_isolation(true)
        .sample_rate(48_000.0)
        .block_size(512)
        .build()?;

    let mut plugin = host.load_plugin(&path)?;
    let info = plugin.info().clone();
    println!(
        "Loaded {} (isolated, helper pid {:?})",
        info.name,
        plugin.isolation_pid()
    );

    if !plugin.has_editor() {
        println!("This plugin reports no editor; nothing to show.");
        return Ok(());
    }

    // The editor window is owned by the helper process, so there is no parent handle to
    // pass — the isolated path ignores it.
    let parent = unsafe { WindowHandle::from_raw(std::ptr::null_mut()) };
    match plugin.open_editor(parent) {
        Ok(()) => {
            let (w, h) = plugin.get_editor_size().unwrap_or((0, 0));
            println!("Editor opened in the helper process ({w}x{h}).");
        }
        Err(e) => {
            println!("Could not open the isolated editor: {e}");
            println!("(GUI across isolation is macOS-only for now.) Continuing with audio.");
        }
    }

    // Prove audio still flows from this process while the editor lives in the helper.
    let audio = host.play(plugin)?;
    audio.lock().send_midi_note(60, 100, MidiChannel::Ch1)?;

    println!("Audio playing and editor live. Press Enter to quit.");
    let _ = std::io::stdin().read(&mut [0u8]);
    Ok(())
}
