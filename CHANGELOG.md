# Changelog

All notable changes to `vst3-host` are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims to follow
[Semantic Versioning](https://semver.org/) (pre-1.0: new features bump the minor version).

## [0.10.0] - 2026-10-09

### Added

- MIDI controller assignment queries, one-to-many `IMidiMapping2` controller mappings, live MIDI
  learning, and keyswitch/articulation metadata. Live input can be delivered through
  `Plugin::send_live_midi_event`, `AudioHandle::send_live_midi`, and `MidiSink::send_live_midi`;
  service deferred learning with `service_host_requests` on the control thread.
- `TransportPosition` and explicit transport seeking through `Plugin`, `AudioHandle`, and
  `RtControl`. Timeline playback now synchronizes the plugin's tempo and project position.
- `Plugin::is_editor_open`, `attached_editor_size`, and `attached_editor_can_resize` query an
  existing editor without creating a temporary view. These queries describe in-process views.
- Bus activation requests are exposed as `HostNotification::BusActivationRequested`, leaving
  the host in control of whether to change routing.

### Fixed

- Multi-architecture VST3 bundles select binaries compatible with the host process. Windows
  on Arm supports the appropriate Arm64EC/Arm64X fallbacks; incompatible bundles report a
  clear load error naming the process architecture and searched folders.
- Linux `ModuleEntry` receives the shared-library handle required by the VST3 ABI.
- Windows builds normalize VST3 enum constants to the unsigned field types they populate.
  Isolation helpers and discovery probes are located using their `.exe` filenames.
- MIDI program changes honor plugin controller mappings before falling back to program lists.
- Plugin setup and reconfiguration refresh cached latency/tail values; MIDI learning and
  transport context handling follow the controller and processor contracts more closely.
- CI metadata tests use a checked-in SDK-generated fixture instead of an installed Dexed
  plugin. Teardown tests still verify that destruction never occurs on the wrong thread,
  without leaking their own test allocations under LeakSanitizer.
- Updated `rtrb` to 0.3.5 and the workspace's `webbrowser` dependency to 1.2.2 to address
  RUSTSEC-2026-0274 and RUSTSEC-2026-0257. The published crate requires `rtrb >= 0.3.5`.

### Compatibility

- `HostNotification`, `process_isolation::HostCommand`, and `HostResponse` have new variants.
  Downstream exhaustive matches must handle them. Rebuild and deploy the isolation helper
  alongside the matching library version when using the new commands.
- Bundles without a compatible Windows/Linux binary now return `Error::PluginLoadFailed`
  instead of resolving another architecture's binary or attempting to load a directory.
- The library's default-feature MSRV remains Rust 1.85. The optional `egui-widgets` feature
  still requires Rust 1.92 or newer.

## [0.9.0] - 2026-07-28

### Changed (VST3 spec-compliance pass — behavior, some breaking)

A pass over the whole host against the VST3 3.7 specification: every place the library
guessed at a contract, sent a plugin something the spec forbids, or claimed a capability it
did not have. Most of it is invisible — plugins simply behave correctly now. These are the
parts that are visible from your code.

- **MIDI CC, pitch bend and channel aftertouch are now parameter changes, not events
  (breaking).** VST3 has no MIDI controller event: a plugin declares through `IMidiMapping`
  which of *its own* parameters each controller drives. `send_midi_cc`,
  `MidiEvent::PitchBend` and `MidiEvent::ChannelAftertouch` are looked up in that table and
  queued as sample-accurate parameter automation. **A controller the plugin does not map is
  now dropped** — silently, still returning `Ok(())` — because the previous code put
  `kLegacyMIDICCOutEvent` entries on the component's *input* event list, which the spec
  forbids; well-behaved plugins ignored them and the rest could misbehave. Plugins that
  implement `IMidiMapping` (most do) respond correctly for the first time; plugins that don't
  no longer receive controllers at all. Call `Plugin::midi_cc_to_parameter` first if you need
  to know. Poly aftertouch is unaffected — it is a first-class VST3 event. `midi_panic` now
  sends real note-offs for every tracked sounding note before routing CC 123/120/121 the same
  way, so it works on plugins that map no controllers.
- **`MidiEvent::ProgramChange` is a no-op on plugins without a program list (breaking).** It
  used to return `Err`. Program changes are resolved through a cache built at load, so the
  audio thread no longer scans `IUnitInfo` per event.
- **`save_state` returns a different byte format (breaking on downgrade).** VST3 defines two
  independent state streams and the library only ever saved one, so a restored plugin's
  editor could disagree with its sound. The blob is now a versioned envelope
  (`VST3HOST_STATE` magic, version, two lengths) carrying the component *and* controller
  streams. Still opaque, still `Vec<u8>`; **blobs written by older versions still load**
  (anything without the magic is treated as a bare component stream). Blobs written now are
  not readable by older versions. `save_vstpreset` gained the matching `"Cont"` chunk
  alongside `"Comp"`.
- **Sidechain and aux buses no longer start active (breaking).** Bus activation at load now
  honours `kDefaultActive` instead of switching every bus on. Secondary buses conventionally
  omit that flag, so a sidechain input that used to receive audio now needs an explicit
  `Plugin::set_bus_active`. An `activateBus` failure is a load error rather than a logged
  warning.
- **Windows class-id spelling fixed (breaking on Windows).** VST3 stores the first three GUID
  fields in COM byte order on Windows; the library hex-encoded them in memory order, so
  `PluginInfo::uid` was a byte-swapped non-canonical id there. It is now canonical on every
  platform. The old spelling is still accepted when *matching* (preset class ids,
  `load_plugin_class`), so existing presets load — but a Windows session keyed on the old uid
  string will not compare equal.
- **`get_parameter_changes` also reports processor-emitted automation.** `ProcessData`'s
  `outputParameterChanges` were wired up but never read, so a plugin's own automation output
  (LFOs, envelope followers, mapped CCs) was discarded. It is now drained into the same poll,
  merged with editor edits rather than short-circuited by them, and marshalled across process
  isolation (where the call previously always returned an empty `Vec`).
- **Stepped parameters quantize per spec (breaking, silent).** `Parameter::step_index` /
  `normalized_to_plain` truncate over `step_count + 1` buckets, as VST3 defines, instead of
  rounding over `step_count`. Same inputs, different plain values on stepped parameters.
- **`RtControl` and `AudioHandle` parameter changes no longer call the controller from the
  audio callback.** `setParamNormalized` belongs to the main-thread domain; the audio path now
  queues processor automation only, and the controller catches up from a bounded deferred-sync
  queue the next time a control-thread call runs. A parameter set from the audio side reaches
  the DSP immediately, the editor on the next control-thread touch.
- **`DetailedPluginInfo` gained `module_info` and `compatibility` (breaking for struct
  literals).** JSON round-trips are unaffected (both fields are `serde(default)`).
- **Editor resize follows the spec callstack.** `IPlugFrame::resizeView` now calls
  `IPlugView::onSize` back from inside the same call, as VST3 requires, and validates the
  view pointer and rectangle instead of just acknowledging. `PluginWindow::is_open` /
  `closed_by_user` may now enter plugin code (they service pending platform events);
  `EmbeddedEditor` is `!Sync` because it caches the last negotiated size.
- **Embedding a Wayland surface fails with an actionable error** naming the missing
  `IWaylandHost`/`IWaylandFrame` support, instead of "expected an X11 handle".
- **A preset load no longer claims to be a project load (breaking, silent).** VST3 lets a
  plugin ask where the bytes it is being handed came from — the host attaches an
  `IStreamAttributes` list to the `setState` stream and the plugin reads
  `PresetAttributes::kStateType` off it (the SDK ships `Vst::Helpers::isProjectState()` for
  exactly this). Every restore used to be tagged `StateType::kProject`, so a plugin could not
  tell a `.vstpreset` load from a session restore and no source path was ever provided. Now:
  `Plugin::load_state` still means a project restore (`kProject`, unchanged), while
  `load_vstpreset` and `load_preset` tag the stream `StateType::kTrackPreset` and publish the
  file's full path under `PresetAttributes::kFilePathStringType` (with the file's stem as
  `IStreamAttributes::getFileName`). `kTrackPreset` is the choice because `kProject` *is* the
  project case and `kDefault` is narrower than it reads — the SDK defines it as "restored from
  a preset (marked as *default*) or the host wants to store a default state of the plug-in" —
  so only `kTrackPreset` says "a preset file the user picked" without lying. A plugin that
  restores differently for a preset than for a session will now do so. New public
  `vst3_host::plugin::StateContext` and `Plugin::load_state_with_context` for hosts holding
  preset bytes they loaded some other way; the context crosses process isolation, so an
  isolated plugin's `setState` sees identical attributes. The isolation wire is skew-safe in
  both directions — the new `LoadState` field is `serde(default)`, and a helper that predates
  it ignores what it does not know and does the project restore it always did.

