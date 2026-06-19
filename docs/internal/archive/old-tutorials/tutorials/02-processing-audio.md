# Tutorial 2: Processing Audio

**Duration: 10 minutes**  
**Prerequisites: Tutorial 1 completed**

Now that you can load a VST3 plugin, let's make it actually process audio! In this tutorial, you'll set up audio input/output, process audio through your plugin, and handle real-time parameter changes.

## What You'll Learn

By the end of this tutorial, you'll be able to:
- ✅ Set up audio input and output with CPAL
- ✅ Process audio buffers through a VST3 plugin
- ✅ Generate test audio signals (sine waves, noise)
- ✅ Handle real-time parameter changes
- ✅ Monitor audio levels and prevent clipping
- ✅ Understand audio buffer management

## Audio Processing Concepts

Before we dive into code, let's understand the key concepts:

### Audio Buffers
Audio is processed in chunks called **buffers** or **blocks**. Think of it like reading a book one page at a time instead of one word at a time - it's more efficient.

- **Buffer Size**: How many audio samples to process at once (e.g., 512 samples)
- **Sample Rate**: How many samples per second (e.g., 44,100 Hz = CD quality)
- **Channels**: Mono (1), Stereo (2), or more for surround sound

### Real-time Constraints
Audio processing happens in real-time, which means:
- Your processing must complete before the next buffer arrives
- Avoid memory allocations in the audio thread
- Keep processing predictable and fast

## Setting Up

Add the CPAL backend feature to your `Cargo.toml`:

```toml
[dependencies]
vst3-host = { version = "0.1.0", features = ["cpal-backend"] }
env_logger = "0.11"
```

## Complete Audio Processing Example

Here's a complete example that processes audio through a VST3 plugin:

