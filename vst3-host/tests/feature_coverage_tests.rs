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

/// Path to our own bundled test synth (built by `just test-plugin`), or `None` with a note.
/// Unlike Dexed, it implements `INoteExpressionController`, so it can prove note expression.
fn test_synth_path() -> Option<&'static str> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test_plugins/TestSynth.vst3"
    );
    if std::path::Path::new(path).exists() {
        Some(path)
    } else {
        println!("TestSynth.vst3 not found at {path} — run `just test-plugin`, skipping");
        None
    }
}

fn load_test_synth() -> Option<(Vst3Host, Plugin)> {
    let path = test_synth_path()?;
    // Large block so one process_audio call yields enough samples to measure pitch (the host
    // clamps numSamples to the configured block size).
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(4096)
        .build()
        .expect("build host");
    let plugin = host.load_plugin(path).expect("load TestSynth");
    Some((host, plugin))
}

/// Estimate a held voice's fundamental frequency by counting zero-crossings over one block of
/// channel-0 output. Rough but enough to prove a pitch change.
fn measure_freq(plugin: &mut Plugin) -> f64 {
    let frames = 4096; // == the configured block size, so the host doesn't clamp
    let sr = 48000.0;
    let mut buffers = AudioBuffers::new(0, 2, frames, sr);
    plugin.process_audio(&mut buffers).expect("process_audio");
    let ch = &buffers.outputs[0];
    let mut crossings = 0;
    for w in ch.windows(2) {
        if (w[0] <= 0.0 && w[1] > 0.0) || (w[0] >= 0.0 && w[1] < 0.0) {
            crossings += 1;
        }
    }
    // Two zero-crossings per cycle.
    (crossings as f64 / 2.0) / (frames as f64 / sr)
}

/// Note expression (MPE) end-to-end against our own TestSynth: a per-note Tuning expression
/// audibly bends one voice's pitch. Dexed can't demonstrate this (no INoteExpressionController).
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn test_note_expression_bends_pitch() {
    use vst3_host::{NoteExpressionType, NoteId};

    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_test_synth() else {
        return;
    };

    // The plugin advertises a Tuning note-expression.
    let exprs = plugin.note_expressions().expect("note_expressions");
    println!("TestSynth note expressions: {exprs:?}");
    assert!(
        exprs.iter().any(|e| e.kind == NoteExpressionType::Tuning),
        "TestSynth should advertise a Tuning note expression"
    );

    plugin.start_processing().expect("start_processing");
    let id: NoteId = plugin
        .note_on(MidiChannel::Ch1, 60, 100)
        .expect("note_on returns a NoteId");

    // Unbent: note 60 ≈ 261.6 Hz.
    let freq_unbent = measure_freq(&mut plugin);
    // Tuning value 1.0 = max bend = +1 octave in our synth → ≈ 523 Hz.
    plugin
        .send_note_expression(id, NoteExpressionType::Tuning, 1.0)
        .expect("send_note_expression");
    let freq_bent = measure_freq(&mut plugin);

    plugin.note_off(id).ok();
    plugin.stop_processing().ok();

    println!("note-expression pitch: unbent={freq_unbent:.1} Hz, bent={freq_bent:.1} Hz");
    assert!(
        freq_unbent > 200.0 && freq_unbent < 320.0,
        "unbent ~261 Hz, got {freq_unbent}"
    );
    assert!(
        freq_bent > freq_unbent * 1.7,
        "Tuning expression should raise the pitch ~1 octave: {freq_unbent} -> {freq_bent}"
    );
}

/// Same as [`load_test_synth`], but runs the plugin out-of-process (process isolation).
#[cfg(feature = "process-isolation")]
fn load_test_synth_isolated() -> Option<(Vst3Host, Plugin)> {
    let path = test_synth_path()?;
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(4096)
        .with_process_isolation(true)
        .build()
        .expect("build isolated host");
    let plugin = host.load_plugin(path).expect("load TestSynth (isolated)");
    Some((host, plugin))
}

/// Regression for the isolated `set_process_mode` / `reconfigure` gap: an out-of-process
/// plugin must accept an offline-mode switch and a sample-rate / block-size change (both
/// returned `Err("… not supported")` before the fix, because `IsolatedPluginImpl` didn't
/// override them), then still process audio at the new config across the IPC boundary.
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn test_isolated_reconfigure_and_process_mode() {
    let Some((_host, mut plugin)) = load_test_synth_isolated() else {
        return;
    };
    // Both of these returned Err on isolated plugins before the fix.
    plugin
        .set_process_mode(ProcessMode::Offline)
        .expect("isolated set_process_mode(Offline)");
    plugin
        .reconfigure(44100.0, 512)
        .expect("isolated reconfigure(44100, 512)");

    // The plugin still works at the new config: a held note produces audio.
    plugin.start_processing().expect("start_processing");
    let _id = plugin.note_on(MidiChannel::Ch1, 60, 100).expect("note_on");
    let mut buffers = AudioBuffers::new(0, 2, 512, 44100.0);
    plugin.process_audio(&mut buffers).expect("process_audio");
    let peak = buffers.outputs[0]
        .iter()
        .fold(0.0f32, |m, &s| m.max(s.abs()));
    plugin.stop_processing().ok();
    assert!(
        peak > 0.0,
        "reconfigured isolated plugin should produce audio, peak={peak}"
    );
}