### Added (VST3 spec-compliance pass)

- **64-bit processing as a fallback.** The host negotiates `canProcessSampleSize` at load,
  prefers `kSample32`, and drives the plugin in `kSample64` when it refuses 32-bit. Your
  buffers stay `f32`; a plugin supporting neither now fails to load instead of being set up
  wrong.
- **`IComponentHandler2` tells the plugin the truth, and you can see what it asked for.**
  `setDirty` / `requestOpenEditor` / `startGroupEdit` / `finishGroupEdit` used to return
  "done" and discard the request. They are now queued and drained by
  `Plugin::take_host_notifications`, joined there by `IUnitHandler`/`IUnitHandler2` (unit
  selection, program-list changes), `IComponentHandler3` context menus (with
  `execute_context_menu_item` / `dismiss_context_menu`) and `IProgress` reports. **Drain it
  regularly**: the queue caps at 1024 and a full queue is refused back to the plugin with
  `kResultFalse`. Note that this stream and `take_parameter_edits` are ordered only within
  themselves — the group-edit brackets cannot yet be matched to the edits they enclose.
- **Restart flags are readable and actionable.** `Plugin::take_restart_flags` gained
  accessors for the remaining `kRestartFlags` (`midi_cc_assignment_changed`,
  `note_expression_changed`, `io_titles_changed`, `prefetchable_support_changed`,
  `routing_info_changed`, `keyswitch_changed`, `param_id_mapping_changed`), and the new
  `Plugin::service_host_requests` performs the deactivate/re-setup/reactivate dance a latency
  or I/O change requires. Both marshal across process isolation, where restart flags
  previously always came back empty.
- **`moduleinfo.json`, snapshots and class compatibility.** `discovery::read_module_info`
  parses a module's declared metadata (bounded and validated before anything is loaded),
  `discovery::get_plugin_compatibility` falls back to the factory's `IPluginCompatibility`
  class, and `discovery::discover_plugin_snapshots` lists the standard UI snapshot PNGs by
  reading directory metadata only. `Plugin::class_compatibility` / `replaced_class_ids` expose
  the retired class ids a plugin supersedes, and `Vst3Host::load_plugin_class` loads a
  specific class from a multi-class factory. These live in `vst3_host::discovery`; they are
  not re-exported at the crate root.
- **`IPluginFactory3`.** The host context is passed to the factory, and class metadata is read
  as UTF-16 where available, so non-ASCII names and versions are no longer mangled.
- **New plugin interfaces the host now speaks:** `IEditControllerHostEditing`
  (`begin_host_edit` / `end_host_edit`), `IMidiLearn` (`send_midi_learn`), `IAutomationState`
  (`set_automation_state`), `IRemapParamID` (`remap_parameter_id`, for loading a project saved
  with an older version of a plugin), `IProgramListData` / `IUnitData` (`get_program_data` /
  `set_program_data` / `get_unit_data` / `set_unit_data`), `IUnitInfo` unit selection
  (`selected_unit` / `select_unit`, `program_pitch_names`), `IStreamAttributes` (host state
  streams now declare their `StateType`), and `IDataExchange` — a bounded host-side queue with
  `Plugin::take_data_exchange_blocks` for plugins that stream analysis data to their editor.
  `IPlugInterfaceSupport` advertises exactly these, and no longer claims host-side interfaces
  it has no business offering.
- **Editor sizing and scaling.** `Plugin::editor_can_resize` / `resize_editor` negotiate a
  size through `checkSizeConstraint` + `onSize` and return what the plugin accepted;
  `Plugin::set_editor_scale_factor` offers a content scale through
  `IPlugViewContentScaleSupport`. On Windows `PluginWindow` applies the window's DPI
  automatically at open and on `WM_DPICHANGED` (serviced by the new
  `PluginWindow::service_platform_events`). `EmbeddedEditor::try_set_rect` reports the
  negotiated rectangle and `take_resize_request` surfaces plugin-initiated resizes.
- **Full VST3 event I/O.** `PluginEvent` / `PluginEventData` carry every event type the spec
  defines — including SysEx (`Plugin::send_sysex`, `send_sysex_at`), note expression text,
  chord and scale events — in both directions (`take_output_events`, plus the lock-free
  `output_event_handle`). `MidiEvent` remains the convenience layer over it.
- **Per-bus audio.** `Plugin::audio_bus_layout`, `create_bus_audio_buffers` and
  `process_bus_audio` let a host address a plugin's individual buses (sidechain in, multi-out)
  instead of one flattened channel list.
