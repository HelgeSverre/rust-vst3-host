//! VST3 plugin discovery functionality

use crate::{error::Result, plugin::PluginInfo};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::Duration;

/// Default time to wait for the discovery probe to introspect a single plugin before
/// treating it as hung and killing the child process.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Factory capability flags declared by `moduleinfo.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModuleFactoryFlags {
    /// Factory and class strings use Unicode.
    pub unicode: bool,
    /// Class objects may be discarded after use.
    pub classes_discardable: bool,
    /// The factory performs a license check.
    pub license_check: bool,
    /// Component objects must not be discarded.
    pub component_non_discardable: bool,
}

/// Factory metadata declared by a bundle's `moduleinfo.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModuleFactoryInfo {
    /// Vendor / manufacturer name.
    pub vendor: String,
    /// Vendor URL.
    pub url: String,
    /// Vendor contact email.
    pub email: String,
    /// Factory capabilities.
    pub flags: ModuleFactoryFlags,
}

/// One class declared by a bundle's `moduleinfo.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModuleClassInfo {
    /// Canonical, uppercase 32-hex-character class id.
    pub class_id: String,
    /// VST3 class category, such as `Audio Module Class`.
    pub category: String,
    /// Display name.
    pub name: String,
    /// Class vendor.
    pub vendor: String,
    /// Class version.
    pub version: String,
    /// VST3 SDK version used to build the class.
    pub sdk_version: String,
    /// Declared VST3 sub-categories.
    pub sub_categories: Vec<String>,
    /// Raw class flags.
    pub class_flags: i32,
    /// Instantiation cardinality.
    pub cardinality: i32,
    /// UI snapshots declared for this class, with paths resolved inside the bundle.
    pub snapshots: Vec<PluginSnapshot>,
}

/// A pre-rendered VST3 plug-in UI snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PluginSnapshot {
    /// Current canonical audio-processor class id represented by the image.
    pub class_id: String,
    /// Display scale factor (`1.0` for the unscaled snapshot).
    pub scale_factor: f64,
    /// Snapshot PNG path inside the VST3 bundle.
    pub path: PathBuf,
}

/// A `moduleinfo.json` class-id migration from one or more retired ids to a current id.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClassCompatibility {
    /// Current replacement class id.
    pub new_class_id: String,
    /// Retired class ids replaced by [`Self::new_class_id`].
    pub old_class_ids: Vec<String>,
}

/// Validated metadata from a VST3 bundle's `moduleinfo.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModuleInfo {
    /// The moduleinfo file that was read.
    pub source: PathBuf,
    /// Module display name.
    pub name: String,
    /// Module version.
    pub version: String,
    /// Factory identity.
    pub factory: ModuleFactoryInfo,
    /// Classes declared by the module.
    pub classes: Vec<ModuleClassInfo>,
    /// Replacement mappings for retired class ids.
    pub compatibility: Vec<ClassCompatibility>,
}

impl ModuleInfo {
    /// Resolve a current or retired class id to the current class id exported by this module.
    pub fn resolve_class_id(&self, requested_class_id: &str) -> Option<&str> {
        if let Some(class) = self.classes.iter().find(|class| {
            crate::internal::utils::class_uid_matches(&class.class_id, requested_class_id)
        }) {
            return Some(&class.class_id);
        }
        self.compatibility.iter().find_map(|mapping| {
            mapping
                .old_class_ids
                .iter()
                .any(|old| crate::internal::utils::class_uid_matches(old, requested_class_id))
                .then_some(mapping.new_class_id.as_str())
        })
    }

    /// Return the retired class ids replaced by `current_class_id`.
    pub fn replaced_class_ids(&self, current_class_id: &str) -> &[String] {
        self.compatibility
            .iter()
            .find(|mapping| {
                crate::internal::utils::class_uid_matches(&mapping.new_class_id, current_class_id)
            })
            .map_or(&[], |mapping| mapping.old_class_ids.as_slice())
    }
}

/// Read and validate the standard `moduleinfo.json` from a VST3 bundle.
///
/// Current bundles place it in `Contents/Resources`; the SDK 3.7.5
/// `Contents/moduleinfo.json` location is accepted as a fallback. Returns `Ok(None)` when
/// neither file exists. File size, collection counts, string sizes, integer ranges, class ids,
/// and replacement mappings are bounded and validated before metadata is returned.
pub fn read_module_info(path: &Path) -> Result<Option<ModuleInfo>> {
    crate::internal::module_info::read(path)
}

