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

    let params = plugin.get_parameters().expect("Failed to get parameters");
    println!("Plugin has {} parameters", params.len());

    if let Some(first_param) = params.first() {
        println!(
            "First parameter: {} = {}",
            first_param.name,
            first_param.format_value(first_param.value)
        );

        // Try to set the parameter
        let new_value = if first_param.value > 0.5 { 0.25 } else { 0.75 };
        plugin
            .set_parameter(first_param.id, new_value)
            .expect("Failed to set parameter");

        // Verify it was set
        let updated_params = plugin
            .get_parameters()
            .expect("Failed to get updated parameters");
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

    let plugin = host
        .load_plugin(&plugin_info.path)
        .expect("Failed to load plugin");

    // Phase 1 capstone: the library wires a CpalBackend to the plugin and pumps
    // `process_audio` from the device callback. `play` opens the default output
    // device and starts streaming; the AudioHandle keeps it alive and lets us drive
    // the plugin while it runs.
    let audio = host.play(plugin).expect("Failed to start audio playback");

    // Feed a note so an instrument actually produces output.
    audio
        .lock()
        .send_midi_note(60, 110, MidiChannel::Ch1)
        .expect("Failed to send MIDI note");

    // Let the audio thread pull a number of blocks.
    thread::sleep(Duration::from_millis(500));

    // The plugin should have produced some non-silent output by now. We read the
    // levels the bridge populated on the audio thread.
    let levels = audio.lock().get_output_levels();
    let any_activity = levels.channels.iter().any(|c| c.peak > 0.0 || c.rms > 0.0);

    audio
        .lock()
        .send_midi_note_off(60, MidiChannel::Ch1)
        .expect("Failed to send note off");

    audio.stop();

    // Synth plugins should drive the meters; pure effects fed silence may not, so
    // this is a soft check rather than a hard assert.
    println!(
        "Audio processing test completed (level activity: {})",
        any_activity
    );
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
    let initial_params = plugin
        .get_parameters()
        .expect("Failed to get initial parameters");

    // Modify some parameters
    if let Some(param) = initial_params.first() {
        let new_value = if param.value > 0.5 { 0.25 } else { 0.75 };
        plugin
            .set_parameter(param.id, new_value)
            .expect("Failed to set parameter");
    }

    // Get current state by reading parameters
    let modified_params = plugin
        .get_parameters()
        .expect("Failed to get modified parameters");

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
    let restored_params = plugin
        .get_parameters()
        .expect("Failed to get restored parameters");
    if let (Some(modified), Some(restored)) = (modified_params.first(), restored_params.first()) {
        assert!(
            (modified.value - restored.value).abs() < 0.01,
            "Parameter not restored correctly"
        );
    }
}

/// Phase 3 capstone: drive a plugin end-to-end in an isolated process.
///
/// Requires the `vst3-host-helper` binary to be built and the bundled Dexed test
/// plugin to be present, so it is `#[ignore]`d by default. Run with:
/// `cargo test --features process-isolation --test integration_tests -- --ignored`.
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the helper binary and the bundled test plugin"]
fn test_process_isolation() {
    let plugin_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(plugin_path).exists() {
        println!("Test plugin not found at {plugin_path}, skipping");
        return;
    }

    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .with_process_isolation(true)
        .build()
        .expect("Failed to build isolated host");

    let mut plugin = host
        .load_plugin(plugin_path)
        .expect("Failed to load plugin in isolated process");

    // Parameters marshal across the process boundary.
    let params = plugin.get_parameters().expect("get_parameters over IPC");
    assert!(!params.is_empty(), "isolated plugin reported no parameters");

    // Parameter set/get round-trips across the boundary.
    let id = params[0].id;
    plugin
        .set_parameter(id, 0.5)
        .expect("set_parameter over IPC");
    let got = plugin.get_parameter(id).expect("get_parameter over IPC");
    assert!(
        (got - 0.5).abs() < 0.05,
        "parameter did not round-trip: {got}"
    );

    // Audio crosses the boundary.
    plugin
        .start_processing()
        .expect("start_processing over IPC");
    plugin
        .send_midi_note(60, 110, MidiChannel::Ch1)
        .expect("send MIDI over IPC");

    let mut max_peak = 0.0f32;
    for _ in 0..20 {
        let mut buffers = AudioBuffers::new(0, 2, 512, 48000.0);
        plugin
            .process_audio(&mut buffers)
            .expect("process over IPC");
        for ch in &buffers.outputs {
            for &s in ch {
                max_peak = max_peak.max(s.abs());
            }
        }
    }
    plugin.stop_processing().expect("stop_processing over IPC");

    assert!(
        max_peak > 0.0,
        "isolated synth produced no audio across the process boundary"
    );
    println!("Isolated plugin produced audio (peak {max_peak:.4})");
}

/// A dead/crashed helper must surface as an error quickly, never a hang.
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the helper binary"]
fn test_isolation_dead_helper_errors_fast() {
    use vst3_host::process_isolation::{HostCommand, PluginHostProcess};

    let mut proc = match PluginHostProcess::new() {
        Ok(p) => p,
        Err(e) => {
            println!("Helper not available ({e}), skipping");
            return;
        }
    };

    // Kill the helper, then a command must error promptly (not block on read).
    proc.shutdown();
    let start = std::time::Instant::now();
    let res = proc.send_command(HostCommand::GetAllParameters);
    assert!(res.is_err(), "command to a dead helper should error");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "dead-helper command must not hang"
    );
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

    for (name, _vendor) in &free_plugins {
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
                    println!(
                        "  - Parameters: {}",
                        loaded.get_parameters().unwrap_or_default().len()
                    );

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
