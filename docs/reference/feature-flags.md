# Feature flags

| Feature | Default | Enables |
| --- | --- | --- |
| `cpal-backend` | ✅ | The bundled [`CpalBackend`](https://docs.rs/vst3-host/latest/vst3_host/backends/struct.CpalBackend.html) and `Vst3Host::play` / `simple::play`. Pulls in `cpal`. |
| `process-isolation` | ✅ | Out-of-process plugin hosting and the `vst3-host-helper` binary. See [Isolate plugin crashes](../how-to/isolate-plugin-crashes.md). |
| `egui-widgets` | ✖ | `EmbeddedEditor` — embed a plugin's native editor inside an egui/eframe window (macOS). Pulls in `egui` + `raw-window-handle`. |

## Defaults

```toml
[dependencies]
vst3-host = "0.3"   # = cpal-backend + process-isolation
```

`process-isolation` is on by default so the helper binary always builds and isolation
works without extra flags. It only changes runtime behavior when you opt in with
`Vst3Host::builder().with_process_isolation(true)` — the default load path is in-process.

## Minimal build

To drop CPAL and isolation (e.g. you supply your own [audio backend](../how-to/custom-audio-backend.md)
and don't need a child process):

```toml
[dependencies]
vst3-host = { version = "0.3", default-features = false }
```

Without `cpal-backend`, `play`/`simple::play` are unavailable; drive plugins with
`play_with_backend` and your own `AudioBackend`, or call `process_audio` directly.
