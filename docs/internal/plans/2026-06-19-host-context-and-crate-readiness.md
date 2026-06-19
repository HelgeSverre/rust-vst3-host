# Host Context & Crate-Readiness Roadmap

> **For agentic workers:** Track A is ready to execute task-by-task now. Tracks B–D are
> sequenced phases with concrete targets; promote each to a full task plan when picked up.
> Steps use checkbox (`- [ ]`) syntax.

**Goal:** Walk `vst3-host` from "works for friendly plugins" to a well-made, publishable
crate that hosts the long tail of real plugins (u-he, Waves/WaveShell, …) — handling the
ones that can't be loaded safely in-process by containing them, and providing the complete
host context the rest expect.

**Architecture:** Two independent problems. (1) **Host correctness** — give plugins the
full host environment the VST3 SDK's standard host provides (`IHostApplication` that vends
`IMessage`/`IAttributeList`, `IPlugInterfaceSupport`). (2) **Containment** — some plugins
(Waves) crash from their *own* packaging (duplicate Obj-C classes) and cannot be fixed
host-side; the only robust handling is process isolation, which we already have. The
roadmap completes (1), makes (2) automatic and recoverable, and then does the crate-quality
work (CI, warnings, metadata) to publish.

**Tech stack:** Rust, the `vst3` crate (0.1.2, bindgen over the VST3 SDK), `cpal`, `xcb`
(Linux), Docker for cross-platform CI.

## Global constraints

- Zero `unsafe` in the public API; all COM/FFI stays under `vst3-host/src/internal/`.
- Every change keeps `cargo test --workspace --all-features` green AND `just linux-check`
  green (the lib builds + unit-tests on Linux in Docker). The c_char/cross-platform work is
  done — don't regress it.
- Verification reality: COM-FFI host objects can't be meaningfully unit-tested in isolation.
  The gate is **(a) it compiles on macOS + Linux, (b) `just selftest` against Dexed/TyrellN6
  shows no regression, (c) where a behavior is observable, a real-plugin check confirms it.**
- macOS is the exercised platform; Linux is build+unit-test-verified via Docker; Windows is
  build-only until CI exists (Track C).
- Commit per task. Attribute incorporated external work (e.g. the khremeviuc1004 fork).

---

## Track A — Complete the host context (the immediate work)

**Why:** The SDK's standard host context implements `IHostApplication` (name **+
`createInstance` that vends `IMessage`/`IAttributeList`**) and `IPlugInterfaceSupport`. We
provide `getName` + `IPlugInterfaceSupport`; `createInstance` currently declines. Plugins
use `createInstance` to allocate the message/attribute objects they pass between their
component and controller halves — declining it is the most likely remaining cause of
misbehavior in the broad plugin tail. All work is in
`vst3-host/src/internal/com_implementations.rs` plus the existing `HostApplication`.

**What it does NOT fix:** the WaveShell crash. That is Obj-C duplicate-class dispatch from
Waves' packaging; no host context change prevents it (see Track B).

### Task A1: `HostAttributeList` (implement `IAttributeList`)

**Files:** Modify `vst3-host/src/internal/com_implementations.rs`.

**Interfaces produced:** `pub struct HostAttributeList` with `Class { Interfaces =
(IAttributeList,) }`; a `create_host_attribute_list() -> ComWrapper<HostAttributeList>`.

Storage: an interior-mutable map of attribute id (`FIDString`/`*const c_char` → owned
`CString` key) to a typed value enum `{ Int(i64), Float(f64), Str(Vec<u16>), Bin(Vec<u8>) }`,
behind a `Mutex`. Implement all `IAttributeListTrait` methods against it:
`setInt/getInt`, `setFloat/getFloat`, `setString/getString` (UTF-16 `TChar`),
`setBinary/getBinary`. Unknown key on a getter → `kResultFalse`; success → `kResultOk`.

- [ ] **Step 1:** Define the value enum + struct + `Class` impl + `create_*` helper.
- [ ] **Step 2:** Implement the 8 trait methods (mechanical; mirror `string_to_vst_string`
      / `vst_string_to_string` in `utils.rs` for the UTF-16 string conversions).
