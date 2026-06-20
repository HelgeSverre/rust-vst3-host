//! Spike: confirm eframe yields a native window handle we can parent a plugin editor into.
//!
//! Opens an eframe window, prints the raw window handle it exposes on the first frame, then
//! closes immediately. De-risks editor embedding (does eframe give us an NSView/HWND/X11
//! parent?). Run: `cargo run -p vst3-host --example eframe_handle_spike`.

use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

fn describe(handle: &RawWindowHandle) -> String {
    match handle {
        RawWindowHandle::AppKit(h) => format!("AppKit NSView ptr = {:?}", h.ns_view),
        RawWindowHandle::Win32(h) => format!("Win32 HWND = {:?}", h.hwnd),
        RawWindowHandle::Xlib(h) => format!("Xlib window = {:#x}", h.window),
        RawWindowHandle::Xcb(h) => format!("Xcb window = {:?}", h.window),
        other => format!("other handle: {other:?}"),
    }
}

struct SpikeApp {
    done: bool,
}

impl eframe::App for SpikeApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| ui.label("handle spike"));
        if !self.done {
            self.done = true;
            match frame.window_handle() {
                Ok(h) => println!("SPIKE_OK: {}", describe(&h.as_raw())),
                Err(e) => println!("SPIKE_ERR: {e:?}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([320.0, 160.0]),
        ..Default::default()
    };
    eframe::run_native(
        "handle spike",
        options,
        Box::new(|_cc| Ok(Box::new(SpikeApp { done: false }))),
    )
}
