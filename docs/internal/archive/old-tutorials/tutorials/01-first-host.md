# Tutorial 1: Your First VST3 Host (5 minutes)

Welcome to VST3 plugin hosting in Rust! This tutorial will get you up and running with your first VST3 plugin host in just 5 minutes.

## What You'll Learn

- How to load a VST3 plugin
- Basic plugin information retrieval
- Simple error handling
- Clean plugin lifecycle management

## Prerequisites

- Rust 1.70 or later installed
- At least one VST3 plugin on your system
- 5 minutes of your time!

## Step 1: Create a New Project

```bash
cargo new my-vst-host
cd my-vst-host
```

Add the vst3-host dependency to your `Cargo.toml`:

```toml
[dependencies]
vst3-host = { version = "0.1", features = ["cpal-backend"] }
```

## Step 2: Your First Plugin Host

Replace the contents of `src/main.rs` with:

```rust
use vst3_host::simple;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎵 VST3 Host - Tutorial 1");
    println!("========================");
    
    // Try to find plugins on your system
    match simple::discover_plugins() {
        Ok(plugins) => {
            println!("Found {} VST3 plugins:", plugins.len());
            for plugin in &plugins {
                println!("  - {} by {}", plugin.name, plugin.vendor);
            }
            
            // Load the first plugin
            if let Some(first_plugin) = plugins.first() {
                println!("\n🔌 Loading: {}", first_plugin.name);
                
                match simple::load_plugin(&first_plugin.path) {
                    Ok(plugin) => {
                        let info = plugin.info();
                        println!("✅ Successfully loaded!");
                        println!("   Name: {}", info.name);
                        println!("   Vendor: {}", info.vendor);
                        println!("   Version: {}", info.version);
                        println!("   Audio I/O: {} → {} channels", info.audio_inputs, info.audio_outputs);
                        println!("   Has GUI: {}", if info.has_gui { "Yes" } else { "No" });
                        println!("   Accepts MIDI: {}", if info.has_midi_input { "Yes" } else { "No" });
                    }
                    Err(e) => {
                        println!("❌ Failed to load plugin: {}", e);
                        println!("💡 Try with a different plugin or check the plugin path");
                    }
                }
            } else {
                println!("\n❌ No plugins found on your system");
                println!("💡 Install some VST3 plugins to continue");
            }
        }
        Err(e) => {
            println!("❌ Plugin discovery failed: {}", e);
            println!("💡 This might be normal if no plugins are installed");
        }
    }
    
    println!("\n🎉 Tutorial completed!");
    Ok(())
}
```

## Step 3: Run Your Host

```bash
cargo run
```

You should see output like:

```
🎵 VST3 Host - Tutorial 1
========================
Found 3 VST3 plugins:
  - Dexed by Digital Suburban
  - Surge XT by Surge Synth Team  
  - OB-Xd by discoDSP

🔌 Loading: Dexed
✅ Successfully loaded!
   Name: Dexed
   Vendor: Digital Suburban
   Version: 1.0.0
   Audio I/O: 0 → 2 channels
   Has GUI: Yes
   Accepts MIDI: Yes

🎉 Tutorial completed!
```

## What Just Happened?

1. **Plugin Discovery**: The `simple::discover_plugins()` function scanned standard system directories for VST3 plugins
2. **Plugin Loading**: We loaded the first found plugin using `simple::load_plugin()`
3. **Information Extraction**: We accessed plugin metadata through `plugin.info()`
4. **Error Handling**: We handled both plugin discovery and loading errors gracefully

## Key Concepts

### Plugin Discovery
```rust
// Automatic discovery in system directories
let plugins = simple::discover_plugins()?;

// Or discover in a specific directory
let plugins = simple::discover_plugins_in("/my/plugins")?;
```

### Safe Plugin Loading
```rust
// The simple API handles all complexity internally
let plugin = simple::load_plugin("/path/to/plugin.vst3")?;

// Plugin is automatically cleaned up when dropped
// No manual memory management required!
```

### Plugin Information
```rust
let info = plugin.info();
println!("Plugin: {} by {}", info.name, info.vendor);
println!("Channels: {} → {}", info.audio_inputs, info.audio_outputs);
println!("MIDI: {}", info.has_midi_input);
println!("GUI: {}", info.has_gui);
```

## Common Issues & Solutions

### "No plugins found"
- **Cause**: No VST3 plugins installed on your system
- **Solution**: Install some free VST3 plugins like Dexed, Surge XT, or OB-Xd

### "Plugin discovery failed"
- **Cause**: Permissions or system configuration issues
- **Solution**: Try running as administrator or check VST3 directory permissions

### "Failed to load plugin"
- **Cause**: Plugin incompatibility or corruption
- **Solution**: Try different plugins or enable process isolation (covered in Tutorial 4)

## Next Steps

🎓 **Tutorial 2: Processing Audio** - Learn to actually process audio through your loaded plugin

🎹 **Tutorial 3: Simple Plugin Host** - Build a basic host with parameter control and MIDI

🔧 **Tutorial 4: Advanced Features** - Error handling, process isolation, and plugin compatibility

## Complete Example Code

You can find the complete working example at `examples/tutorial_01_first_host.rs`:

```bash
cargo run --example tutorial_01_first_host
```

---

**Estimated time**: 5 minutes  
**Difficulty**: Beginner  
**Key skills learned**: Plugin discovery, loading, information retrieval, error handling  
**Next tutorial**: [02-processing-audio.md](02-processing-audio.md)