/// Note expression (MPE) end-to-end across the *process-isolation* boundary: the helper owns
/// the real plugin (and allocates the NoteId), and a per-note Tuning expression marshaled over
/// IPC still audibly bends the voice's pitch. Mirrors [`test_note_expression_bends_pitch`].
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin) and the helper binary"]
fn test_note_expression_bends_pitch_isolated() {
    use vst3_host::{NoteExpressionType, NoteId};

    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_test_synth_isolated() else {
        return;
    };

    // note_expressions() must now marshal across IPC (previously "not supported").
    let exprs = plugin
        .note_expressions()
        .expect("note_expressions over IPC");
    println!("TestSynth (isolated) note expressions: {exprs:?}");
    assert!(
        exprs.iter().any(|e| e.kind == NoteExpressionType::Tuning),
        "isolated TestSynth should advertise a Tuning note expression"
    );

    plugin.start_processing().expect("start_processing");
    // note_on over IPC: the helper allocates the NoteId and returns it across the boundary.
    let id: NoteId = plugin
        .note_on(MidiChannel::Ch1, 60, 100)
        .expect("note_on over IPC returns a NoteId");

    let freq_unbent = measure_freq(&mut plugin);
    // Tuning 1.0 = +1 octave in our synth; the expression event must survive the marshal.
    plugin
        .send_note_expression(id, NoteExpressionType::Tuning, 1.0)
        .expect("send_note_expression over IPC");
    let freq_bent = measure_freq(&mut plugin);

    plugin.note_off(id).ok();
    plugin.stop_processing().ok();

    println!("isolated note-expression pitch: unbent={freq_unbent:.1} Hz, bent={freq_bent:.1} Hz");
    assert!(
        freq_unbent > 200.0 && freq_unbent < 320.0,
        "unbent ~261 Hz, got {freq_unbent}"
    );
    assert!(
        freq_bent > freq_unbent * 1.7,
        "Tuning expression should raise the pitch ~1 octave across IPC: {freq_unbent} -> {freq_bent}"
    );
}

/// Same as [`load_dexed`], but runs the plugin out-of-process (process isolation).
#[cfg(feature = "process-isolation")]
fn load_dexed_isolated() -> Option<(Vst3Host, Plugin)> {
    let path = dexed_path()?;
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .with_process_isolation(true)
        .build()
        .expect("build isolated host");
    let plugin = host.load_plugin(path).expect("load Dexed (isolated)");
    Some((host, plugin))
}

/// Program selection through `IUnitInfo` against Dexed (32 cartridge programs on the root
/// unit). Selecting a valid program succeeds; out-of-range and unknown-unit selections are
/// rejected; and a `ProgramChange` MIDI event is honored (routed to the root unit).
#[test]
#[ignore = "Requires the bundled Dexed plugin"]
fn test_program_selection() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };

    let units = plugin.get_units().expect("get_units");
    let Some(unit) = units.iter().find(|u| !u.programs.is_empty()).cloned() else {
        println!("Plugin exposes no program list, skipping");
        return;
    };
    let count = unit.programs.len();
    println!("Unit {} has {count} programs", unit.id);
    assert!(count >= 2, "expected a multi-program unit");

    plugin
        .select_program(unit.id, (count - 1) as i32)
        .expect("select last program");
    plugin
        .select_program(unit.id, 0)
        .expect("select first program");

    assert!(
        plugin.select_program(unit.id, count as i32).is_err(),
        "an out-of-range index must be rejected"
    );
    assert!(plugin.select_program(unit.id, -1).is_err());
    assert!(
        plugin.select_program(9999, 0).is_err(),
        "an unknown unit must be rejected"
    );

    plugin
        .send_midi_event(MidiEvent::ProgramChange {
            channel: MidiChannel::Ch1,
            program: 0,
        })
        .expect("ProgramChange should be honored (routed to the root unit)");
}

/// Program selection across the *process-isolation* boundary: the helper owns Dexed, and
/// `select_program` marshaled over IPC switches a program (and rejects a bad index).
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the bundled Dexed plugin and the helper binary"]
fn test_program_selection_isolated() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed_isolated() else {
        return;
    };

    // get_units now marshals across isolation (previously fell back to a guessed root unit).
    let units = plugin.get_units().expect("get_units over IPC");
    let unit = units
        .iter()
        .find(|u| !u.programs.is_empty())
        .expect("Dexed should report a unit with a program list, even over IPC");
    let (unit_id, count) = (unit.id, unit.programs.len() as i32);
    println!("isolated unit {unit_id} reports {count} programs");
    assert!(count >= 2, "expected a multi-program unit");

    plugin
        .select_program(unit_id, 1)
        .expect("select_program over IPC");
    assert!(
        plugin.select_program(unit_id, count + 1000).is_err(),
        "an out-of-range index must be rejected across IPC"
    );
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

