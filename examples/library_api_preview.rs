// This file demonstrates what the final library API will look like
// It won't compile yet - it's a design preview

use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    example_1_minimal()?;
    example_2_with_callbacks()?;
    example_3_custom_audio()?;
    example_4_parameter_automation()?;
    example_5_midi_sequencer()?;
    Ok(())
}

// Example 1: Absolute minimal usage
fn example_1_minimal() -> Result<(), Box<dyn std::error::Error>> {
    // Create host with defaults (CPAL backend, 44.1kHz, 512 samples)
    let mut host = Vst3Host::new()?;
    
    // Load first available synth plugin
    let plugins = host.discover_plugins()?;
    let synth = plugins.iter()
        .find(|p| p.category.contains("Instrument"))
        .ok_or("No instrument plugins found")?;
    
    let mut plugin = host.load_plugin(&synth.path)?;
    plugin.start_processing()?;
    
    // Play a C major chord
    plugin.send_midi_note(60, 100, MidiChannel::Ch1)?; // C
    plugin.send_midi_note(64, 100, MidiChannel::Ch1)?; // E
    plugin.send_midi_note(67, 100, MidiChannel::Ch1)?; // G
    
    std::thread::sleep(std::time::Duration::from_secs(2));
    
    // Note offs
    plugin.send_midi_note_off(60, MidiChannel::Ch1)?;
    plugin.send_midi_note_off(64, MidiChannel::Ch1)?;
    plugin.send_midi_note_off(67, MidiChannel::Ch1)?;
    
    Ok(())
}

// Example 2: With callbacks and monitoring
fn example_2_with_callbacks() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Vst3Host::builder()
        .sample_rate(48000)
        .block_size(256)
        .build()?;
    
    let mut plugin = host.load_plugin("/Library/Audio/Plug-Ins/VST3/Surge XT.vst3")?;
    
    // Monitor parameter changes
    plugin.on_parameter_change(|param_id, value| {
        println!("Parameter {} changed to {}", param_id, value);
    });
    
    // Monitor audio levels
    plugin.on_audio_process(|levels| {
        let db_left = 20.0 * levels.channels[0].peak.log10();
        let db_right = 20.0 * levels.channels[1].peak.log10();
        print!("\rL: {:6.1} dB  R: {:6.1} dB", db_left, db_right);
    });
    
    plugin.start_processing()?;
    
    // Automate filter cutoff
    let cutoff_param = plugin.find_parameter("Cutoff")?;
    for i in 0..100 {
        let value = (i as f64 / 100.0).sin() * 0.5 + 0.5;
        plugin.set_parameter(cutoff_param.id, value)?;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    
    Ok(())
}

// Example 3: Custom audio backend
fn example_3_custom_audio() -> Result<(), Box<dyn std::error::Error>> {
    use vst3_host::{AudioBackend, AudioBuffers};
    
    struct FileWriterBackend {
        file: std::fs::File,
        sample_rate: f64,
    }
    
    impl AudioBackend for FileWriterBackend {
        fn start(&mut self) -> Result<()> { Ok(()) }
        fn stop(&mut self) -> Result<()> { Ok(()) }
        
        fn process(&mut self, callback: &mut dyn FnMut(&mut AudioBuffers)) -> Result<()> {
            let mut buffers = AudioBuffers::new(0, 2, 512, self.sample_rate);
            
            // Generate 10 seconds of audio
            for _ in 0..(self.sample_rate as usize * 10 / 512) {
                callback(&mut buffers)?;
                // Write buffers.outputs to file in WAV format
                // ... implementation details ...
            }
            Ok(())
        }
    }
    
    let backend = FileWriterBackend {
        file: std::fs::File::create("output.wav")?,
        sample_rate: 44100.0,
    };
    
    let mut host = Vst3Host::with_backend(backend)?;
    let mut plugin = host.load_plugin("effect.vst3")?;
    
    // Process audio file through plugin
    host.process()?;
    
    Ok(())
}