/// Return the class-id replacement mappings advertised by a VST3 module.
///
/// A validated `moduleinfo.json` is authoritative. When it is absent, this loads the factory,
/// locates its optional `Plugin Compatibility Class`, requests exactly
/// `IPluginCompatibility`, and parses its bounded UTF-8 JSON5 stream.
pub fn get_plugin_compatibility(path: &Path) -> Result<Vec<ClassCompatibility>> {
    if let Some(module_info) = read_module_info(path)? {
        return Ok(module_info.compatibility);
    }

    use vst3::{ComPtr, Steinberg::Vst::IHostApplication, Steinberg::*};
    unsafe {
        // Declared first so it outlives the module and factory — see `get_plugin_info` for
        // why `setHostContext` makes this ordering load-bearing.
        let host_app = crate::internal::com_implementations::create_host_application();
        let host_ctx = host_app.to_com_ptr::<IHostApplication>();
        let context = host_ctx
            .as_ref()
            .map(|pointer| pointer.as_ptr() as *mut FUnknown)
            .unwrap_or(ptr::null_mut());

        let module = crate::internal::module_loader::load_module(path)?;
        let factory_ptr = module.get_factory()?;
        let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or_else(|| {
            crate::Error::PluginLoadFailed("Failed to create factory ComPtr".to_string())
        })?;
        if let Some(factory3) = factory.cast::<IPluginFactory3>() {
            let result = factory3.setHostContext(context);
            if result != kResultOk && result != kResultTrue {
                log::warn!(
                    "IPluginFactory3::setHostContext failed during compatibility discovery: \
                     {result:#x}"
                );
            }
        }
        crate::internal::module_info::read_factory_compatibility(&factory)
    }
}

/// Discover standard UI snapshot PNGs for a current audio-processor class id.
///
/// Only files in `Contents/Resources/Snapshots` whose names follow
/// `<CID>_snapshot.png` or `<CID>_snapshot_<scale>x.png` are returned. This reads directory
/// metadata only; it does not open or decode images. Retired compatibility ids are not
/// resolved here—the caller must provide the current canonical class id.
pub fn discover_plugin_snapshots(
    path: &Path,
    current_class_id: &str,
) -> Result<Vec<PluginSnapshot>> {
    crate::internal::module_info::discover_snapshots(path, current_class_id)
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
    /// Validated static bundle metadata and class-id replacement mappings, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_info: Option<ModuleInfo>,
    /// Effective current/retired class-id mappings (moduleinfo, or runtime fallback).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compatibility: Vec<ClassCompatibility>,
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

    // Directories already visited, by canonical path. A symlink pointing at an ancestor makes the
    // recursion below unbounded — `is_dir()` follows symlinks — so a user whose plug-in folder
    // contains one would hang the scan while `plugins` grew forever with textually-distinct
    // duplicates of the same file (which `dedup` can't collapse, since the paths differ).
    let mut visited = std::collections::HashSet::new();
    for path in paths {
        if path.exists() {
            scan_directory(path, &mut plugins, &mut visited)?;
        }
    }

    // Remove duplicates and sort
    plugins.sort();
    plugins.dedup();

    Ok(plugins)
}