/// Sample-accurate MIDI (`send_midi_event_at`): a note scheduled at a non-zero sample
/// offset must leave the block's leading samples silent and only start sounding at the
/// offset. We prove this differentially — the *same* note at offset 0 fills the leading
/// window with audio, while at offset 256 that window stays (near) silent.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_sample_accurate_midi_offset_delays_onset() {
    let _guard = plugin_guard();

    const BLOCK: usize = 512;
    const OFFSET: i32 = 256;
    // Measure peak amplitude in the block's leading window (before the offset).
    const WINDOW: usize = 200;

    // Render one 512-frame block with a note-on scheduled at `offset`, returning the
    // peak amplitude in [0, WINDOW) and in [OFFSET, BLOCK).
    fn render_onset(offset: i32) -> Option<(f32, f32)> {
        let (_host, mut plugin) = load_dexed()?;
        plugin.start_processing().expect("start_processing");
        let note = MidiEvent::NoteOn {
            channel: MidiChannel::Ch1,
            note: 60,
            velocity: 110,
        };
        plugin
            .send_midi_event_at(note, offset)
            .expect("send_midi_event_at");

        let mut buffers = AudioBuffers::new(0, 2, BLOCK, 48000.0);
        plugin.process_audio(&mut buffers).expect("process_audio");

        let peak = |range: std::ops::Range<usize>| {
            buffers
                .outputs
                .iter()
                .flat_map(|ch| ch[range.clone()].iter())
                .fold(0.0f32, |m, &s| m.max(s.abs()))
        };
        let early = peak(0..WINDOW);
        let late = peak(OFFSET as usize..BLOCK);
        plugin.stop_processing().ok();
        Some((early, late))
    }

    // Control: offset 0 — audio is present from the very first sample.
    let Some((early_at_0, _)) = render_onset(0) else {
        return;
    };
    // Test: offset 256 — the leading window must be (near) silent, audio starts later.
    let (early_at_256, late_at_256) = render_onset(OFFSET).expect("second load");

    println!(
        "offset0 early-peak={early_at_0:.5}  offset256 early-peak={early_at_256:.5} late-peak={late_at_256:.5}"
    );

    // The control must actually make sound in the leading window (else the test proves nothing).
    assert!(
        early_at_0 > 1e-4,
        "control (offset 0) produced no audio in the leading window (peak {early_at_0})"
    );
    // The scheduled note must produce audio somewhere in the block.
    assert!(
        late_at_256 > 1e-4,
        "offset-256 note produced no audio at all (late peak {late_at_256})"
    );
    // The heart of the test: scheduling at offset 256 keeps the leading window quiet —
    // dramatically quieter than the offset-0 control over the same samples.
    assert!(
        early_at_256 < early_at_0 * 0.1,
        "offset did not delay onset: leading-window peak {early_at_256} (offset 256) \
         vs {early_at_0} (offset 0) — expected the offset window to be near-silent"
    );
}

/// `IMidiMapping`: querying CC→parameter assignments returns valid parameter ids (when the
/// plugin implements the interface) and never panics. Mappings are stable across repeat calls.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_midi_cc_to_parameter_mapping() {
    let _guard = plugin_guard();
    let Some((_host, plugin)) = load_dexed() else {
        return;
    };

    // Collect the set of valid parameter ids to validate any returned mapping against.
    let param_ids: std::collections::HashSet<u32> = plugin
        .get_parameters()
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();

    // Sweep the standard MIDI CCs plus the VST3 specials (modwheel, aftertouch, pitch-bend…)
    // on bus 0, channel 0.
    let mut mappings = Vec::new();
    for cc in 0u16..=129 {
        if let Some(id) = plugin.midi_cc_to_parameter(0, 0, cc) {
            mappings.push((cc, id));
            assert!(
                param_ids.contains(&id),
                "CC {cc} mapped to id {id}, which is not a real parameter"
            );
        }
    }

    println!(
        "Dexed reported {} CC→param mappings: {mappings:?}",
        mappings.len()
    );

    // The query must be deterministic: a second pass returns identical results.
    for &(cc, id) in &mappings {
        assert_eq!(
            plugin.midi_cc_to_parameter(0, 0, cc),
            Some(id),
            "CC {cc} mapping changed between calls"
        );
    }

    // Dexed implements IMidiMapping (modulation, breath, foot, pitch-bend → DX7 controllers),
    // so we expect at least one mapping. (If a future test plugin doesn't implement it this
    // would need relaxing — see the printed count above.)
    assert!(
        !mappings.is_empty(),
        "expected at least one CC→param mapping from Dexed"
    );

    // Out-of-range controller numbers (beyond the VST3 0..=129 range) are rejected with None,
    // not forwarded as a meaningless controller.
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, 200), None);
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, u16::MAX), None);
}

/// `midi_cc_to_parameter` across the *process-isolation* boundary: the same CC→param
/// mappings Dexed reports in-process must survive the IPC round trip. Mirrors
/// [`test_midi_cc_to_parameter_mapping`].
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the bundled Dexed plugin and the helper binary"]
fn test_midi_cc_to_parameter_mapping_isolated() {
    let _guard = plugin_guard();
    let Some((_host, plugin)) = load_dexed_isolated() else {
        return;
    };

    let param_ids: std::collections::HashSet<u32> = plugin
        .get_parameters()
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();

    let mut mappings = Vec::new();
    for cc in 0u16..=129 {
        if let Some(id) = plugin.midi_cc_to_parameter(0, 0, cc) {
            mappings.push((cc, id));
            assert!(
                param_ids.contains(&id),
                "CC {cc} mapped to id {id} over IPC, which is not a real parameter"
            );
        }
    }
    println!(
        "Dexed (isolated) reported {} CC→param mappings: {mappings:?}",
        mappings.len()
    );
    assert!(
        !mappings.is_empty(),
        "expected at least one CC→param mapping from Dexed over IPC"
    );

    assert_eq!(plugin.midi_cc_to_parameter(0, 0, 200), None);
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, u16::MAX), None);
}

