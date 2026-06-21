# Functional Gaps — Exploration & Sequencing

Scoping notes for the five remaining functional gaps after the library extraction.
Read-only exploration (2026-06-20); no code changed. Each section: current state, gap,
approach, difficulty, the acceptance gate, and dependencies.

> **Update 2026-06-21:** all but one of these have shipped. MIDI-out in isolation, parameter
> automation, IPlugFrame + resize, and macOS editor embedding are done; the crates.io item was
> resolved by the `vst3` 0.3 bump (pre-generated bindings → no SDK) and **`vst3-host` 0.1.1 is
> published (MIT)**. The single remaining gap is **GUI across the isolation boundary** (helper
> owns its own window — option a below). The original exploration text is kept for reference.

## At a glance

| Task | Difficulty | Self-contained | Headless gate | Main risk / blocker |
|------|-----------|----------------|---------------|---------------------|
| MIDI-out in isolation — ✅ **DONE** | **S** | Yes | serde wire test + parity | Needs a MIDI-emitting test plugin |
| Parameter automation (apply) — ✅ **DONE** | **S–M** core | Yes (in-proc) | mechanism test + A/B render | Plugins that also smooth params |
| IPlugFrame + host resize — ✅ **DONE** | **S** | Yes (in-proc) | Partial | — (shared plumbing) |
| Editor embed into egui — ✅ **DONE** (macOS) | **M** mac/win, **L** linux | In-proc only | Partial (interactive) | Win/Linux embedding still TODO |
| GUI across isolation — ⬜ **OPEN** | **M** (helper-owned window) | Reuses window.rs | Partial (interactive) | Helper needs a UI run loop; macOS true-embed blocked |
| crates.io / VST3_SDK_DIR — ✅ **RESOLVED** (published 0.1.1) | **Decision** | N/A | Yes (clean-room build) | ~~SDK license~~ — moot: vst3 0.3 has no SDK dep |

Two quick, self-contained, headlessly-verifiable wins (MIDI-out in isolation; the core of
parameter automation) sit next to three GUI/packaging items that are larger, GUI-runtime or
decision-gated.

---

## 1. MIDI output capture in isolated mode — **S**

**Current:** In-process capture works — `PluginImpl` drains `output_events` after
`process()` and buffers converted `MidiEvent`s in `output_midi`
(`plugin_impl.rs:740-749`, `event_to_midi` at `:1117`), surfaced via
`take_output_events()` (`:1049`). The isolated helper reuses that exact code, so the
helper's `Plugin` **already captures** emitted MIDI — but nobody reads it: the `Process`
handler returns only `AudioOutput { outputs }` (`vst3-host-helper.rs:185`), and
`IsolatedPluginImpl` doesn't override `take_output_events`, so `Plugin::take_output_midi`
returns empty (`plugin.rs:75-77`).

**Approach:** Piggyback on the per-block `Process` response (no extra round-trip):
add `output_midi: Vec<MidiEvent>` to `HostResponse::AudioOutput`
(`process_isolation.rs:105`); helper fills it from `p.take_output_midi()` after
`process_audio`; `IsolatedPluginImpl` buffers it (same 4096 cap) and overrides
`take_output_events`. `MidiEvent` is already `Serde` and already crosses IPC for `SendMidi`.

**Gate:** isolated host + a MIDI-emitting plugin (arpeggiator/MPE — Dexed does *not* emit);
send a held note, pump ~20 blocks, assert `take_output_midi()` is non-empty. Plus a serde
round-trip guard on the new field. **One new asset dependency:** a MIDI-emitting test plugin.

**Dependencies:** none.

---

## 2. Parameter-automation execution — **S–M** (core), **M** (full)

**Current:** Every parameter write goes to the **edit controller only**, immediately:
`set_parameter` → `controller.setParamNormalized` (`plugin_impl.rs:562`). The processor's
input parameter queue is allocated and wired into `ProcessData`
(`plugin_impl.rs:425,460-469`) but **never populated** — nothing calls
`addParameterData`/`addPoint` before `processor.process()` (`:731`). So plugins that read
parameters from `inputParameterChanges` during `process()` never see host changes
sample-accurately. `ParameterAutomation::value_at_time` (`parameters.rs:209`) and
`ParameterChange{ sample_offset }` (`parameters.rs:102`) exist but are dead outside tests.
The `ParameterChanges`/`ParameterValueQueue` COM impls are correct and ready
(`com_implementations.rs:719,822`).