```rust
// src/main.rs
use vst3_host::prelude::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    
    println!("=== VST3 Audio Processing ===\n");
    
    // Step 1: Create host and load plugin
    let mut host = Vst3Host::builder()
        .sample_rate(44100.0)
        .block_size(512)
        .build()?;
    
    // Discover and load a plugin
    let plugins = host.discover_plugins()?;
    if plugins.is_empty() {
        println!("❌ No plugins found! Please install some VST3 plugins.");
        return Ok(());
    }
    
    // Find an instrument or effect to use
    let plugin_info = find_suitable_plugin(&plugins);
    println!("Loading: {} by {}", plugin_info.name, plugin_info.vendor);
    
    let mut plugin = host.load_plugin(&plugin_info.path)?;
    
    // Step 2: Set up audio configuration
    let config = AudioConfig {
        sample_rate: 44100.0,
        block_size: 512,
        input_channels: plugin.info().audio_inputs.max(2), // At least stereo input
        output_channels: plugin.info().audio_outputs.max(2), // At least stereo output
    };
    
    println!("Audio config: {}Hz, {} samples, {}→{} channels", 
             config.sample_rate, config.block_size, 
             config.input_channels, config.output_channels);
    
    // Step 3: Create audio backend
    let backend = CpalBackend::new()?;
    let output_device = backend.default_output_device()
        .ok_or("No output device available")?;
    
    // Step 4: Create shared state for communication between threads
    let plugin = Arc::new(Mutex::new(plugin));
    let levels = Arc::new(Mutex::new(AudioLevels::new(config.output_channels)));
    let test_signal = Arc::new(Mutex::new(TestSignalGenerator::new(config.sample_rate)));
    
    // Clone for audio thread
    let plugin_clone = plugin.clone();
    let levels_clone = levels.clone();
    let test_signal_clone = test_signal.clone();
    
    // Step 5: Create audio stream
    let stream = backend.create_output_stream(
        &output_device,
        config,
        Box::new(move |output_buffer: &mut [f32]| {
            process_audio_callback(
                output_buffer,
                &plugin_clone,
                &levels_clone,
                &test_signal_clone,
                config.output_channels,
                config.block_size,
            );
        }),
        Box::new(|error| {
            eprintln!("Audio error: {}", error);
        }),
    )?;
    
    // Step 6: Start audio processing
    {
        let mut plugin_lock = plugin.lock().unwrap();
        plugin_lock.start_processing()?;
    }
    
    stream.play()?;
    println!("🎵 Audio processing started!");
    println!("🎵 Playing test signal through plugin...");
    
    // Step 7: Run for a while with parameter automation
    let start_time = Instant::now();
    let duration = Duration::from_secs(10);
    
    while start_time.elapsed() < duration {
        // Update levels and show monitoring
        {
            let levels_lock = levels.lock().unwrap();
            show_audio_levels(&levels_lock);
        }
        
        // Automate parameters
        automate_parameters(&plugin, start_time.elapsed());
        
        std::thread::sleep(Duration::from_millis(100));
    }
    
    // Step 8: Clean shutdown
    stream.pause()?;
    {
        let mut plugin_lock = plugin.lock().unwrap();
        plugin_lock.stop_processing()?;
    }
    
    println!("\n🎉 Audio processing completed successfully!");
    
    Ok(())
}

fn process_audio_callback(
    output_buffer: &mut [f32],
    plugin: &Arc<Mutex<Plugin>>,
    levels: &Arc<Mutex<AudioLevels>>,
    test_signal: &Arc<Mutex<TestSignalGenerator>>,
    output_channels: usize,
    block_size: usize,
) {
    // Clear output buffer
    output_buffer.fill(0.0);
    
    // Generate test input signal
    let mut input_buffers = vec![vec![0.0f32; block_size]; 2];
    {
        let mut signal_gen = test_signal.lock().unwrap();
        signal_gen.generate_stereo_signal(&mut input_buffers[0], &mut input_buffers[1]);
    }
    
    // Create output buffers for plugin processing
    let mut output_buffers = vec![vec![0.0f32; block_size]; output_channels];
    
    // Process through plugin
    if let Ok(mut plugin_lock) = plugin.try_lock() {
        let mut audio_buffers = AudioBuffers {
            inputs: input_buffers,
            outputs: output_buffers,
            sample_rate: 44100.0,
            block_size,
        };
        
        // Process audio through the plugin
        if let Err(e) = plugin_lock.process(&mut audio_buffers) {
            eprintln!("Plugin processing error: {}", e);
            return;
        }
        
        output_buffers = audio_buffers.outputs;
    }
    
    // Copy plugin output to audio output buffer (interleaved format)
    for (frame_idx, output_frame) in output_buffer.chunks_mut(output_channels).enumerate() {
        if frame_idx >= block_size {
            break;
        }
        
        for (ch_idx, output_sample) in output_frame.iter_mut().enumerate() {
            if ch_idx < output_buffers.len() && frame_idx < output_buffers[ch_idx].len() {
                *output_sample = output_buffers[ch_idx][frame_idx];
            }
        }
    }
    
    // Update audio levels for monitoring
    if let Ok(mut levels_lock) = levels.try_lock() {
        levels_lock.update_from_buffers(&output_buffers);
    }
}

// Test signal generator for creating audio input
struct TestSignalGenerator {
    sample_rate: f64,
    phase: f64,
    frequency: f64,
    noise_level: f32,
}

impl TestSignalGenerator {
    fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            phase: 0.0,
            frequency: 440.0, // A4 note
            noise_level: 0.1,
        }
    }
    
    fn generate_stereo_signal(&mut self, left: &mut [f32], right: &mut [f32]) {
        for i in 0..left.len().min(right.len()) {
            // Generate sine wave
            let sine = (self.phase * 2.0 * std::f64::consts::PI).sin() as f32;
            
            // Add a bit of noise for character
            let noise = (rand::random::<f32>() - 0.5) * 2.0 * self.noise_level;
            
            let sample = sine * 0.5 + noise; // 50% sine, small amount of noise
            
            left[i] = sample;
            right[i] = sample * 0.8; // Slightly different for stereo effect
            
            // Advance phase
            self.phase += self.frequency / self.sample_rate;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }
    }
    
    // Change the frequency (for automation)
    fn set_frequency(&mut self, freq: f64) {
        self.frequency = freq;
    }
}

fn find_suitable_plugin(plugins: &[PluginInfo]) -> &PluginInfo {
    // Try to find an instrument first
    for plugin in plugins {
        if plugin.category.contains("Instrument") || 
           (plugin.audio_inputs == 0 && plugin.audio_outputs > 0 && plugin.has_midi_input) {
            return plugin;
        }
    }
    
    // Fall back to any effect
    for plugin in plugins {
        if plugin.audio_inputs > 0 && plugin.audio_outputs > 0 {
            return plugin;
        }
    }
    
    // Use the first plugin as last resort
    &plugins[0]
}

fn show_audio_levels(levels: &AudioLevels) {
    print!("\r🔊 Levels: ");
    for (i, channel) in levels.channels.iter().enumerate() {
        let bars = level_to_bars(channel.peak);
        let clip_indicator = if channel.is_clipping() { "🔴" } else { "" };
        print!("Ch{}: [{}]{} ", i + 1, bars, clip_indicator);
    }
    print!("        "); // Clear any leftover text
    use std::io::{self, Write};
    io::stdout().flush().unwrap();
}

fn level_to_bars(level: f32) -> String {
    let num_bars = ((level * 20.0) as usize).min(20);
    let filled = "█".repeat(num_bars);
    let empty = "░".repeat(20 - num_bars);
    format!("{}{}", filled, empty)
}

fn automate_parameters(plugin: &Arc<Mutex<Plugin>>, elapsed: Duration) {
    if let Ok(mut plugin_lock) = plugin.try_lock() {
        // Get parameters
        if let Ok(params) = plugin_lock.get_parameters() {
            if !params.is_empty() {
                // Automate the first parameter with a sine wave
                let t = elapsed.as_secs_f64();
                let automation_freq = 0.2; // Very slow automation
                let automated_value = 0.5 + 0.3 * (t * automation_freq * 2.0 * std::f64::consts::PI).sin();
                let automated_value = automated_value.clamp(0.0, 1.0) as f32;
                
                // Set the parameter value
                if let Err(e) = plugin_lock.set_parameter(params[0].id, automated_value) {
                    // Don't spam errors, just silently continue
                    if elapsed.as_millis() % 2000 == 0 {
                        eprintln!("Parameter automation error: {}", e);
                    }
                }
            }
        }
    }
}
```

