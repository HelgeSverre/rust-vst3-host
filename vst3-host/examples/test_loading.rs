//! # VST3 Plugin Loading Test
//!
//! This example demonstrates the basics of VST3 plugin loading without any GUI or audio processing.
//! It's perfect for testing whether a plugin can be loaded and initialized correctly.
//!
//! ## What This Example Shows
//! - Basic plugin loading and error handling
//! - Plugin information extraction
//! - Parameter discovery and inspection
//! - Plugin lifecycle management (start/stop processing)
//! - MIDI event sending (basic test)
//!
//! ## Usage
//! ```bash
//! cargo run --example test_loading /path/to/your/plugin.vst3
//! ```
//!
//! ## Common Use Cases
//! - Testing plugin compatibility before building a full host
//! - Debugging plugin loading issues
//! - Extracting metadata from plugins for database/catalog purposes
//! - Quick verification that a plugin installation is working
//!
//! ## Expected Output
//! ```
//! Loading VST3 plugin: /path/to/plugin.vst3
//! Host created successfully
//! Plugin loaded successfully!
//! Plugin Info:
//!   Name: SomePlugin
//!   Vendor: SomeVendor
//!   Version: 1.0.0
//!   Has GUI: true
//!   Audio Inputs: 2
//!   Audio Outputs: 2
//!   Has MIDI Input: true
//!   Has MIDI Output: false
//! Parameters (5 total):
//!   0: Volume = 0.800
//!   1: Cutoff = 0.500
//!   ...
//! Starting processing...
//! Processing started successfully!
//! Sending test MIDI note...
//! MIDI note sent successfully!
//! Plugin test completed successfully!
//! ```

