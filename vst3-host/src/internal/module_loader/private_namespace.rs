//! Private namespace VST3 module loading for macOS
//!
//! This module implements VST3 loading with proper symbol isolation to prevent
//! Objective-C class conflicts. It uses dlopen with RTLD_LOCAL to load plugins
//! in a private namespace, preventing symbol conflicts with system frameworks.
//!
//! According to VST3 specification:
//! - bundleEntry/bundleExit functions are REQUIRED on macOS
//! - Must call bundleEntry after loading and before GetPluginFactory
//! - Must call bundleExit before unloading
//! - Plugins with symbol conflicts require isolated loading

use super::{ModuleLoader, VstModule};
use crate::error::{Error, Result};
use core_foundation::{
    base::{CFRetain, CFRelease, CFTypeRef, Boolean},
    bundle::{CFBundleCreate, CFBundleRef},
    string::CFString,
    url::{CFURLCreateFromFileSystemRepresentation, CFURLRef},
};
use std::{
    ffi::{CString, c_void},
    path::Path,
    ptr,
};
use vst3::Steinberg::IPluginFactory;

/// Function signature for bundleEntry
type BundleEntryFunc = unsafe extern "C" fn(bundle: CFBundleRef) -> Boolean;

/// Function signature for bundleExit  
type BundleExitFunc = unsafe extern "C" fn() -> Boolean;

/// Function signature for GetPluginFactory
type GetPluginFactoryFunc = unsafe extern "C" fn() -> *mut IPluginFactory;

/// Private namespace VST3 module implementation using dlopen with RTLD_LOCAL
pub struct PrivateNamespaceModule {
    /// dlopen handle with private namespace
    dl_handle: *mut c_void,
    /// CFBundle reference (for VST3 compliance)
    bundle: CFBundleRef,
    /// Path to the module
    path: std::path::PathBuf,
    /// bundleExit function pointer (for cleanup)
    bundle_exit: Option<BundleExitFunc>,
    /// GetPluginFactory function pointer
    get_factory_fn: Option<GetPluginFactoryFunc>,
}

