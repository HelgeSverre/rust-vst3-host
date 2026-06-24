//! Process isolation for VST3 plugin hosting
//!
//! This module provides functionality to run VST3 plugins in separate processes
//! for improved stability and crash protection.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

/// Default time to wait for a helper response before treating the plugin as hung.
pub(crate) const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Commands that can be sent to the isolated plugin process.
///
/// This enum is the single source of truth for the isolation IPC protocol — the
/// helper binary imports it from here rather than redefining it, so the two halves
/// can never drift apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HostCommand {
    /// Load a plugin from the specified path, configured for the given audio settings.
    LoadPlugin {
        /// Path to the `.vst3` bundle.
        path: String,
        /// Sample rate to configure the plugin for.
        sample_rate: f64,
        /// Block size to configure the plugin for.
        block_size: u32,
        /// Transport tempo (BPM) to advertise in the plugin's host `ProcessContext`.
        tempo: f64,
        /// Time signature numerator to advertise in the host `ProcessContext`.
        time_sig_numerator: i32,
        /// Time signature denominator to advertise in the host `ProcessContext`.
        time_sig_denominator: i32,
    },
    /// Unload the current plugin
    UnloadPlugin,
    /// Create plugin GUI
    CreateGui,
    /// Close plugin GUI
    CloseGui,
    /// Start the plugin's audio processing.
    StartProcessing,
    /// Stop the plugin's audio processing.
    StopProcessing,
    /// Set a parameter (normalized 0.0..=1.0).
    SetParameter {
        /// Parameter id.
        id: u32,
        /// Normalized value.
        value: f64,
    },
    /// Schedule a parameter change at a sample offset within the next process block.
    SetParameterAt {
        /// Parameter id.
        id: u32,
        /// Normalized value.
        value: f64,
        /// Sample offset within the next processed block.
        offset: i32,
    },
    /// Set the transport tempo (BPM) advertised in the plugin's host `ProcessContext`, taking
    /// effect on the next processed block.
    SetTempo {
        /// Transport tempo in beats per minute (validated `> 0` on the host side).
        bpm: f64,
    },
    /// Set the transport time signature advertised in the plugin's host `ProcessContext`,
    /// taking effect on the next processed block.
    SetTimeSignature {
        /// Time signature numerator (validated `> 0` on the host side).
        numerator: i32,
        /// Time signature denominator (validated `1|2|4|8|16` on the host side).
        denominator: i32,
    },
    /// Toggle the transport playing state (`kPlaying`) in the plugin's host `ProcessContext`,
    /// taking effect on the next processed block.
    SetPlaying {
        /// Whether the transport is playing.
        playing: bool,
    },
    /// Read a parameter's current normalized value.
    GetParameter {
        /// Parameter id.
        id: u32,
    },
    /// Read all parameters.
    GetAllParameters,
    /// Ask the plugin to format a normalized value as a display string.
    FormatParameter {
        /// Parameter id.
        id: u32,
        /// Normalized value to format.
        normalized: f64,
    },
    /// Send a MIDI event to the plugin.
    SendMidi {
        /// The event to deliver.
        event: crate::midi::MidiEvent,
    },
    /// Schedule a MIDI event at a sample offset within the next process block.
    SendMidiAt {
        /// The event to deliver.
        event: crate::midi::MidiEvent,
        /// Sample offset within the next processed block.
        sample_offset: i32,
    },
    /// Process one block of audio. `inputs` is per-channel; `frames` is the block length.
    Process {
        /// Per-channel input samples (`[channel][frame]`).
        inputs: Vec<Vec<f32>>,
        /// Number of frames in this block.
        frames: u32,
    },
    /// Serialize the plugin's current state to an opaque byte blob.
    SaveState,
    /// Restore the plugin's state from a blob previously returned by `SaveState`.
    LoadState {
        /// The opaque state bytes.
        data: Vec<u8>,
    },
    /// Start a note (MPE). The helper's plugin allocates the per-voice note id and returns
    /// it in [`HostResponse::NoteStarted`] (in isolation the helper owns the real plugin).
    NoteOn {
        /// MIDI channel, 0-based index (`MidiChannel::as_index`).
        channel: u8,
        /// Note number (0-127).
        note: u8,
        /// Velocity (0-127).
        velocity: u8,
        /// Sample offset within the next processed block.
        sample_offset: i32,
    },
    /// Release a note previously started with [`HostCommand::NoteOn`].
    NoteOff {
        /// Raw note id returned by `NoteOn`.
        note_id: i32,
        /// Sample offset within the next processed block.
        sample_offset: i32,
    },
    /// Send a per-note expression value (normalized 0..1) for a voice. The expression
    /// dimension crosses the boundary as the serializable `NoteExpressionType` enum.
    SendNoteExpression {
        /// Raw note id returned by `NoteOn`.
        note_id: i32,
        /// Which note-expression dimension to set.
        kind: crate::midi::NoteExpressionType,
        /// Normalized expression value (0..1).
        value: f64,
        /// Sample offset within the next processed block.
        sample_offset: i32,
    },
    /// Enumerate the per-note expressions the plugin advertises (`INoteExpressionController`).
    NoteExpressions {
        /// Event bus index.
        bus: i32,
        /// Channel index.
        channel: i16,
    },
    /// Select a program in a unit's program list (`IUnitInfo`).
    SelectProgram {
        /// Unit id (the root unit is `0`).
        unit_id: i32,
        /// 0-based index into the unit's program list.
        program_index: i32,
    },
    /// Activate or deactivate a single bus (`IComponent::activateBus`).
    SetBusActive {
        /// Whether the bus carries audio or events.
        media_type: crate::audio::MediaType,
        /// Whether the bus is an input or an output.
        direction: crate::audio::BusDirection,
        /// 0-based bus index within its `(media_type, direction)` group.
        bus_index: i32,
        /// `true` to activate, `false` to deactivate.
        active: bool,
    },
    /// Drain the ordered parameter-edit gesture log (begin/change/end) the helper's plugin has
    /// accumulated from its editor since the last poll.
    TakeParameterEdits,
    /// Shutdown the helper process
    Shutdown,
}

