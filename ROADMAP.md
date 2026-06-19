# VST3-Host Continuation Roadmap

> Resumed after ~1 year dormant. Derived from a verified multi-agent state review
> (7 subsystems, 20 adversarially-confirmed findings) on 2026-06-19.
> The grand vision: a **safe, batteries-included VST3 hosting library** — load &
> play a plugin in <10 lines, zero unsafe in the public API, process isolation by
> default, CPAL + egui included, cross-platform.

## State at resume

Extraction is ~40% real / ~70% surface. The in-process load path
(`simple::load_plugin → Vst3Host::load_plugin → PluginImpl::load`) genuinely works
on macOS with safe COM wrapping + RAII teardown. But the headline pillars are not
delivered: no `CpalBackend→Plugin` audio glue (no sound), process isolation is
opt-in and load-only, the only consumer (`vst3-inspector`) ignores the library, all
4 examples + the integration test fail to compile, and ~1700 LOC of dead scaffolding
clutter the tree.

**Acceptance gate (overall):** `cargo test --workspace --all-features` green AND all
examples compile, reached phase by phase.

## Phase 0 — Honest green baseline  ✅ DONE (commits d84ed4a, 948b907, + 0d)
- [x] 0a. Delete orphaned scaffolding: `realtime.rs`, `audio_processing.rs`,
  `stream.rs`, root `com_implementations.rs`, `api_simplification.rs`,
  `error_improvements.rs`. (RT-safe design ideas in `realtime.rs` are recoverable
  from git commit `d20eec2` if Phase 3 wants them — they reference a missing
  `crossbeam` dep and a stale duplicate COM impl, so they can't be revived as-is.)
- [x] 0b. Export `AudioBackend` / `AudioStream` in crate root + prelude; removed the
  `Result<T>` alias from the prelude glob (it shadowed `std::result::Result`).
- [x] 0c. Fixed example/test API drift — all examples + tests compile;
  `discover_plugins_with_callback` implemented. `cargo test --workspace
  --all-features` green (47 pass, 9 ignored needing real plugins/HW or Phase 1/3).
- [x] 0d. Reconciled docs: README_LIBRARY.md no longer claims isolation-by-default /
  timeout / lock-free / pooling (marked _(planned)_ + link ROADMAP); fixed the
  `simple.rs` docstrings that wrongly said discovery is unimplemented.

## Phase 1 — Batteries-included sound (core promise)  ✅ DONE
- [x] 1b. Ownership model: `Plugin` is `Send` (PluginInternal: Send); playback shares
  it as `Arc<Mutex<Plugin>>` behind an `AudioHandle` so the audio thread pumps it
  while the control thread keeps sending MIDI/params.
- [x] 1a. Wired `CpalBackend → Plugin` in `src/playback.rs`: `play_with_backend`
  (generic over `AudioBackend`) + `Vst3Host::play` + `simple::play`. Pure
  `interleave_outputs` is unit-tested (5 tests). Removed the lying `with_backend`
  no-op stub. **Verified on real hardware**: `play_synth` example loads Dexed and
  produces audio (peak 0.13) through the default output device.
- [x] 1c. `CpalBackend` buffer-size negotiation: clamp to the device's supported
  range or fall back to `BufferSize::Default` (fixes CoreAudio rejecting Fixed);
  channel count kept exact so the interleave stays consistent.

## Phase 2 — Prove consumability + close API gaps
- [ ] 2a. Port `vst3-inspector` onto the library (regression net). ← NEXT BIG SLICE (XL)
- [x] 2b. Implemented `discover_plugins_with_callback` (done in Phase 0c).
- [ ] 2c. `IPlugFrame` resize + egui embedding helper.
- [x] 2d. Parameter display correctness: added `Plugin::format_parameter` which calls
  the controller's `getParamStringByValue` (verified against Dexed — e.g.
  `MonoMode = "POLY"`). Documented `Parameter` min/max as normalized and
  `format_value` as an approximation.
- [x] 2e. `get_output_levels` recovers a poisoned lock instead of panicking. Implemented
  PitchBend + ChannelAftertouch (legacy controller channels 129/128), PolyAftertouch
  (first-class `kPolyPressureEvent`) — all verified accepted by Dexed. ProgramChange
  returns an honest "needs IUnitInfo program lists" error (deferred — not a MIDI event
  in VST3).

## Phase 3 — Make process isolation real (safety pillar)  ✅ CORE DONE
- [x] 3a. Extended the IPC protocol with control verbs (LoadPlugin+config, Start/Stop,
  Set/Get/GetAll/FormatParameter, SendMidi, Process with per-channel buffers). The
  `HostCommand`/`HostResponse` enums now live ONLY in the lib; the helper imports them
  (no more duplicate). Added serde derives to `MidiEvent`/`MidiChannel`/`Parameter`.
