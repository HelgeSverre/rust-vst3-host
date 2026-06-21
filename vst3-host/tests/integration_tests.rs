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
    // Prefer the bundled Dexed (free, no license) and read it with the *lightweight*
    // metadata path. `discover_plugins()` instantiates EVERY installed plugin, and some
    // licensed system plugins abort the whole test binary with an uncatchable C++ exception
    // during their license check — so only fall back to discovery if the bundle is missing.
    let bundled = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if std::path::Path::new(bundled).exists() {
        if let Ok(info) = vst3_host::simple::get_plugin_info(bundled) {
            println!(
                "Using bundled test plugin: {} by {}",
                info.name, info.vendor
            );
            return Some(info);
        }
    }

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

/// Track D: real `getState`/`setState` round-trip through the plugin's own serializer,
/// against the bundled Dexed plugin (so it doesn't depend on the host's plugin install).
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_plugin_state_save_restore() {
    let plugin_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(plugin_path).exists() {
        println!("Test plugin not found at {plugin_path}, skipping");
        return;
    }

    let mut host = Vst3Host::new().expect("Failed to create host");
    let mut plugin = host
        .load_plugin(plugin_path)
        .expect("Failed to load plugin");

    // Pick an automatable, writable parameter to drive.
    let params = plugin.get_parameters().expect("get_parameters");
    let param = params
        .iter()
        .find(|p| p.can_automate && !p.is_read_only && !p.is_bypass)
        .or_else(|| params.first())
        .expect("plugin has at least one parameter")
        .clone();
    let id = param.id;

    // Establish a known value, then snapshot it.
    plugin.set_parameter(id, 0.25).expect("set_parameter v1");
    let v1 = plugin.get_parameter(id).expect("get v1");
    let snapshot = plugin.save_state().expect("save_state");
    assert!(!snapshot.is_empty(), "saved state should not be empty");

    // Move the parameter away and confirm the serialized state actually changed.
    plugin.set_parameter(id, 0.75).expect("set_parameter v2");
    let moved = plugin.save_state().expect("save_state after change");
    assert_ne!(
        snapshot, moved,
        "changing a parameter should change the serialized state"
    );

    // Restore the snapshot: bytes must round-trip exactly and the live value must return.
    plugin.load_state(&snapshot).expect("load_state");
    let after_restore = plugin.save_state().expect("save_state after restore");
    let v3 = plugin.get_parameter(id).expect("get after restore");
    println!(
        "param '{}' (id {id}): v1={v1} restored={v3}; bytes snapshot={} moved={} after_restore={}",
        param.name,
        snapshot.len(),
        moved.len(),
        after_restore.len()
    );
    assert_eq!(snapshot, after_restore, "state should round-trip exactly");
    assert!(
        (v3 - v1).abs() < 0.05,
        "parameter not restored: {v3} (expected ~{v1})"
    );
    println!("State round-trip OK ({} bytes)", snapshot.len());
}

