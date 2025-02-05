#![allow(deprecated)]

use eframe::egui;
use raw_window_handle::{HasRawWindowHandle, HasWindowHandle, RawWindowHandle};
use std::ffi::c_void;
use std::ptr;
use vst3::Steinberg::Vst::{IEditController, IEditControllerTrait};
use vst3::Steinberg::{IPlugView, IPlugViewTrait, IPluginFactoryTrait};
use vst3::Steinberg::{IPluginBase, IPluginFactory};
use vst3::{ComPtr, Interface};

use core_foundation::base::TCFType;
use libloading::os::unix::{Library, Symbol};

// const PLUGIN_PATH: &str = "/Library/Audio/Plug-Ins/VST3/Surge XT.vst3/Contents/MacOS/Surge XT";
const PLUGIN_PATH: &str = "/Users/helge/code/vst-host/tmp/Dexed.vst3/Contents/MacOS/Dexed";

fn main() {
    if !std::path::Path::new(PLUGIN_PATH).exists() {
        println!("File does not exist: {}", PLUGIN_PATH);
        return;
    }

    let (_plugin, plug_view) =
        unsafe { load_vst3_plugin(PLUGIN_PATH) }.expect("Failed to load VST3 plugin");

    let native_options = eframe::NativeOptions::default();
    let _ = eframe::run_native(
        "VST3 UI Host",
        native_options,
        Box::new(move |cc| Ok(Box::new(VST3Host::new(cc, plug_view)))),
    );
}

unsafe fn load_vst3_plugin(path: &str) -> Result<(ComPtr<IPluginBase>, ComPtr<IPlugView>), String> {
    let lib = Library::new(path).map_err(|e| format!("❌ Failed to load VST3 bundle: {}", e))?;

    let get_factory: Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> = lib
        .get(b"GetPluginFactory")
        .map_err(|e| format!("❌ Failed to load `GetPluginFactory`: {}", e))?;

    let factory_ptr = get_factory();
    if factory_ptr.is_null() {
        return Err("❌ `GetPluginFactory` returned NULL".into());
    }

    let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr)
        .ok_or("❌ Failed to wrap IPluginFactory")?;

    // Iterate through available plugin classes
    let mut class_id = None;
    for i in 0..factory.countClasses() {
        let mut class_info = std::mem::zeroed();
        if factory.getClassInfo(i, &mut class_info) == vst3::Steinberg::kResultOk {
            let category = std::str::from_utf8(std::slice::from_raw_parts(
                class_info.category.as_ptr() as *const u8,
                class_info.category.len(),
            ))
            .unwrap_or("Invalid UTF-8");

            println!(
                "🔹 Found Class: {} (Category: {:?})",
                std::str::from_utf8(std::slice::from_raw_parts(
                    class_info.name.as_ptr() as *const u8,
                    class_info.name.len()
                ))
                .unwrap_or("Invalid UTF-8"),
                category
            );

            // Choose the correct class (e.g., "Audio Module Class")
            if category.contains("Audio Module Class") {
                println!(
                    "🔹 -------- Using class: {} - {}, cid: {:?}",
                    std::str::from_utf8(std::slice::from_raw_parts(
                        class_info.name.as_ptr() as *const u8,
                        class_info.name.len()
                    ))
                    .unwrap_or("Invalid UTF-8"),
                    category,
                    class_info.cid
                );
                class_id = Some(class_info.cid);
                break;
            }
        }
    }

    let class_id = class_id.ok_or("❌ No valid VST3 plugin class found")?;

    // Create the plugin instance
    let mut plugin_ptr: *mut IPluginBase = ptr::null_mut();
    let result = factory.createInstance(
        class_id.as_ptr() as *const i8,
        IPluginBase::IID.as_ptr() as *const i8,
        &mut plugin_ptr as *mut _ as *mut _,
    );

    if result != vst3::Steinberg::kResultOk {
        return Err(format!(
            "❌ Failed to create plugin instance, result: {}",
            result
        ));
    }

    let plugin =
        ComPtr::<IPluginBase>::from_raw(plugin_ptr).ok_or("❌ Failed to wrap IPluginBase")?;

    // Use `cast` instead of `query_interface`
    let edit_controller = plugin
        .cast::<IEditController>()
        .ok_or("❌ Failed to get IEditController")?;

    let plug_view_ptr = edit_controller.createView(b"editor\0".as_ptr() as *const _);
    if plug_view_ptr.is_null() {
        return Err("❌ Failed to create IPlugView".into());
    }

    let plug_view =
        ComPtr::<IPlugView>::from_raw(plug_view_ptr).ok_or("❌ Failed to wrap IPlugView")?;

    Ok((plugin, plug_view))
}

struct VST3Host {
    plug_view: ComPtr<IPlugView>,
}

impl VST3Host {
    fn new(_cc: &eframe::CreationContext<'_>, plug_view: ComPtr<IPlugView>) -> Self {
        Self { plug_view }
    }
}

impl eframe::App for VST3Host {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("VST3 Plugin Host");

            let window_handle = match frame.window_handle() {
                Ok(handle) => handle,
                Err(_) => {
                    ui.label("Failed to get window handle");
                    return;
                }
            };

            let hwnd: *mut c_void = match window_handle.raw_window_handle().unwrap() {
                // RawWindowHandle::Win32(handle) => handle.hwnd.map(|h| h.get()).unwrap_or(0) as *mut c_void,
                // RawWindowHandle::Xlib(handle) => handle.window as *mut c_void,
                RawWindowHandle::Wayland(handle) => handle.surface.as_ptr(),
                _ => {
                    ui.label("Unsupported platform");
                    return;
                }
            };

