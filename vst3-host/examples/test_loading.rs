//! Test plugin loading without GUI

use vst3_host::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <vst3_plugin_path>", args[0]);
        std::process::exit(1);
    }
    
    let plugin_path = &args[1];
    println!("Loading VST3 plugin: {}", plugin_path);
    
    // Create host
    let mut host = Vst3Host::new()?;
    println!("Host created successfully");
    
    // Load plugin
    println!("Loading plugin...");
    let mut plugin = host.load_plugin(plugin_path)?;
    println!("Plugin loaded successfully!");
    
    // Print plugin info
    let info = plugin.info();
    println!("Plugin Info:");
    println!("  Name: {}", info.name);
    println!("  Vendor: {}", info.vendor);
    println!("  Version: {}", info.version);
    println!("  Has GUI: {}", info.has_gui);
    println!("  Audio Inputs: {}", info.audio_inputs);
    println!("  Audio Outputs: {}", info.audio_outputs);
    println!("  Has MIDI Input: {}", info.has_midi_input);
    println!("  Has MIDI Output: {}", info.has_midi_output);
    
    // Try to get parameters
    match plugin.get_parameters() {
        Ok(params) => {
            println!("Parameters ({} total):", params.len());
            for (i, param) in params.iter().take(5).enumerate() {
                println!("  {}: {} = {:.3}", i, param.name, param.value);
            }
            if params.len() > 5 {
                println!("  ... and {} more parameters", params.len() - 5);
            }
        }
        Err(e) => {
            println!("Failed to get parameters: {}", e);
        }
    }
    
    // Start processing
    println!("Starting processing...");
    plugin.start_processing()?;
    println!("Processing started successfully!");
    
    // Send a test MIDI note
    println!("Sending test MIDI note...");
    plugin.send_midi_event(MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60, // Middle C
        velocity: 100,
    })?;
    println!("MIDI note sent successfully!");
    
    // Stop processing
    plugin.stop_processing()?;
    println!("Plugin test completed successfully!");
    
    Ok(())
}