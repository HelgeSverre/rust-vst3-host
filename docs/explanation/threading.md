# Threading model

Which thread each operation belongs on, and why. The library does **not** enforce these
rules — it can't portably tell what thread you're on — so getting them right is your
responsibility as the host. Breaking them usually shows up as a crash or hang inside the
plugin, not as a Rust panic.

## The two threads that matter

VST3 splits a plugin into a **processor** (audio) and a **controller** (parameters + GUI),
and expects a host to respect a matching thread split:

- **The main thread** owns loading, the editor GUI, and parameter/state changes. On macOS
  this is literally the AppKit main thread; the plugin's editor is a native view
  (`NSView`/`HWND`/X11 window) and the windowing toolkit requires it.
- **The audio thread** owns `process_audio` and nothing else. It runs inside the device
  callback and must never block.

## Where each call belongs

| Operation | Thread | Notes |
|-----------|--------|-------|
| `Vst3Host::load_plugin` | Main | Plugins create resources and query the host on `initialize()`. |
| `Plugin::open_editor` / `close_editor`, `PluginWindow` | Main | Native GUI; required on macOS, expected everywhere. |
| `set_parameter` / `get_parameter` / `update_parameters` | Main | Routed to the controller. |
| `save_state` / `load_state` | Main | Calls the plugin's `getState`/`setState`. |
| `get_parameter_changes` / `take_output_midi` | Main | Poll from your UI loop, e.g. once per frame. |
| `send_midi_*` | Main (or via `AudioHandle`) | See the playback note below. |
| `process_audio` | Audio | The only call that belongs on the audio thread. |

Load on the thread you'll drive the GUI from. A plugin loaded on a worker thread may create
its controller's resources on the wrong thread and crash when you later open its editor.

## During playback

`Vst3Host::play` / `play_with_backend` move the `Plugin` into an
[`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/struct.AudioHandle.html), an
`Arc<Mutex<Plugin>>`. The audio callback locks that mutex to call `process_audio`; your
control thread reaches the plugin through `AudioHandle::lock()`, which takes the **same**
mutex. So while playing:

- Sending MIDI and changing parameters from any thread is safe — the mutex serializes them
  against the audio callback, and the change lands on the next block.
- That safety costs a lock on the audio thread. It's fine for interactive use; it is not a
  hard-real-time guarantee. See [audio processing](audio-processing.md) for the trade-off
  and how to bypass it with your own lock-free plumbing.

## Process isolation changes the picture

When a plugin runs [isolated](process-isolation.md), the plugin's threading rules apply
inside the **helper process**, not yours. Your `Plugin` handle just serializes JSON commands
over a pipe, guarded by an internal mutex, so you can call it from any thread — but only one
call is in flight at a time, and `process_audio` still pays the IPC round-trip per block.

## Why there's no assertion

A portable "are we on the main thread?" check doesn't exist in safe Rust, and a wrong guess
would either crash or falsely reject a valid setup. Rather than ship a misleading
`debug_assert`, the library documents the contract and leaves enforcement to the host, which
knows its own thread layout.