/// Responses from the isolated plugin process
#[derive(Debug, Serialize, Deserialize)]
pub enum HostResponse {
    /// Operation succeeded with message
    Success {
        /// Human-readable success detail.
        message: String,
    },
    /// Operation failed with error
    Error {
        /// Error detail.
        message: String,
    },
    /// Plugin crashed
    Crashed {
        /// Crash detail.
        message: String,
    },
    /// Per-channel audio output data (`[channel][frame]`), plus any MIDI the plugin
    /// emitted during the block (arpeggiators, MPE, etc.).
    AudioOutput {
        /// Output samples per channel.
        outputs: Vec<Vec<f32>>,
        /// MIDI events the plugin emitted this block, in order.
        output_midi: Vec<crate::midi::MidiEvent>,
    },
    /// A single parameter value (normalized).
    ParameterValue {
        /// Normalized value.
        value: f64,
    },
    /// A formatted parameter display string.
    ParameterString {
        /// The plugin-rendered display string.
        value: String,
    },
    /// A list of parameters.
    Parameters {
        /// All parameters reported by the plugin.
        params: Vec<crate::parameters::Parameter>,
    },
    /// Opaque plugin state bytes (reply to `SaveState`).
    State {
        /// The serialized state.
        data: Vec<u8>,
    },
    /// The isolated editor window was created (reply to `CreateGui`); carries the
    /// plugin-reported editor size so the host can report it without a second round-trip.
    GuiCreated {
        /// Editor width in pixels.
        width: i32,
        /// Editor height in pixels.
        height: i32,
    },
    /// Plugin information
    PluginInfo {
        /// Vendor / manufacturer.
        vendor: String,
        /// Plugin name.
        name: String,
        /// Version string (may be empty if the plugin doesn't report one).
        version: String,
        /// Plugin sub-categories (e.g. "Fx", "Instrument|Synth"); may be empty.
        category: String,
        /// Unique plugin class id (hex).
        uid: String,
        /// Whether the plugin has an editor.
        has_gui: bool,
        /// Audio input bus count.
        audio_inputs: i32,
        /// Audio output bus count.
        audio_outputs: i32,
        /// Total output audio channels across all output buses.
        output_channels: i32,
        /// Whether the plugin has a MIDI/event input bus.
        has_midi_input: bool,
        /// Whether the plugin has a MIDI/event output bus.
        has_midi_output: bool,
    },
    /// A note was started (reply to `NoteOn`); carries the helper-allocated raw note id.
    NoteStarted {
        /// Raw note id the host wraps back into a `NoteId`.
        note_id: i32,
    },
    /// The per-note expressions the plugin advertises (reply to `NoteExpressions`).
    NoteExpressions {
        /// The advertised note-expression dimensions.
        expressions: Vec<crate::midi::NoteExpressionInfo>,
    },
    /// The ordered parameter-edit gestures drained from the helper (reply to
    /// `TakeParameterEdits`).
    ParameterEdits {
        /// The gesture events, in the order the plugin's editor reported them.
        edits: Vec<crate::plugin::ParameterEdit>,
    },
}

