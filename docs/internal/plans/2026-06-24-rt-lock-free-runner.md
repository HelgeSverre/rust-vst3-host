# RT lock-free runner path — implementation plan

> Follow-on to `2026-06-23-rt-zero-alloc.md`. That plan made the `RealtimePluginRunner`
> callback **allocation-free and Drop-free** in steady state and proved it with a counting
> allocator. This plan removes the remaining **per-block locks** so the callback is
> **lock-free**, and proves it with a lock-detecting wrapper armed around the measured window
> (the direct analog of the counting allocator — locks cannot be counted globally, so the
> proof must come from instrumenting the lock *type*).

**Goal:** `RealtimePluginRunner::process` takes **zero locks** in steady state, in-process, at
a fixed buffer size, while parameter changes and MIDI flow. Keep all `unsafe` inside
`src/internal/`. Public API additive only.

## Verified ground truth (line-referenced, read 2026-06-24)

Runner path:
- `RealtimePluginRunner` **owns** `plugin: Plugin` by value (`realtime.rs:103`); no `Arc`/`Mutex`
  around it. `process` (`realtime.rs:142`) drains the rtrb SPSC ring serially (`143–155`,
  already lock-free) then calls `plugin.process_audio` (`156`).
- `Plugin::process_audio` (`plugin.rs:944`) calls `internal.process` then locks
  `audio_levels` (`plugin.rs:955`, `Arc<Mutex<AudioLevels>>` field at `plugin.rs:164`).
- `PluginImpl::process` (`plugin_impl.rs:861`) is the per-block work.

The locks reachable from `PluginImpl::process` per block:
1. `self.input_events.clamp_offsets(..)` (`plugin_impl.rs:898`) → `HostEventList.events.lock()`
   (`com_implementations.rs:623`). `HostEventList { events: Mutex<Vec<Event>> }`
   (`com_implementations.rs:597`).
2. The plugin, **inside** `self.processor.process` (`plugin_impl.rs:948`), reads input events
   via the `IEventList` COM vtable → `getEventCount`/`getEvent`
   (`com_implementations.rs:679,702`), each `events.lock()`; and writes output via `addEvent`
   (`com_implementations.rs:729`).
3. `data.input_param_changes.enqueue(..)` (`plugin_impl.rs:904`) →
   `ParameterChanges.queues.lock()` (`com_implementations.rs:775`), and nested
   `ParameterValueQueue.points.lock()` via `insert_point`/`reset`
   (`com_implementations.rs:969,961`). `ParameterChanges { queues: Mutex<Vec<ComWrapper<…>>>,
   used: AtomicUsize }` (`com_implementations.rs:753`); `ParameterValueQueue { param_id:
   AtomicU32, points: Mutex<Vec<(i32,f64)>> }` (`com_implementations.rs:939`).
4. The plugin, inside `processor.process`, reads param changes via `IParameterChanges` /
   `IParamValueQueue` → `getParameterCount`/`getParameterData`/`getPointCount`/`getPoint`
   (`com_implementations.rs:809,837,991,997`), each a `queues.lock()` / `points.lock()`.
5. `self.output_events.for_each_then_clear` (`plugin_impl.rs:971`) → `events.lock()`
   (`com_implementations.rs:643`). (`force_push` into `output_midi` is already lock-free —
   `ArrayQueue`, `plugin_impl.rs:87`.)
6. `handler.parameter_changes.lock()` (`plugin_impl.rs:915`) +
   `gui_param_changes_for_host.lock()` (`plugin_impl.rs:920`). Only reached when a plugin
   editor is open and pushed via `performEdit` (`com_implementations.rs:545`). **Scoped out of
   slice 1** (see Safety, §C).

Hard constraint:
- `ComWrapper<C>` is `Send`/`Sync` **only where `C: Send + Sync`**
  (com-scrape-types-0.1.1 `class.rs:192–193`). `PluginInternal: Send` (`plugin.rs:173`) and
  the COM payloads sit in `ComWrapper`, so any replacement for `Mutex<T>` **must still be
  `Sync`**, or the workspace stops compiling. This rules out bare `UnsafeCell`/`RefCell`; we
  need a wrapper with an explicit `unsafe impl Sync`.

## Safety argument (why each lock can go)

### A. The COM-shared structures (`HostEventList`, `ParameterChanges`, `ParameterValueQueue`)

**Invariant:** on the runner path these objects are touched by **exactly one thread** (the
runner/audio thread that owns the `Plugin`), and **never concurrently**. The intervals are
disjoint and all on that one thread:
- *before* `processor.process`: the host fills `input_events`/`input_param_changes` and clears
  `output_events` (`plugin_impl.rs:879,897–905`);
