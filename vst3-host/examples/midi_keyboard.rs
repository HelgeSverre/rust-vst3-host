//! Interactive MIDI keyboard example using terminal input

use vst3_host::prelude::*;
use std::io::{self, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("VST3 MIDI Keyboard");
    println!("==================\n");
    
    // Create host
    let mut host = Vst3Host::new()?;
    
    // Find an instrument plugin
    println!("Looking for instrument plugins...");
    let plugins = host.discover_plugins()?;
    
    let instruments: Vec<_> = plugins.iter()
        .filter(|p| p.has_midi_input && p.category.contains("Instrument"))
        .collect();
    
    if instruments.is_empty() {
        eprintln!("No instrument plugins found!");
        eprintln!("Please install a VST3 instrument plugin and try again.");
        return Ok(());
    }
    
    // Let user choose an instrument
    println!("\nAvailable instruments:");
    for (i, inst) in instruments.iter().enumerate() {
        println!("  {}. {} by {}", i + 1, inst.name, inst.vendor);
    }
    
    print!("\nSelect instrument (1-{}): ", instruments.len());
    io::stdout().flush()?;
    
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let selection: usize = input.trim().parse::<usize>()
        .unwrap_or(1)
        .saturating_sub(1)
        .min(instruments.len() - 1);
    
    let selected = instruments[selection];
    println!("\nLoading {}...", selected.name);
    
    let mut plugin = host.load_plugin(&selected.path)?;
    plugin.start_processing()?;
    
    // Show instructions
    println!("\nKeyboard Mapping:");
    println!("  Bottom row: Z X C V B N M , . /  = C3 to B3");
    println!("  Top row:    S D   G H J   L ;    = C#3 to A#3");
    println!("  ");
    println!("  Number row: Q W E R T Y U I O P  = C4 to B4");
    println!("  Above:      2 3   5 6 7   9 0    = C#4 to A#4");
    println!();
    println!("  Space: Sustain pedal");
    println!("  - / +: Octave down/up");
    println!("  [ / ]: Velocity down/up");
    println!("  Esc: Quit");
    println!();
    
    // Keyboard state
    let mut octave = 3i8;
    let mut velocity = 80u8;
    let mut sustained = false;
    let mut pressed_notes: std::collections::HashSet<u8> = std::collections::HashSet::new();
    
    // Main keyboard loop
    println!("Ready! Octave: C{}, Velocity: {}", octave, velocity);
    
    // Note: In a real implementation, you'd use a proper keyboard input library
    // like crossterm or termion for raw keyboard input. This is a simplified version.
    loop {
        print!("> ");
        io::stdout().flush()?;
        
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        
        let input = input.trim().to_lowercase();
        if input.is_empty() {
            continue;
        }
        
        match input.as_str() {
            "quit" | "exit" | "q" => break,
            
            // Octave controls
            "-" => {
                if octave > 0 {
                    octave -= 1;
                    println!("Octave: C{}", octave);
                }
            }
            "+" | "=" => {
                if octave < 7 {
                    octave += 1;
                    println!("Octave: C{}", octave);
                }
            }
            
            // Velocity controls
            "[" => {
                velocity = velocity.saturating_sub(10).max(1);
                println!("Velocity: {}", velocity);
            }
            "]" => {
                velocity = (velocity + 10).min(127);
                println!("Velocity: {}", velocity);
            }
            
            // Sustain pedal
            "space" | " " => {
                sustained = !sustained;
                plugin.send_midi_cc(cc::SUSTAIN, if sustained { 127 } else { 0 }, MidiChannel::Ch1)?;
                println!("Sustain: {}", if sustained { "ON" } else { "OFF" });
            }
            
            // Panic
            "panic" => {
                plugin.midi_panic()?;
                pressed_notes.clear();
                println!("MIDI Panic sent");
            }
            
            // Play notes based on input
            _ => {
                // Parse note commands like "c4", "f#5", etc.
                if let Some(note_num) = parse_note_input(&input, octave) {
                    // Toggle note
                    if pressed_notes.contains(&note_num) {
                        plugin.send_midi_note_off(note_num, MidiChannel::Ch1)?;
                        pressed_notes.remove(&note_num);
                        println!("Note OFF: {} ({})", note_to_name(note_num), note_num);
                    } else {
                        plugin.send_midi_note(note_num, velocity, MidiChannel::Ch1)?;
                        pressed_notes.insert(note_num);
                        println!("Note ON: {} ({}) vel={}", note_to_name(note_num), note_num, velocity);
                    }
                } else {
                    // Try to map single keys to notes
                    if let Some(note_offset) = map_key_to_note(&input) {
                        let note_num = (octave as u8 * 12 + 12 + note_offset).min(127);
                        plugin.send_midi_note(note_num, velocity, MidiChannel::Ch1)?;
                        pressed_notes.insert(note_num);
                        println!("Note: {} ({})", note_to_name(note_num), note_num);
                        
                        // Auto-release after a moment for single key presses
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        plugin.send_midi_note_off(note_num, MidiChannel::Ch1)?;
                        pressed_notes.remove(&note_num);
                    } else {
                        println!("Unknown command: {}", input);
                        println!("Type 'quit' to exit");
                    }
                }
            }
        }
        
        // Show current levels
        let levels = plugin.get_output_levels();
        if levels.channels.iter().any(|ch| ch.peak > 0.01) {
            print!("Output: ");
            for (i, ch) in levels.channels.iter().enumerate() {
                if i > 0 { print!(" | "); }
                print!("Ch{}: {:.1}dB", i + 1, ch.peak_db());
            }
            println!();
        }
    }
    
    // Clean up
    println!("\nSending all notes off...");
    plugin.midi_panic()?;
    plugin.stop_processing()?;
    
    println!("Goodbye!");
    Ok(())
}

