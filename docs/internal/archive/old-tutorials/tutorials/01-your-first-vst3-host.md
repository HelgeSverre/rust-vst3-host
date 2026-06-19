# Tutorial 1: Your First VST3 Host

**Duration: 5 minutes**  
**Prerequisites: Basic Rust knowledge**

Welcome to the world of VST3 plugin hosting! In this tutorial, you'll learn the absolute basics: how to load a VST3 plugin and get information about it. We'll start simple with no audio processing - just demonstrating that you can successfully communicate with a VST3 plugin.

## What You'll Learn

By the end of this tutorial, you'll be able to:
- ✅ Create a VST3 host instance
- ✅ Load a VST3 plugin from a file path
- ✅ Extract basic plugin information (name, vendor, capabilities)
- ✅ Handle errors gracefully
- ✅ Understand what VST3 hosting means

## What is VST3 Hosting?

VST3 (Virtual Studio Technology 3) is a plugin standard that allows audio software to load and use external audio processors and instruments. As a **host**, your application provides the environment where plugins run - you're essentially creating a mini digital audio workstation (DAW).

Think of it like this:
- **Plugin** = A musical instrument or effect pedal
- **Host** = The stage/venue where the musician performs
- **Your Code** = The sound engineer controlling everything

## Setting Up

First, add `vst3-host` to your `Cargo.toml`:

```toml
[dependencies]
vst3-host = "0.1.0"
env_logger = "0.11"  # For helpful debug output
```

## Your First Host: The Complete Example

Let's start with a complete, working example and then break it down:

```rust
// src/main.rs
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable logging to see what's happening under the hood
    env_logger::init();
    
    println!("=== Your First VST3 Host ===\n");
    
    // Step 1: Create a host
    let mut host = Vst3Host::new()?;
    println!("✅ VST3 host created successfully!");
    
    // Step 2: For this tutorial, we'll use plugin discovery to find a plugin
    // In real applications, you might hard-code a path or use a file picker
    println!("🔍 Discovering plugins...");
    let plugins = host.discover_plugins()?;
    
    if plugins.is_empty() {
        println!("❌ No VST3 plugins found on your system!");
        println!("💡 Make sure you have VST3 plugins installed in:");
        println!("   - macOS: /Library/Audio/Plug-Ins/VST3 or ~/Library/Audio/Plug-Ins/VST3");
        println!("   - Windows: C:\\Program Files\\Common Files\\VST3");
        println!("   - Linux: /usr/lib/vst3 or ~/.vst3");
        return Ok(());
    }
    
    println!("Found {} plugin(s)! Using the first one...\n", plugins.len());
    
    // Step 3: Load the first plugin we found
    let plugin_info = &plugins[0];
    println!("Loading: {} by {}", plugin_info.name, plugin_info.vendor);
    
    let mut plugin = host.load_plugin(&plugin_info.path)?;
    println!("✅ Plugin loaded successfully!\n");
    
    // Step 4: Explore the plugin's capabilities
    show_plugin_info(&plugin);
    
    // Step 5: Start and stop processing (no audio yet, just testing the lifecycle)
    println!("🔄 Testing plugin lifecycle...");
    plugin.start_processing()?;
    println!("✅ Plugin processing started");
    
    plugin.stop_processing()?;
    println!("✅ Plugin processing stopped");
    
    println!("\n🎉 Success! You've successfully hosted your first VST3 plugin!");
    
    Ok(())
}

fn show_plugin_info(plugin: &Plugin) {
    let info = plugin.info();
    
    println!("=== Plugin Information ===");
    println!("Name: {}", info.name);
    println!("Vendor: {}", info.vendor);
    println!("Version: {}", info.version);
    println!("Category: {}", info.category);
    
    // Audio capabilities
    println!("\n=== Audio Capabilities ===");
    println!("Audio Inputs: {} channels", info.audio_inputs);
    println!("Audio Outputs: {} channels", info.audio_outputs);
    
    // MIDI capabilities  
    println!("\n=== MIDI Capabilities ===");
    println!("Has MIDI Input: {}", if info.has_midi_input { "Yes" } else { "No" });
    println!("Has MIDI Output: {}", if info.has_midi_output { "Yes" } else { "No" });
    
    // GUI capability
    println!("\n=== User Interface ===");
    println!("Has GUI: {}", if info.has_gui { "Yes" } else { "No" });
    
    // Let's also try to get parameter information
    println!("\n=== Parameters ===");
    match plugin.get_parameters() {
        Ok(params) => {
            println!("Total parameters: {}", params.len());
            
            // Show first few parameters as examples
            for (i, param) in params.iter().take(3).enumerate() {
                println!("  {}: {} = {:.3}", i + 1, param.name, param.value);
            }
            
            if params.len() > 3 {
                println!("  ... and {} more parameters", params.len() - 3);
            }
        }
        Err(e) => {
            println!("Could not read parameters: {}", e);
        }
    }
    
    println!();
}
```

