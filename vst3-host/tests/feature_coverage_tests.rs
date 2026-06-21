//! Broad end-to-end coverage of shipped features against the bundled Dexed plugin.
//!
//! These tests complement `integration_tests.rs` (state/vstpreset round-trips,
//! automation, isolation, realtime) by covering the features that previously had
//! thin coverage: parameter set/get/format round-trips, JSON preset save/load
//! (incl. wrong-uid rejection), transport audio for a held note, variable block
//! sizes, and builder tempo/time-signature config.
//!
//! Plugin-dependent tests are `#[ignore]`d (run with `--ignored`); the pure-logic
//! tests at the bottom run in CI without a plugin.
//!
//! ```
//! cargo test -p vst3-host --test feature_coverage_tests -- --ignored --nocapture
//! ```

#![cfg(feature = "cpal-backend")]

use std::sync::{Mutex, MutexGuard};
use vst3_host::prelude::*;

/// Dexed is a C++ plugin with process-global state that does not tolerate two
/// instances being loaded concurrently in the same process. The default test
/// harness runs tests in parallel, so the plugin-dependent tests below take this
/// lock for their whole body to load/drive Dexed one at a time.
static PLUGIN_LOCK: Mutex<()> = Mutex::new(());

fn plugin_guard() -> MutexGuard<'static, ()> {
    PLUGIN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Path to the bundled test plugin; `None` (with a printed note) if it is missing,
/// so an `#[ignore]`d run on a machine without it degrades gracefully.
fn dexed_path() -> Option<&'static str> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if std::path::Path::new(path).exists() {
        Some(path)
    } else {
        println!("Test plugin not found at {path}, skipping");
        None
    }
}

fn load_dexed() -> Option<(Vst3Host, Plugin)> {
    let path = dexed_path()?;
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .expect("build host");
    let plugin = host.load_plugin(path).expect("load Dexed");
    Some((host, plugin))
}

/// Pick a writable, automatable parameter (falling back to the first one).
fn pick_writable_param(plugin: &mut Plugin) -> Parameter {
    let params = plugin.get_parameters().expect("get_parameters");
    params
        .iter()
        .find(|p| p.can_automate && !p.is_read_only && !p.is_bypass)
        .or_else(|| params.first())
        .expect("plugin has at least one parameter")
        .clone()
}

/// Parameter set/get/format round-trip: set a normalized value, read it back, and
/// confirm `format_parameter` renders a non-empty display string for it.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_parameter_set_get_format_roundtrip() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    let param = pick_writable_param(&mut plugin);
    let id = param.id;

    for &v in &[0.0_f64, 0.25, 0.5, 0.9] {
        plugin.set_parameter(id, v).expect("set_parameter");
        let got = plugin.get_parameter(id).expect("get_parameter");
        assert!(
            (got - v).abs() < 0.05,
            "param '{}' (id {id}) did not round-trip: set {v}, got {got}",
            param.name
        );

        // The plugin's own display string for that value must be non-empty.
        let formatted = plugin.format_parameter(id, v).expect("format_parameter");
        assert!(
            !formatted.is_empty(),
            "format_parameter produced an empty string for {v}"
        );
        println!(
            "param '{}' = {v} -> got {got}, display '{formatted}'",
            param.name
        );
    }
}

/// JSON preset save/load round-trip: save -> change param -> load -> value restored.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_json_preset_save_load_roundtrip() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    let param = pick_writable_param(&mut plugin);
    let id = param.id;

    // Establish a known value, save it to a JSON preset.
    plugin.set_parameter(id, 0.3).expect("set v1");
    let v1 = plugin.get_parameter(id).expect("get v1");

    let mut preset_path = std::env::temp_dir();
    preset_path.push(format!("vst3-host-json-preset-{}.json", std::process::id()));
    plugin.save_preset(&preset_path).expect("save_preset");

    // Move the parameter away, then load the preset back and confirm restoration.
    plugin.set_parameter(id, 0.85).expect("set v2");
    plugin.load_preset(&preset_path).expect("load_preset");
    let v3 = plugin.get_parameter(id).expect("get after restore");

    let _ = std::fs::remove_file(&preset_path);

    println!("json preset round-trip: v1={v1} restored={v3}");
    assert!(
        (v3 - v1).abs() < 0.05,
        "parameter not restored from JSON preset: {v3} (expected ~{v1})"
    );
}

/// A JSON preset whose uid belongs to a different plugin must be rejected.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_json_preset_wrong_uid_rejected() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    let param = pick_writable_param(&mut plugin);
    plugin.set_parameter(param.id, 0.4).expect("set");

    // Save a real preset, then corrupt its uid on disk so it claims another plugin.
    let mut preset_path = std::env::temp_dir();
    preset_path.push(format!(
        "vst3-host-wrong-uid-preset-{}.json",
        std::process::id()
    ));
    plugin.save_preset(&preset_path).expect("save_preset");

    let raw = std::fs::read_to_string(&preset_path).expect("read preset");
    let real_uid = plugin.info().uid.clone();
    let bogus = "deadbeefdeadbeefdeadbeefdeadbeef";
    assert_ne!(real_uid, bogus, "test's bogus uid accidentally matched");
    let tampered = raw
        .replacen(&real_uid, bogus, 1)
        .replacen("Dexed", "SomeOtherPlugin", 1);
    assert_ne!(tampered, raw, "uid was not present to replace");
    std::fs::write(&preset_path, tampered).expect("write tampered preset");

    let err = plugin
        .load_preset(&preset_path)
        .expect_err("loading a wrong-uid preset must fail");
    let _ = std::fs::remove_file(&preset_path);

    println!("wrong-uid preset correctly rejected: {err}");
}

