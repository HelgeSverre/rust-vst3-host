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
