//! Platform-specific VST3 module loading implementations

use crate::error::{Error, Result};
use std::path::Path;
use vst3::Steinberg::IPluginFactory;

/// Trait representing a loaded VST3 module
pub trait VstModule: Send {
    /// Get the plugin factory from the module
    fn get_factory(&self) -> Result<*mut IPluginFactory>;
    
    /// Get the path to the module
    fn path(&self) -> &Path;
}

/// Trait for platform-specific module loaders
pub trait ModuleLoader {
    /// Load a VST3 module from the given path
    fn load(path: &Path) -> Result<Box<dyn VstModule>>;
}

/// Detect if a plugin has Objective-C class conflicts requiring process isolation
pub fn has_objc_conflicts(path: &Path) -> bool {
    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
        let filename_lower = filename.to_lowercase();
        
        // Known problematic plugins that have Objective-C class conflicts
        // These plugins MUST use process isolation to prevent SIGSEGV crashes
        if filename_lower.contains("waveshell") ||
           filename_lower.contains("waves")
        {
            log::info!("Detected plugin with Objective-C conflicts requiring process isolation: {}", filename);
            return true;
        }
    }
    
    false
}

/// Detect if a plugin requires private namespace loading due to C symbol conflicts
fn requires_private_namespace(path: &Path) -> bool {
    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
        let filename_lower = filename.to_lowercase();
        
        // Plugins with C symbol conflicts (not Objective-C)
        if filename_lower.contains("ozone") ||
           filename_lower.contains("rx") ||      // iZotope RX series
           filename_lower.contains("neutron") || // iZotope Neutron  
           filename_lower.contains("insight")    // iZotope Insight
        {
            log::info!("Detected plugin requiring private namespace for C symbols: {}", filename);
            return true;
        }
    }
    
    false
}

/// Load a VST3 module using the appropriate platform-specific loader
pub fn load_module(path: &Path) -> Result<Box<dyn VstModule>> {
    #[cfg(target_os = "macos")]
    {
        if has_objc_conflicts(path) || requires_private_namespace(path) {
            // Use private namespace loading for both Objective-C and C symbol conflicts
            log::info!("Using private namespace loader for symbol isolation: {}", path.display());
            private_namespace::PrivateNamespaceModuleLoader::load(path)
        } else {
            // Use standard CFBundle loader for plugins without conflicts
            log::debug!("Using standard CFBundle loader: {}", path.display());
            macos::MacOSModuleLoader::load(path)
        }
    }
    
    #[cfg(target_os = "windows")]
    {
        windows::WindowsModuleLoader::load(path)
    }
    
    #[cfg(target_os = "linux")]
    {
        linux::LinuxModuleLoader::load(path)
    }
}

// Platform-specific modules
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub mod private_namespace;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;