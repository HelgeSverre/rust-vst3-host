# Architecture

VST3 is a C++ COM-style API: raw pointers, manual reference counting, and `unsafe` FFI
everywhere. This library's job is to contain all of that behind a safe Rust surface, so
that callers never write `unsafe` and never see a COM pointer.

## The safe/unsafe boundary

```
your code ──> public API (Plugin, Vst3Host, AudioHandle, ...)   ← no `unsafe` here
                          │
                          ▼
              internal/ (not exported)                          ← all `unsafe` lives here
                ├── plugin_impl       VST3 COM lifecycle (in-process)
                ├── com_implementations  host-side COM objects (event lists, handlers)
                ├── module_loader     platform bundle loading (CFBundle / LoadLibrary)
                └── isolated_plugin_impl  IPC client for out-of-process plugins
```

Everything under `internal/` is private to the crate. The public modules (`plugin`,
`host`, `audio`, `midi`, `parameters`, `playback`, `discovery`, `backends`,
`process_isolation`, `window`) expose only safe types.

## The `Plugin` indirection

[`Plugin`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html) is a safe
handle that holds a boxed `PluginInternal` trait object. There are two implementations,
and the caller can't tell them apart:

- **`PluginImpl`** — runs the plugin in-process. It owns the VST3 `IComponent`,
  `IAudioProcessor`, and `IEditController`, runs the init/terminate lifecycle, and cleans
  up via RAII (`Drop`).
- **`IsolatedPluginImpl`** — forwards every call (parameters, MIDI, processing) over IPC to
  a helper process that runs a real `PluginImpl`. See [Process isolation](process-isolation.md).

Because both satisfy the same trait, `Vst3Host::load_plugin` returns the same `Plugin`
type whether or not isolation is enabled. This is why the rest of the API doesn't change
when you flip isolation on.

## The audio model

A plugin is not `Sync`, and the audio device calls back on its own thread. The bridge
([`play_with_backend`](https://docs.rs/vst3-host/latest/vst3_host/playback/fn.play_with_backend.html))
wraps the plugin in an `Arc<Mutex<Plugin>>` and hands a clone to the device callback. Each
callback locks the plugin, calls `process_audio`, and interleaves the result into the
device buffer.

[`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html)
is what you get back: it owns the stream (drop = stop) and exposes `lock()` so the control
thread can keep sending MIDI and changing parameters while the audio thread processes. This
is why, once a plugin is playing, you reach it through `audio.lock()` rather than a separate
handle. See [Audio processing](audio-processing.md) for the trade-offs of this lock-based
model.

## Discovery vs. loading

These are deliberately separate:

- **Scanning** (`scan_plugin_paths`) just walks the filesystem — fast, safe, loads nothing.
- **Inspecting** (`discover_plugins`, `get_detailed_plugin_info`) instantiates a plugin to
  read its metadata.
- **Loading** (`load_plugin`) fully initializes a plugin for use.

Keeping them apart lets a UI populate a plugin list instantly and only pay the loading cost
when the user picks one.

## Workspace shape

- `vst3-host/` — the library.
- `vst3-inspector/` — a full egui application built only on the library's public API. It's
  both a real tool and the proof that the public surface is sufficient to build a host —
  it contains no VST3/COM code of its own.