fn map_key_to_note(key: &str) -> Option<u8> {
    match key {
        // Bottom row - white keys C to B
        "z" => Some(0),   // C
        "x" => Some(2),   // D
        "c" => Some(4),   // E
        "v" => Some(5),   // F
        "b" => Some(7),   // G
        "n" => Some(9),   // A
        "m" => Some(11),  // B
        "," => Some(12),  // C (next octave)
        "." => Some(14),  // D
        "/" => Some(16),  // E
        
        // Top row - black keys
        "s" => Some(1),   // C#
        "d" => Some(3),   // D#
        "g" => Some(6),   // F#
        "h" => Some(8),   // G#
        "j" => Some(10),  // A#
        "l" => Some(13),  // C# (next octave)
        ";" => Some(15),  // D#
        
        // Number row - white keys one octave up
        "q" => Some(12),  // C
        "w" => Some(14),  // D
        "e" => Some(16),  // E
        "r" => Some(17),  // F
        "t" => Some(19),  // G
        "y" => Some(21),  // A
        "u" => Some(23),  // B
        "i" => Some(24),  // C (next octave)
        "o" => Some(26),  // D
        "p" => Some(28),  // E
        
        // Number row - black keys
        "2" => Some(13),  // C#
        "3" => Some(15),  // D#
        "5" => Some(18),  // F#
        "6" => Some(20),  // G#
        "7" => Some(22),  // A#
        "9" => Some(25),  // C# (next octave)
        "0" => Some(27),  // D#
        
        _ => None,
    }
}

fn parse_note_input(input: &str, default_octave: i8) -> Option<u8> {
    // Try to parse note names like "c4", "f#5", etc.
    if input.len() >= 1 {
        // Check if it ends with a number (octave)
        let (note_part, octave_part) = if input.chars().last()?.is_ascii_digit() {
            let split_pos = input.rfind(|c: char| !c.is_ascii_digit()).map(|i| i + 1).unwrap_or(0);
            (&input[..split_pos], &input[split_pos..])
        } else {
            (input, "")
        };
        
        let octave = if octave_part.is_empty() {
            default_octave
        } else {
            octave_part.parse::<i8>().ok()?
        };
        
        let note_offset = match note_part {
            "c" => Some(0),
            "c#" | "cs" | "db" => Some(1),
            "d" => Some(2),
            "d#" | "ds" | "eb" => Some(3),
            "e" => Some(4),
            "f" => Some(5),
            "f#" | "fs" | "gb" => Some(6),
            "g" => Some(7),
            "g#" | "gs" | "ab" => Some(8),
            "a" => Some(9),
            "a#" | "as" | "bb" => Some(10),
            "b" => Some(11),
            _ => None,
        }?;
        
        let midi_note = (octave + 2) * 12 + note_offset;
        if (0..=127).contains(&midi_note) {
            Some(midi_note as u8)
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(not(feature = "cpal-backend"))]
fn main() {
    eprintln!("This example requires the 'cpal-backend' feature.");
    eprintln!("Run with: cargo run --example midi_keyboard --features cpal-backend");
}