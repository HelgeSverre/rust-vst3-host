# Schedule sample-accurate parameter automation

Move a parameter at exact sample offsets *within* a block, instead of one value per block.
Parameters are normalized `0.0..=1.0`.

## The building block

[`set_parameter`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.set_parameter)
applies a value at the start of the next processed block. For finer timing, use
[`set_parameter_at`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.set_parameter_at),
which schedules a change at a sample offset *inside* the next `process_audio`:

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
plugin.set_parameter_at(0, 0.75, 256)?;   // param 0 reaches 0.75 at frame 256 of the next block
# Ok(())
# }
```

The offset is clamped to the block; keep it within the upcoming block's frame count
(`plugin.block_size()` is the maximum). Call it once per sub-block point — the plugin
receives each change at its offset.

## Build a curve with `ParameterAutomation`

[`ParameterAutomation`](https://docs.rs/vst3-host/latest/vst3_host/parameters/struct.ParameterAutomation.html)
holds time/value points and interpolates between them. Build it with
[`add_point(time_secs, value)`](https://docs.rs/vst3-host/latest/vst3_host/parameters/struct.ParameterAutomation.html#method.add_point),
pick an interpolation with `with_curve` (`Linear`, `Exponential`, `Logarithmic`, `Step`),
and optionally `with_loop`:

```rust
use vst3_host::parameters::{ParameterAutomation, AutomationCurve};

let cutoff = ParameterAutomation::new()
    .add_point(0.0, 0.2)    // at 0 s, value 0.2
    .add_point(1.0, 0.9)    // ramp to 0.9 over one second
    .add_point(2.0, 0.2)    // and back down
    .with_curve(AutomationCurve::Linear)
    .with_loop(true);
```

Points are kept sorted by time. `value_at_time(t)` reads a single value off the curve;
`points_for_block(...)` samples a whole block at once (below).

## Drive a block from the curve

[`points_for_block`](https://docs.rs/vst3-host/latest/vst3_host/parameters/struct.ParameterAutomation.html#method.points_for_block)
samples the curve across one audio block and returns `(sample_offset, value)` pairs ready
to feed straight into `set_parameter_at`:

```rust
fn points_for_block(
    &self,
    block_start_secs: f64,   // where this block starts on the automation timeline
    frames: usize,           // block length in samples
    sample_rate: f64,
    points_per_block: usize, // sub-block resolution (1 = one value at block start)
) -> Vec<(i32, f64)>
```

`points_per_block` controls how many points it emits inside the block: `1` writes a single
value at the block start, higher values give finer ramps (capped at `frames`).

The loop that ties it together — sample the curve, then schedule each point:

```rust
# use vst3_host::{simple, parameters::{ParameterAutomation, AutomationCurve}};
# fn main() -> vst3_host::Result<()> {
let mut plugin = simple::load_plugin("/path/to/synth.vst3")?;
plugin.start_processing()?;

let param_id = 0;
let sample_rate = plugin.sample_rate();
let frames = plugin.block_size();

let curve = ParameterAutomation::new()
    .add_point(0.0, 0.0)
    .add_point(1.0, 1.0)
    .with_curve(AutomationCurve::Exponential)
    .with_loop(true);

// Advance the automation timeline one block at a time.
let mut block_start = 0.0_f64;
loop {
    // 8 points per block = smooth ramp; tune for your plugin and CPU budget.
    for (offset, value) in curve.points_for_block(block_start, frames, sample_rate, 8) {
        plugin.set_parameter_at(param_id, value, offset)?;
    }

    let mut buffers = vst3_host::audio::AudioBuffers::new(0, 2, frames, sample_rate);
    plugin.process_audio(&mut buffers)?;
    // ... consume buffers.outputs ...

    block_start += frames as f64 / sample_rate;
#   break;
}
# Ok(())
# }
```

Call `points_for_block` and `set_parameter_at` *before* each `process_audio`: the changes
are queued for the block that call processes.

## While playing through an audio device

Inside an [`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html),
schedule through the lock. The host drives `process_audio` for you, so emit points for the
upcoming block from your control thread:

```rust
# use vst3_host::{simple, parameters::ParameterAutomation};
# fn main() -> vst3_host::Result<()> {
# let audio = simple::play(simple::load_plugin("/x.vst3")?)?;
# let curve = ParameterAutomation::new().add_point(0.0, 0.5);
# let (block_start, frames, sr) = (0.0, 512, 48000.0);
for (offset, value) in curve.points_for_block(block_start, frames, sr, 8) {
    audio.lock().set_parameter_at(0, value, offset)?;
}
# Ok(())
# }
```

## Caveats

- **Stay in range.** `set_parameter_at` rejects values outside `0.0..=1.0`; the offset is
  clamped to the block, so out-of-block offsets land at the edge rather than erroring.
- **Process isolation is supported.** Under [process isolation](../explanation/process-isolation.md)
  the sample offset **is** carried across the boundary (`HostCommand::SetParameterAt`) and
  applied by the helper's in-process plugin — the change lands at its offset, not at block
  start. (This differs from `send_midi_event_at`, whose offset is not yet marshalled.)
- **`points_for_block` returns empty** for an automation with no points or a zero-length
  block — the loop above simply schedules nothing.

## Related

- [`send_midi_event_at`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.send_midi_event_at)
  schedules a MIDI event at a sample offset within the next block — the MIDI counterpart to
  `set_parameter_at`. See [Send MIDI](send-midi.md).
- The [`parameter_automation` example](https://github.com/Helge-Sverre/vst-host/blob/main/vst3-host/examples/parameter_automation.rs)
  drives several curve shapes against a live plugin.
