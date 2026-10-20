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

If the *maximum* itself needs to change — or the sample rate does, e.g. the output device
switches rate mid-session — call `Plugin::reconfigure(sample_rate, block_size)`. It re-runs
the plugin's `setupProcessing` and rebuilds the audio buffers, so it must be called while the
plugin is **not** processing (`stop_processing` first, then `start_processing` again).
Reconfigure is not yet marshalled across [process isolation](process-isolation.md).

## Parameter and MIDI changes during playback

The plugin runs on the audio thread (inside the device callback). Even on the mutex path,
you don't have to take the audio mutex to reach a playing plugin: `AudioHandle` exposes
**lock-free side channels** that never contend with the callback.

- **In:** `send_midi`, `set_parameter` (normalized), and `midi_panic` queue commands over a
  ring; the audio thread drains and applies them at the start of the next block. They return
  `false` only if the ring is momentarily full.
- **Out:** `output_levels` (per-channel peak, reset on read), `drain_output_midi`, and
  `drain_parameter_changes` (parameter edits the plugin made in its own editor — see below)
  let a UI poll the plugin without blocking the audio thread.
- `try_lock` gives a best-effort full-`Plugin` read, returning `None` if the audio thread
  currently holds the lock.

`AudioHandle::lock()` is still available — it takes the **same** mutex the audio callback
uses, so a call briefly contends with the audio thread and the change lands on the next
block — but you only need it now for the rarer operations the side channels don't cover
(opening the editor, saving/loading state, full introspection).

Parameter changes a user makes **in the plugin's own editor GUI** (the plugin calling
`performEdit`) now also reach the audio processor, so the sound follows the editor; surface
those edits to your UI with `drain_parameter_changes`.

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

### One prepared engine for queued commands and direct slices

`RealtimePluginRunner::start` prepares fixed-capacity event lists, parameter queues, and
audio storage off the audio thread. `process` drains at most one command ring's capacity
and sends the resulting inputs through that prepared engine. `process_direct` accepts MIDI
and parameter slices instead, without draining the command ring. Both interfaces use the
same MIDI mappings, capacity checks, audio conversion, silence flags, transport, and VST
processor call.

```text
RtControl queue ─> runner.process ────────┐
                                       ├─> fixed collections ─┐
caller slices ───> runner.process_direct ┘                     │
                                                             ├─> shared process core ─> VST process()
ordinary Plugin ──────────────────────────> ordinary queues ───┘
```

Use `start_with_capacities` to choose explicit storage limits. Each target of a one-to-many
MIDI mapping consumes a parameter point, while repeated parameter ids share one queue.
When a block exceeds a prepared limit, processing returns an error and clears its audio
outputs instead of growing storage. Caller buffers must already have enough samples for
the requested block, whose size must not exceed the configured maximum.

The host's prepared processing path performs no allocation, deallocation, logging, or mutex
locking, including its first block. This guarantee covers the host code; a third-party
plugin can still allocate or lock inside its own processor. Process-isolated plugins are
rejected during preparation because their IPC implementation cannot provide this guarantee.

The runner owns the lifecycle: create it, start it off-thread, process blocks, then stop the
audio callback before stopping or recovering the ordinary `Plugin`. The
`Plugin::try_into_realtime` / `RealtimePlugin` interface remains available as a thin adapter
over the same runner. New callers can use direct slices on the runner without learning a
second ownership transition.

Preparation also rejects pending input from the ordinary plugin interface, including editor
edits. Process that input before moving the plugin into the runner, or recover the plugin
after the failed start and flush it through ordinary processing. Commands queued through
`RtControl` are retained for the runner's first block.

Copy-only output events and processor parameter feedback reach the plugin's bounded output
queues. SysEx, text, chord, and scale output payloads require owned storage and are refused by
the fixed event list; `RealtimeProcessReport::output_event_overflows` includes those refusals.
Take an output consumer before moving the plugin into the runner if another thread needs to
drain emitted events. A full host output queue keeps the newest feedback. Any displaced owned
payload from ordinary processing is retained in preallocated storage until off-thread teardown.
Accepted parameter and mapped-MIDI values also enter the controller's bounded feedback queue;
control-thread reads or servicing apply them while processing is paused. No controller call
runs inside either process method.

The ordinary `Plugin::process_audio` path shares the prepared audio-processing implementation
and process-data graph, with its editor and dynamically sized event/parameter handling around
that core. Its mutex-backed control services retain their ordinary threading requirements.

**Denormals are flushed** during processing: the host enables flush-to-zero / denormals-are-zero
(MXCSR on x86, FPCR on ARM) for the span of each `process` call and restores the prior FPU
state afterward, so a decaying filter/reverb tail can't drag the audio thread into denormal
CPU spikes.

## Metering

After each ordinary `Plugin::process_audio` block, [`get_output_levels`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.get_output_levels)
returns per-channel peak/RMS/peak-hold. It recovers gracefully if the audio thread panicked
while holding the levels lock, so polling it from a UI thread can't itself cause a panic.
The prepared runner bypasses this mutex-backed meter; custom realtime callers can calculate
levels from the output buffers and publish them through their own bounded channel.
