//! # VST3 Parameter Automation Example
//!
//! This interactive example demonstrates advanced parameter automation and MIDI capabilities.
//! It creates an automated performance that showcases how to build dynamic, musical interactions
//! with VST3 plugins.
//!
//! ## What This Example Shows
//! - Real-time parameter automation with multiple curve types
//! - Complex MIDI sequence generation and playback
//! - Audio level monitoring and visualization
//! - Performance timing and synchronization
//! - Error handling in real-time contexts
//!
//! ## Features Demonstrated
//! - **Smooth Parameter Automation**: Linear, exponential, and sine wave automation curves
//! - **Musical MIDI Sequences**: Chord progressions, arpeggios, and rhythmic patterns  
//! - **Real-time Monitoring**: Audio levels, CPU usage, and timing metrics
//! - **Interactive Control**: Runtime parameter adjustment and pattern selection
//! - **Graceful Error Recovery**: Robust handling of plugin issues
//!
//! ## Usage
//! ```bash
//! # Run with default settings (will discover and use first suitable plugin)
//! cargo run --example parameter_automation --features cpal-backend
//!
//! # Run with specific plugin
//! cargo run --example parameter_automation --features cpal-backend -- /path/to/plugin.vst3
//!
//! # Run with verbose logging
//! RUST_LOG=debug cargo run --example parameter_automation --features cpal-backend
//! ```
//!
//! ## Interactive Controls
//! While running, you can press:
//! - `1-5`: Switch between different automation patterns
//! - `q`: Quit the application
//! - `space`: Pause/resume automation
//! - `r`: Reset all parameters to defaults
//!
//! ## Example Output
//! ```
//! VST3 Parameter Automation Demo
//! ==============================
//!
//! Loaded: SomeSynth v1.0 by VendorName
//! Audio: 44.1kHz, 512 samples, 0→2 channels
//!  Found 23 parameters for automation
//!
//! Starting automation performance...
//!
//! [00:05] Pattern: Chord Progression | CPU: 12.3% | Peak: -8.2dB
//! [00:10] Pattern: Arpeggio Sweep    | CPU: 15.1% | Peak: -6.1dB
//! [00:15] Pattern: Filter Resonance  | CPU: 11.8% | Peak: -9.4dB
//!
//! Controls: [1-5] Patterns | [Space] Pause | [R] Reset | [Q] Quit
//! ```

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use vst3_host::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp(Some(env_logger::fmt::TimestampPrecision::Millis))
        .init();

    println!("VST3 Parameter Automation Demo");
    println!("===============================");
    println!();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let plugin_path = if args.len() > 1 {
        Some(args[1].clone())
    } else {
        None
    };

    // Create and configure the automation demo
    let mut demo = AutomationDemo::new()?;
    demo.run(plugin_path)?;

    Ok(())
}

/// Main automation demo application
#[allow(dead_code)] // `backend` is kept alive for the stream's lifetime, not read directly
struct AutomationDemo {
    host: Vst3Host,
    backend: Option<CpalBackend>,
}

impl AutomationDemo {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let host = Vst3Host::builder()
            .sample_rate(44100.0)
            .block_size(512)
            .scan_default_paths()
            .build()?;

