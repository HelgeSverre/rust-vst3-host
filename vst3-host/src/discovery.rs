//! VST3 plugin discovery functionality

use crate::{error::Result, plugin::PluginInfo};
use std::path::{Path, PathBuf};

/// Scan standard VST3 directories for plugins
pub fn scan_standard_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(format!("{}/Library/Audio/Plug-Ins/VST3", home)));
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

/// Recursively scan a directory for VST3 plugins
fn scan_directory(dir: &Path, plugins: &mut Vec<PathBuf>) -> Result<()> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            
            // Check if it's a VST3 bundle/file
            if let Some(ext) = path.extension() {
                if ext == "vst3" {
                    plugins.push(path);
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
    // This will be implemented to actually load and query the plugin
    // For now, return dummy info
    Ok(PluginInfo {
        path: path.to_path_buf(),
        name: path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string(),
        vendor: "Unknown".to_string(),
        version: "1.0.0".to_string(),
        category: "Unknown".to_string(),
        uid: "unknown".to_string(),
        audio_inputs: 0,
        audio_outputs: 0,
        has_midi_input: false,
        has_midi_output: false,
        has_gui: false,
    })
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