- [ ] **Step 3 (test):** unit-test the pure key/value store via a thin safe wrapper
      (`set_int`/`get_int` etc. that the trait methods call) so the map logic is covered
      without COM — round-trip int/float/string/binary, missing-key returns false.
- [ ] **Step 4:** `cargo test -p vst3-host --lib` green; `cargo build --all-features`.
- [ ] **Step 5:** Commit `feat: HostAttributeList (IAttributeList) for the host context`.

### Task A2: `HostMessage` (implement `IMessage`)

**Files:** Modify `vst3-host/src/internal/com_implementations.rs`.

**Interfaces produced:** `pub struct HostMessage` with `Class { Interfaces = (IMessage,) }`
holding an owned message-id `CString` and a `ComWrapper<HostAttributeList>`;
`create_host_message() -> ComWrapper<HostMessage>`.

Implement `IMessageTrait`: `getMessageID` → the stored id pointer (or null), `setMessageID`
→ copy the incoming C string, `getAttributes` → the attribute list's `IAttributeList` raw
pointer (from `to_com_ptr`).

- [ ] **Step 1:** Struct + `Class` + `create_*`.
- [ ] **Step 2:** Implement the 3 trait methods.
- [ ] **Step 3:** `cargo build --all-features`; `just selftest` (Dexed) unchanged.
- [ ] **Step 4:** Commit `feat: HostMessage (IMessage) for the host context`.

### Task A3: Wire `HostApplication::createInstance` to vend them

**Files:** Modify `HostApplication::createInstance` in
`vst3-host/src/internal/com_implementations.rs`.

Replace the "decline + null" body: compare the requested `cid` against `IMessage::IID` and
`IAttributeList::IID`; create the matching host object, query its requested interface into
`*obj`, return `kResultTrue`. Unknown cid → leave `*obj` null, return `kResultFalse`.
(Resolve the `TUID` signedness compare cleanly — compare the 16 bytes via
`std::slice::from_raw_parts(cid as *const u8, 16)` against `IMessage::IID` reinterpreted
the same way, to avoid the `i8`/`u8` mismatch seen earlier.)

- [ ] **Step 1:** Implement the cid dispatch + interface query into `*obj`.
- [ ] **Step 2:** `cargo build --all-features` (macOS) + `just linux-check`.
- [ ] **Step 3:** `just selftest` for Dexed, TyrellN6, Surge XT, OB-Xd — all still load +
      play (no regression). Note in the commit any plugin whose behavior visibly improves.
- [ ] **Step 4:** Commit `feat: vend IMessage/IAttributeList from the host context`.

### Task A4: Accurate `IPlugInterfaceSupport` (optional polish)

Return `kResultTrue` for `IComponentHandler`/`IComponentHandler2` (which we genuinely
provide) using the same byte-wise `TUID` compare as A3; `kResultFalse` otherwise. Commit
`fix: advertise supported host interfaces accurately`.

**Track A acceptance:** the host context matches the SDK's standard host object; suite +
`just linux-check` green; no plugin regressions.

---

## Track B — Plugin compatibility & containment (handle WaveShell-class)

**Why:** Plugins like WaveShell crash in-process from their own duplicate-Obj-C-class
packaging. We can't fix that host-side; we contain it (isolation) and make it graceful and
recoverable — what DAWs do.

### B1: Auto-isolate known-crashy plugins
`Vst3Host::load_plugin` already routes to isolation only when opted in. Add a builder
opt-in `auto_isolate_risky(true)` (and/or make it default when `process-isolation` is on)
that, for plugins `discovery::has_objc_conflicts()` flags (Waves/WaveShell), loads them
isolated automatically. Files: `host.rs`, `discovery.rs`.
**Acceptance:** loading WaveShell through a default host returns a clean `Err` (helper
crash contained) or loads isolated — the host process never segfaults. Verify with the
`isolated_host` example + a new selftest variant.

### B2: Isolation crash-recovery (auto-respawn)
Finish the deferred Phase 3 item: when `send_command` detects the helper died, respawn it
and re-load the plugin (replaying setup), or surface a typed `PluginCrashed` so the caller
can. Files: `process_isolation.rs`, `internal/isolated_plugin_impl.rs`.
**Acceptance:** a test that kills the helper mid-session and confirms the next call either
recovers or returns `Error::PluginCrashed` (not a generic error); host stays alive.

