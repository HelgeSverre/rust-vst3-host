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

**Tech stack:** Rust, the `vst3` crate (0.3.0, bindgen over the VST3 SDK), `cpal`, `xcb`
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

## Track A — Complete the host context (the immediate work) — DONE

**Status:** A1–A4 are all implemented in `vst3-host/src/internal/com_implementations.rs`:
`HostAttributeList` (`IAttributeList`, unit-tested), `HostMessage` (`IMessage`),
`HostApplication::createInstance` vends both by byte-wise `TUID` compare, and
`isPlugInterfaceSupported` advertises `IComponentHandler`/`IComponentHandler2` accurately.
Suite + Linux build green; no plugin regressions.

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

### B1: Auto-isolate known-crashy plugins — DONE
Implemented as the builder opt-in `Vst3HostBuilder::auto_isolate_problematic(true)`:
`load_plugin` routes to isolation when the flag is set and
`internal::module_loader::has_objc_conflicts(path)` flags the plugin (Waves/WaveShell).
Files: `host.rs`. The host process never segfaults on such plugins (the crash is contained
by the helper).

### B2: Isolation crash-recovery (auto-respawn) — ✅ DONE
A dead/crashed/hung helper now maps to a typed `Error::PluginCrashed` / `PluginTimeout`
(host stays alive); `Plugin::recover()` respawns + reloads + restarts processing. Recovery
is explicit (not inline) to keep respawn off the audio thread. Added `Plugin::isolation_pid`.
Files: `process_isolation.rs`, `internal/isolated_plugin_impl.rs`, `plugin.rs`.
**Acceptance (met):** `test_isolation_crash_recovery` kills the helper mid-session → next
call returns `PluginCrashed` and the host survives → `recover()` reloads Dexed (2238 params).
*Deferred:* parameter values are not replayed on recovery (pair with state save/restore).

### B3: A "validate plugin" probe (the DAW rescan feature) — DONE
`Vst3Host::probe_plugin(path) -> ProbeResult { Ok, Crashed, TimedOut, Failed(String) }`
loads in an isolated helper and reports whether it initializes without crashing. Files:
`host.rs`. Used by the compatibility-matrix harness to gate in-process capability checks.

### B4: Main-thread loading guidance — ✅ DONE
Added `docs/explanation/threading.md` (which thread each call belongs on, why) and linked it
from `docs/README.md`. No debug-assert: a portable main-thread check doesn't exist in safe
Rust, so a wrong guess would mislead — the contract is documented instead.

---

## Track C — Crate readiness (well-made cargo package)

### C1: CI (the biggest missing piece) — ✅ DONE (green on all 3 platforms)
GitHub Actions at `.github/workflows/ci.yml`: macOS (fmt + clippy `-D warnings` + test +
examples), Linux (libxcb/ALSA build + test + clippy `-D warnings`), Windows (LLVM + build).
**Acceptance (met):** the run on push **27844853409** is green across macOS, Linux, and
Windows. Standing CI up immediately paid off — it surfaced and we fixed: (1) a missing
`.gitmodules` that broke the vst3sdk submodule checkout for every fresh clone, (2) Linux-only
clippy dead-code + `c_char` cast lints, and (3) the first-ever Windows compile (HWND import,
`Result` alias misuse, i32/u32 SDK-enum casts) — exactly the portability class CI exists to
catch. *Minor follow-up:* `actions/checkout@v4` emits a Node 20 deprecation notice (bump to
v5 when convenient).

### C2: Warnings to zero — ✅ DONE (runtime GUI check pending)
`window.rs` migrated to `objc2` + `objc2-app-kit` + `objc2-foundation` (typed AppKit APIs);
`cocoa`/`objc` removed from the dependency graph. Both the module-level `#![allow(deprecated)]`
and the crate-root `#![allow(unexpected_cfgs)]` (which only existed for the old `msg_send!`)
are gone. `-D warnings` was already enforced in `just lint` + CI. Files: `window.rs`,
`lib.rs`, `Cargo.toml`.
**Verified:** clean build + clippy `-D warnings` (no allows), workspace tests green, Linux
build via Docker green (objc2 deps are macOS-only). **Pending:** opening a real editor window
is GUI runtime behavior — verify interactively (`cargo run --example plugin_gui`).

### C3: Publish readiness — DONE
`Cargo.toml` metadata complete (keywords, categories, `readme`, `repository`,
`documentation`, `homepage`, `license`, and now `rust-version = "1.81"`); `CHANGELOG.md`,
`LICENSE-MIT`, `LICENSE-APACHE` all present; `[package.metadata.docs.rs]` builds all-features.
**Acceptance (met):** `cargo publish --dry-run -p vst3-host --all-features` succeeds and
verifies. *Fixed along the way:* 537 stale `vst3-host/target/` files had been committed
before `.gitignore` covered them and were being packaged — the dry-run package was 142 MiB
(46 MiB compressed, far over the 10 MiB crates.io limit). After `git rm -r --cached
vst3-host/target`, the package is **37 files, 114 KiB compressed**.
*Caveat (pre-existing, inherited from `vst3`):* a real `cargo publish`/docs.rs build needs
the VST3 SDK via `VST3_SDK_DIR` — documented in the README and CHANGELOG known-limitations.

### C4: API stability pass for 0.1 — DONE
`Error` and `MidiEvent` are both `#[non_exhaustive]` (match with a wildcard arm; variants can
be added without a breaking change). Public surface reviewed; the prelude deliberately omits
`Result` (would shadow `std::result::Result`).

---

## Track D — Remaining functional gaps (from `docs/internal/roadmap.md`)

Sequenced after the above; each is its own plan when picked up:
- **Plugin state save/restore** (`getState`/`setState`) — ✅ DONE. `Plugin::save_state`/
  `load_state` via a host-side `MemoryStream` (`IBStream`); bridged across process isolation
  (`SaveState`/`LoadState` IPC). Verified by exact byte round-trip + value restore against
  Dexed, in-process and isolated. Single- vs separate-component handled (`setComponentState`
  only for separate controllers).
- **Editor embedding into egui** (the `egui-widgets` feature) + `IPlugFrame` resize — ✅ DONE
  (macOS: `EmbeddedEditor`, `HostPlugFrame`, `Plugin::take_editor_resize_request`).
  Windows/Linux embedding still TODO (macOS-only `cfg` today).
- **GUI across the process boundary** (isolated editor windows) — ✅ DONE (macOS). The helper
  owns its editor window (main-thread `NSApplication` event pump + worker-thread commands);
  `CreateGui` returns the real editor size. `just isolated-gui`. Windows/Linux helper run loop
  still TODO.
- **MIDI output capture in isolated mode** — ✅ DONE (piggybacked on the per-block `Process`
  response; `IsolatedPluginImpl` buffers it; `Plugin::take_output_midi` works isolated).
- **Parameter automation execution** — ✅ DONE. Changes feed the processor's input parameter
  queue per block (`set_parameter_at` + sample offsets), dual-written to the controller.

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