/// Mirrors the inspector's "Export WAV" path (roadmap 4.5): snapshot a live plugin's state,
/// load a fresh instance, restore the state, and offline-render to a non-silent WAV via the
/// library's `render_to_wav`. Exercised here against Dexed (single-instance: the first is
/// dropped before the second loads).
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_export_render_with_state_roundtrip() {
    use vst3_host::midi::{MidiChannel, MidiEvent};

    let _guard = plugin_guard();
    let Some(path) = dexed_path() else {
        return;
    };

    // First instance: tweak a parameter, snapshot state, then drop it.
    let state = {
        let mut host = Vst3Host::builder()
            .sample_rate(48000.0)
            .block_size(512)
            .build()
            .expect("build host");
        let mut plugin = host.load_plugin(path).expect("load Dexed");
        let param = pick_writable_param(&mut plugin);
        plugin.set_parameter(param.id, 0.42).expect("set param");
        plugin.save_state().expect("save_state")
    };

    // Fresh instance: restore the state and render offline to a WAV.
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .expect("build host 2");
    let mut plugin = host.load_plugin(path).expect("reload Dexed");
    plugin.load_state(&state).expect("load_state");

    let out = std::env::temp_dir().join(format!("vst3-host-export-{}.wav", std::process::id()));
    let note = MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 110,
    };
    vst3_host::simple::render_to_wav(&mut plugin, 1.0, &[note], &out).expect("render_to_wav");

    // The file exists and is a non-trivial WAV (44-byte header + a second of stereo f32).
    let bytes = std::fs::read(&out).expect("read exported wav");
    let _ = std::fs::remove_file(&out);
    assert_eq!(&bytes[0..4], b"RIFF", "not a RIFF/WAV file");
    assert!(
        bytes.len() > 44 + 48000 * 2 * 4 / 2,
        "exported WAV is implausibly small: {} bytes",
        bytes.len()
    );

    // Confirm the audio isn't pure silence: scan the f32 sample data for a non-zero.
    let any_nonzero = bytes[44..]
        .chunks_exact(4)
        .any(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]).abs() > 1e-4);
    assert!(any_nonzero, "exported WAV is silent");
}

/// Offline render-with-input: feed a sine test signal into the plugin while rendering to WAV.
/// (Dexed is an instrument and ignores audio input, so this verifies the input plumbing +
/// render path, not effect output — a bundled effect would be needed for that.)
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_render_to_wav_with_input_signal() {
    use vst3_host::SignalSource;

    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    let mut source = SignalSource::sine(440.0, 0.5);
    let out = std::env::temp_dir().join(format!("vh_render_in_{}.wav", std::process::id()));
    let note = MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 100,
    };
    vst3_host::simple::render_to_wav_with_input(&mut plugin, 0.5, &[note], &mut source, &out)
        .expect("render_to_wav_with_input");
    let bytes = std::fs::read(&out).expect("read wav");
    let _ = std::fs::remove_file(&out);
    assert_eq!(&bytes[0..4], b"RIFF");
    assert!(bytes.len() > 44, "rendered WAV has no audio data");
}

/// Bus arrangements: query reports Dexed's stereo output, a stereo re-request is accepted, and
/// a request the plugin may refuse leaves the reported arrangement consistent with the channel
/// count (a stereo instrument can prove query + graceful negotiation, not a real layout switch).
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_bus_arrangements() {
    use vst3_host::SpeakerArrangement;

    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };

    // Dexed is an instrument: no audio inputs, one stereo output bus.
    let arr = plugin.bus_arrangements().expect("bus_arrangements");
    println!(
        "Dexed buses: inputs={:?} outputs={:?}",
        arr.inputs, arr.outputs
    );
    assert!(
        arr.inputs.is_empty(),
        "instrument should have no audio inputs"
    );
    assert_eq!(arr.outputs.len(), 1, "expected one output bus");
    assert_eq!(arr.outputs[0], SpeakerArrangement::STEREO);

    // Re-requesting the current (stereo) layout must succeed and stay stereo.
    plugin
        .set_bus_arrangements(&[], &[SpeakerArrangement::STEREO])
        .expect("set stereo");
    assert_eq!(
        plugin.bus_arrangements().expect("re-query").outputs[0],
        SpeakerArrangement::STEREO
    );

    // Requesting mono: the plugin may accept or refuse, but must not error the host, and the
    // reported arrangement must stay consistent with the host's channel count.
    plugin
        .set_bus_arrangements(&[], &[SpeakerArrangement::MONO])
        .expect("mono request should not error the host");
    let after = plugin.bus_arrangements().expect("re-query after mono");
    assert_eq!(
        after.outputs[0].channel_count(),
        plugin.output_channel_count(),
        "reported arrangement and channel count must stay consistent"
    );
}

/// Bus arrangements across the *process-isolation* boundary: query and negotiation both
/// marshal over IPC. Mirrors [`test_bus_arrangements`].
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the bundled Dexed plugin and the helper binary"]
fn test_bus_arrangements_isolated() {
    use vst3_host::SpeakerArrangement;

    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed_isolated() else {
        return;
    };

    let arr = plugin
        .bus_arrangements()
        .expect("bus_arrangements over IPC");
    println!(
        "Dexed (isolated) buses: inputs={:?} outputs={:?}",
        arr.inputs, arr.outputs
    );
    assert!(arr.inputs.is_empty());
    assert_eq!(arr.outputs.len(), 1);
    assert_eq!(arr.outputs[0], SpeakerArrangement::STEREO);

    plugin
        .set_bus_arrangements(&[], &[SpeakerArrangement::STEREO])
        .expect("set stereo over IPC");
    assert_eq!(
        plugin
            .bus_arrangements()
            .expect("re-query over IPC")
            .outputs[0],
        SpeakerArrangement::STEREO
    );
}

