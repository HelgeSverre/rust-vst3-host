//! macOS-specific VST3 module loading using CoreFoundation CFBundle APIs
//!
//! According to VST3 specification:
//! - bundleEntry/bundleExit functions are REQUIRED on macOS
//! - Must call bundleEntry after CFBundleLoadExecutable and before GetPluginFactory
//! - Must call bundleExit before unloading or on program termination
//! - Host must reject plugins not exporting these functions

use super::{ModuleLoader, VstModule};
use crate::error::{Error, Result};
use core_foundation::{
    base::{Boolean, CFRelease, CFRetain, CFTypeRef, TCFType},
    bundle::{
        CFBundleCreate, CFBundleGetFunctionPointerForName, CFBundleLoadExecutable, CFBundleRef,
        CFBundleUnloadExecutable,
    },
    string::CFString,
    url::CFURLCreateFromFileSystemRepresentation,
};
use std::{ffi::CString, path::Path, ptr};
use vst3::Steinberg::IPluginFactory;

/// The architecture this host binary was built for (what the plugin must also provide).
#[cfg(target_arch = "aarch64")]
const HOST_ARCH: &str = "arm64";
#[cfg(target_arch = "x86_64")]
const HOST_ARCH: &str = "x86_64";
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
const HOST_ARCH: &str = "unknown";

/// Best-effort: read the bundle's Mach-O executable and return the CPU architectures it
/// contains ("arm64"/"x86_64"). Empty if it can't be read/parsed. Used only to produce a
/// clearer error when `CFBundleLoadExecutable` fails (commonly an arch mismatch).
fn bundle_executable_archs(bundle_path: &Path) -> Vec<&'static str> {
    let macos_dir = bundle_path.join("Contents/MacOS");
    let exe = std::fs::read_dir(&macos_dir)
        .ok()
        .and_then(|it| it.flatten().map(|e| e.path()).find(|p| p.is_file()));
    let Some(exe) = exe else {
        return Vec::new();
    };
    let Ok(data) = std::fs::read(&exe) else {
        return Vec::new();
    };
    parse_macho_archs(&data)
}

/// Parse Mach-O / fat-binary headers for CPU types we care about. Only x86_64 and arm64 are
/// mapped (the only relevant macOS targets); other/unknown CPU types are ignored.
fn parse_macho_archs(data: &[u8]) -> Vec<&'static str> {
    fn arch_name(cputype: u32) -> Option<&'static str> {
        match cputype {
            0x0100_0007 => Some("x86_64"),
            0x0100_000c => Some("arm64"),
            _ => None,
        }
    }
    if data.len() < 8 {
        return Vec::new();
    }
    let mut archs = Vec::new();
    let magic = [data[0], data[1], data[2], data[3]];
    if magic == [0xca, 0xfe, 0xba, 0xbe] {
        // Fat binary, big-endian header: nfat_arch then fat_arch[] (cputype is first u32).
        let nfat = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        for i in 0..nfat {
            let off = 8 + i * 20;
            if off + 4 > data.len() {
                break;
            }
            let cputype =
                u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
            if let Some(a) = arch_name(cputype) {
                if !archs.contains(&a) {
                    archs.push(a);
                }
            }
        }
    } else if magic == [0xcf, 0xfa, 0xed, 0xfe] {
        // Thin 64-bit Mach-O, little-endian: cputype is the u32 right after the magic.
        let cputype = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        if let Some(a) = arch_name(cputype) {
            archs.push(a);
        }
    }
    archs
}

/// Function signature for bundleEntry
type BundleEntryFunc = unsafe extern "C" fn(bundle: CFBundleRef) -> Boolean;

/// Function signature for bundleExit  
type BundleExitFunc = unsafe extern "C" fn() -> Boolean;

/// Function signature for GetPluginFactory
type GetPluginFactoryFunc = unsafe extern "C" fn() -> *mut IPluginFactory;

/// macOS VST3 module implementation using CFBundle APIs
pub struct MacOSModule {
    /// CoreFoundation bundle reference
    bundle: CFBundleRef,
    /// Path to the module (diagnostics / config record)
    #[allow(dead_code)]
    path: std::path::PathBuf,
    /// bundleExit function pointer (for cleanup)
    bundle_exit: Option<BundleExitFunc>,
    /// GetPluginFactory function pointer
    get_factory_fn: Option<GetPluginFactoryFunc>,
}

