//! VST3 plugin scanner example

use vst3_host::prelude::*;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("VST3 Plugin Scanner");
    println!("==================\n");
    
    // Create a host for scanning
    let mut host = Vst3Host::builder()
        .with_process_isolation(true) // Use process isolation for safety
        .build()?;
    
    // Add any custom paths from command line arguments
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        println!("Adding custom scan paths:");
        for path in &args[1..] {
            println!("  - {}", path);
            host.add_scan_path(path)?;
        }
        println!();
    }
    
    // Discover plugins with progress reporting
    println!("Scanning for VST3 plugins...\n");
    
    let plugins = host.discover_plugins_with_progress(|progress| {
        println!("Scanning: {}", progress.current_path.display());
        if !progress.current_plugin.is_empty() {
            println!("  Found: {}", progress.current_plugin);
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
            println!("  • {} v{} by {}", 
                plugin.name, 
                plugin.version, 
                plugin.vendor
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
    
    let instruments = plugins.iter()
        .filter(|p| p.category.contains("Instrument"))
        .count();
    let effects = plugins.iter()
        .filter(|p| p.category.contains("Fx") || p.category.contains("Effect"))
        .count();
    let midi_plugins = plugins.iter()
        .filter(|p| p.has_midi_input)
        .count();
    let gui_plugins = plugins.iter()
        .filter(|p| p.has_gui)
        .count();
    
    println!("Instruments: {}", instruments);
    println!("Effects: {}", effects);
    println!("MIDI-capable: {}", midi_plugins);
    println!("With GUI: {}", gui_plugins);
    
    Ok(())
}

fn usage() {
    eprintln!("Usage: plugin_scanner [additional_paths...]");
    eprintln!();
    eprintln!("Scans standard VST3 locations and any additional paths provided.");
    eprintln!();
    eprintln!("Standard locations:");
    eprintln!("  macOS: /Library/Audio/Plug-Ins/VST3, ~/Library/Audio/Plug-Ins/VST3");
    eprintln!("  Windows: C:\\Program Files\\Common Files\\VST3");
    eprintln!("  Linux: /usr/lib/vst3, /usr/local/lib/vst3, ~/.vst3");
}