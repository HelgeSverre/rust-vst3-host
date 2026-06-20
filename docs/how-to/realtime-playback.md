# Play a plugin without locking the audio thread

[`Vst3Host::play`](play-a-plugin.md) is easy but locks the plugin on the audio callback. For
a lower-latency path where the audio thread never blocks on a lock a control thread holds,
use the **real-time runner**. Requires `cpal-backend` (on by default).

## The quick way

```rust
use vst3_host::{Vst3Host, midi::{MidiEvent, MidiChannel}};

# fn main() -> vst3_host::Result<()> {
let mut host = Vst3Host::new()?;
let plugin = host.load_plugin("/path/to/synth.vst3")?;

// 1024 = how many MIDI/parameter commands can queue between audio callbacks.
let mut audio = host.play_realtime(plugin, 1024)?;

// Queue control changes from any thread — these never block the audio callback.
audio.control().send_midi(MidiEvent::NoteOn { channel: MidiChannel::Ch1, note: 60, velocity: 100 });
audio.control().set_parameter(0, 0.5);
# Ok(())
# }
```

`play_realtime` returns an [`RtAudioHandle`](https://docs.rs/vst3-host): keep it alive (drop
to stop), and reach the plugin through `control()`, an [`RtControl`](https://docs.rs/vst3-host)
that pushes commands to a lock-free queue. `send_midi`/`set_parameter` return `false` if the
queue is full (the command is dropped, never blocking you) — size the capacity for your worst
control burst.

## Driving it yourself (custom audio thread)

If you run your own audio thread, build the runner directly and call `process` from your
callback:

```rust
use vst3_host::{simple, realtime::RealtimePluginRunner, audio::AudioBuffers};

# fn main() -> vst3_host::Result<()> {
let plugin = simple::load_plugin("/path/to/synth.vst3")?;
let (mut runner, mut control) = RealtimePluginRunner::new(plugin, 1024);
runner.start()?;

// Send `control` to your control thread; move `runner` onto the audio thread.
// In the audio callback (no locks): drain the queue + render one block.
let mut buffers = AudioBuffers::new(0, 2, 512, 48_000.0);
runner.process(&mut buffers)?;
# let _ = &mut control;
# Ok(())
# }
```

## When to use which

| | `play` (`AudioHandle`) | `play_realtime` (`RtAudioHandle`) |
| --- | --- | --- |
| Control | `lock()` the plugin directly | queue via `control()` (lock-free) |
| Audio callback | takes a mutex | takes no cross-thread lock |
| Best for | inspectors, tools, "just hear it" | latency-sensitive / live use |

Neither is a fully RT-audited (zero-allocation) engine yet — see
[Audio processing](../explanation/audio-processing.md) for the model and remaining caveats.
