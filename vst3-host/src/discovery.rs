//! VST3 plugin discovery functionality

use crate::{error::Result, plugin::PluginInfo};
use std::path::{Path, PathBuf};
use std::ptr;

/// Scan standard VST3 directories for plugins
pub fn scan_standard_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(format!(
                "{}/Library/Audio/Plug-Ins/VST3",
                home
            )));
        }
    }

    #[cfg(target_os = "windows")]
    {
        paths.push(PathBuf::from(r"C:\Program Files\Common Files\VST3"));
        paths.push(PathBuf::from(r"C:\Program Files (x86)\Common Files\VST3"));
    }

    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/usr/lib/vst3"));
        paths.push(PathBuf::from("/usr/local/lib/vst3"));
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(format!("{}/.vst3", home)));
        }
    }

    paths
}

/// Scan directories for VST3 plugins
pub fn scan_directories(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut plugins = Vec::new();

    for path in paths {
        if path.exists() {
            scan_directory(path, &mut plugins)?;
        }
    }

    // Remove duplicates and sort
    plugins.sort();
    plugins.dedup();

    Ok(plugins)
}

/// Check if a plugin should be blacklisted
fn is_blacklisted(path: &Path) -> bool {
    if let Some(file_name) = path.file_name() {
        if let Some(name_str) = file_name.to_str() {
            let name_lower = name_str.to_lowercase();
            // Blacklist plugins known to cause issues
            return name_lower.contains("wave") || name_lower.contains("ozone");
        }
    }
    false
}

/// Recursively scan a directory for VST3 plugins
fn scan_directory(dir: &Path, plugins: &mut Vec<PathBuf>) -> Result<()> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // Check if it's a VST3 bundle/file
            if let Some(ext) = path.extension() {
                if ext == "vst3" {
                    // Skip blacklisted plugins
                    if !is_blacklisted(&path) {
                        plugins.push(path.clone());
                    } else {
                        eprintln!("Skipping blacklisted plugin: {}", path.display());
                    }
                }
            }

            // Recursively scan subdirectories (but not .vst3 bundles)
            if path.is_dir() && path.extension() != Some(std::ffi::OsStr::new("vst3")) {
                scan_directory(&path, plugins)?;
            }
        }
    }

    Ok(())
}

/// Get metadata for a VST3 plugin without fully loading it
pub fn get_plugin_info(path: &Path) -> Result<PluginInfo> {
    use vst3::Steinberg::Vst::BusDirections_::*;
    use vst3::Steinberg::Vst::MediaTypes_::*;
    use vst3::{ComPtr, Interface, Steinberg::Vst::*, Steinberg::*};

    unsafe {
        // Get the binary path
        let binary_path = get_vst3_binary_path(path)?;

        // Load the library
        let library = libloading::Library::new(&binary_path).map_err(|e| {
            crate::Error::PluginLoadFailed(format!("Failed to load library: {}", e))
        })?;

        // Get factory function
        type GetPluginFactoryFunc = unsafe extern "C" fn() -> *mut IPluginFactory;
        let get_factory: libloading::Symbol<GetPluginFactoryFunc> =
            library.get(b"GetPluginFactory\0").map_err(|e| {
                crate::Error::PluginLoadFailed(format!("Failed to find GetPluginFactory: {}", e))
            })?;

        let factory_ptr = get_factory();
        if factory_ptr.is_null() {
            return Err(crate::Error::PluginLoadFailed(
                "GetPluginFactory returned null".to_string(),
            ));
        }

        let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or_else(|| {
            crate::Error::PluginLoadFailed("Failed to create factory ComPtr".to_string())
        })?;

        // Get factory info
        let mut factory_info: PFactoryInfo = std::mem::zeroed();
        factory.getFactoryInfo(&mut factory_info);

        let vendor = crate::internal::utils::c_str_to_string(&factory_info.vendor);

        // Find audio component
        let num_classes = factory.countClasses();
        let mut plugin_name = String::new();
        let mut category = String::new();
        let mut uid = String::new();
        let mut has_midi_input = false;
        let mut audio_inputs = 0u32;
        let mut audio_outputs = 0u32;
        let mut has_gui = false;

        for i in 0..num_classes {
            let mut class_info: PClassInfo = std::mem::zeroed();
            if factory.getClassInfo(i, &mut class_info) == kResultOk {
                let class_category = crate::internal::utils::c_str_to_string(&class_info.category);

                if class_category.contains("Audio Module Class") {
                    plugin_name = crate::internal::utils::c_str_to_string(&class_info.name);
                    category = if class_category.contains("Instrument") {
                        "Instrument".to_string()
                    } else if class_category.contains("Fx") {
                        "Effect".to_string()
                    } else {
                        "Other".to_string()
                    };

                    // Convert UID to string
                    // cid is an array of bytes, convert to hex string
                    uid = class_info
                        .cid
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<String>();

                    // Try to create component to get more info
                    let mut component_ptr: *mut IComponent = ptr::null_mut();
                    let result = factory.createInstance(
                        class_info.cid.as_ptr() as *const i8,
                        IComponent::IID.as_ptr() as *const i8,
                        &mut component_ptr as *mut _ as *mut _,
                    );

                    if result == kResultOk && !component_ptr.is_null() {
                        let component = ComPtr::<IComponent>::from_raw(component_ptr).unwrap();

                        // Initialize to get bus info
                        component.initialize(ptr::null_mut());

                        // Get bus counts
                        audio_inputs = component.getBusCount(kAudio as i32, kInput as i32) as u32;
                        audio_outputs = component.getBusCount(kAudio as i32, kOutput as i32) as u32;

                        // Check for MIDI input
                        let event_inputs = component.getBusCount(kEvent as i32, kInput as i32);
                        has_midi_input = event_inputs > 0;

                        // Check for GUI
                        if let Some(controller) = component.cast::<IEditController>() {
                            has_gui = true;
                            controller.terminate();
                        }

                        // Cleanup
                        component.terminate();
                    }

                    break;
                }
            }
        }

        // If no audio component found, use first class
        if plugin_name.is_empty() && num_classes > 0 {
            let mut class_info: PClassInfo = std::mem::zeroed();
            if factory.getClassInfo(0, &mut class_info) == kResultOk {
                plugin_name = crate::internal::utils::c_str_to_string(&class_info.name);
            }
        }

        Ok(PluginInfo {
            path: path.to_path_buf(),
            name: if plugin_name.is_empty() {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Unknown")
                    .to_string()
            } else {
                plugin_name
            },
            vendor,
            version: "1.0.0".to_string(), // Version not available in PClassInfo
            category,
            uid,
            audio_inputs,
            audio_outputs,
            has_midi_input,
            has_midi_output: false, // Would need to check event output buses
            has_gui,
        })
    }
}