/// Recursively scan a directory for VST3 plugins.
///
/// `visited` holds the canonical path of every directory already descended into, so a symlink
/// loop terminates instead of recursing forever.
fn scan_directory(
    dir: &Path,
    plugins: &mut Vec<PathBuf>,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Result<()> {
    // Resolve through symlinks so two routes to the same directory collapse to one entry. A
    // directory we can't canonicalize (permissions, a broken link) is simply not descended into.
    match dir.canonicalize() {
        Ok(real) => {
            if !visited.insert(real) {
                return Ok(());
            }
        }
        Err(_) => return Ok(()),
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // Check if it's a VST3 bundle/file
            if let Some(ext) = path.extension() {
                if ext == "vst3" {
                    plugins.push(path.clone());
                }
            }

            // Recursively scan subdirectories (but not .vst3 bundles)
            if path.is_dir() && path.extension() != Some(std::ffi::OsStr::new("vst3")) {
                scan_directory(&path, plugins, visited)?;
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
        // Declared before the module and factory so it drops *after* them: locals drop in
        // reverse declaration order, and `IPluginFactory3::setHostContext` stores this pointer
        // in a module-global without an addRef (the SDK's `CPluginFactory` keeps it in
        // `gPluginContext`). Releasing the host application before the module unloads would
        // leave that global dangling for the rest of the plugin's teardown.
        let host_app = crate::internal::com_implementations::create_host_application();
        let host_ctx = host_app.to_com_ptr::<IHostApplication>();
        let context = host_ctx
            .as_ref()
            .map(|p| p.as_ptr() as *mut FUnknown)
            .unwrap_or(ptr::null_mut());

        // Load the module using our VST3-compliant module loader
        let module = crate::internal::module_loader::load_module(path)?;

        // Get factory using the proper VST3 loading sequence
        let factory_ptr = module.get_factory()?;

        let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or_else(|| {
            crate::Error::PluginLoadFailed("Failed to create factory ComPtr".to_string())
        })?;
        if let Some(factory3) = factory.cast::<IPluginFactory3>() {
            let result = factory3.setHostContext(context);
            if result != kResultOk && result != kResultTrue {
                log::warn!("IPluginFactory3::setHostContext failed during discovery: {result:#x}");
            }
        }

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
                    if let Some(f3) = factory.cast::<IPluginFactory3>() {
                        let mut info3: PClassInfoW = std::mem::zeroed();
                        if f3.getClassInfoUnicode(i, &mut info3) == kResultOk {
                            let utf16 = |value: &[u16]| {
                                let end =
                                    value.iter().position(|&ch| ch == 0).unwrap_or(value.len());
                                String::from_utf16_lossy(&value[..end])
                            };
                            let unicode_name = utf16(&info3.name);
                            let unicode_version = utf16(&info3.version);
                            if !unicode_name.is_empty() {
                                plugin_name = unicode_name;
                            }
                            if !unicode_version.is_empty() {
                                version = unicode_version;
                            }
                            let unicode_category =
                                crate::internal::utils::c_str_to_string(&info3.subCategories);
                            if !unicode_category.is_empty() {
                                category = unicode_category;
                            }
                        }
                    }

                    uid = crate::internal::utils::format_class_uid(&class_info.cid);

                    // Try to create component to get more info
                    let mut component_ptr: *mut IComponent = ptr::null_mut();
                    let result = factory.createInstance(
                        class_info.cid.as_ptr() as *const std::os::raw::c_char,
                        IComponent::IID.as_ptr() as *const std::os::raw::c_char,
                        &mut component_ptr as *mut _ as *mut _,
                    );

                    if result == kResultOk && !component_ptr.is_null() {
                        let component =
                            ComPtr::<IComponent>::from_raw(component_ptr).ok_or_else(|| {
                                crate::error::Error::Other("Failed to wrap component".to_string())
                            })?;

                        // Initialize with a host context (null crashes u-he/Waves plugins).
                        component.initialize(context);

                        // Get bus counts
                        audio_inputs = component.getBusCount(kAudio as i32, kInput as i32) as u32;
                        audio_outputs = component.getBusCount(kAudio as i32, kOutput as i32) as u32;

                        // MIDI capability from event bus presence.
                        has_midi_input = component.getBusCount(kEvent as i32, kInput as i32) > 0;
                        has_midi_output = component.getBusCount(kEvent as i32, kOutput as i32) > 0;

                        // GUI detection (lightweight). A plugin has an editor when it provides
                        // an edit controller — either the component itself implements
                        // IEditController (single-component) or it names a separate controller
                        // class. The previous check only handled the single-component case, so
                        // it wrongly reported "no GUI" for the common separate-component
                        // plugins. A precise createView probe needs the plugin's full setup
                        // (component handler + activation) that only the load path performs;
                        // controller presence is the reliable fast signal here.
                        has_gui = component.cast::<IEditController>().is_some() || {
                            let mut cid: [std::os::raw::c_char; 16] = [0; 16];
                            component.getControllerClassId(&mut cid) == kResultOk
                        };

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

    // Static metadata is read before loading code so malformed or hostile metadata is rejected
    // by the bounded parser. Runtime bus information still comes from the component.
    let module_info = read_module_info(path)?;

    // Reuse the lightweight pass for the basic info.
    let info = get_plugin_info(path)?;

    unsafe {
        // Declared first so it outlives the module and factory — see `get_plugin_info` for
        // why `setHostContext` makes this ordering load-bearing.
        let host_app = crate::internal::com_implementations::create_host_application();
        let host_ctx = host_app.to_com_ptr::<IHostApplication>();
        let context = host_ctx
            .as_ref()
            .map(|p| p.as_ptr() as *mut FUnknown)
            .unwrap_or(ptr::null_mut());

        let module = crate::internal::module_loader::load_module(path)?;
        let factory_ptr = module.get_factory()?;
        let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or_else(|| {
            crate::Error::PluginLoadFailed("Failed to create factory ComPtr".to_string())
        })?;
        if let Some(factory3) = factory.cast::<IPluginFactory3>() {
            let result = factory3.setHostContext(context);
            if result != kResultOk && result != kResultTrue {
                log::warn!(
                    "IPluginFactory3::setHostContext failed during detailed discovery: \
                     {result:#x}"
                );
            }
        }
        let compatibility = match module_info.as_ref() {
            Some(module_info) => module_info.compatibility.clone(),
            None => crate::internal::module_info::read_factory_compatibility(&factory)?,
        };

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
                let class_id = crate::internal::utils::format_class_uid(&ci.cid);
                if category.contains("Audio Module Class") && audio_cid.is_none() {
                    audio_cid = Some(ci.cid);
                }
                let mut name = crate::internal::utils::c_str_to_string(&ci.name);
                let mut version = String::new();
                if let Some(factory3) = factory.cast::<IPluginFactory3>() {
                    let mut info3: PClassInfoW = std::mem::zeroed();
                    if factory3.getClassInfoUnicode(i, &mut info3) == kResultOk {
                        let utf16 = |value: &[u16]| {
                            let end = value.iter().position(|&ch| ch == 0).unwrap_or(value.len());
                            String::from_utf16_lossy(&value[..end])
                        };
                        let unicode_name = utf16(&info3.name);
                        if !unicode_name.is_empty() {
                            name = unicode_name;
                        }
                        version = utf16(&info3.version);
                    }
                } else if let Some(factory2) = factory.cast::<IPluginFactory2>() {
                    let mut info2: PClassInfo2 = std::mem::zeroed();
                    if factory2.getClassInfo2(i, &mut info2) == kResultOk {
                        version = crate::internal::utils::c_str_to_string(&info2.version);
                    }
                }
                classes.push(ClassInfo {
                    name,
                    category,
                    class_id,
                    cardinality: ci.cardinality,
                    version,
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
            module_info,
            compatibility,
        })
    }
}

// ---------------------------------------------------------------------------
// Crash-resistant ("safe") discovery via a probe subprocess.
//
// `get_plugin_info` / `get_detailed_plugin_info` INSTANTIATE each plugin in-process to
// introspect it. Some installed plugins (licensed plugins that fail their auth check,
// etc.) call `abort()` or trigger a pure-virtual call during instantiation — which kills
// the whole host process. A Rust `catch_unwind` cannot help: an `abort()` terminates the
// process, it does not unwind. The only robust isolation is to do the risky introspection
// in a child process so the crash kills the child, not us.
//
// This path is independent of the run-time isolation IPC (`process_isolation` /
// `vst3-host-helper`): it spawns a dedicated, minimal `vst3-host-probe` binary once per
// plugin, reads one JSON line of `DetailedPluginInfo` from its stdout, and skips any
// plugin whose probe crashed / timed out / exited non-zero. Correctness over speed: a
// process spawn per plugin is slower than the in-process scan, which is the accepted
// trade-off for a crash-proof scan.
// ---------------------------------------------------------------------------

/// Why a single plugin was skipped during a safe scan. Surfaced via
/// [`SafeDiscoveryReport`] so callers can log or display *why* a plugin was omitted.
#[derive(Debug, Clone)]
pub enum SafeDiscoverySkip {
    /// The probe process crashed (e.g. the plugin called `abort()` or made a
    /// pure-virtual call) — exactly the case in-process scanning cannot survive.
    Crashed {
        /// The plugin path that was skipped.
        path: PathBuf,
        /// Human-readable detail (exit status / signal).
        detail: String,
    },
    /// The probe did not finish within the timeout and was killed.
    TimedOut {
        /// The plugin path that was skipped.
        path: PathBuf,
    },
    /// The probe ran but reported a (non-crash) failure introspecting the plugin.
    Failed {
        /// The plugin path that was skipped.
        path: PathBuf,
        /// Error detail from the probe (or this process).
        detail: String,
    },
}

impl SafeDiscoverySkip {
    /// The plugin path that was skipped.
    pub fn path(&self) -> &Path {
        match self {
            SafeDiscoverySkip::Crashed { path, .. }
            | SafeDiscoverySkip::TimedOut { path }
            | SafeDiscoverySkip::Failed { path, .. } => path,
        }
    }
}

/// Result of a crash-resistant scan: the plugins that introspected cleanly, plus a record
/// of every plugin that was skipped and why.
#[derive(Debug, Default)]
pub struct SafeDiscoveryReport {
    /// Plugins that introspected successfully.
    pub plugins: Vec<DetailedPluginInfo>,
    /// Plugins that were skipped (crashed / timed out / failed), with the reason.
    pub skipped: Vec<SafeDiscoverySkip>,
    /// Why the scan could not run at all, if it could not — the probe binary was missing or
    /// unusable, so **no plugin was examined**. An empty report with `error: None` means the
    /// scan ran and found nothing; an empty report with `error: Some(..)` means it never ran,
    /// and a host should say so rather than claim there are no plugins installed.
    pub error: Option<String>,
}

impl SafeDiscoveryReport {
    /// Whether the scan actually ran. `false` means [`Self::error`] explains why not, and
    /// [`Self::plugins`] / [`Self::skipped`] are empty for that reason alone.
    pub fn scan_ran(&self) -> bool {
        self.error.is_none()
    }
}

/// Whether this executable is itself running from a cargo `target/{debug,release}` tree — i.e.
/// it is a `cargo run` / `cargo test` / example binary rather than a deployed application.
///
/// Used to decide whether it is reasonable to go looking for sibling helper binaries in ancestor
/// directories: inside a build tree that is the whole point, and outside one it would mean
/// executing something from a path the host doesn't control.
pub(crate) fn running_from_cargo_target(exe_dir: &Path) -> bool {
    exe_dir.ancestors().any(|dir| {
        matches!(
            dir.file_name().and_then(|n| n.to_str()),
            Some("debug") | Some("release")
        ) && dir
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("target")
    })
}

/// Locate the `vst3-host-probe` binary that does the risky introspection out-of-process.
///
/// Mirrors the heuristic the isolation layer uses to find `vst3-host-helper` (same exe
/// directory → examples parent → cargo `target/{debug,release}`), and honours an explicit
/// override via the `VST3_HOST_PROBE_PATH` environment variable. Kept self-contained here
/// rather than reusing the isolation module's resolver so the two stay decoupled.
fn find_probe_binary() -> std::result::Result<PathBuf, String> {
    let probe_name = format!("vst3-host-probe{}", std::env::consts::EXE_SUFFIX);

    if let Some(p) = std::env::var_os("VST3_HOST_PROBE_PATH").map(PathBuf::from) {
        if p.exists() {
            return Ok(p);
        }
        return Err(format!(
            "VST3_HOST_PROBE_PATH does not exist: {}",
            p.display()
        ));
    }

    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to get current exe: {}", e))?;
    let exe_dir = exe_path.parent().ok_or("Failed to get exe directory")?;

    // Same directory as the current executable.
    let direct = exe_dir.join(&probe_name);
    if direct.exists() {
        return Ok(direct);
    }

    // If we're in an examples/ directory, try the parent (where bins land).
    if exe_dir.file_name() == Some(std::ffi::OsStr::new("examples")) {
        if let Some(parent) = exe_dir.parent() {
            let p = parent.join(&probe_name);
            if p.exists() {
                return Ok(p);
            }
        }
    }

    // Walk up looking for a cargo target/{debug,release} that holds the probe.
    //
    // Only when *we* are running from inside a cargo target directory — i.e. a `cargo run`/`cargo
    // test` binary, which is the case this fallback exists for (test and example binaries live in
    // `target/<profile>/deps` and `…/examples`, so neither check above finds the sibling helper).
    // A shipped application's executable is not under `target/<profile>/`, and for it this walk
    // would be a liability: it reaches into directories an unprivileged process can write, and
    // whatever it finds is executed and then trusted for everything the host believes about a
    // plugin. Deployed builds use `VST3_HOST_PROBE_PATH` or a binary beside the executable.
    if running_from_cargo_target(exe_dir) {
        let mut current = exe_dir;
        while let Some(parent) = current.parent() {
            for profile in ["debug", "release"] {
                let candidate = parent.join("target").join(profile).join(&probe_name);
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
            if parent.join("Cargo.toml").exists() {
                break;
            }
            current = parent;
        }
    }

    Err(format!(
        "Probe executable '{probe_name}' not found near {} or in target/{{debug,release}}. \
         Build it with `cargo build --bin vst3-host-probe`, or set VST3_HOST_PROBE_PATH.",
        exe_dir.display()
    ))
}

/// Outcome of probing a single plugin out-of-process.
enum ProbeOutcome {
    /// Introspection succeeded.
    Ok(Box<DetailedPluginInfo>),
    /// The probe process crashed (killed by a signal / non-graceful exit).
    Crashed(String),
    /// The probe exceeded the timeout and was killed.
    TimedOut,
    /// The probe ran but reported a (non-crash) failure.
    Failed(String),
}

/// Extra time allowed for the probe's already-written output to reach us after the child has
/// exited, when the timeout budget is already spent. Bounded, unlike waiting for pipe EOF.
const PROBE_OUTPUT_GRACE: Duration = Duration::from_millis(250);

/// Run the probe binary against one plugin path with a timeout, returning the parsed
/// outcome. The crash of a misbehaving plugin kills *the probe child*, surfacing here as
/// [`ProbeOutcome::Crashed`] rather than taking down this process.
///
/// Every wait here is bounded. A plugin that spawns a grandchild (a license daemon, say)
/// hands it the inherited stdout pipe, whose write end then stays open after the probe itself
/// exits or is killed — so a read-to-EOF, or a `join()` on the thread performing it, would
/// outlive the timeout by the grandchild's lifetime and defeat the very timeout the safe scan
/// exists for. The reader thread is therefore detached and reports through a channel we only
/// ever wait on with a deadline; it reads a line at a time so the probe's single JSON line
/// arrives without EOF.
fn run_probe(probe: &Path, plugin: &Path, timeout: Duration) -> ProbeOutcome {
    use std::process::{Command, Stdio};

    let mut child = match Command::new(probe)
        .arg(plugin)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ProbeOutcome::Failed(format!("failed to spawn probe: {e}")),
    };

    // Read stdout on a detached thread so we can enforce a wall-clock timeout on the child.
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return ProbeOutcome::Failed("probe produced no stdout pipe".to_string());
        }
    };
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut line = String::new();
        // One JSON object on one line is the probe's entire protocol, so a line read (rather
        // than read-to-end) completes as soon as it is written, whoever else holds the pipe.
        let mut reader = std::io::BufReader::new(stdout);
        let _ = reader.read_line(&mut line);
        // The receiver is gone once `run_probe` has returned; dropping the line is correct.
        let _ = tx.send(line);
    });

    /// Time left before `deadline`, never zero: a bounded grace so output the child already
    /// wrote is not thrown away just because the budget ran out at the same moment.
    fn remaining(deadline: std::time::Instant) -> Duration {
        deadline
            .saturating_duration_since(std::time::Instant::now())
            .max(PROBE_OUTPUT_GRACE)
    }

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Child exited; collect what it printed, still under a deadline.
                let output = rx.recv_timeout(remaining(deadline)).unwrap_or_default();
                if status.success() {
                    let line = output.trim();
                    return match serde_json::from_str::<DetailedPluginInfo>(line) {
                        Ok(info) => ProbeOutcome::Ok(Box::new(info)),
                        Err(e) => ProbeOutcome::Failed(format!(
                            "probe succeeded but its output did not parse: {e}"
                        )),
                    };
                }
                // Non-success exit. A signal-kill (segfault/abort) has no exit code on
                // Unix; treat both signal deaths and explicit non-zero exits as a crash —
                // the point of the safe path is that *neither* is fatal to us.
                return ProbeOutcome::Crashed(format!("probe exited with {status}"));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ProbeOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return ProbeOutcome::Failed(format!("failed to wait on probe: {e}"));
            }
        }
    }
}

