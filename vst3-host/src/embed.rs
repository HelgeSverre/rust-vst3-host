//! Embed a plugin editor inside a host window (e.g. an egui/eframe window), as an
//! alternative to the standalone [`PluginWindow`](crate::PluginWindow).
//!
//! Instead of opening its own top-level OS window, the plugin's native editor view is
//! parented as a child of a window your UI framework already owns, positioned to track a
//! region you allocate. You provide the parent window's [`RawWindowHandle`] and a target
//! rectangle (in logical points, top-left origin — the egui convention) each frame.
//!
//! Implemented on macOS (verified), Windows, and Linux/X11. Other platforms return an error
//! from [`EmbeddedEditor::embed`]. Requires the `egui-widgets` feature.
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
    #[cfg(target_os = "windows")]
    inner: windows::WinEmbed,
    #[cfg(target_os = "linux")]
    inner: linux::LinuxEmbed,
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
        #[cfg(target_os = "windows")]
        {
            let inner = windows::WinEmbed::new(&plugin, parent, rect)?;
            Ok(Self { plugin, inner })
        }
        #[cfg(target_os = "linux")]
        {
            let inner = linux::LinuxEmbed::new(&plugin, parent, rect)?;
            Ok(Self { plugin, inner })
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = (&plugin, parent, rect);
            Err(Error::Other(
                "editor embedding is not implemented on this platform".to_string(),
            ))
        }
    }

    /// Reposition/resize the embedded editor to track `rect`. Call each frame so the editor
    /// follows the host layout (scroll, window resize). No-op off macOS.
    pub fn set_rect(&self, rect: EditorRect) {
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        self.inner.set_rect(rect);
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
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
            let child = NSView::initWithFrame(NSView::alloc(mtm), frame);
            parent.addSubview(&child);

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
            self.child.setFrame(frame);
        }
    }

    impl Drop for MacEmbed {
        fn drop(&mut self) {
            self.child.removeFromSuperview();
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use winapi::shared::windef::HWND;
    use winapi::um::libloaderapi::GetModuleHandleW;
    use winapi::um::winuser::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, SetWindowPos, ShowWindow,
        CS_HREDRAW, CS_VREDRAW, SWP_NOZORDER, SW_SHOW, WNDCLASSEXW, WS_CHILD, WS_VISIBLE,
    };

    /// A plugin editor embedded as a child `HWND` of the host window.
    pub struct WinEmbed {
        child: HWND,
    }

    impl WinEmbed {
        pub fn new(
            plugin: &Arc<Mutex<Plugin>>,
            parent: RawWindowHandle,
            rect: EditorRect,
        ) -> Result<Self> {
            let RawWindowHandle::Win32(h) = parent else {
                return Err(Error::Other(
                    "expected a Win32 window handle for the parent".to_string(),
                ));
            };
            unsafe {
                let parent_hwnd = h.hwnd.get() as HWND;
                let hinstance = GetModuleHandleW(std::ptr::null());

                // Register a child window class (idempotent across calls).
                let class_name: Vec<u16> = "VST3EmbeddedEditor\0".encode_utf16().collect();
                let mut wc: WNDCLASSEXW = std::mem::zeroed();
                wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
                wc.style = CS_HREDRAW | CS_VREDRAW;
                wc.lpfnWndProc = Some(DefWindowProcW);
                wc.hInstance = hinstance;
                wc.lpszClassName = class_name.as_ptr();
                RegisterClassExW(&wc);

                let child = CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    std::ptr::null(),
                    WS_CHILD | WS_VISIBLE,
                    rect.x as i32,
                    rect.y as i32,
                    rect.width as i32,
                    rect.height as i32,
                    parent_hwnd,
                    std::ptr::null_mut(),
                    hinstance,
                    std::ptr::null_mut(),
                );
                if child.is_null() {
                    return Err(Error::Other("Failed to create child window".to_string()));
                }

                let handle = crate::plugin::WindowHandle::from_hwnd(child as *mut std::ffi::c_void);
                if let Err(e) = plugin
                    .lock()
                    .map_err(|_| Error::Other("plugin lock poisoned".to_string()))?
                    .open_editor(handle)
                {
                    DestroyWindow(child);
                    return Err(e);
                }
                ShowWindow(child, SW_SHOW);
                Ok(Self { child })
            }
        }

        pub fn set_rect(&self, rect: EditorRect) {
            unsafe {
                SetWindowPos(
                    self.child,
                    std::ptr::null_mut(),
                    rect.x as i32,
                    rect.y as i32,
                    rect.width as i32,
                    rect.height as i32,
                    SWP_NOZORDER,
                );
            }
        }
    }

    impl Drop for WinEmbed {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.child);
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use xcb::{x, Xid, XidNew};

    /// A plugin editor embedded as a child X11 window of the host window.
    pub struct LinuxEmbed {
        connection: xcb::Connection,
        child: x::Window,
    }

    impl LinuxEmbed {
        pub fn new(
            plugin: &Arc<Mutex<Plugin>>,
            parent: RawWindowHandle,
            rect: EditorRect,
        ) -> Result<Self> {
            let parent_id: u32 = match parent {
                RawWindowHandle::Xcb(h) => h.window.get(),
                RawWindowHandle::Xlib(h) => h.window as u32,
                _ => {
                    return Err(Error::Other(
                        "expected an X11 (Xcb/Xlib) window handle for the parent".to_string(),
                    ))
                }
            };

            let (connection, screen_number) = xcb::Connection::connect(None)
                .map_err(|e| Error::Other(format!("Failed to connect to X server: {e}")))?;
            let visual = {
                let setup = connection.get_setup();
                let screen = setup
                    .roots()
                    .nth(screen_number as usize)
                    .ok_or_else(|| Error::Other("No X11 screen found".to_string()))?;
                screen.root_visual()
            };
            // SAFETY: `parent_id` is a live X11 window id from the host's RawWindowHandle.
            let parent_win: x::Window = unsafe { x::Window::new(parent_id) };
            let child = connection.generate_id();

            connection
                .send_and_check_request(&x::CreateWindow {
                    depth: x::COPY_FROM_PARENT as u8,
                    wid: child,
                    parent: parent_win,
                    x: rect.x as i16,
                    y: rect.y as i16,
                    width: (rect.width as u16).max(1),
                    height: (rect.height as u16).max(1),
                    border_width: 0,
                    class: x::WindowClass::InputOutput,
                    visual,
                    value_list: &[x::Cw::EventMask(x::EventMask::EXPOSURE)],
                })
                .map_err(|e| Error::Other(format!("Failed to create X11 child window: {e}")))?;
            connection.send_request(&x::MapWindow { window: child });
            let _ = connection.flush();

            let handle = crate::plugin::WindowHandle::from_x11(child.resource_id());
            if let Err(e) = plugin
                .lock()
                .map_err(|_| Error::Other("plugin lock poisoned".to_string()))?
                .open_editor(handle)
            {
                connection.send_request(&x::DestroyWindow { window: child });
                let _ = connection.flush();
                return Err(e);
            }

            Ok(Self { connection, child })
        }

        pub fn set_rect(&self, rect: EditorRect) {
            self.connection.send_request(&x::ConfigureWindow {
                window: self.child,
                value_list: &[
                    x::ConfigWindow::X(rect.x as i32),
                    x::ConfigWindow::Y(rect.y as i32),
                    x::ConfigWindow::Width((rect.width as u32).max(1)),
                    x::ConfigWindow::Height((rect.height as u32).max(1)),
                ],
            });
            let _ = self.connection.flush();
        }
    }

    impl Drop for LinuxEmbed {
        fn drop(&mut self) {
            self.connection
                .send_request(&x::DestroyWindow { window: self.child });
            let _ = self.connection.flush();
        }
    }
}
