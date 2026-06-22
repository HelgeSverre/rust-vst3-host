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
            // Blacklist plugins known to cause issues.
            return name_lower.contains("ozone");
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
                        log::debug!("Skipping blacklisted plugin: {}", path.display());
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
                        let component =
                            ComPtr::<IComponent>::from_raw(component_ptr).ok_or_else(|| {
                                crate::error::Error::Other("Failed to wrap component".to_string())
                            })?;

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
}

/// Locate the `vst3-host-probe` binary that does the risky introspection out-of-process.
///
/// Mirrors the heuristic the isolation layer uses to find `vst3-host-helper` (same exe
/// directory → examples parent → cargo `target/{debug,release}`), and honours an explicit
/// override via the `VST3_HOST_PROBE_PATH` environment variable. Kept self-contained here
/// rather than reusing the isolation module's resolver so the two stay decoupled.
fn find_probe_binary() -> std::result::Result<PathBuf, String> {
    const PROBE_NAME: &str = "vst3-host-probe";

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
    let direct = exe_dir.join(PROBE_NAME);
    if direct.exists() {
        return Ok(direct);
    }

    // If we're in an examples/ directory, try the parent (where bins land).
    if exe_dir.file_name() == Some(std::ffi::OsStr::new("examples")) {
        if let Some(parent) = exe_dir.parent() {
            let p = parent.join(PROBE_NAME);
            if p.exists() {
                return Ok(p);
            }
        }
    }

    // Walk up looking for a cargo target/{debug,release} that holds the probe.
    let mut current = exe_dir;
    while let Some(parent) = current.parent() {
        for profile in ["debug", "release"] {
            let candidate = parent.join("target").join(profile).join(PROBE_NAME);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
        if parent.join("Cargo.toml").exists() {
            break;
        }
        current = parent;
    }

    Err(format!(
        "Probe executable '{PROBE_NAME}' not found near {} or in target/{{debug,release}}. \
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

/// Run the probe binary against one plugin path with a timeout, returning the parsed
/// outcome. The crash of a misbehaving plugin kills *the probe child*, surfacing here as
/// [`ProbeOutcome::Crashed`] rather than taking down this process.
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

    // Read stdout on a thread so we can enforce a wall-clock timeout on the child.
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return ProbeOutcome::Failed("probe produced no stdout pipe".to_string()),
    };
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let mut stdout = stdout;
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Child exited; collect whatever it printed.
                let output = rx.recv().unwrap_or_default();
                let _ = reader.join();
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
                    let _ = reader.join();
                    return ProbeOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = reader.join();
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
pub fn discover_plugins_safe(paths: &[PathBuf], timeout: Duration) -> SafeDiscoveryReport {
    let probe = match find_probe_binary() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("Safe discovery unavailable: {e}");
            return SafeDiscoveryReport::default();
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
            // Look for the .vst3 in the per-arch Contents folder. VST3 uses `arm64-win`
            // (and `arm64ec-win`) for ARM64 — not `aarch64-win`. Native arch first.
            let contents = bundle_path.join("Contents");
            let arm64_path = contents.join("arm64-win");
            let arm64ec_path = contents.join("arm64ec-win");
            let x64_path = contents.join("x86_64-win");
            let x86_path = contents.join("x86-win");

            for contents_path in &[arm64_path, arm64ec_path, x64_path, x86_path] {
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
                contents_path.join("aarch64-linux"),
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