        Ok(Self {
            host,
            backend: None,
        })
    }

    fn run(&mut self, plugin_path: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
        // Step 1: Find or load plugin
        let plugin = if let Some(path) = plugin_path {
            println!("Loading specified plugin: {}", path);
            self.host.load_plugin(&path)?
        } else {
            println!("Discovering plugins...");
            let plugins = self.host.discover_plugins()?;

            if plugins.is_empty() {
                eprintln!("No VST3 plugins found!");
                eprintln!("   Please install some VST3 plugins or specify a path.");
                return Ok(());
            }

            // Find a suitable plugin (prefer instruments)
            let suitable_plugin = plugins
                .iter()
                .find(|p| p.has_midi_input && p.audio_outputs > 0)
                .or_else(|| plugins.iter().find(|p| p.audio_outputs > 0))
                .unwrap_or(&plugins[0]);

            println!(
                "Using plugin: {} by {}",
                suitable_plugin.name, suitable_plugin.vendor
            );
            self.host.load_plugin(&suitable_plugin.path)?
        };

        // Step 2: Initialize plugin
        let mut plugin = plugin;
        plugin.start_processing()?;

        let info = plugin.info();
        println!("Loaded: {} v{} by {}", info.name, info.version, info.vendor);
        println!(
            "Audio: {}kHz, {} samples, {}→{} channels",
            44.1, 512, info.audio_inputs, info.audio_outputs
        );

        // Step 3: Discover parameters for automation
        let parameters = plugin.get_parameters().unwrap_or_default();
        println!("Found {} parameters for automation", parameters.len());

        if parameters.is_empty() {
            println!("No parameters available for automation");
            println!("   This plugin may not support parameter automation");
            return Ok(());
        }

        // Step 4: Set up audio processing
        self.setup_audio(plugin)?;

        Ok(())
    }

    fn setup_audio(&mut self, plugin: Plugin) -> Result<(), Box<dyn std::error::Error>> {
        // Create audio backend
        let backend = CpalBackend::new()?;
        let device = backend
            .default_output_device()
            .ok_or("No audio output device found")?;

        let config = AudioConfig {
            sample_rate: 44100.0,
            block_size: 512,
            input_channels: 0,
            output_channels: 2,
        };

        // Create shared state for the automation performance
        let performance = Arc::new(Mutex::new(AutomationPerformance::new(plugin)));
        let performance_clone = performance.clone();

        // Set up communication channels
        let (control_tx, control_rx) = mpsc::channel::<ControlMessage>();
        let (status_tx, status_rx) = mpsc::channel::<StatusMessage>();

        // Create audio stream
        let stream = backend.create_output_stream(
            &device,
            config,
            Box::new(move |output: &mut [f32]| {
                audio_callback(output, &performance_clone, &status_tx, config);
            }),
            Box::new(|err| eprintln!("Audio error: {}", err)),
        )?;

        stream.play()?;
        println!("Audio stream started");

        // Start automation controller
        let automation_performance = performance.clone();
        let automation_control_rx = control_rx;
        thread::spawn(move || {
            automation_controller(automation_performance, automation_control_rx);
        });

        // Start the interactive controller
        println!();
        println!("Starting automation performance...");
        println!();
        self.run_interactive_controller(control_tx, status_rx)?;

        Ok(())
    }

    fn run_interactive_controller(
        &self,
        control_tx: mpsc::Sender<ControlMessage>,
        status_rx: mpsc::Receiver<StatusMessage>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::io::{self, Read};

        println!("Controls: [1-5] Patterns | [Space] Pause | [R] Reset | [Q] Quit");
        println!();

        // Set up non-blocking input
        let mut stdin = io::stdin();
        let mut input_buffer = [0u8; 1];

        let start_time = Instant::now();
        let mut last_status_time = Instant::now();

        loop {
            // Handle status updates
            while let Ok(status) = status_rx.try_recv() {
                match status {
                    StatusMessage::PerformanceUpdate {
                        pattern,
                        cpu_usage,
                        peak_level,
                        automation_active,
                    } => {
                        if last_status_time.elapsed() > Duration::from_millis(500) {
                            let elapsed = start_time.elapsed();
                            let minutes = elapsed.as_secs() / 60;
                            let seconds = elapsed.as_secs() % 60;

                            let status_icon = if automation_active {
                                "[active]"
                            } else {
                                "[idle]"
                            };
                            let peak_db = if peak_level > 0.0001 {
                                format!("{:.1}dB", 20.0 * peak_level.log10())
                            } else {
                                "-∞dB".to_string()
                            };

                            println!(
                                "[{:02}:{:02}] {} Pattern: {} | CPU: {:.1}% | Peak: {}",
                                minutes, seconds, status_icon, pattern, cpu_usage, peak_db
                            );

                            last_status_time = Instant::now();
                        }
                    }
                    StatusMessage::Error(msg) => {
                        println!("Error: {}", msg);
                    }
                }
            }

            // Handle user input (non-blocking)
            if stdin.read_exact(&mut input_buffer).is_ok() {
                match input_buffer[0] {
                    b'q' | b'Q' => {
                        println!("Stopping automation...");
                        control_tx.send(ControlMessage::Stop)?;
                        break;
                    }
                    b' ' => {
                        control_tx
                            .send(ControlMessage::TogglePause)
                            .map_err(|_| Error::Other("Control channel closed".to_string()))?;
                    }
                    b'r' | b'R' => {
                        println!("Resetting parameters...");
                        control_tx
                            .send(ControlMessage::Reset)
                            .map_err(|_| Error::Other("Control channel closed".to_string()))?;
                    }
                    b'1'..=b'5' => {
                        let pattern_id = (input_buffer[0] - b'1') as usize;
                        println!("Switching to pattern {}", pattern_id + 1);
                        control_tx
                            .send(ControlMessage::ChangePattern(pattern_id))
                            .map_err(|_| Error::Other("Control channel closed".to_string()))?;
                    }
                    _ => {}
                }
            }

            thread::sleep(Duration::from_millis(50));
        }

        println!("Automation demo completed!");
        Ok(())
    }
}

