# Changelog

All notable changes to `vst3-host` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project aims to follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

First usable release of the safe VST3 hosting library, extracted from the inspector app.

### Added
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

### Known limitations
- Building requires the VST3 SDK and the `VST3_SDK_DIR` environment variable (inherited
  from the `vst3` dependency). See the README.
- The audio path is correctness-first, not yet lock-free/real-time-tuned.
- Plugins that crash from their own packaging (e.g. Waves/WaveShell) are *contained* via
  isolation, not loadable in-process.
- `MidiEvent::ProgramChange` is unsupported (VST3 uses program lists, not MIDI).
