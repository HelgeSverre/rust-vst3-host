//! VST3 Host Helper Process
//!
//! Runs a single VST3 plugin in isolation from the main process. It is intentionally
//! a thin wrapper around the library's own (in-process) public API: every command
//! delegates to a real [`vst3_host::Plugin`], so the isolated path reuses exactly the
//! same, verified plugin handling as the non-isolated path — there is no separate
//! VST3 implementation to drift out of sync.
//!
//! The protocol enums are imported from the library (`vst3_host::process_isolation`),
//! so host and helper can never disagree about the wire format.
//!
//! ## Threading (macOS)
//!
//! A plugin editor needs a native UI run loop on the **main thread** to be interactive.
//! So on macOS the main thread runs an `NSApplication` event pump and stdin/command
//! processing moves to a worker thread; the plugin is shared behind an
//! `Arc<Mutex<Option<Plugin>>>`. `CreateGui`/`CloseGui` are forwarded from the worker to
//! the main thread (which owns the `NSWindow`) over a channel. Audio/control commands run
//! exactly as before, just on the worker thread. On other platforms the helper stays a
//! single-threaded stdin loop and GUI is not yet supported.

use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

use vst3_host::{
    audio::AudioBuffers,
    process_isolation::{HostCommand, HostResponse},
    Plugin, Vst3Host,
};

/// Loaded plugin shared between the command worker and (on macOS) the UI main thread.
type SharedPlugin = Arc<Mutex<Option<Plugin>>>;

fn main() {
    eprintln!("VST3 Host Helper Process Started");

    let plugin: SharedPlugin = Arc::new(Mutex::new(None));

    #[cfg(target_os = "macos")]
    {
        macos::run(plugin);
    }

    #[cfg(not(target_os = "macos"))]
    {
        // No UI run loop needed: process commands on this (main) thread directly.
        let stdin = io::stdin();
        let mut stdout = io::stdout();
        let mut sample_rate = 44100.0;
        for line in stdin.lock().lines() {
            let Some(command) = parse_line(line, &mut stdout) else {
                continue;
            };
            if matches!(command, HostCommand::Shutdown) {
                eprintln!("Shutting down helper process");
                break;
            }
            let response = handle(command, &plugin, &mut sample_rate, None);
            respond(&mut stdout, &response);
        }
    }
}

/// Parse one stdin line into a command, reporting (and skipping) blank/invalid lines.
fn parse_line(line: io::Result<String>, stdout: &mut io::Stdout) -> Option<HostCommand> {
    let line = match line {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to read line: {}", e);
            return None;
        }
    };
    if line.trim().is_empty() {
        return None;
    }
    match serde_json::from_str(&line) {
        Ok(cmd) => Some(cmd),
        Err(e) => {
            respond(
                stdout,
                &HostResponse::Error {
                    message: format!("Invalid command: {}", e),
                },
            );
            None
        }
    }
}

fn respond(stdout: &mut io::Stdout, response: &HostResponse) {
    if let Ok(json) = serde_json::to_string(response) {
        let _ = writeln!(stdout, "{}", json);
        let _ = stdout.flush();
    }
}

fn err<E: std::fmt::Display>(prefix: &str, e: E) -> HostResponse {
    HostResponse::Error {
        message: format!("{prefix}: {e}"),
    }
}

/// A GUI request the worker forwards to the main thread (which owns the window).
#[cfg(target_os = "macos")]
struct GuiRequest {
    open: bool,
    reply: std::sync::mpsc::Sender<HostResponse>,
}

