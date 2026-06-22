//! Linux-specific VST3 module loading
//!
//! According to VST3 specification:
//! - ModuleEntry/ModuleExit functions are REQUIRED on Linux
//! - Must call ModuleEntry after dlopen and before GetPluginFactory
//! - Must call ModuleExit before dlclose or on program termination
//! - Host must reject plugins not exporting these functions

use super::{ModuleLoader, VstModule};
use crate::error::{Error, Result};
use std::path::Path;
use vst3::Steinberg::IPluginFactory;

#[cfg(target_os = "linux")]
use libloading::{Library, Symbol};

/// Function signature for ModuleEntry
type ModuleEntryFunc = unsafe extern "C" fn() -> bool;

/// Function signature for ModuleExit
type ModuleExitFunc = unsafe extern "C" fn() -> bool;

/// Function signature for GetPluginFactory
type GetPluginFactoryFunc = unsafe extern "C" fn() -> *mut IPluginFactory;

/// Linux VST3 module implementation
pub struct LinuxModule {
    /// libloading Library handle. Never read directly, but MUST outlive the symbols below:
    /// `module_exit`/`get_factory_fn` are transmuted to `'static` and point into this
    /// library, so dropping it early would dangle them. Kept to own its lifetime.
    #[allow(dead_code)]
    library: Library,
    /// Path to the module
    path: std::path::PathBuf,
    /// ModuleExit function pointer (for cleanup)
    module_exit: Symbol<'static, ModuleExitFunc>,
    /// GetPluginFactory function pointer
    get_factory_fn: Symbol<'static, GetPluginFactoryFunc>,
}

/// Best-effort: read the plugin's ELF header and, if its architecture differs from the host's,
/// return an actionable error sentence. `None` if the file can't be read/parsed (e.g. a `.vst3`
/// bundle directory) or the arch matches — the caller falls back to the generic message.
fn arch_mismatch_detail(path: &Path) -> Option<String> {
    use super::arch;
    let data = std::fs::read(path).ok()?;
    let name = arch::elf_machine_name(arch::detect_elf_machine(&data)?);
    arch::mismatch_detail("shared object", name)
}

impl LinuxModule {
    /// Load a VST3 shared object using the correct Linux sequence
    fn load_internal(path: &Path) -> Result<Self> {
        unsafe {
            log::info!("=== Linux VST3 MODULE LOADING START ===");
            log::info!("Loading VST3 shared object: {}", path.display());

            // Step 1: Load the library
            log::debug!("Step 1: Loading shared object...");
            let library = Library::new(path).map_err(|e| {
                // Diagnose an architecture mismatch from the ELF header when possible.
                arch_mismatch_detail(path)
                    .map(Error::PluginLoadFailed)
                    .unwrap_or_else(|| {
                        Error::PluginLoadFailed(format!("Failed to load shared object: {}", e))
                    })
            })?;
            log::debug!("Shared object loaded successfully");

            // Step 2: Get ModuleEntry function (REQUIRED)
            log::debug!("Step 2: Getting ModuleEntry function...");
            let module_entry = library
                .get::<ModuleEntryFunc>(b"ModuleEntry")
                .map_err(|_| {
                    Error::PluginLoadFailed(
                        "Shared object does not export the required 'ModuleEntry' function"
                            .to_string(),
                    )
                })?;
            log::debug!("ModuleEntry function found");

            // Step 3: Get ModuleExit function (REQUIRED)
            log::debug!("Step 3: Getting ModuleExit function...");
            let module_exit = library.get::<ModuleExitFunc>(b"ModuleExit").map_err(|_| {
                Error::PluginLoadFailed(
                    "Shared object does not export the required 'ModuleExit' function".to_string(),
                )
            })?;
            log::debug!("ModuleExit function found");

            // Step 4: Call ModuleEntry (MUST be called before GetPluginFactory)
            log::debug!("Step 4: Calling ModuleEntry...");
            let entry_result = module_entry();
            if !entry_result {
                return Err(Error::PluginLoadFailed(
                    "ModuleEntry function returned false".to_string(),
                ));
            }
            log::debug!("ModuleEntry called successfully");

            // Step 5: Get GetPluginFactory function (REQUIRED)
            log::debug!("Step 5: Getting GetPluginFactory function...");
            let get_factory_fn = library
                .get::<GetPluginFactoryFunc>(b"GetPluginFactory")
                .map_err(|e| {
                    // Cleanup on failure
                    let _ = module_exit();
                    Error::PluginLoadFailed(format!("Failed to find GetPluginFactory: {}", e))
                })?;

            log::debug!("GetPluginFactory function found");

            // SAFETY: We extend the lifetime to 'static because we're storing these in the struct
            // and will ensure they're dropped before the library is unloaded
            let module_exit: Symbol<'static, ModuleExitFunc> = std::mem::transmute(module_exit);
            let get_factory_fn: Symbol<'static, GetPluginFactoryFunc> =
                std::mem::transmute(get_factory_fn);

            log::info!("=== Linux VST3 MODULE LOADING COMPLETE ===");
            log::info!("Shared object loaded successfully: {}", path.display());

            Ok(LinuxModule {
                library,
                path: path.to_path_buf(),
                module_exit,
                get_factory_fn,
            })
        }
    }
}

impl VstModule for LinuxModule {
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

impl Drop for LinuxModule {
    fn drop(&mut self) {
        unsafe {
            log::debug!("=== Linux VST3 MODULE CLEANUP START ===");

            // Step 1: Call ModuleExit (REQUIRED)
            log::debug!("Calling ModuleExit...");
            let exit_result = (self.module_exit)();
            if exit_result {
                log::debug!("ModuleExit called successfully");
            } else {
                log::warn!("ModuleExit returned false");
            }

            // Step 2: Library will be automatically unloaded when dropped
            log::debug!("Unloading shared object...");
            // The Drop implementation of Library handles dlclose
            log::debug!("Shared object unloaded");

            log::debug!("=== Linux VST3 MODULE CLEANUP COMPLETE ===");
        }
    }
}

/// Linux module loader implementation
pub struct LinuxModuleLoader;

impl ModuleLoader for LinuxModuleLoader {
    fn load(path: &Path) -> Result<Box<dyn VstModule>> {
        let module = LinuxModule::load_internal(path)?;
        Ok(Box::new(module))
    }
}
