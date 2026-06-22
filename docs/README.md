# vst3-host documentation

Find what you need by what you're trying to do.

## Tutorials — learn by doing

Start here if you're new. Each tutorial works start to finish.

- **[Getting started](tutorials/getting-started.md)** — install the library, load a plugin, and hear it play.
- **[Build a mini host](tutorials/build-a-mini-host.md)** — a small program that lists parameters, sends MIDI, and reads output levels.

## How-to guides — solve a specific task

Focused recipes. They assume you've done the getting-started tutorial.

- [Discover installed plugins](how-to/discover-plugins.md)
- [Play a plugin through an audio device](how-to/play-a-plugin.md)
- [Play without locking the audio thread](how-to/realtime-playback.md)
- [Control parameters](how-to/control-parameters.md)
- [Schedule sample-accurate parameter automation](how-to/sample-accurate-automation.md)
- [Save and restore plugin state](how-to/save-and-restore-state.md)
- [Send MIDI](how-to/send-midi.md)
- [Open or embed a plugin editor](how-to/open-plugin-editor.md)
- [Monitor audio levels](how-to/monitor-audio-levels.md)
- [Isolate plugin crashes](how-to/isolate-plugin-crashes.md)
- [Use a custom audio backend](how-to/custom-audio-backend.md)

## Reference — look something up

- [API overview](reference/api-overview.md) — map of the public types; full signatures on [docs.rs](https://docs.rs/vst3-host).
- [Feature flags](reference/feature-flags.md)
- [Platform support](reference/platform-support.md)
- [Plugin compatibility](reference/compatibility.md) — how real plugins fare, by capability.

## Explanation — understand how it works

- [Architecture](explanation/architecture.md) — how safe Rust wraps the VST3 COM API.
- [Process isolation](explanation/process-isolation.md) — running plugins in a separate process.
- [Audio processing](explanation/audio-processing.md) — the audio model and its current limits.
- [Threading model](explanation/threading.md) — which thread each call belongs on, and why.
- [VST3 primer](explanation/vst3-primer.md) — the VST3 mental model, if you're new to it.

---

Working notes, design docs, and the project roadmap live in [`internal/`](internal/) — they
are not part of the user-facing documentation.
