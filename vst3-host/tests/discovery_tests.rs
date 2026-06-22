use vst3_host::host::DiscoveryProgress;
use vst3_host::plugin::PluginInfo;

#[test]
fn test_plugin_info() {
    let info = PluginInfo {
        path: std::path::PathBuf::from("/path/to/plugin.vst3"),
        name: "Test Plugin".to_string(),
        vendor: "Test Vendor".to_string(),
        version: "1.0.0".to_string(),
        category: "Fx".to_string(),
        uid: "123456789ABCDEF0".to_string(),
        audio_inputs: 2,
        audio_outputs: 2,
        has_midi_input: false,
        has_midi_output: false,
        has_gui: true,
    };

    assert_eq!(info.name, "Test Plugin");
    assert_eq!(info.vendor, "Test Vendor");
    assert_eq!(info.version, "1.0.0");
    assert_eq!(info.path, std::path::PathBuf::from("/path/to/plugin.vst3"));
    assert_eq!(info.audio_inputs, 2);
    assert_eq!(info.audio_outputs, 2);
    assert!(!info.has_midi_input);
    assert!(info.has_gui);
}

#[test]
fn test_discovery_progress() {
    // Test Started variant
    let progress = DiscoveryProgress::Started { total_plugins: 10 };
    match progress {
        DiscoveryProgress::Started { total_plugins } => {
            assert_eq!(total_plugins, 10);
        }
        _ => panic!("Wrong variant"),
    }

    // Test Found variant
    let info = PluginInfo {
        path: std::path::PathBuf::from("/test/path.vst3"),
        name: "Found Plugin".to_string(),
        vendor: "Vendor".to_string(),
        version: "1.0".to_string(),
        category: "Instrument".to_string(),
        uid: "0000000000000000".to_string(),
        audio_inputs: 0,
        audio_outputs: 2,
        has_midi_input: true,
        has_midi_output: false,
        has_gui: false,
    };

    let progress = DiscoveryProgress::Found {
        plugin: info.clone(),
        current: 5,
        total: 10,
    };

    match progress {
        DiscoveryProgress::Found {
            plugin,
            current,
            total,
        } => {
            assert_eq!(plugin.name, "Found Plugin");
            assert_eq!(current, 5);
            assert_eq!(total, 10);
        }
        _ => panic!("Wrong variant"),
    }

    // Test Error variant
    let progress = DiscoveryProgress::Error {
        path: "/bad/plugin.vst3".to_string(),
        error: "Failed to load".to_string(),
    };

    match progress {
        DiscoveryProgress::Error { path, error } => {
            assert_eq!(path, "/bad/plugin.vst3");
            assert_eq!(error, "Failed to load");
        }
        _ => panic!("Wrong variant"),
    }

    // Test Completed variant
    let progress = DiscoveryProgress::Completed { total_found: 8 };
    match progress {
        DiscoveryProgress::Completed { total_found } => {
            assert_eq!(total_found, 8);
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn test_plugin_info_uid() {
    let uid = "0123456789ABCDEFFEDCBA9876543210".to_string();

    let info = PluginInfo {
        path: std::path::PathBuf::from("/test.vst3"),
        name: "UID Test".to_string(),
        vendor: "Test".to_string(),
        version: "1.0".to_string(),
        category: "Fx".to_string(),
        uid: uid.clone(),
        audio_inputs: 0,
        audio_outputs: 0,
        has_midi_input: false,
        has_midi_output: false,
        has_gui: false,
    };

    // Verify UID is stored correctly
    assert_eq!(info.uid, uid);
}

#[test]
fn test_instrument_vs_effect() {
    // Test instrument
    let instrument = PluginInfo {
        path: std::path::PathBuf::from("/synth.vst3"),
        name: "Synth".to_string(),
        vendor: "Vendor".to_string(),
        version: "1.0".to_string(),
        category: "Instrument".to_string(),
        uid: "0000000000000000".to_string(),
        audio_inputs: 0,
        audio_outputs: 2,
        has_midi_input: true,
        has_midi_output: false,
        has_gui: true,
    };

    assert_eq!(instrument.category, "Instrument");
    assert_eq!(instrument.audio_inputs, 0); // Instruments often have no audio input
    assert!(instrument.has_midi_input); // But they do have MIDI input

    // Test effect
    let effect = PluginInfo {
        path: std::path::PathBuf::from("/reverb.vst3"),
        name: "Reverb".to_string(),
        vendor: "Vendor".to_string(),
        version: "1.0".to_string(),
        category: "Fx".to_string(),
        uid: "1111111111111111".to_string(),
        audio_inputs: 2,
        audio_outputs: 2,
        has_midi_input: false,
        has_midi_output: false,
        has_gui: true,
    };

    assert_eq!(effect.category, "Fx");
    assert_eq!(effect.audio_inputs, 2); // Effects typically process input
    assert_eq!(effect.audio_outputs, 2);
}

#[test]
fn test_scan_plugin_paths_is_lightweight_and_finds_bundles() {
    use vst3_host::Vst3Host;

    // Point at the bundled test plugins dir (no standard paths) and confirm
    // scan_plugin_paths lists the .vst3 bundle without loading it.
    let test_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins");
    let host = Vst3Host::builder()
        .add_scan_path(test_dir)
        .build()
        .expect("build host");

    let paths = host.scan_plugin_paths();
    if std::path::Path::new(test_dir).join("Dexed.vst3").exists() {
        assert!(
            paths.iter().any(|p| p.ends_with("Dexed.vst3")),
            "expected to find Dexed.vst3 in {paths:?}"
        );
    }
    // Every result must be a .vst3 path.
    assert!(paths
        .iter()
        .all(|p| p.extension().map(|e| e == "vst3").unwrap_or(false)));
}

#[test]
#[ignore = "requires the bundled Dexed test plugin"]
fn test_detailed_plugin_info_dexed() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(path).exists() {
        println!("Dexed not present, skipping");
        return;
    }
    let d = vst3_host::get_detailed_plugin_info(std::path::Path::new(path)).expect("detailed info");
    println!(
        "name={} vendor={} url={} classes={} audio_out_buses={} event_in_buses={}",
        d.info.name,
        d.factory.vendor,
        d.factory.url,
        d.classes.len(),
        d.buses.audio_outputs.len(),
        d.buses.event_inputs.len(),
    );
    assert!(
        !d.factory.vendor.is_empty(),
        "factory vendor should be populated"
    );
    assert!(!d.classes.is_empty(), "should enumerate at least one class");
    assert!(
        !d.buses.audio_outputs.is_empty(),
        "Dexed (a synth) should have an audio output bus"
    );
}

// ---------------------------------------------------------------------------
// Crash-resistant ("safe") discovery via the vst3-host-probe subprocess.
// ---------------------------------------------------------------------------

/// Locate the `vst3-host-probe` binary that the safe-discovery path needs. `cargo test`
/// builds workspace bins, so it sits in the same profile dir as the test binary.
fn ensure_probe_on_path() -> std::path::PathBuf {
    // `cargo test` builds workspace bins, so the probe is next to the test deps dir.
    // current_exe() => target/<profile>/deps/<test-bin>; the probe is two levels up.
    let exe = std::env::current_exe().expect("current_exe");
    let mut dir = exe.parent().expect("deps dir"); // .../deps
    if dir.file_name() == Some(std::ffi::OsStr::new("deps")) {
        dir = dir.parent().expect("profile dir"); // .../<profile>
    }
    dir.join("vst3-host-probe")
}

#[test]
fn safe_discovery_skips_garbage_and_does_not_panic() {
    // A folder containing a bogus ".vst3" entry must NOT take down the scan: the probe
    // fails to introspect it, and the safe path skips it and returns normally.
    let probe = ensure_probe_on_path();
    if !probe.exists() {
        eprintln!("vst3-host-probe not built at {probe:?}; skipping");
        return;
    }
    std::env::set_var("VST3_HOST_PROBE_PATH", &probe);

    let tmp = std::env::temp_dir().join(format!("vst3-safe-disc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("mk tmp");
    // A fake bundle: a directory named *.vst3 that is not a real plugin.
    let garbage = tmp.join("garbage.vst3");
    std::fs::create_dir_all(garbage.join("Contents").join("MacOS")).expect("mk garbage");
    std::fs::write(
        garbage.join("Contents").join("MacOS").join("garbage"),
        b"not a real plugin binary",
    )
    .expect("write garbage");

    let report = vst3_host::discover_plugins_safe(
        std::slice::from_ref(&tmp),
        std::time::Duration::from_secs(10),
    );

    // The bad plugin must be omitted from the successful results...
    assert!(
        !report
            .plugins
            .iter()
            .any(|p| p.info.path.ends_with("garbage.vst3")),
        "garbage plugin should not appear in successful results"
    );
    // ...and recorded as skipped (crashed/failed/timed out), not silently dropped.
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.path().ends_with("garbage.vst3")),
        "garbage plugin should be recorded as skipped, got: {:?}",
        report.skipped
    );

    let _ = std::fs::remove_dir_all(&tmp);
    std::env::remove_var("VST3_HOST_PROBE_PATH");
}

#[test]
fn probe_plugin_info_isolated_returns_err_for_garbage_not_panic() {
    // The single-plugin entry point returns a descriptive Err (never panics / aborts) for
    // a path that cannot be introspected.
    let probe = ensure_probe_on_path();
    if !probe.exists() {
        eprintln!("vst3-host-probe not built at {probe:?}; skipping");
        return;
    }
    std::env::set_var("VST3_HOST_PROBE_PATH", &probe);

    let bogus = std::path::Path::new("/nonexistent/does-not-exist.vst3");
    let result = vst3_host::probe_plugin_info_isolated(bogus, std::time::Duration::from_secs(10));
    assert!(
        result.is_err(),
        "probing a nonexistent plugin should be an Err, got: {result:?}"
    );

    std::env::remove_var("VST3_HOST_PROBE_PATH");
}

#[cfg(unix)]
#[test]
fn safe_discovery_survives_a_probe_that_aborts() {
    // The whole point of out-of-process introspection: a probe that dies by SIGABRT (what a
    // licensed plugin calling abort() during init looks like) must be *survived* by the
    // parent and classified as a crash-skip, not take the scanner down. We stand in a fake
    // probe that aborts itself, so this exercises the parent's child-death handling without
    // needing a real crashing plugin.
    let dir = std::env::temp_dir().join(format!("vst3-abort-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mk dir");

    // A "probe" that kills itself with SIGABRT (signal 6) the moment it runs.
    let fake_probe = dir.join("vst3-host-probe");
    std::fs::write(&fake_probe, "#!/bin/sh\nkill -ABRT $$\n").expect("write fake probe");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake_probe, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake probe");

    // A candidate plugin for the scan to feed to the (aborting) probe.
    let scan = dir.join("scan");
    std::fs::create_dir_all(scan.join("bad.vst3").join("Contents").join("MacOS"))
        .expect("mk fake bundle");
    std::fs::write(
        scan.join("bad.vst3")
            .join("Contents")
            .join("MacOS")
            .join("bad"),
        b"x",
    )
    .expect("write fake bin");

    std::env::set_var("VST3_HOST_PROBE_PATH", &fake_probe);
    let report = vst3_host::discover_plugins_safe(
        std::slice::from_ref(&scan),
        std::time::Duration::from_secs(5),
    );
    std::env::remove_var("VST3_HOST_PROBE_PATH");

    // We are still alive (the abort killed the child, not us), no plugin was returned, and
    // the casualty is recorded as a crash skip.
    assert!(
        report.plugins.is_empty(),
        "no plugin should have introspected"
    );
    assert!(
        report.skipped.iter().any(|s| matches!(
            s,
            vst3_host::SafeDiscoverySkip::Crashed { path, .. } if path.ends_with("bad.vst3")
        )),
        "the aborting probe should be recorded as a Crashed skip, got: {:?}",
        report.skipped
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "requires the bundled Dexed test plugin"]
fn safe_discovery_finds_dexed_out_of_process() {
    let probe = ensure_probe_on_path();
    if !probe.exists() {
        eprintln!("vst3-host-probe not built at {probe:?}; skipping");
        return;
    }
    std::env::set_var("VST3_HOST_PROBE_PATH", &probe);

    let test_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins");
    if !std::path::Path::new(test_dir).join("Dexed.vst3").exists() {
        println!("Dexed not present, skipping");
        std::env::remove_var("VST3_HOST_PROBE_PATH");
        return;
    }

    let report = vst3_host::discover_plugins_safe(
        &[std::path::PathBuf::from(test_dir)],
        std::time::Duration::from_secs(20),
    );

    assert!(
        report
            .plugins
            .iter()
            .any(|p| p.info.path.ends_with("Dexed.vst3")),
        "Dexed should be introspected out-of-process, got plugins: {:?}, skipped: {:?}",
        report
            .plugins
            .iter()
            .map(|p| &p.info.name)
            .collect::<Vec<_>>(),
        report.skipped,
    );

    std::env::remove_var("VST3_HOST_PROBE_PATH");
}
