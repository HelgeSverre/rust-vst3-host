# Process isolation

A VST3 plugin is third-party native code running in your address space. A bug in it can
crash your whole application. Process isolation runs the plugin in a separate process, so a
crash is contained: the helper dies, your app gets an `Err`, and you carry on.

## How it works

When you build a host with `with_process_isolation(true)`, `load_plugin` spawns the
`vst3-host-helper` binary and returns a `Plugin` backed by `IsolatedPluginImpl` instead of
the usual in-process implementation. Every call you make — `set_parameter`, `send_midi_*`,
`process_audio`, `start_processing` — is serialized to a command, sent to the helper over
its stdin, and answered over stdout.

```
your app                          helper process
────────                          ──────────────
Plugin (IsolatedPluginImpl)  ──>  reads HostCommand from stdin
  send_command(JSON) ───────────> drives a real in-process Plugin
  <──────────── HostResponse(JSON)  (the same PluginImpl path)
```

The helper is a thin wrapper around the library's own public `Plugin` API. That's a
deliberate choice: the isolated path reuses the exact, tested in-process code rather than
a second VST3 implementation that could drift out of sync. The command and response enums
([`HostCommand`](https://docs.rs/vst3-host/latest/vst3_host/process_isolation/enum.HostCommand.html),
[`HostResponse`](https://docs.rs/vst3-host/latest/vst3_host/process_isolation/enum.HostResponse.html))
live in the library and are shared by both sides.

## Crash and hang containment

Responses are read on a background thread and delivered over a channel, so a call can wait
with a deadline (5 seconds by default):

- **Crash** — the helper's stdout closes; the next call returns an error instead of
  hanging.
- **Hang** — if the plugin stops responding, the call times out, the helper is killed, and
  you get a timeout error rather than a permanently blocked thread.

## Configuring the helper

Two builder options tune the isolated path; both are honored on the initial load and on
[`Plugin::recover()`](#recovery):

```rust
use std::time::Duration;
use vst3_host::Vst3Host;

let host = Vst3Host::builder()
    .with_process_isolation(true)
    // Slower plugins may need longer than the 5s default before a call is treated as hung.
    .response_timeout(Duration::from_secs(15))
    // Point at a helper binary in a non-standard location instead of the heuristic search.
    .helper_path("/opt/myapp/bin/vst3-host-helper")
    .build()?;
```

- **`response_timeout`** — how long a single command waits before the helper is declared hung
  and killed. Defaults to 5 seconds. Only affects isolated loads.
- **`helper_path`** — an explicit path to the `vst3-host-helper` binary, overriding the
  default search (executable dir, then common `target/` locations). The
  `VST3_HOST_HELPER_PATH` environment variable does the same. A configured path that doesn't
  exist fails fast with a clear error rather than silently falling back to the search.

## Why it's opt-in, not the default

The headline vision was "isolation by default," but defaulting it on has a real cost:
isolation needs the `vst3-host-helper` binary to be present wherever your app runs. For a
library consumed by other crates, that binary isn't automatically shipped to the end user's
machine. So the runtime default stays in-process, and isolation is something you opt into
(and ship the helper for) when crash containment matters more than the extra moving part.

The `process-isolation` feature is on by default so the helper *builds* without extra
flags — but loading is in-process unless you call `with_process_isolation(true)`.

Parameters, audio, plugin state (`save_state`/`load_state`), **MIDI the plugin emits**
(`take_output_midi`), **per-note expression / MPE** (`note_on` / `send_note_expression`),
**runtime reconfiguration** (`reconfigure`, `set_process_mode`), **bus arrangements**
(`bus_arrangements` / `set_bus_arrangements`), **units and program lists** (`get_units`), and
**latency / tail / MIDI-CC mapping** (`latency_samples`, `tail_samples`,
`midi_cc_to_parameter`) all marshal across the boundary, so an isolated `Plugin` behaves like
an in-process one for those.

## Recovery

A dead or hung helper surfaces as a typed `Error::PluginCrashed` / `Error::PluginTimeout`
(never a hang or a host crash). `Plugin::recover()` respawns the helper and reloads the
plugin; it's explicit (not inline) so the expensive respawn stays off the audio thread. See
[Isolate plugin crashes](../how-to/isolate-plugin-crashes.md).

## Current limits

- **GUI across the boundary is macOS-only** — on macOS the helper process owns the editor
  window (the helper runs its own UI run loop), so `open_editor` works and the editor stays
  in the isolated process. On Windows and Linux it isn't implemented yet and returns an error.
- **Recovery loses live state** — `recover()` reloads from the default state; re-apply
  parameters/preset with `save_state`/`load_state` if you need them preserved.
- **IPC overhead per audio block** — driving audio through isolation marshals each block
  over the pipe. It works, but it's heavier than in-process and not aimed at the lowest
  latency.

See [Isolate plugin crashes](../how-to/isolate-plugin-crashes.md) for the how-to.