/// Performance state management
struct AutomationPerformance {
    plugin: Plugin,
    parameters: Vec<Parameter>,
    current_pattern: usize,
    pattern_start_time: Instant,
    is_paused: bool,
    automation_patterns: Vec<AutomationPattern>,
    midi_sequences: HashMap<usize, MidiSequence>,
}

impl AutomationPerformance {
    fn new(plugin: Plugin) -> Self {
        let parameters = plugin.get_parameters().unwrap_or_default();

        Self {
            plugin,
            parameters,
            current_pattern: 0,
            pattern_start_time: Instant::now(),
            is_paused: false,
            automation_patterns: create_automation_patterns(),
            midi_sequences: create_midi_sequences(),
        }
    }

    fn update(&mut self, elapsed: Duration) -> Result<(), vst3_host::Error> {
        if self.is_paused {
            return Ok(());
        }

        let pattern_time = elapsed.as_secs_f64();

        // Apply current automation pattern
        if let Some(pattern) = self.automation_patterns.get(self.current_pattern).cloned() {
            self.apply_automation_pattern(&pattern, pattern_time)?;
        }

        // Play MIDI sequence
        if let Some(sequence) = self.midi_sequences.get(&self.current_pattern).cloned() {
            self.play_midi_sequence(&sequence, pattern_time)?;
        }

        Ok(())
    }

    fn apply_automation_pattern(
        &mut self,
        pattern: &AutomationPattern,
        time: f64,
    ) -> Result<(), vst3_host::Error> {
        for automation in &pattern.automations {
            if let Some(param) = self.parameters.iter().find(|p| {
                p.name
                    .to_lowercase()
                    .contains(&automation.parameter_name.to_lowercase())
            }) {
                let value = automation.curve.calculate_value(time);
                self.plugin.set_parameter(param.id, value as f64)?;
            }
        }
        Ok(())
    }

    fn play_midi_sequence(
        &mut self,
        sequence: &MidiSequence,
        time: f64,
    ) -> Result<(), vst3_host::Error> {
        // Simple note triggering based on time
        let beat_time = (time * sequence.tempo / 60.0) % sequence.length;

        for event in &sequence.events {
            if (beat_time - event.time).abs() < 0.1 {
                self.plugin.send_midi_event(event.event)?;
            }
        }

        Ok(())
    }

    fn change_pattern(&mut self, pattern_id: usize) {
        if pattern_id < self.automation_patterns.len() {
            self.current_pattern = pattern_id;
            self.pattern_start_time = Instant::now();
        }
    }

    fn toggle_pause(&mut self) {
        self.is_paused = !self.is_paused;
        if !self.is_paused {
            self.pattern_start_time = Instant::now();
        }
    }

    fn reset_parameters(&mut self) -> Result<(), vst3_host::Error> {
        // Reset all parameters to their default values (0.5 for normalized parameters)
        for param in &self.parameters {
            self.plugin.set_parameter(param.id, 0.5)?;
        }
        Ok(())
    }
}

/// Automation pattern definition
#[allow(dead_code)] // illustrative fields not all read in this demo
#[derive(Debug, Clone)]
struct AutomationPattern {
    name: String,
    duration: f64, // seconds
    automations: Vec<ParameterAutomation>,
}

#[derive(Debug, Clone)]
struct ParameterAutomation {
    parameter_name: String,
    curve: AutomationCurve,
}

#[derive(Debug, Clone)]
enum AutomationCurve {
    Linear {
        start: f32,
        end: f32,
    },
    Sine {
        center: f32,
        amplitude: f32,
        frequency: f64,
    },
    Exponential {
        start: f32,
        end: f32,
        factor: f64,
    },
    Steps {
        values: Vec<f32>,
        step_duration: f64,
    },
}

