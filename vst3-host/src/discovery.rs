//! VST3 plugin discovery functionality

use crate::{error::Result, plugin::PluginInfo};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::ptr;

/// Factory-level metadata (the plugin vendor's identity).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FactoryInfo {
    /// Vendor / manufacturer name.
    pub vendor: String,
    /// Vendor URL.
    pub url: String,
    /// Vendor contact email.
    pub email: String,
    /// Raw factory flags.
    pub flags: i32,
}

/// One class exported by a plugin's factory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClassInfo {
    /// Class display name.
    pub name: String,
    /// Class category (e.g. "Audio Module Class").
    pub category: String,
    /// Class id, hex-encoded.
    pub class_id: String,
    /// Instantiation cardinality.
    pub cardinality: i32,
    /// Version string (if available).
    pub version: String,
}

/// One audio or event bus.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BusInfo {
    /// Bus display name.
    pub name: String,
    /// Bus type (Main = 0, Aux = 1).
    pub bus_type: i32,
    /// Raw bus flags.
    pub flags: i32,
    /// Number of channels on this bus.
    pub channel_count: i32,
}

/// The plugin's full bus layout.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BusLayout {
    /// Audio input buses.
    pub audio_inputs: Vec<BusInfo>,
    /// Audio output buses.
    pub audio_outputs: Vec<BusInfo>,
    /// Event (MIDI) input buses.
    pub event_inputs: Vec<BusInfo>,
    /// Event (MIDI) output buses.
    pub event_outputs: Vec<BusInfo>,
}

/// A deep introspection report for a VST3 plugin — factory, classes, and bus layout.
/// This is the static metadata a plugin *inspector* UI needs, beyond the lightweight
/// [`PluginInfo`]. For the parameter list, load the plugin and call
/// [`crate::Plugin::get_parameters`] (which runs the full controller logic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailedPluginInfo {
    /// The basic metadata (also part of this report for convenience).
    pub info: PluginInfo,
    /// Factory / vendor identity.
    pub factory: FactoryInfo,
    /// All classes exported by the factory.
    pub classes: Vec<ClassInfo>,
    /// Full audio + event bus layout.
    pub buses: BusLayout,
}

/// A complete, serializable report of a plugin: static introspection plus its parameter
/// list. Build it after loading the plugin and serialize to JSON for export (e.g. the
/// inspector's "Copy JSON", or feeding plugin metadata to other tools).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginReport {
    /// Static introspection: factory, classes, bus layout, basic info.
    pub detailed: DetailedPluginInfo,
    /// The plugin's parameters (normalized values + metadata).
    pub parameters: Vec<crate::parameters::Parameter>,
}

impl PluginReport {
    /// Bundle a [`DetailedPluginInfo`] with a parameter list (from
    /// [`crate::Plugin::get_parameters`]).
    pub fn new(
        detailed: DetailedPluginInfo,
        parameters: Vec<crate::parameters::Parameter>,
    ) -> Self {
        Self {
            detailed,
            parameters,
        }
    }

    /// Serialize the report to pretty-printed JSON.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

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
            // Blacklist plugins known to cause issues (removed wave blacklisting)
            return name_lower.contains("ozone"); // Only ozone for now
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
        // Load the module using our VST3-compliant module loader
        let module = crate::internal::module_loader::load_module(path)?;

        // Get factory using the proper VST3 loading sequence
        let factory_ptr = module.get_factory()?;

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
        let mut version = String::new();
        let mut uid = String::new();
        let mut has_midi_input = false;
        let mut has_midi_output = false;
        let mut audio_inputs = 0u32;
        let mut audio_outputs = 0u32;
        let mut has_gui = false;

