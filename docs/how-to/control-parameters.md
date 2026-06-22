# Control parameters

Read, set, and display a plugin's parameters. All parameter values are **normalized** to
`0.0–1.0`; the plugin maps that to its real range internally.

## List parameters

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
let plugin = simple::load_plugin("/path/to/plugin.vst3")?;
for p in plugin.get_parameters()? {
    println!("{} (id {}) = {:.3}", p.name, p.id, p.value);
}
# Ok(())
# }
```

Each [`Parameter`](https://docs.rs/vst3-host/latest/vst3_host/parameters/struct.Parameter.html)
has `id`, `name`, `value` (normalized), `unit`, `step_count`, and flags like `can_automate`
and `is_bypass`.

## Read and set a value

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
let v = plugin.get_parameter(0)?;       // by id
plugin.set_parameter(0, 0.75)?;
plugin.set_parameter_by_name("Cutoff", 0.5)?;
# let _ = v;
# Ok(())
# }
```

## Display a value the way the plugin does

`value` is a raw `0.0–1.0` number. To show what the plugin's own UI would show, ask the
plugin to format it with `format_parameter`:

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
let id = 0;
let normalized = plugin.get_parameter(id)?;
let shown = plugin.format_parameter(id, normalized)?;   // e.g. "440.00 Hz", "Sine"
# let _ = shown;
# Ok(())
# }
```

`Parameter::format_value` exists as an offline fallback, but it can only approximate
without the plugin's mapping — prefer `format_parameter`.

## While the plugin is playing

If the plugin is running inside an [`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html),
the lock-free path is `audio.set_parameter(id, value)` — it queues the (normalized) change
for the next block without contending on the audio mutex (returns `false` if the queue is
full):

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let audio = simple::play(simple::load_plugin("/x.vst3")?)?;
audio.set_parameter(0, 0.5);          // non-blocking
audio.lock().set_parameter(0, 0.5)?;  // full-lock alternative
# Ok(())
# }
```

To read changes the plugin's *own editor* makes while it plays, drain them off the playing
path without locking with `audio.drain_parameter_changes()`:

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let audio = simple::play(simple::load_plugin("/x.vst3")?)?;
for (id, value) in audio.drain_parameter_changes() {
    println!("editor changed param {id} -> {value}");
}
# Ok(())
# }
```

## Listen for changes the plugin makes

Some plugins change their own parameters (e.g. from their GUI). Register a callback, or
drain the changes:

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
plugin.on_parameter_change(|id, value| println!("param {id} -> {value}"));
// or, poll:
for (id, value) in plugin.get_parameter_changes() {
    println!("changed: {id} = {value}");
}
# Ok(())
# }
```