- `RtControl::service_teardown` — see the fix below.

### Fixed (VST3 spec-compliance pass)

- **A zero-sample flush handed the plugin audio buses it must not see.** VST3's parameter-only
  `process()` call requires no bus counts and no channel pointers; both are now cleared for
  that call and restored afterwards, including when the plugin reports failure.
- **Bus arrangements were never negotiated.** The plugin's own advertised arrangements are now
  fed back through `setBusArrangements` before the first `setupProcessing`, and again when it
  raises an I/O restart, so a plugin that only finalizes its layout on that call is configured
  rather than assumed.
- **Component↔controller messages from the wrong thread vanished without trace.** VST3
  requires `IConnectionPoint::notify` on the UI thread and the host's proxy drops anything
  else, exactly as the SDK reference host does — but silently. The drops are now counted and
  logged (first occurrence, then every 256th, plus a total when the plugin unloads), so a
  plugin whose meters never update is diagnosable. The "UI thread" is the thread that loaded
  the plugin; see [the threading model](docs/explanation/threading.md).
- **The discovery host context could dangle during module unload.** `IPluginFactory3` stores
  the `setHostContext` pointer in a module global without retaining it, and the host
  application was declared after the module in all three discovery paths — so it dropped
  first, leaving that global pointing at freed memory for the rest of teardown. It now
  outlives the factory and the module.
- **A dead isolation helper looked like "nothing to report".** The polling accessors
  (`get_parameter_changes`, `take_parameter_edits`, `take_host_notifications`,
  `take_data_exchange_blocks`, `take_restart_flags`, `latency_samples`, `tail_samples`,
  `midi_cc_to_parameter`) turned a transport failure into an empty result. They still return
  the same types, but now log the failure once per death; the crash itself continues to
  surface from the next fallible command.
- **`RealtimePluginRunner` could destroy a plugin on the audio thread.** Teardown is handed to
  the thread that created the runner over a one-slot channel and serviced by
  `RtControl::service_teardown`; a full or disconnected channel leaks rather than running COM
  termination and unloading executable code from a real-time callback.
- **An isolated load that killed the helper is retried once with a fresh helper.** Some real
  plugins lose a race inside their own cold-start initialization and take the process down
  with them: Dexed (JUCE) segfaults or aborts in `juce::MessageQueue::runLoopSourceCallback`,
  dispatching an async update into an object whose construction has not finished, on ~6% of
  cold loads in a fresh helper (measured 13 failures in 200 loads). Nothing the host does
  provokes or prevents it. A helper that dies *during* `LoadPlugin` held nothing worth keeping
  — its load never completed — so the load is now replayed once against a freshly spawned
  helper (250 ms backoff, `log::warn!` with the crash detail). This covers the initial
  isolated load, `Vst3Host::probe_plugin`, `Plugin::recover()` and the auto-recover respawn
  alike, and it is bounded at one retry so a plugin that genuinely cannot load still errors
  after two attempts. Only `LoadPlugin` is replayed — every other command runs against a
  helper holding live plugin state a fresh process would not have. Separately, an auto-recover
  attempt whose respawn+reload failed used to abandon the command outright; it now spends one
  of `auto_recover_max_retries` and tries again, which is what that budget always claimed to
  mean.

### Fixed (third pass — five parallel subsystem reviews)

Five reviewers went over the whole workspace — COM internals, the realtime path, process
isolation, the public surface, the inspector — and every confirmed finding was fixed and
regression-tested (four new test files, ~50 findings). Highlights:

- **Plugin stdout could permanently desync the isolation protocol.** The helper's protocol
  channel was fd 1, shared with the plugin it hosts; one `printf` line and every later
  command received the *previous* command's reply — silently. The helper now claims a
  private fd for the protocol and points fd 1 at stderr before any plugin code runs.
  Relatedly, NaN/Inf samples no longer break the Process exchange: audio crosses the wire
  as base64 bit patterns instead of JSON numbers (which encode non-finite as `null`).
- **The VST3 lifecycle order was wrong on the primary path**: `setActive` ran before any
  `setupProcessing`, so a plugin that sizes its DSP during activation prepared at its
  default rate. Measured on Dexed at 96 kHz: envelope timing was 48% off, now 1.2%. Setup
  now runs first, and a changed configuration deactivates before re-running setup, as the
  spec requires and `reconfigure` already enforced.
- **`IComponent::initialize` failures were ignored**, and error unwind after a successful
  init released COM objects without `terminate()` before unloading the bundle — a plugin
  thread started in init could be executing unmapped code. Both fixed; teardown is a scope
  guard sharing its implementation with `Drop`.
- **The duplex input bridge dropped samples, not frames, on ring overflow** — one overflow
  left stereo permanently L/R-swapped. Push/pop are now frame-atomic and the ring capacity
  is always a whole number of frames.
- **A probed plugin's grandchild could hang `discover_plugins_safe` forever** (and
  `Plugin::drop`, via the same unbounded pipe-reader join in the isolation supervisor):
  the pipe only hits EOF when *every* write end closes, and license-daemon-style children
  inherit it. All waits are bounded now; readers detach instead of joining.
- **Split blocks fired every queued event in chunk 0** with clamped offsets — up to ~20 ms
  early, collapsing relative timing between events. Events and parameter changes now land
  in the chunk containing their offset, rebased.
- **`MemoryStream` could be panicked across the FFI boundary** (process abort) by a huge
  seek followed by a write; now checked arithmetic against a 64 MiB cap with proper COM
  error codes instead of unwinding out of an `extern "system"` thunk.
- Isolation robustness: a helper crash mid-response is classified as `PluginCrashed`
  immediately (was `Error::Other` for one command cycle); recovery replays live transport
  (tempo, time signature, playing) instead of load-time values; load/save-state commands
  get their own 30 s deadline and a timeout on them is not auto-retried into a
  helper-killing loop; all helper output is bounded and wire-provided counts clamped.
- Panics and contract gaps reachable from safe code: `render_to_wav(f64::INFINITY)`
  capacity-overflow panic; unvalidated `MidiEvent` fields reaching plugins as out-of-spec
  values (pitch 255, velocity 2.0, negative CC bytes); `points_for_block` offsets wrapping
  past `i32::MAX`; `note_to_name` fabricating names for 128–255 that `name_to_note`
  rejects; the builder accepting time signatures the runtime rejects;
  `process_audio`-while-stopped allocating a String per block on the audio thread instead
  of returning `Error::NotProcessing`; unbounded `pending_param_changes` growth while
  stopped; and audio-callback command drains with no iteration cap.
