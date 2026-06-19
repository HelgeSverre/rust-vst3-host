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
| [`Plugin`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html) | The loaded plugin: parameters, MIDI, processing, editor. |
| [`PluginInfo`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.PluginInfo.html) | Lightweight metadata (name, vendor, bus counts, capabilities). |
| [`Parameter`](https://docs.rs/vst3-host/latest/vst3_host/parameters/struct.Parameter.html) | One parameter's id, name, normalized value, unit, flags. |
| [`MidiEvent`](https://docs.rs/vst3-host/latest/vst3_host/midi/enum.MidiEvent.html) / [`MidiChannel`](https://docs.rs/vst3-host/latest/vst3_host/midi/enum.MidiChannel.html) | MIDI input. The `midi::cc` module has named CC constants. |

## Audio

| Type | Use it for |
| --- | --- |
| [`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html) | A running stream driving a plugin. `lock()` to control it, drop to stop. |
| [`play_with_backend`](https://docs.rs/vst3-host/latest/vst3_host/playback/fn.play_with_backend.html) | Drive a plugin with any `AudioBackend`. |
| [`CpalBackend`](https://docs.rs/vst3-host/latest/vst3_host/backends/struct.CpalBackend.html) | The bundled CPAL backend (feature `cpal-backend`). |
| [`AudioBackend`](https://docs.rs/vst3-host/latest/vst3_host/audio/trait.AudioBackend.html) / [`AudioBuffers`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.AudioBuffers.html) / [`AudioLevels`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.AudioLevels.html) | Custom backends, manual processing, metering. |

## Other

| Type | Use it for |
| --- | --- |
| [`process_isolation`](https://docs.rs/vst3-host/latest/vst3_host/process_isolation/) | Low-level isolation IPC (usually reached via the builder, not directly). |
| [`PluginWindow`](https://docs.rs/vst3-host/latest/vst3_host/window/struct.PluginWindow.html) | Open a plugin's native editor in a standalone window. |
| [`Error`](https://docs.rs/vst3-host/latest/vst3_host/error/enum.Error.html) / [`Result`](https://docs.rs/vst3-host/latest/vst3_host/error/type.Result.html) | Error handling. `Result<T> = std::result::Result<T, Error>`. |

## The prelude

`use vst3_host::prelude::*;` re-exports the common types. Note it does **not** export
`Result` — that would shadow `std::result::Result` and break `Result<T, E>` in your code.
Refer to the crate's result type explicitly as `vst3_host::Result`.