impl MacOSModule {
    /// Load a VST3 bundle using the correct macOS sequence
    fn load_internal(path: &Path) -> Result<Self> {
        unsafe {
            log::info!("=== macOS VST3 MODULE LOADING START ===");
            log::info!("Loading VST3 bundle: {}", path.display());

            // For VST3 bundles, we need to load the bundle itself, not the binary
            // The CFBundle APIs will automatically find and load the correct binary
            let bundle_path = if path.extension().and_then(|s| s.to_str()) == Some("vst3") {
                // This is already a .vst3 bundle, use it directly
                path.to_path_buf()
            } else {
                // This might be a path to the binary inside the bundle
                // Try to find the .vst3 bundle in the parent directories
                let mut current = path;
                loop {
                    if let Some(ext) = current.extension() {
                        if ext == "vst3" {
                            break current.to_path_buf();
                        }
                    }
                    match current.parent() {
                        Some(parent) => current = parent,
                        None => {
                            return Err(Error::PluginLoadFailed(
                                "Could not find .vst3 bundle in path hierarchy".to_string(),
                            ))
                        }
                    }
                }
            };

            log::debug!("Using bundle path: {}", bundle_path.display());

            // Note: plugins with ObjC symbol conflicts (e.g. Waves/WaveShell) are
            // routed to the private-namespace loader (see module_loader/mod.rs); the
            // standard path below needs no conflict resolution.

            // Step 1: Create CFBundle from bundle path
            log::debug!("Step 1: Creating CFBundle from bundle path...");
            let path_cstring = CString::new(bundle_path.to_string_lossy().as_bytes())
                .map_err(|e| Error::PluginLoadFailed(format!("Invalid path string: {}", e)))?;

            let url = CFURLCreateFromFileSystemRepresentation(
                ptr::null_mut(),
                path_cstring.as_ptr() as *const u8,
                path_cstring.as_bytes().len() as isize,
                1, // isDirectory - VST3 bundles are directories (1 = true)
            );

            if url.is_null() {
                return Err(Error::PluginLoadFailed(
                    "Failed to create URL from path".to_string(),
                ));
            }

            let bundle = CFBundleCreate(ptr::null_mut(), url);
            CFRelease(url as CFTypeRef);

            if bundle.is_null() {
                return Err(Error::PluginLoadFailed(
                    "Failed to create CFBundle".to_string(),
                ));
            }

            log::debug!("✅ CFBundle created successfully");

            // Step 2: Load the bundle executable
            log::debug!("Step 2: Loading bundle executable...");
            let load_result = CFBundleLoadExecutable(bundle);
            if load_result == 0 {
                CFRelease(bundle as CFTypeRef);
                // The most common cause on Apple Silicon is an architecture mismatch
                // (e.g. an x86_64-only plugin in an arm64 host). Diagnose it so the error
                // is actionable rather than just "failed to load".
                let detail = match bundle_executable_archs(&bundle_path) {
                    archs if !archs.is_empty() && !archs.contains(&HOST_ARCH) => format!(
                        "Failed to load bundle executable: plugin is {}-only but this host \
                         is {HOST_ARCH}. Run the host under Rosetta or use a universal/{HOST_ARCH} \
                         build of the plugin.",
                        archs.join("+")
                    ),
                    _ => "Failed to load bundle executable".to_string(),
                };
                return Err(Error::PluginLoadFailed(detail));
            }

            log::debug!("✅ Bundle executable loaded");

            // Step 3: Get bundleEntry function pointer (REQUIRED)
            log::debug!("Step 3: Getting bundleEntry function...");
            let bundle_entry_name = CFString::new("bundleEntry");
            let bundle_entry_ptr =
                CFBundleGetFunctionPointerForName(bundle, bundle_entry_name.as_concrete_TypeRef());

            if bundle_entry_ptr.is_null() {
                CFBundleUnloadExecutable(bundle);
                CFRelease(bundle as CFTypeRef);
                return Err(Error::PluginLoadFailed(
                    "Bundle does not export the required 'bundleEntry' function".to_string(),
                ));
            }

            let bundle_entry: BundleEntryFunc = std::mem::transmute(bundle_entry_ptr);
            log::debug!("✅ bundleEntry function found");

            // Step 4: Get bundleExit function pointer (REQUIRED)
            log::debug!("Step 4: Getting bundleExit function...");
            let bundle_exit_name = CFString::new("bundleExit");
            let bundle_exit_ptr =
                CFBundleGetFunctionPointerForName(bundle, bundle_exit_name.as_concrete_TypeRef());

            if bundle_exit_ptr.is_null() {
                CFBundleUnloadExecutable(bundle);
                CFRelease(bundle as CFTypeRef);
                return Err(Error::PluginLoadFailed(
                    "Bundle does not export the required 'bundleExit' function".to_string(),
                ));
            }

            let bundle_exit: BundleExitFunc = std::mem::transmute(bundle_exit_ptr);
            log::debug!("✅ bundleExit function found");

            // Step 5: Call bundleEntry (MUST be called before GetPluginFactory)
            log::debug!("Step 5: Calling bundleEntry...");
            CFRetain(bundle as CFTypeRef); // Retain for bundleEntry
            let entry_result = bundle_entry(bundle);
            if entry_result == 0 {
                CFRelease(bundle as CFTypeRef); // Release our retain
                CFBundleUnloadExecutable(bundle);
                CFRelease(bundle as CFTypeRef);
                return Err(Error::PluginLoadFailed(
                    "bundleEntry function returned false".to_string(),
                ));
            }

            log::debug!("✅ bundleEntry called successfully");

            // Step 6: Get GetPluginFactory function pointer
            log::debug!("Step 6: Getting GetPluginFactory function...");
            let factory_name = CFString::new("GetPluginFactory");
            let factory_ptr =
                CFBundleGetFunctionPointerForName(bundle, factory_name.as_concrete_TypeRef());

            if factory_ptr.is_null() {
                // Cleanup on failure
                bundle_exit();
                CFRelease(bundle as CFTypeRef); // Release bundleEntry's retain
                CFBundleUnloadExecutable(bundle);
                CFRelease(bundle as CFTypeRef);
                return Err(Error::PluginLoadFailed(
                    "Bundle does not export 'GetPluginFactory' function".to_string(),
                ));
            }

            let get_factory_fn: GetPluginFactoryFunc = std::mem::transmute(factory_ptr);
            log::debug!("✅ GetPluginFactory function found");

            log::info!("=== macOS VST3 MODULE LOADING COMPLETE ===");
            log::info!("Bundle loaded successfully: {}", path.display());

            Ok(MacOSModule {
                bundle,
                path: bundle_path,
                bundle_exit: Some(bundle_exit),
                get_factory_fn: Some(get_factory_fn),
            })
        }
    }
}

