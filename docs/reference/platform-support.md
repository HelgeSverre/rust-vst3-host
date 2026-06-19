# Platform support

| Platform | Loading & audio | Plugin editor window | Notes |
| --- | --- | --- | --- |
| macOS | Working, exercised against real plugins | Standalone native window (`PluginWindow`) | Primary development platform. |
| Windows | Implemented | Implemented, less exercised | Loading uses the Win32 module path. |
| Linux | Implemented | X11 via XCB (`PluginWindow`) | Editor support ported from the [khremeviuc1004 fork](https://github.com/khremeviuc1004/rust-vst3-host); see note below. |

This reflects where each path has been run, not a guarantee. macOS is the most exercised;
contributions and reports for Windows/Linux are welcome.

> **Linux editor — needs verification.** The X11/XCB plugin-editor window
> (`vst3-host/src/window.rs`) was ported from a fork that ran it on Linux, but it has not
> been compiled or run in this project's CI/dev environment (which is macOS). It is
> cfg-gated, so it does not affect the macOS/Windows builds. Building on Linux requires
> the `libxcb` development headers. If you run it on Linux, reports are very welcome.

## Default plugin scan directories

| Platform | Directories |
| --- | --- |
| macOS | `/Library/Audio/Plug-Ins/VST3`, `~/Library/Audio/Plug-Ins/VST3` |
| Windows | `C:\Program Files\Common Files\VST3`, `C:\Program Files (x86)\Common Files\VST3` |
| Linux | `/usr/lib/vst3`, `/usr/local/lib/vst3`, `~/.vst3` |

Add your own with `Vst3Host::builder().add_scan_path(...)`.

## Build requirements

- The VST3 SDK, included as the `vst3sdk` git submodule. Clone with `--recursive`, or run
  `git submodule update --init --recursive`.
- `.cargo/config.toml` points `VST3_SDK_DIR` at the submodule, so no extra setup is needed
  for an in-tree build.
- Audio output requires a working device; `play` opens the system default.
