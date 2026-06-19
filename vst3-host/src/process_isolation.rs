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
const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Commands that can be sent to the isolated plugin process.
///
/// This enum is the single source of truth for the isolation IPC protocol — the
/// helper binary imports it from here rather than redefining it, so the two halves
/// can never drift apart.
#[derive(Debug, Serialize, Deserialize)]
pub enum HostCommand {
    /// Load a plugin from the specified path, configured for the given audio settings.
    LoadPlugin {
        /// Path to the `.vst3` bundle.
        path: String,
        /// Sample rate to configure the plugin for.
        sample_rate: f64,
        /// Block size to configure the plugin for.
        block_size: u32,
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
    /// Process one block of audio. `inputs` is per-channel; `frames` is the block length.
    Process {
        /// Per-channel input samples (`[channel][frame]`).
        inputs: Vec<Vec<f32>>,
        /// Number of frames in this block.
        frames: u32,
    },
    /// Shutdown the helper process
    Shutdown,
}

/// Responses from the isolated plugin process
#[derive(Debug, Serialize, Deserialize)]
pub enum HostResponse {
    /// Operation succeeded with message
    Success { message: String },
    /// Operation failed with error
    Error { message: String },
    /// Plugin crashed
    Crashed { message: String },
    /// Per-channel audio output data (`[channel][frame]`).
    AudioOutput {
        /// Output samples per channel.
        outputs: Vec<Vec<f32>>,
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
    /// Plugin information
    PluginInfo {
        /// Vendor / manufacturer.
        vendor: String,
        /// Plugin name.
        name: String,
        /// Version string.
        version: String,
        /// Whether the plugin has an editor.
        has_gui: bool,
        /// Audio input bus count.
        audio_inputs: i32,
        /// Audio output bus count.
        audio_outputs: i32,
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
    pub fn new() -> Result<Self, String> {
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

        // Start the helper process
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
            timeout: DEFAULT_RESPONSE_TIMEOUT,
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
            Ok(line) => serde_json::from_str(&line)
                .map_err(|e| format!("Failed to parse response: {}", e)),
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
            if let (Some(stdin), Ok(json)) =
                (self.stdin.as_mut(), serde_json::to_string(&HostCommand::Shutdown))
            {
                let _ = writeln!(stdin, "{}", json);
                let _ = stdin.flush();
            }
        }

        // Dropping stdin gives the helper's read loop EOF, guaranteeing it exits even
        // if it ignored the Shutdown command; that in turn ends the reader thread.
        self.stdin = None;

        if let Some(mut process) = self.process.take() {
            let _ = process.wait();
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