impl AutomationCurve {
    fn calculate_value(&self, time: f64) -> f32 {
        match self {
            AutomationCurve::Linear { start, end } => {
                let progress = (time % 10.0) / 10.0; // 10-second cycle
                start + (end - start) * progress as f32
            }
            AutomationCurve::Sine {
                center,
                amplitude,
                frequency,
            } => center + amplitude * (time * frequency * 2.0 * std::f64::consts::PI).sin() as f32,
            AutomationCurve::Exponential { start, end, factor } => {
                let progress = (time % 8.0) / 8.0; // 8-second cycle
                let exp_progress = (progress * factor).exp() / factor.exp();
                start + (end - start) * exp_progress as f32
            }
            AutomationCurve::Steps {
                values,
                step_duration,
            } => {
                let step_index = ((time / step_duration) as usize) % values.len();
                values[step_index]
            }
        }
    }
}

/// MIDI sequence definition
#[derive(Debug, Clone)]
struct MidiSequence {
    tempo: f64,  // BPM
    length: f64, // beats
    events: Vec<TimedMidiEvent>,
}

#[derive(Debug, Clone)]
struct TimedMidiEvent {
    time: f64, // beat position
    event: MidiEvent,
}

/// Communication messages
#[derive(Debug)]
enum ControlMessage {
    ChangePattern(usize),
    TogglePause,
    Reset,
    Stop,
}

#[derive(Debug)]
enum StatusMessage {
    PerformanceUpdate {
        pattern: String,
        cpu_usage: f32,
        peak_level: f32,
        automation_active: bool,
    },
    Error(String),
}

/// Audio processing callback
fn audio_callback(
    output: &mut [f32],
    performance: &Arc<Mutex<AutomationPerformance>>,
    status_tx: &mpsc::Sender<StatusMessage>,
    config: AudioConfig,
) {
    let start_time = Instant::now();

    // Clear output buffer
    output.fill(0.0);

    // Process through plugin
    if let Ok(mut perf) = performance.try_lock() {
        // Update automation
        let elapsed = perf.pattern_start_time.elapsed();
        if let Err(e) = perf.update(elapsed) {
            let _ = status_tx.send(StatusMessage::Error(format!("Automation error: {}", e)));
            return;
        }

        // Create audio buffers
        let mut buffers = AudioBuffers::new(0, 2, config.block_size, config.sample_rate);

        // Process audio
        if perf.plugin.process_audio(&mut buffers).is_ok() {
            // Copy to output buffer (interleaved)
            for (frame_idx, output_frame) in output.chunks_mut(2).enumerate() {
                if frame_idx >= config.block_size {
                    break;
                }

                for (ch_idx, output_sample) in output_frame.iter_mut().enumerate() {
                    if ch_idx < buffers.outputs.len() && frame_idx < buffers.outputs[ch_idx].len() {
                        *output_sample = buffers.outputs[ch_idx][frame_idx];
                    }
                }
            }

            // Calculate peak level
            let peak = buffers
                .outputs
                .iter()
                .flat_map(|channel| channel.iter())
                .map(|&sample| sample.abs())
                .fold(0.0f32, f32::max);

            // Calculate CPU usage
            let processing_time = start_time.elapsed();
            let buffer_duration =
                Duration::from_secs_f64(config.block_size as f64 / config.sample_rate);
            let cpu_usage =
                (processing_time.as_secs_f64() / buffer_duration.as_secs_f64() * 100.0) as f32;

            // Send status update
            if let Some(pattern) = perf.automation_patterns.get(perf.current_pattern) {
                let _ = status_tx.send(StatusMessage::PerformanceUpdate {
                    pattern: pattern.name.clone(),
                    cpu_usage,
                    peak_level: peak,
                    automation_active: !perf.is_paused,
                });
            }
        }
    }
}

/// Automation controller thread
fn automation_controller(
    performance: Arc<Mutex<AutomationPerformance>>,
    control_rx: mpsc::Receiver<ControlMessage>,
) {
    let mut running = true;

    while running {
        // Handle control messages
        if let Ok(message) = control_rx.recv_timeout(Duration::from_millis(100)) {
            if let Ok(mut perf) = performance.lock() {
                match message {
                    ControlMessage::ChangePattern(pattern_id) => {
                        perf.change_pattern(pattern_id);
                    }
                    ControlMessage::TogglePause => {
                        perf.toggle_pause();
                    }
                    ControlMessage::Reset => {
                        let _ = perf.reset_parameters();
                    }
                    ControlMessage::Stop => {
                        running = false;
                    }
                }
            }
        }
    }
}

