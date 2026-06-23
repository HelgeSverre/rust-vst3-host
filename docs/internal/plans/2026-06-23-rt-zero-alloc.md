# RT-safe zero-allocation realtime path — implementation plan

> Source: the `rt-zero-alloc-engine` orchestration (2026-06-23): 5 line-referenced code
> investigations + 3 research briefs → plan → two adversarial critics. The critics returned
> **needs-revision** (RT correctness) and **refuted** (gate validity); this doc is the
> **corrected** plan with their concrete fixes folded in. One critic empirically measured the
> allocation counts (see "Why the naive slice fails" below), which drove the scope change.

**Goal:** Make the `RealtimePluginRunner` callback **allocation-free and Drop-free in steady
state** (param changes + MIDI), in-process, at a fixed buffer size — and prove it with a
regression-guarded counting-allocator test.

**Explicit non-goal for slice 1: NOT yet lock-free.** `process()` still takes several mutexes
per block (`component_handler.parameter_changes`, `audio_levels`, and the internal mutexes in
`ParameterChanges`/`HostEventList`). The counting-allocator gate proves alloc/Drop-free; it does
**not** detect locks, so we do not claim lock-free here. Lock removal is later slices.

## Why the naive slice fails (the adversarial finding)

The original plan fixed only `std::mem::take(&mut self.pending_param_changes)` (plugin_impl.rs:867,
leaves a zero-cap Vec → next `set_parameter` push reallocates) and expected the gate to read 0.
But over the proposed schedule (a param every 8th block across 200 blocks) a critic measured:

- **~100 total** alloc/realloc/dealloc events in the armed window.
- Removing only the `mem::take` realloc → **~25** (the `pending_param_changes` residual).
- The other **~75** come from `ParameterChanges::clear_all()` (com_implementations.rs:767) running
  **every block** at plugin_impl.rs:956, which `queues.clear()` — **dropping every**
  `ComWrapper<ParameterValueQueue>`. The next param block re-`enqueue()`s, finds no queue, and
  `ComWrapper::new(ParameterValueQueue::new(id))` (com_implementations.rs:759) **allocates afresh**,
  plus `insert_point`'s `points.insert` (line 917) on an emptied Vec.

So a genuine steady-state zero **requires fixing the ParameterChanges queue churn too**. The
param path's two allocation sources (`pending_param_changes` + ComWrapper churn) are the same
conceptual bug — "a param change in steady state allocates" — so this slice fixes **both**.

## Verified ground truth

- `RealtimePluginRunner::process` (realtime.rs:118-133) drains the rtrb ring (already lock-free,
  alloc-free) then calls `plugin.process_audio`. The control ring is RT-clean; gaps are downstream.
- `Plugin::process_audio` (plugin.rs:900-921) → `internal.process` then locks `audio_levels`;
  `update_from_buffers` (audio.rs:118-145) is alloc-free (indexes a pre-sized Vec).
- `AudioBuffers` (audio.rs:5-33) is `Vec<Vec<f32>>` allocated once by `AudioBuffers::new`; the
  runner never resizes it as long as the caller passes a fixed `block_size` == configured size.
- Accessors that **actually exist**: `Plugin::sample_rate()` (plugin.rs:349), `block_size()` (354),
  `PluginInternal::output_channel_count()` (337). There is **no** `max_block_size()`,
  `input_channels()`, or `output_channels()` on `Plugin` (the original sketch was wrong).
- `ParameterChange` (parameters.rs:101) is `Debug + Clone`, **not `Copy`**.

## Global constraints

- Keep `unsafe`/COM in `src/internal/`. Public API additions additive only.
- `#![deny(missing_docs)]`; `just check` green per commit; commit per task.
- Capacity for `pending_param_changes`/param scratch must be **>= the RtControl ring capacity**
  (so a one-block burst that fills the ring can never overflow the Vec and reallocate). Derive it
  from the ring capacity rather than a magic 256.

---

### Task 1: Failing acceptance gate — drive the runner with param + MIDI, count alloc+realloc+dealloc

**Files:** modify `vst3-host/tests/alloc_tests.rs`.

- [ ] In the counting allocator, **also count `dealloc` while armed** (so the gate proves Drop-free, not just alloc-free): in `dealloc`, `if ON.load(Relaxed) { ALLOCS.fetch_add(1, Relaxed); }` before `System.dealloc`.
- [ ] Add `#[test] #[ignore = "Requires the bundled test plugin"] fn realtime_runner_steady_state_is_allocation_free()`: skip-if-missing-plugin; build host at 48000/512; `RealtimePluginRunner::new(plugin, ring_cap)`; `runner.start()`; build `AudioBuffers::new(0, 2, 512, 48000.0)` **outside** the armed window.
- [ ] **Warm up before arming** (critic fixes): pump ≥16 blocks; in warmup queue **one `set_parameter` per id** you'll automate (pre-warms the per-id queue once the ComWrapper fix retains it) **and** push a NoteOn+NoteOff **on the same block** at least once (so `input_events` is pre-sized for 2 simultaneous events).
- [ ] Arm (`ON=true`); for `i in 0..200`: every 8th block `set_parameter` over a small fixed pre-warmed id set, every 16th block NoteOn/NoteOff, then `runner.process(&mut buf)`. Disarm; capture the raw count; **then** print + `assert_eq!(n, 0)`. No `format!`/`println!`/panic-formatting inside the armed window.
- [ ] This FAILS now (prints ~100). Run it; record the actual count + per-cause breakdown during bring-up to confirm the before/after the plan predicts.