**Approach:** (A) per-block, drain a thread-safe inbox into `input_param_changes`
(`addParameterData(id)` + `addPoint(sampleOffset,value)`) before `process()`, and **clear it
after** (needs a new `ParameterChanges::clear`). (B) inbox = `Arc<Mutex<Vec<ParameterChange>>>`
on `PluginImpl`. (C) **dual-write**: feed the processor queue (audio) *and* call
`setParamNormalized` (controller/GUI/`format_parameter`) — VST3 requires both. (D) optional
automation driver mapping curve time → `sample_offset`. Minimal v1 = one point per param per
block at offset 0 (block-rate); sub-block accuracy is an increment.

**Gate:** offline render two passes (param low vs high) through the new path, assert output
RMS/spectrum differs. Plus a mechanism unit test: after enqueue + one block,
`input_param_changes.getParameterCount()==1`, `getPoint(0)==(offset,value)`, and the queue is
empty next block.

**Dependencies:** none (isolation parity is a follow-on). Risk: plugins that internally
smooth may double-apply; respect `step_count`; clamp offsets to actual `numSamples`.

---

## 3. IPlugFrame + host-driven resize (in-process) — **S** (shared plumbing)

**Current:** `IPlugView::setFrame` is never called; no host `IPlugFrame` exists; the view is
attached frameless, so the plugin can't request resizes and the host can't drive `onSize`.
Confirmed signatures: `IPlugFrameTrait::resizeView(*mut IPlugView, *mut ViewRect)`,
`IPlugViewTrait::{onSize, canResize, checkSizeConstraint}`.

**Approach:** Add `HostPlugFrame` COM object (copy the `ComponentHandler` pattern); in
`open_editor`, call `view.setFrame(frame)` **before** `attached`. `resizeView` records the
requested size (poll via a new `take_editor_resize_request()`, applied on the UI thread —
do **not** mutate AppKit from the plugin's thread). Add `Plugin::set_editor_size` →
`checkSizeConstraint` → resize container → `onSize`.

**Gate:** `setFrame` called before `attached`; a synthetic `resizeView` records a size
observable via `take_editor_resize_request`; host-driven `set_editor_size` resizes the
container and `onSize` returns `kResultOk`.

**Dependencies:** prerequisite for #4 and #5's resize story; do it once in-process.

---

## 4. Editor embedding into egui — **M** (mac/win), **L** (linux)

**Current:** The `egui-widgets` feature is a stub (`window.rs:328` `PluginWindowBuilder`
just wraps the standalone `PluginWindow`). The example opens a *separate* OS window
(`plugin_gui.rs:312`). No `raw-window-handle` use; `eframe` is only a dev-dependency.

**Approach:** Embed the editor as a native child inside the eframe window (not a new
top-level): add `raw-window-handle`, get the eframe/winit native handle from `eframe::Frame`,
create a child NSView/HWND/X11 parented into it, attach via the existing `open_editor` path,
and track the egui-allocated rect each frame (the native view draws on top; egui paints
nothing there). Move `eframe` to an optional dep under `egui-widgets`. Add `PluginInternal`
defaults so isolation/in-proc split cleanly.

**Gate:** `examples/embedded_editor.rs` shows Dexed's editor **inside** the eframe window
(no second OS window), interactive, with `get_parameter_changes` reflecting knob turns.

**Risk (#1):** whether eframe 0.31 yields a usable parent handle (which view: winit content
vs wgpu/glow surface) — **needs a spike before committing**. Also: overlay clipping in scroll
areas, event-loop interaction with plugin timers, `resizeView` re-entrancy vs the
`Arc<Mutex<Plugin>>` the audio callback also locks. **In-process only.**

**Dependencies:** #3 (IPlugFrame). Blocks #5 option (b).

---

## 5. GUI across the isolation boundary — **M** (option a) / **XL, mac-blocked** (option b)

**Current:** `open_editor` on an isolated plugin sends `CreateGui`, but the helper returns
"not supported" (`vst3-host-helper.rs:202`); `get_editor_size` is hardcoded `(800,600)`
(`isolated_plugin_impl.rs:232`). The helper is a blocking stdin loop with **no UI run loop**.

**Approach — recommended option (a): helper owns its own top-level window.** Restructure the
helper so the **main thread runs a native UI run loop** and stdin processing moves to a
worker; on `CreateGui`, build a `PluginWindow` around the helper's `Plugin` (reuses
`window.rs` verbatim). Editor is a separate OS window owned by the helper process — crash
isolation preserved, no handle marshalling. **Option (b)** (true cross-process embedding into
a host window) is Windows/Linux-only (SetParent / XEmbed) and **effectively blocked on macOS**
(VST3 plugins expect a same-process NSView; no XPC remote-view path) — defer.