/// Crash-resistantly introspect a single plugin out-of-process.
///
/// Spawns the `vst3-host-probe` binary to do the risky instantiation in a child process,
/// so a plugin that `abort()`s or makes a pure-virtual call during init kills the child
/// instead of this process. Returns `Ok(info)` on success; `Err` (with a descriptive
/// message) if the probe crashed, timed out, failed, or could not be located — callers
/// that want a "skip the bad one and keep going" scan should use
/// [`discover_plugins_safe`] instead, which never returns an error for a single bad plugin.
pub fn probe_plugin_info_isolated(path: &Path, timeout: Duration) -> Result<DetailedPluginInfo> {
    let probe = find_probe_binary().map_err(crate::Error::Other)?;
    match run_probe(&probe, path, timeout) {
        ProbeOutcome::Ok(info) => Ok(*info),
        ProbeOutcome::Crashed(detail) => Err(crate::Error::PluginLoadFailed(format!(
            "probe crashed introspecting {}: {detail}",
            path.display()
        ))),
        ProbeOutcome::TimedOut => Err(crate::Error::PluginTimeout),
        ProbeOutcome::Failed(detail) => Err(crate::Error::PluginLoadFailed(detail)),
    }
}

/// Crash-resistantly discover plugins in `paths`: introspect every `.vst3` bundle in a
/// child process and **skip** any plugin whose probe crashes, hangs, or fails — the scan
/// always completes and returns the plugins it could introspect.
///
/// This is the robust answer to "one bad plugin in the folder takes down the scan": an
/// `abort()`/pure-virtual-call during instantiation kills the probe child, not the host.
/// Each skipped plugin is logged (`log::warn!`) and recorded in
/// [`SafeDiscoveryReport::skipped`].
///
/// Trade-off: this spawns one `vst3-host-probe` process per plugin, so it is slower than
/// the in-process [`crate::Vst3Host::discover_plugins`]. Use it for a robust "safe scan"
/// of an untrusted folder; keep the in-process path for speed when you trust the plugins.
///
/// If the probe binary cannot be located the scan cannot run at all: the returned report is
/// empty and carries the reason in [`SafeDiscoveryReport::error`]. Check
/// [`SafeDiscoveryReport::scan_ran`] before reporting "no plugins found" — the two are
/// otherwise indistinguishable.
pub fn discover_plugins_safe(paths: &[PathBuf], timeout: Duration) -> SafeDiscoveryReport {
    let probe = match find_probe_binary() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("Safe discovery unavailable: {e}");
            return SafeDiscoveryReport {
                error: Some(e),
                ..Default::default()
            };
        }
    };

    let plugin_paths = scan_directories(paths).unwrap_or_default();
    let mut report = SafeDiscoveryReport::default();

    for path in plugin_paths {
        match run_probe(&probe, &path, timeout) {
            ProbeOutcome::Ok(info) => report.plugins.push(*info),
            ProbeOutcome::Crashed(detail) => {
                log::warn!(
                    "Skipping plugin that crashed the probe: {} ({detail})",
                    path.display()
                );
                report
                    .skipped
                    .push(SafeDiscoverySkip::Crashed { path, detail });
            }
            ProbeOutcome::TimedOut => {
                log::warn!("Skipping plugin whose probe timed out: {}", path.display());
                report.skipped.push(SafeDiscoverySkip::TimedOut { path });
            }
            ProbeOutcome::Failed(detail) => {
                log::warn!(
                    "Skipping plugin the probe could not introspect: {} ({detail})",
                    path.display()
                );
                report
                    .skipped
                    .push(SafeDiscoverySkip::Failed { path, detail });
            }
        }
    }

    report
}