- `restartComponent` flags are recorded and exposed (`Plugin::take_restart_flags`) instead
  of acknowledged and dropped; multi-class factories report the class actually
  instantiated, not the first audio class; macOS bundle loading honors
  `CFBundleExecutable` before falling back to scanning `Contents/MacOS`.

### Changed (breaking — lands in the next release)

- `WindowHandle::from_nsview` / `from_hwnd` are now `unsafe fn`: they grant exactly the
  capability `from_raw` guards, and a safe caller could previously make the plugin
  dereference a garbage pointer. `from_x11` remains safe (X11 ids are integers).
- `AudioHandle` / `RtAudioHandle` are now genuinely `!Send`, as their docs always claimed:
  the `unsafe impl Send` on the cpal stream wrapper is gone (`AudioStream` and
  `AudioBackend::Stream` dropped their `Send` bounds). `MidiSink`, `RtControl` and
  `RealtimePluginRunner` remain `Send`.
- The isolation wire format changed (base64 audio, tagged non-finite floats): the
  `vst3-host-helper` binary must come from the same build as the library (`just helper`).
  A hosted plugin's stdout now appears on the host's stderr.
- `SafeDiscoveryReport` gained a public `error` field (plus `scan_ran()`), so a scan that
  could not run is distinguishable from "no plugins installed".
- `Vst3HostBuilder::build()` rejects time signatures the runtime rejects (denominator not
  in 1|2|4|8|16); previously they were accepted and advertised to every plugin.

### vst3-inspector

- Loading a plugin resets per-plugin UI state: automation detaches from its parameter id
  (it previously kept writing the old id into the newly loaded plugin at ~60 fps), the
  stale edit highlight clears, and held virtual-keyboard keys are released.
- A failed load can be retried — a plugin is only "Current" once its load succeeded — and
  the session file remembers the loaded path, not the last attempt.
- The headless selftest fails on silence instead of printing `SELFTEST OK` regardless.
- MIDI handling: file playback resyncs its clock after a UI stall instead of flushing the
  backlog through the command ring (dropped NoteOffs no longer leave notes stuck), MIDI
  input connects by port name so device hotplug can't bind the wrong port, file-player
  events show up in the MIDI Monitor, and plugin channel aftertouch is labeled correctly.
- The native editor window's close button is detected (macOS visibility probe; Windows
  handles `WM_CLOSE` itself instead of letting the HWND die under a live `IPlugView`) and
  the GUI state resyncs; file dialogs run async so they no longer freeze the plugin
  editor's run loop.

### Fixed (second review pass)

Three more reviewers went at the crate — one debugging a real crash, one attacking the changes
above, one running adversarial inputs through the public API. Everything below was reproduced.

- **Four regressions from the fixes below, before they ever shipped in a release.** The chunked
  `process()` panicked on the audio thread when the caller's channel Vecs had unequal lengths (the
  block length comes from the first channel, so a shorter later channel got sliced past its end);
  it span forever if `block_size` was 0; gating the helper/probe ancestor search on
  `debug_assertions` broke every isolation test in release builds, because test binaries live in
  `target/<profile>/deps` where no other resolver looks; and the inspector's WAV-export toast was
  erased by the reload it triggers, so the export result was never visible at all.
- **`Vst3HostBuilder::build` validates sample rate and block size**, matching
  `Plugin::reconfigure`. The two disagreed: `block_size(0)` gave permanent silence and
  `sample_rate(0.0)` reached `setupProcessing`, where plugins computing `1.0 / sampleRate` produce
  NaN coefficients.
- **`RmsWindow` could be latched to silence permanently by one bad sample.** The running sum is an
  accumulator, so a NaN (or a sample large enough that its square overflows) poisoned it forever —
  evicting the entry subtracts NaN again — and `rms()`'s `max(0.0)` guard renders NaN as `0.0`. A
  meter would read digital silence for the rest of its life while full-scale audio flowed through.
  `RmsWindow::from_duration` also panicked with a capacity overflow on a non-finite duration.
- **A symlink loop made plugin scanning hang forever.** `scan_directory` recursed on `is_dir()`,
  which follows symlinks, with no visited set — so a plug-in folder containing a link to an
  ancestor wedged the scan while the result list grew unboundedly with textually distinct
  duplicates that `dedup` couldn't collapse. Directories are now tracked by canonical path.
- **Automation could stop audio rendering entirely.** Only the interpolating branch of
  `value_at_time` clamped, so a curve whose first or last point was out of range returned it raw to
  `Plugin::set_parameter_at`, which rejects it — and `Timeline::drive_block` propagates that
  *before* processing audio, so rendering stopped once the playhead reached the last point, losing
  that block's MIDI with it. All branches now clamp and map non-finite to a usable value.
- Overflow panics reachable from public APIs: `Timeline::advance_block` after a large
  `seek_frame`, `ParameterAutomation::points_for_block` with an absurd block length, and
  `midi::name_to_note` on an extreme octave (`"C2147483647"`).
- The inspector serviced the Linux editor run loop with a raw `try_lock`, which treats a poisoned
  mutex as a failed race — permanent, not transient — so a VSTGUI editor would stop painting for
  the rest of the session after any audio-thread panic.

### Changed

- `Vst3Host::discover_plugins` now documents that it instantiates every candidate **in-process**,
  so a plugin that aborts during its own init takes the host down with it — verified with a
  licensed Waves plugin that aborts with "Rust cannot catch foreign exceptions" during its license
  check. [`discover_plugins_safe`] exists for this and should be preferred.

### Fixed

Found by a five-lens review of the whole crate; each was confirmed by reading the code, and two
were reproduced standalone.

- **Dropping a `Plugin` with its editor open never detached the view.** Teardown deactivated and
  `terminate()`d the controller, then released the `IPlugView` as a plain field drop — so a view
  that was still `attached()` was destroyed without `removed()`, leaving the plugin's platform
  frame and idle timer pointing at a host window the caller was about to free. `open_editor` /
  `close_editor` paired correctly; the drop path didn't. It now detaches first.
- **A failed `process()` skipped every per-block cleanup.** The early return bypassed the input
  event clear, the input parameter queue clear *and* the pending-change clear (the comment claiming
  otherwise was wrong), so a plugin returning a failure code got every stale event re-delivered on
  each subsequent block — re-triggering held notes — while the queues grew without bound. Cleanup
  now always runs, and the error is a non-allocating `Error::ProcessFailed(tresult)` instead of a
  `format!` on the audio thread (`Error::NotProcessing` likewise).
- **`Plugin::note_off(NoteId)` sent pitch 0 on channel 1.** The event carried only the note id, so
  any synth that matches releases by pitch — most non-MPE synths — never released the real note.
  Note-ons are now tracked (bounded, pre-reserved, no audio-thread allocation) so the release
  reproduces the original channel and pitch.
