//! Integration tests for vst3-host library
//!
//! These tests require actual VST3 plugins to be installed on the system.
//! They are ignored by default and can be run with:
//! ```
//! cargo test --features cpal-backend -- --ignored
//! ```

#![cfg(feature = "cpal-backend")]

use std::thread;
use std::time::Duration;
use vst3_host::prelude::*;

/// Helper to find a test plugin
fn find_test_plugin() -> Option<PluginInfo> {
    // Try to find common free VST3 plugins
    let test_plugins = [
        "Vital",            // Vital synthesizer
        "Surge XT Effects", // Surge XT Effects
        "Surge XT",         // Surge XT synthesizer
        "Dexed",            // Dexed FM synthesizer
        "TAL-NoiseMaker",   // TAL NoiseMaker
        "OB-Xd",            // OB-Xd synthesizer
    ];

    let mut host = Vst3Host::new().ok()?;
    let plugins = host.discover_plugins().ok()?;

    // Try to find one of our preferred test plugins
    for test_name in &test_plugins {
        if let Some(plugin) = plugins.iter().find(|p| p.name.contains(test_name)) {
            println!("Found test plugin: {} by {}", plugin.name, plugin.vendor);
            return Some(plugin.clone());
        }
    }

    // Return any plugin if none of the test plugins are found
    if let Some(plugin) = plugins.into_iter().next() {
        println!(
            "Using available plugin: {} by {}",
            plugin.name, plugin.vendor
        );
        Some(plugin)
    } else {
        None
    }
}

#[test]
#[ignore = "Requires VST3 plugins to be installed"]
fn test_plugin_discovery() {
    let mut host = Vst3Host::new().expect("Failed to create host");
    let plugins = host.discover_plugins().expect("Failed to discover plugins");

    // Should find at least one plugin if any are installed
    if !plugins.is_empty() {
        println!("Found {} plugins:", plugins.len());
        for plugin in &plugins[..5.min(plugins.len())] {
            println!(
                "  - {} by {} ({})",
                plugin.name, plugin.vendor, plugin.version
            );
        }
    }
}

#[test]
#[ignore = "Requires VST3 plugins to be installed"]
fn test_plugin_loading() {
    let Some(plugin_info) = find_test_plugin() else {
        println!("No VST3 plugins found, skipping test");
        return;
    };

    println!(
        "Testing with plugin: {} by {}",
        plugin_info.name, plugin_info.vendor
    );

    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .expect("Failed to create host");

    let plugin = host
        .load_plugin(&plugin_info.path)
        .expect("Failed to load plugin");

    assert_eq!(plugin.info().name, plugin_info.name);
    assert_eq!(plugin.info().vendor, plugin_info.vendor);
}

#[test]
#[ignore = "Requires VST3 plugins to be installed"]
fn test_plugin_parameters() {
    let Some(plugin_info) = find_test_plugin() else {
        println!("No VST3 plugins found, skipping test");
        return;
    };

    let mut host = Vst3Host::new().expect("Failed to create host");
    let mut plugin = host
        .load_plugin(&plugin_info.path)
        .expect("Failed to load plugin");

    let params = plugin.get_parameters();
    println!("Plugin has {} parameters", params.len());

    if let Some(first_param) = params.first() {
        println!(
            "First parameter: {} = {} {}",
            first_param.name, first_param.value_text, first_param.unit
        );

        // Try to set the parameter
        let new_value = if first_param.value > 0.5 { 0.25 } else { 0.75 };
        plugin
            .set_parameter(first_param.id, new_value)
            .expect("Failed to set parameter");

        // Verify it was set
        let updated_params = plugin.get_parameters();
        let updated = updated_params
            .iter()
            .find(|p| p.id == first_param.id)
            .expect("Parameter not found after update");

        assert!(
            (updated.value - new_value).abs() < 0.01,
            "Parameter value not updated correctly"
        );
    }
}

#[test]
#[ignore = "Requires VST3 plugins to be installed"]
fn test_midi_processing() {
    let Some(plugin_info) = find_test_plugin() else {
        println!("No VST3 plugins found, skipping test");
        return;
    };

    // Skip if plugin doesn't support MIDI
    if !plugin_info.has_midi_input {
        println!("Plugin doesn't support MIDI input, skipping test");
        return;
    }

    let mut host = Vst3Host::new().expect("Failed to create host");
    let mut plugin = host
        .load_plugin(&plugin_info.path)
        .expect("Failed to load plugin");

    plugin
        .start_processing()
        .expect("Failed to start processing");

    // Send some MIDI notes
    plugin
        .send_midi_note(60, 100, MidiChannel::Ch1)
        .expect("Failed to send note on");
    thread::sleep(Duration::from_millis(100));

    plugin
        .send_midi_note_off(60, MidiChannel::Ch1)
        .expect("Failed to send note off");
    thread::sleep(Duration::from_millis(100));

    plugin.stop_processing().expect("Failed to stop processing");
}