/// The `Contents` folders of a Windows VST3 bundle holding a binary that a process built for
/// `arch` (a `std::env::consts::ARCH` value) can load, its own architecture's first.
///
/// Which binaries load where is the compatibility table of Steinberg's plug-in format doc: an
/// Arm64 process loads Arm64 and Arm64X (Arm64 and Arm64EC code in one binary); an Arm64EC
/// process Arm64EC, Arm64X and x64, in the SDK's `module_win32.cpp` order; an x64 process x64,
/// and Arm64EC and Arm64X too when it runs emulated on Arm64 Windows. Compiled into tests on
/// every platform, so CI checks the lists wherever it runs.
#[cfg(any(test, target_os = "windows"))]
fn windows_arch_folders(arch: &str, on_arm64_windows: bool) -> &'static [&'static str] {
    match arch {
        "x86" => &["x86-win"],
        "x86_64" if on_arm64_windows => &["x86_64-win", "arm64ec-win", "arm64x-win"],
        "x86_64" => &["x86_64-win"],
        "arm64ec" => &["arm64ec-win", "arm64x-win", "x86_64-win"],
        "aarch64" => &["arm64-win", "arm64x-win"],
        _ => &[],
    }
}

/// The `Contents` folder of a Linux VST3 bundle holding a binary that a process built for
/// `arch` (a `std::env::consts::ARCH` value) can load.
#[cfg(any(test, target_os = "linux"))]
fn linux_arch_folders(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86" => &["i386-linux"],
        "x86_64" => &["x86_64-linux"],
        "aarch64" => &["aarch64-linux"],
        _ => &[],
    }
}