/// Offline process mode: `set_process_mode(Offline)` is rejected while processing, accepted
/// while stopped, and a held note still renders audio in offline mode.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_offline_process_mode() {
    use vst3_host::ProcessMode;

    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };

    // Rejected while processing.
    plugin.start_processing().expect("start_processing");
    assert!(
        plugin.set_process_mode(ProcessMode::Offline).is_err(),
        "set_process_mode while processing must error"
    );
    plugin.stop_processing().ok();

    // Accepted while stopped.
    plugin
        .set_process_mode(ProcessMode::Offline)
        .expect("set Offline while stopped");

    // Still renders audio in offline mode.
    plugin.start_processing().expect("restart processing");
    plugin
        .send_midi_note(60, 110, MidiChannel::Ch1)
        .expect("note on");
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
    plugin.send_midi_note_off(60, MidiChannel::Ch1).ok();
    plugin.stop_processing().ok();

    // Back to realtime works too.
    plugin
        .set_process_mode(ProcessMode::Realtime)
        .expect("set Realtime while stopped");

    println!("offline-mode peak: {peak:.4}");
    assert!(peak > 0.0, "offline mode produced no audio (peak {peak})");
}

/// Runtime reconfigure: after `reconfigure(44100, 256)` the plugin reports the new settings,
/// still produces audio for a held note at the new rate, and refuses to reconfigure while
/// processing.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_runtime_reconfigure_changes_settings() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };

    // Starts at the host-configured 48 kHz / 512.
    assert_eq!(plugin.block_size(), 512);
    assert!((plugin.sample_rate() - 48000.0).abs() < 1.0);

    // Reconfiguring while processing is rejected.
    plugin.start_processing().expect("start_processing");
    let err = plugin.reconfigure(44100.0, 256);
    assert!(
        err.is_err(),
        "reconfigure while processing must error, got {err:?}"
    );
    plugin.stop_processing().ok();

    // Now reconfigure to a new sample rate and block size.
    plugin
        .reconfigure(44100.0, 256)
        .expect("reconfigure to 44100/256");
    assert_eq!(plugin.block_size(), 256);
    assert!((plugin.sample_rate() - 44100.0).abs() < 1.0);

    // The plugin still produces audio at the new configuration.
    plugin.start_processing().expect("restart processing");
    plugin
        .send_midi_note(60, 110, MidiChannel::Ch1)
        .expect("note on");
    let mut peak = 0.0f32;
    for _ in 0..40 {
        let mut buffers = AudioBuffers::new(0, 2, 256, 44100.0);
        plugin.process_audio(&mut buffers).expect("process_audio");
        assert_eq!(buffers.outputs[0].len(), 256);
        for ch in &buffers.outputs {
            for &s in ch {
                peak = peak.max(s.abs());
            }
        }
    }
    plugin.send_midi_note_off(60, MidiChannel::Ch1).ok();
    plugin.stop_processing().ok();

    println!("post-reconfigure peak: {peak:.4}");
    assert!(peak > 0.0, "no audio after reconfigure (peak {peak})");

    // Invalid arguments are rejected.
    assert!(plugin.reconfigure(0.0, 256).is_err(), "zero sample rate");
    assert!(plugin.reconfigure(44100.0, 0).is_err(), "zero block size");
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

/// `process_audio` before `start_processing` must error, not panic or produce garbage.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_process_before_start_errors() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    let mut buffers = AudioBuffers::new(0, 2, 512, 48000.0);
    assert!(
        plugin.process_audio(&mut buffers).is_err(),
        "process_audio before start_processing should return Err"
    );
}

/// A block larger than the configured maximum must be clamped, not crash/overflow.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_oversized_block_is_clamped() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    plugin.start_processing().expect("start_processing");
    // Host was built with block_size 512; ask it to process 1024 frames.
    let mut buffers = AudioBuffers::new(0, 2, 1024, 48000.0);
    plugin
        .process_audio(&mut buffers)
        .expect("oversized block should be clamped and processed, not error");
}

/// A `.vstpreset` whose embedded class id doesn't match the loaded plugin is rejected.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_vstpreset_wrong_class_id_rejected() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    let dir = std::env::temp_dir();
    let path = dir.join("vh_wrong_classid.vstpreset");
    plugin.save_vstpreset(&path).expect("save_vstpreset");

    // The 32-char ASCII class id lives at bytes 8..40 of the container; corrupt it.
    let mut bytes = std::fs::read(&path).expect("read preset");
    for b in &mut bytes[8..40] {
        *b = b'0';
    }
    std::fs::write(&path, &bytes).expect("write tampered preset");

    let result = plugin.load_vstpreset(&path);
    let _ = std::fs::remove_file(&path);
    assert!(
        result.is_err(),
        "load_vstpreset should reject a preset whose class id differs from the plugin"
    );
}