Add this to your `Cargo.toml` for the random number generation:

```toml
[dependencies]
vst3-host = { version = "0.1.0", features = ["cpal-backend"] }
env_logger = "0.11"
rand = "0.8"
```

## Running the Audio Example

1. **Build and run:**
   ```bash
   cargo run
   ```

2. **Expected behavior:**
   - The program loads a plugin and starts audio processing
   - You should hear a 440Hz tone (A4 note) processed through the plugin
   - Real-time level meters show in the terminal
   - Parameter automation slowly changes the first plugin parameter
   - The program runs for 10 seconds then stops

3. **Expected output:**
   ```
   === VST3 Audio Processing ===
   
   Loading: SomePlugin by SomeVendor
   Audio config: 44100Hz, 512 samples, 2→2 channels
   🎵 Audio processing started!
   🎵 Playing test signal through plugin...
   🔊 Levels: Ch1: [████████░░░░░░░░░░░░] Ch2: [███████░░░░░░░░░░░░░] 
   ```

## Understanding the Code

### Audio Configuration
```rust
let config = AudioConfig {
    sample_rate: 44100.0,  // CD-quality sample rate
    block_size: 512,       // Common buffer size - balance between latency and efficiency
    input_channels: 2,     // Stereo input
    output_channels: 2,    // Stereo output
};
```

### Thread-Safe Plugin Access
```rust
let plugin = Arc<Mutex<Plugin>>(plugin);
```
We wrap the plugin in `Arc<Mutex<>>` because:
- **Arc** allows sharing between threads
- **Mutex** ensures thread-safe access (only one thread can use the plugin at a time)

### Real-time Audio Callback
```rust
Box::new(move |output_buffer: &mut [f32]| {
    // This runs in the audio thread - keep it fast!
    process_audio_callback(/* ... */);
})
```
This callback is called by the audio system every time it needs a new buffer of audio. It runs in a high-priority real-time thread.

### Buffer Format
Audio buffers come in two formats:
- **Interleaved**: `[L1, R1, L2, R2, L3, R3, ...]` (CPAL uses this)
- **Planar**: `[L1, L2, L3, ...], [R1, R2, R3, ...]` (VST3 plugins use this)

Our code converts between these formats.

## Common Patterns

### Safe Parameter Changes
```rust
if let Ok(mut plugin_lock) = plugin.try_lock() {
    plugin_lock.set_parameter(param_id, value)?;
}
```
Use `try_lock()` in audio contexts to avoid blocking the audio thread.

### Level Monitoring
```rust
levels.update_from_buffers(&output_buffers);
if levels.is_clipping() {
    println!("⚠️ Audio clipping detected!");
}
```

### Error Handling in Audio Callbacks
```rust
if let Err(e) = plugin_lock.process(&mut audio_buffers) {
    eprintln!("Plugin processing error: {}", e);
    // Continue processing - don't crash the audio thread!
    return;
}
```
Never panic or crash in audio callbacks - it will stop all audio.

## Advanced Concepts

### Buffer Size vs. Latency
- **Smaller buffers** = Lower latency, higher CPU usage
- **Larger buffers** = Higher latency, lower CPU usage
- Common sizes: 64, 128, 256, 512, 1024 samples

### Sample Rate Considerations
- **44.1kHz**: CD quality, most common
- **48kHz**: Video/pro audio standard
- **96kHz+**: High-resolution audio

Calculate latency: `latency_ms = (buffer_size / sample_rate) * 1000`

### Plugin Types and Audio Routing
```rust
let info = plugin.info();

if info.audio_inputs == 0 && info.audio_outputs > 0 {
    // Instrument: Generate audio from MIDI
    println!("This is an instrument - send MIDI!");
} else if info.audio_inputs > 0 && info.audio_outputs > 0 {
    // Effect: Process incoming audio
    println!("This is an effect - send audio!");
}
```

## Troubleshooting

### No Audio Output
- Check your system's default audio device
- Verify plugin loaded correctly
- Ensure plugin is started with `start_processing()`

### Audio Dropouts/Glitches
- Increase buffer size
- Check for CPU overload
- Avoid allocations in audio callback

### Plugin Not Processing
- Some plugins need MIDI input to generate sound
- Check if plugin parameters need specific values
- Verify audio routing matches plugin's input/output configuration

## What's Next?

You now have real-time audio processing! In the next tutorial, we'll add a GUI to control your plugin interactively.

**Coming up in Tutorial 3**: Building a graphical interface with real-time parameter controls and visualizations.

## Key Concepts Learned

- **Audio Buffers**: Chunks of audio samples processed together
- **Real-time Processing**: Time-critical audio thread constraints  
- **Thread Safety**: Using Arc<Mutex<>> for safe plugin access
- **Buffer Conversion**: Interleaved vs. planar audio formats
- **Level Monitoring**: Tracking audio levels and preventing clipping
- **Parameter Automation**: Changing plugin parameters over time

You're now processing audio in real-time! Next, we'll make it interactive.