        for i in 0..num_classes {
            let mut class_info: PClassInfo = std::mem::zeroed();
            if factory.getClassInfo(i, &mut class_info) == kResultOk {
                let class_category = crate::internal::utils::c_str_to_string(&class_info.category);

                if class_category.contains("Audio Module Class") {
                    plugin_name = crate::internal::utils::c_str_to_string(&class_info.name);

                    // Real version + sub-categories via IPluginFactory2 (PClassInfo.category
                    // is just "Audio Module Class"; the useful sub-categories live in
                    // PClassInfo2.subCategories). Left empty rather than faked when absent.
                    if let Some(f2) = factory.cast::<IPluginFactory2>() {
                        let mut info2: PClassInfo2 = std::mem::zeroed();
                        if f2.getClassInfo2(i, &mut info2) == kResultOk {
                            version = crate::internal::utils::c_str_to_string(&info2.version);
                            category =
                                crate::internal::utils::c_str_to_string(&info2.subCategories);
                        }
                    }

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
                        class_info.cid.as_ptr() as *const std::os::raw::c_char,
                        IComponent::IID.as_ptr() as *const std::os::raw::c_char,
                        &mut component_ptr as *mut _ as *mut _,
                    );

                    if result == kResultOk && !component_ptr.is_null() {
                        let component = ComPtr::<IComponent>::from_raw(component_ptr).unwrap();

                        // Initialize with a host context (null crashes u-he/Waves plugins).
                        let host_app =
                            crate::internal::com_implementations::create_host_application();
                        let host_ctx = host_app.to_com_ptr::<IHostApplication>();
                        let context = host_ctx
                            .as_ref()
                            .map(|p| p.as_ptr() as *mut FUnknown)
                            .unwrap_or(ptr::null_mut());
                        component.initialize(context);

                        // Get bus counts
                        audio_inputs = component.getBusCount(kAudio as i32, kInput as i32) as u32;
                        audio_outputs = component.getBusCount(kAudio as i32, kOutput as i32) as u32;

                        // MIDI capability from event bus presence.
                        has_midi_input = component.getBusCount(kEvent as i32, kInput as i32) > 0;
                        has_midi_output = component.getBusCount(kEvent as i32, kOutput as i32) > 0;

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
            version,
            category,
            uid,
            audio_inputs,
            audio_outputs,
            has_midi_input,
            has_midi_output,
            has_gui,
        })
    }
}

