pub fn scan_vst3_directories() -> Vec<String> {
    let mut plugins = Vec::new();

    #[cfg(target_os = "macos")]
    {
        let paths = [
            "/Library/Audio/Plug-Ins/VST3",
            &format!(
                "{}/Library/Audio/Plug-Ins/VST3",
                std::env::var("HOME").unwrap_or_default()
            ),
        ];

        for path in &paths {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension() == Some(std::ffi::OsStr::new("vst3")) {
                        plugins.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let paths = [
            r"C:\Program Files\Common Files\VST3",
            r"C:\Program Files (x86)\Common Files\VST3",
        ];

        for path in &paths {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension() == Some(std::ffi::OsStr::new("vst3")) {
                        plugins.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    plugins.sort();
    plugins
}