impl PrivateNamespaceModule {
    /// Load a VST3 bundle using private namespace isolation
    fn load_internal(path: &Path) -> Result<Self> {
        unsafe {
            log::info!("=== PRIVATE NAMESPACE VST3 MODULE LOADING START ===");
            log::info!("Loading VST3 bundle with symbol isolation: {}", path.display());

            // Handle both bundle paths and direct binary paths
            let bundle_path = if path.extension().and_then(|s| s.to_str()) == Some("vst3") {
                path.to_path_buf()
            } else {
                // Find the .vst3 bundle in parent directories
                let mut current = path;
                loop {
                    if let Some(ext) = current.extension() {
                        if ext == "vst3" {
                            break current.to_path_buf();
                        }
                    }
                    match current.parent() {
                        Some(parent) => current = parent,
                        None => return Err(Error::PluginLoadFailed(
                            "Could not find .vst3 bundle in path hierarchy".to_string()
                        ))
                    }
                }
            };

            log::debug!("Using bundle path: {}", bundle_path.display());

            // Step 1: Create CFBundle reference for VST3 compliance
            log::debug!("Step 1: Creating CFBundle reference...");
            let bundle = Self::create_cfbundle(&bundle_path)?;
            log::debug!("✅ CFBundle created successfully");

            // Step 2: Find the actual executable within the bundle
            log::debug!("Step 2: Finding executable binary...");
            let executable_path = Self::find_bundle_executable(&bundle_path)?;
            log::debug!("✅ Found executable: {}", executable_path.display());

            // Step 3: Load with dlopen using RTLD_LOCAL for private namespace
            log::debug!("Step 3: Loading with dlopen RTLD_LOCAL...");
            let executable_cstring = CString::new(executable_path.to_string_lossy().as_bytes())
                .map_err(|e| Error::PluginLoadFailed(format!("Invalid executable path: {}", e)))?;
            
            // RTLD_LOCAL = 0x4, RTLD_LAZY = 0x1
            const RTLD_LOCAL: i32 = 0x4;
            const RTLD_LAZY: i32 = 0x1;
            let dl_handle = libc::dlopen(executable_cstring.as_ptr(), RTLD_LOCAL | RTLD_LAZY);
            
            if dl_handle.is_null() {
                CFRelease(bundle as CFTypeRef);
                let error_msg = std::ffi::CStr::from_ptr(libc::dlerror())
                    .to_string_lossy()
                    .to_string();
                return Err(Error::PluginLoadFailed(format!("dlopen failed: {}", error_msg)));
            }
            log::debug!("✅ dlopen successful with private namespace");

            // Step 4: Get bundleEntry function (REQUIRED)
            log::debug!("Step 4: Getting bundleEntry function...");
            let bundle_entry_name = CString::new("bundleEntry").unwrap();
            let bundle_entry_ptr = libc::dlsym(dl_handle, bundle_entry_name.as_ptr());
            
            if bundle_entry_ptr.is_null() {
                libc::dlclose(dl_handle);
                CFRelease(bundle as CFTypeRef);
                return Err(Error::PluginLoadFailed(
                    "Bundle does not export required 'bundleEntry' function".to_string(),
                ));
            }
            
            let bundle_entry: BundleEntryFunc = std::mem::transmute(bundle_entry_ptr);
            log::debug!("✅ bundleEntry function found");

            // Step 5: Get bundleExit function (REQUIRED)
            log::debug!("Step 5: Getting bundleExit function...");
            let bundle_exit_name = CString::new("bundleExit").unwrap();
            let bundle_exit_ptr = libc::dlsym(dl_handle, bundle_exit_name.as_ptr());
            
            let bundle_exit: Option<BundleExitFunc> = if bundle_exit_ptr.is_null() {
                log::warn!("⚠️ bundleExit function not found (unusual but proceeding)");
                None
            } else {
                log::debug!("✅ bundleExit function found");
                Some(std::mem::transmute(bundle_exit_ptr))
            };

            // Step 6: Call bundleEntry (MUST be called before GetPluginFactory)
            log::debug!("Step 6: Calling bundleEntry...");
            let entry_result = bundle_entry(bundle);
            if entry_result == 0 {
                libc::dlclose(dl_handle);
                CFRelease(bundle as CFTypeRef);
                return Err(Error::PluginLoadFailed(
                    "bundleEntry function returned false".to_string(),
                ));
            }
            log::debug!("✅ bundleEntry called successfully");

            // Step 7: Get GetPluginFactory function (REQUIRED)
            log::debug!("Step 7: Getting GetPluginFactory function...");
            let factory_name = CString::new("GetPluginFactory").unwrap();
            let factory_ptr = libc::dlsym(dl_handle, factory_name.as_ptr());
            
            let get_factory_fn: Option<GetPluginFactoryFunc> = if factory_ptr.is_null() {
                // Cleanup on failure
                if let Some(exit_fn) = bundle_exit {
                    let _ = exit_fn();
                }
                libc::dlclose(dl_handle);
                CFRelease(bundle as CFTypeRef);
                return Err(Error::PluginLoadFailed(
                    "Failed to find GetPluginFactory function".to_string(),
                ));
            } else {
                log::debug!("✅ GetPluginFactory function found");
                Some(std::mem::transmute(factory_ptr))
            };

            log::info!("=== PRIVATE NAMESPACE VST3 MODULE LOADING COMPLETE ===");
            log::info!("Bundle loaded with symbol isolation: {}", bundle_path.display());

            Ok(PrivateNamespaceModule {
                dl_handle,
                bundle,
                path: bundle_path,
                bundle_exit,
                get_factory_fn,
            })
        }
    }

