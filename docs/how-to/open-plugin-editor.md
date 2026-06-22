# Open or embed a plugin editor

Show a plugin's native GUI — either in its own OS window, or parented inside a window your
app already owns. Only plugins that ship an editor have one; check
[`Plugin::has_editor`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.has_editor)
first.

## Standalone window

[`PluginWindow`](https://docs.rs/vst3-host/latest/vst3_host/window/struct.PluginWindow.html)
creates a native top-level window, sizes it to the editor, and parents the plugin's view
into it. It takes an `Arc<Mutex<Plugin>>` because the window manages the editor's lifecycle
alongside whatever else holds the plugin.

```rust
use std::sync::{Arc, Mutex};
use vst3_host::{simple, PluginWindow};

# fn main() -> vst3_host::Result<()> {
let plugin = simple::load_plugin("/path/to/synth.vst3")?;
let plugin = Arc::new(Mutex::new(plugin));

let mut window = PluginWindow::new(plugin.clone());
window.open()?;        // creates the OS window and opens the editor in it
// ... your run loop drives the editor ...
window.close();        // also happens on drop
# Ok(())
# }
```

- **Call `open` on the main thread.** On macOS the window is an `NSWindow`, which AppKit
  requires you to create on the main thread; `open` returns an error otherwise.
- `open` returns an error if the plugin has no editor (`"Plugin does not have a GUI
  editor"`).
- Dropping the `PluginWindow` closes the editor and destroys the OS window.

If you build editors and audio together, note that the window holds the same
`Arc<Mutex<Plugin>>` you use elsewhere — lock it to send MIDI or set parameters while the
GUI is up.

## Embed in a host window (egui)

To put the editor *inside* a window your UI framework owns (one window, not a separate
editor window), use
[`EmbeddedEditor`](https://docs.rs/vst3-host/latest/vst3_host/embed/struct.EmbeddedEditor.html).
It parents the plugin's view as a child of your window and tracks a rectangle you give it
each frame. Requires the `egui-widgets` feature.

```toml
vst3-host = { version = "...", features = ["egui-widgets"] }
```

[`EmbeddedEditor::embed`](https://docs.rs/vst3-host/latest/vst3_host/embed/struct.EmbeddedEditor.html#method.embed)
takes the plugin, the parent window's `RawWindowHandle`, and an
[`EditorRect`](https://docs.rs/vst3-host/latest/vst3_host/embed/struct.EditorRect.html)
(logical points, top-left origin — the egui convention):

```rust,ignore
use raw_window_handle::HasWindowHandle;
use vst3_host::{EditorRect, EmbeddedEditor};

// In your eframe `update`, once the window exists:
let (w, h) = plugin.lock().unwrap().get_editor_size().unwrap_or((400, 300));
let rect = EditorRect { x: 0.0, y: 0.0, width: w as f32, height: h as f32 };

let handle = frame.window_handle()?.as_raw();   // eframe::Frame
let editor = EmbeddedEditor::embed(plugin.clone(), handle, rect)?;
```

Each frame, reserve the editor's area in your layout and feed the resulting rectangle to
[`set_rect`](https://docs.rs/vst3-host/latest/vst3_host/embed/struct.EmbeddedEditor.html#method.set_rect)
so the native view follows scroll and window resize:

```rust,ignore
let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
editor.set_rect(EditorRect {
    x: rect.min.x,
    y: rect.min.y,
    width: rect.width(),
    height: rect.height(),
});
```

- **Call `embed` and `set_rect` on the UI/main thread** (where your event loop runs).
- Dropping the `EmbeddedEditor` (or letting it go out of scope) detaches the editor and
  removes the child view.

A full working example is in `vst3-host/examples/embedded_editor.rs`:

```bash
cargo run -p vst3-host --example embedded_editor --features egui-widgets
```

## Follow editor resize requests

Resizable editors ask the host to resize their container via VST3's `IPlugFrame`. Poll
[`take_editor_resize_request`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.take_editor_resize_request)
on your UI thread (each frame) while the editor is open; it returns the requested
`(width, height)` in pixels, or `None`:

```rust,ignore
if let Some((w, h)) = plugin.lock().unwrap().take_editor_resize_request() {
    // grow your container / EditorRect to match
    editor.set_rect(EditorRect { x: 0.0, y: 0.0, width: w as f32, height: h as f32 });
}
```

Only the in-process editor path reports resize requests.

## Caveats

- **Editors are in-process.** Both `PluginWindow` and `EmbeddedEditor` parent the plugin's
  view into a window in *your* process, so they work for in-process plugins only. The one
  exception is the macOS helper: under [process isolation](../explanation/process-isolation.md),
  `open_editor` works because the helper process owns its own editor window and runs its own
  UI run loop — but that window is the helper's, not parented into yours, so you cannot embed
  it. On Windows and Linux, `open_editor` across the isolation boundary is not implemented
  and returns an error.
- **Embedding is verified on macOS only.** `EmbeddedEditor` has Windows (child `HWND`) and
  Linux/X11 (child window) implementations, but they are not yet verified; treat them as
  experimental. On unsupported platforms `embed` returns an error.
- **`get_editor_size` is a hint.** Some plugins report a size before the editor is open;
  fall back to a default (e.g. `400x300`) and let `take_editor_resize_request` correct it.
