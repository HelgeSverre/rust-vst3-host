//! Basic VST3 host example demonstrating core functionality

use vst3_host::prelude::*;
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("VST3 Host Basic Example");
    println!("======================\n");
    
    // Create a host with default settings
    let mut host = Vst3Host::new()?;
    
    // Discover available plugins
    println!("Discovering VST3 plugins...");
    let plugins = host.discover_plugins()?;
    
    println!("Found {} plugins:", plugins.len());
    for (i, plugin) in plugins.iter().enumerate() {
        println!("  {}. {} by {} ({})", 
            i + 1, 
            plugin.name, 
            plugin.vendor,
            plugin.category
        );
    }
    
    if plugins.is_empty() {
        println!("\nNo VST3 plugins found. Please install some VST3 plugins and try again.");
        return Ok(());
    }
    
    // Load the first instrument plugin found (or first plugin if no instruments)
    let plugin_info = plugins.iter()
        .find(|p| p.category.contains("Instrument"))
        .or_else(|| plugins.first())
        .expect("No plugins found");
    
    println!("\nLoading plugin: {}", plugin_info.name);
    let mut plugin = host.load_plugin(&plugin_info.path)?;
    
    // Print plugin details
    println!("\nPlugin Details:");
    println!("  Name: {}", plugin.info().name);
    println!("  Vendor: {}", plugin.info().vendor);
    println!("  Version: {}", plugin.info().version);
    println!("  Audio: {} inputs, {} outputs", 
        plugin.info().audio_inputs, 
        plugin.info().audio_outputs
    );
    println!("  MIDI Input: {}", plugin.info().has_midi_input);
    println!("  Has GUI: {}", plugin.info().has_gui);
    
    // Get and display parameters
    let parameters = plugin.get_parameters()?;
    println!("\nPlugin has {} parameters:", parameters.len());
    if parameters.len() > 5 {
        println!("(Showing first 5)");
    }
    
    for param in parameters.iter().take(5) {
        println!("  {}: {} = {} {}", 
            param.id,
            param.name,
            param.format_value(param.value),
            param.unit
        );
    }
    
    // Set up parameter change monitoring
    plugin.on_parameter_change(|id, value| {
        println!("Parameter {} changed to {:.3}", id, value);
    });
    
    // Set up audio level monitoring
    plugin.on_audio_process(|levels| {
        // This would be called after each audio process cycle
        // For now, we'll just use it once after processing
    });
    
    // Start audio processing
    println!("\nStarting audio processing...");
    plugin.start_processing()?;
    
    // Play some notes if it's a MIDI instrument
    if plugin.info().has_midi_input {
        println!("\nPlaying test notes...");
        
        // Play a C major chord
        plugin.send_midi_note(60, 100, MidiChannel::Ch1)?; // C4
        thread::sleep(Duration::from_millis(100));
        
        plugin.send_midi_note(64, 100, MidiChannel::Ch1)?; // E4
        thread::sleep(Duration::from_millis(100));
        
        plugin.send_midi_note(67, 100, MidiChannel::Ch1)?; // G4
        
        // Hold the chord
        thread::sleep(Duration::from_secs(2));
        
        // Release the notes
        plugin.send_midi_note_off(60, MidiChannel::Ch1)?;
        plugin.send_midi_note_off(64, MidiChannel::Ch1)?;
        plugin.send_midi_note_off(67, MidiChannel::Ch1)?;
        
        // Wait for release
        thread::sleep(Duration::from_millis(500));
        
        // Play an arpeggio
        println!("Playing arpeggio...");
        let notes = [60, 64, 67, 72]; // C E G C
        for &note in &notes {
            plugin.send_midi_note(note, 80, MidiChannel::Ch1)?;
            thread::sleep(Duration::from_millis(200));
            plugin.send_midi_note_off(note, MidiChannel::Ch1)?;
            thread::sleep(Duration::from_millis(50));
        }
        
        thread::sleep(Duration::from_millis(500));
    }
    
    // Get final audio levels
    let levels = plugin.get_output_levels();
    println!("\nOutput levels:");
    for (i, channel) in levels.channels.iter().enumerate() {
        println!("  Channel {}: Peak = {:.1} dB, RMS = {:.1} dB",
            i + 1,
            channel.peak_db(),
            channel.rms_db()
        );
    }
    
    // Stop processing
    println!("\nStopping audio processing...");
    plugin.stop_processing()?;
    
    println!("\nExample completed successfully!");
    
    Ok(())
}

#[cfg(not(feature = "cpal-backend"))]
fn main() {
    eprintln!("This example requires the 'cpal-backend' feature.");
    eprintln!("Run with: cargo run --example basic_host --features cpal-backend");
}