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
  timeout error and the helper is killed, instead of blocking your thread forever.

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
    ProbeResult::Crashed => println!("crashes on load — blacklist it"),
    ProbeResult::TimedOut => println!("hung on load"),
    ProbeResult::Failed(msg) => println!("failed: {msg}"),
}
# Ok(())
# }
```

## Auto-isolate crash-prone plugins

Some plugins (e.g. Waves/WaveShell) crash from their *own* packaging and can't be loaded
safely in-process. Let the host route those to isolation automatically, so a crash becomes
a returned `Err` instead of a dead host:

```rust
# use vst3_host::Vst3Host;
# fn main() -> vst3_host::Result<()> {
let mut host = Vst3Host::builder()
    .auto_isolate_problematic(true)   // Waves etc. load isolated; others stay in-process
    .build()?;
// host.load_plugin("…WaveShell….vst3") now returns Err if it crashes — the host survives.
# Ok(())
# }
```

## Current limits

- **Not the runtime default.** The default load path is in-process. Isolation is opt-in
  because it requires the helper binary to be present where your app runs.
- **No GUI across the boundary.** Opening a plugin's editor in isolated mode is not
  supported yet (`open_editor` returns an error).
- **No automatic respawn.** After a crash you create a new isolated plugin to recover.

See [Process isolation](../explanation/process-isolation.md) for how the IPC protocol
works and why these limits exist.
