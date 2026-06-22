# Use a custom audio backend

The bundled `CpalBackend` covers most cases. Implement your own backend when you need a
different audio API (JACK, an offline renderer, a test harness) or to feed an effect its
input audio.

## The trait

A backend implements [`AudioBackend`](https://docs.rs/vst3-host/latest/vst3_host/audio/trait.AudioBackend.html),
which produces an [`AudioStream`](https://docs.rs/vst3-host/latest/vst3_host/audio/trait.AudioStream.html).
The key method creates an output stream whose data callback fills an interleaved `&mut [f32]`:

```rust,ignore
use vst3_host::{AudioBackend, AudioConfig};

// Sketch — see backends/cpal_backend.rs for a complete reference implementation.
impl AudioBackend for MyBackend {
    type Stream = MyStream;
    type Device = MyDevice;
    type Error = MyError;

    fn default_output_device(&self) -> Option<Self::Device> { /* ... */ }

    fn create_output_stream(
        &self,
        device: &Self::Device,
        config: AudioConfig,
        data_callback: Box<dyn FnMut(&mut [f32]) + Send>,
        error_callback: Box<dyn FnMut(Self::Error) + Send>,
    ) -> Result<Self::Stream, Self::Error> { /* drive data_callback from your audio thread */ }

    // ...plus enumerate_output_devices, enumerate_input_devices,
    //         default_input_device, create_input_stream
}
```

The full method set is `enumerate_output_devices`, `enumerate_input_devices`,
`default_output_device`, `default_input_device`, `create_output_stream`, and
`create_input_stream`. There is no duplex method — to run an effect on live input, you don't
need a custom backend at all: [`play_with_input_backend`](https://docs.rs/vst3-host/latest/vst3_host/playback/fn.play_with_input_backend.html)
opens an input stream alongside the output and feeds captured audio through the plugin.

## Drive a plugin with it

Once you have a backend, [`play_with_backend`](https://docs.rs/vst3-host/latest/vst3_host/playback/fn.play_with_backend.html)
wires it to a plugin exactly like `Vst3Host::play` does — it returns the same
`AudioHandle`:

```rust
use vst3_host::{playback::play_with_backend, AudioConfig};

# fn run<B: vst3_host::AudioBackend>(backend: B, plugin: vst3_host::Plugin) -> vst3_host::Result<()> {
let config = AudioConfig { input_channels: 0, output_channels: 2, ..Default::default() };
let audio = play_with_backend(&backend, plugin, config)?;
# let _ = audio;
# Ok(())
# }
```

The bridge starts the plugin, then each callback deinterleaves the device buffer, calls
`plugin.process_audio`, and interleaves the output back. It keeps the channel count exactly
as you requested so the interleaving stays consistent.

## Driving the plugin yourself

For full control (offline rendering, custom routing), skip the backend entirely and call
`process_audio` directly with your own [`AudioBuffers`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.AudioBuffers.html):

```rust
# use vst3_host::{simple, AudioBuffers};
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
plugin.start_processing()?;
let mut buffers = AudioBuffers::new(0, 2, 512, 48000.0);  // 0 in, 2 out, 512 frames
plugin.process_audio(&mut buffers)?;                       // buffers.outputs now holds the audio
# Ok(())
# }
```

`process_audio` handles a block length up to the configured maximum; pass buffers sized to
the block you want rendered.
