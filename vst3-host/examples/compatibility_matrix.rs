//! Generate a plugin compatibility matrix as Markdown.
//!
//! For each plugin it probes (crash-safe, in an isolated process) whether it loads, then —
//! only if the probe is clean — loads it in-process and checks capabilities: parameters,
//! audio output, state save/restore, MIDI output, and editor GUI. Plugins that crash the
//! probe are reported as such and never loaded in-process, so a bad plugin can't kill the
//! run.
//!
//! Usage:
//!   cargo run -p vst3-host --example compatibility_matrix --features cpal-backend,process-isolation
//!     [-- <plugin.vst3> ...]
//!
//! With no paths it discovers installed plugins. Output is Markdown on stdout; redirect it
//! into `docs/reference/compatibility.md` (a curated version is committed there).

use std::path::Path;
use vst3_host::{midi::MidiChannel, AudioBuffers, ProbeResult, Vst3Host};

/// One capability cell.
enum Cell {
    Yes(String),
    No,
    Na,
}

impl Cell {
    fn md(&self) -> String {
        match self {
            Cell::Yes(s) if s.is_empty() => "✅".to_string(),
            Cell::Yes(s) => format!("✅ {s}"),
            Cell::No => "❌".to_string(),
            Cell::Na => "—".to_string(),
        }
    }
}

struct Row {
    name: String,
    vendor: String,
    version: String,
    category: String,
    load: String,
    params: Cell,
    audio: Cell,
    state: Cell,
    midi_out: Cell,
    gui: Cell,
}

fn test_plugin(path: &Path) -> Row {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    let mut row = Row {
        name,
        vendor: String::new(),
        version: String::new(),
        category: String::new(),
        load: "?".into(),
        params: Cell::Na,
        audio: Cell::Na,
        state: Cell::Na,
        midi_out: Cell::Na,
        gui: Cell::Na,
    };

    // 1) Crash-safe probe in an isolated process.
    let host = match Vst3Host::new() {
        Ok(h) => h,
        Err(e) => {
            row.load = format!("host error: {e}");
            return row;
        }
    };
    match host.probe_plugin(path) {
        ProbeResult::Ok => row.load = "✅ loads".into(),
        ProbeResult::Crashed => {
            row.load = "💥 crashes (contained by isolation)".into();
            return row;
        }
        ProbeResult::TimedOut => {
            row.load = "⏱ timed out".into();
            return row;
        }
        ProbeResult::Failed(e) => {
            row.load = format!("❌ failed: {}", e.lines().next().unwrap_or(""));
            return row;
        }
    }

    // 2) Probe was clean → safe to load in-process and gather capabilities.
    let mut host = match Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            row.load = format!("host error: {e}");
            return row;
        }
    };
    let mut plugin = match host.load_plugin(path) {
        Ok(p) => p,
        Err(e) => {
            row.load = format!("❌ in-process load failed: {e}");
            return row;
        }
    };

    let info = plugin.info().clone();
    row.vendor = info.vendor.clone();
    row.version = info.version.clone();
    row.category = info.category.clone();
    row.midi_out = if info.has_midi_output {
        Cell::Yes(String::new())
    } else {
        Cell::No
    };
    row.gui = if plugin.has_editor() {
        Cell::Yes(String::new())
    } else {
        Cell::No
    };

    // Parameters.
    let params = plugin.get_parameters().unwrap_or_default();
    row.params = if params.is_empty() {
        Cell::No
    } else {
        Cell::Yes(format!("{}", params.len()))
    };

    // State save/restore round-trip on the first automatable parameter.
    row.state = test_state(&mut plugin, &params);

    // Audio: drive a note (instruments) or process silence (effects), measure peak.
    row.audio = test_audio(&mut plugin, info.has_midi_input);

    row
}

fn test_state(plugin: &mut vst3_host::Plugin, params: &[vst3_host::Parameter]) -> Cell {
    let Some(p) = params
        .iter()
        .find(|p| p.can_automate && !p.is_read_only && !p.is_bypass)
    else {
        return Cell::Na;
    };
    let id = p.id;
    if plugin.set_parameter(id, 0.25).is_err() {
        return Cell::Na;
    }
    let Ok(snapshot) = plugin.save_state() else {
        return Cell::No;
    };
    let _ = plugin.set_parameter(id, 0.75);
    if plugin.load_state(&snapshot).is_err() {
        return Cell::No;
    }
    match plugin.get_parameter(id) {
        Ok(v) if (v - 0.25).abs() < 0.05 => Cell::Yes(String::new()),
        _ => Cell::No,
    }
}

fn test_audio(plugin: &mut vst3_host::Plugin, is_instrument: bool) -> Cell {
    if plugin.start_processing().is_err() {
        return Cell::No;
    }
    if is_instrument {
        let _ = plugin.send_midi_note(60, 110, MidiChannel::Ch1);
    }
    let mut peak = 0.0f32;
    for _ in 0..40 {
        let mut buf = AudioBuffers::new(0, 2, 512, 48000.0);
        if plugin.process_audio(&mut buf).is_err() {
            return Cell::No;
        }
        for ch in &buf.outputs {
            for &s in ch {
                peak = peak.max(s.abs());
            }
        }
    }
    let _ = plugin.stop_processing();
    if is_instrument {
        if peak > 0.0 {
            Cell::Yes(format!("peak {peak:.3}"))
        } else {
            // Processed fine but produced silence (e.g. needs a preset).
            Cell::Yes("silent".into())
        }
    } else {
        // Effects fed silence stay silent; "processes" is the meaningful signal.
        Cell::Yes("processes".into())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<std::path::PathBuf> = if args.is_empty() {
        eprintln!("No paths given; discovering installed plugins…");
        Vst3Host::new()
            .map(|h| h.scan_plugin_paths())
            .unwrap_or_default()
    } else {
        args.iter().map(std::path::PathBuf::from).collect()
    };

    eprintln!("Testing {} plugin(s)…", paths.len());
    let mut rows = Vec::new();
    for path in &paths {
        eprintln!("  • {}", path.display());
        rows.push(test_plugin(path));
    }
    rows.sort_by_key(|a| a.name.to_lowercase());

    println!(
        "| Plugin | Vendor | Version | Category | Load | Params | Audio | State | MIDI-out | GUI |"
    );
    println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
    for r in &rows {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.name,
            r.vendor,
            r.version,
            r.category,
            r.load,
            r.params.md(),
            r.audio.md(),
            r.state.md(),
            r.midi_out.md(),
            r.gui.md(),
        );
    }
}
