# API overview

A map of the public API. Full signatures and per-item docs are on
[docs.rs](https://docs.rs/vst3-host) — this page orients you to the main types and where to
reach for each.

## Entry points

| Type / module | Use it for |
| --- | --- |
| [`simple`](https://docs.rs/vst3-host/latest/vst3_host/simple/) | One-call helpers: `load_plugin`, `play`, `discover_plugins`, `get_plugin_info`. The fastest start. |
| [`Vst3Host`](https://docs.rs/vst3-host/latest/vst3_host/host/struct.Vst3Host.html) | Configured hosting: sample rate, block size, isolation, custom scan paths. Built via `Vst3Host::builder()`. |
| [`get_detailed_plugin_info`](https://docs.rs/vst3-host/latest/vst3_host/fn.get_detailed_plugin_info.html) | Deep introspection (factory, classes, bus layout, declared `moduleinfo.json` metadata) for inspector-style UIs. |
| [`discovery`](https://docs.rs/vst3-host/latest/vst3_host/discovery/) | Static module metadata without loading code: `read_module_info` (validated `moduleinfo.json`), `get_plugin_compatibility` (retired class ids a plugin supersedes), `discover_plugin_snapshots` (standard UI snapshot PNGs). Not re-exported at the crate root — reach them as `vst3_host::discovery::*`. |
| [`Vst3Host::discover_plugins_safe`](https://docs.rs/vst3-host/latest/vst3_host/host/struct.Vst3Host.html#method.discover_plugins_safe) / [`discovery::probe_plugin_info_isolated`](https://docs.rs/vst3-host/latest/vst3_host/discovery/fn.probe_plugin_info_isolated.html) | Crash-resistant discovery: introspect each plugin in a throwaway `vst3-host-probe` process so a crashing plugin is skipped, not fatal. Returns a [`SafeDiscoveryReport`](https://docs.rs/vst3-host/latest/vst3_host/struct.SafeDiscoveryReport.html) (`plugins` + `skipped`, each a [`SafeDiscoverySkip`](https://docs.rs/vst3-host/latest/vst3_host/enum.SafeDiscoverySkip.html)). |

## Working with a plugin

| Type | Use it for |
| --- | --- |
| [`Plugin`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html) | The loaded plugin: parameters, MIDI, processing, editor, `save_state`/`load_state`, `take_output_midi`, and (isolated) `recover`/`isolation_pid`. Program selection (`get_units`, `select_program`), bus activation (`set_bus_active`), runtime transport (`set_tempo`/`set_time_signature`/`set_playing`), and parameter-edit gesture capture (`take_parameter_edits`). Also the per-note expression / MPE surface: `note_on`/`note_on_at`, `note_off`/`note_off_at`, `send_note_expression`/`send_note_expression_at`, `note_expressions` (works in-process and under process isolation). |
| [`Plugin`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html) — talking back | What the plugin asks the host for: `take_host_notifications` (dirty state, "open my editor", group edits, unit/program-list changes, progress, context menus — **drain this regularly**), `take_restart_flags` + `service_host_requests` (latency/IO changes), `take_data_exchange_blocks`, `take_output_events` / `output_event_handle`. |
| [`Plugin`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html) — editor & units | Sizing (`editor_can_resize`, `resize_editor`, `set_editor_scale_factor`), unit selection (`selected_unit`, `select_unit`, `program_pitch_names`), per-unit state (`get_program_data`/`set_program_data`, `get_unit_data`/`set_unit_data`), host-driven edits (`begin_host_edit`/`end_host_edit`), `send_midi_learn`, `set_automation_state`, `remap_parameter_id`, `midi_cc_to_parameter`. |
| [`PluginEvent`](https://docs.rs/vst3-host/latest/vst3_host/midi/struct.PluginEvent.html) / [`PluginEventData`](https://docs.rs/vst3-host/latest/vst3_host/midi/enum.PluginEventData.html) | The full VST3 event surface under `MidiEvent`: SysEx (`Plugin::send_sysex`), note-expression text, chord and scale events, in both directions. |
| [`HostNotification`](https://docs.rs/vst3-host/latest/vst3_host/plugin/enum.HostNotification.html) / [`ContextMenuItem`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.ContextMenuItem.html) / [`DataExchangeBlock`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.DataExchangeBlock.html) | What `take_host_notifications` / `take_data_exchange_blocks` hand you. |
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
| [`AudioBusLayout`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.AudioBusLayout.html) / [`BusAudioBuffers`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.BusAudioBuffers.html) | Address a plugin's individual buses (sidechain in, multi-out) instead of one flattened channel list: `Plugin::audio_bus_layout`, `create_bus_audio_buffers`, `process_bus_audio`. |
| [`PeakMeter`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.PeakMeter.html) / [`RmsWindow`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.RmsWindow.html) | UI level meters: falling peak + timed hold, windowed RMS. |

## Other

| Type | Use it for |
| --- | --- |
| [`process_isolation`](https://docs.rs/vst3-host/latest/vst3_host/process_isolation/) | Low-level isolation IPC (usually reached via the builder, not directly). |
| [`ProbeResult`](https://docs.rs/vst3-host/latest/vst3_host/host/enum.ProbeResult.html) | Result of `Vst3Host::probe_plugin` — validate a plugin (loads / crashes / times out) without risking the host. |
| [`PluginWindow`](https://docs.rs/vst3-host/latest/vst3_host/window/struct.PluginWindow.html) | Open a plugin's native editor in a standalone window. |
| [`EmbeddedEditor`](https://docs.rs/vst3-host/latest/vst3_host/embed/struct.EmbeddedEditor.html) / `EditorRect` | Embed a plugin editor inside a host (egui) window (feature `egui-widgets`, macOS). |
| [`midi_input`](https://docs.rs/vst3-host/latest/vst3_host/midi_input/) | Bind a hardware/virtual MIDI input port and forward events into a running `AudioHandle` (feature `midi-input`). `list_midi_input_ports`, `connect`, `bind_to_handle`. |
| [`Error`](https://docs.rs/vst3-host/latest/vst3_host/error/enum.Error.html) / [`Result`](https://docs.rs/vst3-host/latest/vst3_host/error/type.Result.html) | Error handling. `Result<T> = std::result::Result<T, Error>`. |

## The prelude

`use vst3_host::prelude::*;` re-exports the common types. Note it does **not** export
`Result` — that would shadow `std::result::Result` and break `Result<T, E>` in your code.
Refer to the crate's result type explicitly as `vst3_host::Result`.

## Transport and controller metadata

`TransportPosition { samples, quarter_notes }` describes the start of the next block.
Use `Plugin::set_transport_position` to seek, including signed preroll positions;
`AudioHandle` and `RtControl` expose queued setters. `transport_position` reports the
next block position. Tempo changes preserve accumulated beats, stopped transport freezes
project position, and `Timeline::drive_block` supplies its own tempo and seek position.
Only populated process-context fields are advertised as valid.

`Plugin::midi_controller_assignments(direction)` returns owned mappings, preferring
VST3 Mapping 2 and falling back to Mapping 1. `MidiController` represents MIDI 1 numbers
or registered/assignable bank/index addresses. `send_midi_controller_at` applies a
normalized value to every matching parameter at the supplied block offset.
Output assignments are metadata; this does not add UMP device parsing or automatic
parameter-to-device feedback.

`notify_live_midi_controller` delivers an immediate UI-thread learn notification,
preferring Learn 2 with a MIDI 1 fallback. `send_live_midi_event` queues DSP input and
bounded deferred learning; call `service_host_requests` on the loading thread.
`keyswitches(bus, channel)` returns owned `KeyswitchInfo` metadata, or an empty list
when the optional interface is absent. These APIs also work with process isolation.

`latency_samples` and `tail_samples` read cached values collected during setup and
restart servicing while the component is inactive. Poll and service restart requests
to keep those values current.