- [x] 3b. Rewrote the helper as a thin wrapper over the library's public `Plugin` API
  (reuses the verified in-process path — no separate VST3 impl). `IsolatedPluginImpl`
  now forwards every verb. **Verified on hardware**: `isolated_host` loads Dexed in a
  separate process, marshals 2238 params, round-trips set/get/format, and produces
  audio across the boundary (peak 0.12, matching in-process).
- [x] 3d. `process-isolation` is now a default feature so the helper binary always
  builds and the opt-in works without manual flags. Runtime default stays in-process
  (defaulting isolation ON needs helper-binary distribution in the consumer env).
- [x] 3c. Timeout + crash detection: responses are read on a background thread and
  `send_command` waits with a 5s deadline — a hung plugin yields a timeout error (and
  the child is killed), a crashed/exited helper surfaces as a disconnect error, and a
  dead helper short-circuits. Verified by `test_isolation_dead_helper_errors_fast`
  (errors in <1s, no hang). Auto-respawn-and-reload is the one remaining sub-item.
- [ ] 3e. Plugin GUI across the process boundary (CreateGui/CloseGui currently error).
  REMAINING.

## Phase 4 — Polish
- [x] Deleted the dead stub `objc_conflict_resolver.rs` + its misleading "conflicts
  resolved" log (real fix is the private-namespace loader). Cleared ~10 warnings.
- [x] 4b (partial). Cleared the `missing_docs` warnings on the IPC protocol types.
  Warnings down 83 → 61. Remaining are mostly the cocoa deprecations (4a) and a few
  platform-conditional dead-code items.
- [ ] 4a. Migrate `window.rs` `cocoa`/`objc` → `objc2` (clears the remaining ~50
  deprecation + `cargo-clippy` cfg warnings). GUI work — verify interactively.
- [ ] 4b (rest). Escalate `missing_docs` to `deny` in CI once 4a is clean.
- [ ] 4c. Real COM-level tests (load a bundled `.vst3`, run `process()`, assert cleanup).

## Phase 2a — Port `vst3-inspector` onto the library (IN PROGRESS)
A multi-agent analysis produced an incremental, compile-at-each-step slice plan.
- [x] Slice 1. Discovery → library. Added `Vst3Host::scan_plugin_paths()` (lightweight
  path scan; `discover_plugins` loads every plugin so it was wrong for startup). Inspector
  now uses it; deleted `plugin_discovery.rs`. `grep vst3_host`: 0 → consumer.
- [x] Slice 4 (partial). MIDI note-name converters delegate to `vst3_host::midi`
  (deleted ~60 lines of dup; test now guards the library's C3=60 convention).
- [x] Slice 0 (lib). `DetailedPluginInfo` + `get_detailed_plugin_info(path)` (factory,
  classes, bus layout) re-exported at the crate root; `discovery` is now a public module.
  Verified against Dexed. Unblocks the load migration.
- [ ] Slices 2/3/5 (THE BIG COUPLED CHANGE — needs interactive verification). Replace the
  inspector's raw COM/audio state with `vst3_host::Plugin` + `AudioHandle`. SIZED: **73
  `self.{component,controller,processor,plugin_library,host_process_data,
  shared_audio_state,plugin_view,plugin_host_process}` references** across load, the
  audio-thread-shared state (16 refs), MIDI, and the editor — one atomic change
  (the lib hides COM, so the fields can't be removed piecemeal). Deletes
  `com_implementations.rs`, `audio_processing.rs`. **Verification gap:** the GUI launches
  and is screenshottable (rendering verified), but egui takes no external input
  injection, so the *behavior* this change alters (loading, param edits, audio playback,
  MIDI, editor open) can only be confirmed by a human clicking through it. Do it in an
  interactive session — or add a startup self-test path that auto-loads Dexed + logs
  params/audio-peak to make it headlessly verifiable.
- [ ] Slice 6. Process isolation → library; delete `plugin_host_process.rs` +
  `vst3-inspector-helper.rs` (parity already proven).
- [ ] Slice 9 (LAST, interactive-only). Editor window → `Plugin::open_editor`/`PluginWindow`.

## Other remaining (need interactive verification)
- [ ] 2c. `IPlugFrame` resize + egui embedding helper.
- [ ] 3e. Plugin GUI across the process boundary; isolation auto-respawn-and-reload.
- [ ] 4a. `window.rs` cocoa → objc2 migration (~50 warnings).
