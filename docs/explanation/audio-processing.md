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

## What this model does *not* guarantee yet

The current implementation is **correctness-first, not real-time-tuned**. Be aware:

- **The audio callback takes a mutex.** Locking on the audio thread risks priority
  inversion and, under contention, dropouts. It's fine for interactive use and development;
  it is not a hard-real-time guarantee.
- **Some allocation happens off the steady-state path.** The scratch buffer is reused, but
  the design hasn't been audited for zero-allocation on the audio thread.
- **No dedicated real-time thread or lock-free parameter queue.** Those are the standard
  next steps for professional low-latency hosting and are not implemented.

If you need hard real-time behavior, drive `process_audio` from your own audio thread with
your own lock-free control plumbing rather than relying on `lock()`.

## Metering

After each block, [`get_output_levels`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.get_output_levels)
returns per-channel peak/RMS/peak-hold. It recovers gracefully if the audio thread panicked
while holding the levels lock, so polling it from a UI thread can't itself cause a panic.