/// `get_units` enumerates IUnitInfo units/program lists without error (a plugin without
/// IUnitInfo returns an empty list; one with it returns at least the root unit).
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_get_units_enumerates() {
    let _guard = plugin_guard();
    let Some((_host, plugin)) = load_dexed() else {
        return;
    };
    let units = plugin.get_units().expect("get_units should not error");
    println!("Dexed reports {} unit(s)", units.len());
    for u in &units {
        println!(
            "  unit {} (parent {}): '{}', {} program(s)",
            u.id,
            u.parent_id,
            u.name,
            u.programs.len()
        );
        // Program names, when present, must be readable strings (not garbage/empty-only).
        for (i, p) in u.programs.iter().take(3).enumerate() {
            println!("    program[{i}] = '{p}'");
        }
    }
    // Unit ids should be internally consistent. (Names are NOT asserted non-empty: VST3 does
    // not guarantee a unit name — the root unit in particular is often unnamed.)
    let ids: std::collections::HashSet<i32> = units.iter().map(|u| u.id).collect();
    assert_eq!(ids.len(), units.len(), "duplicate unit ids reported");
}

/// `get_units` across the *process-isolation* boundary: unit/program-list enumeration
/// marshals over IPC. Mirrors [`test_get_units_enumerates`].
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the bundled Dexed plugin and the helper binary"]
fn test_get_units_enumerates_isolated() {
    let _guard = plugin_guard();
    let Some((_host, plugin)) = load_dexed_isolated() else {
        return;
    };
    let units = plugin.get_units().expect("get_units over IPC");
    println!("Dexed (isolated) reports {} unit(s)", units.len());
    assert!(
        !units.is_empty(),
        "Dexed implements IUnitInfo, expected at least the root unit over IPC"
    );
    let ids: std::collections::HashSet<i32> = units.iter().map(|u| u.id).collect();
    assert_eq!(ids.len(), units.len(), "duplicate unit ids reported");
}

/// Offline render-to-WAV: bounce a held note from Dexed to a WAV file and verify the file
/// has a valid float-WAV header and non-silent audio.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_render_to_wav_produces_audio() {
    use vst3_host::midi::{MidiChannel, MidiEvent};
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_dexed() else {
        return;
    };
    let path = std::env::temp_dir().join("vh_render_test.wav");
    let note = MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 110,
    };
    vst3_host::simple::render_to_wav(&mut plugin, 0.5, &[note], &path).expect("render_to_wav");

    let bytes = std::fs::read(&path).expect("read rendered wav");
    let _ = std::fs::remove_file(&path);
    // Header: RIFF/WAVE, IEEE float, 48 kHz (the load_dexed sample rate).
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 3, "IEEE float");
    let sr = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    assert_eq!(sr, 48_000);
    // 0.5 s of stereo float at 48 kHz ~ 192 KB of data; assert a substantial file.
    assert!(
        bytes.len() > 100_000,
        "rendered wav too small: {}",
        bytes.len()
    );
    // Scan the float samples for non-silence.
    let mut peak = 0.0f32;
    for chunk in bytes[44..].chunks_exact(4) {
        let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        peak = peak.max(s.abs());
    }
    assert!(peak > 0.0, "rendered wav is silent");
    println!("rendered {} bytes, peak {peak:.3}", bytes.len());
}

/// Offline effect-input path: feed a loud signal through an effect plugin and confirm the
/// input reaches the plugin's DSP — output with signal must exceed output with silence.
/// This verifies the audio-input plumbing (used by `play_with_input`) without a live device.
#[test]
#[ignore = "Requires an effect VST3 (Surge XT Effects / Valhalla) installed"]
fn test_effect_processes_audio_input() {
    let _guard = plugin_guard();
    // Effects are free system plugins; load by known path (not discovery) to stay safe.
    let candidates = [
        "/Library/Audio/Plug-Ins/VST3/Surge XT Effects.vst3",
        "/Library/Audio/Plug-Ins/VST3/ValhallaSupermassive.vst3",
    ];
    let Some(path) = candidates
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .copied()
    else {
        println!("No effect plugin installed, skipping");
        return;
    };

    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .expect("build host");
    let mut plugin = host.load_plugin(path).expect("load effect");
    assert!(
        plugin.info().audio_inputs > 0,
        "expected an effect with audio inputs"
    );
    plugin.start_processing().expect("start_processing");

    // Measure output peak over many blocks for: silence input, then loud-sine input.
    fn run(plugin: &mut Plugin, amp: f32) -> f32 {
        let mut phase = 0.0f32;
        let mut peak = 0.0f32;
        for _ in 0..60 {
            let mut buf = AudioBuffers::new(2, 2, 512, 48000.0);
            for frame in 0..512 {
                let s = amp * (phase).sin();
                phase += 2.0 * std::f32::consts::PI * 220.0 / 48000.0;
                for ch in buf.inputs.iter_mut() {
                    ch[frame] = s;
                }
            }
            plugin.process_audio(&mut buf).expect("process_audio");
            for ch in &buf.outputs {
                for &s in ch {
                    peak = peak.max(s.abs());
                }
            }
        }
        peak
    }

    let silent_peak = run(&mut plugin, 0.0);
    let signal_peak = run(&mut plugin, 0.5);
    plugin.stop_processing().ok();

    println!("effect output peak: silence={silent_peak:.4}, signal={signal_peak:.4}");
    assert!(
        signal_peak > silent_peak + 0.001,
        "effect output did not respond to input (silence {silent_peak}, signal {signal_peak}) \
         — audio-input path not delivering"
    );
}

