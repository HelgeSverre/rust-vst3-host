# Monitor audio levels

Read peak and RMS levels from a processing plugin, and drive a meter UI that falls
naturally instead of freezing at the loudest sample.

## Read the levels

While a plugin is processing, ask it for the level of its last output block:

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let audio = simple::play(simple::load_plugin("/x.vst3")?)?;
let levels = audio.lock().get_output_levels();
for (i, ch) in levels.channels.iter().enumerate() {
    println!("ch{i}: peak {:.1} dB, rms {:.1} dB", ch.peak_db(), ch.rms_db());
}
if levels.is_clipping() {
    eprintln!("clipping!");
}
# Ok(())
# }
```

`get_output_levels` works on a [`Plugin`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html)
directly or through an [`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html)'s
lock while it plays. It returns an [`AudioLevels`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.AudioLevels.html):

- `channels: Vec<ChannelLevel>` — one entry per output channel.
- `is_clipping()` — true if any channel exceeded `0 dB`.

Each [`ChannelLevel`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.ChannelLevel.html) carries:

- `peak`, `rms`, `peak_hold` — linear amplitudes (`0.0–1.0`, where `1.0` = `0 dB`).
- `peak_db()`, `rms_db()` — the same in decibels (`-inf` for silence).
- `is_clipping()` — `peak > 1.0`.

Levels are computed on the audio thread per block; polling is cheap and never blocks (a
poisoned lock is recovered, not propagated).

## Why a raw peak_hold isn't a meter

`ChannelLevel::peak_hold` is **sticky**: it only ever rises, so once a loud transient hits
it stays pinned at the top forever. That's wrong for a UI meter, which should fall back
toward the current signal and only briefly hold the recent peak.

[`PeakMeter`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.PeakMeter.html) does
that for you: a falling ballistic plus a *timed* peak-hold marker that latches the loudest
value and then decays once its hold window expires.

## Drive a falling meter

Create one `PeakMeter` per channel and feed it each poll's `peak`. Time is injected, so
pass `Instant::now()` (or a synthetic instant in tests):

```rust
use std::time::{Duration, Instant};
use vst3_host::audio::PeakMeter;
# use vst3_host::simple;

# fn main() -> vst3_host::Result<()> {
# let audio = simple::play(simple::load_plugin("/x.vst3")?)?;
// 20 dB/s fall, hold the marker for 2 s. One meter per channel.
let mut meters = vec![PeakMeter::new(20.0, Duration::from_secs(2)); 2];

// Call this each UI frame:
loop {
    let levels = audio.lock().get_output_levels();
    let now = Instant::now();
    for (meter, ch) in meters.iter_mut().zip(&levels.channels) {
        meter.push(ch.peak, now);
        // meter.level()     → the falling bar value
        // meter.peak_hold() → the held marker
    }
#   break;
}
# Ok(())
# }
```

`push(block_peak, now)` snaps the level up instantly to a louder peak and decays toward
quieter input; `reset()` clears it back to silence. A typical UI uses ~20 dB/s and a 1–3 s
hold.

## Smooth RMS over a time window

`ChannelLevel::rms` resets every block, so it jitters with block size. For a steady loudness
reading use [`RmsWindow`](https://docs.rs/vst3-host/latest/vst3_host/audio/struct.RmsWindow.html),
a moving RMS over a fixed span:

```rust
use vst3_host::audio::RmsWindow;

# fn main() {
// 300 ms window at 48 kHz.
let mut rms = RmsWindow::from_duration(0.3, 48_000.0);
# let block = [0.0f32; 256];
rms.push_block(&block);   // or push_sample(x) one at a time
let _level = rms.rms();   // 0.0 until it has samples
# }
```

`RmsWindow::new(window_samples)` sizes it in samples instead. Feed it the same per-channel
audio you would meter (one window per channel).

## Caveats

- Levels only update while the plugin is processing — a stopped plugin reports its last
  block. Poll on a timer/UI frame, not faster than you'll draw.
- `PeakMeter`/`RmsWindow` are plain DSP helpers: they hold no audio-thread state and are not
  fed automatically. You push values into them from wherever you poll.
- See [Audio processing](../explanation/audio-processing.md) for how blocks reach the
  plugin, and [Play a plugin](play-a-plugin.md) for getting an `AudioHandle` in the first
  place.