/// Track D: real `.vstpreset` save/load round-trip through the standard Steinberg container
/// format, against the bundled Dexed plugin.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_vstpreset_save_load() {
    let plugin_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(plugin_path).exists() {
        println!("Test plugin not found at {plugin_path}, skipping");
        return;
    }

    let mut host = Vst3Host::new().expect("Failed to create host");
    let mut plugin = host
        .load_plugin(plugin_path)
        .expect("Failed to load plugin");

    let params = plugin.get_parameters().expect("get_parameters");
    let param = params
        .iter()
        .find(|p| p.can_automate && !p.is_read_only && !p.is_bypass)
        .or_else(|| params.first())
        .expect("plugin has at least one parameter")
        .clone();
    let id = param.id;

    // Establish a known value, then save it to a .vstpreset file.
    plugin.set_parameter(id, 0.25).expect("set_parameter v1");
    let v1 = plugin.get_parameter(id).expect("get v1");

    let mut preset_path = std::env::temp_dir();
    preset_path.push(format!("vst3-host-test-{}.vstpreset", std::process::id()));
    plugin.save_vstpreset(&preset_path).expect("save_vstpreset");

    // The on-disk file must carry the standard container header tagged with our class id.
    let raw = std::fs::read(&preset_path).expect("read written preset");
    assert!(raw.len() >= 48, "preset shorter than the header");
    assert_eq!(&raw[0..4], b"VST3", "magic");
    assert_eq!(&raw[8..40], plugin.info().uid.as_bytes(), "class id");

    // Move the parameter away, then load the preset back and confirm it is restored.
    plugin.set_parameter(id, 0.75).expect("set_parameter v2");
    plugin.load_vstpreset(&preset_path).expect("load_vstpreset");
    let v3 = plugin.get_parameter(id).expect("get after restore");

    let _ = std::fs::remove_file(&preset_path);

    println!(
        "vstpreset round-trip: v1={v1} restored={v3} ({} bytes)",
        raw.len()
    );
    assert!(
        (v3 - v1).abs() < 0.05,
        "parameter not restored from vstpreset: {v3} (expected ~{v1})"
    );
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

    // Accurate metadata crosses the boundary (not the old "unknown"/default placeholders).
    let info = plugin.info();
    assert!(
        info.category.to_lowercase().contains("inst")
            || info.category.to_lowercase().contains("synth"),
        "isolated metadata lost the real category: {:?}",
        info.category
    );
    assert_ne!(
        info.uid, "unknown",
        "isolated plugin uid should marshal across IPC"
    );
    assert!(
        info.has_midi_input,
        "isolated Dexed should report MIDI input"
    );
    assert_eq!(
        plugin.output_channel_count(),
        2,
        "isolated channel count should be real"
    );

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

/// Track D: plugin state save/restore survives the process-isolation boundary.
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the helper binary and the bundled test plugin"]
fn test_isolation_state_roundtrip() {
    let plugin_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(plugin_path).exists() {
        println!("Test plugin not found at {plugin_path}, skipping");
        return;
    }

    let mut host = Vst3Host::builder()
        .with_process_isolation(true)
        .build()
        .expect("build isolated host");
    let mut plugin = host.load_plugin(plugin_path).expect("load isolated");

    let id = plugin.get_parameters().expect("params")[0].id;

    // Snapshot a known value, move away, restore, confirm the value comes back — all
    // across the IPC boundary.
    plugin.set_parameter(id, 0.25).expect("set v1");
    let snapshot = plugin.save_state().expect("save_state over IPC");
    assert!(
        !snapshot.is_empty(),
        "isolated save_state returned no bytes"
    );

    plugin.set_parameter(id, 0.75).expect("set v2");
    plugin.load_state(&snapshot).expect("load_state over IPC");

    let restored = plugin.get_parameter(id).expect("get after restore");
    assert!(
        (restored - 0.25).abs() < 0.05,
        "isolated state did not restore: {restored} (expected ~0.25)"
    );
    println!("Isolated state round-trip OK ({} bytes)", snapshot.len());
}

/// Review #2/#3: metadata is detected (version, category, MIDI capability, channel count),
/// not hardcoded. Checked against bundled Dexed (an instrument synth).
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_plugin_metadata_is_detected() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(path).exists() {
        println!("Test plugin not found, skipping");
        return;
    }
    let mut host = Vst3Host::new().unwrap();
    let plugin = host.load_plugin(path).unwrap();
    let i = plugin.info();
    assert!(!i.version.is_empty(), "version should be detected");
    let cat = i.category.to_lowercase();
    assert!(
        cat.contains("inst") || cat.contains("synth"),
        "Dexed is an instrument; got category {:?}",
        i.category
    );
    assert!(i.has_midi_input, "Dexed accepts MIDI input");
    assert_eq!(plugin.output_channel_count(), 2, "Dexed is stereo");
}

