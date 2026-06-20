# Platform support

| Platform | Loading & audio | Plugin editor window | Notes |
| --- | --- | --- | --- |
| macOS | Working, exercised against real plugins | Standalone window + embedded-in-egui (`PluginWindow`, `EmbeddedEditor`) | Primary, fully exercised platform. |
| Windows | Builds + tested in CI; not interactively exercised | Standalone window implemented, not runtime-verified | Loading uses the Win32 module path. |
| Linux | Builds + unit-tested in CI (Docker) | X11 via XCB implemented, not runtime-verified | Editor ported from the [khremeviuc1004 fork](https://github.com/khremeviuc1004/rust-vst3-host); see note below. |

CI builds and tests all three platforms on every push (macOS full; Linux build+test+clippy;
Windows build), so compilation is verified everywhere. **"Exercised" — actually running
plugins and opening editors — is macOS-only.** This table reflects what has been *run*, not a
guarantee; reports for Windows/Linux are very welcome.

- **Editor embedding into egui** (`EmbeddedEditor`) is implemented on **macOS only** so far;
  other platforms return an error.
- **Process isolation** (the helper binary) is exercised on macOS; the IPC is platform-neutral
  but hasn't been run on Windows/Linux.

> **Windows/Linux editors — compiled, not run.** The Win32 and X11/XCB plugin-editor windows
> (`vst3-host/src/window.rs`) build in CI but have **not been opened against a real plugin** in
> this project's environment (which is macOS). They are cfg-gated and don't affect the macOS
> build. Building on Linux needs the `libxcb` development headers. If you run them, please
> report back.

## Default plugin scan directories

| Platform | Directories |
| --- | --- |
| macOS | `/Library/Audio/Plug-Ins/VST3`, `~/Library/Audio/Plug-Ins/VST3` |
| Windows | `C:\Program Files\Common Files\VST3`, `C:\Program Files (x86)\Common Files\VST3` |
| Linux | `/usr/lib/vst3`, `/usr/local/lib/vst3`, `~/.vst3` |

Add your own with `Vst3Host::builder().add_scan_path(...)`.

## Build requirements

- No VST3 SDK needed. The `vst3` dependency (0.3) ships pre-generated bindings, so a plain
  `cargo build` works — there is no `VST3_SDK_DIR` and no submodule to initialize.
- `libclang` is required at build time for `cpal`'s `coreaudio-sys` (macOS) and `alsa-sys`
  (Linux, which also needs the ALSA + libxcb dev headers).
- Audio output requires a working device; `play` opens the system default.