/// Deep-introspect a VST3 plugin: factory identity, exported classes, and bus layout.
///
/// Heavier than [`get_plugin_info`] (it enumerates every class and bus) but still does
/// not require driving audio. For the parameter list, load the plugin and call
/// [`crate::Plugin::get_parameters`].
pub fn get_detailed_plugin_info(path: &Path) -> Result<DetailedPluginInfo> {
    use vst3::Steinberg::Vst::BusDirections_::*;
    use vst3::Steinberg::Vst::BusInfo as VstBusInfo;
    use vst3::Steinberg::Vst::MediaTypes_::*;
    use vst3::{ComPtr, Interface, Steinberg::Vst::*, Steinberg::*};

    // Reuse the lightweight pass for the basic info.
    let info = get_plugin_info(path)?;

    unsafe {
        let module = crate::internal::module_loader::load_module(path)?;
        let factory_ptr = module.get_factory()?;
        let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or_else(|| {
            crate::Error::PluginLoadFailed("Failed to create factory ComPtr".to_string())
        })?;

        // Factory identity.
        let mut fi: PFactoryInfo = std::mem::zeroed();
        factory.getFactoryInfo(&mut fi);
        let factory_info = FactoryInfo {
            vendor: crate::internal::utils::c_str_to_string(&fi.vendor),
            url: crate::internal::utils::c_str_to_string(&fi.url),
            email: crate::internal::utils::c_str_to_string(&fi.email),
            flags: fi.flags,
        };

        // Exported classes + locate the audio component class id.
        let num_classes = factory.countClasses();
        let mut classes = Vec::new();
        let mut audio_cid: Option<[std::os::raw::c_char; 16]> = None;
        for i in 0..num_classes {
            let mut ci: PClassInfo = std::mem::zeroed();
            if factory.getClassInfo(i, &mut ci) == kResultOk {
                let category = crate::internal::utils::c_str_to_string(&ci.category);
                let class_id = ci
                    .cid
                    .iter()
                    .map(|b| format!("{:02X}", b))
                    .collect::<String>();
                if category.contains("Audio Module Class") && audio_cid.is_none() {
                    audio_cid = Some(ci.cid);
                }
                classes.push(ClassInfo {
                    name: crate::internal::utils::c_str_to_string(&ci.name),
                    category,
                    class_id,
                    cardinality: ci.cardinality,
                    version: String::new(), // not available in PClassInfo
                });
            }
        }

        // Bus layout from the audio component.
        let mut buses = BusLayout::default();
        if let Some(cid) = audio_cid {
            let mut component_ptr: *mut IComponent = ptr::null_mut();
            let result = factory.createInstance(
                cid.as_ptr(),
                IComponent::IID.as_ptr() as *const std::os::raw::c_char,
                &mut component_ptr as *mut _ as *mut _,
            );
            if result == kResultOk && !component_ptr.is_null() {
                if let Some(component) = ComPtr::<IComponent>::from_raw(component_ptr) {
                    // Initialize with a host context (null crashes u-he/Waves plugins).
                    let host_app = crate::internal::com_implementations::create_host_application();
                    let host_ctx = host_app.to_com_ptr::<IHostApplication>();
                    let context = host_ctx
                        .as_ref()
                        .map(|p| p.as_ptr() as *mut FUnknown)
                        .unwrap_or(ptr::null_mut());
                    component.initialize(context);

                    let collect = |media: i32, dir: i32| -> Vec<crate::discovery::BusInfo> {
                        let mut out = Vec::new();
                        let count = component.getBusCount(media, dir);
                        for i in 0..count {
                            let mut bi: VstBusInfo = std::mem::zeroed();
                            if component.getBusInfo(media, dir, i, &mut bi) == kResultOk {
                                out.push(crate::discovery::BusInfo {
                                    name: crate::internal::utils::vst_string_to_string(&bi.name),
                                    bus_type: bi.busType,
                                    flags: bi.flags as i32,
                                    channel_count: bi.channelCount,
                                });
                            }
                        }
                        out
                    };

                    buses.audio_inputs = collect(kAudio as i32, kInput as i32);
                    buses.audio_outputs = collect(kAudio as i32, kOutput as i32);
                    buses.event_inputs = collect(kEvent as i32, kInput as i32);
                    buses.event_outputs = collect(kEvent as i32, kOutput as i32);

                    component.terminate();
                }
            }
        }

        Ok(DetailedPluginInfo {
            info,
            factory: factory_info,
            classes,
            buses,
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

#[cfg(test)]
mod report_tests {
    use super::*;
    use crate::plugin::PluginInfo;

    #[test]
    fn plugin_report_serializes_and_round_trips() {
        let detail = DetailedPluginInfo {
            info: PluginInfo {
                path: std::path::PathBuf::from("/x/Dexed.vst3"),
                name: "Dexed".into(),
                vendor: "Digital Suburban".into(),
                version: "1.0.0".into(),
                category: "Instrument|Synth".into(),
                uid: "ABCD".into(),
                audio_inputs: 0,
                audio_outputs: 1,
                has_midi_input: true,
                has_midi_output: true,
                has_gui: true,
            },
            factory: FactoryInfo {
                vendor: "Digital Suburban".into(),
                ..Default::default()
            },
            classes: vec![ClassInfo {
                name: "Dexed".into(),
                ..Default::default()
            }],
            buses: BusLayout::default(),
        };
        let report = PluginReport::new(detail, Vec::new());
        let json = report.to_json().expect("to_json");
        // The export round-trips and preserves the accurate metadata.
        let back: PluginReport = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(back.detailed.info.name, "Dexed");
        assert_eq!(back.detailed.info.category, "Instrument|Synth");
        assert!(back.detailed.info.has_midi_output);
        assert_eq!(back.detailed.classes.len(), 1);
    }
}
