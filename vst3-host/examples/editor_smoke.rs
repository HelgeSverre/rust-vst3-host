//! Smoke check for the native plugin-editor path: load a plugin, open its editor in a real OS
//! window, service it for a few frames, then close it. On Linux/X11 it also sends the same
//! `WM_DELETE_WINDOW` message a title-bar close button uses. Prints what it did and exits `0`
//! on success, `1` on failure — so it can be wired into CI or a `just` recipe as a pass/fail
//! gate.
//!
//! ```bash
//! cargo run -p vst3-host --example editor_smoke                       # test_plugins/Dexed.vst3
//! cargo run -p vst3-host --example editor_smoke -- /path/to/Some.vst3
//! RUST_LOG=vst3_host=debug cargo run -p vst3-host --example editor_smoke  # incl. resize traffic
//! ```
//!
//! Against the in-repo `TestSynth.vst3` it does more than survive: that plugin's view publishes
//! what it saw during the handshake as read-only parameters, so this run *asserts* the host
//! attached the view, offered it a content scale, and answered its `IPlugFrame::resizeView`
//! request with an `onSize` of exactly that size — the half of the editor protocol no
//! binary-only plugin lets a test observe. Against any other plugin those checks are skipped and
//! the run is the survival smoke test it has always been.
//!
//! This exists next to `tests/editor_open_tests.rs` because the same check as a `#[test]` needs
//! the harness to hand it the process main thread — AppKit refuses to create an `NSWindow`
//! anywhere else, and libtest always runs test functions on a spawned thread, `--test-threads=1`
//! included. An example binary owns `main`, so the constraint is met by construction; the test
//! runs this binary.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use vst3_host::prelude::*;

/// Frames of host servicing to run while the editor is up, at roughly 60 Hz.
const FRAMES: u32 = 30;

// TestSynth's editor instrumentation. Ids and encodings are defined (and documented) in
// `test-plugin/src/lib.rs` under "Editor and state instrumentation"; every scale is a power of
// two, so decoding is exact.
const EDITOR_ATTACHED_PARAM_ID: u32 = 1000;
const EDITOR_WIDTH_PARAM_ID: u32 = 1001;
const EDITOR_HEIGHT_PARAM_ID: u32 = 1002;
const EDITOR_SCALE_PARAM_ID: u32 = 1003;
/// Each probe's id together with the exact title the plugin publishes it under.
const EDITOR_PROBES: [(u32, &str); 4] = [
    (EDITOR_ATTACHED_PARAM_ID, "Editor Attached"),
    (EDITOR_WIDTH_PARAM_ID, "Editor Width"),
    (EDITOR_HEIGHT_PARAM_ID, "Editor Height"),
    (EDITOR_SCALE_PARAM_ID, "Editor Scale"),
];
const EDITOR_SIZE_SCALE: f64 = 4096.0;
const EDITOR_SCALE_SCALE: f64 = 8.0;
/// The size TestSynth's view asks the host for, once, right after it is attached.
const EDITOR_SELF_RESIZE: (i32, i32) = (560, 400);

/// What an instrumented plugin's view recorded during the handshake.
struct EditorInstrumentation {
    attached: bool,
    last_on_size: (i32, i32),
    /// The content scale factor the host offered, or `0.0` if it never did.
    scale: f32,
}

fn main() {
    env_logger::init();
    std::process::exit(match run() {
        Ok(()) => {
            println!("EDITOR SMOKE OK");
            0
        }
        Err(message) => {
            eprintln!("EDITOR SMOKE FAILED: {message}");
            1
        }
    });
}

fn run() -> Result<(), String> {
    let path = std::env::args().nth(1).unwrap_or_else(default_plugin_path);
    println!("=== editor smoke: {path} ===");
    if !std::path::Path::new(&path).exists() {
        return Err(format!("plugin not found at {path}"));
    }

    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .map_err(|e| format!("build host: {e}"))?;
    let plugin = host
        .load_plugin(&path)
        .map_err(|e| format!("load {path}: {e}"))?;
    println!("load: {}", plugin.info().name);
    #[cfg(target_os = "linux")]
    let window_title = format!("{} - VST3", plugin.info().name);

    if !plugin.has_editor() {
        return Err("plugin reports no editor, nothing to smoke-test".to_string());
    }
    let size = plugin.get_editor_size();
    println!(
        "editor: reported size {size:?}, resizable={}",
        plugin.editor_can_resize()
    );

    let plugin = Arc::new(Mutex::new(plugin));
    let mut window = PluginWindow::new(plugin.clone());
    window.open().map_err(|e| format!("open editor: {e}"))?;
    if !window.is_open() {
        return Err("editor reported closed right after open()".to_string());
    }
    println!("open: editor window is up");

    for _ in 0..FRAMES {
        window
            .service_platform_events()
            .map_err(|e| format!("service editor window: {e}"))?;
        plugin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .service_run_loop();
        std::thread::sleep(Duration::from_millis(16));
    }
    if !window.is_open() {
        return Err("editor window went away while it was being serviced".to_string());
    }
    println!("service: survived {FRAMES} frames");

    check_instrumentation(&plugin.lock().unwrap_or_else(|p| p.into_inner()))?;

    #[cfg(target_os = "linux")]
    verify_x11_title_bar_close(&mut window, &plugin, &window_title)?;

    window.close();
    if window.is_open() {
        return Err("window still open after close()".to_string());
    }
    println!("close: editor detached and window destroyed");
    Ok(())
}

