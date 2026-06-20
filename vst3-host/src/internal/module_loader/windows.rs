//! Windows-specific VST3 module loading
//!
//! According to VST3 specification:
//! - InitDll/ExitDll functions are OPTIONAL on Windows
//! - Should call InitDll after LoadLibrary and before GetPluginFactory (if present)
//! - Should call ExitDll before FreeLibrary or on program termination (if present)
//! - Windows already has DllMain functionality, so these are optional

use super::{ModuleLoader, VstModule};
use crate::error::{Error, Result};
use std::path::Path;
use vst3::Steinberg::IPluginFactory;

#[cfg(target_os = "windows")]
use libloading::{Library, Symbol};

/// Function signature for InitDll
type InitDllFunc = unsafe extern "C" fn() -> bool;

/// Function signature for ExitDll
type ExitDllFunc = unsafe extern "C" fn() -> bool;

/// Function signature for GetPluginFactory
type GetPluginFactoryFunc = unsafe extern "C" fn() -> *mut IPluginFactory;

/// Windows VST3 module implementation
pub struct WindowsModule {
    /// libloading Library handle. Never read directly, but MUST outlive the transmuted
    /// `'static` symbols below, which point into it. Kept to own its lifetime.
    #[allow(dead_code)]
    library: Library,
    /// Path to the module
    path: std::path::PathBuf,
    /// ExitDll function pointer (for cleanup, if present)
    exit_dll: Option<Symbol<'static, ExitDllFunc>>,
    /// GetPluginFactory function pointer
    get_factory_fn: Symbol<'static, GetPluginFactoryFunc>,
}

impl WindowsModule {
    /// Load a VST3 DLL using the correct Windows sequence
    fn load_internal(path: &Path) -> Result<Self> {
        unsafe {
            log::info!("=== Windows VST3 MODULE LOADING START ===");
            log::info!("Loading VST3 DLL: {}", path.display());

            // Step 1: Load the library
            log::debug!("Step 1: Loading DLL...");
            let library = Library::new(path)
                .map_err(|e| Error::PluginLoadFailed(format!("Failed to load DLL: {}", e)))?;
            log::debug!("DLL loaded successfully");

            // Step 2: Try to get InitDll function (OPTIONAL).
            // Note: `library.get` returns `libloading::Result`, not the crate's `Result`
            // alias — let it infer rather than annotating with the 1-arg alias.
            log::debug!("Step 2: Looking for InitDll function...");
            let init_dll_result = library.get::<InitDllFunc>(b"InitDll");
            match init_dll_result {
                Ok(init_dll) => {
                    log::debug!("InitDll function found, calling it...");
                    let init_result = init_dll();
                    if init_result {
                        log::debug!("InitDll called successfully");
                    } else {
                        log::warn!("InitDll returned false");
                        return Err(Error::PluginLoadFailed(
                            "InitDll function returned false".to_string(),
                        ));
                    }
                }
                Err(_) => {
                    log::debug!(
                        "InitDll function not found (this is OK, it's optional on Windows)"
                    );
                }
            }

            // Step 3: Try to get ExitDll function for cleanup (OPTIONAL)
            log::debug!("Step 3: Looking for ExitDll function...");
            let exit_dll = match library.get::<ExitDllFunc>(b"ExitDll") {
                Ok(func) => {
                    log::debug!("ExitDll function found (will be called on cleanup)");
                    // SAFETY: We extend the lifetime to 'static because we're storing this in the struct
                    // and will ensure it's dropped before the library is unloaded
                    Some(std::mem::transmute(func))
                }
                Err(_) => {
                    log::debug!(
                        "ExitDll function not found (this is OK, it's optional on Windows)"
                    );
                    None
                }
            };

            // Step 4: Get GetPluginFactory function (REQUIRED)
            log::debug!("Step 4: Getting GetPluginFactory function...");
            let get_factory_fn = library
                .get::<GetPluginFactoryFunc>(b"GetPluginFactory")
                .map_err(|e| {
                    Error::PluginLoadFailed(format!("Failed to find GetPluginFactory: {}", e))
                })?;

            log::debug!("GetPluginFactory function found");

            // SAFETY: We extend the lifetime to 'static because we're storing this in the struct
            // and will ensure it's dropped before the library is unloaded
            let get_factory_fn: Symbol<'static, GetPluginFactoryFunc> =
                std::mem::transmute(get_factory_fn);

            log::info!("=== Windows VST3 MODULE LOADING COMPLETE ===");
            log::info!("DLL loaded successfully: {}", path.display());

            Ok(WindowsModule {
                library,
                path: path.to_path_buf(),
                exit_dll,
                get_factory_fn,
            })
        }
    }
}

impl VstModule for WindowsModule {
    fn get_factory(&self) -> Result<*mut IPluginFactory> {
        let factory = unsafe { (self.get_factory_fn)() };
        if factory.is_null() {
            Err(Error::PluginLoadFailed(
                "GetPluginFactory returned null".to_string(),
            ))
        } else {
            Ok(factory)
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WindowsModule {
    fn drop(&mut self) {
        unsafe {
            log::debug!("=== Windows VST3 MODULE CLEANUP START ===");

            // Step 1: Call ExitDll if available
            if let Some(exit_dll) = self.exit_dll.take() {
                log::debug!("Calling ExitDll...");
                let exit_result = exit_dll();
                if exit_result {
                    log::debug!("ExitDll called successfully");
                } else {
                    log::warn!("ExitDll returned false");
                }
            }

            // Step 2: Library will be automatically unloaded when dropped
            log::debug!("Unloading DLL...");
            // The Drop implementation of Library handles FreeLibrary
            log::debug!("DLL unloaded");

            log::debug!("=== Windows VST3 MODULE CLEANUP COMPLETE ===");
        }
    }
}

/// Windows module loader implementation
pub struct WindowsModuleLoader;

impl ModuleLoader for WindowsModuleLoader {
    fn load(path: &Path) -> Result<Box<dyn VstModule>> {
        let module = WindowsModule::load_internal(path)?;
        Ok(Box::new(module))
    }
}