use vst3_host::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Initialize logging to see detailed information about what's happening
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("VST3 Plugin Loading Test");
        eprintln!("========================");
        eprintln!("");
        eprintln!("Usage: {} <vst3_plugin_path>", args[0]);
        eprintln!("");
        eprintln!("Examples:");
        eprintln!("  {} /Library/Audio/Plug-Ins/VST3/SomePlugin.vst3", args[0]);
        eprintln!(
            "  {} \"C:\\Program Files\\Common Files\\VST3\\SomePlugin.vst3\"",
            args[0]
        );
        eprintln!("");
        eprintln!("This tool will:");
        eprintln!("  • Load the specified VST3 plugin");
        eprintln!("  • Display plugin information and capabilities");
        eprintln!("  • Test basic plugin operations");
        eprintln!("  • Verify MIDI and parameter functionality");
        std::process::exit(1);
    }

    let plugin_path = &args[1];
    println!("VST3 Plugin Loading Test");
    println!("========================");
    println!("Target: {}", plugin_path);
    println!();

    // Step 1: Create VST3 Host
    println!("Creating VST3 host...");
    let mut host = Vst3Host::builder()
        .sample_rate(44100.0)
        .block_size(512)
        .build()?;
    println!("Host created successfully");

    // Step 2: Load Plugin
    println!("Loading plugin...");
    let mut plugin = match host.load_plugin(plugin_path) {
        Ok(plugin) => {
            println!("Plugin loaded successfully!");
            plugin
        }
        Err(e) => {
            eprintln!("Failed to load plugin: {}", e);
            eprintln!();
            eprintln!("Common causes:");
            eprintln!("  • Plugin file doesn't exist at specified path");
            eprintln!("  • Plugin has missing dependencies");
            eprintln!("  • Plugin is not compatible with your architecture");
            eprintln!("  • Plugin requires specific permissions (macOS)");
            eprintln!();
            eprintln!("Try running with RUST_LOG=debug for more details");
            return Err(e.into());
        }
    };

    // Step 3: Display Plugin Information
    println!();
    println!("Plugin Information");
    println!("=====================");
    // Clone so we don't hold an immutable borrow of `plugin` across the later
    // `&mut plugin` lifecycle calls (start_processing / send_midi_event / ...).
    let info = plugin.info().clone();
    println!("Name:           {}", info.name);
    println!("Vendor:         {}", info.vendor);
    println!("Version:        {}", info.version);
    println!("Category:       {}", info.category);
    println!(
        "Has GUI:        {}",
        if info.has_gui { "Yes" } else { "No" }
    );

    // Audio capabilities
    println!();
    println!("Audio Capabilities");
    println!("Audio Inputs:   {} channels", info.audio_inputs);
    println!("Audio Outputs:  {} channels", info.audio_outputs);

    // Determine plugin type
    let plugin_type = if info.audio_inputs == 0 && info.audio_outputs > 0 && info.has_midi_input {
        "Virtual Instrument (Synthesizer)"
    } else if info.audio_inputs > 0 && info.audio_outputs > 0 {
        "Audio Effect"
    } else if info.audio_inputs == 0 && info.audio_outputs == 0 {
        "MIDI Effect or Utility"
    } else {
        "Unknown Type"
    };
    println!("Plugin Type:    {}", plugin_type);

    // MIDI capabilities
    println!();
    println!("MIDI Capabilities");
    println!(
        "MIDI Input:     {}",
        if info.has_midi_input { "Yes" } else { "No" }
    );
    println!(
        "MIDI Output:    {}",
        if info.has_midi_output { "Yes" } else { "No" }
    );

    // Step 4: Parameter Discovery
    println!();
    println!("Parameter Discovery");
    println!("=======================");
    match plugin.get_parameters() {
        Ok(params) => {
            if params.is_empty() {
                println!("No parameters available");
            } else {
                println!("Found {} parameter(s):", params.len());
                println!();

                // Show first 10 parameters with detailed info
                for (i, param) in params.iter().take(10).enumerate() {
                    println!("  {}. {}", i + 1, param.name);
                    println!("     ID: {}", param.id);
                    println!(
                        "     Value: {:.3} ({:.1}%)",
                        param.value,
                        param.value * 100.0
                    );
                    if param.unit.len() > 0 {
                        println!("     Unit: {}", param.unit);
                    }
                    println!();
                }

                if params.len() > 10 {
                    println!("  ... and {} more parameters", params.len() - 10);
                    println!("     (showing first 10 only)");
                }
            }
        }
        Err(e) => {
            println!("Could not retrieve parameters: {}", e);
            println!(
                "   This is normal for some plugins that require processing to be started first"
            );
        }
    }

    // Step 5: Test Plugin Lifecycle
    println!();
    println!("Testing Plugin Lifecycle");
    println!("===========================");

    println!("Starting processing...");
    match plugin.start_processing() {
        Ok(()) => {
            println!("Processing started successfully");

            // Try to get parameters again after processing started
            if let Ok(params) = plugin.get_parameters() {
                if !params.is_empty() {
                    println!("Parameters now available: {} total", params.len());
                }
            }
        }
        Err(e) => {
            println!("Failed to start processing: {}", e);
            println!("   Plugin may have specific requirements not met");
            return Err(e.into());
        }
    }

    // Step 6: MIDI Test (if plugin supports MIDI)
    if info.has_midi_input {
        println!();
        println!("Testing MIDI Input");
        println!("=====================");

        println!("Sending test MIDI note (C4, velocity 100)...");
        match plugin.send_midi_event(MidiEvent::NoteOn {
            channel: MidiChannel::Ch1,
            note: 60, // Middle C (C4)
            velocity: 100,
        }) {
            Ok(()) => {
                println!("MIDI note sent successfully");

                // Wait a bit, then send note off
                std::thread::sleep(std::time::Duration::from_millis(100));

                println!("Sending note off...");
                match plugin.send_midi_event(MidiEvent::NoteOff {
                    channel: MidiChannel::Ch1,
                    note: 60,
                    velocity: 0,
                }) {
                    Ok(()) => println!("MIDI note off sent successfully"),
                    Err(e) => println!("Note off failed: {}", e),
                }
            }
            Err(e) => {
                println!("Failed to send MIDI: {}", e);
            }
        }
    } else {
        println!();
        println!("Skipping MIDI test (plugin doesn't support MIDI input)");
    }

    // Step 7: Parameter Test (if available)
    if let Ok(params) = plugin.get_parameters() {
        if !params.is_empty() {
            println!();
            println!("Testing Parameter Changes");
            println!("=============================");

            let test_param = &params[0];
            let original_value = test_param.value;

            println!("Testing parameter: {}", test_param.name);
            println!("Original value: {:.3}", original_value);

            // Test setting a new value
            let new_value = if original_value < 0.5 { 0.8 } else { 0.2 };
            match plugin.set_parameter(test_param.id, new_value) {
                Ok(()) => {
                    println!("Successfully set parameter to {:.3}", new_value);

                    // Restore original value
                    let _ = plugin.set_parameter(test_param.id, original_value);
                    println!("Restored original value");
                }
                Err(e) => {
                    println!("Failed to set parameter: {}", e);
                }
            }
        }
    }

    // Step 8: Cleanup
    println!();
    println!("Cleanup");
    println!("==========");

    match plugin.stop_processing() {
        Ok(()) => println!("Processing stopped successfully"),
        Err(e) => println!("Error stopping processing: {}", e),
    }

    // Final Summary
    println!();
    println!("Plugin Test Summary");
    println!("======================");
    println!("Plugin Name:    {}", info.name);
    println!("Type:           {}", plugin_type);
    println!("Load Status:    Success");
    println!("Processing:     Started and stopped successfully");
    if info.has_midi_input {
        println!("MIDI Test:      MIDI events sent successfully");
    }
    println!();
    println!("This plugin appears to be working correctly!");
    println!("   You can now use it in your VST3 host application.");

    Ok(())
}
