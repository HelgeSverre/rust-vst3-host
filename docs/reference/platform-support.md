# Platform support

| Platform | Loading & audio | Plugin editor window | Notes |
| --- | --- | --- | --- |
| macOS | Working, exercised against real plugins | Standalone native window (`PluginWindow`) | Primary development platform. |
| Windows | Implemented | Implemented, less exercised | Loading uses the Win32 module path. |
| Linux | Implemented | Editor returns an error | Audio/loading present; native editor embedding not implemented. |

This reflects where each path has been run, not a guarantee. macOS is the most exercised;
contributions and reports for Windows/Linux are welcome.

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
