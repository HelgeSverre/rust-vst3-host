# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

A Cargo workspace with two members:

- **`vst3-host/`** — a safe Rust library for hosting VST3 plugins. **This is the product.**
- **`vst3-inspector/`** — an egui app for browsing/inspecting/testing plugins, built
  entirely on the library's public API (it contains no VST3/COM code of its own — it's the
  proof that the public surface is sufficient, and a worked example of consuming it).

The library wraps the VST3 C++/COM API behind a safe Rust surface: callers never write
`unsafe` and never see a COM pointer.

## Commands

Common tasks are wrapped in a [`justfile`](justfile) (`just --list` to see all):

```bash
just build              # cargo build --workspace
just test               # cargo test --workspace --all-features
just lint               # cargo clippy --workspace --all-features  (alias: just clippy)
just check              # fmt-check + clippy + test (pre-merge gate)
just fmt                # cargo fmt
just inspector          # run the egui app
just play [PLUGIN]      # play_synth example (defaults to test_plugins/Dexed.vst3)
just isolated [PLUGIN]  # run a plugin in an isolated process
just selftest [PLUGIN]  # headless: drive the library through the inspector binary, no GUI
just helper             # build the process-isolation helper binary
just test-isolation     # run the (ignored) isolation capstone tests
```

`just selftest` is the fastest way to verify the end-to-end path (discover → load →
params → play → audio) without a GUI; use it as a smoke test after changes.

## Library architecture (`vst3-host/src/`)

Public modules expose only safe types:

- `simple` — one-call helpers (`load_plugin`, `play`, `discover_plugins`).
- `host` — `Vst3Host` + `Vst3HostBuilder` (sample rate, block size, isolation, scan paths).
- `plugin` — `Plugin` (the loaded plugin handle), `PluginInfo`.
- `playback` — `AudioHandle`, `play_with_backend` (the cpal→plugin bridge), plus the
  lock-free `RtAudioHandle` / `play_realtime_with_backend`.
- `realtime` — `RealtimePluginRunner` + `RtControl`: owns the plugin on the audio thread,
  control commands over a lock-free SPSC ring (no lock in the callback).
- `embed` — `EmbeddedEditor` (feature `egui-widgets`): parent a plugin editor into a host
  (egui) window. macOS only so far.
- `parameters`, `midi`, `audio`, `error`, `discovery` (incl. `PluginReport` JSON export),
  `backends` (CpalBackend), `process_isolation`, `window`.

All `unsafe`/COM lives in **`src/internal/`** (not exported):

- `plugin_impl` — in-process VST3 COM lifecycle (the real work; RAII cleanup).
- `com_implementations` — host-side COM objects (event lists, component handler).
- `module_loader/` — platform bundle loading (CFBundle on macOS, etc.).
- `isolated_plugin_impl` — IPC client for the out-of-process path.

`Plugin` holds a boxed `PluginInternal` trait object; `PluginImpl` (in-process) and
`IsolatedPluginImpl` (IPC to the helper) both implement it, so `load_plugin` returns the
same `Plugin` type either way. See `docs/explanation/architecture.md`.

## Key facts

- **MIDI note convention: C3 = 60** (`midi::note_to_name`/`name_to_note`).
- **Parameters are normalized** (`0.0–1.0`); use `Plugin::format_parameter` to render the
  plugin's own display string.
- **Audio**: two paths. `Vst3Host::play`/`simple::play` (mutex on the callback,
  correctness-first) and `Vst3Host::play_realtime`/`RealtimePluginRunner` (lock-free command
  ring, no lock on the callback). Neither is a fully zero-allocation RT engine yet.
  `process_audio` handles variable block sizes (clamped to the configured max).
- **Process isolation**: the `process-isolation` feature is on by default (so the
  `vst3-host-helper` binary always builds), but the runtime default is in-process. Opt in
  with `Vst3Host::builder().with_process_isolation(true)`. Crashes surface as
  `Error::PluginCrashed` and `Plugin::recover()` respawns+reloads; GUI-across-boundary is
  still not implemented. Parameters/audio/state/output-MIDI all marshal across the boundary.
  See `docs/explanation/process-isolation.md`.
- **Program selection** uses `IUnitInfo` program lists: `Plugin::select_program(unit_id,
  program_index)` sets the unit's `kIsProgramChange` parameter. `MidiEvent::ProgramChange` is
  routed to the **root unit (id 0)** — the MIDI channel does not map to a VST3 unit. Both
  paths marshal across process isolation.
- Features: `cpal-backend` (default), `process-isolation` (default), `egui-widgets`
  (`EmbeddedEditor` — embed a plugin editor in an egui window, macOS).
- The `prelude` does NOT export `Result` (it would shadow `std::result::Result`); use
  `vst3_host::Result`.

## Build setup

No special setup: `git clone` then `cargo build`. The `vst3` dependency (0.3) ships
pre-generated bindings, so there is **no VST3 SDK requirement and no `VST3_SDK_DIR`** — the
old `vst3sdk` submodule and `.cargo/config.toml` have been removed. (`libclang` is still
needed for `cpal`'s `coreaudio-sys`/`alsa-sys` bindgen on macOS/Linux.)

## Documentation

User-facing docs are in [`docs/`](docs/) (tutorials / how-to / reference /
explanation), indexed by [`docs/README.md`](docs/README.md). Working notes, the roadmap,
design history, and VST3 SDK internals notes are in [`docs/internal/`](docs/internal/) —
not user-facing.

## Conventions

- Run `just check` (or at least `cargo fmt` + `cargo test --workspace --all-features`)
  before considering work done. Keep the tree compiling and the suite green per change.
- Real plugins for testing live in `test_plugins/` (e.g. `Dexed.vst3`).
- When adding library API, prefer extending the safe public surface over exposing COM;
  keep all `unsafe` inside `src/internal/`.
