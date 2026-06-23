# Isolate plugin crashes

Run a plugin in a separate process so that if it crashes, it takes down only a helper
process — not your application. Requires the `process-isolation` feature (on by default).

## Enable it

Opt in on the host builder. Everything else — loading, parameters, MIDI, audio — works
through the same `Plugin` API; the calls are forwarded to the helper process over IPC.

```rust
use vst3_host::Vst3Host;

# fn main() -> vst3_host::Result<()> {
let mut host = Vst3Host::builder()
    .with_process_isolation(true)
    .build()?;

let mut plugin = host.load_plugin("/path/to/sketchy-plugin.vst3")?;  // runs in a child process
let params = plugin.get_parameters()?;                                // marshaled over IPC
plugin.set_parameter(0, 0.5)?;
plugin.start_processing()?;
# let _ = params;
# Ok(())
# }
```

The convenience function `simple::load_plugin_isolated(path)` does the same with defaults.

## Requirements

The helper binary `vst3-host-helper` must exist next to your executable (or in the cargo
`target/` directory during development). It is built automatically when the
`process-isolation` feature is enabled (which it is by default). If you ship an
isolation-using app, ship the helper alongside it.

## What you get

- **Crash containment** — a plugin crash kills the helper, surfaced to you as an `Err`
  rather than a process abort.
- **Hang protection** — calls wait with a timeout (5s by default); a hung plugin yields a
  timeout error and the helper is killed, instead of blocking your thread forever. This
  includes the **load** itself: a plugin that hangs during initialization is bounded here,
  whereas an in-process `load_plugin` is a synchronous call that cannot be interrupted and
  will block the calling thread if the plugin hangs. Isolation is the only way to bound a
  hanging plugin.

## Validate a plugin before loading it

To check whether a plugin loads safely — the "validate plugins" step a scanner does —
probe it. The probe loads it in the isolated helper, so a crash is contained and reported,
never taking down your process:

```rust
use vst3_host::{Vst3Host, ProbeResult};

# fn main() -> vst3_host::Result<()> {
let host = Vst3Host::new()?;
match host.probe_plugin("/path/to/plugin.vst3") {
    ProbeResult::Ok => println!("safe to load"),
    ProbeResult::Crashed => println!("crashes on load — skip it / add to your deny-list"),
    ProbeResult::TimedOut => println!("hung on load"),
    ProbeResult::Failed(msg) => println!("failed: {msg}"),
}
# Ok(())
# }
```

## Recover from a crash

When an isolated plugin's helper dies (a crash or a hang), calls return a typed
`Error::PluginCrashed` (or `Error::PluginTimeout`) — the host process stays alive.
`Plugin::recover()` respawns the helper and reloads the plugin from the same path and
settings, restarting processing if it was running:

```rust
# use vst3_host::{Plugin, Error};
# fn keep_going(plugin: &mut Plugin) -> vst3_host::Result<()> {
if let Err(Error::PluginCrashed) = plugin.get_parameters() {
    plugin.recover()?; // respawn + reload; the plugin is usable again
}
# Ok(())
# }
```

The reloaded plugin starts from its default state — parameter values and any loaded preset
are lost. Snapshot with [`save_state`](../how-to/control-parameters.md) beforehand and
`load_state` after recovering to preserve them. `Plugin::isolation_pid()` exposes the helper
PID for monitoring.

## Current limits

- **Not the runtime default.** The default load path is in-process. Isolation is opt-in
  because it requires the helper binary to be present where your app runs.
- **GUI across the boundary is macOS-only.** On macOS, `open_editor` on an isolated plugin
  opens the editor in a window owned by the helper process (so a crash stays contained). On
  Windows and Linux it still returns an error.
- **Recovery is explicit, not inline.** A crash surfaces as `Error::PluginCrashed`; call
  `recover()` off the audio thread (it respawns + reloads, which is too slow to do inside a
  process callback).

See [Process isolation](../explanation/process-isolation.md) for how the IPC protocol
works and why these limits exist.