- *during* `processor.process` (`plugin_impl.rs:948`): the plugin reads inputs and writes
  outputs via the COM vtable, **synchronously on the calling thread**. VST3's contract: the
  `ProcessData` COM pointers are valid only for the duration of the `process()` call and a
  conformant plugin accesses them only synchronously within it;
- *after* `processor.process`: the host drains `output_events` and clears the input lists
  (`plugin_impl.rs:960,963,971`).

What **enforces** single-thread access (this is the load-bearing part — the invariant is true
today but not *structurally* guaranteed, so we make it structural):
- `RealtimePluginRunner` owns `Plugin` by value; the only mutation entry from other threads is
  the rtrb SPSC ring, drained serially before `process` (`realtime.rs:143–156`). The ring's
  release/acquire is the single audited happens-before edge from control thread to audio
  thread.
- The replacement cell type is **not exposed**; the structures stay in `src/internal/`. We
  keep `unsafe impl Sync` to satisfy the `ComWrapper` bound, with the documented invariant
  "only dereferenced on the thread that owns the `PluginImpl`."

**Honest caveat — the other consumer.** The *same* `HostEventList`/`ParameterChanges` types
are also used by the mutex-based `simple::play` path, where `Plugin` lives behind
`Arc<Mutex<Plugin>>`. That path still serializes all access through the outer `Mutex<Plugin>`,
so there is still only one thread inside the cell at a time — the outer mutex is the
enforcer there. The unsafe-`Sync` promise therefore holds for **both** paths *as long as*
every host-thread method that mutates a live plugin's COM state goes through either the runner
(by value, single thread) or the outer `Mutex<Plugin>`. Audit target: `add_event` /
`send_midi_event*` (`plugin_impl.rs:1298,1562,1575,1596,1942`), `enqueue`, `clear`. All are
`&mut self` on `PluginImpl` or run inside `processor.process`; none is callable concurrently
with `process` on the runner path. This audit is task 1 and is a precondition for the unsafe
impl.

**Soundness of dropping the lock:** with single-thread access proven, replacing `Mutex<T>`
with a cell that does `&mut *cell.get()` has no data race (no two threads, ever, reach the
cell concurrently). This is exactly the VST3 SDK's own host implementation, which uses
**no locks** on these structures.

**Lowest-unsafe choice.** Two options, pick per the open question below:
- **B1 (preferred, least new unsafe):** `atomic_refcell::AtomicRefCell<T>`. It is `Sync where
  T: Send + Sync`, so `ComWrapper` is satisfied with **no new `unsafe impl`**, and a genuine
  concurrent access **panics loudly** instead of silent UB. The borrow is a single atomic
  RMW — already lock-free for the gate's purposes (no blocking, no syscall, no priority
  inversion). **Must use `try_borrow`/`try_borrow_mut` inside the COM-callback methods**
  (`getEvent`, `getPoint`, …) so a contended borrow cannot `panic!` and unwind across the C++
  FFI boundary (the code already avoids FFI unwinds, e.g. `unwrap_or_else(|p| p.into_inner())`
  at `com_implementations.rs:991`). Adds one tiny `no_std` dep.
- **B2 (zero deps, more unsafe):** a private `RtCell<T>(UnsafeCell<T>)` in `src/internal/`
  with `unsafe impl<T: Send> Sync for RtCell<T> {}` and the documented single-thread
  invariant. No atomic on the access path at all (truly sync-free), but the `Sync` promise is
  unchecked. Recommended only if the dep is unwanted; pair with the loom model (later slice).

### B. `audio_levels` (`plugin.rs:164`, locked at `plugin.rs:955`)

**Invariant:** the audio thread *writes* once per block after `process` returns
(`plugin.rs:955–962`); a UI thread *reads* via `get_output_levels` (`plugin.rs:972`). It is
**not** a COM object (no `ComWrapper` bound), so it has full freedom of representation. This is
a classic single-writer / single-reader hand-off. Replace the `Mutex<AudioLevels>` with a
lock-free snapshot: a small fixed set of per-channel `AtomicU32` (bit-cast `f32`) for peak/RMS
written with `Relaxed`/`Release` and read with `Acquire`. No lock, and a torn read is
impossible per-field (each field is one atomic); cross-field tearing is harmless for a meter.
This removes the one lock that is **not** inside a COM object and is the easiest win.

### C. Editor parameter relay — scoped out of slice 1

`handler.parameter_changes.lock()` + `gui_param_changes_for_host.lock()`
(`plugin_impl.rs:915,920`) are only reached when a plugin **editor is open** and the editor
calls `performEdit` from its UI thread (`com_implementations.rs:545`) — a genuine cross-thread
producer the runner does not own. Removing these soundly needs a lock-free MPSC hand-off
(crossbeam `ArrayQueue`, like `output_midi`) and is a real design change. **Slice 1 proves
lock-freedom for the headless runner path with no editor open** (which is exactly what the
gate exercises and what `RealtimePluginRunner` documents). The editor relay is an explicit
later slice. The plan states this honestly rather than claiming a blanket lock-free.