/// Track D: automating a parameter through the public API changes the rendered audio
/// (end-to-end). The processor-queue mechanism itself is covered deterministically by the
/// `parameter_changes_tests` unit test; this proves the full path produces an audible effect
/// and that feeding the input queue didn't break processing.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_parameter_automation_changes_audio() {
    let plugin_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(plugin_path).exists() {
        println!("Test plugin not found, skipping");
        return;
    }

    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .unwrap();
    let mut plugin = host.load_plugin(plugin_path).unwrap();
    plugin.start_processing().unwrap();

    let params = plugin.get_parameters().unwrap();
    let cutoff = params
        .iter()
        .find(|p| p.name.to_lowercase().contains("cutoff"))
        .map(|p| p.id)
        .unwrap_or(params[0].id);

    // Render a held note with the parameter set to a value, measuring output RMS.
    fn render_rms(plugin: &mut Plugin, id: u32, value: f64) -> f64 {
        plugin.set_parameter(id, value).unwrap();
        plugin.send_midi_note(60, 110, MidiChannel::Ch1).unwrap();
        let (mut sumsq, mut n) = (0.0f64, 0u64);
        for _ in 0..40 {
            let mut b = AudioBuffers::new(0, 2, 512, 48000.0);
            plugin.process_audio(&mut b).unwrap();
            for ch in &b.outputs {
                for &s in ch {
                    sumsq += (s as f64) * (s as f64);
                    n += 1;
                }
            }
        }
        plugin.send_midi_note_off(60, MidiChannel::Ch1).unwrap();
        (sumsq / n.max(1) as f64).sqrt()
    }

    let low = render_rms(&mut plugin, cutoff, 0.05);
    let high = render_rms(&mut plugin, cutoff, 0.95);
    plugin.stop_processing().ok();

    println!("automation A/B (cutoff): low RMS={low:.6}, high RMS={high:.6}");
    assert!(low > 0.0 || high > 0.0, "plugin produced no audio at all");
    assert!(
        (low - high).abs() > 1e-4,
        "automating the parameter did not change the audio: low={low:.6} high={high:.6}"
    );
}

/// Track D: MIDI a plugin emits is captured across the process-isolation boundary.
///
/// Needs a MIDI-*emitting* plugin (arpeggiator/sequencer that produces output without GUI
/// configuration). The bundled Dexed does not emit, and common installed sequencers
/// (HY-MPS3, TranceEngine) emit nothing until a pattern is programmed in their GUI — so on
/// most machines this can only assert the boundary doesn't *fabricate or drop* MIDI relative
/// to the in-process path (parity), which it does by running the same capture code. If a
/// real emitter is present it additionally asserts events actually cross. The wire format
/// itself is covered deterministically by `process_isolation::wire_tests`.
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the helper binary; a real emitter only if one is installed"]
fn test_isolation_output_midi_parity() {
    // Drive the SAME plugin in-process and isolated with identical input; the isolated
    // output MIDI must equal the in-process output MIDI (the boundary must be transparent).
    let candidates = [
        "/Library/Audio/Plug-Ins/VST3/HY-MPS3 free.vst3",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3"),
    ];
    let Some(path) = candidates.iter().find(|p| std::path::Path::new(p).exists()) else {
        println!("No candidate plugin installed, skipping");
        return;
    };

    fn drive(host: &mut Vst3Host, path: &str) -> Vec<MidiEvent> {
        let mut plugin = host.load_plugin(path).expect("load");
        plugin.start_processing().expect("start");
        for note in [60, 64, 67] {
            let _ = plugin.send_midi_note(note, 100, MidiChannel::Ch1);
        }
        let mut out = Vec::new();
        for _ in 0..200 {
            let mut buffers = AudioBuffers::new(0, 2, 512, 48000.0);
            plugin.process_audio(&mut buffers).expect("process");
            out.extend(plugin.take_output_midi());
        }
        plugin.stop_processing().ok();
        out
    }

    let mut inproc = Vst3Host::builder().block_size(512).build().unwrap();
    let in_events = drive(&mut inproc, path);

    let mut iso = Vst3Host::builder()
        .block_size(512)
        .with_process_isolation(true)
        .build()
        .unwrap();
    let iso_events = drive(&mut iso, path);

    assert_eq!(
        in_events, iso_events,
        "isolated output MIDI must match in-process for the same input"
    );
    println!(
        "Output-MIDI parity holds across the boundary ({} event(s)){}",
        in_events.len(),
        if in_events.is_empty() {
            " — note: this plugin emits nothing without configuration, so the cross-boundary \
             data path is exercised but not observed carrying events"
        } else {
            ""
        }
    );
}