/// Manages a plugin running in an isolated process.
///
/// Responses are read on a background thread and delivered over a channel, so
/// [`Self::send_command`] can wait with a deadline: a hung plugin yields a timeout
/// error (and the child is killed) instead of blocking the host forever, and a
/// crashed helper surfaces as a disconnect error rather than a silent wedge.
pub struct PluginHostProcess {
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    /// Lines received from the helper's stdout (one JSON response each).
    responses: Receiver<String>,
    /// Background reader thread handle (joined on shutdown).
    reader: Option<JoinHandle<()>>,
    /// How long to wait for a single response before declaring a timeout.
    timeout: Duration,
    /// Set once the child has been killed/exited so we stop trying to talk to it.
    dead: bool,
}

impl PluginHostProcess {
    /// Create a new isolated plugin host process
    pub fn new(
        helper_override: Option<std::path::PathBuf>,
        timeout: Duration,
    ) -> Result<Self, String> {
        // An explicit helper path (builder option or the VST3_HOST_HELPER_PATH env var) wins
        // over the heuristic search below — and a missing one is reported clearly here.
        let override_path = helper_override
            .or_else(|| std::env::var_os("VST3_HOST_HELPER_PATH").map(std::path::PathBuf::from));
        if let Some(p) = override_path {
            if !p.exists() {
                return Err(format!(
                    "Configured helper path does not exist: {}",
                    p.display()
                ));
            }
            return Self::spawn(p, timeout);
        }

        // Get the path to our helper executable
        let exe_path =
            std::env::current_exe().map_err(|e| format!("Failed to get current exe: {}", e))?;

        let exe_dir = exe_path.parent().ok_or("Failed to get exe directory")?;

        // Try different possible helper names and locations
        let helper_names = ["vst3-host-helper", "vst3-inspector-helper"];
        let mut helper_path = None;

        // First try in the same directory as the executable
        for name in &helper_names {
            let path = exe_dir.join(name);
            if path.exists() {
                helper_path = Some(path);
                break;
            }
        }

        // If not found and we're in an examples directory, try parent
        if helper_path.is_none() && exe_dir.file_name() == Some(std::ffi::OsStr::new("examples")) {
            if let Some(parent_dir) = exe_dir.parent() {
                for name in &helper_names {
                    let path = parent_dir.join(name);
                    if path.exists() {
                        helper_path = Some(path);
                        break;
                    }
                }
            }
        }

        // Also check common cargo target directories
        if helper_path.is_none() {
            // Try to find the workspace root and look in target/debug or target/release
            let mut current_dir = exe_dir;
            while let Some(parent) = current_dir.parent() {
                let debug_path = parent.join("target").join("debug").join("vst3-host-helper");
                let release_path = parent
                    .join("target")
                    .join("release")
                    .join("vst3-host-helper");

                if debug_path.exists() {
                    helper_path = Some(debug_path);
                    break;
                } else if release_path.exists() {
                    helper_path = Some(release_path);
                    break;
                }

                // Check if we've reached a Cargo.toml (workspace root)
                if parent.join("Cargo.toml").exists() {
                    break;
                }
                current_dir = parent;
            }
        }

        let helper_path = helper_path
            .ok_or_else(|| format!("Helper executable not found. Searched in {:?} and parent directories. Make sure to build with --bins flag.", exe_dir))?;

        Self::spawn(helper_path, timeout)
    }

    /// Spawn the helper at `helper_path` and wire up the response reader thread.
    fn spawn(helper_path: std::path::PathBuf, timeout: Duration) -> Result<Self, String> {
        let mut child = Command::new(&helper_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Failed to spawn helper process: {}", e))?;

        let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;

        // Read responses on a background thread so the caller can apply a deadline.
        // The thread ends (dropping the sender) when stdout hits EOF — i.e. when the
        // helper process exits or crashes — which the receiver sees as Disconnected.
        let (tx, rx) = mpsc::channel::<String>();
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break, // EOF: helper exited
                    Ok(_) => {
                        if tx.send(std::mem::take(&mut line)).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            process: Some(child),
            stdin: Some(stdin),
            responses: rx,
            reader: Some(reader),
            timeout,
            dead: false,
        })
    }