            let result = unsafe {
                self.plug_view
                    .attached(hwnd, vst3::Steinberg::kPlatformTypeHWND)
            };

            if result == vst3::Steinberg::kResultOk {
                ui.label("VST3 GUI loaded successfully");
            } else {
                ui.label("Failed to attach VST3 GUI to window");
            }
        });
    }
}

// #![allow(deprecated)]
//
// use eframe::egui;
// use libloading::{Library, Symbol};
// use raw_window_handle::{HasRawWindowHandle, HasWindowHandle, RawWindowHandle};
// use std::ffi::c_void;
// use std::ptr;
// use vst3::Steinberg::Vst::{IEditController, IEditControllerTrait};
// use vst3::Steinberg::{IPlugView, IPlugViewTrait, IPluginFactoryTrait};
// use vst3::Steinberg::{IPluginBase, IPluginFactory};
// use vst3::{ComPtr, Interface};
//
// const PLUGIN_PATH: &str = "/Users/helge/code/vst-host/tmp/Dexed.vst3";
//
// fn main() {
//     // Check if file exists
//     if !std::path::Path::new(PLUGIN_PATH).exists() {
//         println!("File does not exist: {}", PLUGIN_PATH);
//         return;
//     }
//
//     let (_plugin, plug_view) = load_vst3_plugin(PLUGIN_PATH).expect("Failed to load VST3 plugin");
//
//     let native_options = eframe::NativeOptions::default();
//     let _ = eframe::run_native(
//         "VST3 UI Host",
//         native_options,
//         Box::new(move |cc| Ok(Box::new(VST3Host::new(cc, plug_view)))),
//     );
// }
//
// fn load_vst3_plugin(path: &str) -> Result<(ComPtr<IPluginBase>, ComPtr<IPlugView>), String> {
//     let lib = unsafe { Library::new(path) }.map_err(|e| format!("Failed to load plugin: {}", e))?;
//
//     let get_factory: Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> =
//         unsafe { lib.get(b"GetPluginFactory") }
//             .map_err(|e| format!("Failed to load GetPluginFactory: {}", e))?;
//
//     let factory_ptr = unsafe { get_factory() };
//     if factory_ptr.is_null() {
//         return Err("Failed to get IPluginFactory".into());
//     }
//
//     let factory = unsafe { ComPtr::<IPluginFactory>::from_raw(factory_ptr) }
//         .ok_or("Failed to wrap IPluginFactory")?;
//
//     let mut plugin_ptr: *mut IPluginBase = ptr::null_mut();
//     let class_id = vst3::Steinberg::Vst::IComponent::IID;
//
//     let result = unsafe {
//         factory.createInstance(
//             class_id.as_ptr() as *const i8,         // Convert to FIDString
//             IPluginBase::IID.as_ptr() as *const i8, // Convert to FIDString
//             &mut plugin_ptr as *mut _ as *mut _,
//         )
//     };
//
//     if result != vst3::Steinberg::kResultOk {
//         return Err(format!("Failed to create plugin instance, result: 0x{:x}", result));
//     }
//
//     let plugin = unsafe { ComPtr::<IPluginBase>::from_raw(plugin_ptr) }
//         .ok_or("Failed to wrap IPluginBase")?;
//
//     let edit_controller =
//         { plugin.cast::<IEditController>() }.ok_or("Failed to get IEditController")?;
//
//     let plug_view_ptr = unsafe { edit_controller.createView(b"editor\0".as_ptr() as *const _) };
//     if plug_view_ptr.is_null() {
//         return Err("Failed to create IPlugView".into());
//     }
//
//     let plug_view = unsafe { ComPtr::<IPlugView>::from_raw(plug_view_ptr) }
//         .ok_or("Failed to wrap IPlugView")?;
//
//     Ok((plugin, plug_view))
// }
//
// struct VST3Host {
//     plug_view: ComPtr<IPlugView>,
// }
//
// impl VST3Host {
//     fn new(_cc: &eframe::CreationContext<'_>, plug_view: ComPtr<IPlugView>) -> Self {
//         Self { plug_view }
//     }
// }
//
// impl eframe::App for VST3Host {
//     fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
//         egui::CentralPanel::default().show(ctx, |ui| {
//             ui.heading("VST3 Plugin Host");
//
//             let window_handle = match frame.window_handle() {
//                 Ok(handle) => handle,
//                 Err(_) => {
//                     ui.label("Failed to get window handle");
//                     return;
//                 }
//             };
//
//             let hwnd: *mut c_void = match window_handle.raw_window_handle().unwrap() {
//                 // RawWindowHandle::Win32(handle) => handle.hwnd.map(|h| h.get()).unwrap_or(0) as *mut c_void,
//                 // RawWindowHandle::Xlib(handle) => handle.window as *mut c_void,
//                 RawWindowHandle::Wayland(handle) => handle.surface.as_ptr(),
//                 _ => {
//                     ui.label("Unsupported platform");
//                     return;
//                 }
//             };
//
//             let result = unsafe {
//                 self.plug_view
//                     .attached(hwnd, vst3::Steinberg::kPlatformTypeHWND)
//             };
//
//             if result == vst3::Steinberg::kResultOk {
//                 ui.label("VST3 GUI loaded successfully");
//             } else {
//                 ui.label("Failed to attach VST3 GUI to window");
//             }
//         });
//     }
// }