/// Latency/tail accessors return the plugin's reported values without error.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn test_latency_and_tail_accessors() {
    let _guard = plugin_guard();
    let Some((_host, plugin)) = load_dexed() else {
        return;
    };
    let latency = plugin.latency_samples();
    let tail = plugin.tail_samples();
    println!("Dexed latency={latency} samples, tail={tail} samples");
    // Sanity: a synth's latency is small; we just assert the calls work and are bounded.
    assert!(latency < 1_000_000, "implausible latency {latency}");
}

/// Latency/tail accessors across the *process-isolation* boundary: both round-trip over IPC
/// instead of silently reporting 0. Mirrors [`test_latency_and_tail_accessors`].
#[cfg(feature = "process-isolation")]
#[test]
#[ignore = "Requires the bundled Dexed plugin and the helper binary"]
fn test_latency_and_tail_accessors_isolated() {
    let _guard = plugin_guard();
    let Some((_host, plugin)) = load_dexed_isolated() else {
        return;
    };
    let latency = plugin.latency_samples();
    let tail = plugin.tail_samples();
    println!("Dexed (isolated) latency={latency} samples, tail={tail} samples");
    assert!(latency < 1_000_000, "implausible latency {latency}");
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

// ---------------------------------------------------------------------------
// TestSynth capability coverage: these run the library's state, program, MIDI-mapping and
// sample-accurate-event paths against the in-repo synth, so they need no third-party plugin.
// ---------------------------------------------------------------------------

/// State persistence: parameter values survive a save_state → fresh instance → load_state trip.
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn test_testsynth_state_roundtrip() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_test_synth() else {
        return;
    };
    plugin.set_parameter(0, 0.33).expect("set cutoff"); // Cutoff
    plugin.set_parameter(2, 0.77).expect("set detune"); // Detune
    plugin.set_parameter(7, 0.41).expect("set sustain"); // Amp Sustain
                                                         // Parameter changes reach the processor as part of process(), so run one block before
                                                         // asking the processor to serialize its state.
    plugin.start_processing().expect("start_processing");
    let mut buffers = AudioBuffers::new(0, 2, 512, 48000.0);
    plugin.process_audio(&mut buffers).expect("process_audio");
    plugin.stop_processing().ok();
    let state = plugin.save_state().expect("save_state");
    assert!(!state.is_empty(), "saved state is empty");
    drop(plugin);

    let (_host2, mut restored) = load_test_synth().expect("second load");
    restored.load_state(&state).expect("load_state");
    for (id, expected) in [(0u32, 0.33), (2, 0.77), (7, 0.41)] {
        let got = restored.get_parameter(id).expect("get_parameter");
        assert!(
            (got - expected).abs() < 1e-9,
            "param {id} did not survive the state round-trip: got {got}, expected {expected}"
        );
    }
}

/// Factory programs: IUnitInfo exposes the list, and select_program loads the preset.
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn test_testsynth_program_selection() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_test_synth() else {
        return;
    };
    let units = plugin.get_units().expect("get_units");
    let root = units
        .iter()
        .find(|u| !u.programs.is_empty())
        .expect("TestSynth should expose a unit with programs");
    assert_eq!(
        root.programs,
        vec!["Init Sine", "Trance Pluck", "Super Lead", "Lush Pad"],
        "unexpected factory program list"
    );

    plugin
        .select_program(root.id, 1)
        .expect("select Trance Pluck");
    // The Trance Pluck preset sets Cutoff (#0) to 0.42 and Waveform (#1) to Super Saw (1.0).
    let cutoff = plugin.get_parameter(0).expect("get cutoff");
    let waveform = plugin.get_parameter(1).expect("get waveform");
    assert!(
        (cutoff - 0.42).abs() < 1e-9,
        "program change did not load preset cutoff (got {cutoff})"
    );
    assert!(
        (waveform - 1.0).abs() < 1e-9,
        "program change did not load preset waveform (got {waveform})"
    );
}

/// Sample-accurate events: a note scheduled at offset 256 keeps the block's leading window
/// silent (same differential design as the Dexed variant of this test).
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn test_testsynth_sample_accurate_offset() {
    let _guard = plugin_guard();

    const BLOCK: usize = 512;
    const OFFSET: i32 = 256;
    const WINDOW: usize = 200;

    fn render_onset(offset: i32) -> Option<(f32, f32)> {
        let path = test_synth_path()?;
        let mut host = Vst3Host::builder()
            .sample_rate(48000.0)
            .block_size(BLOCK)
            .build()
            .expect("build host");
        let mut plugin = host.load_plugin(path).expect("load TestSynth");
        plugin.start_processing().expect("start_processing");
        let note = MidiEvent::NoteOn {
            channel: MidiChannel::Ch1,
            note: 60,
            velocity: 110,
        };
        plugin
            .send_midi_event_at(note, offset)
            .expect("send at offset");
        let mut buffers = AudioBuffers::new(0, 2, BLOCK, 48000.0);
        plugin.process_audio(&mut buffers).expect("process_audio");
        let peak = |range: std::ops::Range<usize>| {
            buffers
                .outputs
                .iter()
                .flat_map(|ch| ch[range.clone()].iter())
                .fold(0.0f32, |m, &s| m.max(s.abs()))
        };
        Some((peak(0..WINDOW), peak(OFFSET as usize..BLOCK)))
    }

    let Some((early_at_0, _)) = render_onset(0) else {
        return;
    };
    let (early_at_256, late_at_256) = render_onset(OFFSET).expect("second load");
    println!(
        "TestSynth offset0 early={early_at_0:.5} offset256 early={early_at_256:.5} late={late_at_256:.5}"
    );
    assert!(
        early_at_0 > 1e-4,
        "control produced no audio in the leading window"
    );
    assert!(late_at_256 > 1e-4, "offset note produced no audio at all");
    assert!(
        early_at_256 < early_at_0 * 0.1,
        "offset did not delay onset: early {early_at_256} vs control {early_at_0}"
    );
}

