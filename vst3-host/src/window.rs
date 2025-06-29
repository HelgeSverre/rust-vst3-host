//! Plugin window management
//!
//! This module provides platform-specific window creation and management
//! for VST3 plugin GUIs.

use crate::error::{Error, Result};
use crate::plugin::Plugin;
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
use cocoa::{
    appkit::{NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask},
    base::{id, nil, NO},
    foundation::{NSPoint, NSRect, NSSize, NSString},
};
#[cfg(target_os = "macos")]
use objc::{class, msg_send, sel, sel_impl};

#[cfg(target_os = "windows")]
use winapi::{
    shared::minwindef::{HINSTANCE, HWND, LPARAM, LRESULT, UINT, WPARAM},
    shared::windef::RECT,
    um::libloaderapi::GetModuleHandleW,
    um::winuser::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, LoadCursorW, RegisterClassExW, ShowWindow,
        UpdateWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, SW_SHOW, WNDCLASSEXW,
        WS_OVERLAPPEDWINDOW,
    },
};

/// A plugin window that manages the native window and plugin editor lifecycle
pub struct PluginWindow {
    plugin: Arc<Mutex<Plugin>>,
    #[cfg(target_os = "macos")]
    native_window: Option<id>,
    #[cfg(target_os = "windows")]
    native_window: Option<HWND>,
    #[cfg(target_os = "linux")]
    native_window: Option<u64>,
}

impl PluginWindow {
    /// Create a new plugin window for the given plugin
    pub fn new(plugin: Arc<Mutex<Plugin>>) -> Self {
        Self {
            plugin,
            #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
            native_window: None,
        }
    }

    /// Open the plugin window
    pub fn open(&mut self) -> Result<()> {
        // Check if plugin has editor
        let has_editor = self.plugin.lock().unwrap().has_editor();
        if !has_editor {
            return Err(Error::Other(
                "Plugin does not have a GUI editor".to_string(),
            ));
        }

        // Close existing window if any
        if self.is_open() {
            self.close();
        }

        // Get plugin info for window title
        let plugin_info = self.plugin.lock().unwrap().info().clone();

        // Try to get editor size
        let (width, height) = self
            .plugin
            .lock()
            .unwrap()
            .get_editor_size()
            .unwrap_or((800, 600));

        // Create native window
        #[cfg(target_os = "macos")]
        {
            unsafe {
                // Create window frame
                let frame = NSRect::new(
                    NSPoint::new(100.0, 100.0),
                    NSSize::new(width as f64, height as f64),
                );

                let style = NSWindowStyleMask::NSTitledWindowMask
                    | NSWindowStyleMask::NSClosableWindowMask
                    | NSWindowStyleMask::NSMiniaturizableWindowMask;

                // Create window
                let window: id = msg_send![class!(NSWindow), alloc];
                let window: id = msg_send![window,
                    initWithContentRect:frame
                    styleMask:style
                    backing:NSBackingStoreType::NSBackingStoreBuffered
                    defer:NO];

                // Set window title
                let title = NSString::alloc(nil).init_str(&format!("{} - VST3", plugin_info.name));
                let _: () = msg_send![window, setTitle:title];

                // Get content view
                let content_view: id = msg_send![window, contentView];

                // Create a container view for the plugin with exact size
                let container_frame = NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(width as f64, height as f64),
                );
                let container_view: id = msg_send![class!(NSView), alloc];
                let container_view: id = msg_send![container_view, initWithFrame:container_frame];
                let _: () = msg_send![content_view, addSubview:container_view];

                // Try to open plugin editor
                let window_handle = crate::plugin::WindowHandle::from_nsview(
                    container_view as *mut std::ffi::c_void,
                );
                self.plugin.lock().unwrap().open_editor(window_handle)?;

                // Resize window to match plugin view
                let _: () = msg_send![window, setContentSize:container_frame.size];

                // Show and center window
                let _: () = msg_send![window, makeKeyAndOrderFront:nil];
                let _: () = msg_send![window, center];

                self.native_window = Some(window);
            }
        }

        #[cfg(target_os = "windows")]
        {
            unsafe {
                use std::mem;
                use std::ptr;

                // Register window class if not already registered
                let class_name = "VST3PluginWindow\0".encode_utf16().collect::<Vec<u16>>();
                let mut wc: WNDCLASSEXW = mem::zeroed();
                wc.cbSize = mem::size_of::<WNDCLASSEXW>() as UINT;
                wc.style = CS_HREDRAW | CS_VREDRAW;
                wc.lpfnWndProc = Some(DefWindowProcW);
                wc.hInstance = GetModuleHandleW(ptr::null());
                wc.hCursor = LoadCursorW(ptr::null_mut(), IDC_ARROW);
                wc.lpszClassName = class_name.as_ptr();

                // Try to register, ignore if already registered
                RegisterClassExW(&wc);

                // Create window
                let window_title = format!("{} - VST3\0", plugin_info.name);
                let window_name = window_title.encode_utf16().collect::<Vec<u16>>();

                // Calculate window size including borders
                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                };

                winapi::um::winuser::AdjustWindowRectEx(
                    &mut rect,
                    WS_OVERLAPPEDWINDOW,
                    0, // No menu
                    0, // No extended style
                );

                let window_width = rect.right - rect.left;
                let window_height = rect.bottom - rect.top;

                let hwnd = CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    window_name.as_ptr(),
                    WS_OVERLAPPEDWINDOW,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    window_width,
                    window_height,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    GetModuleHandleW(ptr::null()),
                    ptr::null_mut(),
                );

                if hwnd.is_null() {
                    return Err(Error::Other("Failed to create native window".to_string()));
                }

                // Try to open plugin editor
                let window_handle =
                    crate::plugin::WindowHandle::from_hwnd(hwnd as *mut std::ffi::c_void);
                match self.plugin.lock().unwrap().open_editor(window_handle) {
                    Ok(()) => {
                        ShowWindow(hwnd, SW_SHOW);
                        UpdateWindow(hwnd);
                        self.native_window = Some(hwnd);
                    }
                    Err(e) => {
                        DestroyWindow(hwnd);
                        return Err(e);
                    }
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            return Err(Error::Other(
                "Plugin GUI not yet implemented for Linux".to_string(),
            ));
        }

        Ok(())
    }

    /// Close the plugin window
    pub fn close(&mut self) {
        // Close the plugin editor first
        if let Ok(mut plugin) = self.plugin.lock() {
            let _ = plugin.close_editor();
        }

        // Then close the native window
        #[cfg(target_os = "macos")]
        {
            if let Some(window) = self.native_window.take() {
                unsafe {
                    let _: () = msg_send![window, close];
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Some(hwnd) = self.native_window.take() {
                unsafe {
                    DestroyWindow(hwnd);
                }
            }
        }
    }

    /// Check if the window is currently open
    pub fn is_open(&self) -> bool {
        self.native_window.is_some()
    }
}

impl Drop for PluginWindow {
    fn drop(&mut self) {
        self.close();
    }
}

/// Builder for creating plugin windows with egui integration
#[cfg(feature = "egui-widgets")]
pub struct PluginWindowBuilder {
    plugin: Arc<Mutex<Plugin>>,
}

#[cfg(feature = "egui-widgets")]
impl PluginWindowBuilder {
    /// Create a new builder for the given plugin
    pub fn new(plugin: Arc<Mutex<Plugin>>) -> Self {
        Self { plugin }
    }

    /// Build and open a standalone plugin window
    pub fn open_standalone(&self) -> Result<PluginWindow> {
        let mut window = PluginWindow::new(self.plugin.clone());
        window.open()?;
        Ok(window)
    }
}
