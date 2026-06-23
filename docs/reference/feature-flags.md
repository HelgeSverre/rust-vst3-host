# Feature flags

| Feature | Default | Enables |
| --- | --- | --- |
| `cpal-backend` | ✅ | The bundled [`CpalBackend`](https://docs.rs/vst3-host/latest/vst3_host/backends/struct.CpalBackend.html) and `Vst3Host::play` / `simple::play`. Pulls in `cpal`. |
| `process-isolation` | ✅ | Out-of-process plugin hosting and the `vst3-host-helper` binary. See [Isolate plugin crashes](../how-to/isolate-plugin-crashes.md). |
| `egui-widgets` | ✖ | `EmbeddedEditor` — embed a plugin's native editor inside an egui/eframe window (macOS). Pulls in `egui` + `raw-window-handle`. |
| `midi-input` | ✖ | The [`midi_input`](https://docs.rs/vst3-host/latest/vst3_host/midi_input/index.html) module — bind a hardware/virtual MIDI port and forward events into a running `AudioHandle`. Pulls in `midir`. |

## Defaults

```toml
[dependencies]
vst3-host = "0.4"   # = cpal-backend + process-isolation
```

`process-isolation` is on by default so the helper binary always builds and isolation
works without extra flags. It only changes runtime behavior when you opt in with
`Vst3Host::builder().with_process_isolation(true)` — the default load path is in-process.

## Binaries

The crate ships two binaries. The `vst3-host-helper` binary (out-of-process hosting) is
gated on the `process-isolation` feature. The `vst3-host-probe` binary — used by
crash-resistant discovery ([`discover_plugins_safe`](../how-to/discover-plugins.md)) — is
**not** gated on any feature: it builds unconditionally, since it only calls the library's
introspection API.

## Minimal build

To drop CPAL and isolation (e.g. you supply your own [audio backend](../how-to/custom-audio-backend.md)
and don't need a child process):

```toml
[dependencies]
vst3-host = { version = "0.4", default-features = false }
```

Without `cpal-backend`, `play`/`simple::play` are unavailable; drive plugins with
`play_with_backend` and your own `AudioBackend`, or call `process_audio` directly.
