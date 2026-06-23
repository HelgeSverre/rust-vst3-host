# Timeline / sequencer / automation engine — implementation plan

> Source: the `next-big-feature` orchestration (2026-06-23). Ranked #1 of 7 candidates
> (avg 19.3/3 judges; top pick of both the RT-audio-engineer and maintainer lenses).
> Adversarial feasibility verdict: **sound**, with three fixes folded in below.

**Goal:** A `transport` module that schedules MIDI clips + parameter-automation lanes on a
**musical (beat) timeline** and drives them into a plugin sample-accurately — first offline, then live.

**Architecture:** Pure safe-Rust composition over existing primitives. No new `unsafe`/COM,
single-plugin (respects "one plugin per handle"). The sample-offset machinery already exists
in-process (`send_midi_event_at`, `set_parameter_at`, `ParameterAutomation::points_for_block`);
this slice closes the three places the offset is currently dropped, then layers the timeline on top.

**Timebase decision (2026-06-23): beats from day one.** Clips and lanes are authored in **beats**;
the `Timeline` owns a tempo (BPM) and sample rate and converts beat↔frame each block. Slice 1 uses a
**constant tempo** (a fixed `bpm` on the `Timeline`); tempo *changes over the timeline* (a tempo
automation curve) are deferred to a later slice. The existing `ParameterAutomation`/`points_for_block`
API is seconds-based, so the driver converts the block's beat window to the corresponding seconds
window (`secs = beat / bpm * 60`) before calling it — no change to `parameters.rs` needed for slice 1.

## Why this won

- **Leverages 0.5.0 directly**: builds on the just-shipped gesture capture (future Recorder slice),
  runtime transport, and the existing sample-accurate MIDI/automation primitives.
- **Contained risk**: a layer strictly above the safe `Plugin` handle; all `unsafe` stays in `internal/`.
- **Differentiated**: no mature safe-Rust host offers a sample-accurate timeline; this is a category move
  toward "mini-DAW engine" without the full multi-plugin-graph commitment yet.

## Verified existing surface (ground truth)

- `Plugin::send_midi_event_at(event, offset)` (plugin.rs:776) → `PluginImpl` sets `Event.sampleOffset`. **In-process scheduling already works.**
- `Plugin::set_parameter_at(id, value, offset)` (plugin.rs:521) routes offsets in-process **and** across isolation (`HostCommand::SetParameterAt`).
- `ParameterAutomation::points_for_block(start_secs, frames, sr, points_per_block)` (parameters.rs:270) yields sample-offset automation points (seconds timebase).
- `simple::render_to_wav` (simple.rs:346) + `audio::write_wav` (audio.rs:507) = the offline drive loop + WAV writer.

## Three gaps this slice closes

1. **Realtime ring drops the offset** — `RtCommand::Midi(MidiEvent)` → `send_midi_event` at block start (realtime.rs:122).
2. **Mutex bridge drops the offset** — `HybridCommand::Midi(MidiEvent)` → `send_midi_event` (playback.rs:67).
3. **Isolation parity** — `IsolatedPluginImpl` does not override `send_midi_event_at`; `HostCommand::SendMidi` has no offset field, so isolated playback silently diverges from in-process for scheduled MIDI.

## Global constraints

- Keep `unsafe`/COM in `src/internal/`. Additive public API only (no 0.5.0 signature breaks).
- `#![deny(missing_docs)]` — rustdoc every new public item.
- `just check` (fmt + clippy `-D warnings` + tests) green per commit; commit per task.

---

### Task 1: `SendMidiAt` IPC variant + isolated override (isolation parity)

**Files:** modify `vst3-host/src/process_isolation.rs` (add `HostCommand::SendMidiAt { event, sample_offset: i32 }`), `internal/isolated_plugin_impl.rs` (override `send_midi_event_at` → send `SendMidiAt`), `bin/vst3-host-helper.rs` (handle arm → `p.send_midi_event_at(event, sample_offset)`).

- [ ] Test first: in `process_isolation.rs` `wire_tests` (beside `set_parameter_at_round_trips_across_the_wire`), add `scheduled_midi_offset_ipc_round_trips` — serialize `SendMidiAt { NoteOn{..}, 256 }`, assert deserialize preserves event + offset. Plugin-free (CI).
- [ ] Mirror the `SetParameterAt` pattern exactly. Keep `SendMidi` for helper/host version skew.
- [ ] `just check`.

### Task 2: carry offset through the realtime ring

**Files:** modify `vst3-host/src/realtime.rs`.

- [ ] Test first: `midi_offset_round_trips_through_the_ring` (mirror `transport_commands_round_trip_through_the_ring`) — `send_midi_at(NoteOn, 128)`, pop, assert offset 128.
- [ ] `RtCommand::Midi(MidiEvent)` → `Midi { event, offset: i32 }`; `process()` (realtime.rs:121) calls `send_midi_event_at`. Keep `send_midi` (offset 0) for back-compat; add `pub fn send_midi_at(&mut self, event, sample_offset) -> bool`.
- [ ] `just check`.

### Task 3: carry offset through the mutex playback bridge

**Files:** modify `vst3-host/src/playback.rs`.

- [ ] Test first: `hybrid_midi_offset_is_applied` — push `HybridCommand::Midi { event, offset }`, assert `apply_control` forwards the offset (assert on the queued command shape; plugin-free).
- [ ] `HybridCommand::Midi(MidiEvent)` → `Midi { event, offset: i32 }`; `apply_control` (playback.rs:67) calls `send_midi_event_at`. `AudioHandle::send_midi`/`MidiSink::send_midi` push offset 0; add `send_midi_at` variants.
- [ ] `just check`.