**Gate:** `examples/isolated_gui.rs` — isolated Dexed editor window appears (owned by the
helper PID), interactive, audio still flows concurrently; `CreateGui` returns `Success` and
`get_editor_size` returns the real size.

**Risk:** main-thread ownership in the helper (plugin currently lives on the stdin thread;
audio `Process` commands keep arriving); GUI commands vs the 5s response watchdog;
macOS app-activation/Dock identity.

**Dependencies:** option (a) is independent and reuses `window.rs`. Option (b) blocks on #4.

---

## 6. crates.io / `VST3_SDK_DIR` — a **decision**, not (mainly) code

**Current:** The `vst3` dep's build script `exit(1)`s if `VST3_SDK_DIR` is unset and then
generates bindings from `$VST3_SDK_DIR/pluginterfaces/*.h` via libclang at build time
(`vst3-0.1.2/build.rs`, `vst3-bindgen-0.2.2 generate()`). In-tree this is set by
`.cargo/config.toml` (excluded from the package). A downstream `cargo add vst3-host` has no
env var and no SDK → **the dependency fails to compile**. docs.rs likewise fails (no SDK, no
libclang); the `[package.metadata.docs.rs]` + `--cfg docsrs` block is currently a no-op
(`docsrs` is never used in source).

**The blocker is licensing, not engineering.** The Steinberg SDK is dual-licensed
"VST3 License **or** GPLv3" (`vst3sdk/LICENSE.txt`):
- Proprietary arm: *may not be distributed in parts without written agreement* → **cannot
  vendor the headers (or header-derived generated bindings) into an MIT/Apache crate.**
- GPLv3 arm: redistribution allowed **but makes the derived work GPLv3** → conflicts with
  `license = "MIT OR Apache-2.0"`; would relicense the crate and bind consumers to GPLv3.

**Options:** (a) document-only + clear `build.rs` error, keep MIT/Apache, consumer supplies
the SDK (matches upstream `vst3`'s own posture) — docs.rs won't render; (b) vendor headers —
**license-blocked** unless relicensing to GPLv3 or a Steinberg agreement; (c) fork/upstream a
`vst3` variant that ships **pregenerated, checked-in bindings** behind a default feature (the
only path that makes `cargo add` + `cargo install` + docs.rs all work with no SDK/libclang) —
but the generated bindings face the same license fork.

**Recommendation:** decide the license posture first (product/legal call — not codeable).
If staying MIT/Apache: publish with documented SDK setup + a friendly `build.rs` error, and
accept docs.rs won't render until upstream `vst3` gains a pregenerated-bindings feature. The
README currently has **zero** mention of `VST3_SDK_DIR` — fix that regardless.

**Gate:** clean-room (`/tmp` consumer, no env, no submodule, no `.cargo/config.toml`):
`cargo build` either succeeds (option c) or fails with a clear documented error (a/e);
`cargo package` + build of the packaged artifact reproduces it; docs.rs-style
`RUSTDOCFLAGS=--cfg docsrs cargo doc --all-features --no-deps` renders (or the gate is waived
and the dead docs.rs metadata removed).

---

## Recommended order

1. **MIDI-out in isolation** (S, self-contained, headless) — finishes isolation parity.
2. **Parameter automation, core** (S–M, headless A/B) — fixes a real correctness gap.
3. **IPlugFrame + host resize** (S) — shared GUI plumbing, in-process.
4. **eframe-handle spike → editor embedding** (M; interactive verification).
5. **GUI across isolation, option (a)** (M; interactive) — helper-owned window.
6. **crates.io/VST3_SDK_DIR** — gated on a license/product decision; parallel to all above,
   required before any publish.