/// IMidiMapping: the documented CC assignments resolve, unmapped/invalid CCs do not.
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn test_testsynth_midi_cc_mapping() {
    let _guard = plugin_guard();
    let Some((_host, plugin)) = load_test_synth() else {
        return;
    };
    // Mod wheel → Filter Env Amount (#13), GM2 sound controllers → filter/env params.
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, 1), Some(13));
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, 71), Some(4)); // Resonance
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, 72), Some(8)); // Amp Release
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, 73), Some(5)); // Amp Attack
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, 74), Some(0)); // Cutoff
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, 7), None); // volume: unmapped
    assert_eq!(plugin.midi_cc_to_parameter(0, 0, 200), None); // out of range
}

/// The super saw is genuinely stereo: with detune up, left and right differ; the default
/// sine stays mono (identical channels), which older tests rely on.
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn test_testsynth_supersaw_stereo() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_test_synth() else {
        return;
    };
    plugin.start_processing().expect("start_processing");

    let note = MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 57,
        velocity: 100,
    };
    let side_energy = |plugin: &mut Plugin| {
        let mut buffers = AudioBuffers::new(0, 2, 4096, 48000.0);
        plugin.process_audio(&mut buffers).expect("process_audio");
        buffers.outputs[0]
            .iter()
            .zip(buffers.outputs[1].iter())
            .map(|(l, r)| ((l - r) * (l - r)) as f64)
            .sum::<f64>()
    };

    // Control: default sine is mono — channels identical.
    plugin.send_midi_event(note).expect("note on (sine)");
    let sine_side = side_energy(&mut plugin);
    plugin
        .send_midi_event(MidiEvent::NoteOff {
            channel: MidiChannel::Ch1,
            note: 57,
            velocity: 0,
        })
        .expect("note off");
    let _ = side_energy(&mut plugin); // let releases die out

    // Super saw with detune: the panned side oscillators must decorrelate the channels.
    plugin.set_parameter(1, 1.0).expect("waveform → super saw");
    plugin.set_parameter(2, 0.6).expect("detune");
    plugin.send_midi_event(note).expect("note on (supersaw)");
    let saw_side = side_energy(&mut plugin);

    println!("side energy: sine={sine_side:.6}, supersaw={saw_side:.6}");
    assert!(
        sine_side < 1e-9,
        "sine should be mono (side energy {sine_side})"
    );
    assert!(
        saw_side > 1e-4,
        "super saw should be stereo (side energy {saw_side})"
    );
}

/// Multitimbral routing: TestSynth plays MIDI channel 2 with a second timbre gated by its
/// "Ch2 Level" parameter — channel-2 notes are silent at the default level 0 (proving the
/// event's channel actually reaches the plugin) and audible once the part is dialed up.
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn test_testsynth_multitimbral_channels() {
    let _guard = plugin_guard();
    let Some((_host, mut plugin)) = load_test_synth() else {
        return;
    };
    let params = plugin.get_parameters().expect("get_parameters");
    let ch2_level = params
        .iter()
        .find(|p| p.name == "Ch2 Level")
        .expect("Ch2 Level param")
        .id;
    plugin.start_processing().expect("start_processing");

    let peak_of = |plugin: &mut Plugin, channel: MidiChannel| {
        plugin
            .send_midi_event(MidiEvent::NoteOn {
                channel,
                note: 60,
                velocity: 100,
            })
            .expect("note on");
        let mut peak = 0.0f32;
        for _ in 0..4 {
            let mut buffers = AudioBuffers::new(0, 2, 4096, 48000.0);
            plugin.process_audio(&mut buffers).expect("process_audio");
            for ch in &buffers.outputs {
                for &s in ch {
                    peak = peak.max(s.abs());
                }
            }
        }
        plugin
            .send_midi_event(MidiEvent::NoteOff {
                channel,
                note: 60,
                velocity: 0,
            })
            .expect("note off");
        // Drain the release tail so runs don't bleed into each other.
        for _ in 0..4 {
            let mut buffers = AudioBuffers::new(0, 2, 4096, 48000.0);
            plugin.process_audio(&mut buffers).expect("process_audio");
        }
        peak
    };

    let ch1 = peak_of(&mut plugin, MidiChannel::Ch1);
    let ch2_muted = peak_of(&mut plugin, MidiChannel::Ch2);
    plugin.set_parameter(ch2_level, 1.0).expect("set ch2 level");
    let ch2_live = peak_of(&mut plugin, MidiChannel::Ch2);

    println!(
        "multitimbral peaks: ch1={ch1:.4} ch2(level 0)={ch2_muted:.4} ch2(level 1)={ch2_live:.4}"
    );
    assert!(ch1 > 1e-3, "channel 1 should sound with the live params");
    assert!(
        ch2_muted < 1e-5,
        "channel 2 should be silent at Ch2 Level 0 (got {ch2_muted})"
    );
    assert!(
        ch2_live > 1e-3,
        "channel 2 should sound once Ch2 Level is up (got {ch2_live})"
    );
}
