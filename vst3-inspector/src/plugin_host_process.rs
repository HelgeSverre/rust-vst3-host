use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[derive(Debug, Serialize, Deserialize)]
pub enum HostCommand {
    LoadPlugin { path: String },
    UnloadPlugin,
    CreateGui,
    CloseGui,
    Process { audio_data: Vec<f32> },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum HostResponse {
    Success {
        message: String,
    },
    Error {
        message: String,
    },
    Crashed {
        message: String,
    },
    AudioOutput {
        data: Vec<f32>,
    },
    PluginInfo {
        vendor: String,
        name: String,
        version: String,
        has_gui: bool,
        audio_inputs: i32,
        audio_outputs: i32,
    },
}

pub struct PluginHostProcess {
    process: Option<std::process::Child>,
    stdin: Option<std::process::ChildStdin>,
    stdout: Option<BufReader<std::process::ChildStdout>>,
}

impl PluginHostProcess {
    pub fn new() -> Result<Self, String> {
        // Get the path to our helper executable
        let exe_path =
            std::env::current_exe().map_err(|e| format!("Failed to get current exe: {}", e))?;

        let exe_dir = exe_path.parent().ok_or("Failed to get exe directory")?;

        let helper_path = exe_dir.join("vst3-inspector-helper");

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