### Task 2: Reuse `pending_param_changes` capacity (drain-in-place + pre-reserve)

**Files:** modify `vst3-host/src/internal/plugin_impl.rs`.

- [ ] Pre-reserve `pending_param_changes` to the configured capacity (>= ring cap) at construction (near plugin_impl.rs:378).
- [ ] Replace `std::mem::take(&mut self.pending_param_changes)` (line 867) with an in-place drain that retains capacity (e.g. iterate/`drain(..)` into the param queue, then the Vec stays allocated; do **not** reassign `Vec::new()`).
- [ ] Gate count drops by ~25; still non-zero (ComWrapper churn remains). `cargo test -p vst3-host` stays green.

### Task 3: Stop the per-block ComWrapper churn in `ParameterChanges` (the ~75)

**Files:** modify `vst3-host/src/internal/com_implementations.rs`.

- [ ] `ParameterChanges::clear_all()` (com_implementations.rs:767): **retain** the `ComWrapper<ParameterValueQueue>` entries; clear each queue's `points` Vec **in place** instead of `queues.clear()` (which drops the wrappers).
- [ ] `enqueue()` (754-764): look up the existing pre-warmed queue for the id and reuse it; only allocate a new ComWrapper for a genuinely new id (amortized once, in warmup for the gate's id set).
- [ ] `insert_point` (911-918): ensure the `points` Vec is **cleared in place + pre-reserved**, not re-created, so `insert` never reallocates in steady state.
- [ ] Gate now reaches **0**; flip the test from expected-fail to expected-pass. `just check`.

### Task 4: Full gate + honest scope doc note

**Files:** none (verification) + a short note where the runner is documented.

- [ ] `cargo test -p vst3-host` (unit) + `cargo test -p vst3-host --test alloc_tests -- --ignored --nocapture` (gate prints `...: 0`) + `just lint`.
- [ ] Document the slice-1 guarantee precisely in `RealtimePluginRunner` rustdoc: **allocation-free + Drop-free** in steady state, **in-process only**, **fixed buffer size**, **not yet lock-free**, and the number is **Dexed-pinned** (a plugin's own controller/process may allocate; off-thread plugin allocs are invisible to the counter).

> `make_buffers` is intentionally **dropped from slice 1** — the test hardcodes `AudioBuffers::new(0, 2, 512, ..)`, so the helper is not on the critical path and its accessor names were wrong. Add it (with real `output_channel_count`-backed getters) in the live-wiring slice if/when callers need it.

## Later slices

- **Lock-free event/param queues:** replace the `Mutex<Vec<Event>>` in `HostEventList` and the `Mutex` in `ParameterChanges` with pre-sized lock-free structures so clear/clamp/enqueue/drain take **no lock** on the callback. Extend the gate to push a **new** param id mid-window and still assert 0.
- **Lock-free output-MIDI feedback ring:** replace `output_midi: Arc<Mutex<Vec<MidiEvent>>>` + extend/drain (plugin_impl.rs:959-970) with a pre-sized rtrb SPSC of `MidiEvent` (Copy → Drop-free pop), drain `HostEventList` straight into the producer, drop-newest-on-full with a counter; wire it into `RealtimePluginRunner` (the lock-free path finally gets feedback). Gate with an arp plugin emitting MIDI.
- **Remove `audio_levels` Mutex from the runner path:** publish peak via per-channel `AtomicU32 fetch_max` (the playback.rs:84 pattern) instead of locking, so the callback stays lock-free under UI contention.
- **Denormal guard scope:** confirm `DenormalGuard` (denormal.rs:30-90) is unwind-safe; consider once-per-stream FTZ/DAZ on the audio thread vs per-block construct/drop.
- **Opt-in thread priority/QoS seam:** non-fatal `on_audio_thread_start` hook (default OFF); on macOS reserve it for the isolation helper's own audio thread (`os_workgroup_join`, feature-gated, in `internal/`). Document priority as best-effort; correctness is the real guarantee.
- **Criterion + sentinel CI gate:** benchmark `runner.process` steady-state; a CI job (where Dexed is present) that runs the `#[ignore]`d alloc gates and **hard-fails if the plugin is absent** (wired like the isolation capstone), rather than silently skipping.
- **Explicit public RT-safe tier:** a `docs/explanation` page + a clearly-labeled API tier stating exactly what is guaranteed alloc/lock/Drop-free, under which conditions, and what is explicitly not (isolation IPC path, variable block sizes on the mutex path).

## Open questions (maintainer decisions)

1. **Capacity source:** derive `pending_param_changes`/param-scratch capacity from `RtControl`'s ring capacity (must be ≥ it), or a separate builder knob? (Resolved default: tie to ring capacity.)
2. **`audio_levels` lock in slice 1:** acceptable to keep (uncontended while the runner owns the plugin) and defer removal, given slice 1 is explicitly "not lock-free"? (Plan assumes yes.)
3. **Gate's automated param id(s):** which stable Dexed parameter id(s) are cheap to automate every 8th block and pre-warm? Need ids guaranteed to exist on the bundled Dexed.
4. **CI plugin availability:** is `Dexed.vst3` on the CI runner that enforces this, and do we want hard-fail-on-missing (capstone style) rather than skip?
5. **Buffer ownership:** keep the caller-owned `&mut AudioBuffers` API (document the fixed-size contract) or have the runner own/lend a pre-allocated buffer so callers can't break the guarantee? (Plan keeps caller-owned + documents.)