    /// Create a CFBundle reference from the bundle path
    unsafe fn create_cfbundle(bundle_path: &Path) -> Result<CFBundleRef> {
        let path_cstring = CString::new(bundle_path.to_string_lossy().as_bytes())
            .map_err(|e| Error::PluginLoadFailed(format!("Invalid bundle path: {}", e)))?;

        let url = CFURLCreateFromFileSystemRepresentation(
            ptr::null_mut(),
            path_cstring.as_ptr() as *const u8,
            path_cstring.as_bytes().len() as isize,
            1, // isDirectory - VST3 bundles are directories
        );

        if url.is_null() {
            return Err(Error::PluginLoadFailed(
                "Failed to create CFURL from bundle path".to_string(),
            ));
        }

        let bundle = CFBundleCreate(ptr::null_mut(), url);
        CFRelease(url as CFTypeRef);

        if bundle.is_null() {
            return Err(Error::PluginLoadFailed(
                "Failed to create CFBundle".to_string(),
            ));
        }

        Ok(bundle)
    }

    /// Find the executable binary within the VST3 bundle
    fn find_bundle_executable(bundle_path: &Path) -> Result<std::path::PathBuf> {
        // Standard macOS bundle structure: Contents/MacOS/
        let macos_dir = bundle_path.join("Contents").join("MacOS");
        
        if !macos_dir.exists() {
            return Err(Error::PluginLoadFailed(
                "VST3 bundle missing Contents/MacOS directory".to_string(),
            ));
        }

        // Find the first executable file
        let entries = std::fs::read_dir(&macos_dir)
            .map_err(|e| Error::PluginLoadFailed(format!("Cannot read MacOS directory: {}", e)))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    // Skip hidden files and known non-executables
                    if !name.starts_with('.') && !name.ends_with(".plist") {
                        log::debug!("Found potential executable: {}", path.display());
                        return Ok(path);
                    }
                }
            }
        }

        Err(Error::PluginLoadFailed(
            "No executable found in VST3 bundle".to_string(),
        ))
    }
}

// SAFETY: dlopen handles are thread-safe and CFBundleRef is immutable after creation
unsafe impl Send for PrivateNamespaceModule {}

impl VstModule for PrivateNamespaceModule {
    fn get_factory(&self) -> Result<*mut IPluginFactory> {
        if let Some(get_factory_fn) = self.get_factory_fn {
            let factory = unsafe { get_factory_fn() };
            if factory.is_null() {
                Err(Error::PluginLoadFailed(
                    "GetPluginFactory returned null".to_string(),
                ))
            } else {
                Ok(factory)
            }
        } else {
            Err(Error::PluginLoadFailed(
                "GetPluginFactory function not available".to_string(),
            ))
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateNamespaceModule {
    fn drop(&mut self) {
        unsafe {
            log::debug!("=== PRIVATE NAMESPACE VST3 MODULE CLEANUP START ===");

            // Step 1: Call bundleExit if available (REQUIRED)
            if let Some(bundle_exit) = self.bundle_exit {
                log::debug!("Calling bundleExit...");
                let exit_result = bundle_exit();
                if exit_result != 0 {
                    log::debug!("✅ bundleExit called successfully");
                } else {
                    log::warn!("⚠️ bundleExit returned false");
                }
            }

            // Step 2: Close the dlopen handle
            log::debug!("Closing dlopen handle...");
            if libc::dlclose(self.dl_handle) != 0 {
                let error_msg = std::ffi::CStr::from_ptr(libc::dlerror())
                    .to_string_lossy();
                log::warn!("⚠️ dlclose failed: {}", error_msg);
            } else {
                log::debug!("✅ dlopen handle closed successfully");
            }

            // Step 3: Release CFBundle
            log::debug!("Releasing CFBundle...");
            CFRelease(self.bundle as CFTypeRef);
            log::debug!("✅ CFBundle released");

            log::debug!("=== PRIVATE NAMESPACE VST3 MODULE CLEANUP COMPLETE ===");
        }
    }
}

/// Private namespace module loader implementation
pub struct PrivateNamespaceModuleLoader;

impl ModuleLoader for PrivateNamespaceModuleLoader {
    fn load(path: &Path) -> Result<Box<dyn VstModule>> {
        let module = PrivateNamespaceModule::load_internal(path)?;
        Ok(Box::new(module))
    }
}