- **A caller block larger than the configured block size left the tail silent.** The plugin may
  only be handed `maxSamplesPerBlock` at a time and the excess was simply dropped, which is
  reachable in practice: the cpal backend clamps the requested buffer size *up* into the device's
  supported range, and `BufferSize::Default` lets the device choose. That produced an audible gate
  at the device's block rate and a transport advancing at a fraction of real time. Oversized blocks
  are now split into successive chunks; queued events apply to the first chunk.
- **`midi::name_to_note` panicked on non-ASCII input.** It sliced at byte index 1 to detect a flat,
  which is not a char boundary once `to_uppercase` produces a multi-byte char — so `name_to_note("éB3")`
  panicked instead of returning `None`, from a safe `Option`-returning parser fed by text fields.
  Now parsed by chars.
- **Stepped-parameter helpers used the wrong `stepCount` convention.** VST3 counts the gaps between
  discrete values, so a toggle is `1` and an N-value list is `N-1` — the convention this crate
  already relies on in `select_program` and that `test-plugin` uses. But `is_boolean()` tested `== 2`
  and `is_discrete()` tested `> 1`, so a three-way selector was reported as a toggle (its third
  value unreachable through `format_value`) while a real bypass toggle was reported as continuous.
  `is_boolean()` is now `== 1`, `is_discrete()` is `>= 1`, `normalized_to_plain` snaps toggles too,
  and the new `Parameter::step_index` reports which of the `step_count + 1` values is selected.
- **The helper and probe lookup no longer executes binaries found above the executable in release
  builds.** Both resolvers walked up ancestor directories and ran any `target/{debug,release}/…`
  they found — a fine convenience in a cargo tree, an arbitrary-code-execution foothold in a shipped
  app, since the spawned binary is then trusted for everything the host believes about a plugin.
  The ancestor walk is now `debug_assertions`-only; release builds use the explicit path/env
  override or a binary beside the executable.

### Changed

- `Error` gained `ProcessFailed(i32)` and `NotProcessing`. The enum is `#[non_exhaustive]`, so
  matching code is unaffected, but text that used to arrive as `Error::Other` now has its own
  variants.

### vst3-inspector

- The plugin is now loaded on the UI thread. Only introspection runs on a worker — the library's own
  `docs/explanation/threading.md` requires `load_plugin` to run on the thread that will drive the
  GUI, warning that a plugin which builds its controller's resources elsewhere crashes when its
  editor is opened. The inspector was doing exactly that, and superseded loads were dropped (full
  COM teardown) on the worker thread too.
- WAV export no longer always claims the reload failed. It inferred success by checking `self.audio`
  immediately after starting a load that is asynchronous, so the check always saw `None`: users
  always got "reloading the live plugin failed" and never the real export result, and the state
  snapshot it meant to restore was silently discarded. The snapshot now rides along with the reload
  and is applied when it completes. Export also closes the editor window first, so the old instance
  is really gone before a second one loads.

## [0.8.0] - 2026-07-27

### Added

