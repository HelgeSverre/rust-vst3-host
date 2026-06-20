# Changelog

All notable changes to `vst3-host` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project aims to follow
[Semantic Versioning](https://semver.org/).

## [0.1.1] - 2026-06-20

### Changed
- Relicensed to **MIT only** (was `MIT OR Apache-2.0`). A single `LICENSE` file now sits at
  the repo root and in the crate.
- Expanded the README with a full set of examples (discovery, parameters, state, isolation,
  realtime) and a complete end-to-end "putting it all together" walkthrough.

## [0.1.0] - 2026-06-20

First usable release of the safe VST3 hosting library, extracted from the inspector app.

### Added
- Lock-free real-time audio path alongside the mutex-based `play`: `RealtimePluginRunner` +
  `RtControl` (lock-free SPSC command queue), wired to CPAL via `Vst3Host::play_realtime` /
  `play_realtime_with_backend`.
- Editor embedding into an egui/eframe window (`EmbeddedEditor`, `egui-widgets` feature,
  macOS), plus `IPlugFrame` so plugins can request host resizes
  (`Plugin::take_editor_resize_request`). macOS window creation migrated to `objc2`.
- Accurate plugin metadata — real version + sub-categories (`getClassInfo2`) and MIDI in/out
  from event-bus presence; bus-aware output channel count (`Plugin::output_channel_count`).
- JSON export: serializable introspection types + `PluginReport { detailed, parameters }`
  with `to_json()` (the inspector's "Copy JSON").
- Compatibility-matrix harness (`just compat` / `examples/compatibility_matrix.rs`) and
  `docs/reference/compatibility.md`.
- macOS loader diagnoses architecture mismatch (e.g. an x86_64-only plugin on an arm64 host)
  with an actionable error instead of a bare failure.
- Cross-platform CI (macOS / Linux / Windows) on every push.
- Safe plugin loading and discovery (`Vst3Host`, `simple::load_plugin`, `discover_plugins`,
  `scan_plugin_paths`, `get_detailed_plugin_info`).
- Batteries-included audio: `Vst3Host::play` / `simple::play` drive a plugin through the
  bundled `CpalBackend`; `play_with_backend` for custom backends; `AudioHandle` to control
  a plugin while it plays.
- Parameters (`get`/`set`/`format_parameter`), MIDI in (`send_midi_*`, pitch bend,
  aftertouch) and MIDI out capture (`take_output_midi`).
- Plugin state save/restore (`Plugin::save_state` / `load_state`) via the plugin's own
  `getState`/`setState`; works in-process and across process isolation.
- Parameter changes now reach the audio **processor** (fed into the per-block input
  parameter queue), not only the edit controller — so set/automated values actually drive
  the DSP. Applied at the start of the next process block.
- MIDI output capture (`Plugin::take_output_midi`) now works under process isolation, not
  just in-process.
- Process isolation: opt-in out-of-process hosting with timeout/crash detection,
  `auto_isolate_problematic`, and `probe_plugin` to validate plugins safely. Crashes now
  surface as a typed `Error::PluginCrashed`; `Plugin::recover()` respawns and reloads, and
  `Plugin::isolation_pid()` exposes the helper process id.
- Complete host context (`IHostApplication` + `IPlugInterfaceSupport`, vending
  `IMessage`/`IAttributeList`) — fixes crashes in plugins that query the host (e.g. u-he).
- Native plugin editor windows (macOS/Windows; Linux via X11/XCB).
- Cross-platform: builds and unit-tests pass on macOS and Linux (incl. aarch64).
- Declared MSRV (`rust-version = "1.81"`, the floor of the optional `egui` dependency).
- Upgraded the `vst3` binding 0.1.2 -> 0.3.0 (its `TChar` widened from `i16` to `u16`);
  verified end-to-end against Dexed and TyrellN6 (load, params, audio, state round-trip).
- Bumped `cpal` 0.16 -> 0.18 and `rfd` 0.15 -> 0.17.

### API stability
- `Error` and `MidiEvent` are `#[non_exhaustive]` — match them with a wildcard arm; new
  variants can be added without a breaking change.

### Known limitations
- `libclang` is required at build time (for `cpal`'s `coreaudio-sys`/`alsa-sys` bindgen).
  No VST3 SDK is needed — `vst3` 0.3 ships pre-generated bindings.
- The audio path is correctness-first, not yet lock-free/real-time-tuned.
- Plugins that crash from their own packaging (e.g. Waves/WaveShell) are *contained* via
  isolation, not loadable in-process.
- `MidiEvent::ProgramChange` is unsupported (VST3 uses program lists, not MIDI).