/// Platform-specific VST3 binary path resolution
pub fn get_vst3_binary_path(bundle_path: &Path) -> Result<PathBuf> {
    // If it's already pointing to the binary, use it
    if bundle_path.is_file() {
        return Ok(bundle_path.to_path_buf());
    }

    // Platform-specific VST3 bundle handling
    #[cfg(target_os = "macos")]
    {
        // macOS: .vst3 bundle structure
        if bundle_path.extension() == Some(std::ffi::OsStr::new("vst3")) {
            let contents_path = bundle_path.join("Contents").join("MacOS");
            if let Ok(entries) = std::fs::read_dir(&contents_path) {
                for entry in entries.flatten() {
                    let file_path = entry.path();
                    if file_path.is_file() {
                        if let Some(name) = file_path.file_name() {
                            if let Some(name_str) = name.to_str() {
                                // Skip hidden files and common non-binary files
                                if !name_str.starts_with('.')
                                    && !name_str.ends_with(".plist")
                                    && !name_str.ends_with(".txt")
                                {
                                    return Ok(file_path);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Windows: .vst3 file or folder structure
        if bundle_path.is_dir() {
            // Look for .vst3 file in Contents/x86_64-win or Contents/x86-win
            let x64_path = bundle_path.join("Contents").join("x86_64-win");
            let x86_path = bundle_path.join("Contents").join("x86-win");

            for contents_path in &[x64_path, x86_path] {
                if let Ok(entries) = std::fs::read_dir(contents_path) {
                    for entry in entries.flatten() {
                        let file_path = entry.path();
                        if file_path.extension() == Some(std::ffi::OsStr::new("vst3")) {
                            return Ok(file_path);
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Linux: Similar to Windows
        if bundle_path.is_dir() {
            let contents_path = bundle_path.join("Contents");
            let arch_paths = [
                contents_path.join("x86_64-linux"),
                contents_path.join("i386-linux"),
            ];

            for arch_path in &arch_paths {
                if let Ok(entries) = std::fs::read_dir(arch_path) {
                    for entry in entries.flatten() {
                        let file_path = entry.path();
                        if file_path.extension() == Some(std::ffi::OsStr::new("so")) {
                            return Ok(file_path);
                        }
                    }
                }
            }
        }
    }

    Err(crate::Error::PluginNotFound(format!(
        "Could not find VST3 binary in bundle: {}",
        bundle_path.display()
    )))
}