/// Handle a command against the shared plugin. GUI commands are delegated to `gui` (the
/// main-thread channel) when present; without it they report "not supported".
fn handle(
    command: HostCommand,
    plugin: &SharedPlugin,
    sample_rate: &mut f64,
    #[allow(unused_variables)] gui: Option<&GuiChannel>,
) -> HostResponse {
    // Convenience: run a closure against the loaded plugin or report "no plugin".
    fn with<F: FnOnce(&mut Plugin) -> HostResponse>(p: &SharedPlugin, f: F) -> HostResponse {
        let mut guard = match p.lock() {
            Ok(g) => g,
            Err(_) => {
                return HostResponse::Error {
                    message: "plugin lock poisoned".to_string(),
                }
            }
        };
        match guard.as_mut() {
            Some(pl) => f(pl),
            None => HostResponse::Error {
                message: "No plugin loaded".to_string(),
            },
        }
    }

    match command {
        HostCommand::LoadPlugin {
            path,
            sample_rate: sr,
            block_size,
            tempo,
            time_sig_numerator,
            time_sig_denominator,
        } => {
            *sample_rate = sr;
            let mut host = match Vst3Host::builder()
                .sample_rate(sr)
                .block_size(block_size as usize)
                .tempo(tempo)
                .time_signature(time_sig_numerator, time_sig_denominator)
                .build()
            {
                Ok(h) => h,
                Err(e) => return err("Failed to build host", e),
            };
            match host.load_plugin(&path) {
                Ok(p) => {
                    let info = p.info().clone();
                    let output_channels = p.output_channel_count() as i32;
                    *plugin.lock().unwrap() = Some(p);
                    HostResponse::PluginInfo {
                        vendor: info.vendor,
                        name: info.name,
                        version: info.version,
                        category: info.category,
                        uid: info.uid,
                        has_gui: info.has_gui,
                        audio_inputs: info.audio_inputs as i32,
                        audio_outputs: info.audio_outputs as i32,
                        output_channels,
                        has_midi_input: info.has_midi_input,
                        has_midi_output: info.has_midi_output,
                    }
                }
                Err(e) => err("Failed to load plugin", e),
            }
        }
        HostCommand::UnloadPlugin => {
            *plugin.lock().unwrap() = None;
            HostResponse::Success {
                message: "Plugin unloaded".to_string(),
            }
        }
        HostCommand::StartProcessing => with(plugin, |p| match p.start_processing() {
            Ok(()) => HostResponse::Success {
                message: "processing started".to_string(),
            },
            Err(e) => err("StartProcessing", e),
        }),
        HostCommand::StopProcessing => with(plugin, |p| match p.stop_processing() {
            Ok(()) => HostResponse::Success {
                message: "processing stopped".to_string(),
            },
            Err(e) => err("StopProcessing", e),
        }),
        HostCommand::SetParameter { id, value } => {
            with(plugin, |p| match p.set_parameter(id, value) {
                Ok(()) => HostResponse::Success {
                    message: "parameter set".to_string(),
                },
                Err(e) => err("SetParameter", e),
            })
        }
        HostCommand::SetParameterAt { id, value, offset } => {
            with(plugin, |p| match p.set_parameter_at(id, value, offset) {
                Ok(()) => HostResponse::Success {
                    message: "parameter scheduled".to_string(),
                },
                Err(e) => err("SetParameterAt", e),
            })
        }
        HostCommand::GetParameter { id } => with(plugin, |p| match p.get_parameter(id) {
            Ok(value) => HostResponse::ParameterValue { value },
            Err(e) => err("GetParameter", e),
        }),
        HostCommand::GetAllParameters => with(plugin, |p| match p.get_parameters() {
            Ok(params) => HostResponse::Parameters { params },
            Err(e) => err("GetAllParameters", e),
        }),
        HostCommand::FormatParameter { id, normalized } => {
            with(plugin, |p| match p.format_parameter(id, normalized) {
                Ok(value) => HostResponse::ParameterString { value },
                Err(e) => err("FormatParameter", e),
            })
        }
        HostCommand::SendMidi { event } => with(plugin, |p| match p.send_midi_event(event) {
            Ok(()) => HostResponse::Success {
                message: "midi sent".to_string(),
            },
            Err(e) => err("SendMidi", e),
        }),
        HostCommand::Process { inputs, frames } => {
            let sr = *sample_rate;
            with(plugin, |p| {
                let out_channels = p.info().audio_outputs.max(1) as usize * 2;
                let mut buffers = AudioBuffers {
                    inputs,
                    outputs: vec![vec![0.0; frames as usize]; out_channels],
                    sample_rate: sr,
                    block_size: frames as usize,
                };
                match p.process_audio(&mut buffers) {
                    Ok(()) => HostResponse::AudioOutput {
                        outputs: buffers.outputs,
                        output_midi: p.take_output_midi(),
                    },
                    Err(e) => err("Process", e),
                }
            })
        }
        HostCommand::SaveState => with(plugin, |p| match p.save_state() {
            Ok(data) => HostResponse::State { data },
            Err(e) => err("SaveState", e),
        }),
        HostCommand::LoadState { data } => with(plugin, |p| match p.load_state(&data) {
            Ok(()) => HostResponse::Success {
                message: "state restored".to_string(),
            },
            Err(e) => err("LoadState", e),
        }),
        HostCommand::NoteOn {
            channel,
            note,
            velocity,
            sample_offset,
        } => with(plugin, |p| {
            let Some(ch) = vst3_host::MidiChannel::from_index(channel) else {
                return HostResponse::Error {
                    message: format!("NoteOn: invalid channel index {channel}"),
                };
            };
            // The in-process plugin allocates the per-voice NoteId; return its raw id.
            match p.note_on_at(ch, note, velocity, sample_offset) {
                Ok(id) => HostResponse::NoteStarted { note_id: id.raw() },
                Err(e) => err("NoteOn", e),
            }
        }),
        HostCommand::NoteOff {
            note_id,
            sample_offset,
        } => with(plugin, |p| {
            match p.note_off_at(vst3_host::NoteId::from_raw(note_id), sample_offset) {
                Ok(()) => HostResponse::Success {
                    message: "note off".to_string(),
                },
                Err(e) => err("NoteOff", e),
            }
        }),
        HostCommand::SendNoteExpression {
            note_id,
            kind,
            value,
            sample_offset,
        } => with(plugin, |p| {
            match p.send_note_expression_at(
                vst3_host::NoteId::from_raw(note_id),
                kind,
                value,
                sample_offset,
            ) {
                Ok(()) => HostResponse::Success {
                    message: "note expression sent".to_string(),
                },
                Err(e) => err("SendNoteExpression", e),
            }
        }),
        // Note: the public API enumerates bus 0 / channel 0 (the conventional MPE bus); the
        // bus/channel carried by the command is currently always (0, 0) from the client.
        HostCommand::NoteExpressions { bus: _, channel: _ } => {
            with(plugin, |p| match p.note_expressions() {
                Ok(expressions) => HostResponse::NoteExpressions { expressions },
                Err(e) => err("NoteExpressions", e),
            })
        }
        HostCommand::CreateGui => gui_request(gui, true),
        HostCommand::CloseGui => gui_request(gui, false),
        HostCommand::Shutdown => HostResponse::Success {
            message: "shutting down".to_string(),
        },
    }
}