    /// Set how long to wait for a helper response before declaring a timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Send a command to the helper process and wait (with a deadline) for a response.
    ///
    /// Returns an error without blocking indefinitely if the plugin hangs (the child
    /// is killed) or the helper has crashed/exited.
    pub fn send_command(&mut self, command: HostCommand) -> Result<HostResponse, String> {
        if self.dead {
            return Err("Helper process is no longer running".to_string());
        }

        let command_json = serde_json::to_string(&command)
            .map_err(|e| format!("Failed to serialize command: {}", e))?;

        {
            let stdin = self.stdin.as_mut().ok_or("No stdin available")?;
            writeln!(stdin, "{}", command_json).map_err(|e| {
                self.dead = true;
                format!("Failed to write command (helper gone?): {}", e)
            })?;
            stdin.flush().map_err(|e| {
                self.dead = true;
                format!("Failed to flush stdin (helper gone?): {}", e)
            })?;
        }

        match self.responses.recv_timeout(self.timeout) {
            Ok(line) => {
                serde_json::from_str(&line).map_err(|e| format!("Failed to parse response: {}", e))
            }
            Err(RecvTimeoutError::Timeout) => {
                // The plugin is hung. Kill the child so it can't wedge us further.
                self.dead = true;
                if let Some(ref mut process) = self.process {
                    let _ = process.kill();
                }
                Err(format!(
                    "Timed out after {:?} waiting for helper response (plugin may have hung)",
                    self.timeout
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Reader thread ended => stdout closed => helper exited/crashed.
                self.dead = true;
                match self.check_process_status() {
                    Err(status) => Err(format!("Helper process crashed: {}", status)),
                    Ok(()) => Err("Helper process exited unexpectedly".to_string()),
                }
            }
        }
    }

    /// Whether the helper process is still considered alive.
    pub fn is_alive(&self) -> bool {
        !self.dead
    }

    /// OS process id of the running helper, if any. Useful for monitoring — and for tests
    /// that need to simulate a crash by killing the helper.
    pub fn helper_pid(&self) -> Option<u32> {
        self.process.as_ref().map(|c| c.id())
    }

    /// Check if the helper process is still running
    pub fn check_process_status(&mut self) -> Result<(), String> {
        if let Some(ref mut process) = self.process {
            match process.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return Err(format!("Helper process exited with status: {}", status));
                    }
                }
                Ok(None) => {
                    // Still running
                    return Ok(());
                }
                Err(e) => {
                    return Err(format!("Failed to check process status: {}", e));
                }
            }
        }
        Ok(())
    }

    /// Shutdown the helper process
    pub fn shutdown(&mut self) {
        // Best-effort Shutdown command (no response expected — the helper just exits).
        // We do NOT use send_command here: it waits for a reply, and Shutdown has none.
        if !self.dead {
            if let (Some(stdin), Ok(json)) = (
                self.stdin.as_mut(),
                serde_json::to_string(&HostCommand::Shutdown),
            ) {
                let _ = writeln!(stdin, "{}", json);
                let _ = stdin.flush();
            }
        }

        // Dropping stdin gives the helper's read loop EOF, guaranteeing it exits even
        // if it ignored the Shutdown command; that in turn ends the reader thread.
        self.stdin = None;

        if let Some(mut process) = self.process.take() {
            // Bounded wait, then SIGKILL: this runs from Drop, so a wedged helper must not
            // be able to hang the host on exit. Poll for a clean exit up to a deadline, then
            // force-kill (mirrors the kill-on-timeout pattern in send_command).
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                match process.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() >= deadline => {
                        let _ = process.kill();
                        let _ = process.wait();
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                    Err(_) => {
                        let _ = process.kill();
                        break;
                    }
                }
            }
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        self.dead = true;
    }
}

impl Drop for PluginHostProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Result type for process isolation operations
pub type IsolationResult<T> = std::result::Result<T, IsolationError>;

