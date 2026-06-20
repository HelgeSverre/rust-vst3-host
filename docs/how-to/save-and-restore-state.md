# Save and restore plugin state

Capture a plugin's full state (parameters, internal settings, loaded preset) to bytes and
restore it later — for presets, sessions, or undo.

## Save and restore

```rust
use vst3_host::simple;

# fn main() -> vst3_host::Result<()> {
let mut plugin = simple::load_plugin("/path/to/synth.vst3")?;

// Save: an opaque byte blob the plugin produced via its own getState.
let state: Vec<u8> = plugin.save_state()?;

// ... change parameters, load a different preset, etc ...

// Restore exactly what was saved.
plugin.load_state(&state)?;
# Ok(())
# }
```

`save_state` returns the plugin's own serialized state — **treat the bytes as opaque** and
pair them with the plugin's identity ([`PluginInfo::uid`](https://docs.rs/vst3-host)); they
only mean something to the same plugin. Persist them however you like (file, database, your
session format).

## Notes

- **Call on the main thread.** State maps to the plugin's `getState`/`setState`; do it on the
  thread you load and drive the plugin from, not the audio thread.
- **Works under process isolation too.** The blob marshals across the boundary, so an
  isolated plugin saves/restores just like an in-process one.
- **Use it with crash recovery.** [`Plugin::recover()`](isolate-plugin-crashes.md) reloads an
  isolated plugin from its *default* state — snapshot with `save_state` first and `load_state`
  after to keep the user's settings.
- **Different plugins reject foreign bytes.** Passing state from plugin A to plugin B is
  undefined (the plugin decides what to do with bytes it doesn't recognize).

To export human-readable metadata (not the opaque state) for tooling, see `PluginReport` /
the inspector's "Copy JSON" instead.