/// Create predefined automation patterns
fn create_automation_patterns() -> Vec<AutomationPattern> {
    vec![
        AutomationPattern {
            name: "Gentle Filter Sweep".to_string(),
            duration: 16.0,
            automations: vec![
                ParameterAutomation {
                    parameter_name: "cutoff".to_string(),
                    curve: AutomationCurve::Sine {
                        center: 0.6,
                        amplitude: 0.3,
                        frequency: 0.1,
                    },
                },
                ParameterAutomation {
                    parameter_name: "resonance".to_string(),
                    curve: AutomationCurve::Linear {
                        start: 0.2,
                        end: 0.8,
                    },
                },
            ],
        },
        AutomationPattern {
            name: "Rhythmic Tremolo".to_string(),
            duration: 8.0,
            automations: vec![ParameterAutomation {
                parameter_name: "volume".to_string(),
                curve: AutomationCurve::Steps {
                    values: vec![0.8, 0.3, 0.8, 0.3, 0.8, 0.5],
                    step_duration: 0.25,
                },
            }],
        },
        AutomationPattern {
            name: "Exponential Build".to_string(),
            duration: 12.0,
            automations: vec![
                ParameterAutomation {
                    parameter_name: "gain".to_string(),
                    curve: AutomationCurve::Exponential {
                        start: 0.1,
                        end: 0.9,
                        factor: 3.0,
                    },
                },
                ParameterAutomation {
                    parameter_name: "reverb".to_string(),
                    curve: AutomationCurve::Linear {
                        start: 0.0,
                        end: 0.6,
                    },
                },
            ],
        },
        AutomationPattern {
            name: "Complex Modulation".to_string(),
            duration: 20.0,
            automations: vec![
                ParameterAutomation {
                    parameter_name: "cutoff".to_string(),
                    curve: AutomationCurve::Sine {
                        center: 0.5,
                        amplitude: 0.4,
                        frequency: 0.3,
                    },
                },
                ParameterAutomation {
                    parameter_name: "delay".to_string(),
                    curve: AutomationCurve::Sine {
                        center: 0.3,
                        amplitude: 0.2,
                        frequency: 0.1,
                    },
                },
            ],
        },
        AutomationPattern {
            name: "Stepped Sequence".to_string(),
            duration: 16.0,
            automations: vec![ParameterAutomation {
                parameter_name: "pitch".to_string(),
                curve: AutomationCurve::Steps {
                    values: vec![0.0, 0.2, 0.4, 0.6, 0.8, 1.0, 0.8, 0.6, 0.4, 0.2],
                    step_duration: 0.5,
                },
            }],
        },
    ]
}

/// Create MIDI sequences for each pattern
fn create_midi_sequences() -> HashMap<usize, MidiSequence> {
    let mut sequences = HashMap::new();

    // Pattern 0: Simple chord progression
    sequences.insert(
        0,
        MidiSequence {
            tempo: 120.0,
            length: 16.0,
            events: vec![
                TimedMidiEvent {
                    time: 0.0,
                    event: MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 60,
                        velocity: 80,
                    },
                },
                TimedMidiEvent {
                    time: 4.0,
                    event: MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 64,
                        velocity: 80,
                    },
                },
                TimedMidiEvent {
                    time: 8.0,
                    event: MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 67,
                        velocity: 80,
                    },
                },
                TimedMidiEvent {
                    time: 12.0,
                    event: MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 72,
                        velocity: 80,
                    },
                },
            ],
        },
    );

    // Pattern 1: Rhythmic pattern
    sequences.insert(
        1,
        MidiSequence {
            tempo: 140.0,
            length: 8.0,
            events: vec![
                TimedMidiEvent {
                    time: 0.0,
                    event: MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 60,
                        velocity: 100,
                    },
                },
                TimedMidiEvent {
                    time: 1.0,
                    event: MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 60,
                        velocity: 70,
                    },
                },
                TimedMidiEvent {
                    time: 2.0,
                    event: MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 60,
                        velocity: 100,
                    },
                },
                TimedMidiEvent {
                    time: 3.5,
                    event: MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 60,
                        velocity: 90,
                    },
                },
            ],
        },
    );

    sequences
}
