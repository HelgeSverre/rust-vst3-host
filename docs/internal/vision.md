# Vision & positioning

Strategic direction for `vst3-host`, distilled from external review (GPT-5.5, 2026-06-20)
and the project's own goals. Internal note — not user-facing marketing copy yet.

## Identity

**A safe, hackable VST3 host *engine* for weird tools — not a DAW.** The one-liner framing:
*"SQLite for VST3 hosting"* — embed it, call it from Rust, never touch COM/C++, and don't
let a plugin crash your process unless you opted into danger mode.

The core promise: **"load any VST3 and safely poke it from Rust."** Don't aim at "a tiny DAW
in Rust"; aim at the engine that powers the long tail of audio/MIDI tooling.

## Target use cases (what it's *for*)

Plugin inspectors, test harnesses, tiny synth hosts, batch/offline renderers, AI/audio
automation experiments, plugin fuzzing, preset browsers, MIDI routers, "one synth in a
window" tools, small modular-rack apps. Breadth of *weird tools*, not one monolith.

## What to be honest about (don't oversell)

- **Realtime audio.** The playback bridge wraps the plugin in `Arc<Mutex<Plugin>>` and the
  device callback locks it. This is correctness-first — fine for inspectors, debugging,
  "hear the plugin," offline work — **not** a hard-realtime live-performance engine yet.
  Market as "audio playback / interactive audio," not "production low-latency."
- **Process isolation.** Genuinely useful: native third-party code can crash the host, and
  the isolated helper turns crashes/hangs into typed errors/timeouts. But the helper binary
  must ship, GUI-across-the-boundary is limited, and per-block audio IPC is heavier than
  in-process. Market as **"crash-safe probing / safer experimentation,"** not "free
  production-grade sandboxing."
- **Platforms.** macOS is exercised; Windows/Linux build + test in CI but aren't run against
  real plugins/editors. Keep platform claims conservative — audio/plugin projects die by
  "works on my machine."

## Milestones (the path)

### M1 — A brutally good plugin inspection / dev tool
Make `vst3-inspector` the flagship dogfood: discovery, detailed metadata, bus layout,
parameters with **plugin-formatted** values, MIDI in/out, editor open (standalone +
embedded), audio meters, crash/isolation status, and **"copy JSON"** export. That alone is
useful and proves the public API. (Accurate metadata + bus-aware channels — done 2026-06-20 —
feed directly into this.)

### M2 — A boringly reliable host + a compatibility matrix
Track real plugins (Dexed, Surge XT, Vital, u-he/TyrellN6, Arturia, Waves/WaveShell,
Valhalla, …) across capabilities: **loads / audio / parameters / GUI / state save / isolation
/ MIDI-out**. A living matrix builds trust faster than abstract docs. Candidate home:
`docs/reference/compatibility.md`, generated/checked by a harness (extend `--selftest`).

### M3 — A separate pro realtime path (don't uglify the simple API)
Keep `simple::play()` as the friendly mutex-based convenience API. Add a lower-level
`RealtimePluginRunner` *alongside* it: command queues, preallocated buffers, **no locks in
the audio callback**, explicit lock-free rings for parameter/audio/MIDI events. Two tiers:
nice API on top, serious API underneath.

## Near-term backlog (smaller, concrete)

- Compatibility matrix harness + `docs/reference/compatibility.md` (M2 seed).
- Inspector: "copy JSON" of full detailed plugin info (M1).
- GUI across the isolation boundary (helper-owned editor window) — in progress.
- Windows/Linux editor embedding (macOS done).
- Sample-accurate / timeline parameter automation (on the per-block queue).
- VST3 call logger (host↔plugin COM tracer) — see roadmap.
- crates.io publish — blocked on the Steinberg SDK licensing decision.
