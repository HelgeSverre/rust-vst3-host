# Linux plugin-editor window support (XCB)

Notes on the Linux editor-window support, ported from a community fork.

## Source

[`khremeviuc1004/rust-vst3-host`](https://github.com/khremeviuc1004/rust-vst3-host) — three
commits on top of the (pre-extraction) `main`:

1. "Added very primitive Linux support for plugin windows."
2. "Fixed plugin windows not being sized properly."
3. "Fixed Linux plugin window hiding; made the AudioPluginWindow trait available for other OSes."

The other two forks (`XiangCoder`, `Jurek-Raben`) had no commits ahead of base.

## What the fork did

Added an X11 path for hosting a plugin's editor, using the `xcb` crate (1.7):

- Connect to the X server (`xcb::Connection::connect`), create an `InputOutput` window
  sized to the plugin's editor, set its title, and map it.
- Pass the X11 **window id** to the plugin via VST3 `IPlugView::attached` with the
  `"X11EmbedWindowID"` platform type.
- Unmap/destroy the window on close.

## How it maps to this codebase

The fork targeted the old monolith (`src/main.rs`). In the extracted library the pieces are:

- **`vst3-host/src/internal/plugin_impl.rs::open_editor`** — already handled the
  `X11EmbedWindowID` platform type and the `attached()` call (it was platform-cfg'd from
  the start). No change needed there.
- **`vst3-host/src/window.rs`** — the Linux branch of `PluginWindow::open()` previously
  returned an error. It now creates the XCB window (ported from the fork), maps it, and
  passes `WindowHandle::from_x11(window_id)` to `open_editor`. `close()` unmaps/destroys it.
- **`vst3-host/src/plugin.rs`** — added `WindowHandle::from_x11(u32)` (the VST3 X11 platform
  type takes the window id itself as the handle value).
- **`vst3-host/Cargo.toml`** — added `xcb = "1.7"` as a Linux-only dependency.

## Status

Cfg-gated to Linux, so it does not affect the macOS/Windows builds (verified). **Not yet
compiled or run in this environment** (macOS, no Linux target / `libxcb`). Needs
verification on a Linux machine with X11 + `libxcb` dev headers. Wayland is not addressed
(X11 only). Window sizing/resize on editor-resize requests (`IPlugFrame`) is still the
broader editor-embedding gap tracked in the roadmap.