### Task 4: the `transport` module — `Timeline` + `MidiClip` + `AutomationLane` (pure-logic core)

**Files:** create `vst3-host/src/transport.rs`; modify `vst3-host/src/lib.rs` (`pub mod transport;` + re-export + prelude).

- [ ] Test first: `timeline_slices_clip_and_lane_into_block_offsets` — sr=48000, **bpm=120** (so 1 beat = 0.5s = 24000 frames), a `MidiClip` with NoteOn@beat 0 and NoteOff@beat 0.02 (=0.01s=480 frames) + one `AutomationLane`; `advance_block(512)` twice; assert block 0 yields NoteOn@offset 0 and NoteOff@offset 480, block 1 yields no MIDI, lanes yield correct offsets, `sample_clock += 512` each call.
- [ ] **(stress fix)** Add an explicit boundary case: an event whose frame index == `clock + frames` must land in the NEXT block (exclusive upper bound `[clock, clock+frames)`), guarding the off-by-one.
- [ ] Add `beat_to_frame`/`frame_to_beat` unit tests at bpm=120 and bpm=140 (rounding behaviour at block edges).
- [ ] `MidiClip { events: Vec<(f64 /*beats*/, MidiEvent)> }`; `AutomationLane { param_id: u32, automation: ParameterAutomation, points_per_block: usize }`; `Timeline { sample_rate, bpm, sample_clock: u64, clips, lanes, scratch... }`. `advance_block(frames) -> BlockEvents { midi: Vec<(MidiEvent, i32)>, params: Vec<(u32, i32, f64)> }` — convert each clip event's beat to a frame index via `bpm`/`sample_rate`, window against `[clock, clock+frames)`; for lanes, convert the block's beat window to a seconds window (`secs = beat / bpm * 60`) and call `points_for_block`. Reuse cleared scratch Vecs. Document the **beats timebase** + constant-tempo (slice 1) / tempo-automation (later) split.
- [ ] `just check`.

### Task 5: `Timeline::drive_block` convenience + offline-render acceptance gate

**Files:** modify `vst3-host/src/transport.rs`; create `vst3-host/tests/timeline_tests.rs`.

- [ ] `drive_block(&mut Plugin, &mut AudioBuffers)`: `advance_block(buffers.block_size)` **(stress fix: `AudioBuffers` has no `frames()`; use `block_size` / `outputs[0].len()`)**, push each `(event, offset)` via `send_midi_event_at`, each `(id, offset, value)` via `set_parameter_at`, then `process_audio(buffers)`.
- [ ] Acceptance gate `timeline_renders_scheduled_sequence_offline` (`#[ignore]`, plugin-guarded; mirrors integration_tests.rs:1107): load synth at 48000/512 with `bpm=120`, find cutoff param, build `Timeline` with a held-note clip (NoteOn@beat 0, NoteOff@beat 2) + a cutoff ramp lane over the first 2 beats, loop blocks accumulating output, `write_wav`, assert (1) `late_quarter_energy > early_quarter_energy` and (2) **(stress fix)** held-note-window RMS shows a **decay trend** vs a release window padded long enough to clear the tail — NOT near-silence (Dexed is FM with patch-dependent release).
- [ ] **(stress fix / open question)** Prefer targeting the license-free, always-present `test_plugins/TestSynth.vst3` so the gate can be a non-`#[ignore]` CI gate, if TestSynth exposes a usable param; else keep Dexed + `#[ignore]`.
- [ ] `just check` + run the gate.

---

## Later slices

- **Slice 2 — live timeline:** multiple clips/lanes, loop, seek, exposed playhead; wire `drive_block` into both live paths via the now-offset-carrying rings (a `RealtimePluginRunner` timeline tick feeds the SPSC ring).
- **Slice 3 — gesture Recorder:** timestamp `Plugin::take_parameter_edits()` (0.5.0) against the playhead; fold Begin..End gesture brackets into `AutomationPoint`s with thinning (value epsilon + min-time-gap). Block-granular, not sample-accurate (disclose).
- **Slice 4 — tempo automation:** a tempo curve over the timeline (varying BPM), so beat↔frame conversion integrates tempo across the block instead of using a constant `bpm`; feed the per-block tempo into the `ProcessContext` transport too. (Beats themselves land in slice 1.)
- **Slice 5 — event-driven latency:** handle `restartComponent(kLatencyChanged)` (currently only logged) via the side-channel ring.
- **Slice 6 — RCU snapshot:** `Arc<Timeline>` read on the audio thread, RCU swap on edit, for dense automation instead of streaming every point through the ring.

## Decisions & open questions

- **Timebase — RESOLVED: beats from day one** (constant tempo in slice 1; tempo automation in slice 4).
- **IPC back-compat — default:** add `SendMidiAt`, keep `SendMidi` (tolerates host/helper version skew).
- **Offset-bridge API shape — default:** additive `send_midi_at` alongside offset-0 `send_midi` (no 0.5.0 break).
- **Prelude inclusion — default:** `transport` always-on (no new deps), exported from the prelude.
- **Acceptance-gate plugin — open:** prefer license-free `TestSynth.vst3` for a non-`#[ignore]` CI gate if it exposes a usable ramp param; else Dexed + `#[ignore]`. Decide at Task 5.
