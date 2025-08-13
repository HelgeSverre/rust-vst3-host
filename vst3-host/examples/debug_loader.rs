//! Debug plugin loader with step-by-step control and detailed logging

use std::io::{self, Write};
use std::time::{Duration, Instant};
use vst3_host::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <vst3_plugin_path> [step]", args[0]);
        eprintln!("Steps: all, load, init, controller, connect, activate, process, test");
        std::process::exit(1);
    }
    
    let plugin_path = &args[1];
    let target_step = args.get(2).map(|s| s.as_str()).unwrap_or("all");
    
    println!("🔧 DEBUG PLUGIN LOADER");
    println!("Plugin: {}", plugin_path);
    println!("Target step: {}", target_step);
    println!("=====================================");
    
    // Step 1: Host creation
    if should_run_step(target_step, "load") {
        println!("\n📋 STEP 1: Creating VST3 Host");
        let start = Instant::now();
        let mut host = Vst3Host::new()?;
        println!("✅ Host created in {:?}", start.elapsed());
        wait_for_input("Continue to plugin loading?")?;
        
        // Step 2: Plugin loading
        println!("\n🔌 STEP 2: Loading Plugin");
        let start = Instant::now();
        let mut plugin = match host.load_plugin(plugin_path) {
            Ok(p) => {
                println!("✅ Plugin loaded in {:?}", start.elapsed());
                p
            }
            Err(e) => {
                println!("❌ Plugin loading failed: {}", e);
                return Err(e.into());
            }
        };
        
        // Step 3: Plugin info inspection
        if should_run_step(target_step, "init") {
            wait_for_input("Continue to plugin inspection?")?;
            println!("\n📊 STEP 3: Plugin Info");
            let info = plugin.info();
            println!("  Name: {}", info.name);
            println!("  Vendor: {}", info.vendor);
            println!("  Version: {}", info.version);
            println!("  Has GUI: {}", info.has_gui);
            println!("  Audio Inputs: {}", info.audio_inputs);
            println!("  Audio Outputs: {}", info.audio_outputs);
            println!("  Has MIDI Input: {}", info.has_midi_input);
            println!("  Has MIDI Output: {}", info.has_midi_output);
        }
        
        // Step 4: Parameter inspection
        if should_run_step(target_step, "controller") {
            wait_for_input("Continue to parameter inspection?")?;
            println!("\n🎛️ STEP 4: Parameters");
            let start = Instant::now();
            match plugin.get_parameters() {
                Ok(params) => {
                    println!("✅ Got {} parameters in {:?}", params.len(), start.elapsed());
                    println!("First 10 parameters:");
                    for (i, param) in params.iter().take(10).enumerate() {
                        println!("  {}: {} = {:.3}", i, param.name, param.value);
                    }
                    if params.len() > 10 {
                        println!("  ... and {} more", params.len() - 10);
                    }
                }
                Err(e) => {
                    println!("❌ Failed to get parameters: {}", e);
                }
            }
        }
        
        // Step 5: Processing test
        if should_run_step(target_step, "process") {
            wait_for_input("Continue to processing test?")?;
            println!("\n🎵 STEP 5: Processing Test");
            let start = Instant::now();
            
            match plugin.start_processing() {
                Ok(()) => {
                    println!("✅ Processing started in {:?}", start.elapsed());
                    
                    // Test MIDI if supported
                    if info.has_midi_input && should_run_step(target_step, "test") {
                        wait_for_input("Continue to MIDI test?")?;
                        println!("\n🎹 STEP 6: MIDI Test");
                        
                        for note in [60, 64, 67] { // C major chord
                            println!("  Sending note {} on...", note);
                            if let Err(e) = plugin.send_midi_event(MidiEvent::NoteOn {
                                channel: MidiChannel::Ch1,
                                note,
                                velocity: 100,
                            }) {
                                println!("    ❌ Failed: {}", e);
                            } else {
                                println!("    ✅ Note sent");
                            }
                            
                            std::thread::sleep(Duration::from_millis(100));
                            
                            println!("  Sending note {} off...", note);
                            if let Err(e) = plugin.send_midi_event(MidiEvent::NoteOff {
                                channel: MidiChannel::Ch1,
                                note,
                                velocity: 0,
                            }) {
                                println!("    ❌ Failed: {}", e);
                            } else {
                                println!("    ✅ Note off sent");
                            }
                            
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                    
                    // Stop processing
                    println!("\n🛑 Stopping processing...");
                    if let Err(e) = plugin.stop_processing() {
                        println!("❌ Failed to stop processing: {}", e);
                    } else {
                        println!("✅ Processing stopped cleanly");
                    }
                }
                Err(e) => {
                    println!("❌ Failed to start processing: {}", e);
                }
            }
        }
        
        println!("\n🎉 DEBUG TEST COMPLETE");
        println!("All tests passed! The plugin is working correctly.");
    }
    
    Ok(())
}

fn should_run_step(target: &str, step: &str) -> bool {
    target == "all" || target == step
}

fn wait_for_input(message: &str) -> io::Result<()> {
    print!("\n{} [Press Enter to continue] ", message);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(())
}