/// Drive the same ICCCM request an X11 window manager sends for a title-bar close. This proves
/// the host advertises `WM_DELETE_WINDOW`, observes the event while servicing the editor, and
/// keeps the parent X11 window alive until the caller performs the normal plugin-first teardown.
#[cfg(target_os = "linux")]
fn verify_x11_title_bar_close(
    plugin_window: &mut PluginWindow,
    plugin: &Arc<Mutex<Plugin>>,
    title: &str,
) -> Result<(), String> {
    use xcb::Xid;

    let (connection, screen_number) = xcb::Connection::connect(None)
        .map_err(|error| format!("connect X11 close-test client: {error}"))?;
    let wm_protocols = intern_x11_atom(&connection, b"WM_PROTOCOLS")?;
    let wm_delete_window = intern_x11_atom(&connection, b"WM_DELETE_WINDOW")?;
    let screen = connection
        .get_setup()
        .roots()
        .nth(screen_number as usize)
        .ok_or_else(|| "find X11 screen for close test".to_string())?;
    let window = find_x11_window_by_title(&connection, screen.root(), title)?;

    let protocols = connection
        .wait_for_reply(connection.send_request(&xcb::x::GetProperty {
            delete: false,
            window,
            property: wm_protocols,
            r#type: xcb::x::ATOM_ATOM,
            long_offset: 0,
            long_length: 16,
        }))
        .map_err(|error| format!("read WM_PROTOCOLS: {error}"))?;
    if protocols.r#type() != xcb::x::ATOM_ATOM
        || protocols.format() != 32
        || !protocols
            .value::<u32>()
            .contains(&wm_delete_window.resource_id())
    {
        return Err("editor window does not advertise WM_DELETE_WINDOW".to_string());
    }

    let close = xcb::x::ClientMessageEvent::new(
        window,
        wm_protocols,
        xcb::x::ClientMessageData::Data32([wm_delete_window.resource_id(), 0, 0, 0, 0]),
    );
    connection
        .send_and_check_request(&xcb::x::SendEvent {
            propagate: false,
            destination: xcb::x::SendEventDest::Window(window),
            event_mask: xcb::x::EventMask::empty(),
            event: &close,
        })
        .map_err(|error| format!("send WM_DELETE_WINDOW: {error}"))?;

    // `closed_by_user` remains safe while a caller has the plugin lock: it polls only XCB and
    // records the close request instead of entering the plugin from the window event path.
    let plugin_guard = plugin.lock().unwrap_or_else(|poison| poison.into_inner());
    if !plugin_window.closed_by_user() {
        return Err("editor did not report WM_DELETE_WINDOW".to_string());
    }
    drop(plugin_guard);
    if plugin_window.is_open() {
        return Err("editor stayed open after WM_DELETE_WINDOW".to_string());
    }
    // The host has recorded the request but has not yet detached the plugin or destroyed its X11
    // parent. A query through a separate X11 connection verifies that invariant.
    connection
        .wait_for_reply(connection.send_request(&xcb::x::GetWindowAttributes { window }))
        .map_err(|error| format!("editor parent disappeared before close(): {error}"))?;
    println!("close: X11 WM_DELETE_WINDOW detected before teardown");
    Ok(())
}

#[cfg(target_os = "linux")]
fn intern_x11_atom(connection: &xcb::Connection, name: &[u8]) -> Result<xcb::x::Atom, String> {
    let request = connection.send_request(&xcb::x::InternAtom {
        only_if_exists: false,
        name,
    });
    connection
        .wait_for_reply(request)
        .map(|reply| reply.atom())
        .map_err(|error| format!("intern X11 atom {}: {error}", String::from_utf8_lossy(name)))
}