### B3: A "validate plugin" probe (the DAW rescan feature)
A `Vst3Host::probe_plugin(path) -> PluginStatus { Ok, Crashed, Timeout }` that loads in an
isolated helper and reports whether it initializes without crashing — so a UI can blacklist
bad plugins during a scan. Builds on B1/B2. Files: `host.rs`, `process_isolation.rs`.
**Acceptance:** `probe_plugin(WaveShell)` returns `Crashed` (not a host crash);
`probe_plugin(Dexed)` returns `Ok`.

### B4: Main-thread loading guidance
Document (and optionally provide a helper that asserts) that plugins should be loaded on the
host's main thread; relevant for editor GUIs. Files: `docs/explanation/`, a debug-assert in
`PluginImpl::load`. Low effort; mostly docs.

---

## Track C — Crate readiness (well-made cargo package)

### C1: CI (the biggest missing piece)
GitHub Actions at `.github/workflows/ci.yml`: matrix of macOS + Linux (reuse the
`just linux-check` Docker recipe or a native libxcb/ALSA job) + Windows; run `cargo fmt
--check`, `cargo clippy --workspace --all-features`, `cargo test --workspace
--all-features`, `cargo build --examples`. Cache the VST3 SDK submodule.
**Acceptance:** green CI on a push; the matrix actually compiles Windows + Linux (catches
the next c_char-style portability bug automatically).

### C2: Warnings to zero
Migrate `window.rs` from deprecated `cocoa`/`objc` to `objc2`/`objc2-app-kit` (clears ~50
warnings + the `cargo-clippy` cfg noise), clear the remaining `missing_docs`, then add
`#![deny(missing_docs)]` and `-D warnings` in CI (flip `just lint`). Files: `window.rs`,
`lib.rs`, CI. This is GUI code — verify the macOS editor window still opens interactively.

### C3: Publish readiness
`Cargo.toml` metadata audit (keywords/categories present; `readme`, `repository`,
`documentation`, `rust-version` floor); add `CHANGELOG.md`; ensure `LICENSE-MIT` +
`LICENSE-APACHE` exist; `cargo publish --dry-run -p vst3-host` clean; verify `docs.rs`
builds with `--all-features` (the `[package.metadata.docs.rs]` cfg is already present).
**Acceptance:** `cargo publish --dry-run` succeeds; docs.rs preview builds.

### C4: API stability pass for 0.1
Review the public surface (what's `pub`, the prelude, error variants) for naming and
forward-compatibility before publishing; decide `#[non_exhaustive]` on `Error`/`MidiEvent`.
Lightweight; a doc + small edits.

---

## Track D — Remaining functional gaps (from `docs/internal/roadmap.md`)

Sequenced after the above; each is its own plan when picked up:
- **Plugin state save/restore** (`getState`/`setState`) — needed for presets; reference
  Jurek's working tool. Likely a quick, high-value add.
- **Editor embedding into egui** (the `egui-widgets` feature) + `IPlugFrame` resize.
- **GUI across the process boundary** (isolated editor windows).
- **MIDI output capture in isolated mode** (in-process already done).
- **Parameter automation execution** (`ParameterAutomation` is computed but not applied).

---

## Recommended order

1. **Track A** (host context) — small, self-contained, improves the broad plugin tail. Do now.
2. **B1 + B3** (auto-isolation + probe) — makes WaveShell-class plugins *handled* (graceful),
   which is the user-visible answer to "handle plugins like WaveShell."
3. **C1** (CI) — locks in cross-platform correctness before more growth.
4. **C2/C3/C4** — publish-ready.
5. **B2, D** — recovery + the functional long tail.

## Self-review notes

- Spec coverage: host-context completion (A), WaveShell handling (B1/B3 + the existing
  isolation), well-made-crate (C). ✓
- The WaveShell crash is explicitly scoped as *contained, not fixed* — no task claims to make
  it load in-process, because that's a Waves packaging issue.
- Verification adapted to COM-FFI (build + real-plugin selftest), stated in Global Constraints.
