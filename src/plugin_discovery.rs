
pub fn scan_vst3_directories_with_custom(custom_paths: &[String]) -> Vec<String> {
    let mut plugins = Vec::new();
    let mut all_paths = Vec::new();

    #[cfg(target_os = "macos")]
    {
        all_paths.push("/Library/Audio/Plug-Ins/VST3".to_string());
        all_paths.push(format!(
            "{}/Library/Audio/Plug-Ins/VST3",
            std::env::var("HOME").unwrap_or_default()
        ));
    }

    #[cfg(target_os = "windows")]
    {
        all_paths.push(r"C:\Program Files\Common Files\VST3".to_string());
        all_paths.push(r"C:\Program Files (x86)\Common Files\VST3".to_string());
    }
    
    // Add custom paths
    all_paths.extend(custom_paths.iter().cloned());

    // Scan all paths
    for path in &all_paths {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension() == Some(std::ffi::OsStr::new("vst3")) {
                    plugins.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    // Remove duplicates and sort
    plugins.sort();
    plugins.dedup();
    plugins
}