/// Transport: a held note must produce audio (peak > 0) after processing several blocks.
/// This is the offline `process_audio` path, not the device callback, so no hardware.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_held_note_produces_audio() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    plugin.start_processing().expect("start_processing");
    plugin
        .send_midi_note(60, 110, MidiChannel::Ch1)
        .expect("send note on");

    let mut peak = 0.0f32;
    for _ in 0..40 {
        let mut buffers = AudioBuffers::new(0, 2, 512, 48000.0);
        plugin.process_audio(&mut buffers).expect("process_audio");
        for ch in &buffers.outputs {
            for &s in ch {
                peak = peak.max(s.abs());
            }
        }
    }
    plugin
        .send_midi_note_off(60, MidiChannel::Ch1)
        .expect("send note off");
    plugin.stop_processing().ok();

    println!("held-note peak: {peak:.4}");
    assert!(peak > 0.0, "held note produced no audio (peak {peak})");
}

/// Variable block sizes: 64, 128, and 512 frames all process without error and the
/// configured-max path (512) still produces audio for a held note.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_variable_block_sizes_process() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    plugin.start_processing().expect("start_processing");
    plugin
        .send_midi_note(60, 110, MidiChannel::Ch1)
        .expect("send note on");

    for &block in &[64usize, 128, 512] {
        let mut peak = 0.0f32;
        for _ in 0..20 {
            let mut buffers = AudioBuffers::new(0, 2, block, 48000.0);
            plugin
                .process_audio(&mut buffers)
                .unwrap_or_else(|e| panic!("process_audio failed at block {block}: {e}"));
            assert_eq!(
                buffers.outputs[0].len(),
                block,
                "output buffer length changed for block {block}"
            );
            for ch in &buffers.outputs {
                for &s in ch {
                    peak = peak.max(s.abs());
                }
            }
        }
        println!("block {block}: peak {peak:.4}");
        assert!(peak > 0.0, "block size {block} produced no audio");
    }

    plugin
        .send_midi_note_off(60, MidiChannel::Ch1)
        .expect("send note off");
    plugin.stop_processing().ok();
}

/// Builder tempo / time-signature config is recorded in the host config and the
/// plugin still processes audio when those values are set.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_builder_tempo_time_signature_applies() {
    let _guard = plugin_guard();
    let Some(path) = dexed_path() else {
        return;
    };
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .tempo(140.0)
        .time_signature(3, 4)
        .build()
        .expect("build host with tempo/time-sig");

    // The config carries the requested transport settings.
    assert_eq!(host.config().tempo, 140.0);
    assert_eq!(host.config().time_sig_numerator, 3);
    assert_eq!(host.config().time_sig_denominator, 4);

    let mut plugin = host.load_plugin(path).expect("load Dexed");
    plugin.start_processing().expect("start_processing");
    plugin
        .send_midi_note(60, 110, MidiChannel::Ch1)
        .expect("send note on");

    let mut peak = 0.0f32;
    for _ in 0..40 {
        let mut buffers = AudioBuffers::new(0, 2, 512, 48000.0);
        plugin.process_audio(&mut buffers).expect("process_audio");
        for ch in &buffers.outputs {
            for &s in ch {
                peak = peak.max(s.abs());
            }
        }
    }
    plugin
        .send_midi_note_off(60, MidiChannel::Ch1)
        .expect("send note off");
    plugin.stop_processing().ok();

    println!("tempo/time-sig configured (140 bpm, 3/4); held-note peak {peak:.4}");
    assert!(
        peak > 0.0,
        "plugin produced no audio with custom tempo/time signature"
    );
}

// --- Pure-logic tests (run in CI without a plugin) ---------------------------

/// The builder records tempo / time-signature on the config without needing a plugin.
#[test]
fn test_builder_tempo_time_signature_recorded() {
    let host = Vst3Host::builder()
        .tempo(90.0)
        .time_signature(6, 8)
        .build()
        .expect("build host");
    assert_eq!(host.config().tempo, 90.0);
    assert_eq!(host.config().time_sig_numerator, 6);
    assert_eq!(host.config().time_sig_denominator, 8);
}

/// Defaults: tempo 120 bpm, 4/4 time signature.
#[test]
fn test_default_transport_config() {
    let host = Vst3Host::new().expect("build host");
    assert_eq!(host.config().tempo, 120.0);
    assert_eq!(host.config().time_sig_numerator, 4);
    assert_eq!(host.config().time_sig_denominator, 4);
}

/// A PluginPreset serializes and deserializes through serde, preserving uid/name/state.
/// This is the on-disk wire format used by save_preset/load_preset, exercised without
/// touching a plugin.
#[test]
fn test_plugin_preset_serde_roundtrip() {
    use vst3_host::plugin::PluginPreset;

    let preset = PluginPreset {
        uid: "0123456789abcdef0123456789abcdef".to_string(),
        plugin_name: "TestSynth".to_string(),
        state: vec![1, 2, 3, 4, 250, 0, 99],
    };
    let json = serde_json::to_vec_pretty(&preset).expect("serialize");
    let back: PluginPreset = serde_json::from_slice(&json).expect("deserialize");

    assert_eq!(back.uid, preset.uid);
    assert_eq!(back.plugin_name, preset.plugin_name);
    assert_eq!(back.state, preset.state);
}
