# Discover installed plugins

Find VST3 plugins on the system. There are two ways, depending on whether you need
metadata.

## List plugin paths (fast, no loading)

`scan_plugin_paths()` walks the standard VST3 directories (plus any you add) and returns
`.vst3` paths without loading anything. Use it to populate a picker.

```rust
use vst3_host::Vst3Host;

# fn main() -> vst3_host::Result<()> {
let host = Vst3Host::builder().scan_default_paths().build()?;
for path in host.scan_plugin_paths() {
    println!("{}", path.display());
}
# Ok(())
# }
```

## List plugins with metadata

`discover_plugins()` loads each plugin to read its name, vendor, category, bus counts, and
GUI/MIDI capabilities, returning [`PluginInfo`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.PluginInfo.html).

```rust
use vst3_host::Vst3Host;

# fn main() -> vst3_host::Result<()> {
let mut host = Vst3Host::builder().scan_default_paths().build()?;
for info in host.discover_plugins()? {
    println!("{} by {} ({} out)", info.name, info.vendor, info.audio_outputs);
}
# Ok(())
# }
```

> `discover_plugins()` instantiates and initializes every plugin to read metadata, so it
> is slower than `scan_plugin_paths()` and can be affected by a misbehaving plugin. For a
> startup picker, prefer `scan_plugin_paths()` and load on demand.

## Crash-resistant discovery

`discover_plugins()` instantiates every plugin **in-process**, so one that `abort()`s or
makes a pure-virtual call during init takes the whole scan down with it.
`discover_plugins_safe()` introspects each plugin in a throwaway child process
(`vst3-host-probe`): a crashing plugin kills only its probe, and the scan completes anyway.
It returns a [`SafeDiscoveryReport`](https://docs.rs/vst3-host/latest/vst3_host/struct.SafeDiscoveryReport.html)
with the plugins that introspected cleanly (`plugins`) and a record of every one that was
skipped and why (`skipped`).

```rust
use vst3_host::{Vst3Host, SafeDiscoverySkip};

# fn main() -> vst3_host::Result<()> {
let host = Vst3Host::builder().scan_default_paths().build()?;
let report = host.discover_plugins_safe();

for detailed in &report.plugins {
    println!("{} by {}", detailed.info.name, detailed.factory.vendor);
}
for skip in &report.skipped {
    let reason = match skip {
        SafeDiscoverySkip::Crashed { detail, .. } => format!("crashed: {detail}"),
        SafeDiscoverySkip::TimedOut { .. } => "timed out".to_string(),
        SafeDiscoverySkip::Failed { detail, .. } => format!("failed: {detail}"),
    };
    eprintln!("skipped {}: {reason}", skip.path().display());
}
# Ok(())
# }
```

The trade-off is speed: one process spawn per plugin, so this is slower than the in-process
path. Use it to scan an untrusted folder; keep `discover_plugins()` when you trust the
plugins.

Each probe has a timeout (default
[`DEFAULT_PROBE_TIMEOUT`](https://docs.rs/vst3-host/latest/vst3_host/constant.DEFAULT_PROBE_TIMEOUT.html),
10s). Override it with `Vst3HostBuilder::probe_timeout`:

```rust
use std::time::Duration;
use vst3_host::Vst3Host;

# fn main() -> vst3_host::Result<()> {
let host = Vst3Host::builder()
    .scan_default_paths()
    .probe_timeout(Duration::from_secs(3))
    .build()?;
let report = host.discover_plugins_safe();
# let _ = report;
# Ok(())
# }
```

To probe a single known path out-of-process, use
[`discovery::probe_plugin_info_isolated`](https://docs.rs/vst3-host/latest/vst3_host/discovery/fn.probe_plugin_info_isolated.html):

```rust
use std::time::Duration;

# fn main() -> vst3_host::Result<()> {
let detailed = vst3_host::discovery::probe_plugin_info_isolated(
    std::path::Path::new("/path/plugin.vst3"),
    Duration::from_secs(10),
)?;
# let _ = detailed;
# Ok(())
# }
```

## Add custom scan locations

```rust
let host = Vst3Host::builder()
    .scan_default_paths()                 // standard system directories
    .add_scan_path("/my/plugins")         // plus your own
    .build()?;
```

## Report progress during a scan

For a long scan with a progress bar, use the callback variant:

```rust
use vst3_host::{Vst3Host, DiscoveryProgress};

# fn main() -> vst3_host::Result<()> {
# let mut host = Vst3Host::builder().scan_default_paths().build()?;
let plugins = host.discover_plugins_with_callback(|progress| match progress {
    DiscoveryProgress::Started { total_plugins } => println!("scanning {total_plugins}..."),
    DiscoveryProgress::Found { plugin, current, total } => {
        println!("[{current}/{total}] {}", plugin.name)
    }
    DiscoveryProgress::Error { path, error } => eprintln!("skip {path}: {error}"),
    DiscoveryProgress::Completed { total_found } => println!("found {total_found}"),
})?;
# let _ = plugins;
# Ok(())
# }
```

## Deep introspection

To read a plugin's full factory, class list, and bus layout (for an inspector-style UI),
use [`get_detailed_plugin_info`](https://docs.rs/vst3-host/latest/vst3_host/fn.get_detailed_plugin_info.html):

```rust
let detailed = vst3_host::get_detailed_plugin_info(std::path::Path::new("/path/plugin.vst3"))?;
println!("{} by {}", detailed.info.name, detailed.factory.vendor);
println!("{} classes, {} audio output buses", detailed.classes.len(), detailed.buses.audio_outputs.len());
```

## Where it looks

| Platform | Default scan directories |
| --- | --- |
| macOS | `/Library/Audio/Plug-Ins/VST3`, `~/Library/Audio/Plug-Ins/VST3` |
| Windows | `C:\Program Files\Common Files\VST3`, `C:\Program Files (x86)\Common Files\VST3` |
| Linux | `/usr/lib/vst3`, `/usr/local/lib/vst3`, `~/.vst3` |