/// The first binary with `extension` in the first of the bundle's `Contents/<folder>` folders
/// that holds one.
#[cfg(any(test, target_os = "windows", target_os = "linux"))]
fn find_arch_binary(bundle_path: &Path, folders: &[&str], extension: &str) -> Option<PathBuf> {
    let contents = bundle_path.join("Contents");
    folders.iter().find_map(|folder| {
        std::fs::read_dir(contents.join(folder))
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension() == Some(std::ffi::OsStr::new(extension)))
    })
}

/// The error for a bundle without a binary this process can load. The bundle exists, so this is
/// a failure to load it rather than a missing plugin.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn no_loadable_binary(bundle_path: &Path, folders: &[&str]) -> crate::Error {
    let searched = if folders.is_empty() {
        "none".to_owned()
    } else {
        folders.join(", ")
    };
    crate::Error::PluginLoadFailed(format!(
        "{} has no binary this {} process can load (Contents folders searched: {searched})",
        bundle_path.display(),
        std::env::consts::ARCH,
    ))
}

/// Platform-specific VST3 binary path resolution.
///
/// A path to a file is returned as given. On macOS the binary comes from `Contents/MacOS`. On
/// Windows and Linux only the `Contents/<arch>` folders holding a binary this process can load
/// are searched, its own architecture's first, so a bundle that ships binaries only for other
/// architectures is an `Error::PluginLoadFailed`.
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
            // Only the per-arch Contents folders this process can load; a bundle may ship
            // binaries for several architectures side by side.
            let folders = windows_arch_folders(
                std::env::consts::ARCH,
                crate::internal::module_loader::windows::runs_on_arm64_windows(),
            );
            return find_arch_binary(bundle_path, folders, "vst3")
                .ok_or_else(|| no_loadable_binary(bundle_path, folders));
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Linux: Similar to Windows
        if bundle_path.is_dir() {
            let folders = linux_arch_folders(std::env::consts::ARCH);
            return find_arch_binary(bundle_path, folders, "so")
                .ok_or_else(|| no_loadable_binary(bundle_path, folders));
        }
    }

    Err(crate::Error::PluginNotFound(format!(
        "Could not find VST3 binary in bundle: {}",
        bundle_path.display()
    )))
}

