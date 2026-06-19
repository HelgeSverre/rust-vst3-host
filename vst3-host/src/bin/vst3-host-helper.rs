//! VST3 Host Helper Process
//!
//! Runs a single VST3 plugin in isolation from the main process. It is intentionally
//! a thin wrapper around the library's own (in-process) public API: every command
//! delegates to a real [`vst3_host::Plugin`], so the isolated path reuses exactly the
//! same, verified plugin handling as the non-isolated path — there is no separate
//! VST3 implementation to drift out of sync.
//!
//! The protocol enums are imported from the library (`vst3_host::process_isolation`),
//! so host and helper can never disagree about the wire format.

use std::io::{self, BufRead, Write};

use vst3_host::{
    audio::AudioBuffers,
    process_isolation::{HostCommand, HostResponse},
    Plugin, Vst3Host,
};

fn main() {
    eprintln!("VST3 Host Helper Process Started");

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut plugin: Option<Plugin> = None;
    // Config carried by LoadPlugin so process() can size buffers correctly.
    let mut sample_rate = 44100.0;

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Failed to read line: {}", e);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let command: HostCommand = match serde_json::from_str(&line) {
            Ok(cmd) => cmd,
            Err(e) => {
                respond(
                    &mut stdout,
                    &HostResponse::Error {
                        message: format!("Invalid command: {}", e),
                    },
                );
                continue;
            }
        };

        if matches!(command, HostCommand::Shutdown) {
            eprintln!("Shutting down helper process");
            break;
        }

        let response = handle(command, &mut plugin, &mut sample_rate);
        respond(&mut stdout, &response);
    }
}

fn respond(stdout: &mut io::Stdout, response: &HostResponse) {
    if let Ok(json) = serde_json::to_string(response) {
        let _ = writeln!(stdout, "{}", json);
        let _ = stdout.flush();
    }
}

/// Convenience: run a fallible op against the loaded plugin, mapping `None`/`Err`.
fn with_plugin<F>(plugin: &mut Option<Plugin>, f: F) -> HostResponse
where
    F: FnOnce(&mut Plugin) -> HostResponse,
{
    match plugin {
        Some(p) => f(p),
        None => HostResponse::Error {
            message: "No plugin loaded".to_string(),
        },
    }
}

fn err<E: std::fmt::Display>(prefix: &str, e: E) -> HostResponse {
    HostResponse::Error {
        message: format!("{prefix}: {e}"),
    }
}

fn handle(
    command: HostCommand,
    plugin: &mut Option<Plugin>,
    sample_rate: &mut f64,
) -> HostResponse {
    match command {
        HostCommand::LoadPlugin {
            path,
            sample_rate: sr,
            block_size,
        } => {
            *sample_rate = sr;
            let mut host = match Vst3Host::builder()
                .sample_rate(sr)
                .block_size(block_size as usize)
                .build()
            {
                Ok(h) => h,
                Err(e) => return err("Failed to build host", e),
            };
            match host.load_plugin(&path) {
                Ok(p) => {
                    let info = p.info().clone();
                    *plugin = Some(p);
                    HostResponse::PluginInfo {
                        vendor: info.vendor,
                        name: info.name,
                        version: info.version,
                        has_gui: info.has_gui,
                        audio_inputs: info.audio_inputs as i32,
                        audio_outputs: info.audio_outputs as i32,
                    }
                }
                Err(e) => err("Failed to load plugin", e),
            }
        }
        HostCommand::UnloadPlugin => {
            *plugin = None;
            HostResponse::Success {
                message: "Plugin unloaded".to_string(),
            }
        }
        HostCommand::StartProcessing => with_plugin(plugin, |p| match p.start_processing() {
            Ok(()) => HostResponse::Success {
                message: "processing started".to_string(),
            },
            Err(e) => err("StartProcessing", e),
        }),
        HostCommand::StopProcessing => with_plugin(plugin, |p| match p.stop_processing() {
            Ok(()) => HostResponse::Success {
                message: "processing stopped".to_string(),
            },
            Err(e) => err("StopProcessing", e),
        }),
        HostCommand::SetParameter { id, value } => {
            with_plugin(plugin, |p| match p.set_parameter(id, value) {
                Ok(()) => HostResponse::Success {
                    message: "parameter set".to_string(),
                },
                Err(e) => err("SetParameter", e),
            })
        }
        HostCommand::GetParameter { id } => with_plugin(plugin, |p| match p.get_parameter(id) {
            Ok(value) => HostResponse::ParameterValue { value },
            Err(e) => err("GetParameter", e),
        }),
        HostCommand::GetAllParameters => with_plugin(plugin, |p| match p.get_parameters() {
            Ok(params) => HostResponse::Parameters { params },
            Err(e) => err("GetAllParameters", e),
        }),
        HostCommand::FormatParameter { id, normalized } => {
            with_plugin(plugin, |p| match p.format_parameter(id, normalized) {
                Ok(value) => HostResponse::ParameterString { value },
                Err(e) => err("FormatParameter", e),
            })
        }
        HostCommand::SendMidi { event } => {
            with_plugin(plugin, |p| match p.send_midi_event(event) {
                Ok(()) => HostResponse::Success {
                    message: "midi sent".to_string(),
                },
                Err(e) => err("SendMidi", e),
            })
        }
        HostCommand::Process { inputs, frames } => {
            let sr = *sample_rate;
            with_plugin(plugin, |p| {
                let out_channels = p.info().audio_outputs.max(1) as usize * 2;
                let mut buffers = AudioBuffers {
                    inputs,
                    outputs: vec![vec![0.0; frames as usize]; out_channels],
                    sample_rate: sr,
                    block_size: frames as usize,
                };
                match p.process_audio(&mut buffers) {
                    Ok(()) => HostResponse::AudioOutput {
                        outputs: buffers.outputs,
                    },
                    Err(e) => err("Process", e),
                }
            })
        }
        HostCommand::SaveState => with_plugin(plugin, |p| match p.save_state() {
            Ok(data) => HostResponse::State { data },
            Err(e) => err("SaveState", e),
        }),
        HostCommand::LoadState { data } => with_plugin(plugin, |p| match p.load_state(&data) {
            Ok(()) => HostResponse::Success {
                message: "state restored".to_string(),
            },
            Err(e) => err("LoadState", e),
        }),
        HostCommand::CreateGui | HostCommand::CloseGui => HostResponse::Error {
            message: "Plugin GUI is not supported across process isolation yet".to_string(),
        },
        HostCommand::Shutdown => HostResponse::Success {
            message: "shutting down".to_string(),
        },
    }
}