- **Linux `IRunLoop` host service, so VSTGUI-based plugin editors work.** The host frame
  (`HostPlugFrame`) now also implements `Steinberg::Linux::IRunLoop`: the plugin's editor
  registers its file-descriptor event handlers (its X11 connection) and periodic timers with
  the host, which are serviced by a new `Plugin::service_run_loop()` — call it on the UI
  thread every frame (~30–60 Hz) while an editor is open. Without this, VSTGUI-based editors
  (and most non-JUCE UIs) attach but never paint or respond; JUCE-based editors were
  unaffected because they drive their own event loop. Verified end-to-end against sfizz
  (VSTGUI): the editor paints, its knobs and file dialog respond, and audio renders. Timers
  and fd handlers are serviced snapshot-then-invoke so a callback can safely re-register from
  inside itself (VSTGUI does). No-op on non-Linux and under process isolation. Contributed by
  [@Rodvader8](https://github.com/Rodvader8) ([#10]).

### Fixed

- **`outputParameterChanges` is cleared before each process block.** The container is reused
  across blocks; without a reset, a plugin emitting output parameter changes for the same id
  every block kept being handed its own already-active queue and appending to it — mixing
  stale points into later blocks, making sample offsets describe more than the current block,
  and growing point storage without bound (so the audio thread could reallocate long after
  warm-up). The clear resets the active count only, so pooled queues keep their capacity and
  the reset stays allocation-free. Reported by [@Boscop](https://github.com/Boscop) ([#8],
  fixed in [#9]).
- The Linux run-loop registry is cleared in `close_editor`, so a plugin that doesn't
  unregister its own event handlers or timers can't leave the host dispatching into a removed
  view on the next `service_run_loop()`.
- **Host-side buffers a plugin fills are now all bounded.** A sweep for more of the
  unbounded-growth class [#8] reported found four more buffers with the same shape — written
  from the plugin side, drained only by an optional host poll. They now share the policy
  outgoing MIDI already used (`MAX_OUTPUT_MIDI`): pre-reserved to the cap so steady-state
  pushes never reallocate, and pushes past it dropped.
  - The `IComponentHandler` gesture log grew for the plugin's lifetime unless the host called
    `Plugin::take_parameter_edits()`, which is optional — dragging one knob appends per UI
    frame.
  - The raw `performEdit` sink grew the same way while the plugin wasn't processing.
  - The editor-parameter stash was appended **on the audio thread** every block and drained
    only by `Plugin::get_parameter_changes()`, which `RealtimePluginRunner` never calls — so
    the runner documented as allocation-free in steady state would grow and reallocate on the
    audio thread for as long as an editor stayed open.
  - `HostEventList` had no cap and `process()` (its only drain) returns early while stopped,
    so queueing MIDI at a stopped plugin grew without bound and then dumped every stale event
    into the first block once it started.
- Drains no longer `mem::take` their buffers, which left a zero-capacity `Vec` behind so the
  next push reallocated — the anti-pattern `HostEventList::for_each_then_clear` already
  documented avoiding.

### Known limitations

- Nothing drains editor feedback in the realtime path, so a `play_realtime` host sees no
  editor parameter changes (they are bounded and discarded rather than accumulating). Giving
  `RtAudioHandle` a parameter-feedback ring like `AudioHandle`'s is still to do.
- GUI across the process-isolation boundary is still not implemented, and
  `Plugin::service_run_loop()` is a no-op for isolated plugins.

## [0.7.0] - 2026-07-14

### Added

- **Full `PluginInternal` parity for process-isolated plugins.** Everything the in-process
  path supports now marshals across the IPC boundary:
  - `Plugin::reconfigure` and `Plugin::set_process_mode` (`kRealtime`/`kOffline`), so
    faster-than-real-time offline bouncing works out-of-process. Contributed by
    [@ro-ag](https://github.com/ro-ag) ([#7], fixes [#6]).
  - `bus_arrangements` / `set_bus_arrangements` (speaker-layout query and negotiation),
    `get_units` (unit/program-list enumeration — previously silently empty),
    `latency_samples` / `tail_samples` (previously silently `0`), and
    `midi_cc_to_parameter` (`IMidiMapping` — previously silently `None`).
- **Crash recovery replays the runtime config.** `Plugin::recover()` (and auto-recover)
  reloads a respawned helper at the last `reconfigure`d sample rate / block size and
  re-applies a non-default process mode, instead of silently reverting to the load-time
  settings. Verified end-to-end by a capstone that kills the helper and proves the rendered
  pitch is unchanged.
- MIDI input example (`examples/`).

### Fixed

- The isolation helper sized `process` output buffers as *bus count × 2* (a stereo-per-bus
  assumption), silently dropping channels beyond two per bus after a negotiated non-stereo
  arrangement; it now uses the plugin's live per-bus channel count.
- `Plugin::output_channel_count()` on an isolated plugin returned the load-time snapshot
  forever; a successful `set_bus_arrangements` now refreshes it from the arrangement the
  plugin actually applied.
- The helper committed its tracked sample rate before the plugin accepted a `Reconfigure`;
  a rejected reconfigure no longer desyncs the post-crash reload rate.
- `Plugin::reconfigure` rejects `block_size > i32::MAX` instead of silently truncating it in
  the internal casts.

## [0.6.1] - 2026-07-08

### Fixed

- **Memory leak: host-owned COM objects leaked a reference on every `process()` setup.** The
  host handed borrowed, host-owned COM objects to the plugin with `to_com_ptr().into_raw()`,
  which transfers a `+1` reference that is never released. The affected pointers — `ProcessData`
  input/output event lists and parameter changes, the `IComponentHandler` passed to
  `setComponentHandler`, and the `IParamValueQueue` pointers returned from `IParameterChanges` —
  are only borrowed for the duration of the call, so they are now passed as borrowed pointers via
  `as_com_ref().as_ptr()`. The `queryInterface` sites are unchanged: returning ownership there is
  required by the COM contract. LeakSanitizer reported 944 bytes leaked in 17 allocations before
  the fix; the sanitizer suite is clean after. ([#4])
- Bumped the `crossbeam-epoch` lockfile entry to 0.9.20 to clear an advisory flagged by
  `cargo-deny`.

## [0.6.0] - 2026-07-06

### Added

- **Timeline / sequencer engine** (`transport` module): schedule MIDI clips (`MidiClip`) and
  parameter-automation lanes (`AutomationLane`) on a beat grid and drive them into a plugin
  sample-accurately. `Timeline::advance_block` resolves the next block to `(event, sample_offset)`
  and `(param_id, sample_offset, value)` lists; `Timeline::drive_block` pushes them into a
  `Plugin` and renders one block. Beats timebase at a constant tempo (a tempo curve is future
  work). Exported from the crate root and prelude.
- `RtControl::send_midi_at`, `AudioHandle::send_midi_at`, `MidiSink::send_midi_at`, and
  `HostCommand::SendMidiAt` (isolation) carry a MIDI sample offset end-to-end, so scheduled notes
  land at the same block position on the in-process, lock-free, and isolated paths (the realtime
  and mutex playback bridges previously dropped the offset to block start).
- `Plugin::output_midi_handle()` returns an [`OutputMidiConsumer`] — a `Send` + `Sync` handle
  for draining the MIDI a plugin emits (arpeggiators, MPE, thru) from any thread without locking
  the audio thread. Pairs with `RealtimePluginRunner`: the lock-free runner previously had no way
  to surface output MIDI; now you take the handle, move the plugin into the runner, and poll from
  your UI thread while the audio thread pushes. In-process only (`None` under process isolation,
  where output MIDI crosses the boundary in IPC responses).
- **Android build support**: the crate now compiles for `target_os = "android"` (verified
  on-device on `arm64-v8a`, Android 13). Android reuses the Linux VST3 module path — `dlopen`
  plus the required `ModuleEntry`/`ModuleExit` entry points — so discover → load → parameters →
  `process_audio` all run natively on a phone through the library's public API. Plugin-editor
  windowing is not supported on Android (`PluginWindow::open` returns an error; there is no X11).

### Changed

- Output MIDI is now buffered in a lock-free bounded queue (`crossbeam-queue`) instead of an
  `Arc<Mutex<Vec>>`, so capturing emitted events takes no lock on the audio thread (oldest event
  dropped when full, as before).

- `RealtimePluginRunner::process` is now allocation-free and `Drop`-free in steady state — no
  per-block heap allocation, reallocation, or free once warmed up, even while parameter changes
  and MIDI (in and out) flow — given a fixed-size buffer and an in-process plugin. Fixed the
  three steady-state churns: `pending_param_changes` (drained in place instead of `mem::take`),
  the per-block `ParameterValueQueue` recreation (now a reuse pool), and output-MIDI capture
  (drained in place into a pre-reserved buffer). Guarded by `tests/alloc_tests.rs`. Not yet
  lock-free (a few short, uncontended mutexes remain per block); see the `RealtimePluginRunner`
  docs.

## [0.5.0] - 2026-06-23

### Added

- **Program selection** — `Plugin::select_program(unit_id, program_index)` selects a program
  from a unit's `IUnitInfo` program list (via the unit's `kIsProgramChange` parameter), and
  `MidiEvent::ProgramChange` is now honored instead of rejected (routed to the root unit's
  program-change parameter; the MIDI channel is ignored, as VST3 has no channel→unit mapping).
  Marshaled across process isolation.
- **Bus activation** — `Plugin::set_bus_active(media_type, direction, bus_index, active)` calls
  `IComponent::activateBus`, unlocking sidechain/aux/surround buses that must be explicitly
  activated. New public `audio::MediaType` and `audio::BusDirection` enums. Marshaled across
  process isolation.
- **Parameter-edit gesture capture** — `Plugin::take_parameter_edits()` drains an ordered log of
  the plugin GUI's `beginEdit`/`performEdit`/`endEdit` callbacks as `ParameterEdit` /
  `ParameterEditKind`, so hosts can record automation gestures, not just final values (which
  `get_parameter_changes` still returns). Marshaled across process isolation.
- **Runtime transport mutation** — `Plugin::set_tempo`/`set_time_signature`/`set_playing` change
  the `ProcessContext` transport on the next block while processing. Lock-free equivalents on
  `AudioHandle` and `RtControl` apply the change from the audio callback without a lock.
  Marshaled across process isolation.
- **Live MIDI input** (feature `midi-input`) — `midi_input::list_midi_input_ports`, `connect`,
  and `bind_to_handle` bind a hardware/virtual MIDI port (via `midir`) and forward parsed events
  into a running `AudioHandle`. New `AudioHandle::midi_sink()` returns a `Send` `MidiSink` for
  cross-thread MIDI injection. Off by default; additive when disabled.
- Export `AutomationCurve` and `AutomationPoint` from the crate root and prelude — they were
  public but unreachable without the full module path, even though `ParameterAutomation::with_curve`
  takes an `AutomationCurve`.

### Documentation

- Synced the user docs with 0.4.x: documented the crash-resistant discovery API
  (`discover_plugins_safe`, `SafeDiscoveryReport`, `probe_timeout`, the `vst3-host-probe`
  binary), corrected stale "MPE is in-process only" claims (it now works across process
  isolation), and fixed the "no GUI across the boundary" note (macOS isolated editors work).

## [0.4.2] - 2026-06-23

### Changed

- Inspector loads plugins on a background thread: introspection, load, and audio start run off
  the UI thread, so a slow or hanging plugin (e.g. Reason Rack) no longer freezes the window —
  it shows a "Loading…" spinner and stays responsive. (A crashing plugin still aborts the
  process; only process isolation contains crashes.)
- Documented and test-locked the process-isolation hang guarantee: a plugin that hangs the
  helper (including during load) yields a timeout error and the helper is killed, so it can't
  block the host. A plain in-process `load_plugin` is synchronous and cannot be bounded — a
  hanging plugin (e.g. Reason Rack, which expects its host ecosystem) blocks the caller, so
  load it with process isolation.

## [0.4.1] - 2026-06-23

### Fixed

- Restore the `IPluginBase::terminate()` teardown that 0.4.0 dropped. 0.4.0 skipped `terminate()`
  to work around a crash in Waves/WaveShell's own `terminate()`, but that regressed plugins
  which **require** it to break their controller↔component link before release — e.g. Jup-8000
  and Analog Lab V crashed on unload. `terminate()` is the spec-compliant teardown and is needed
  by most (especially dual-component) plugins, so it is called again. WaveShell loads and runs
  but can crash intermittently on unload (a Waves packaging bug); use process isolation if a
  clean unload matters for such plugins.

## [0.4.0] - 2026-06-23

### Added

- **Crash-resistant plugin discovery**: `Vst3Host::discover_plugins_safe()` (and
  `discovery::discover_plugins_safe` / `probe_plugin_info_isolated`) introspect each plugin in
  a throwaway `vst3-host-probe` subprocess, so a plugin that `abort()`s or makes a
  pure-virtual call during instantiation is skipped (reported in `SafeDiscoveryReport.skipped`)
  instead of taking down the host. `Vst3HostBuilder::probe_timeout` bounds each probe. The fast
  in-process `discover_plugins` is unchanged. (Trades scan speed for safety — one process per
  plugin.)
- **Per-note expression (MPE) across process isolation**: `note_on` / `note_off` /
  `send_note_expression` / `note_expressions` now marshal to the isolation helper, so MPE works
  the same in-process and out-of-process (previously the isolated path returned "not
  supported"). Verified end-to-end: a Tuning expression bends a voice an octave across the
  subprocess boundary.

### Removed

- `Vst3HostBuilder::auto_isolate_problematic` and the internal name-based "Objective-C
  conflict" detection. They auto-isolated plugins like Waves/WaveShell out-of-process by
  matching their filename — a band-aid for a crash that is now fixed properly (see below), so
  those plugins load in-process like any other. Explicit `with_process_isolation(true)` is
  unchanged for callers who still want isolation. The hardcoded Ozone discovery blacklist was
  also removed (Ozone loads fine).

### Fixed

- Plugins that crash in their own `terminate()` no longer take down the host. Some plugins —
  notably shell plugins like Waves' WaveShell — null-deref inside `terminate()` on teardown
  (an uncatchable native crash). `Plugin` teardown now stops processing, deactivates, and
  disconnects the component/controller, then drops the COM references (the plugin frees itself
  in its destructor via `Release`) — it no longer calls `terminate()`. Real hosts mitigate the
  same way. WaveShell, Ozone Imager, and Access Virus Editor now load and tear down cleanly
  in-process.
- Loading **or opening the editor of** an editor-style plugin no longer crashes the host.
  Three places created the plugin's editor view to probe it (the `has_gui` check at load,
  `has_editor()`, and `get_editor_size()` — the latter run by `PluginWindow::open()` before
  attaching) and then called `IPlugView::removed()` on the view. But those views were never
  `attached()`, which violates the `IPlugView` lifecycle. Plugins that only initialize their
  close state on attach (e.g. Access Virus Editor) segfaulted in `OnUIClose()`. The probe views
  are now released (dropped) without the spurious `removed()` call; only the real
  attach/detach path (`open_editor`/`close_editor`) calls `removed()`.

## [0.3.0] - 2026-06-22

### Added

- **Per-note expression (MPE)**: `Plugin::note_on` returns a `NoteId`, and
  `send_note_expression(id, NoteExpressionType, value)` targets per-voice expression
  (tuning / volume / pan / brightness / …) via VST3 note-expression events;
  `note_expressions()` discovers what a plugin supports (`INoteExpressionController`). Verified
  end-to-end against a new in-repo `test-plugin/` VST3 synth (`just test-plugin`) — a Tuning
  expression bends one voice an octave.
- `AudioHandle::try_lock` — non-blocking plugin lock for UI/render threads. Returns `None`
  when the audio callback holds the lock (held for each `process_audio` block) instead of
  stalling the caller.
- **Lock-free side channels on `AudioHandle`** so a UI/control thread never contends with the
  audio thread on the hot path: `send_midi` / `set_parameter` / `midi_panic` queue control onto
  a ring the callback drains before each block; `output_levels` (per-channel peak via atomics),
  `drain_output_midi`, and `drain_parameter_changes` read feedback the callback publishes after
  each block. The plugin mutex (`lock()`) is now only needed for rare state ops (preset
  save/load, WAV export, processing start/stop). `play`/`play_with_backend` wire this
  automatically.

### Changed

- Inspector: the Processing tab now scrolls instead of clipping content on a short window,
  secondary sections collapse, and the on-screen keyboard / VU meters are responsive to width.
- Inspector: defaults to the bundled `test_plugins/Dexed.vst3` at startup (was a hardcoded
  system plugin path), falling back to the last-loaded plugin if it isn't present.
- Inspector: repaints continuously so input is always processed promptly — a click that doesn't
  move the mouse no longer waits for the next event to register.
- Inspector: migrated off the egui 0.34 deprecated panel / rounding / `screen_rect` APIs.

### Fixed

- The builder's `sample_rate` / `block_size` are now applied to in-process plugins at load
  (they were ignored — plugins ran at the 44100/512 defaults while `Plugin::sample_rate()`
  reported the configured value).
- Inspector input lag / dropped clicks: the UI thread no longer touches the audio mutex during
  interaction at all. All control (MIDI, parameter edits) and all per-frame feedback (VU
  meters, MIDI monitor, editor parameter sync) now flow through the new lock-free
  `AudioHandle` side channels; the mutex is reserved for rare lifecycle ops. (An interim fix
  used `try_lock` for the per-frame reads.)
- Parameter edits made in a plugin's **own editor GUI** now affect the audio. The plugin
  reports these via `IComponentHandler::performEdit`; the host captured them for display but
  never routed them to the audio processor, so plugins that don't internally relay
  editor→processor changes (e.g. some dual-component synths) ignored GUI knob turns while
  presets still worked. `process()` now feeds those edits into the processor's input
  parameter queue.

## [0.2.1] - 2026-06-22

### Added

- `MidiEvent::from_midi_bytes` — parse a raw channel-voice MIDI message (status + data) into a
  `MidiEvent` (note on/off, CC, pitch bend, aftertouch), for forwarding hardware-controller MIDI.
- Inspector: a "MIDI Input Device" picker that forwards a connected controller's MIDI into the
  loaded plugin live — cross-platform via `midir` (CoreMIDI / ALSA / WinMM); device → plugin
  only, no feedback loop.

## [0.2.0] - 2026-06-22

A large, fully backward-compatible feature release: the public API only grows. Highlights are
real VST3 protocol coverage (transport, units, MIDI mapping, bus arrangements), live and
offline audio I/O, richer process isolation, metering, and a much more capable inspector.

### Added — library

- **Transport / `ProcessContext`**: the playhead now advances per block and advertises validity
  flags; `Vst3HostBuilder::tempo` / `time_signature` configure the transport.
- **Sample-accurate MIDI**: `Plugin::send_midi_event_at(event, sample_offset)`.
- **Sample-accurate parameter automation** is now carried across the process-isolation boundary.
- **Offline process mode**: `ProcessMode` + `Plugin::set_process_mode` (`kRealtime`/`kOffline`).
- **Runtime reconfigure**: `Plugin::reconfigure(sample_rate, block_size)`.
- **Bus-arrangement negotiation**: `audio::SpeakerArrangement` / `BusArrangements`,
  `Plugin::bus_arrangements` / `set_bus_arrangements`.
- **Units & programs**: `Plugin::get_units` (`IUnitInfo`).
- **MIDI controller mapping**: `Plugin::midi_cc_to_parameter` (`IMidiMapping`).
- **Latency / tail**: `Plugin::latency_samples` / `tail_samples`.
- **Presets**: `.vstpreset` load/save (`save_vstpreset` / `load_vstpreset`), JSON `PluginPreset`
  wrappers (`save_preset` / `load_preset`).
- **Live audio input / effect hosting**: `simple::play_with_input`, `play_with_input_backend`.
- **Offline render**: `simple::render_to_wav`, `render_to_wav_with_input`; dependency-free
  `audio::write_wav` / `read_wav`.
- **Test signals**: `audio::SignalSource` (sine / white-noise / WAV) + `InputSource`.
- **Metering**: `audio::PeakMeter` (falling ballistic + timed hold) and `RmsWindow`.
- **Denormal guard**: flush-to-zero / denormals-are-zero around processing (x86 MXCSR, ARM FPCR).
- **egui editor embedding** on Windows and Linux (in addition to macOS).
- **Process isolation**: configurable `Vst3HostBuilder::helper_path` (+ `VST3_HOST_HELPER_PATH`)
  and `response_timeout`; optional `auto_recover_plugins` with `Plugin::recovery_count`;
  bounded shutdown with SIGKILL fallback; GUI across the boundary on macOS.
- **Diagnostics**: actionable architecture-mismatch errors when loading a wrong-arch plugin on
  Windows/Linux (mirrors the macOS Mach-O diagnostic).
- **Input-stream buffer-size negotiation** for capture devices.

### Added — inspector

- Preset save/load, A/B preset compare, audio export to WAV, MIDI file (`.mid`) playback, and a
  parameter-automation demo.
- Session persistence (window size, last tab, MIDI channel, last-loaded plugin) and in-UI error
  surfacing.

### Changed

- Upgraded `egui` / `eframe` to 0.34.

### Fixed

- Playhead now advances during playback (was frozen).
- Crash when closing a plugin editor; UI input lag between events.
- Robustness hardening: poison-lock recovery, NaN-safe automation sort, empty-buffer RMS guard,
  non-finite meter input, MIDI offsets clamped to the actual block.
- Windows/Linux loaders resolve the `.vst3` bundle directory to the inner binary before loading.

### Docs

- New how-to guides: open/embed a plugin editor, sample-accurate automation, monitor audio
  levels; plus architecture and process-isolation explanations.

## [0.1.1] - 2026-06-21

- Relicensed to MIT and published to crates.io with no VST3 SDK build requirement (the `vst3`
  0.3 crate ships pre-generated bindings).

## [0.1.0] - 2026-06-21

- Initial release: safe VST3 hosting — discover, load, parameters, MIDI, audio playback, state
  save/restore, and process isolation.

[Unreleased]: https://github.com/HelgeSverre/rust-vst3-host/compare/v0.8.0...HEAD
[0.9.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.9.0
[0.8.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.8.0
[0.7.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.7.0
[0.6.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.6.1
[0.6.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.6.0
[0.5.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.5.0
[0.4.2]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.4.2
[0.4.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.4.1
[0.4.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.4.0
[0.3.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.3.0
[0.2.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.2.1
[0.2.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.2.0
[0.1.1]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.1.1
[0.1.0]: https://github.com/HelgeSverre/rust-vst3-host/releases/tag/v0.1.0
[#4]: https://github.com/HelgeSverre/rust-vst3-host/issues/4
[#6]: https://github.com/HelgeSverre/rust-vst3-host/issues/6
[#7]: https://github.com/HelgeSverre/rust-vst3-host/pull/7
[#8]: https://github.com/HelgeSverre/rust-vst3-host/issues/8
[#9]: https://github.com/HelgeSverre/rust-vst3-host/pull/9
[#10]: https://github.com/HelgeSverre/rust-vst3-host/pull/10