/// B2: when an isolated helper dies mid-session, calls surface a typed `PluginCrashed`
/// (the host stays alive) and `recover()` brings the plugin back.
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the helper binary and the bundled test plugin"]
fn test_isolation_crash_recovery() {
    let plugin_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(plugin_path).exists() {
        println!("Test plugin not found at {plugin_path}, skipping");
        return;
    }

    let mut host = Vst3Host::builder()
        .with_process_isolation(true)
        .build()
        .expect("build isolated host");
    let mut plugin = host.load_plugin(plugin_path).expect("load isolated");
    plugin.start_processing().expect("start");
    let id = plugin.get_parameters().expect("params")[0].id;
    plugin.set_parameter(id, 0.5).expect("set");

    // Simulate a crash: kill the helper process out from under us.
    let pid = plugin.isolation_pid().expect("helper pid");
    let killed = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .expect("kill helper");
    assert!(killed.success(), "failed to kill helper");
    thread::sleep(Duration::from_millis(300));

    // The next call must surface a typed crash, not hang and not kill us. Reaching this
    // assert at all proves the host process survived the helper's death.
    let err = plugin
        .get_parameters()
        .expect_err("call to a dead helper should error");
    assert!(
        matches!(err, Error::PluginCrashed),
        "expected PluginCrashed, got {err:?}"
    );

    // Explicit recovery respawns + reloads; the plugin is usable again.
    plugin.recover().expect("recover");
    let params = plugin.get_parameters().expect("params after recover");
    assert!(!params.is_empty(), "no parameters after recovery");
    println!(
        "Crash recovery OK: host survived, plugin reloaded ({} params)",
        params.len()
    );
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

/// Track B: probing + auto-isolation handle crash-prone plugins without killing the host.
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "requires the helper binary and installed plugins"]
fn test_probe_and_auto_isolate() {
    use vst3_host::{ProbeResult, Vst3Host};

    let host = Vst3Host::new().expect("host");

    // A plugin that loads cleanly probes Ok.
    let dexed = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if std::path::Path::new(dexed).exists() {
        assert_eq!(
            host.probe_plugin(dexed),
            ProbeResult::Ok,
            "Dexed should probe Ok"
        );
    }

    // A bogus path fails cleanly (not a crash).
    assert!(matches!(
        host.probe_plugin("/no/such/plugin.vst3"),
        ProbeResult::Failed(_)
    ));

    // WaveShell (if installed) crashes the isolated helper — contained as Crashed — and an
    // auto-isolating host returns an Err instead of segfaulting this process.
    let waveshell = "/Library/Audio/Plug-Ins/VST3/WaveShell1-VST3 14.12.vst3";
    if std::path::Path::new(waveshell).exists() {
        assert_eq!(
            host.probe_plugin(waveshell),
            ProbeResult::Crashed,
            "WaveShell should probe as Crashed"
        );
        let mut h = Vst3Host::builder()
            .auto_isolate_problematic(true)
            .build()
            .unwrap();
        assert!(
            h.load_plugin(waveshell).is_err(),
            "auto-isolated WaveShell should error, not crash the host"
        );
        // Reaching here means the host process survived WaveShell's crash.
        println!("WaveShell contained: host survived");
    }
}