/// The worker's handle to the main thread's GUI loop (macOS only).
#[cfg(target_os = "macos")]
struct GuiChannel(std::sync::mpsc::Sender<GuiRequest>);
#[cfg(not(target_os = "macos"))]
struct GuiChannel;

/// Forward a GUI open/close to the main thread and wait for its reply.
fn gui_request(gui: Option<&GuiChannel>, open: bool) -> HostResponse {
    #[cfg(target_os = "macos")]
    {
        let Some(GuiChannel(tx)) = gui else {
            return HostResponse::Error {
                message: "GUI loop unavailable".to_string(),
            };
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if tx
            .send(GuiRequest {
                open,
                reply: reply_tx,
            })
            .is_err()
        {
            return HostResponse::Error {
                message: "GUI loop is gone".to_string(),
            };
        }
        reply_rx.recv().unwrap_or(HostResponse::Error {
            message: "GUI loop did not reply".to_string(),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (gui, open);
        HostResponse::Error {
            message: "Plugin GUI is not supported across process isolation on this platform"
                .to_string(),
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSView,
        NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};
    use std::sync::mpsc;

    /// Entry point on macOS: spawn the stdin/command worker, then run the UI event pump on
    /// this (main) thread.
    pub fn run(plugin: SharedPlugin) {
        let (gui_tx, gui_rx) = mpsc::channel::<GuiRequest>();
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        // Worker: read stdin and process commands; GUI verbs are delegated to the main loop.
        {
            let plugin = plugin.clone();
            std::thread::spawn(move || {
                let stdin = io::stdin();
                let mut stdout = io::stdout();
                let mut sample_rate = 44100.0;
                let gui = GuiChannel(gui_tx);
                for line in stdin.lock().lines() {
                    let Some(command) = parse_line(line, &mut stdout) else {
                        continue;
                    };
                    if matches!(command, HostCommand::Shutdown) {
                        eprintln!("Shutting down helper process");
                        let _ = shutdown_tx.send(());
                        break;
                    }
                    let response = handle(command, &plugin, &mut sample_rate, Some(&gui));
                    respond(&mut stdout, &response);
                }
                // stdin closed → ask the main loop to exit too.
                let _ = shutdown_tx.send(());
            });
        }

        run_event_loop(&plugin, &gui_rx, &shutdown_rx);
    }

    /// The main-thread native event pump. Interleaves AppKit event dispatch with polling
    /// the GUI-request and shutdown channels.
    fn run_event_loop(
        plugin: &SharedPlugin,
        gui_rx: &mpsc::Receiver<GuiRequest>,
        shutdown_rx: &mpsc::Receiver<()>,
    ) {
        let mtm = MainThreadMarker::new().expect("helper UI loop must run on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        // Accessory: no Dock icon / menu bar for the (usually headless) helper.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();

        let mut window: Option<Retained<NSWindow>> = None;

        loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }

            while let Ok(req) = gui_rx.try_recv() {
                let response = if req.open {
                    match open_editor_window(plugin, mtm, &app) {
                        Ok((w, width, height)) => {
                            window = Some(w);
                            HostResponse::GuiCreated { width, height }
                        }
                        Err(e) => HostResponse::Error { message: e },
                    }
                } else {
                    close_editor_window(plugin, window.take());
                    HostResponse::Success {
                        message: "editor closed".to_string(),
                    }
                };
                let _ = req.reply.send(response);
            }

            // Pump native events, waking at least every 20 ms to re-check the channels.
            let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
            while let Some(event) = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&until),
                    NSDefaultRunLoopMode,
                    true,
                )
            } {
                app.sendEvent(&event);
            }
        }

        close_editor_window(plugin, window.take());
    }

    /// Create a top-level window owned by this (helper) process and attach the plugin's
    /// editor into it. Returns the window plus its content size.
    fn open_editor_window(
        plugin: &SharedPlugin,
        mtm: MainThreadMarker,
        app: &NSApplication,
    ) -> std::result::Result<(Retained<NSWindow>, i32, i32), String> {
        let mut guard = plugin
            .lock()
            .map_err(|_| "plugin lock poisoned".to_string())?;
        let p = guard
            .as_mut()
            .ok_or_else(|| "No plugin loaded".to_string())?;
        if !p.has_editor() {
            return Err("Plugin does not have a GUI editor".to_string());
        }

        let (width, height) = p.get_editor_size().unwrap_or((800, 600));
        let title = format!("{} - VST3", p.info().name);

        let frame = NSRect::new(
            NSPoint::new(120.0, 120.0),
            NSSize::new(width as f64, height as f64),
        );
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;
        // SAFETY: standard AppKit window/view construction on the main thread.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // We own the window's lifetime via `Retained`; opt out of release-on-close to avoid
        // a double-free when the editor window is closed.
        // SAFETY: standard AppKit setter on the main thread.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(&title));

        let container = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        );
        if let Some(content) = window.contentView() {
            content.addSubview(&container);
        }

        let handle = vst3_host::WindowHandle::from_nsview(
            Retained::as_ptr(&container) as *mut std::ffi::c_void
        );
        p.open_editor(handle).map_err(|e| e.to_string())?;

        window.setContentSize(frame.size);
        window.center();
        window.makeKeyAndOrderFront(None);
        // Bring the helper forward so the editor is usable.
        app.activate();

        Ok((window, width, height))
    }

    fn close_editor_window(plugin: &SharedPlugin, window: Option<Retained<NSWindow>>) {
        if let Ok(mut guard) = plugin.lock() {
            if let Some(p) = guard.as_mut() {
                let _ = p.close_editor();
            }
        }
        if let Some(w) = window {
            w.close();
        }
    }
}