## Acceptance gate (proves zero locks on the callback)

Locks have no global hook (an uncontended `Mutex::lock` is one atomic CAS, no syscall, no
alloc — invisible to the counting allocator). So the proof instruments the **lock type**: a
thread-local "armed" flag plus an `RtMutex<T>` wrapper that counts (and is documented to be
swapped onto every hot-path lock). The test arms the flag on the runner thread around the
measured window and asserts the count is 0 — the byte-for-byte analog of `alloc_tests.rs`.

New file `src/internal/rt_lock_gate.rs` (cfg gated, compiled only under `test`/a `rt-lock-gate`
feature; production builds use plain `Mutex` so there is zero overhead). New integration test
`tests/lock_tests.rs` parallel to `realtime_runner_steady_state_is_allocation_free`: warm up,
arm, run 200 blocks driving params + MIDI, assert `LOCKS == 0`.

**Prototype (run 2026-06-24, standalone)** demonstrating the mechanism and observed output —
a `RtMutex` that bumps a global when locked while armed; "before" still has a lock on the
armed path, "after" is lock-free:

```
BEFORE lock removal: 200 lock acquisitions over 100 blocks
AFTER lock removal: 0 lock acquisitions over 100 blocks
GATE PASSED
```

The real gate's "before" run will print a non-zero count (every COM method + `audio_levels`
acquiring through `RtMutex`); after the slice tasks land it prints `0` and the
`assert_eq!(locks, 0)` is the CI lock-freedom gate. Caveat (documented in the test): it only
sees locks routed through `RtMutex`; a `std::Mutex` hidden in a transitive dep is invisible —
acceptable because every lock on the audited path is one we own.

## TDD tasks (slice 1)

Ordered; each compiles green and the gate count strictly drops.

1. **Audit + document the single-thread invariant (no code change beyond comments).** Confirm
   every caller of `add_event`/`send_midi_event*`/`enqueue`/`clear` on a live plugin is either
   `&mut self` on `PluginImpl` or inside `processor.process`; write the invariant into a doc
   comment on `HostEventList`/`ParameterChanges`. Test: existing suite stays green; this is the
   precondition the unsafe rests on.
2. **Lock gate harness.** Add `src/internal/rt_lock_gate.rs` (`RtMutex<T>`, `arm()`, `disarm()`,
   `lock_count()`), and `tests/lock_tests.rs` (`#[ignore]`, needs Dexed) that arms around 200
   runner blocks. Initially wire `RtMutex` onto `audio_levels` only and assert the count is
   **non-zero** (proves the gate detects a real lock), then flip to `assert_eq!(…, 0)` as the
   later tasks remove locks.
3. **Lock-free `audio_levels`.** Replace `Arc<Mutex<AudioLevels>>` (`plugin.rs:164`) with an
   atomic snapshot type; update `process_audio` (`plugin.rs:955`) and `get_output_levels`
   (`plugin.rs:972`). Test: gate count drops by the `audio_levels` contribution;
   `get_output_levels` still returns the last block's levels (unit test on the snapshot type).
4. **Lock-free `ParameterValueQueue.points` + `ParameterChanges.queues`.** Swap both `Mutex<>`
   for the chosen cell (B1 `AtomicRefCell` or B2 `RtCell`); use `try_borrow` in the COM-vtable
   methods (`getParameterCount/Data/PointCount/Point`, `com_implementations.rs:809–1011`).
   Test: existing `parameter_changes_tests` stay green; gate count drops further.
5. **Lock-free `HostEventList.events`.** Same cell swap; `try_borrow` in
   `getEventCount/getEvent/addEvent`. Test: existing event round-trip behavior unchanged; gate
   count → **0** and `tests/lock_tests.rs` flips to `assert_eq!(lock_count(), 0)`.
6. **Update `RealtimePluginRunner` rustdoc** (`realtime.rs:95–101`) to state the callback is
   now lock-free with no editor open, and point at `tests/lock_tests.rs`.

## Later slices

- Editor parameter relay (§C): replace `ComponentHandler.parameter_changes` +
  `gui_param_changes_for_host` with a lock-free MPSC `ArrayQueue` hand-off; then the gate holds
  even with an editor open.
- loom model of the SPSC-ring + cell two-thread interleaving to machine-check the data-race
  freedom of the unsafe-`Sync` cells (only needed if B2 is chosen over B1).