/// M3: the RealtimePluginRunner applies control commands from its lock-free queue and
/// renders audio, driven offline (no device).
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_realtime_runner_applies_commands_and_renders() {
    use vst3_host::realtime::RealtimePluginRunner;

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(path).exists() {
        println!("Test plugin not found, skipping");
        return;
    }
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .unwrap();
    let plugin = host.load_plugin(path).unwrap();
    let params = plugin.get_parameters().unwrap();
    let cutoff = params
        .iter()
        .find(|p| p.name.to_lowercase().contains("cutoff"))
        .map(|p| p.id)
        .unwrap_or(params[0].id);

    let (mut runner, mut control) = RealtimePluginRunner::new(plugin, 256);
    runner.start().unwrap();

    // Queue control changes through the lock-free handle (as a control thread would).
    assert!(control.set_parameter(cutoff, 0.9));
    assert!(control.send_midi(MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 110
    }));

    // Render offline; the runner drains the queue + processes with no locks.
    let mut peak = 0.0f32;
    for _ in 0..40 {
        let mut b = AudioBuffers::new(0, 2, 512, 48000.0);
        runner.process(&mut b).unwrap();
        for ch in &b.outputs {
            for &s in ch {
                peak = peak.max(s.abs());
            }
        }
    }
    assert!(peak > 0.0, "runner produced no audio from the queued note");

    // The queued parameter change was applied.
    let plugin = runner.into_plugin();
    let v = plugin.get_parameter(cutoff).unwrap();
    assert!(
        (v - 0.9).abs() < 0.05,
        "queued parameter change not applied: {v}"
    );
    println!("Realtime runner OK: peak {peak:.3}, cutoff {v}");
}

/// M3: the lock-free cpal play_realtime path runs end-to-end (needs an output device).
#[test]
#[ignore = "Requires audio hardware and the bundled test plugin"]
fn test_play_realtime_smoke() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(path).exists() {
        println!("Test plugin not found, skipping");
        return;
    }
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .unwrap();
    let plugin = host.load_plugin(path).unwrap();
    let mut audio = host.play_realtime(plugin, 1024).expect("play_realtime");

    // Drive control changes over the lock-free queue; the audio callback never locks.
    assert!(audio.control().send_midi(MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 110
    }));
    std::thread::sleep(Duration::from_millis(300));
    audio.control().send_midi(MidiEvent::NoteOff {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 0,
    });
    audio.stop();
    // Reaching here means the realtime callback ran without crashing.
    println!("play_realtime smoke OK");
}

/// M-automation: a ParameterAutomation curve, scheduled sample-accurately each block via
/// set_parameter_at, audibly evolves the output over a timeline (offline render).
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_sample_accurate_automation_evolves_output() {
    use vst3_host::parameters::{AutomationCurve, ParameterAutomation};

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(path).exists() {
        println!("Test plugin not found, skipping");
        return;
    }
    let sr = 48000.0;
    let block = 512usize;
    let mut host = Vst3Host::builder()
        .sample_rate(sr)
        .block_size(block)
        .build()
        .unwrap();
    let mut plugin = host.load_plugin(path).unwrap();
    plugin.start_processing().unwrap();

    let cutoff = plugin
        .get_parameters()
        .unwrap()
        .iter()
        .find(|p| p.name.to_lowercase().contains("cutoff"))
        .map(|p| p.id)
        .expect("Dexed has a cutoff param");
    plugin.send_midi_note(60, 110, MidiChannel::Ch1).unwrap();

    // Cutoff ramps closed -> open over one second.
    let auto = ParameterAutomation::new()
        .add_point(0.0, 0.05)
        .add_point(1.0, 0.95)
        .with_curve(AutomationCurve::Linear);

    let blocks = sr as usize / block; // ~1 second
    let (mut early, mut late) = (0.0f64, 0.0f64);
    let mut t = 0.0;
    for b in 0..blocks {
        // Schedule this block's sub-block automation points (sample-accurate).
        for (offset, value) in auto.points_for_block(t, block, sr, 8) {
            plugin.set_parameter_at(cutoff, value, offset).unwrap();
        }
        let mut buf = AudioBuffers::new(0, 2, block, sr);
        plugin.process_audio(&mut buf).unwrap();
        let sumsq: f64 = buf
            .outputs
            .iter()
            .flat_map(|c| c.iter())
            .map(|&s| (s as f64) * (s as f64))
            .sum();
        let rms = (sumsq / (block as f64 * 2.0)).sqrt();
        if b < blocks / 4 {
            early += rms;
        } else if b >= 3 * blocks / 4 {
            late += rms;
        }
        t += block as f64 / sr;
    }
    plugin.stop_processing().ok();

    println!("automation timeline: early-quarter energy {early:.4}, late-quarter {late:.4}");
    assert!(
        late > early,
        "opening the cutoff over the timeline should raise output energy: early={early:.4} late={late:.4}"
    );
}
