//! Process isolation for VST3 plugin hosting
//!
//! This module provides functionality to run VST3 plugins in separate processes
//! for improved stability and crash protection.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

/// Commands that can be sent to the isolated plugin process
#[derive(Debug, Serialize, Deserialize)]
pub enum HostCommand {
    /// Load a plugin from the specified path
    LoadPlugin { path: String },
    /// Unload the current plugin
    UnloadPlugin,
    /// Create plugin GUI
    CreateGui,
    /// Close plugin GUI
    CloseGui,
    /// Process audio buffers
    Process { audio_data: Vec<f32> },
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
    /// Audio output data
    AudioOutput { data: Vec<f32> },
    /// Plugin information
    PluginInfo {
        vendor: String,
        name: String,
        version: String,
        has_gui: bool,
        audio_inputs: i32,
        audio_outputs: i32,
    },
}

/// Manages a plugin running in an isolated process
pub struct PluginHostProcess {
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<std::process::ChildStdout>>,
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

        Ok(Self {
            process: Some(child),
            stdin: Some(stdin),
            stdout: Some(BufReader::new(stdout)),
        })
    }

    /// Send a command to the helper process and get a response
    pub fn send_command(&mut self, command: HostCommand) -> Result<HostResponse, String> {
        let stdin = self.stdin.as_mut().ok_or("No stdin available")?;

        let stdout = self.stdout.as_mut().ok_or("No stdout available")?;

        // Send command
        let command_json = serde_json::to_string(&command)
            .map_err(|e| format!("Failed to serialize command: {}", e))?;

        writeln!(stdin, "{}", command_json)
            .map_err(|e| format!("Failed to write command: {}", e))?;

        stdin
            .flush()
            .map_err(|e| format!("Failed to flush stdin: {}", e))?;

        // Read response with timeout
        let mut response_line = String::new();
        stdout
            .read_line(&mut response_line)
            .map_err(|e| format!("Failed to read response: {}", e))?;

        if response_line.is_empty() {
            // Process might have crashed
            self.check_process_status()?;
            return Err("No response from helper process".to_string());
        }

        serde_json::from_str(&response_line).map_err(|e| format!("Failed to parse response: {}", e))
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
        // Send shutdown command
        let _ = self.send_command(HostCommand::Shutdown);

        // Wait for process to exit
        if let Some(mut process) = self.process.take() {
            let _ = process.wait();
        }

        self.stdin = None;
        self.stdout = None;
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