## Running Your First Host

1. **Create a new Rust project:**
   ```bash
   cargo new my-vst3-host
   cd my-vst3-host
   ```

2. **Add the dependencies** to `Cargo.toml` as shown above

3. **Copy the code** into `src/main.rs`

4. **Run it:**
   ```bash
   cargo run
   ```

Expected output (will vary based on your installed plugins):
```
=== Your First VST3 Host ===

✅ VST3 host created successfully!
🔍 Discovering plugins...
Found 12 plugin(s)! Using the first one...

Loading: Piano One by Sound Magic
✅ Plugin loaded successfully!

=== Plugin Information ===
Name: Piano One
Vendor: Sound Magic
Version: 1.0.0
Category: Instrument

=== Audio Capabilities ===
Audio Inputs: 0 channels
Audio Outputs: 2 channels

=== MIDI Capabilities ===
Has MIDI Input: Yes
Has MIDI Output: No

=== User Interface ===
Has GUI: Yes

=== Parameters ===
Total parameters: 15
  1: Volume = 0.800
  2: Reverb = 0.200
  3: Brightness = 0.500
  ... and 12 more parameters

🔄 Testing plugin lifecycle...
✅ Plugin processing started
✅ Plugin processing stopped

🎉 Success! You've successfully hosted your first VST3 plugin!
```

## Understanding the Code

### Step 1: Creating the Host
```rust
let mut host = Vst3Host::new()?;
```
This creates a VST3 host with default settings. The host is your "manager" that handles loading plugins and providing them with audio configuration.

### Step 2: Plugin Discovery
```rust
let plugins = host.discover_plugins()?;
```
This scans your system's standard VST3 directories and returns a list of available plugins. Each plugin has metadata like name, vendor, and capabilities.

### Step 3: Loading a Plugin
```rust
let mut plugin = host.load_plugin(&plugin_info.path)?;
```
This loads the actual plugin library into memory and initializes it. The plugin is now ready to be configured and used.

### Step 4: Plugin Lifecycle
```rust
plugin.start_processing()?;
// ... plugin is now ready to process audio
plugin.stop_processing()?;
```
VST3 plugins have a specific lifecycle. You must start processing before sending audio or MIDI data, and stop it when done.

## Common Patterns You'll See

### Error Handling
VST3 hosting involves lots of dynamic loading and platform-specific behavior, so robust error handling is crucial:

```rust
match host.load_plugin(path) {
    Ok(plugin) => {
        println!("Plugin loaded: {}", plugin.info().name);
    }
    Err(Error::PluginLoadFailed { path, reason }) => {
        eprintln!("Failed to load plugin at {}: {}", path.display(), reason);
    }
    Err(e) => {
        eprintln!("Unexpected error: {}", e);
    }
}
```

### Plugin Types
Different plugins have different capabilities:

```rust
let info = plugin.info();

if info.audio_inputs == 0 && info.audio_outputs > 0 && info.has_midi_input {
    println!("This is likely a virtual instrument");
} else if info.audio_inputs > 0 && info.audio_outputs > 0 {
    println!("This is likely an audio effect");
} else {
    println!("This might be a utility or special-purpose plugin");
}
```

## Troubleshooting

### "No plugins found"
- **macOS**: Check `/Library/Audio/Plug-Ins/VST3` and `~/Library/Audio/Plug-Ins/VST3`
- **Windows**: Check `C:\Program Files\Common Files\VST3`
- **Linux**: Check `/usr/lib/vst3`, `/usr/local/lib/vst3`, and `~/.vst3`

### "Plugin failed to load"
- The plugin might be corrupted or incompatible
- On macOS, you might need to allow unsigned plugins
- Try with a different plugin to isolate the issue

### Logging
Enable detailed logging to see what's happening:
```rust
env_logger::Builder::from_default_env()
    .filter_level(log::LevelFilter::Debug)
    .init();
```

## What's Next?

Congratulations! You can now load and inspect VST3 plugins. In the next tutorial, we'll make your host actually process audio through the plugin.

**Coming up in Tutorial 2**: Setting up audio input/output and processing audio through your loaded plugin.

## Key Concepts Learned

- **VST3 Host**: The application that provides the environment for plugins
- **Plugin Discovery**: Finding available plugins on the system
- **Plugin Loading**: Loading a plugin library and initializing it
- **Plugin Lifecycle**: Starting and stopping processing
- **Plugin Capabilities**: Understanding what a plugin can do (audio I/O, MIDI, GUI)

The foundation is set! You're now ready to start building more sophisticated VST3 host applications.