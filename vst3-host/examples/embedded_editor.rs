//! Embed a VST3 plugin's editor *inside* an eframe window (one window, not a separate
//! editor window). Demonstrates `vst3_host::EmbeddedEditor`.
//!
//! Run: `cargo run -p vst3-host --example embedded_editor --features egui-widgets`
//! (optionally pass a `.vst3` path; defaults to the bundled Dexed).

use eframe::egui;
use raw_window_handle::HasWindowHandle;
use std::sync::{Arc, Mutex};
use vst3_host::prelude::*;
use vst3_host::{EditorRect, EmbeddedEditor};

struct App {
    plugin: Arc<Mutex<Plugin>>,
    editor_size: (i32, i32),
    editor: Option<EmbeddedEditor>,
    error: Option<String>,
    // Headless smoke mode (EMBED_SMOKE=1): auto-embed then quit, to verify the embed path
    // doesn't crash without needing a human to click.
    smoke: bool,
    frame: u64,
}

impl App {
    fn new(path: &str) -> Result<Self, String> {
        let mut host = Vst3Host::new().map_err(|e| e.to_string())?;
        let plugin = host.load_plugin(path).map_err(|e| e.to_string())?;
        if !plugin.has_editor() {
            return Err(format!("{} has no editor GUI", plugin.info().name));
        }
        let editor_size = plugin.get_editor_size().unwrap_or((400, 300));
        Ok(Self {
            plugin: Arc::new(Mutex::new(plugin)),
            editor_size,
            editor: None,
            error: None,
            smoke: std::env::var("EMBED_SMOKE").is_ok(),
            frame: 0,
        })
    }

    fn try_embed(&mut self, frame: &mut eframe::Frame) {
        match frame.window_handle() {
            Ok(h) => {
                let rect = EditorRect {
                    x: 0.0,
                    y: 0.0,
                    width: self.editor_size.0 as f32,
                    height: self.editor_size.1 as f32,
                };
                match EmbeddedEditor::embed(self.plugin.clone(), h.as_raw(), rect) {
                    Ok(e) => self.editor = Some(e),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Err(e) => self.error = Some(format!("no window handle: {e:?}")),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.frame += 1;
        if self.smoke {
            // Auto-embed once the window exists, render a few frames, then quit.
            if self.frame == 3 {
                self.try_embed(frame);
                match (&self.editor, &self.error) {
                    (Some(_), _) => println!("EMBED_SMOKE_OK: editor embedded without crashing"),
                    (None, Some(e)) => println!("EMBED_SMOKE_ERR: {e}"),
                    _ => {}
                }
            }
            if self.frame > 15 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.editor.is_none() {
                    if ui.button("Show embedded editor").clicked() {
                        self.try_embed(frame);
                    }
                } else {
                    if ui.button("Close editor").clicked() {
                        self.editor = None;
                    }
                    ui.label("editor embedded in this window");
                }
                if let Some(err) = &self.error {
                    ui.colored_label(egui::Color32::RED, err);
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // Reserve the editor's area; the native editor view overlays exactly this rect.
            let desired = egui::vec2(self.editor_size.0 as f32, self.editor_size.1 as f32);
            let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
            if let Some(editor) = &self.editor {
                editor.set_rect(EditorRect {
                    x: rect.min.x,
                    y: rect.min.y,
                    width: rect.width(),
                    height: rect.height(),
                });
            } else {
                ui.painter()
                    .rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "click \"Show embedded editor\"",
                    egui::FontId::proportional(14.0),
                    ui.visuals().weak_text_color(),
                );
            }
        });

        // Keep tracking the rect as the window scrolls/resizes.
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3").to_string()
    });

    let app = match App::new(&path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Failed to load plugin: {e}");
            std::process::exit(1);
        }
    };
    let size = app.editor_size;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([size.0 as f32 + 24.0, size.1 as f32 + 64.0])
            .with_title("Embedded VST3 editor"),
        ..Default::default()
    };
    eframe::run_native(
        "Embedded VST3 editor",
        options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
}