// SAFETY: CFBundleRef is not automatically Send, but VST3 modules are typically loaded
// and used on a single thread (usually the main thread), so this is safe in practice.
// The bundle loading and unloading happens in a controlled manner.
unsafe impl Send for MacOSModule {}

impl VstModule for MacOSModule {
    fn get_factory(&self) -> Result<*mut IPluginFactory> {
        match self.get_factory_fn {
            Some(factory_fn) => {
                let factory = unsafe { factory_fn() };
                if factory.is_null() {
                    Err(Error::PluginLoadFailed(
                        "GetPluginFactory returned null".to_string(),
                    ))
                } else {
                    Ok(factory)
                }
            }
            None => Err(Error::PluginLoadFailed(
                "GetPluginFactory function not available".to_string(),
            )),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MacOSModule {
    fn drop(&mut self) {
        unsafe {
            log::debug!("=== macOS VST3 MODULE CLEANUP START ===");

            // Step 1: Call bundleExit if available
            if let Some(bundle_exit) = self.bundle_exit.take() {
                log::debug!("Calling bundleExit...");
                let exit_result = bundle_exit();
                if exit_result != 0 {
                    log::debug!("✅ bundleExit called successfully");
                } else {
                    log::warn!("⚠️ bundleExit returned false");
                }

                // Release the retain from bundleEntry
                CFRelease(self.bundle as CFTypeRef);
            }

            // Step 2: Unload the bundle executable
            log::debug!("Unloading bundle executable...");
            CFBundleUnloadExecutable(self.bundle);
            log::debug!("✅ Bundle executable unloaded");

            // Step 3: Release the bundle
            log::debug!("Releasing CFBundle...");
            CFRelease(self.bundle as CFTypeRef);
            log::debug!("✅ CFBundle released");

            log::debug!("=== macOS VST3 MODULE CLEANUP COMPLETE ===");
        }
    }
}

/// macOS module loader implementation
pub struct MacOSModuleLoader;

impl ModuleLoader for MacOSModuleLoader {
    fn load(path: &Path) -> Result<Box<dyn VstModule>> {
        let module = MacOSModule::load_internal(path)?;
        Ok(Box::new(module))
    }
}

#[cfg(test)]
mod arch_tests {
    use super::*;

    #[test]
    fn parses_thin_x86_64() {
        // MH_MAGIC_64 on disk (LE): CF FA ED FE, then cputype x86_64 (0x01000007) LE.
        let mut data = vec![0xcf, 0xfa, 0xed, 0xfe, 0x07, 0x00, 0x00, 0x01];
        data.resize(64, 0);
        assert_eq!(parse_macho_archs(&data), vec!["x86_64"]);
    }

    #[test]
    fn parses_fat_universal() {
        // FAT_MAGIC (BE): CA FE BA BE, nfat=2, two fat_arch entries (cputype is first u32 BE).
        let mut data = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2];
        data.extend_from_slice(&0x0100_0007u32.to_be_bytes()); // x86_64
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&0x0100_000cu32.to_be_bytes()); // arm64
        data.extend_from_slice(&[0u8; 16]);
        assert_eq!(parse_macho_archs(&data), vec!["x86_64", "arm64"]);
    }

    #[test]
    fn ignores_garbage() {
        assert!(parse_macho_archs(&[0, 1, 2]).is_empty());
    }
}