/// Errors that can occur during process isolation
#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Plugin error
    #[error("Plugin error: {0}")]
    Plugin(String),

    /// Plugin crashed
    #[error("Plugin crashed: {0}")]
    Crashed(String),

    /// Helper process not running
    #[error("Helper process not running")]
    NotRunning,

    /// Unexpected response
    #[error("Unexpected response from helper")]
    UnexpectedResponse,
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use crate::midi::{MidiChannel, MidiEvent};

    #[test]
    fn audio_output_carries_midi_across_the_wire() {
        // The Process response carries emitted MIDI alongside audio; check the variant
        // round-trips through the JSON transport host and helper share.
        let resp = HostResponse::AudioOutput {
            outputs: vec![vec![0.0, 0.5], vec![-0.5, 0.0]],
            output_midi: vec![
                MidiEvent::NoteOn {
                    channel: MidiChannel::Ch1,
                    note: 60,
                    velocity: 100,
                },
                MidiEvent::NoteOff {
                    channel: MidiChannel::Ch1,
                    note: 60,
                    velocity: 0,
                },
            ],
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        let back: HostResponse = serde_json::from_str(&json).expect("deserialize");
        match back {
            HostResponse::AudioOutput {
                outputs,
                output_midi,
            } => {
                assert_eq!(outputs, vec![vec![0.0, 0.5], vec![-0.5, 0.0]]);
                assert_eq!(output_midi.len(), 2);
                assert_eq!(
                    output_midi[0],
                    MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 60,
                        velocity: 100
                    }
                );
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn state_commands_round_trip_across_the_wire() {
        // SaveState/LoadState/State carry the opaque plugin state blob across isolation.
        let blob: Vec<u8> = vec![0, 1, 2, 250, 255, 42];

        let save = serde_json::to_string(&HostCommand::SaveState).expect("serialize SaveState");
        assert!(matches!(
            serde_json::from_str::<HostCommand>(&save).expect("deserialize SaveState"),
            HostCommand::SaveState
        ));

        let load = HostCommand::LoadState { data: blob.clone() };
        let load_json = serde_json::to_string(&load).expect("serialize LoadState");
        match serde_json::from_str::<HostCommand>(&load_json).expect("deserialize LoadState") {
            HostCommand::LoadState { data } => assert_eq!(data, blob),
            other => panic!("LoadState round-trip changed the variant: {other:?}"),
        }

        let state = HostResponse::State { data: blob.clone() };
        let state_json = serde_json::to_string(&state).expect("serialize State");
        match serde_json::from_str::<HostResponse>(&state_json).expect("deserialize State") {
            HostResponse::State { data } => assert_eq!(data, blob),
            other => panic!("State round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn set_parameter_at_round_trips_across_the_wire() {
        // The sample-accurate automation command must survive the JSON transport intact
        // (the offset is carried across the isolation boundary).
        let cmd = HostCommand::SetParameterAt {
            id: 42,
            value: 0.75,
            offset: 256,
        };
        let json = serde_json::to_string(&cmd).expect("serialize SetParameterAt");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetParameterAt") {
            HostCommand::SetParameterAt { id, value, offset } => {
                assert_eq!(id, 42);
                assert_eq!(value, 0.75);
                assert_eq!(offset, 256);
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn scheduled_midi_offset_ipc_round_trips() {
        // Sample-accurate MIDI must carry its offset across the isolation boundary, so isolated
        // playback schedules the event in the same block position as the in-process path.
        use crate::midi::{MidiChannel, MidiEvent};
        let cmd = HostCommand::SendMidiAt {
            event: MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 100,
            },
            sample_offset: 256,
        };
        let json = serde_json::to_string(&cmd).expect("serialize SendMidiAt");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SendMidiAt") {
            HostCommand::SendMidiAt {
                event,
                sample_offset,
            } => {
                assert_eq!(
                    event,
                    MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 60,
                        velocity: 100
                    }
                );
                assert_eq!(sample_offset, 256);
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn select_program_round_trips_across_the_wire() {
        // Program selection must survive the JSON transport host and helper share.
        let cmd = HostCommand::SelectProgram {
            unit_id: 0,
            program_index: 17,
        };
        let json = serde_json::to_string(&cmd).expect("serialize SelectProgram");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SelectProgram") {
            HostCommand::SelectProgram {
                unit_id,
                program_index,
            } => {
                assert_eq!(unit_id, 0);
                assert_eq!(program_index, 17);
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn transport_commands_round_trip_across_the_wire() {
        // The runtime transport mutations must survive the JSON transport intact so the helper
        // applies the same change the host requested.
        let tempo = HostCommand::SetTempo { bpm: 137.5 };
        let json = serde_json::to_string(&tempo).expect("serialize SetTempo");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetTempo") {
            HostCommand::SetTempo { bpm } => assert_eq!(bpm, 137.5),
            other => panic!("round-trip changed the variant: {other:?}"),
        }

        let ts = HostCommand::SetTimeSignature {
            numerator: 7,
            denominator: 8,
        };
        let json = serde_json::to_string(&ts).expect("serialize SetTimeSignature");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetTimeSignature") {
            HostCommand::SetTimeSignature {
                numerator,
                denominator,
            } => assert_eq!((numerator, denominator), (7, 8)),
            other => panic!("round-trip changed the variant: {other:?}"),
        }

        let playing = HostCommand::SetPlaying { playing: false };
        let json = serde_json::to_string(&playing).expect("serialize SetPlaying");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetPlaying") {
            HostCommand::SetPlaying { playing } => assert!(!playing),
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn set_bus_active_round_trips_across_the_wire() {
        use crate::audio::{BusDirection, MediaType};
        let cmd = HostCommand::SetBusActive {
            media_type: MediaType::Audio,
            direction: BusDirection::Input,
            bus_index: 1,
            active: true,
        };
        let json = serde_json::to_string(&cmd).expect("serialize SetBusActive");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetBusActive") {
            HostCommand::SetBusActive {
                media_type,
                direction,
                bus_index,
                active,
            } => {
                assert_eq!(media_type, MediaType::Audio);
                assert_eq!(direction, BusDirection::Input);
                assert_eq!(bus_index, 1);
                assert!(active);
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn parameter_edits_round_trip_across_the_wire() {
        // The ordered gesture log must survive the JSON transport host and helper share, both
        // the empty command and the populated reply.
        use crate::plugin::{ParameterEdit, ParameterEditKind};

        let cmd = serde_json::to_string(&HostCommand::TakeParameterEdits)
            .expect("serialize TakeParameterEdits");
        assert!(matches!(
            serde_json::from_str::<HostCommand>(&cmd).expect("deserialize TakeParameterEdits"),
            HostCommand::TakeParameterEdits
        ));

        let edits = vec![
            ParameterEdit {
                id: 9,
                kind: ParameterEditKind::BeginGesture,
                value: None,
            },
            ParameterEdit {
                id: 9,
                kind: ParameterEditKind::ValueChange,
                value: Some(0.3),
            },
            ParameterEdit {
                id: 9,
                kind: ParameterEditKind::EndGesture,
                value: None,
            },
        ];
        let resp = HostResponse::ParameterEdits {
            edits: edits.clone(),
        };
        let resp_json = serde_json::to_string(&resp).expect("serialize ParameterEdits");
        match serde_json::from_str::<HostResponse>(&resp_json).expect("deserialize ParameterEdits")
        {
            HostResponse::ParameterEdits { edits: back } => assert_eq!(back, edits),
            other => panic!("ParameterEdits round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn note_expression_commands_round_trip_across_the_wire() {
        // The MPE commands/responses must survive the JSON transport host and helper share.
        use crate::midi::{NoteExpressionInfo, NoteExpressionType};

        let on = HostCommand::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
            sample_offset: 0,
        };
        let on_json = serde_json::to_string(&on).expect("serialize NoteOn");
        match serde_json::from_str::<HostCommand>(&on_json).expect("deserialize NoteOn") {
            HostCommand::NoteOn {
                channel,
                note,
                velocity,
                sample_offset,
            } => {
                assert_eq!((channel, note, velocity, sample_offset), (0, 60, 100, 0));
            }
            other => panic!("NoteOn round-trip changed the variant: {other:?}"),
        }

        let expr = HostCommand::SendNoteExpression {
            note_id: 7,
            kind: NoteExpressionType::Tuning,
            value: 1.0,
            sample_offset: 0,
        };
        let expr_json = serde_json::to_string(&expr).expect("serialize SendNoteExpression");
        match serde_json::from_str::<HostCommand>(&expr_json).expect("deserialize") {
            HostCommand::SendNoteExpression {
                note_id,
                kind,
                value,
                ..
            } => {
                assert_eq!(note_id, 7);
                assert_eq!(kind, NoteExpressionType::Tuning);
                assert_eq!(value, 1.0);
            }
            other => panic!("SendNoteExpression round-trip changed the variant: {other:?}"),
        }

        let started = HostResponse::NoteStarted { note_id: 42 };
        let started_json = serde_json::to_string(&started).expect("serialize NoteStarted");
        match serde_json::from_str::<HostResponse>(&started_json).expect("deserialize") {
            HostResponse::NoteStarted { note_id } => assert_eq!(note_id, 42),
            other => panic!("NoteStarted round-trip changed the variant: {other:?}"),
        }

        let info = NoteExpressionInfo {
            kind: NoteExpressionType::Tuning,
            title: "Tuning".to_string(),
            short_title: "Tun".to_string(),
            units: String::new(),
            default_value: 0.5,
            min: 0.0,
            max: 1.0,
            step_count: 0,
            is_bipolar: true,
            is_one_shot: false,
            is_absolute: false,
        };
        let resp = HostResponse::NoteExpressions {
            expressions: vec![info.clone()],
        };
        let resp_json = serde_json::to_string(&resp).expect("serialize NoteExpressions");
        match serde_json::from_str::<HostResponse>(&resp_json).expect("deserialize") {
            HostResponse::NoteExpressions { expressions } => {
                assert_eq!(expressions, vec![info]);
            }
            other => panic!("NoteExpressions round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn explicit_helper_override_missing_path_reports_clearly() {
        // An explicit helper path that doesn't exist must fail with a clear, path-naming
        // error *before* spawning — not fall through to the heuristic search. This is the
        // observable contract for the builder's `helper_path()` override (roadmap 3.3).
        let bogus = std::path::PathBuf::from("/nonexistent/vst3-host-helper-xyz");
        let err = match PluginHostProcess::new(Some(bogus.clone()), DEFAULT_RESPONSE_TIMEOUT) {
            Ok(_) => panic!("a missing override path must error, not spawn"),
            Err(e) => e,
        };
        assert!(
            err.contains("does not exist"),
            "error should explain the missing path, got: {err}"
        );
        assert!(
            err.contains("vst3-host-helper-xyz"),
            "error should name the offending path, got: {err}"
        );
    }

    /// A helper that never responds (a hung plugin) must not hang the host: `send_command`
    /// returns an error within the timeout and kills the child.
    #[cfg(unix)]
    #[test]
    fn hung_helper_times_out_and_is_killed_not_blocking() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        // Fake helper: read nothing, write nothing, just sleep — i.e. hang forever.
        let dir = std::env::temp_dir().join(format!("vst3_hang_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("hung-helper");
        let mut f = std::fs::File::create(&fake).unwrap();
        // `exec` so the shell is replaced by sleep (no orphaned child holding the stdout
        // pipe); killing the helper then closes the pipe and ends the reader thread promptly.
        writeln!(f, "#!/bin/sh\nexec sleep 30").unwrap();
        drop(f);
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut proc =
            PluginHostProcess::spawn(fake.clone(), Duration::from_millis(200)).expect("spawn");
        let started = Instant::now();
        let res = proc.send_command(HostCommand::Shutdown);
        let elapsed = started.elapsed();

        assert!(
            res.is_err(),
            "a hung helper must yield an error, got {res:?}"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "send_command must return promptly on timeout, took {elapsed:?}"
        );
        // The child was killed; a follow-up command also errors rather than hanging.
        assert!(proc.send_command(HostCommand::Shutdown).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Crash protection utilities for in-process plugins
pub mod crash_protection {
    use std::panic::catch_unwind;
    use std::panic::UnwindSafe;
    use std::time::Duration;

    /// Status of a plugin after a protected call
    #[derive(Debug, Clone, PartialEq)]
    pub enum PluginStatus {
        /// Plugin executed successfully
        Ok,
        /// Plugin crashed with panic
        Crashed(String),
        /// Plugin took too long to execute
        Timeout(Duration),
    }

    /// Execute a function with panic protection
    pub fn protected_call<F, R>(f: F) -> Result<R, String>
    where
        F: FnOnce() -> R + UnwindSafe,
    {
        catch_unwind(f).map_err(|e| {
            if let Some(s) = e.downcast_ref::<&str>() {
                format!("Plugin panicked: {}", s)
            } else if let Some(s) = e.downcast_ref::<String>() {
                format!("Plugin panicked: {}", s)
            } else {
                "Plugin panicked with unknown error".to_string()
            }
        })
    }
}
