# Threading model

Which thread each operation belongs on, and why. The library checks the captured loading
thread for selected controller operations, including MIDI learn and keyswitch queries.
It cannot portably identify your application's main thread, so loading on that thread
and respecting the remaining thread contracts is your responsibility as the host. Breaking them usually shows up as a crash or hang inside the
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
| `take_parameter_edits` / `take_host_notifications` / `take_restart_flags` | Main | Same — see [the polling note](#polling-the-plugins-requests). |
| `send_midi_*` | Main (or via `AudioHandle`) | See the playback note below. |
| `process_audio` | Audio | The only call that belongs on the audio thread. |

Load on the thread you'll drive the GUI from. A plugin loaded on a worker thread may create
its controller's resources on the wrong thread and crash when you later open its editor.

## The load thread *is* the UI thread, as far as the plugin is concerned

The library has no other way to learn which thread is yours, so it captures the loading
thread and treats that as the UI thread for the rest of the plugin's life. This matters in
one visible place: **component↔controller messages**.

A plugin split into separate component and controller halves passes data between them over
`IConnectionPoint`. VST3 requires those messages to be delivered on the UI thread, so the
host sits in the middle as a proxy — and the proxy refuses (and drops) any `notify` that
arrives from another thread, exactly as the SDK's reference host does. Plugins that push
meter levels or waveform frames from inside `process()` are doing that, so those updates
never reach their editor; such editors normally fall back to polling and look fine.

Two consequences:

- Load on your GUI thread. If you load on a worker, the gate follows the worker and messages
  from your actual GUI thread get dropped instead.
- Dropped messages are not silent. The first drop and every 256th are logged at `warn`, with
  a running total logged when the plugin unloads. A frozen meter with no log line is a
  different bug.

The library does not queue these messages onto your UI thread: that would need an owned copy
of a plugin-provided COM object plus a pump you are not required to run.

## During playback

`Vst3Host::play` / `play_with_backend` move the `Plugin` into an
[`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/struct.AudioHandle.html), an
`Arc<Mutex<Plugin>>`. The audio callback locks that mutex to call `process_audio`; your
control thread reaches the plugin through `AudioHandle::lock()`, which takes the **same**
mutex. So while playing:

- Use the side channels to send MIDI and parameter changes from another thread. Taking
  the mutex prevents concurrent processing but does not move controller calls to the UI thread.
- You don't have to take that lock for the common cases, though. `AudioHandle` ships
  built-in **lock-free side channels** — `send_midi` / `set_parameter` / `midi_panic` in,
  `output_levels` / `drain_output_midi` / `drain_parameter_changes` out, plus `try_lock` for
  best-effort reads — so you no longer need to roll your own lock-free plumbing to avoid the
  audio-thread lock. `lock()` remains for the rarer full-`Plugin` operations.
- The mutex path is fine for interactive use; it is not a hard-real-time guarantee. See
  [audio processing](audio-processing.md) for the trade-off and the fully lock-free
  `play_realtime` path.

## Polling the plugin's requests

A plugin talks back to the host through two queues, and you drain both from your UI loop:

- `take_parameter_edits` — the editor's `begin`/`change`/`end` gestures, in order.
- `take_host_notifications` — control-plane requests: "my state is dirty", "please open my
  editor", group-edit brackets, unit/program-list changes, progress reports, context menus, bus activation requests.

**Drain `take_host_notifications` regularly.** Unlike the other queues, a full one is
reported back to the plugin (`kResultFalse`), so a host that never polls eventually makes the
plugin's own calls start failing. The cap is 1024 pending notifications.

The two queues are **ordered only within themselves**. There is no recorded interleaving
between them, so a `GroupEditStarted` / `GroupEditFinished` pair cannot be matched to the
specific parameter edits it brackets — only read as "a multi-parameter change happened
somewhere between these two drains" (useful for coalescing undo, not for delimiting).

## Process isolation changes the picture

When a plugin runs [isolated](process-isolation.md), the plugin's threading rules apply
inside the **helper process**, not yours. Your `Plugin` handle just serializes JSON commands
over a pipe, guarded by an internal mutex, so you can call it from any thread — but only one
call is in flight at a time, and `process_audio` still pays the IPC round-trip per block.

## Why there is no global main-thread assertion

A portable "are we on the main thread?" check doesn't exist in safe Rust, and a wrong guess
would either crash or falsely reject a valid setup. Rather than ship a misleading
`debug_assert`, the library checks the captured loading thread where possible and leaves
identifying the application main thread to the host.

On macOS, the isolation helper loads and controls the plugin on its AppKit main thread;
only audio processing runs on the stdin worker. Controller calls and native editor
callbacks therefore share the same thread.

For live input, `midi_input::bind_to_handle` uses `AudioHandle::send_live_midi`.
Call `AudioHandle::service_host_requests` on the loading/UI thread each tick to deliver
queued learn notifications and refresh changed mappings. The learn queue holds 256
controller notifications and replaces the oldest on overflow. Sequenced `send_midi`
calls do not trigger learn. Service calls briefly hold the audio mutex.

A `HostNotification::BusActivationRequested` is a routing request. Inspect it and use
`Plugin::set_bus_active` under that API's stopped-processing requirements when accepting;
queueing the notification does not change the audio layout.
