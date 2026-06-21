# vst3-host Implementation Roadmap

## Progress (updated 2026-06-21)

**Done** (merged to main, CI green): all of Tier 1's quick wins plus the Tier-2 headline
correctness fix —
- Robustness/bugs: 6.1, 6.2, 6.3, 6.4, 3.1.
- API consistency: 7.1; preset files 7.2 (`Plugin::save_preset`/`load_preset` + `PluginPreset`).
- Docs corrections: 8.1, 8.2.
- CI/quality: 6.5 (clippy `--all-targets`), 6.6 (deny missing_docs), 6.7 (MSRV — corrected to
  1.85), 6.8 (docs.rs), 6.9 (cargo-deny + `deny.toml`), 6.10 (feature matrix), 6.11 (state
  wire tests), 6.12 (helper/example build); 5.1/5.5 (Windows tests + `--all-features`).
- Accessors: 5.4 (ARM64 discovery), 2.7 (RtControl drop counter).
- **1.1**: the `ProcessContext` playhead advances per block AND advertises validity flags
  (`kContTimeValid`/`kProjectTimeMusicValid`) so conformant plugins honor it; builder
  `tempo()`/`time_signature()` configure the transport (validated; threaded through the
  in-process and isolated paths).
- **1.3**: `.vstpreset` container load/save (`Plugin::save_vstpreset`/`load_vstpreset`).
- **4.1**: inspector Save/Load Preset buttons (JSON + `.vstpreset`).
- **Test coverage**: new `tests/feature_coverage_tests.rs` (params, presets, transport audio,
  variable/oversized blocks, process-before-start, .vstpreset error paths) + transport
  state-flag regression test; `find_test_plugin` made robust against licensed system plugins.
- **1.2**: `Plugin::get_units()` enumerates `IUnitInfo` units + program lists.
- **1.5/1.6**: `Plugin::latency_samples()`/`tail_samples()` accessors.
- **2.1**: live audio-input duplex (`play_with_input`, `play_with_input_backend` — built;
  manual real-device verification pending). **2.2**: offline render (`simple::render_to_wav`,
  dep-free `audio::write_wav`). **2.6**: duplex API cleanup.
- **5.2**: Windows + Linux egui editor embedding (`WinEmbed`/`LinuxEmbed`, CI compile-verified).
- **3.2/3.3**: configurable IPC response timeout + helper-path override
  (`Vst3HostBuilder::response_timeout()`/`helper_path()`, `VST3_HOST_HELPER_PATH` env),
  threaded through crash recovery; missing override path fails fast with a clear error.
- **1.7**: sample-accurate MIDI — `Plugin::send_midi_event_at(event, sample_offset)` populates
  the per-event `Event.sampleOffset` (was hardcoded 0). Verified end-to-end: a note at
  offset 256 leaves the block's leading window silent while the offset-0 control fills it.
- **7.3**: metering ergonomics — new `audio::PeakMeter` (falling-ballistic + timed peak-hold,
  time injected for determinism) and `audio::RmsWindow` (moving RMS). Adopted in the inspector's
  VU meter (replaced its hand-rolled `*0.95` decay + `(f32, Instant)` hold). 8 unit tests + 2
  doctests; exported from the crate root and prelude.
- **2.4**: runtime reconfigure — `Plugin::reconfigure(sample_rate, block_size)` re-runs
  `setupProcessing` (deactivate → setup → reactivate) and rebuilds buffers; errors while
  processing / on invalid args / under isolation. Verified: reconfigured 48k/512 → 44.1k/256
  still produces audio (peak 0.1255) and reports the new settings.

**Next up** (deferred, still open): 2.3 denormal guard (ARM-unverifiable), 4.2/4.3 inspector
polish, 4.5 inspector audio export, 1.8 `IMidiMapping`, 2.5 input buffer, 3.5 isolated
sample-accurate automation (pairs with 1.7), and the rest of Tier 3/4. Known pre-existing fragility: `discover_plugins()` instantiates
every installed plugin and can be aborted by a licensed plugin's C++ exception — isolating
discovery is a future robustness item.

## Summary

