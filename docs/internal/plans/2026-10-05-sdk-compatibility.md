# VST3 SDK compatibility implementation plan

Scope: the six defects and four missing capabilities identified in the SDK review.
Existing public entry points remain available; new values are owned, serializable Rust
values shared by the in-process and isolated adapters. No SDK pointers escape.

## Interface decisions before implementation

- Transport: add `TransportPosition { samples: i64, quarter_notes: f64 }`, plus
  `Plugin::set_transport_position` and `transport_position`. Signed samples allow
  preroll; finite quarter notes allow tempo-map-aware seeking without deriving beats
  from the current tempo. Existing tempo, signature and playing setters remain.
  Timeline supplies its tempo/position when driving a block. Processing while stopped
  does not move project position. Reconfiguration preserves position.
- Context validity: advertise only values actually maintained by the host. Keep
  unsupported cycle/chord/SMPTE/clock flags clear, regardless of plugin requests.
- MIDI: `MidiController` distinguishes MIDI 1 controllers from registered/assignable
  bank/index controllers. Owned `MidiControllerAssignment` values expose all mappings,
  including multiple parameters for one controller. Query Mapping 2 first; fall back
  to Mapping 1. Controller sends use normalized values and explicit event bus/channel.
- Live input: explicit live-event entry points feed a bounded deferred learn queue.
  Existing sequenced sends do not learn. `service_host_requests` drains learning on the
  control thread; AudioHandle provides a convenience wrapper. Prefer Learn 2, fall
  back to Learn 1 for MIDI 1. Unsupported learn is normal, not an audio callback error.
- Keyswitches: return `Vec<KeyswitchInfo>` for an event bus/channel; absent optional
  support returns an empty list. Preserve SDK type/flags and optional remapped key.
- Bus activation: add a typed HostNotification request. Acceptance of the queued
  notification is not activation; the application uses existing bus activation APIs
  after deciding how to route it. No implicit buffer/layout mutation.
- Progress: expose IProgress on ComponentHandler; preserve the existing notification
  API. Test interface discovery through the actual handler pointer.
- Metadata: invalidate unit metadata on title-change restart flags. Cache latency and
  tail at successful setup and while inactive during latency restarts; getters never
  invoke plugin code.
- macOS helper: stdin and processing remain on a worker. All other plugin operations
  are executed by the main event loop, serialized with native editor callbacks.
  GUI requests must never synchronously dispatch back onto that same main loop.

## Stages and validation

1. Transport/context fixes and API, including IPC and timeline integration. Tests for
   tempo transitions, stopped processing, seeks, invalid flags and reconfiguration.
2. Callback wiring, unit invalidation, latency/tail caching. COM-level regression tests.
3. MIDI mappings/learn, live-input plumbing, keyswitches and bus notifications with
   TestSynth coverage and IPC parity. Validate addresses/values before native calls;
   bound plugin-reported allocation sizes and deferred queue drains.
4. macOS main-thread dispatch with headless isolation tests and editor smoke tests.
5. Documentation, examples where useful, formatting, workspace Clippy/tests, ignored
   TestSynth/isolation regressions, allocation regression checks and final diff review.

Do not infer full MIDI 2 UMP byte-stream support from Mapping 2: this adds the VST3
controller mapping/learn interfaces; the existing device parser remains MIDI 1.

## Implementation notes

The released SDK uses a two-byte MIDI 2 controller and a 12-byte assignment, while
`vst3` 0.3 contains the prerelease four-byte controller under the same interface ID.
An internal adapter uses the released ABI for mapping buffers and by-value learn calls.
The public API remains independent of these native layouts.

## Completed validation

- Workspace all-features tests and doctests: 338 passed.
- Ignored bundled-plugin checks: 70 passed, including allocation checks, native editor
  smoke tests and 61 feature tests. New APIs run both in-process and isolated.
- Targeted integration tests: 19 passed; the hardware audio test initially timed out
  changing device sample rate and passed when retried alone.
- Released MIDI 2 layout and native by-value learn-call regressions passed.
- Minimal-feature build, formatting and diff whitespace checks passed.
- All-target/all-feature Clippy passed with two existing Rust 1.99 diagnostics allowed:
  deprecated `fetch_update` and `chunks_exact_to_as_chunks`. Their suggested replacements
  are newer than the project's Rust 1.85 minimum.
- Full ignored discovery of installed third-party plugins aborted with a foreign
  exception after Ozone Imager loaded. Bundled-plugin coverage passed; system-wide
  discovery and the remaining installed-plugin scans are not claimed as passing.

The final diff review also corrected sample-rate propagation from main-thread helper
reconfiguration to its audio worker. The test fixture returns sentinel latency/tail
values when queried while active, so cached getters are verified against the lifecycle
contract rather than only against constant values.
