# Play a plugin through an audio device

Drive a loaded plugin to the default audio output. Requires the `cpal-backend` feature
(on by default).

## The quick way

```rust
use vst3_host::simple;

# fn main() -> vst3_host::Result<()> {
let plugin = simple::load_plugin("/path/to/synth.vst3")?;
let audio = simple::play(plugin)?;   // opens the default output, starts processing
// ... audio plays until `audio` is dropped ...
# Ok(())
# }
```

## With a configured host

To set the sample rate or block size, build a host and call `play` on it:

```rust
use vst3_host::Vst3Host;

# fn main() -> vst3_host::Result<()> {
let mut host = Vst3Host::builder().sample_rate(48000.0).block_size(512).build()?;
let plugin = host.load_plugin("/path/to/synth.vst3")?;
let audio = host.play(plugin)?;
# let _ = audio;
# Ok(())
# }
```

## The AudioHandle

`play` returns an [`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html).
It owns the running stream and the plugin:

- **Keep it alive.** Dropping the handle stops audio. Store it in your app state.
- **Control the plugin while it plays** with `audio.lock()`, which returns a guard you can
  call any `Plugin` method on:

  ```rust
  # use vst3_host::{simple, midi::MidiChannel};
  # fn main() -> vst3_host::Result<()> {
  # let audio = simple::play(simple::load_plugin("/x.vst3")?)?;
  audio.lock().send_midi_note(60, 100, MidiChannel::Ch1)?;
  audio.lock().set_parameter(0, 0.5)?;
  let levels = audio.lock().get_output_levels();
  # let _ = levels;
  # Ok(())
  # }
  ```

- **Stop** by dropping it (`drop(audio)`) or calling `audio.stop()`.

## How the audio gets there

`play` deinterleaves the device's output buffer into per-channel buffers, calls the
plugin's `process_audio`, and interleaves the result back. The device may request varying
block sizes; the bridge handles that. See [Audio processing](../explanation/audio-processing.md)
for the model and its current limits (the audio path is correctness-first, not yet tuned
for the lowest latency).

## Effects vs. instruments

`play` works for both. An instrument produces sound when you send it MIDI notes. An effect
processes its audio input — but `play` feeds it silence, so to hear an effect you need an
input source, which means a [custom backend](custom-audio-backend.md) with a duplex stream.
