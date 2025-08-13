# VST3 Host Troubleshooting Guide

This guide helps you diagnose and solve common issues when building VST3 hosts with this library.

## Table of Contents

- [Quick Diagnostics](#quick-diagnostics)
- [Plugin Discovery Issues](#plugin-discovery-issues)
- [Plugin Loading Problems](#plugin-loading-problems)
- [Audio System Issues](#audio-system-issues)
- [Performance Problems](#performance-problems)
- [Platform-Specific Issues](#platform-specific-issues)
- [Development Issues](#development-issues)
- [Error Reference](#error-reference)

## Quick Diagnostics

### Test Your Setup
Run this minimal test to verify your environment:

```rust
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    
    println!("🔍 Testing VST3 host setup...");
    
    // Test 1: Create host
    let mut host = Vst3Host::new()?;
    println!("✅ Host created successfully");
    
    // Test 2: Plugin discovery
    let plugins = host.discover_plugins()?;
    println!("✅ Found {} plugins", plugins.len());
    
    if plugins.is_empty() {
        println!("⚠️  No plugins found - check plugin installation");
        return Ok(());
    }
    
    // Test 3: Load first plugin
    let plugin_info = &plugins[0];
    println!("🔄 Testing plugin: {}", plugin_info.name);
    
    match host.load_plugin(&plugin_info.path) {
        Ok(mut plugin) => {
            println!("✅ Plugin loaded successfully");
            
            // Test 4: Start processing
            match plugin.start_processing() {
                Ok(()) => println!("✅ Plugin processing started"),
                Err(e) => println!("❌ Failed to start processing: {}", e),
            }
        }
        Err(e) => println!("❌ Failed to load plugin: {}", e),
    }
    
    println!("🎉 Basic setup test completed");
    Ok(())
}
```

### Enable Debug Logging
Add comprehensive logging to see what's happening:

```rust
env_logger::Builder::from_default_env()
    .filter_level(log::LevelFilter::Debug)
    .format_timestamp(Some(env_logger::fmt::TimestampPrecision::Millis))
    .init();
```

## Plugin Discovery Issues

### Problem: No Plugins Found

**Symptoms:**
- `discover_plugins()` returns empty vector
- "No VST3 plugins found" message

**Solutions:**

1. **Check Plugin Installation Paths:**
   ```rust
   // Verify standard paths exist
   let paths = [
       "/Library/Audio/Plug-Ins/VST3",           // macOS system
       "~/Library/Audio/Plug-Ins/VST3",          // macOS user
       "C:\\Program Files\\Common Files\\VST3",   // Windows system
       "C:\\Program Files (x86)\\Common Files\\VST3", // Windows x86
       "/usr/lib/vst3",                          // Linux system
       "~/.vst3",                                // Linux user
   ];
   
   for path in &paths {
       let expanded = shellexpand::tilde(path);
       let path = std::path::Path::new(expanded.as_ref());
       println!("{}: {}", path.display(), path.exists());
   }
   ```

2. **Manual Plugin Check:**
   ```bash
   # macOS
   ls -la "/Library/Audio/Plug-Ins/VST3"
   ls -la "~/Library/Audio/Plug-Ins/VST3"
   
   # Windows
   dir "C:\Program Files\Common Files\VST3"
   
   # Linux
   ls -la /usr/lib/vst3
   ls -la ~/.vst3
   ```

3. **Add Custom Scan Paths:**
   ```rust
   let mut host = Vst3Host::builder()
       .scan_default_paths()
       .add_scan_path("/custom/vst3/path")
       .build()?;
   ```

4. **Install Test Plugins:**
   - Download free VST3 plugins (e.g., Surge XT, Cardinal)
   - Verify they're in correct directories
   - Test with simple plugins first

### Problem: Some Plugins Not Discovered

**Symptoms:**
- Some plugins missing from discovery results
- Plugins work in other hosts

**Solutions:**

1. **Check Plugin Blacklist:**
   ```rust
   // The library may blacklist problematic plugins
   // Check discovery logs for "Skipping blacklisted plugin" messages
   ```

2. **Verify Plugin Architecture:**
   ```rust
   // Ensure plugin matches your host architecture (64-bit vs 32-bit)
   println!("Host architecture: {}", std::env::consts::ARCH);
   ```

3. **Test Individual Plugin:**
   ```rust
   // Try loading specific plugin directly
   let result = host.load_plugin("/path/to/specific/plugin.vst3");
   match result {
       Ok(_) => println!("Plugin loads fine"),
       Err(e) => println!("Plugin error: {}", e),
   }
   ```

## Plugin Loading Problems

### Problem: Plugin Load Failed

**Symptoms:**
- `PluginLoadFailed` error
- Plugin loads in other hosts but not yours

**Common Causes & Solutions:**

1. **Missing Dependencies:**
   ```bash
   # macOS: Check for missing frameworks
   otool -L /path/to/plugin.vst3/Contents/MacOS/plugin
   
   # Linux: Check for missing libraries
   ldd /path/to/plugin.so
   
   # Windows: Use Dependency Walker
   depends.exe plugin.vst3
   ```

2. **Permission Issues:**
   ```bash
   # macOS: Check Gatekeeper
   spctl --assess --type exec /path/to/plugin.vst3
   
   # Fix with:
   xattr -rd com.apple.quarantine /path/to/plugin.vst3
   ```

3. **Process Isolation for Problematic Plugins:**
   ```rust
   let mut host = Vst3Host::builder()
       .with_process_isolation(true)  // Enable for all plugins
       .build()?;
   
   // Or per-plugin:
   let is_problematic = plugin_info.vendor.contains("ProblematicVendor");
   if is_problematic {
       // Load with isolation
   }
   ```

### Problem: Plugin Loads but Crashes

**Symptoms:**
- Plugin loads successfully
- Crashes during `start_processing()` or `process_audio()`

**Solutions:**

1. **Enable Process Isolation:**
   ```rust
   let mut host = Vst3Host::builder()
       .with_process_isolation(true)
       .build()?;
   ```

2. **Crash Protection:**
   ```rust
   use std::panic::catch_unwind;
   
   let result = catch_unwind(|| {
       plugin.start_processing()
   });
   
   match result {
       Ok(Ok(())) => println!("Plugin started successfully"),
       Ok(Err(e)) => println!("Plugin error: {}", e),
       Err(_) => println!("Plugin panicked!"),
   }
   ```

3. **Test with Minimal Configuration:**
   ```rust
   // Try with smallest possible buffer
   let mut host = Vst3Host::builder()
       .sample_rate(44100.0)
       .block_size(64)  // Minimal buffer
       .build()?;
   ```

### Problem: Parameters Not Working

**Symptoms:**
- `get_parameters()` returns empty vector
- `set_parameter()` has no effect

**Solutions:**

1. **Check Parameter Support:**
   ```rust
   let plugin_info = plugin.info();
   println!("Plugin supports {} parameters", plugin_info.parameter_count);
   
   // Some plugins need to be initialized first
   plugin.start_processing()?;
   let params = plugin.get_parameters()?;
   ```

2. **Parameter Range Validation:**
   ```rust
   for param in params {
       println!("Parameter: {} (ID: {}, Range: {} - {})", 
                param.name, param.id, param.min_value, param.max_value);
       
       // Ensure value is in range
       let normalized_value = param.value.clamp(0.0, 1.0);
       plugin.set_parameter(param.id, normalized_value)?;
   }
   ```

3. **Controller vs Processor Parameters:**
   ```rust
   // Some plugins have separate controller components
   // Parameters might only be available after controller initialization
   ```

## Audio System Issues

### Problem: No Audio Output

**Symptoms:**
- Audio stream starts successfully
- No sound output
- Level meters show silence

**Solutions:**

1. **Check Audio Device:**
   ```rust
   let backend = CpalBackend::new()?;
   let devices = backend.enumerate_output_devices()?;
   
   for (i, device) in devices.iter().enumerate() {
       println!("Device {}: {:?}", i, device.name());
   }
   
   // Try specific device
   let device = &devices[device_index];
   ```

2. **Verify Audio Configuration:**
   ```rust
   let config = AudioConfig {
       sample_rate: 44100.0,
       block_size: 512,
       input_channels: 0,
       output_channels: 2,  // Ensure matches your setup
   };
   
   println!("Config: {:?}", config);
   ```

3. **Test Audio Pipeline:**
   ```rust
   // Generate test tone
   fn generate_test_tone(output: &mut [f32], sample_rate: f64) {
       for (i, sample) in output.iter_mut().enumerate() {
           let t = i as f64 / sample_rate;
           *sample = (t * 440.0 * 2.0 * std::f64::consts::PI).sin() as f32 * 0.1;
       }
   }
   
   // Use in audio callback to verify audio path
   ```

4. **Check Plugin Audio Routing:**
   ```rust
   let info = plugin.info();
   println!("Plugin: {} inputs, {} outputs", info.audio_inputs, info.audio_outputs);
   
   // Ensure buffers match plugin expectations
   let mut buffers = AudioBuffers::new(
       info.audio_inputs.max(2),    // At least stereo input
       info.audio_outputs.max(2),   // At least stereo output
       block_size,
       sample_rate,
   );
   ```

### Problem: Audio Dropouts/Glitches

**Symptoms:**
- Intermittent audio glitches
- Crackling or popping sounds
- "Audio underrun" messages

**Solutions:**

1. **Increase Buffer Size:**
   ```rust
   // Try larger buffer sizes
   let buffer_sizes = [256, 512, 1024, 2048];
   for &size in &buffer_sizes {
       let config = AudioConfig { block_size: size, ..config };
       // Test with this configuration
   }
   ```

2. **Optimize Audio Callback:**
   ```rust
   // Avoid blocking operations in audio thread
   fn audio_callback(output: &mut [f32]) {
       // Good: Non-blocking operations
       if let Ok(mut plugin) = plugin.try_lock() {
           let _ = plugin.process_audio(&mut buffers);
       }
       
       // Bad: Blocking operations
       // let mut plugin = plugin.lock(); // Can block!
   }
   ```

3. **Monitor CPU Usage:**
   ```rust
   use std::time::Instant;
   
   let start = Instant::now();
   plugin.process_audio(&mut buffers)?;
   let processing_time = start.elapsed();
   
   let buffer_time = Duration::from_secs_f64(block_size as f64 / sample_rate);
   let cpu_usage = processing_time.as_secs_f64() / buffer_time.as_secs_f64() * 100.0;
   
   if cpu_usage > 80.0 {
       println!("Warning: High CPU usage: {:.1}%", cpu_usage);
   }
   ```

### Problem: High Latency

**Symptoms:**
- Noticeable delay between input and output
- Poor real-time performance

**Solutions:**

1. **Reduce Buffer Size:**
   ```rust
   // Try smaller buffers for lower latency
   let config = AudioConfig {
       block_size: 64,  // Very low latency
       ..config
   };
   
   // Calculate latency
   let latency_ms = (config.block_size as f64 / config.sample_rate) * 1000.0;
   println!("Latency: {:.1}ms", latency_ms);
   ```

2. **Use Real-time Audio Backend:**
   ```bash
   # Linux: Use JACK for professional audio
   sudo apt install jackd2
   
   # macOS: Check Core Audio configuration
   # Windows: Use ASIO drivers
   ```

## Performance Problems

### Problem: High CPU Usage

**Symptoms:**
- CPU usage consistently above 80%
- System becomes unresponsive
- Audio dropouts under load

**Solutions:**

1. **Profile Plugin Performance:**
   ```rust
   use std::time::Instant;
   
   struct PerformanceMonitor {
       plugin_times: HashMap<String, Duration>,
       sample_count: usize,
   }
   
   impl PerformanceMonitor {
       fn measure_plugin(&mut self, name: &str, f: impl FnOnce()) {
           let start = Instant::now();
           f();
           let elapsed = start.elapsed();
           
           *self.plugin_times.entry(name.to_string()).or_default() += elapsed;
           self.sample_count += 1;
           
           if self.sample_count % 1000 == 0 {
               self.print_stats();
           }
       }
       
       fn print_stats(&self) {
           for (name, time) in &self.plugin_times {
               let avg_time = time.as_secs_f64() / self.sample_count as f64 * 1000.0;
               println!("{}: {:.2}ms avg", name, avg_time);
           }
       }
   }
   ```

2. **Optimize Buffer Sizes:**
   ```rust
   // Find optimal buffer size for your system
   fn find_optimal_buffer_size() -> usize {
       let sizes = [64, 128, 256, 512, 1024];
       let mut best_size = 512;
       let mut lowest_cpu = 100.0;
       
       for &size in &sizes {
           let cpu_usage = benchmark_buffer_size(size);
           if cpu_usage < lowest_cpu {
               lowest_cpu = cpu_usage;
               best_size = size;
           }
       }
       
       best_size
   }
   ```

3. **Use Process Isolation for Heavy Plugins:**
   ```rust
   // Identify CPU-heavy plugins
   if plugin_info.vendor.contains("HeavyVendor") || 
      plugin_info.name.contains("HeavySynth") {
       let host = Vst3Host::builder()
           .with_process_isolation(true)
           .build()?;
   }
   ```

### Problem: Memory Leaks

**Symptoms:**
- Memory usage grows over time
- Application eventually crashes with out-of-memory

**Solutions:**

1. **Monitor Memory Usage:**
   ```rust
   #[cfg(target_os = "linux")]
   fn get_memory_usage() -> usize {
       let status = std::fs::read_to_string("/proc/self/status").unwrap();
       for line in status.lines() {
           if line.starts_with("VmRSS:") {
               let kb: usize = line.split_whitespace().nth(1).unwrap().parse().unwrap();
               return kb * 1024;
           }
       }
       0
   }
   ```

2. **Proper Plugin Cleanup:**
   ```rust
   impl Drop for PluginHost {
       fn drop(&mut self) {
           // Stop audio processing first
           self.stop_audio();
           
           // Stop all plugins
           for plugin in &mut self.plugins {
               let _ = plugin.stop_processing();
           }
           
           // Clear plugin list
           self.plugins.clear();
       }
   }
   ```

3. **Avoid Memory Allocations in Audio Thread:**
   ```rust
   // Good: Pre-allocated buffers
   struct AudioProcessor {
       buffers: Vec<AudioBuffers>,
       current_buffer: usize,
   }
   
   // Bad: Allocating in audio callback
   fn audio_callback(output: &mut [f32]) {
       let mut buffers = AudioBuffers::new(2, 2, 512, 44100.0); // Allocation!
   }
   ```

## Platform-Specific Issues

### macOS Issues

#### Problem: Plugin Permission Denied
```bash
# Check quarantine status
xattr -l /path/to/plugin.vst3

# Remove quarantine
sudo xattr -rd com.apple.quarantine /path/to/plugin.vst3

# Check code signing
codesign -dv /path/to/plugin.vst3
```

#### Problem: Objective-C Runtime Conflicts
```rust
// Enable automatic conflict resolution
let host = Vst3Host::builder()
    .with_objc_conflict_resolution(true)
    .build()?;
```

#### Problem: Sandboxing Issues
```xml
<!-- Add to Info.plist for sandbox exceptions -->
<key>com.apple.security.temporary-exception.audio-unit-host</key>
<true/>
```

### Windows Issues

#### Problem: MSVC Runtime Missing
```bash
# Install Visual C++ Redistributable
# Download from Microsoft or use vcredist installer
```

#### Problem: Path Length Limitations
```rust
// Use UNC paths for long paths
let long_path = r"\\?\C:\Very\Long\Path\To\Plugin.vst3";
```

#### Problem: ASIO Driver Issues
```rust
// Fallback to DirectSound if ASIO fails
let backend = CpalBackend::new().or_else(|_| {
    println!("ASIO failed, trying DirectSound...");
    CpalBackend::new_with_host(cpal::HostId::DirectSound)
})?;
```

### Linux Issues

#### Problem: Missing Audio System
```bash
# Install ALSA development packages
sudo apt install libasound2-dev

# Or PulseAudio
sudo apt install libpulse-dev

# Or JACK
sudo apt install libjack-dev
```

#### Problem: Plugin Dependencies
```bash
# Check missing libraries
ldd /path/to/plugin.so

# Install common dependencies
sudo apt install libstdc++6 libgcc1
```

## Development Issues

### Problem: Compilation Errors

#### "VST3 SDK not found"
```bash
# Ensure VST3 SDK is properly configured
export VST3_SDK_DIR=/path/to/vst3sdk

# Or in .cargo/config.toml
[env]
VST3_SDK_DIR = "/path/to/vst3sdk"
```

#### Link errors
```toml
# Add to Cargo.toml
[target.'cfg(target_os = "macos")'.dependencies]
cocoa = "0.26"
core-foundation = "0.10"

[target.'cfg(target_os = "windows")'.dependencies]
winapi = { version = "0.3", features = ["winuser", "windef"] }
```

### Problem: Runtime Panics

#### "Plugin interface not found"
```rust
// Check plugin compatibility
let info = get_plugin_info(&plugin_path)?;
if info.version < "3.0" {
    return Err("Plugin version not supported".into());
}
```

#### "Thread panicked in audio callback"
```rust
// Wrap audio processing in panic protection
fn safe_audio_process(plugin: &mut Plugin, buffers: &mut AudioBuffers) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        plugin.process_audio(buffers)
    }));
    
    match result {
        Ok(Ok(())) => {}, // Success
        Ok(Err(e)) => eprintln!("Plugin error: {}", e),
        Err(_) => eprintln!("Plugin panicked!"),
    }
}
```

## Error Reference

### Common Error Types

| Error | Meaning | Solution |
|-------|---------|----------|
| `PluginNotFound` | Plugin file doesn't exist | Check file path, verify plugin installation |
| `PluginLoadFailed` | Plugin library couldn't be loaded | Check dependencies, permissions, architecture |
| `AudioBackendError` | Audio system initialization failed | Check audio drivers, device availability |
| `ParameterError` | Invalid parameter operation | Verify parameter ID, value range |
| `MidiError` | MIDI operation failed | Check MIDI configuration, plugin MIDI support |
| `ProcessingError` | Audio processing failed | Check buffer sizes, plugin state |

### Error Severity Levels

#### Critical (Application Should Stop)
- `AudioBackendError` - No audio system available
- Host initialization failures

#### Warning (Continue with Degraded Function)
- `PluginLoadFailed` - Skip problematic plugin
- `ParameterError` - Use default values

#### Info (Log and Continue)
- `MidiError` - MIDI not essential for all plugins
- Individual parameter set failures

### Error Recovery Strategies

```rust
fn handle_plugin_error(error: &vst3_host::Error) -> ErrorAction {
    match error {
        vst3_host::Error::PluginLoadFailed { .. } => {
            ErrorAction::SkipPlugin
        }
        vst3_host::Error::AudioBackendError(_) => {
            ErrorAction::FallbackAudio
        }
        vst3_host::Error::ProcessingError(_) => {
            ErrorAction::BypassPlugin
        }
        _ => ErrorAction::LogAndContinue,
    }
}

enum ErrorAction {
    Stop,
    SkipPlugin,
    FallbackAudio,
    BypassPlugin,
    LogAndContinue,
}
```

## Getting Help

### Before Asking for Help

1. **Enable debug logging** and check the output
2. **Test with simple plugins** (e.g., basic synthesizers)
3. **Verify your environment** with the quick diagnostic test
4. **Check known issues** in the repository
5. **Try process isolation** for problematic plugins

### Information to Include

When reporting issues, include:

- **Operating system** and version
- **Rust version** (`rustc --version`)
- **Plugin name and vendor** that's causing issues
- **Complete error messages** with debug logging
- **Minimal reproduction code**
- **What you've already tried**

### Useful Commands for Debugging

```bash
# Rust environment info
rustc --version
cargo --version

# System info
uname -a                    # Linux/macOS
systeminfo                 # Windows

# Audio system info
aplay -l                    # Linux ALSA
pactl list sinks           # Linux PulseAudio
system_profiler SPAudioDataType  # macOS
```

Remember: Most VST3 hosting issues are related to plugin compatibility, audio system configuration, or missing dependencies. The process isolation feature can solve many plugin-related problems!