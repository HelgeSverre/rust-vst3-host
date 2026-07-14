# Changelog

All notable changes to `vst3-host` are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims to follow
[Semantic Versioning](https://semver.org/) (pre-1.0: new features bump the minor version).

## [Unreleased]

## [0.7.0] - 2026-07-14

### Added

- **Full `PluginInternal` parity for process-isolated plugins.** Everything the in-process
  path supports now marshals across the IPC boundary:
  - `Plugin::reconfigure` and `Plugin::set_process_mode` (`kRealtime`/`kOffline`), so
    faster-than-real-time offline bouncing works out-of-process. Contributed by
    [@ro-ag](https://github.com/ro-ag) ([#7], fixes [#6]).
  - `bus_arrangements` / `set_bus_arrangements` (speaker-layout query and negotiation),
    `get_units` (unit/program-list enumeration — previously silently empty),
    `latency_samples` / `tail_samples` (previously silently `0`), and
    `midi_cc_to_parameter` (`IMidiMapping` — previously silently `None`).
- **Crash recovery replays the runtime config.** `Plugin::recover()` (and auto-recover)
  reloads a respawned helper at the last `reconfigure`d sample rate / block size and
  re-applies a non-default process mode, instead of silently reverting to the load-time
  settings. Verified end-to-end by a capstone that kills the helper and proves the rendered
  pitch is unchanged.
- MIDI input example (`examples/`).

### Fixed

- The isolation helper sized `process` output buffers as *bus count × 2* (a stereo-per-bus
  assumption), silently dropping channels beyond two per bus after a negotiated non-stereo
  arrangement; it now uses the plugin's live per-bus channel count.
- `Plugin::output_channel_count()` on an isolated plugin returned the load-time snapshot
  forever; a successful `set_bus_arrangements` now refreshes it from the arrangement the
  plugin actually applied.
- The helper committed its tracked sample rate before the plugin accepted a `Reconfigure`;
  a rejected reconfigure no longer desyncs the post-crash reload rate.
- `Plugin::reconfigure` rejects `block_size > i32::MAX` instead of silently truncating it in
  the internal casts.

## [0.6.1] - 2026-07-08

### Fixed

- **Memory leak: host-owned COM objects leaked a reference on every `process()` setup.** The
  host handed borrowed, host-owned COM objects to the plugin with `to_com_ptr().into_raw()`,
  which transfers a `+1` reference that is never released. The affected pointers — `ProcessData`
  input/output event lists and parameter changes, the `IComponentHandler` passed to
  `setComponentHandler`, and the `IParamValueQueue` pointers returned from `IParameterChanges` —
  are only borrowed for the duration of the call, so they are now passed as borrowed pointers via
  `as_com_ref().as_ptr()`. The `queryInterface` sites are unchanged: returning ownership there is
  required by the COM contract. LeakSanitizer reported 944 bytes leaked in 17 allocations before
  the fix; the sanitizer suite is clean after. ([#4])
- Bumped the `crossbeam-epoch` lockfile entry to 0.9.20 to clear an advisory flagged by
  `cargo-deny`.

## [0.6.0] - 2026-07-06

### Added

- **Timeline / sequencer engine** (`transport` module): schedule MIDI clips (`MidiClip`) and
  parameter-automation lanes (`AutomationLane`) on a beat grid and drive them into a plugin
  sample-accurately. `Timeline::advance_block` resolves the next block to `(event, sample_offset)`
  and `(param_id, sample_offset, value)` lists; `Timeline::drive_block` pushes them into a
  `Plugin` and renders one block. Beats timebase at a constant tempo (a tempo curve is future
  work). Exported from the crate root and prelude.
- `RtControl::send_midi_at`, `AudioHandle::send_midi_at`, `MidiSink::send_midi_at`, and
  `HostCommand::SendMidiAt` (isolation) carry a MIDI sample offset end-to-end, so scheduled notes
  land at the same block position on the in-process, lock-free, and isolated paths (the realtime
  and mutex playback bridges previously dropped the offset to block start).
- `Plugin::output_midi_handle()` returns an [`OutputMidiConsumer`] — a `Send` + `Sync` handle
  for draining the MIDI a plugin emits (arpeggiators, MPE, thru) from any thread without locking
  the audio thread. Pairs with `RealtimePluginRunner`: the lock-free runner previously had no way
  to surface output MIDI; now you take the handle, move the plugin into the runner, and poll from
  your UI thread while the audio thread pushes. In-process only (`None` under process isolation,
  where output MIDI crosses the boundary in IPC responses).
- **Android build support**: the crate now compiles for `target_os = "android"` (verified
  on-device on `arm64-v8a`, Android 13). Android reuses the Linux VST3 module path — `dlopen`
  plus the required `ModuleEntry`/`ModuleExit` entry points — so discover → load → parameters →
  `process_audio` all run natively on a phone through the library's public API. Plugin-editor
  windowing is not supported on Android (`PluginWindow::open` returns an error; there is no X11).

### Changed

- Output MIDI is now buffered in a lock-free bounded queue (`crossbeam-queue`) instead of an
  `Arc<Mutex<Vec>>`, so capturing emitted events takes no lock on the audio thread (oldest event
  dropped when full, as before).

- `RealtimePluginRunner::process` is now allocation-free and `Drop`-free in steady state — no
  per-block heap allocation, reallocation, or free once warmed up, even while parameter changes
  and MIDI (in and out) flow — given a fixed-size buffer and an in-process plugin. Fixed the
  three steady-state churns: `pending_param_changes` (drained in place instead of `mem::take`),
  the per-block `ParameterValueQueue` recreation (now a reuse pool), and output-MIDI capture
  (drained in place into a pre-reserved buffer). Guarded by `tests/alloc_tests.rs`. Not yet
  lock-free (a few short, uncontended mutexes remain per block); see the `RealtimePluginRunner`
  docs.

## [0.5.0] - 2026-06-23

### Added

- **Program selection** — `Plugin::select_program(unit_id, program_index)` selects a program
  from a unit's `IUnitInfo` program list (via the unit's `kIsProgramChange` parameter), and
  `MidiEvent::ProgramChange` is now honored instead of rejected (routed to the root unit's
  program-change parameter; the MIDI channel is ignored, as VST3 has no channel→unit mapping).
  Marshaled across process isolation.
- **Bus activation** — `Plugin::set_bus_active(media_type, direction, bus_index, active)` calls
  `IComponent::activateBus`, unlocking sidechain/aux/surround buses that must be explicitly
  activated. New public `audio::MediaType` and `audio::BusDirection` enums. Marshaled across
  process isolation.
- **Parameter-edit gesture capture** — `Plugin::take_parameter_edits()` drains an ordered log of
  the plugin GUI's `beginEdit`/`performEdit`/`endEdit` callbacks as `ParameterEdit` /
  `ParameterEditKind`, so hosts can record automation gestures, not just final values (which
  `get_parameter_changes` still returns). Marshaled across process isolation.
- **Runtime transport mutation** — `Plugin::set_tempo`/`set_time_signature`/`set_playing` change
  the `ProcessContext` transport on the next block while processing. Lock-free equivalents on
  `AudioHandle` and `RtControl` apply the change from the audio callback without a lock.
  Marshaled across process isolation.
- **Live MIDI input** (feature `midi-input`) — `midi_input::list_midi_input_ports`, `connect`,
  and `bind_to_handle` bind a hardware/virtual MIDI port (via `midir`) and forward parsed events
  into a running `AudioHandle`. New `AudioHandle::midi_sink()` returns a `Send` `MidiSink` for
  cross-thread MIDI injection. Off by default; additive when disabled.
- Export `AutomationCurve` and `AutomationPoint` from the crate root and prelude — they were
  public but unreachable without the full module path, even though `ParameterAutomation::with_curve`
  takes an `AutomationCurve`.

### Documentation

- Synced the user docs with 0.4.x: documented the crash-resistant discovery API
  (`discover_plugins_safe`, `SafeDiscoveryReport`, `probe_timeout`, the `vst3-host-probe`
  binary), corrected stale "MPE is in-process only" claims (it now works across process
  isolation), and fixed the "no GUI across the boundary" note (macOS isolated editors work).

## [0.4.2] - 2026-06-23

### Changed

- Inspector loads plugins on a background thread: introspection, load, and audio start run off
  the UI thread, so a slow or hanging plugin (e.g. Reason Rack) no longer freezes the window —
  it shows a "Loading…" spinner and stays responsive. (A crashing plugin still aborts the
  process; only process isolation contains crashes.)
- Documented and test-locked the process-isolation hang guarantee: a plugin that hangs the
  helper (including during load) yields a timeout error and the helper is killed, so it can't
  block the host. A plain in-process `load_plugin` is synchronous and cannot be bounded — a
  hanging plugin (e.g. Reason Rack, which expects its host ecosystem) blocks the caller, so
  load it with process isolation.

## [0.4.1] - 2026-06-23

### Fixed

- Restore the `IPluginBase::terminate()` teardown that 0.4.0 dropped. 0.4.0 skipped `terminate()`
  to work around a crash in Waves/WaveShell's own `terminate()`, but that regressed plugins
  which **require** it to break their controller↔component link before release — e.g. Jup-8000
  and Analog Lab V crashed on unload. `terminate()` is the spec-compliant teardown and is needed
  by most (especially dual-component) plugins, so it is called again. WaveShell loads and runs
  but can crash intermittently on unload (a Waves packaging bug); use process isolation if a
  clean unload matters for such plugins.

## [0.4.0] - 2026-06-23

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

### Removed

- `Vst3HostBuilder::auto_isolate_problematic` and the internal name-based "Objective-C
  conflict" detection. They auto-isolated plugins like Waves/WaveShell out-of-process by
  matching their filename — a band-aid for a crash that is now fixed properly (see below), so
  those plugins load in-process like any other. Explicit `with_process_isolation(true)` is
  unchanged for callers who still want isolation. The hardcoded Ozone discovery blacklist was
  also removed (Ozone loads fine).

### Fixed

- Plugins that crash in their own `terminate()` no longer take down the host. Some plugins —
  notably shell plugins like Waves' WaveShell — null-deref inside `terminate()` on teardown
  (an uncatchable native crash). `Plugin` teardown now stops processing, deactivates, and
  disconnects the component/controller, then drops the COM references (the plugin frees itself
  in its destructor via `Release`) — it no longer calls `terminate()`. Real hosts mitigate the
  same way. WaveShell, Ozone Imager, and Access Virus Editor now load and tear down cleanly
  in-process.
- Loading **or opening the editor of** an editor-style plugin no longer crashes the host.
  Three places created the plugin's editor view to probe it (the `has_gui` check at load,
  `has_editor()`, and `get_editor_size()` — the latter run by `PluginWindow::open()` before
  attaching) and then called `IPlugView::removed()` on the view. But those views were never
  `attached()`, which violates the `IPlugView` lifecycle. Plugins that only initialize their
  close state on attach (e.g. Access Virus Editor) segfaulted in `OnUIClose()`. The probe views
  are now released (dropped) without the spurious `removed()` call; only the real
  attach/detach path (`open_editor`/`close_editor`) calls `removed()`.

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

[Unreleased]: https://github.com/HelgeSverre/rust-vst3-host/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.7.0
[0.6.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.6.1
[0.6.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.6.0
[0.5.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.5.0
[0.4.2]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.4.2
[0.4.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.4.1
[0.4.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.4.0
[0.3.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.3.0
[0.2.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.2.1
[0.2.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.2.0
[0.1.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.1.1
[0.1.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.1.0
[#4]: https://github.com/HelgeSverre/rust-vst3-host/issues/4
[#6]: https://github.com/HelgeSverre/rust-vst3-host/issues/6
[#7]: https://github.com/HelgeSverre/rust-vst3-host/pull/7
