//! Embed a plugin editor inside a host window (e.g. an egui/eframe window), as an
//! alternative to the standalone [`PluginWindow`](crate::PluginWindow).
//!
//! Instead of opening its own top-level OS window, the plugin's native editor view is
//! parented as a child of a window your UI framework already owns, positioned to track a
//! region you allocate. You provide the parent window's [`RawWindowHandle`] and a target
//! rectangle (in logical points, top-left origin — the egui convention) each frame.
//!
//! Currently implemented on **macOS only**; other platforms return an error from
//! [`EmbeddedEditor::embed`]. Requires the `egui-widgets` feature.
#![cfg(feature = "egui-widgets")]

use crate::error::{Error, Result};
use crate::plugin::Plugin;
use raw_window_handle::RawWindowHandle;
use std::sync::{Arc, Mutex};

/// A rectangle in the host view's logical points, **top-left origin** (egui convention).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorRect {
    /// Left edge, points from the window's left.
    pub x: f32,
    /// Top edge, points from the window's top.
    pub y: f32,
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
}

/// A plugin editor embedded into a host window. Drop it (or call [`Self::close`]) to detach
/// the editor and remove the child view.
pub struct EmbeddedEditor {
    plugin: Arc<Mutex<Plugin>>,
    #[cfg(target_os = "macos")]
    inner: macos::MacEmbed,
}

impl EmbeddedEditor {
    /// Embed `plugin`'s editor as a child of `parent`, at `rect`.
    ///
    /// Must be called on the UI/main thread (where your event loop runs). `parent` is the
    /// host window's handle (e.g. from `eframe::Frame::window_handle()`).
    pub fn embed(
        plugin: Arc<Mutex<Plugin>>,
        parent: RawWindowHandle,
        rect: EditorRect,
    ) -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let inner = macos::MacEmbed::new(&plugin, parent, rect)?;
            Ok(Self { plugin, inner })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (&plugin, parent, rect);
            Err(Error::Other(
                "editor embedding is only implemented on macOS so far".to_string(),
            ))
        }
    }

    /// Reposition/resize the embedded editor to track `rect`. Call each frame so the editor
    /// follows the host layout (scroll, window resize). No-op off macOS.
    pub fn set_rect(&self, rect: EditorRect) {
        #[cfg(target_os = "macos")]
        self.inner.set_rect(rect);
        #[cfg(not(target_os = "macos"))]
        let _ = rect;
    }

    /// Detach the editor and remove the child view (also done on drop).
    pub fn close(self) {}
}

impl Drop for EmbeddedEditor {
    fn drop(&mut self) {
        // Detach the plugin's view first; the platform child view is torn down by `inner`.
        if let Ok(mut p) = self.plugin.lock() {
            let _ = p.close_editor();
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::{rc::Retained, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    pub struct MacEmbed {
        parent: Retained<NSView>,
        child: Retained<NSView>,
    }

    impl MacEmbed {
        pub fn new(
            plugin: &Arc<Mutex<Plugin>>,
            parent: RawWindowHandle,
            rect: EditorRect,
        ) -> Result<Self> {
            let mtm = MainThreadMarker::new().ok_or_else(|| {
                Error::Other("editor embedding must run on the main thread".to_string())
            })?;
            let RawWindowHandle::AppKit(h) = parent else {
                return Err(Error::Other(
                    "expected an AppKit window handle for the parent".to_string(),
                ));
            };
            // The host owns `ns_view`; retain it so it outlives our use.
            let parent: Retained<NSView> =
                unsafe { Retained::retain(h.ns_view.as_ptr() as *mut NSView) }
                    .ok_or_else(|| Error::Other("null parent NSView".to_string()))?;

            // Create the container child view the plugin attaches into.
            let frame = NSRect::new(
                NSPoint::new(rect.x as f64, 0.0),
                NSSize::new(rect.width as f64, rect.height as f64),
            );
            let child = unsafe { NSView::initWithFrame(NSView::alloc(mtm), frame) };
            unsafe { parent.addSubview(&child) };

            let handle = crate::plugin::WindowHandle::from_nsview(
                Retained::as_ptr(&child) as *mut std::ffi::c_void
            );
            plugin
                .lock()
                .map_err(|_| Error::Other("plugin lock poisoned".to_string()))?
                .open_editor(handle)?;

            let embed = Self { parent, child };
            embed.set_rect(rect);
            Ok(embed)
        }

        pub fn set_rect(&self, rect: EditorRect) {
            // Convert egui's top-left origin to the parent view's coordinate space. AppKit
            // views are bottom-left origin unless flipped, so flip Y against the parent's
            // current height (which changes as the window resizes).
            let flipped = self.parent.isFlipped();
            let parent_height = self.parent.bounds().size.height;
            let y = if flipped {
                rect.y as f64
            } else {
                parent_height - (rect.y + rect.height) as f64
            };
            let frame = NSRect::new(
                NSPoint::new(rect.x as f64, y),
                NSSize::new(rect.width as f64, rect.height as f64),
            );
            unsafe { self.child.setFrame(frame) };
        }
    }

    impl Drop for MacEmbed {
        fn drop(&mut self) {
            unsafe { self.child.removeFromSuperview() };
        }
    }
}