#[cfg(test)]
mod bundle_binary_tests {
    use super::*;

    /// A bundle in the temp dir with an empty `Test.<extension>` in each of `folders`, removed
    /// on drop.
    struct TempBundle(PathBuf);

    impl TempBundle {
        fn new(folders: &[&str], extension: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let bundle = std::env::temp_dir().join(format!(
                "vst3-host-multi-arch-{}-{}.vst3",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            // A bundle left by a killed run with the same process id would add stray binaries.
            let _ = std::fs::remove_dir_all(&bundle);
            for folder in folders {
                let dir = bundle.join("Contents").join(folder);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(format!("Test.{extension}")), b"").unwrap();
            }
            Self(bundle)
        }
    }

    impl Drop for TempBundle {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The folders each kind of Windows process looks in, in order, written out apart from
    /// `windows_arch_folders` so a wrong entry there can't become its own expectation.
    #[test]
    fn a_windows_process_looks_only_in_folders_it_can_load_its_own_first() {
        // (process architecture, runs on Arm64 Windows, folders looked in)
        let cases: &[(&str, bool, &[&str])] = &[
            ("x86", false, &["x86-win"]),
            ("x86_64", false, &["x86_64-win"]),
            ("x86_64", true, &["x86_64-win", "arm64ec-win", "arm64x-win"]),
            (
                "arm64ec",
                true,
                &["arm64ec-win", "arm64x-win", "x86_64-win"],
            ),
            ("aarch64", true, &["arm64-win", "arm64x-win"]),
        ];
        for &(arch, on_arm64, expected) in cases {
            assert_eq!(
                windows_arch_folders(arch, on_arm64),
                expected,
                "{arch} process, on Arm64 Windows: {on_arm64}"
            );
        }
    }

    /// The folder each kind of Linux process looks in.
    #[test]
    fn a_linux_process_looks_only_in_its_own_folder() {
        for (arch, expected) in [
            ("x86", "i386-linux"),
            ("x86_64", "x86_64-linux"),
            ("aarch64", "aarch64-linux"),
        ] {
            assert_eq!(linux_arch_folders(arch), [expected], "{arch} process");
        }
    }

