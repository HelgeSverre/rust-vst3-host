# Changelog

All notable changes to `vst3-host` are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims to follow
[Semantic Versioning](https://semver.org/) (pre-1.0: new features bump the minor version).

## [Unreleased]

### Added

- **Crash-resistant plugin discovery**: `Vst3Host::discover_plugins_safe()` (and
  `discovery::discover_plugins_safe` / `probe_plugin_info_isolated`) introspect each plugin in
  a throwaway `vst3-host-probe` subprocess, so a plugin that `abort()`s or makes a
  pure-virtual call during instantiation is skipped (reported in `SafeDiscoveryReport.skipped`)
  instead of taking down the host. `Vst3HostBuilder::probe_timeout` bounds each probe. The fast
  in-process `discover_plugins` is unchanged. (Trades scan speed for safety — one process per
  plugin.)
- **Per-note expression (MPE) across process isolation**: `note_on` / `note_off` /
  `send_note_expression` / `note_expressions` now marshal to the isolation helper, so MPE works
  the same in-process and out-of-process (previously the isolated path returned "not
  supported"). Verified end-to-end: a Tuning expression bends a voice an octave across the
  subprocess boundary.

## [0.3.0] - 2026-06-22

### Added

- **Per-note expression (MPE)**: `Plugin::note_on` returns a `NoteId`, and
  `send_note_expression(id, NoteExpressionType, value)` targets per-voice expression
  (tuning / volume / pan / brightness / …) via VST3 note-expression events;
  `note_expressions()` discovers what a plugin supports (`INoteExpressionController`). Verified
  end-to-end against a new in-repo `test-plugin/` VST3 synth (`just test-plugin`) — a Tuning
  expression bends one voice an octave.
- `AudioHandle::try_lock` — non-blocking plugin lock for UI/render threads. Returns `None`
  when the audio callback holds the lock (held for each `process_audio` block) instead of
  stalling the caller.
- **Lock-free side channels on `AudioHandle`** so a UI/control thread never contends with the
  audio thread on the hot path: `send_midi` / `set_parameter` / `midi_panic` queue control onto
  a ring the callback drains before each block; `output_levels` (per-channel peak via atomics),
  `drain_output_midi`, and `drain_parameter_changes` read feedback the callback publishes after
  each block. The plugin mutex (`lock()`) is now only needed for rare state ops (preset
  save/load, WAV export, processing start/stop). `play`/`play_with_backend` wire this
  automatically.

### Changed

- Inspector: the Processing tab now scrolls instead of clipping content on a short window,
  secondary sections collapse, and the on-screen keyboard / VU meters are responsive to width.
- Inspector: defaults to the bundled `test_plugins/Dexed.vst3` at startup (was a hardcoded
  system plugin path), falling back to the last-loaded plugin if it isn't present.
- Inspector: repaints continuously so input is always processed promptly — a click that doesn't
  move the mouse no longer waits for the next event to register.
- Inspector: migrated off the egui 0.34 deprecated panel / rounding / `screen_rect` APIs.

### Fixed

- The builder's `sample_rate` / `block_size` are now applied to in-process plugins at load
  (they were ignored — plugins ran at the 44100/512 defaults while `Plugin::sample_rate()`
  reported the configured value).
- Inspector input lag / dropped clicks: the UI thread no longer touches the audio mutex during
  interaction at all. All control (MIDI, parameter edits) and all per-frame feedback (VU
  meters, MIDI monitor, editor parameter sync) now flow through the new lock-free
  `AudioHandle` side channels; the mutex is reserved for rare lifecycle ops. (An interim fix
  used `try_lock` for the per-frame reads.)
- Parameter edits made in a plugin's **own editor GUI** now affect the audio. The plugin
  reports these via `IComponentHandler::performEdit`; the host captured them for display but
  never routed them to the audio processor, so plugins that don't internally relay
  editor→processor changes (e.g. some dual-component synths) ignored GUI knob turns while
  presets still worked. `process()` now feeds those edits into the processor's input
  parameter queue.

## [0.2.1] - 2026-06-22

### Added

- `MidiEvent::from_midi_bytes` — parse a raw channel-voice MIDI message (status + data) into a
  `MidiEvent` (note on/off, CC, pitch bend, aftertouch), for forwarding hardware-controller MIDI.
- Inspector: a "MIDI Input Device" picker that forwards a connected controller's MIDI into the
  loaded plugin live — cross-platform via `midir` (CoreMIDI / ALSA / WinMM); device → plugin
  only, no feedback loop.

## [0.2.0] - 2026-06-22

A large, fully backward-compatible feature release: the public API only grows. Highlights are
real VST3 protocol coverage (transport, units, MIDI mapping, bus arrangements), live and
offline audio I/O, richer process isolation, metering, and a much more capable inspector.

### Added — library

- **Transport / `ProcessContext`**: the playhead now advances per block and advertises validity
  flags; `Vst3HostBuilder::tempo` / `time_signature` configure the transport.
- **Sample-accurate MIDI**: `Plugin::send_midi_event_at(event, sample_offset)`.
- **Sample-accurate parameter automation** is now carried across the process-isolation boundary.
- **Offline process mode**: `ProcessMode` + `Plugin::set_process_mode` (`kRealtime`/`kOffline`).
- **Runtime reconfigure**: `Plugin::reconfigure(sample_rate, block_size)`.
- **Bus-arrangement negotiation**: `audio::SpeakerArrangement` / `BusArrangements`,
  `Plugin::bus_arrangements` / `set_bus_arrangements`.
- **Units & programs**: `Plugin::get_units` (`IUnitInfo`).
- **MIDI controller mapping**: `Plugin::midi_cc_to_parameter` (`IMidiMapping`).
- **Latency / tail**: `Plugin::latency_samples` / `tail_samples`.
- **Presets**: `.vstpreset` load/save (`save_vstpreset` / `load_vstpreset`), JSON `PluginPreset`
  wrappers (`save_preset` / `load_preset`).
- **Live audio input / effect hosting**: `simple::play_with_input`, `play_with_input_backend`.
- **Offline render**: `simple::render_to_wav`, `render_to_wav_with_input`; dependency-free
  `audio::write_wav` / `read_wav`.
- **Test signals**: `audio::SignalSource` (sine / white-noise / WAV) + `InputSource`.
- **Metering**: `audio::PeakMeter` (falling ballistic + timed hold) and `RmsWindow`.
- **Denormal guard**: flush-to-zero / denormals-are-zero around processing (x86 MXCSR, ARM FPCR).
- **egui editor embedding** on Windows and Linux (in addition to macOS).
- **Process isolation**: configurable `Vst3HostBuilder::helper_path` (+ `VST3_HOST_HELPER_PATH`)
  and `response_timeout`; optional `auto_recover_plugins` with `Plugin::recovery_count`;
  bounded shutdown with SIGKILL fallback; GUI across the boundary on macOS.
- **Diagnostics**: actionable architecture-mismatch errors when loading a wrong-arch plugin on
  Windows/Linux (mirrors the macOS Mach-O diagnostic).
- **Input-stream buffer-size negotiation** for capture devices.

### Added — inspector

- Preset save/load, A/B preset compare, audio export to WAV, MIDI file (`.mid`) playback, and a
  parameter-automation demo.
- Session persistence (window size, last tab, MIDI channel, last-loaded plugin) and in-UI error
  surfacing.

### Changed

- Upgraded `egui` / `eframe` to 0.34.

### Fixed

- Playhead now advances during playback (was frozen).
- Crash when closing a plugin editor; UI input lag between events.
- Robustness hardening: poison-lock recovery, NaN-safe automation sort, empty-buffer RMS guard,
  non-finite meter input, MIDI offsets clamped to the actual block.
- Windows/Linux loaders resolve the `.vst3` bundle directory to the inner binary before loading.

### Docs

- New how-to guides: open/embed a plugin editor, sample-accurate automation, monitor audio
  levels; plus architecture and process-isolation explanations.

## [0.1.1] - 2026-06-21

- Relicensed to MIT and published to crates.io with no VST3 SDK build requirement (the `vst3`
  0.3 crate ships pre-generated bindings).

## [0.1.0] - 2026-06-21

- Initial release: safe VST3 hosting — discover, load, parameters, MIDI, audio playback, state
  save/restore, and process isolation.

[0.3.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.3.0
[0.2.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.2.1
[0.2.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.2.0
[0.1.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.1.1
[0.1.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.1.0