The `vst3-host` library is a maturing, macOS-first VST3 hosting product with a solid core (discover → load → params → MIDI → play → state → process isolation) and a working egui inspector as its proof-of-API. The biggest opportunities are: (1) closing concrete VST3 protocol gaps that affect whole plugin classes (a frozen playhead/transport in `ProcessContext`, `IUnitInfo`/program lists, `.vstpreset` interop, bus-arrangement negotiation), (2) enabling live/offline audio I/O (audio-input routing, render-to-file), and (3) cheap CI/robustness hardening (Windows tests, denormal/poison-lock fixes, doc corrections). Most defects found are latent or low-severity consistency issues rather than live crashes — the codebase is in good shape, and the highest leverage is in protocol coverage plus a handful of S-effort hardening wins.

---

## Theme 1: VST3 Protocol Coverage

### 1.1 — Advance playhead / transport in `ProcessContext` (medium, M)
- **What:** `ProcessContext` is initialized once and never updated; `continousTimeSamples`/`projectTimeSamples` are never advanced per block, so tempo-synced DSP (LFOs, sync'd delays/arps) sees a frozen transport at time 0.
- **Why:** Affects correctness of an entire class of tempo-aware plugins, even at fixed tempo.
- **Dependencies:** Realtime path (`realtime.rs` `RtCommand`) and isolation IPC for the full transport-mutation slice. Scope first slice to playhead advancement + builder-level static tempo/time-sig.
- **Files:** `vst3-host/src/internal/plugin_impl.rs` (487-502, 749-838), `src/host.rs` (builder 351-396), `src/realtime.rs`.

### 1.2 — `IUnitInfo` / program-list support (medium, L)
- **What:** Query unit hierarchy + program lists; expose `DetailedPluginInfo.units: Vec<UnitInfo>` and a program-select API. `MidiEvent::ProgramChange` is currently rejected.
- **Why:** Multi-unit synths with named program lists cannot switch programs; only parameter-as-program workaround exists.
- **Note:** Program selection needs `IUnitInfo` on the controller (`getUnitCount`/`getProgramListInfo`/`getProgramName` + program-list parameter), not a generic "set the parameter."
- **Dependencies:** Controller COM surface, public discovery types, IPC marshalling (`DetailedPluginInfo` crosses the boundary).
- **Files:** `src/internal/plugin_impl.rs` (920-929), `src/discovery.rs` (67-76), isolation path.

### 1.3 — `.vstpreset` file load/save (medium, M)
- **What:** Parse/serialize the standardized `.vstpreset` container (magic, FUID class ID, chunk list) wrapping the existing `getState`/`setState` payload; expose `Plugin::load_vstpreset`/`save_vstpreset`.
- **Why:** Cannot read factory presets or interchange with DAWs; only opaque in-app round-trip exists today.
- **Dependencies:** Sits above `PluginInternal`, so works for both in-process and isolated paths.
- **Files:** `src/internal/plugin_impl.rs` (1159, 1176), `src/plugin.rs` (447-476).

### 1.4 — Bus-arrangement negotiation (`setBusArrangements`) (medium, L)
- **What:** Enumerate/select speaker arrangements; call `setBusArrangements()` before `setupProcessing`. (Channel counts are already read dynamically — the gap is negotiation, not a hardwired 2-in/2-out.)
- **Why:** Cannot reconfigure IO (sidechain, surround, aux). Also a latent spec-compliance issue: some plugins expect `setBusArrangements` to be called.
- **Dependencies:** Ordering changes around `setupProcessing`, buffer re-prep, new public API, isolation marshalling.
- **Files:** `src/internal/plugin_impl.rs` (452, 548-600).

### 1.5 — Plugin latency query (`getLatencySamples`) (low, S)
- **What:** Call `IAudioProcessor::getLatencySamples()`, expose `Plugin::latency_samples()`. (No `ProcessContext.latencyInSamples` field exists — drop that part of the original proposal.)
- **Why:** Introspection completeness; minimal real impact for a player/inspector host.
- **Files:** `src/internal/plugin_impl.rs` (processor at 31/172), `src/plugin.rs` (trait at 59), isolated path.

### 1.6 — Plugin tail-time query (`getTailSamples`) (low, S)
- **What:** Single COM call + `Plugin::tail_samples()`/`output_tail_duration_secs()`. Pair with introspection, not realtime docs (streaming path does not truncate tails).
- **Why:** Useful once an offline-render path exists; low payoff today.
- **Files:** `src/internal/plugin_impl.rs` (172), `src/plugin.rs`.

### 1.7 — Sample-accurate MIDI scheduling (low, S)
- **What:** Populate the existing per-event `Event.sampleOffset` (hardcoded 0 today) from the API; parameter automation is already sample-accurate.
- **Why:** Matters mainly for offline/precise-sequencer use; a few ms at typical block sizes.
- **Design call:** Add `_at` overloads vs extending the `#[non_exhaustive]` `MidiEvent` enum.
- **Files:** `src/internal/plugin_impl.rs` (847, 1369), `src/midi.rs`, `src/plugin.rs` (221-276).

### 1.8 — `IMidiMapping` introspection (low, M)
- **What:** Query `getMidiControllerAssignment` to resolve `(bus, channel, cc) -> ParamID`. The `vst3` crate exposes `IMidiMapping`.
- **Correction:** Mapping is CC→param and read-only; the proposed `Parameter.cc_mapping: Option<u8>` and `map_cc_to_parameter` setter are the wrong shape. Use `Plugin::midi_cc_to_parameter(bus, channel, cc) -> Option<ParamId>`.
- **Files:** `src/internal/plugin_impl.rs` (controller at 32), `src/parameters.rs`.

### 1.9 — `INoteExpressionController` / MPE (low, L)
- **What:** Per-note expression (tuning/timbre/pressure/bend); requires a noteId allocation scheme (currently hardcoded -1), event-list wiring, new public API + `MidiEvent` variant, isolation marshalling.
- **Why:** Genuine VST3 capability but narrow audience on top of an already-solid MIDI surface.
- **Files:** `src/internal/plugin_impl.rs` (843+, 863/874/918), `src/midi.rs`, isolated path.

### 1.10 — Offline process mode (`kOffline`) (low, M)
- **What:** `Plugin::set_process_mode(ProcessMode)` re-running `setupProcessing` (mode is hardcoded `kRealtime`). Faster-than-realtime batch rendering already works via a `process_audio` loop — this only signals quality/lookahead switches.
- **Files:** `src/internal/plugin_impl.rs` (446, 499).

---

## Theme 2: Audio Engine

### 2.1 — Live audio-input routing / effect hosting (medium, L)
- **What:** Add `Vst3Host::play_with_input`/record; build a working duplex bridge (current `create_duplex_stream` feeds zeros and ignores the input device). Plugin core already feeds `buffers.inputs`.
- **Why:** Hosting effect plugins on live input is a major VST3-host use case; today every play path hardcodes `input_channels: 0`.
- **Correction:** `PluginInfo.audio_inputs` already exists — drop the proposed `PluginInfo` change; only a convenience accessor is net-new.
- **Dependencies:** Needs a lock-free ring bridge between a separate input stream and the output callback (cpal has no real duplex).
- **Files:** `src/playback.rs` (96, 177), `src/backends/cpal_backend.rs` (223-266), `src/host.rs` (482-523), `src/simple.rs`.

### 2.2 — Offline render-to-file helper (low, M)
- **What:** `simple::render_plugin(plugin, duration, output_path)` wrapping the block loop + a minimal WAV writer (new dep, e.g. `hound`). Also useful for inspector audio export (see 4.5).
- **Why:** Common host need (regression fixtures, batch bounce); all primitives are public, but consumers hand-roll 20-40 LOC.
- **Files:** `src/simple.rs`, `src/playback.rs`, `src/plugin.rs` (310), `src/audio.rs`.

### 2.3 — Denormal flush (FTZ/DAZ) guard (low, M)
- **What:** Feature-gated RAII guard setting MXCSR (x86_64) / FPCR (ARM) around `process_audio` and `RealtimePluginRunner::process`.
- **Why:** Standard host idiom; needed before claiming x86 production RT-safety. ARM (primary macOS target) flushes by default, so impact is mainly x86.
- **Files:** `src/internal/plugin_impl.rs` (803), `src/realtime.rs` (94).

### 2.4 — Runtime sample-rate / block-size reconfigure (low, L)
- **What:** `Plugin::reconfigure(sample_rate, block_size)` re-running `setupProcessing` + buffer prep; error when `is_processing`. (`Plugin.sample_rate`/`block_size` are write-only `#[allow(dead_code)]` today.)
- **Why:** Mid-session device sample-rate switching; reload is the current escape hatch.
- **Files:** `src/internal/plugin_impl.rs` (442-465, 951), `src/plugin.rs` (44-49, 59-115), `src/host.rs`.

### 2.5 — Input-stream buffer-size negotiation (low, S)
- **What:** Generalize `resolve_output_buffer_size` to cover `create_input_stream` (uses unconditional `BufferSize::Fixed` today, which CoreAudio may reject). For `create_duplex_stream`, apply the output resolver (it builds an output stream).
- **Files:** `src/backends/cpal_backend.rs` (190-221, 236).

### 2.6 — Remove dead duplex Mutex (low, S→M)
- **What:** The unused `create_duplex_stream` locks a never-written dummy `Arc<Mutex<Vec<f32>>>` inside the audio callback. Delete it (S) or implement real duplex via `rtrb` (M). Folds into 2.1 if that lands first.
- **Files:** `src/backends/cpal_backend.rs` (240-252).

### 2.7 — RtControl drop counter (low, S)
- **What:** Add an `AtomicU64` drop counter + `dropped_command_count()` getter on `RtControl`/`RtAudioHandle`. The per-call `bool` already exists (reachable via `RtAudioHandle::control()`); only aggregate observability is missing. Skip the proposed overflow callback.
- **Files:** `src/realtime.rs` (63-66, 112-120), `src/playback.rs` (164).

---

## Theme 3: Process Isolation

### 3.1 — Bounded shutdown with kill fallback (low → bug, S)
- **What:** `shutdown()` calls `process.wait()` with no timeout/SIGKILL fallback and runs from `Drop` — a wedged helper can hang the host on exit. `send_command` already has the kill-on-timeout pattern to mirror.
- **Files:** `src/internal/process_isolation.rs` (381-411).

### 3.2 — Configurable plugin timeout (low, S)
- **What:** `builder().response_timeout(Duration)` plumbed into the host-constructed `PluginHostProcess::set_timeout` (hardcoded 5s today; setter exists but unreachable). Per-plugin override optional (and a no-op in-process).
- **Files:** `src/internal/process_isolation.rs` (14, 288-294), `src/internal/isolated_plugin_impl.rs` (44-63), `src/host.rs` (341-419).

### 3.3 — Helper-path override + early probe (low, S)
- **What:** `builder().helper_path(PathBuf)` + `VST3_HOST_HELPER_PATH` env var threaded into `PluginHostProcess::new` before the heuristic search; surface missing-helper at host creation. Drop the embed-as-resource idea.
- **Files:** `src/internal/process_isolation.rs` (188-249), `src/host.rs` (341-410).

### 3.4 — Binary IPC for audio frames (low, M)
- **What:** Replace JSON with length-prefixed binary (bincode/messagepack) for `Process`/`AudioOutput` variants; per-block f32 JSON is slow. **Drop the proposed back-pressure/window** — `send_command` is strictly synchronous request/response, so the pipe can't be over-run.
- **Files:** `src/internal/process_isolation.rs` (69-73, 302-346), `src/internal/isolated_plugin_impl.rs` (159-162).

### 3.5 — Sample-accurate parameter automation across isolation (low, M)
- **What:** Add `HostCommand::SetParameterAt { id, value, offset }`, handle in helper, override `set_parameter_at` in `IsolatedPluginImpl` (offset silently dropped today). Already documented as a known limitation. Needs a cross-boundary timing test.
- **Dependencies:** Pairs with 1.7.
- **Files:** `src/internal/process_isolation.rs` (43-48), `src/internal/isolated_plugin_impl.rs`, `src/bin/vst3-host-helper.rs` (~191).

### 3.6 — Optional auto-recover (low, M)
- **What:** `builder().auto_recover_plugins(true)` with bounded retries + a recovery notification/callback. Manual `recover()` works today (roadmap calls auto-respawn the one remaining sub-item). State-replay overlaps existing save/load.
- **Files:** `src/internal/isolated_plugin_impl.rs` (308-338), `src/plugin.rs` (507-518), `src/host.rs`.

### 3.7 — Document/cap isolation MIDI-out overflow (low, S)
- **What:** Document the 4096 `MAX_OUTPUT_MIDI` cap (oldest-dropped, silent) in the public docstring; optional debug-log of drop count. Full Error/callback is gold-plating. Also fix the stale docstring claiming isolated output MIDI isn't captured.
- **Files:** `src/internal/isolated_plugin_impl.rs` (180-189), `src/internal/plugin_impl.rs` (819-824), `src/plugin.rs` (433-445).

### 3.8 — Windows/Linux isolated editor GUI (low, L)
- **What:** Wire a Win32/X11 event loop into the non-macOS helper and forward `CreateGui`/`CloseGui` (returns an error off-macOS today; macOS path already shipped). Cheap partial win: document the limitation.
- **Files:** `src/bin/vst3-host-helper.rs` (44-61, 287-294), `src/window.rs` (146-277).

---

## Theme 4: Inspector UX

### 4.1 — Preset save/load UI (medium, M)
- **What:** Add Save/Load Preset buttons (JSON wrapper: plugin uid + timestamp + state bytes) using the library's already-shipped `save_state`/`load_state`. Handle the isolation error path (state not bridged across boundary).
- **Why:** Inspector is the "proof the public surface is sufficient" — and it doesn't surface a working library feature.
- **Files:** `vst3-inspector/src/main.rs` (`show_plugin_tab` 888, Preferences 432-437).

### 4.2 — Surface errors in UI, not console (low, S)
- **What:** `set_error(msg)` helper setting `self.last_error` with a 3s auto-clear; replace `println!("Failed...")` across ~15 sites (param/preference/processing/MIDI paths). Field exists and is displayed but rarely populated.
- **Files:** `vst3-inspector/src/main.rs` (480, 578-580; sites at 600/765/795/1188/1512/1530/1586/1637/1695/2385/2934/2945).

### 4.3 — Persist inspector UI/session state (low, S)
- **What:** Wire up the already-declared-but-dead `last_loaded_plugin` + `window_size`, add last tab / MIDI channel / sample rate / block size. Skip `last_parameter_values` (duplicates library state save). Currently only scan-paths persist.
- **Files:** `vst3-inspector/src/main.rs` (Preferences 431-437, `from_path` 2991-2997).

### 4.4 — MIDI file (SMF) playback (low, M)
- **What:** Load `.mid` via `midly`, replay NoteOn/Off/CC on a timer thread calling existing `send_midi_event`. **Scope down** the proposal's sample-accurate-sync timeline (L → M minimal).
- **Files:** `vst3-inspector/src/main.rs` (2025, 2491-2539, 2754).

### 4.5 — Audio export / record-to-file (low, M)
- **What:** Record plugin output to WAV. Cleaner as a **library offline-render helper (2.2)** that the inspector calls, rather than tapping `AudioHandle`'s metering.
- **Dependencies:** 2.2.
- **Files:** `vst3-inspector/src/main.rs` (705), `src/playback.rs`.

### 4.6 — Audio file / test-signal input (medium, L)
- **What:** Load a WAV / generate sine/noise as plugin audio input (`play` feeds silence today). Core is "load/generate signal, loop through input bus"; the tempo-sync UI is gold-plating.
- **Why:** No way to test EQ/reverb/compressor with a known signal.
- **Dependencies:** Requires the audio-input plumbing from 2.1.
- **Files:** `vst3-inspector/src/main.rs` (257, 2361), `src/playback.rs` (79-141), `src/backends/cpal_backend.rs`.

### 4.7 — A/B preset compare (low, M)
- **What:** Two in-memory states + toggle + diff highlight. Drop export-comparison-file / specific colors.
- **Dependencies:** Builds on 4.1.
- **Files:** `vst3-inspector/src/main.rs`.

### 4.8 — Parameter automation demo (low, L)
- **What:** Scope down to one parameter with a small envelope feeding `points_for_block()` → `set_parameter_at()` during `play`. Currently the sample-accurate automation API has **no consumer anywhere** (even the library's own example reimplements it locally).
- **Why:** Proves an existing public API works end-to-end.
- **Files:** `vst3-inspector/src/main.rs` (2244), `src/parameters.rs` (269), `src/plugin.rs` (164).

---

## Theme 5: Cross-Platform & Parity

### 5.1 — Run tests on Windows CI (medium, S)
- **What:** Windows job only runs `cargo build`; add `cargo test -p vst3-host` (one line). Windows DLL/module-loading and path logic are the likeliest to silently regress. (Linux already uses `--all-features`; only Windows is default-only.)
- **Files:** `.github/workflows/ci.yml` (after line 55).

### 5.2 — Windows/Linux egui editor embedding (medium, M)
- **What:** Implement `EmbeddedEditor` for Windows (`CreateWindowEx`/`SetParent`) and Linux (XCB reparent); macOS-only today. **Correction:** `egui-widgets` already compiles on all platforms (it's a feature, not a target gate) — only the runtime call errors off-macOS, and no module refactor is needed. `window.rs` already has cross-platform standalone window code to adapt → effort M, not L.
- **Files:** `src/embed.rs` (54-161), `src/window.rs` (146-274), `src/plugin.rs` (375, 553/564).

### 5.3 — Architecture diagnostics for Windows/Linux module loading (low, M)
- **What:** Parse PE COFF machine word / ELF `e_machine` on load failure to produce an actionable arch-mismatch message, mirroring the tested Mach-O path on macOS.
- **Files:** `src/internal/module_loader/windows.rs` (49-50), `linux.rs` (50-52), `macos.rs` (34-87 as reference).

### 5.4 — ARM64 discovery paths (low, S each)
- **What:** Add `aarch64-linux` and `arm64-win` (**not** `aarch64-win` — VST3 spec uses `arm64-win`/`arm64ec-win`) to the discovery arch arrays. Module loaders are arch-agnostic; only discovery gates these out.
- **Files:** `src/discovery.rs` (490-509 Windows, 511-532 Linux).

### 5.5 — Windows `--all-features` + tests (low, S)
- **What:** Folds into 5.1 — add `--all-features` to the Windows job to also exercise `egui-widgets` compilation. (cpal/isolation are already default features, so the practical delta is small.)
- **Files:** `.github/workflows/ci.yml` (55).

### 5.6 — Window.rs platform-path test coverage (low, M)
- **What:** Xvfb-on-Linux-CI smoke test or a fake-editor injection seam to exercise Linux XCB / Windows HWND open/close (zero coverage today). Pure-mock isn't feasible without an abstraction seam; needs a fixture plugin.
- **Files:** `src/window.rs` (60-277), CI.

---

## Theme 6: Quality, Robustness & CI Hardening

### 6.1 — Poison-lock recovery in `window.rs` (medium → bug, S)
- **What:** Replace ~6 bare `.lock().unwrap()` (lines 62/75/81/136/209/274) with `unwrap_or_else(|p| p.into_inner())`, matching `playback.rs:40` and `window.rs:285`'s own `close()`. A panicking audio thread can otherwise panic the UI thread.
- **Files:** `src/window.rs`.

### 6.2 — Poison-lock recovery in COM FFI param queue (low → bug, S)
- **What:** Three `.lock().unwrap()` in `ParameterValueQueue` FFI thunks (881/885/899) can unwind across the COM boundary (UB/abort). Replace with poison recovery (matching `insert_point` at 861). Skip the parking_lot/catch_unwind over-engineering.
- **Files:** `src/internal/com_implementations.rs`.

### 6.3 — NaN-safe automation sort (low → bug, S)
- **What:** `parameters.rs:190` `partial_cmp(...).unwrap()` panics on NaN time from the public `add_point`. Use `unwrap_or(Ordering::Equal)` / `total_cmp` (builder returns `Self`, so `Result` would break chaining).
- **Files:** `src/parameters.rs` (183-192).

### 6.4 — Consistent `ComPtr::from_raw` handling (low → bug, S)
- **What:** Replace `.unwrap()` with the codebase's existing `ok_or_else(...)?` / `if let Some` pattern at `plugin_impl.rs:257/1000/1098` and `discovery.rs:270`. All are null-guarded so the panic is effectively unreachable — pure consistency cleanup. Use `Error::Other`.
- **Files:** `src/internal/plugin_impl.rs`, `src/discovery.rs`.

### 6.5 — `clippy --all-targets` in CI (low, S)
- **What:** Add `--all-targets` to the macOS clippy step (and `justfile:95`) to lint tests/examples (~7000 LOC unlinted).
- **Files:** `.github/workflows/ci.yml` (22), `justfile` (94-95).

### 6.6 — Escalate `missing_docs` to deny (low, S)
- **What:** Flip `#![warn(missing_docs)]` → `deny` (`lib.rs:55`). Verified: the API is **already fully documented** (0 warnings), so it's a 1-char regression lock per the roadmap's own 4b item.
- **Files:** `vst3-host/src/lib.rs`.

### 6.7 — MSRV check in CI (low, S)
- **What:** Add a `dtolnay/rust-toolchain@1.81` job (`rust-version = "1.81"` declared but never verified) for a published crate.
- **Files:** `.github/workflows/ci.yml`.

### 6.8 — docs.rs build validation (low, S)
- **What:** Add `RUSTDOCFLAGS="--cfg docsrs -D warnings" cargo +nightly doc -p vst3-host --all-features --no-deps`. Must include `--cfg docsrs` (the proposal omits it) to exercise doc-cfg paths.
- **Files:** `.github/workflows/ci.yml`, `Cargo.toml` (124-126).

### 6.9 — cargo-deny / cargo-audit (low, S)
- **What:** One CI step + optional `deny.toml`; license scanning is relevant after the MIT relicense. Hygiene, not urgent (attack surface is the loaded plugins).
- **Files:** `.github/workflows/ci.yml`, new `deny.toml`.

### 6.10 — Feature-combination build matrix (low, S)
- **What:** Small matrix over `--no-default-features`, single features, and `cpal-backend,egui-widgets`. All combos pass today — preventive guard for cfg'd code (`embed.rs:11`, `lib.rs:70-112`).
- **Files:** `.github/workflows/ci.yml`, optional `justfile` recipe.

### 6.11 — Wire-protocol serde round-trip tests (low, S)
- **What:** Add a deterministic round-trip test for `SaveState`/`LoadState`/`State` IPC messages (existing `wire_tests` only covers `AudioOutput`). Platform-independent, runs in CI without a plugin.
- **Files:** `src/internal/process_isolation.rs` (76-78, 127, 446-490).

### 6.12 — Build isolation helper + smoke example in CI (low, S→L)
- **What:** Cheap slice: add `cargo build --bin vst3-host-helper` and `cargo run --example plugin_scanner` so the isolation path/examples keep working. **Running the 6 ignored isolation tests is L and blocked** — `test_plugins/Dexed.vst3` is gitignored and no headless MIDI emitter exists.
- **Files:** `.github/workflows/ci.yml`, `justfile` (35-36).

---

## Theme 7: API Polish

### 7.1 — `get_parameter(&self)` instead of `&mut self` (low, S)
- **What:** `plugin.rs:178` is the only read method taking `&mut self`; the trait method and both impls already take `&self`. Trivial `&mut`/`as_mut()` → `&`/`as_ref()` flip; no audit needed (proposal over-hedges). API-consistency cleanup.
- **Files:** `src/plugin.rs` (178-182).

### 7.2 — State file-I/O convenience wrapper (low, S)
- **What:** `Plugin::save_state_to_file`/`load_state_from_file` + a `PluginPreset { uid, version, state, ... }` serde type embedding the uid for mismatch detection (load on wrong plugin is "undefined" today). serde already a dep. Drop the `metadata: HashMap` field. Overlaps/merges with 1.3 (`.vstpreset`) and 4.1.
- **Files:** `src/plugin.rs` (458-476).

### 7.3 — Metering ergonomics: peak-hold decay + windowed RMS (low, S)
- **What:** `AudioLevels::with_peak_hold_decay(Duration)` (per-channel last-update `Instant`) + a windowed RMS helper. Both real consumers (inspector `main.rs:516-517/720-739`, `examples/plugin_gui.rs`) hand-roll decay because the library's sticky `peak_hold` is unusable for UI meters.
- **Files:** `src/audio.rs` (103-154).

---

## Theme 8: Documentation

### 8.1 — Fix stale isolation MIDI-out doc (medium, S)
- **What:** Delete/correct the false line at `docs/how-to/send-midi.md:77` — output MIDI **is** captured under isolation (shipped). Misleads users away from a working path.
- **Files:** `docs/how-to/send-midi.md`.

### 8.2 — Fix isolated-editor doc contradiction (medium, S)
- **What:** `isolate-plugin-crashes.md:108-109` and `process-isolation.md:64-66` falsely claim editor-across-isolation is unsupported and that `open_editor` errors — it works on macOS (helper-owned window) and returns `Ok`. Change to "macOS only"; add a platform matrix.
- **Files:** `docs/how-to/isolate-plugin-crashes.md`, `docs/explanation/process-isolation.md`.

### 8.3 — Editor open/embed how-to (low, S)
- **What:** Create one editor how-to covering `open_editor`/`PluginWindow` + `EmbeddedEditor::set_rect` + the `take_editor_resize_request` poll loop (no editor how-to exists at all). Note in-process-only / macOS-embed-only caveats.
- **Files:** new `docs/how-to/` page; references `src/plugin.rs` (495-505), `src/embed.rs` (63-67).

### 8.4 — Sample-accurate automation how-to (low, S)
- **What:** Guide for `set_parameter_at`, sample offsets, `ParameterAutomation` curves, `points_for_block`, isolation caveat. Working example exists (`examples/parameter_automation.rs`); no narrative doc.
- **Files:** new `docs/how-to/sample-accurate-automation.md`.

### 8.5 — Audio metering how-to (low, S)
- **What:** Consolidate scattered metering docs into one task page. Lower priority — most content already exists in rustdoc + explanation + tutorial.
- **Files:** new `docs/how-to/monitor-audio-levels.md`.

---

## Recommended Order

### Tier 1 — Quick Wins (S, high value)
1. **8.1, 8.2** — Fix stale/wrong docs that hide shipped features (isolation MIDI-out, isolated editor). *(medium severity, trivial.)*
2. **5.1** — Run tests on Windows CI. *(catches the most regression-prone platform code.)*
3. **6.1** — Poison-lock recovery in `window.rs`. *(medium-severity robustness, mechanical.)*
4. **6.2, 6.3, 6.4** — Remaining lock/NaN/`from_raw` robustness consistency fixes.
5. **3.1** — Bounded shutdown with kill fallback (avoids host hang on exit).
6. **7.1** — `get_parameter(&self)` consistency flip.
7. **6.6, 6.5, 6.7, 6.8, 6.9, 6.11** — Cheap CI/quality hardening (deny missing_docs, clippy --all-targets, MSRV, docs.rs, cargo-deny, wire serde tests).
8. **3.2, 3.3** — Configurable timeout + helper-path override.
9. **5.4** — ARM64 discovery paths (use correct `arm64-win`).
10. **1.5, 1.6, 1.7** — Latency/tail/sample-accurate-MIDI thin accessors.
11. **7.2, 7.3, 2.7, 2.5** — State file wrapper, metering ergonomics, RtControl drop counter, input buffer-size negotiation.

### Tier 2 — High-Value Mid-Size (M/L)
1. **1.1** — Advance playhead/transport in `ProcessContext`. *(biggest correctness win; fixes a whole plugin class.)*
2. **1.3 / 7.2 merged** — `.vstpreset` + state file wrappers.
3. **4.1** — Inspector preset save/load UI (surfaces a shipped library feature).
4. **5.2** — Windows/Linux egui editor embedding.
5. **1.2** — `IUnitInfo` / program-list support.
6. **2.3** — Denormal flush guard.
7. **4.2, 4.3** — Inspector error surfacing + session persistence.

### Tier 3 — Larger Efforts (L/XL)
1. **2.1 + 4.6** — Live audio-input routing + inspector test-signal input. *(unlocks effect-plugin hosting, a major use case.)*
2. **1.4** — Bus-arrangement negotiation.
3. **2.2 + 4.5** — Offline render-to-file (library helper + inspector export).
4. **2.4** — Runtime sample-rate/block-size reconfigure.
5. **3.4, 3.5, 3.6** — Binary IPC, isolated sample-accurate automation, auto-recover.
6. **1.8, 1.9, 1.10** — IMidiMapping, note expression/MPE, offline process mode.
7. **4.4, 4.7, 4.8** — Inspector SMF playback, A/B compare, automation demo.

### Tier 4 — Cross-Platform / Parity
1. **3.8** — Windows/Linux isolated editor GUI (+ document limitation as a cheap partial).
2. **5.3** — Windows/Linux module-loader arch diagnostics.
3. **5.5, 5.6, 6.10, 6.12** — Windows `--all-features`, window.rs platform tests, feature-combo matrix, isolation-helper CI build.
4. **8.3, 8.4, 8.5** — Editor, automation, and metering how-to guides.