    /// The first listed folder that holds a binary wins: without the process's own binary the
    /// next folder it can load is taken, and folders it can't load are never looked in.
    #[test]
    fn a_missing_native_binary_falls_back_in_order_to_a_loadable_one() {
        let bundle = TempBundle::new(&["fallback-a", "fallback-b", "unloadable"], "vst3");
        let found_in = |folders: &[&str]| {
            let binary = find_arch_binary(&bundle.0, folders, "vst3")?;
            Some(binary.parent()?.file_name()?.to_str()?.to_owned())
        };
        let a_first = found_in(&["native", "fallback-a", "fallback-b"]);
        assert_eq!(a_first.as_deref(), Some("fallback-a"));
        let b_first = found_in(&["native", "fallback-b", "fallback-a"]);
        assert_eq!(b_first.as_deref(), Some("fallback-b"));
        assert_eq!(found_in(&["native"]), None);
    }

    #[cfg(target_os = "windows")]
    const ALL_FOLDERS: &[&str] = &[
        "arm64-win",
        "arm64ec-win",
        "arm64x-win",
        "x86-win",
        "x86_64-win",
    ];
    #[cfg(target_os = "windows")]
    const EXTENSION: &str = "vst3";

    #[cfg(target_os = "linux")]
    const ALL_FOLDERS: &[&str] = &["aarch64-linux", "i386-linux", "x86_64-linux"];
    #[cfg(target_os = "linux")]
    const EXTENSION: &str = "so";

    /// This process's own folder, and folders holding binaries it can never load, written out
    /// apart from the code under test.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn own_and_unloadable_folders() -> (&'static str, &'static [&'static str]) {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86") => (
                "x86-win",
                &["arm64-win", "arm64ec-win", "arm64x-win", "x86_64-win"],
            ),
            ("windows", "x86_64") => ("x86_64-win", &["arm64-win", "x86-win"]),
            ("windows", "arm64ec") => ("arm64ec-win", &["arm64-win", "x86-win"]),
            ("windows", "aarch64") => ("arm64-win", &["arm64ec-win", "x86-win", "x86_64-win"]),
            ("linux", "x86") => ("i386-linux", &["aarch64-linux", "x86_64-linux"]),
            ("linux", "x86_64") => ("x86_64-linux", &["aarch64-linux", "i386-linux"]),
            ("linux", "aarch64") => ("aarch64-linux", &["i386-linux", "x86_64-linux"]),
            (os, arch) => panic!("no VST3 bundle folder is known for {arch} {os}"),
        }
    }

    /// `get_vst3_binary_path` resolves a bundle that ships every architecture's binary to this
    /// process's own. With the old fixed search order, x64 hosts picked the Arm64 binary.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn a_multi_arch_bundle_resolves_to_this_processs_own_binary() {
        let (own, _) = own_and_unloadable_folders();
        let bundle = TempBundle::new(ALL_FOLDERS, EXTENSION);
        let found = get_vst3_binary_path(&bundle.0).unwrap();
        assert_eq!(
            found.parent().and_then(|folder| folder.file_name()),
            Some(std::ffi::OsStr::new(own)),
            "{}",
            found.display()
        );
    }

    /// Loading a bundle that only ships other architectures' binaries fails as a load failure
    /// naming this process's architecture, not with the OS's error for loading the folder.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn a_bundle_with_only_other_architectures_binaries_fails_to_load_saying_so() {
        let (_, unloadable) = own_and_unloadable_folders();
        let bundle = TempBundle::new(unloadable, EXTENSION);
        let Err(error) = crate::internal::module_loader::load_module(&bundle.0) else {
            panic!("loaded a bundle with only other architectures' binaries");
        };
        assert!(
            matches!(error, crate::Error::PluginLoadFailed(_)),
            "{error}"
        );
        let arch = std::env::consts::ARCH;
        assert!(
            error.to_string().contains(&format!("this {arch} process")),
            "{error}"
        );
    }
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
            module_info: None,
            compatibility: Vec::new(),
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

#[cfg(test)]
mod scan_tests {
    use super::*;

    /// A symlink pointing back at an ancestor makes the recursive scan unbounded, because
    /// `Path::is_dir` follows symlinks. It hung and grew `plugins` forever with textually distinct
    /// duplicates of the same file — which `dedup` cannot collapse, since the paths differ.
    #[cfg(unix)]
    #[test]
    fn scan_terminates_on_a_symlink_cycle_and_does_not_duplicate() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("vst3-scan-cycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mk root");
        std::fs::create_dir_all(root.join("Real.vst3")).expect("mk bundle");
        // Three branches, each looping back to the root: without a visited set this explodes.
        for name in ["a", "b", "c"] {
            let sub = root.join(name);
            std::fs::create_dir_all(&sub).expect("mk sub");
            symlink(&root, sub.join("loop")).expect("symlink");
        }

        let found = scan_directories(std::slice::from_ref(&root)).expect("scan");

        let bundles: Vec<_> = found
            .iter()
            .filter(|p| p.file_name() == Some(std::ffi::OsStr::new("Real.vst3")))
            .collect();
        assert_eq!(
            bundles.len(),
            1,
            "the same bundle was reported {} times through symlink routes: {found:?}",
            bundles.len()
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