/// Find the host-created X11 window by its title. Window managers may re-parent clients under
/// decoration windows, so walk the full tree rather than assuming a direct root child.
#[cfg(target_os = "linux")]
fn find_x11_window_by_title(
    connection: &xcb::Connection,
    root: xcb::x::Window,
    title: &str,
) -> Result<xcb::x::Window, String> {
    let mut parents = vec![root];
    let mut matches = Vec::new();
    while let Some(parent) = parents.pop() {
        let tree = connection
            .wait_for_reply(connection.send_request(&xcb::x::QueryTree { window: parent }))
            .map_err(|error| format!("list X11 child windows: {error}"))?;
        for window in tree.children() {
            let property =
                connection.wait_for_reply(connection.send_request(&xcb::x::GetProperty {
                    delete: false,
                    window: *window,
                    property: xcb::x::ATOM_WM_NAME,
                    r#type: xcb::x::ATOM_STRING,
                    long_offset: 0,
                    long_length: title.len() as u32,
                }));
            if property.is_ok_and(|reply| {
                reply.r#type() == xcb::x::ATOM_STRING
                    && reply.format() == 8
                    && reply.value::<u8>() == title.as_bytes()
            }) {
                matches.push(*window);
            }
            parents.push(*window);
        }
    }
    match matches.as_slice() {
        [window] => Ok(*window),
        [] => Err(format!("find X11 editor window titled {title:?}")),
        _ => Err(format!(
            "found {} X11 editor windows titled {title:?}; close-test target is ambiguous",
            matches.len()
        )),
    }
}

/// Verify the editor handshake against a plugin that reports what it saw.
///
/// A plugin without the instrumentation parameters (anything but our own TestSynth) is reported
/// and skipped — there is nothing to read, and the survival checks above already ran.
fn check_instrumentation(plugin: &Plugin) -> Result<(), String> {
    let Some(seen) = read_instrumentation(plugin) else {
        println!("instrumentation: none — plugin does not publish editor probes, skipping");
        return Ok(());
    };
    println!(
        "instrumentation: attached={} last onSize={}x{} content scale={:.2}",
        seen.attached, seen.last_on_size.0, seen.last_on_size.1, seen.scale
    );

    if !seen.attached {
        return Err("the plugin's view never saw IPlugView::attached".to_string());
    }
    if seen.last_on_size != EDITOR_SELF_RESIZE {
        return Err(format!(
            "the view asked the host to resize it to {}x{} and the host answered onSize {}x{} \
             — the resizeView -> container resize -> onSize chain is broken",
            EDITOR_SELF_RESIZE.0, EDITOR_SELF_RESIZE.1, seen.last_on_size.0, seen.last_on_size.1
        ));
    }
    // The window consumes the request while servicing frames. A leftover one means the host
    // recorded the resize but never applied it to the container.
    if let Some(pending) = plugin.take_editor_resize_request() {
        return Err(format!(
            "the host still has an unapplied resize request {pending:?} after servicing frames"
        ));
    }
    // macOS is the one platform where the host always offers a scale factor before attaching
    // (Windows offers it from the window's DPI, X11 has no host-side source for one).
    if cfg!(target_os = "macos") && seen.scale <= 0.0 {
        return Err("the host never offered the view a content scale factor".to_string());
    }
    println!("instrumentation: editor handshake verified end to end");
    Ok(())
}

/// Read the editor probes, or `None` when this plugin does not publish them.
fn read_instrumentation(plugin: &Plugin) -> Option<EditorInstrumentation> {
    let parameters = plugin.get_parameters().ok()?;
    // Match on id *and* title *and* the read-only flag. Plenty of plugins own id 1000 already —
    // Dexed's MIDI CC map does — and mistaking one of those for a probe would fail the run for
    // no reason at all.
    let published = EDITOR_PROBES.iter().all(|(id, title)| {
        parameters
            .iter()
            .any(|p| p.id == *id && p.name == *title && p.is_read_only)
    });
    if !published {
        return None;
    }
    let read = |id| plugin.get_parameter(id).unwrap_or(0.0);
    Some(EditorInstrumentation {
        attached: read(EDITOR_ATTACHED_PARAM_ID) >= 0.5,
        last_on_size: (
            (read(EDITOR_WIDTH_PARAM_ID) * EDITOR_SIZE_SCALE).round() as i32,
            (read(EDITOR_HEIGHT_PARAM_ID) * EDITOR_SIZE_SCALE).round() as i32,
        ),
        scale: (read(EDITOR_SCALE_PARAM_ID) * EDITOR_SCALE_SCALE) as f32,
    })
}

fn default_plugin_path() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3").to_string()
}