#[test]
#[ignore = "Requires VST3 plugins to be installed and audio hardware"]
fn test_audio_processing() {
    let Some(plugin_info) = find_test_plugin() else {
        println!("No VST3 plugins found, skipping test");
        return;
    };

    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .expect("Failed to create host");

    // Create audio backend separately
    let backend = CpalBackend::new().expect("Failed to create audio backend");

    let mut plugin = host
        .load_plugin_with_backend(&plugin_info.path, Box::new(backend))
        .expect("Failed to load plugin");

    // Set up audio level monitoring
    let levels_received = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let levels_clone = levels_received.clone();
    plugin.on_audio_process(move |levels| {
        // Check if any channel has activity
        for channel in &levels.channels {
            if channel.peak > 0.0 || channel.rms > 0.0 {
                levels_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        }
    });

    plugin
        .start_processing()
        .expect("Failed to start processing");

    // Let it process for a bit
    thread::sleep(Duration::from_secs(1));

    plugin.stop_processing().expect("Failed to stop processing");

    // Note: We can't really assert that audio was processed without
    // generating test signals, which would require more setup
    println!("Audio processing test completed");
}

#[test]
#[ignore = "Requires VST3 plugins to be installed"]
fn test_plugin_state_save_restore() {
    let Some(plugin_info) = find_test_plugin() else {
        println!("No VST3 plugins found, skipping test");
        return;
    };

    let mut host = Vst3Host::new().expect("Failed to create host");
    let mut plugin = host
        .load_plugin(&plugin_info.path)
        .expect("Failed to load plugin");

    // Get initial parameters
    let initial_params = plugin.get_parameters();

    // Modify some parameters
    if let Some(param) = initial_params.first() {
        let new_value = if param.value > 0.5 { 0.25 } else { 0.75 };
        plugin
            .set_parameter(param.id, new_value)
            .expect("Failed to set parameter");
    }

    // Get current state by reading parameters
    let modified_params = plugin.get_parameters();

    // Reset parameters to different values
    if let Some(param) = initial_params.first() {
        plugin
            .set_parameter(param.id, 0.5)
            .expect("Failed to set parameter");
    }

    // Restore parameters to modified state
    for param in &modified_params {
        plugin
            .set_parameter(param.id, param.value)
            .expect("Failed to restore parameter");
    }

    // Verify parameters were restored
    let restored_params = plugin.get_parameters();
    if let (Some(modified), Some(restored)) = (modified_params.first(), restored_params.first()) {
        assert!(
            (modified.value - restored.value).abs() < 0.01,
            "Parameter not restored correctly"
        );
    }
}

/// Test process isolation feature if enabled
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires helper binary and VST3 plugins"]
fn test_process_isolation() {
    use std::env;
    use std::path::Path;
    use vst3_host::process_isolation::IsolatedPlugin;

    // Try to find the helper binary
    let exe_path = env::current_exe().expect("Failed to get current exe");
    let helper_path = exe_path
        .parent()
        .expect("Failed to get exe directory")
        .join("examples")
        .join("isolated_plugin_helper");

    if !helper_path.exists() {
        println!(
            "Helper binary not found at {:?}, skipping test",
            helper_path
        );
        return;
    }

    let mut isolated = IsolatedPlugin::new(helper_path);
    isolated.start().expect("Failed to start helper process");

    // Load a plugin (the example returns mock data)
    let info = isolated
        .load_plugin(Path::new("/fake/plugin.vst3"))
        .expect("Failed to load plugin in isolated process");

    assert_eq!(info.name, "Example Plugin");
    assert_eq!(info.vendor, "Example Vendor");

    // Get parameters
    let params = isolated.get_parameters().expect("Failed to get parameters");
    assert_eq!(params.len(), 3);

    // Set a parameter
    isolated
        .set_parameter(0, 0.75)
        .expect("Failed to set parameter");

    // Process audio
    let input = vec![vec![0.0; 512]; 2];
    let output = isolated
        .process_audio(input.clone(), 512)
        .expect("Failed to process audio");
    assert_eq!(output, input); // Example just passes through

    isolated.stop().expect("Failed to stop helper process");
}

#[test]
#[ignore = "Requires free VST3 synths to be installed"]
fn test_specific_free_plugins() {
    let mut host = Vst3Host::new().expect("Failed to create host");
    let plugins = host.discover_plugins().expect("Failed to discover plugins");

    // Check for specific free plugins
    let free_plugins = [
        ("Vital", "Matt Tytel"),
        ("Surge XT", "Surge Synth Team"),
        ("Dexed", "Digital Suburban"),
        ("OB-Xd", "discoDSP"),
    ];

    for (name, vendor) in &free_plugins {
        if let Some(plugin) = plugins.iter().find(|p| p.name.contains(name)) {
            println!(
                "Found {}: {} by {} v{}",
                name, plugin.name, plugin.vendor, plugin.version
            );

            // Try to load it
            match host.load_plugin(&plugin.path) {
                Ok(mut loaded) => {
                    println!("  - Successfully loaded");
                    println!(
                        "  - Audio I/O: {}x{}",
                        plugin.audio_inputs, plugin.audio_outputs
                    );
                    println!(
                        "  - MIDI: {}",
                        if plugin.has_midi_input { "Yes" } else { "No" }
                    );
                    println!("  - Parameters: {}", loaded.get_parameters().len());

                    // Test basic operations
                    if loaded.start_processing().is_ok() {
                        println!("  - Processing started successfully");
                        loaded.stop_processing().ok();
                    }
                }
                Err(e) => {
                    println!("  - Failed to load: {}", e);
                }
            }
            println!();
        }
    }
}
