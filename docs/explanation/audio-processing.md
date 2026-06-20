# Audio processing

How audio flows from a device, through a plugin, and back — and what the current model
does and doesn't guarantee.

## The flow

A plugin processes audio in **deinterleaved** form: separate buffers per channel
([`AudioBuffers`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.AudioBuffers.html),
`outputs[channel][frame]`). Audio devices work in **interleaved** form
(`[L, R, L, R, ...]`). The bridge between them, per device callback:

1. Take the device's interleaved output buffer.
2. Figure out the frame count and zero a reusable scratch buffer.
3. Call `plugin.process_audio(&mut scratch)`.
4. Interleave `scratch.outputs` back into the device buffer.

`Vst3Host::play` and `play_with_backend` do this for you. You can also call
`process_audio` directly for offline rendering or custom routing.

## Variable block sizes

The device decides how many frames it wants per callback, and it can vary (especially with
`BufferSize::Default`). A plugin is configured at setup with a *maximum* block size, but
each `process` call may ask for fewer frames. `process_audio` sets the per-call frame count
accordingly and copies only that many samples. If you call it yourself, size your
`AudioBuffers` to the block you want rendered (up to the configured maximum).

## Parameter and MIDI changes during playback

The plugin runs on the audio thread (inside the device callback). Your control thread
reaches it through `AudioHandle::lock()`, which takes the same mutex the audio callback
uses. A `set_parameter` or `send_midi_note` call therefore briefly contends with the audio
thread for the lock; the change is applied on the next block.

## Two paths: convenient (mutex) vs. real-time (lock-free)

There are two ways to drive a plugin, and you pick by how much you care about the audio
thread blocking:

- **`Vst3Host::play` / `simple::play` → `AudioHandle`** (above). Correctness-first and easy:
  the callback locks the plugin, so control calls briefly contend for that lock. Great for
  inspectors, tools, development, and "just hear it." **Not** a hard-real-time guarantee.
- **`Vst3Host::play_realtime` / `RealtimePluginRunner` → `RtAudioHandle`.** The runner *owns*
  the plugin on the audio thread; MIDI and parameter changes cross from your control thread
  over a **lock-free SPSC ring** ([`RtControl`](https://docs.rs/vst3-host)) and are applied at
  the start of each block. The callback never takes a lock a control thread could hold, so
  `set_parameter`/`send_midi` can't block it.

```rust,no_run
# use vst3_host::{Vst3Host, midi::{MidiEvent, MidiChannel}};
# fn main() -> vst3_host::Result<()> {
let mut host = Vst3Host::new()?;
let plugin = host.load_plugin("/path/synth.vst3")?;
let mut audio = host.play_realtime(plugin, 1024)?; // 1024 = command-queue capacity
audio.control().send_midi(MidiEvent::NoteOn { channel: MidiChannel::Ch1, note: 60, velocity: 100 });
# Ok(())
# }
```

### Still correctness-first, not fully RT-audited

Even the lock-free runner isn't a hard-real-time guarantee yet:

- **Allocation hasn't been fully audited.** The scratch buffer is reused, but the steady-state
  path isn't proven zero-allocation.
- **The plugin's internal event list still uses an (uncontended) mutex.** Only the audio
  thread touches it under the runner, so there's no cross-thread contention, but it isn't
  strictly lock-free internally.

The runner removes the *cross-thread* lock (the big win); the remaining items are refinements
for strict low-latency work.

## Metering

After each block, [`get_output_levels`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.get_output_levels)
returns per-channel peak/RMS/peak-hold. It recovers gracefully if the audio thread panicked
while holding the levels lock, so polling it from a UI thread can't itself cause a panic.
