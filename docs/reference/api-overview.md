# API overview

A map of the public API. Full signatures and per-item docs are on
[docs.rs](https://docs.rs/vst3-host) — this page orients you to the main types and where to
reach for each.

## Entry points

| Type / module | Use it for |
| --- | --- |
| [`simple`](https://docs.rs/vst3-host/latest/vst3_host/simple/) | One-call helpers: `load_plugin`, `play`, `discover_plugins`, `get_plugin_info`. The fastest start. |
| [`Vst3Host`](https://docs.rs/vst3-host/latest/vst3_host/host/struct.Vst3Host.html) | Configured hosting: sample rate, block size, isolation, custom scan paths. Built via `Vst3Host::builder()`. |
| [`get_detailed_plugin_info`](https://docs.rs/vst3-host/latest/vst3_host/fn.get_detailed_plugin_info.html) | Deep introspection (factory, classes, bus layout) for inspector-style UIs. |

## Working with a plugin

| Type | Use it for |
| --- | --- |
| [`Plugin`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html) | The loaded plugin: parameters, MIDI, processing, editor, `save_state`/`load_state`, `take_output_midi`, and (isolated) `recover`/`isolation_pid`. Also the per-note expression / MPE surface: `note_on`/`note_on_at`, `note_off`/`note_off_at`, `send_note_expression`/`send_note_expression_at`, `note_expressions` (in-process only — these error under process isolation). |
| [`PluginInfo`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.PluginInfo.html) | Metadata (name, vendor, version, category, bus counts, MIDI/audio capability). Serializable. |
| [`PluginReport`](https://docs.rs/vst3-host/latest/vst3_host/struct.PluginReport.html) | Full serializable report (`detailed` info + `parameters`) with `to_json()` — for export / tooling. |
| [`Parameter`](https://docs.rs/vst3-host/latest/vst3_host/parameters/struct.Parameter.html) | One parameter's id, name, normalized value, unit, flags. |
| [`MidiEvent`](https://docs.rs/vst3-host/latest/vst3_host/midi/enum.MidiEvent.html) / [`MidiChannel`](https://docs.rs/vst3-host/latest/vst3_host/midi/enum.MidiChannel.html) | MIDI input and captured output. The `midi::cc` module has named CC constants. |
| [`NoteId`](https://docs.rs/vst3-host/latest/vst3_host/struct.NoteId.html) | Handle returned by `Plugin::note_on`; identifies a sounding note for `note_off` and `send_note_expression`. |
| [`NoteExpressionType`](https://docs.rs/vst3-host/latest/vst3_host/enum.NoteExpressionType.html) | Which per-note expression to send: `Volume`, `Pan`, `Tuning`, `Vibrato`, `Expression`, `Brightness`, `Custom(u32)`. Values are normalized `0.0..=1.0`; `Tuning` is bipolar (`0.5` = centered). |
| [`NoteExpressionInfo`](https://docs.rs/vst3-host/latest/vst3_host/struct.NoteExpressionInfo.html) | Describes one expression the plugin advertises (via `Plugin::note_expressions`): its type, id, and value range. |

## Audio

| Type | Use it for |
| --- | --- |
| [`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html) | A running stream driving a plugin (mutex path); returned by `Vst3Host::play`, `simple::play`, `playback::play_with_backend`, and `play_with_input_backend` (live audio in). Lock-free UI-thread side channels (no audio-mutex contention): in — `send_midi`, `set_parameter`, `midi_panic`; out — `output_levels`, `drain_output_midi`, `drain_parameter_changes` (editor-driven edits); plus `try_lock` (best-effort read, `None` if the audio thread holds the lock). `lock()` / `plugin()` remain for rarer full-`Plugin` ops; `stop()` (or drop) to stop. |
| [`RealtimePluginRunner`](https://docs.rs/vst3-host/latest/vst3_host/realtime/struct.RealtimePluginRunner.html) / [`RtControl`](https://docs.rs/vst3-host/latest/vst3_host/realtime/struct.RtControl.html) | Lock-free path: owns the plugin on the audio thread; control via a lock-free queue. `Vst3Host::play_realtime` wires it to CPAL (returns `RtAudioHandle`). |
| [`play_with_backend`](https://docs.rs/vst3-host/latest/vst3_host/playback/fn.play_with_backend.html) / `play_realtime_with_backend` | Drive a plugin with any `AudioBackend` (mutex or lock-free). |
| [`CpalBackend`](https://docs.rs/vst3-host/latest/vst3_host/backends/struct.CpalBackend.html) | The bundled CPAL backend (feature `cpal-backend`). |
| [`AudioBackend`](https://docs.rs/vst3-host/latest/vst3_host/audio/trait.AudioBackend.html) / [`AudioBuffers`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.AudioBuffers.html) / [`AudioLevels`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.AudioLevels.html) | Custom backends, manual processing, metering. |
| [`PeakMeter`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.PeakMeter.html) / [`RmsWindow`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.RmsWindow.html) | UI level meters: falling peak + timed hold, windowed RMS. |

## Other

| Type | Use it for |
| --- | --- |
| [`process_isolation`](https://docs.rs/vst3-host/latest/vst3_host/process_isolation/) | Low-level isolation IPC (usually reached via the builder, not directly). |
| [`ProbeResult`](https://docs.rs/vst3-host/latest/vst3_host/host/enum.ProbeResult.html) | Result of `Vst3Host::probe_plugin` — validate a plugin (loads / crashes / times out) without risking the host. |
| [`PluginWindow`](https://docs.rs/vst3-host/latest/vst3_host/window/struct.PluginWindow.html) | Open a plugin's native editor in a standalone window. |
| [`EmbeddedEditor`](https://docs.rs/vst3-host/latest/vst3_host/embed/struct.EmbeddedEditor.html) / `EditorRect` | Embed a plugin editor inside a host (egui) window (feature `egui-widgets`, macOS). |
| [`Error`](https://docs.rs/vst3-host/latest/vst3_host/error/enum.Error.html) / [`Result`](https://docs.rs/vst3-host/latest/vst3_host/error/type.Result.html) | Error handling. `Result<T> = std::result::Result<T, Error>`. |

## The prelude

`use vst3_host::prelude::*;` re-exports the common types. Note it does **not** export
`Result` — that would shadow `std::result::Result` and break `Result<T, E>` in your code.
Refer to the crate's result type explicitly as `vst3_host::Result`.
