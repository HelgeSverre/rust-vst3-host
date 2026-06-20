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