// Example 4: Parameter automation with curves
fn example_4_parameter_automation() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Vst3Host::new()?;
    let mut plugin = host.load_plugin("synth.vst3")?;
    
    // Create automation curves
    let automation = ParameterAutomation::new()
        .add_point(0.0, 0.0)      // Start at 0
        .add_point(1.0, 1.0)      // Ramp to 1 over 1 second  
        .add_point(2.0, 0.5)      // Drop to 0.5
        .add_point(3.0, 0.5)      // Hold
        .with_curve(AutomationCurve::Exponential);
    
    // Apply to filter cutoff
    let cutoff = plugin.find_parameter("Cutoff")?;
    plugin.automate_parameter(cutoff.id, automation)?;
    
    plugin.start_processing()?;
    
    // Play note while automation runs
    plugin.send_midi_note(60, 100, MidiChannel::Ch1)?;
    std::thread::sleep(std::time::Duration::from_secs(4));
    plugin.send_midi_note_off(60, MidiChannel::Ch1)?;
    
    Ok(())
}

// Example 5: MIDI sequencer
fn example_5_midi_sequencer() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Vst3Host::new()?;
    let mut plugin = host.load_plugin("drums.vst3")?;
    
    // Create a simple drum pattern
    let pattern = MidiPattern::new()
        .set_length_beats(4)
        .add_note(0.0, 36, 127, 0.1)    // Kick on 1
        .add_note(1.0, 38, 100, 0.1)    // Snare on 2
        .add_note(2.0, 36, 127, 0.1)    // Kick on 3
        .add_note(3.0, 38, 100, 0.1)    // Snare on 4
        .add_note(0.5, 42, 80, 0.05)    // Hi-hat 8ths
        .add_note(1.5, 42, 80, 0.05)
        .add_note(2.5, 42, 80, 0.05)
        .add_note(3.5, 42, 80, 0.05);
    
    // Create sequencer at 120 BPM
    let mut sequencer = MidiSequencer::new(120.0);
    sequencer.set_pattern(pattern);
    sequencer.set_loop(true);
    
    // Connect sequencer to plugin
    plugin.set_midi_input(sequencer.output());
    plugin.start_processing()?;
    
    // Play for 10 seconds
    sequencer.start();
    std::thread::sleep(std::time::Duration::from_secs(10));
    sequencer.stop();
    
    Ok(())
}

// Example 6: Plugin chain with effects
fn example_6_plugin_chain() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Vst3Host::new()?;
    
    // Create a chain: Synth -> Delay -> Reverb
    let mut chain = PluginChain::new();
    chain.add(host.load_plugin("synth.vst3")?);
    chain.add(host.load_plugin("delay.vst3")?);  
    chain.add(host.load_plugin("reverb.vst3")?);
    
    // Configure the chain
    chain.connect_all_in_series()?;
    chain.start_processing()?;
    
    // Send MIDI to first plugin (synth)
    chain.first_mut()?.send_midi_note(60, 100, MidiChannel::Ch1)?;
    
    // Adjust reverb mix on last plugin
    chain.last_mut()?.set_parameter_by_name("Mix", 0.3)?;
    
    std::thread::sleep(std::time::Duration::from_secs(5));
    
    Ok(())
}

// Example 7: Safe state management
fn example_7_state_management() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Vst3Host::new()?;
    let mut plugin = host.load_plugin("complex_synth.vst3")?;
    
    // Save current state
    let state = plugin.save_state()?;
    
    // Randomize all parameters
    for param in plugin.get_parameters()? {
        let random_value = rand::random::<f64>();
        plugin.set_parameter(param.id, random_value)?;
    }
    
    // Restore original state
    plugin.load_state(&state)?;
    
    // Export/import preset files
    plugin.export_preset("my_preset.vstpreset")?;
    plugin.import_preset("factory_preset.vstpreset")?;
    
    Ok(())
}