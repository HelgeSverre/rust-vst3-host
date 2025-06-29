//! VST3 plugin scanner example
//!
//! This example demonstrates plugin discovery with and without process isolation.
//! Pass --isolated to enable process isolation for safer scanning.

use vst3_host::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let use_isolation = args.iter().any(|arg| arg == "--isolated");
    let show_help = args.iter().any(|arg| arg == "--help" || arg == "-h");

    if show_help {
        usage();
        return Ok(());
    }

    println!("VST3 Plugin Scanner");
    println!("==================");
    println!(
        "Process isolation: {}\n",
        if use_isolation { "ENABLED" } else { "DISABLED" }
    );

    // Create a host for scanning
    let mut host = Vst3Host::builder()
        .with_process_isolation(use_isolation)
        .scan_default_paths() // Scan standard system paths
        .build()?;

    // Add any custom paths from command line arguments
    let custom_paths: Vec<&String> = args
        .iter()
        .skip(1)
        .filter(|arg| !arg.starts_with("--"))
        .collect();

    if !custom_paths.is_empty() {
        println!("Adding custom scan paths:");
        for path in &custom_paths {
            println!("  - {}", path);
            host.add_scan_path(path)?;
        }
        println!();
    }

    // Discover plugins with progress reporting
    println!("Scanning for VST3 plugins...\n");

    let plugins = host.discover_plugins_with_callback(|progress| match progress {
        DiscoveryProgress::Started { total_plugins } => {
            println!("Scanning {} plugin locations...", total_plugins);
        }
        DiscoveryProgress::Found {
            plugin,
            current,
            total,
        } => {
            println!(
                "[{}/{}] Found: {} by {}",
                current, total, plugin.name, plugin.vendor
            );
        }
        DiscoveryProgress::Error { path, error } => {
            println!("Error scanning {}: {}", path, error);
        }
        DiscoveryProgress::Completed { total_found } => {
            println!("\nScan complete! Found {} plugins.", total_found);
        }
    })?;

    // Display results
    println!("\n{} plugins found:\n", plugins.len());

    // Group by category
    let mut by_category: std::collections::HashMap<String, Vec<&PluginInfo>> =
        std::collections::HashMap::new();

    for plugin in &plugins {
        by_category
            .entry(plugin.category.clone())
            .or_default()
            .push(plugin);
    }

    // Display by category
    let mut categories: Vec<_> = by_category.keys().collect();
    categories.sort();

    for category in categories {
        let plugins_in_category = &by_category[category];
        println!("{} ({} plugins)", category, plugins_in_category.len());
        println!("{}", "-".repeat(category.len() + 12));

        for plugin in plugins_in_category {
            println!(
                "  • {} v{} by {}",
                plugin.name, plugin.version, plugin.vendor
            );

            // Show capabilities
            let mut caps = Vec::new();

            if plugin.audio_inputs > 0 {
                caps.push(format!("{}in", plugin.audio_inputs));
            }
            if plugin.audio_outputs > 0 {
                caps.push(format!("{}out", plugin.audio_outputs));
            }
            if plugin.has_midi_input {
                caps.push("MIDI".to_string());
            }
            if plugin.has_gui {
                caps.push("GUI".to_string());
            }

            if !caps.is_empty() {
                println!("    [{}]", caps.join(", "));
            }

            println!("    Path: {}", plugin.path.display());
            println!();
        }
        println!();
    }

    // Summary statistics
    println!("Summary");
    println!("-------");
    println!("Total plugins: {}", plugins.len());
    println!("Categories: {}", by_category.len());

    let instruments = plugins
        .iter()
        .filter(|p| p.category.contains("Instrument"))
        .count();
    let effects = plugins
        .iter()
        .filter(|p| p.category.contains("Fx") || p.category.contains("Effect"))
        .count();
    let midi_plugins = plugins.iter().filter(|p| p.has_midi_input).count();
    let gui_plugins = plugins.iter().filter(|p| p.has_gui).count();

    println!("Instruments: {}", instruments);
    println!("Effects: {}", effects);
    println!("MIDI-capable: {}", midi_plugins);
    println!("With GUI: {}", gui_plugins);

    Ok(())
}

fn usage() {
    eprintln!("Usage: plugin_scanner [OPTIONS] [additional_paths...]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --isolated    Enable process isolation for safer scanning");
    eprintln!("  --help, -h    Show this help message");
    eprintln!();
    eprintln!("Scans standard VST3 locations and any additional paths provided.");
    eprintln!();
    eprintln!("Standard locations:");
    eprintln!("  macOS: /Library/Audio/Plug-Ins/VST3, ~/Library/Audio/Plug-Ins/VST3");
    eprintln!("  Windows: C:\\Program Files\\Common Files\\VST3");
    eprintln!("  Linux: /usr/lib/vst3, /usr/local/lib/vst3, ~/.vst3");
    eprintln!();
    eprintln!("Example:");
    eprintln!("  plugin_scanner --isolated /custom/vst3/path");
}