- Compile-time `RtSafe` sealed marker trait the lock types don't implement, required on the
  fields `process()` touches, so a future regression that reintroduces a lock fails to compile.
- Wire the same gate into the `simple::play` mutex path's docs (it is *not* lock-free by
  design; document the contrast).

## Open questions (maintainer)

- **B1 vs B2:** accept the tiny `atomic_refcell` dependency (panic-on-misuse safety net, no new
  `unsafe impl`) or stay zero-dep with `RtCell` + `unsafe impl Sync` (truly sync-free, unchecked
  promise, wants the loom model)? This is the only real fork.
- Should the lock gate be a `cfg(test)`-only harness or a named `rt-lock-gate` feature so it can
  also gate the `simple::play` path and external benches?
- Is "lock-free only with no editor open" an acceptable slice-1 claim, or must the editor relay
  (§C) land in the same slice before we advertise lock-freedom anywhere user-facing?

---

## Adversarial review — DECISIVE (2026-06-24)

Three independent critics reviewed this plan. Verdicts: VST3-contract **sound** (but conditional),
concurrency/UB **needs-revision**, gate-validity **needs-revision**. The findings change the
recommendation: **do NOT remove the COM-shared queue locks.**

### Why the COM-queue lock removal is an unsound trade (not a clean win)

- The single-thread safety argument is **delegated to plugin conformance the host cannot enforce.**
  The host hands the plugin *owning, ref-counted* COM pointers: `inputEvents`/`inputParameterChanges`
  are wired via `to_com_ptr().into_raw()` (plugin_impl.rs:602-618) and `getParameterData`/`getPoint`
  return `into_raw()` per call (com_implementations.rs:845,889,904) — each bumps the `ComWrapper` Arc
  strong count, so the object **survives past `process()` return**. A non-conformant or aggressive
  plugin can (a) cache those pointers and read them from a DSP worker thread or a later block, or
  (b) run a parallel voice pool that reads them *during* a single `process()` call. Today the `Mutex`
  makes that a harmless contended lock. After the swap it is **silent UB** (UnsafeCell + unsafe-Sync)
  or **a panic across the C++ FFI / a silently-dropped event** (AtomicRefCell + try_borrow).
- The vst3 0.3 crate ships **raw bindings only** — no enforced contract — so this assumption can't be
  proven from our own code.
- **The trade has no upside here:** these locks are *uncontended* on the runner path (the runner owns
  the plugin; nothing else touches the queues). We'd be adding UB/panic risk to remove a lock that
  never blocks anyone.

### Concrete error the review caught in this plan

- `component_handler.parameter_changes.lock()` (plugin_impl.rs:915) was scoped out as "editor-only".
  **Wrong:** `component_handler` is set **unconditionally at load** (plugin_impl.rs:267,290,383), so
  that lock is taken on **every headless block** (the `is_empty()` check is *after* the lock). Any
  "flip the gate to zero" step would have failed on it.

### Revised recommendation

**Keep the COM-shared queue locks** (`HostEventList.events`, `ParameterChanges.queues`,
`ParameterValueQueue.points`). They are uncontended and safe; removing them is a net negative.

The **sound, plugin-independent** subset that remains worth doing (host-side only, no COM-shared
cells, no plugin-conformance dependency):

1. **`audio_levels` → lock-free atomic snapshot** (per-channel `AtomicU32` bit-cast `f32`,
   Release/Acquire). Single-writer (audio) / single-reader (UI). Sound. (Multi-channel reads are
   per-field tear-free but not a coherent snapshot — fine for a meter; document it.)
2. **`component_handler.parameter_changes` per-block lock → lock-free fast path**: an `AtomicBool`
   "has pending editor changes" set by `performEdit`, checked before locking, so the headless path
   (no editor pushing) takes no lock. The `Mutex` stays for the actual rare drain.
3. **Lock-detecting gate harness** (`RtMutex` + thread-local armed flag + global counter), with the
   review's fixes: a `SERIAL: Mutex<()>` guard + a `Drop`-based disarm (panic-safe), and the gate
   wraps *only* the two removed host-side locks and asserts they reach **0** on the runner path
   (the COM-queue locks remain plain `std::Mutex`, deliberately, and are not counted).

This delivers a sound "the host-side per-block locks on the headless runner path are gone, proven by
a gate" — without trading safety for plugin-conformance-dependent UB. Honest scope: the callback is
not "lock-free" absolutely (the kept COM-queue locks, plus plugin/dependency internals are out of
host control); the rustdoc must say exactly that, mirroring the allocation wording.

**For robustness against hostile/non-conformant plugins, the right tool is the existing
out-of-process isolation path** (the cells live in the helper's single thread), not unsafe-Sync in
the host.
