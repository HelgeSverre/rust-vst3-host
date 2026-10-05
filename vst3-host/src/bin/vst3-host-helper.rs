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
//!
//! ## Threading (macOS)
//!
//! The main thread owns controller, lifecycle and native editor operations. A worker
//! reads commands and executes audio processing, forwarding all other commands to the
//! main event loop. This keeps controller calls serialized with native UI callbacks.
//! Other platforms process commands on the main stdin thread (GUI is unsupported).

use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

use vst3_host::{
    audio::AudioBuffers,
    process_isolation::{HostCommand, HostResponse, ProtocolChannel},
    Plugin, Vst3Host,
};

/// Loaded plugin shared between the command worker and (on macOS) the UI main thread.
type SharedPlugin = Arc<Mutex<Option<Plugin>>>;

fn main() {
    // Before anything else — certainly before a plugin binary is loaded and can run its own
    // code — take the protocol channel away from stdout. A hosted plugin shares this
    // process's descriptors and third-party plugins do print; on the shared stdout a single
    // `printf` line would be read by the host as a response and desynchronise every later
    // command from its reply.
    let protocol = ProtocolChannel::claim();

    eprintln!("VST3 Host Helper Process Started");

    let plugin: SharedPlugin = Arc::new(Mutex::new(None));

    #[cfg(target_os = "macos")]
    {
        macos::run(plugin, protocol);
    }

    #[cfg(not(target_os = "macos"))]
    {
        // No UI run loop needed: process commands on this (main) thread directly.
        let mut protocol = protocol;
        let stdin = io::stdin();
        let mut sample_rate = 44100.0;
        for line in stdin.lock().lines() {
            let Some(command) = parse_line(line, &mut protocol) else {
                continue;
            };
            if matches!(command, HostCommand::Shutdown) {
                eprintln!("Shutting down helper process");
                break;
            }
            let response = handle(command, &plugin, &mut sample_rate);
            respond(&mut protocol, &response);
        }
    }
}

/// Parse one stdin line into a command, reporting (and skipping) blank/invalid lines.
fn parse_line(line: io::Result<String>, protocol: &mut ProtocolChannel) -> Option<HostCommand> {
    let line = match line {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to read line: {}", e);
            return None;
        }
    };
    if line.trim().is_empty() {
        return None;
    }
    match serde_json::from_str(&line) {
        Ok(cmd) => Some(cmd),
        Err(e) => {
            respond(
                protocol,
                &HostResponse::Error {
                    message: format!("Invalid command: {}", e),
                },
            );
            None
        }
    }
}

fn respond(protocol: &mut ProtocolChannel, response: &HostResponse) {
    if let Ok(json) = serde_json::to_string(response) {
        let _ = writeln!(protocol, "{}", json);
        let _ = protocol.flush();
    }
}

fn err<E: std::fmt::Display>(prefix: &str, e: E) -> HostResponse {
    HostResponse::Error {
        message: format!("{prefix}: {e}"),
    }
}

/// A controller/lifecycle request to execute on the UI thread.
#[cfg(target_os = "macos")]
struct MainRequest {
    command: HostCommand,
    reply: std::sync::mpsc::Sender<HostResponse>,
}

/// Handle non-GUI commands against the shared plugin. The macOS main loop handles GUI commands.
fn handle(command: HostCommand, plugin: &SharedPlugin, sample_rate: &mut f64) -> HostResponse {
    // Convenience: run a closure against the loaded plugin or report "no plugin".
    fn with<F: FnOnce(&mut Plugin) -> HostResponse>(p: &SharedPlugin, f: F) -> HostResponse {
        let mut guard = match p.lock() {
            Ok(g) => g,
            Err(_) => {
                return HostResponse::Error {
                    message: "plugin lock poisoned".to_string(),
                }
            }
        };
        match guard.as_mut() {
            Some(pl) => f(pl),
            None => HostResponse::Error {
                message: "No plugin loaded".to_string(),
            },
        }
    }

    match command {
        HostCommand::MidiControllerAssignments { direction } => {
            with(plugin, |p| match p.midi_controller_assignments(direction) {
                Ok(assignments) => HostResponse::MidiControllerAssignments { assignments },
                Err(e) => err("MidiControllerAssignments", e),
            })
        }
        HostCommand::SendMidiControllerAt {
            bus,
            channel,
            controller,
            value,
            sample_offset,
        } => with(plugin, |p| {
            match p.send_midi_controller_at(bus, channel, controller, value, sample_offset) {
                Ok(()) => HostResponse::Success {
                    message: "send_midi_controller_at".into(),
                },
                Err(e) => err("SendMidiControllerAt", e),
            }
        }),
        HostCommand::NotifyLiveMidiController {
            bus,
            channel,
            controller,
        } => with(plugin, |p| {
            match p.notify_live_midi_controller(bus, channel, controller) {
                Ok(accepted) => HostResponse::NotifyLiveMidiController { accepted },
                Err(e) => err("NotifyLiveMidiController", e),
            }
        }),
        HostCommand::SendLiveMidiEvent { event } => {
            with(plugin, |p| match p.send_live_midi_event(event) {
                Ok(()) => HostResponse::Success {
                    message: "send_live_midi_event".into(),
                },
                Err(e) => err("SendLiveMidiEvent", e),
            })
        }
        HostCommand::Keyswitches { bus, channel } => {
            with(plugin, |p| match p.keyswitches(bus, channel) {
                Ok(keyswitches) => HostResponse::Keyswitches { keyswitches },
                Err(e) => err("Keyswitches", e),
            })
        }
        HostCommand::SetTransportPosition { position } => {
            with(plugin, |p| match p.set_transport_position(position) {
                Ok(()) => HostResponse::Success {
                    message: "transport positioned".into(),
                },
                Err(e) => err("SetTransportPosition", e),
            })
        }
        HostCommand::TransportPosition => with(plugin, |p| match p.transport_position() {
            Ok(position) => HostResponse::TransportPosition { position },
            Err(e) => err("TransportPosition", e),
        }),
        HostCommand::LoadPlugin {
            path,
            sample_rate: sr,
            block_size,
            tempo,
            time_sig_numerator,
            time_sig_denominator,
            class_id,
        } => {
            *sample_rate = sr;
            let mut host = match Vst3Host::builder()
                .sample_rate(sr)
                .block_size(block_size as usize)
                .tempo(tempo)
                .time_signature(time_sig_numerator, time_sig_denominator)
                .build()
            {
                Ok(h) => h,
                Err(e) => return err("Failed to build host", e),
            };
            let loaded = match class_id {
                Some(class_id) => host.load_plugin_class(&path, &class_id),
                None => host.load_plugin(&path),
            };
            match loaded {
                Ok(p) => {
                    let info = p.info().clone();
                    let compatibility = p.class_compatibility().to_vec();
                    let output_channels = p.output_channel_count() as i32;
                    *plugin.lock().unwrap() = Some(p);
                    HostResponse::PluginInfo {
                        vendor: info.vendor,
                        name: info.name,
                        version: info.version,
                        category: info.category,
                        uid: info.uid,
                        has_gui: info.has_gui,
                        audio_inputs: info.audio_inputs as i32,
                        audio_outputs: info.audio_outputs as i32,
                        output_channels,
                        has_midi_input: info.has_midi_input,
                        has_midi_output: info.has_midi_output,
                        compatibility,
                    }
                }
                Err(e) => err("Failed to load plugin", e),
            }
        }
        HostCommand::UnloadPlugin => {
            *plugin.lock().unwrap() = None;
            HostResponse::Success {
                message: "Plugin unloaded".to_string(),
            }
        }
        HostCommand::StartProcessing => with(plugin, |p| match p.start_processing() {
            Ok(()) => HostResponse::Success {
                message: "processing started".to_string(),
            },
            Err(e) => err("StartProcessing", e),
        }),
        HostCommand::StopProcessing => with(plugin, |p| match p.stop_processing() {
            Ok(()) => HostResponse::Success {
                message: "processing stopped".to_string(),
            },
            Err(e) => err("StopProcessing", e),
        }),
        HostCommand::Reconfigure {
            sample_rate: sr,
            block_size,
        } => with(plugin, |p| match p.reconfigure(sr, block_size as usize) {
            Ok(()) => {
                // Track the accepted rate so a post-crash reload uses it. Only on success:
                // a rejected reconfigure must not desync the tracked rate from the plugin.
                *sample_rate = sr;
                HostResponse::Success {
                    message: "reconfigured".to_string(),
                }
            }
            Err(e) => err("Reconfigure", e),
        }),
        HostCommand::SetProcessMode { mode } => with(plugin, |p| match p.set_process_mode(mode) {
            Ok(()) => HostResponse::Success {
                message: "process mode set".to_string(),
            },
            Err(e) => err("SetProcessMode", e),
        }),
        HostCommand::SetParameter { id, value } => {
            with(plugin, |p| match p.set_parameter(id, value) {
                Ok(()) => HostResponse::Success {
                    message: "parameter set".to_string(),
                },
                Err(e) => err("SetParameter", e),
            })
        }
        HostCommand::SetParameterAt { id, value, offset } => {
            with(plugin, |p| match p.set_parameter_at(id, value, offset) {
                Ok(()) => HostResponse::Success {
                    message: "parameter scheduled".to_string(),
                },
                Err(e) => err("SetParameterAt", e),
            })
        }
        HostCommand::SetTempo { bpm } => with(plugin, |p| match p.set_tempo(bpm) {
            Ok(()) => HostResponse::Success {
                message: "tempo set".to_string(),
            },
            Err(e) => err("SetTempo", e),
        }),
        HostCommand::SetTimeSignature {
            numerator,
            denominator,
        } => with(plugin, |p| {
            match p.set_time_signature(numerator, denominator) {
                Ok(()) => HostResponse::Success {
                    message: "time signature set".to_string(),
                },
                Err(e) => err("SetTimeSignature", e),
            }
        }),
        HostCommand::SetPlaying { playing } => with(plugin, |p| match p.set_playing(playing) {
            Ok(()) => HostResponse::Success {
                message: "playing state set".to_string(),
            },
            Err(e) => err("SetPlaying", e),
        }),
        HostCommand::GetParameter { id } => with(plugin, |p| match p.get_parameter(id) {
            Ok(value) => HostResponse::ParameterValue { value },
            Err(e) => err("GetParameter", e),
        }),
        HostCommand::GetAllParameters => with(plugin, |p| match p.get_parameters() {
            Ok(params) => HostResponse::Parameters { params },
            Err(e) => err("GetAllParameters", e),
        }),
        HostCommand::FormatParameter { id, normalized } => {
            with(plugin, |p| match p.format_parameter(id, normalized) {
                Ok(value) => HostResponse::ParameterString { value },
                Err(e) => err("FormatParameter", e),
            })
        }
        HostCommand::SendMidi { event } => with(plugin, |p| match p.send_midi_event(event) {
            Ok(()) => HostResponse::Success {
                message: "midi sent".to_string(),
            },
            Err(e) => err("SendMidi", e),
        }),
        HostCommand::SendMidiAt {
            event,
            sample_offset,
        } => with(plugin, |p| {
            match p.send_midi_event_at(event, sample_offset) {
                Ok(()) => HostResponse::Success {
                    message: "midi sent".to_string(),
                },
                Err(e) => err("SendMidiAt", e),
            }
        }),
        HostCommand::SendPluginEvent { event } => {
            with(plugin, |p| match p.send_plugin_event(event) {
                Ok(()) => HostResponse::Success {
                    message: "plugin event sent".to_string(),
                },
                Err(e) => err("SendPluginEvent", e),
            })
        }
        HostCommand::MidiPanic => with(plugin, |p| match p.midi_panic() {
            Ok(()) => HostResponse::Success {
                message: "MIDI panic queued".to_string(),
            },
            Err(e) => err("MidiPanic", e),
        }),
        HostCommand::SetBusActive {
            media_type,
            direction,
            bus_index,
            active,
        } => with(plugin, |p| {
            match p.set_bus_active(media_type, direction, bus_index, active) {
                Ok(()) => HostResponse::Success {
                    message: "bus activation set".to_string(),
                },
                Err(e) => err("SetBusActive", e),
            }
        }),
        HostCommand::BusArrangements => with(plugin, |p| match p.bus_arrangements() {
            Ok(arrangements) => HostResponse::BusArrangements { arrangements },
            Err(e) => err("BusArrangements", e),
        }),
        HostCommand::SetBusArrangements { inputs, outputs } => with(plugin, |p| {
            match p.set_bus_arrangements(&inputs, &outputs) {
                Ok(()) => HostResponse::Success {
                    message: "bus arrangements set".to_string(),
                },
                Err(e) => err("SetBusArrangements", e),
            }
        }),
        HostCommand::GetUnits => with(plugin, |p| match p.get_units() {
            Ok(units) => HostResponse::Units { units },
            Err(e) => err("GetUnits", e),
        }),
        HostCommand::GetSelectedUnit => with(plugin, |p| match p.selected_unit() {
            Ok(unit_id) => HostResponse::SelectedUnit { unit_id },
            Err(e) => err("GetSelectedUnit", e),
        }),
        HostCommand::SelectUnit { unit_id } => with(plugin, |p| match p.select_unit(unit_id) {
            Ok(()) => HostResponse::Success {
                message: "unit selected".to_string(),
            },
            Err(e) => err("SelectUnit", e),
        }),
        HostCommand::ProgramPitchNames {
            program_list_id,
            program_index,
        } => with(plugin, |p| {
            match p.program_pitch_names(program_list_id, program_index) {
                Ok(names) => HostResponse::ProgramPitchNames { names },
                Err(e) => err("ProgramPitchNames", e),
            }
        }),
        HostCommand::GetProgramData {
            program_list_id,
            program_index,
        } => with(plugin, |p| {
            match p.get_program_data(program_list_id, program_index) {
                Ok(data) => HostResponse::OpaqueData {
                    supported: data.is_some(),
                    data: data.unwrap_or_default(),
                },
                Err(e) => err("GetProgramData", e),
            }
        }),
        HostCommand::SetProgramData {
            program_list_id,
            program_index,
            data,
        } => with(plugin, |p| {
            match p.set_program_data(program_list_id, program_index, &data) {
                Ok(()) => HostResponse::Success {
                    message: "program data restored".to_string(),
                },
                Err(e) => err("SetProgramData", e),
            }
        }),
        HostCommand::GetUnitData { unit_id } => with(plugin, |p| match p.get_unit_data(unit_id) {
            Ok(data) => HostResponse::OpaqueData {
                supported: data.is_some(),
                data: data.unwrap_or_default(),
            },
            Err(e) => err("GetUnitData", e),
        }),
        HostCommand::SetUnitData { unit_id, data } => {
            with(plugin, |p| match p.set_unit_data(unit_id, &data) {
                Ok(()) => HostResponse::Success {
                    message: "unit data restored".to_string(),
                },
                Err(e) => err("SetUnitData", e),
            })
        }
        HostCommand::BeginHostEdit { parameter_id } => {
            with(plugin, |p| match p.begin_host_edit(parameter_id) {
                Ok(()) => HostResponse::Success {
                    message: "host edit begun".to_string(),
                },
                Err(e) => err("BeginHostEdit", e),
            })
        }
        HostCommand::EndHostEdit { parameter_id } => {
            with(plugin, |p| match p.end_host_edit(parameter_id) {
                Ok(()) => HostResponse::Success {
                    message: "host edit ended".to_string(),
                },
                Err(e) => err("EndHostEdit", e),
            })
        }
        HostCommand::SendMidiLearn {
            bus,
            channel,
            controller,
        } => with(plugin, |p| {
            match p.send_midi_learn(bus, channel, controller) {
                Ok(()) => HostResponse::Success {
                    message: "MIDI learn notified".to_string(),
                },
                Err(e) => err("SendMidiLearn", e),
            }
        }),
        HostCommand::SetAutomationState { state } => {
            with(plugin, |p| match p.set_automation_state(state) {
                Ok(()) => HostResponse::Success {
                    message: "automation state set".to_string(),
                },
                Err(e) => err("SetAutomationState", e),
            })
        }
        HostCommand::RemapParameterId {
            old_plugin_uid,
            old_param_id,
        } => with(plugin, |p| {
            match p.remap_parameter_id(&old_plugin_uid, old_param_id) {
                Ok(id) => HostResponse::RemappedParameter { id },
                Err(e) => err("RemapParameterId", e),
            }
        }),
        HostCommand::LatencySamples => with(plugin, |p| HostResponse::LatencySamples {
            samples: p.latency_samples(),
        }),
        HostCommand::TailSamples => with(plugin, |p| HostResponse::TailSamples {
            samples: p.tail_samples(),
        }),
        HostCommand::MidiCcToParameter { bus, channel, cc } => {
            with(plugin, |p| HostResponse::MidiParameterMapping {
                id: p.midi_cc_to_parameter(bus, channel, cc),
            })
        }
        HostCommand::Process { inputs, frames } => {
            let sr = *sample_rate;
            with(plugin, |p| {
                // Live channel count (sums getBusInfo across output buses), so a negotiated
                // non-stereo arrangement (mono, 5.1, …) marshals all its channels back.
                let out_channels = p.output_channel_count().max(1);
                let mut buffers = AudioBuffers {
                    inputs,
                    outputs: vec![vec![0.0; frames as usize]; out_channels],
                    sample_rate: sr,
                    block_size: frames as usize,
                };
                match p.process_audio(&mut buffers) {
                    Ok(()) => HostResponse::AudioOutput {
                        outputs: buffers.outputs,
                        output_events: p.take_output_events(),
                    },
                    Err(e) => err("Process", e),
                }
            })
        }
        HostCommand::ProcessBuses {
            inputs,
            outputs,
            frames,
        } => {
            if inputs.len() > 256
                || outputs.len() > 256
                || frames as usize > (1 << 20)
                || outputs.iter().any(|bus| bus.channel_count > 256)
            {
                return HostResponse::Error {
                    message: "ProcessBuses: bus shape exceeds wire limits".to_string(),
                };
            }
            let sr = *sample_rate;
            with(plugin, |p| {
                let mut buffers = vst3_host::BusAudioBuffers {
                    inputs,
                    outputs: outputs
                        .iter()
                        .map(|config| {
                            vst3_host::AudioBusBuffer::new(
                                config.channel_count,
                                frames as usize,
                                config.active,
                            )
                        })
                        .collect(),
                    sample_rate: sr,
                    block_size: frames as usize,
                };
                match p.process_bus_audio(&mut buffers) {
                    Ok(()) => HostResponse::BusAudioOutput {
                        outputs: buffers.outputs,
                        output_events: p.take_output_events(),
                    },
                    Err(e) => err("ProcessBuses", e),
                }
            })
        }
        HostCommand::AudioBusLayout => with(plugin, |p| match p.audio_bus_layout() {
            Ok(layout) => HostResponse::AudioBusLayout { layout },
            Err(e) => err("AudioBusLayout", e),
        }),
        HostCommand::SaveState => with(plugin, |p| match p.save_state() {
            Ok(data) => HostResponse::State { data },
            Err(e) => err("SaveState", e),
        }),
        HostCommand::LoadState { data, context } => with(plugin, |p| {
            match p.load_state_with_context(&data, &context) {
                Ok(()) => HostResponse::Success {
                    message: "state restored".to_string(),
                },
                Err(e) => err("LoadState", e),
            }
        }),
        HostCommand::NoteOn {
            channel,
            note,
            velocity,
            sample_offset,
        } => with(plugin, |p| {
            let Some(ch) = vst3_host::MidiChannel::from_index(channel) else {
                return HostResponse::Error {
                    message: format!("NoteOn: invalid channel index {channel}"),
                };
            };
            // The in-process plugin allocates the per-voice NoteId; return its raw id.
            match p.note_on_at(ch, note, velocity, sample_offset) {
                Ok(id) => HostResponse::NoteStarted { note_id: id.raw() },
                Err(e) => err("NoteOn", e),
            }
        }),
        HostCommand::NoteOff {
            note_id,
            sample_offset,
        } => with(plugin, |p| {
            match p.note_off_at(vst3_host::NoteId::from_raw(note_id), sample_offset) {
                Ok(()) => HostResponse::Success {
                    message: "note off".to_string(),
                },
                Err(e) => err("NoteOff", e),
            }
        }),
        HostCommand::SendNoteExpression {
            note_id,
            kind,
            value,
            sample_offset,
        } => with(plugin, |p| {
            match p.send_note_expression_at(
                vst3_host::NoteId::from_raw(note_id),
                kind,
                value,
                sample_offset,
            ) {
                Ok(()) => HostResponse::Success {
                    message: "note expression sent".to_string(),
                },
                Err(e) => err("SendNoteExpression", e),
            }
        }),
        // Note: the public API enumerates bus 0 / channel 0 (the conventional MPE bus); the
        // bus/channel carried by the command is currently always (0, 0) from the client.
        HostCommand::NoteExpressions { bus: _, channel: _ } => {
            with(plugin, |p| match p.note_expressions() {
                Ok(expressions) => HostResponse::NoteExpressions { expressions },
                Err(e) => err("NoteExpressions", e),
            })
        }
        HostCommand::SelectProgram {
            unit_id,
            program_index,
        } => with(plugin, |p| match p.select_program(unit_id, program_index) {
            Ok(()) => HostResponse::Success {
                message: "program selected".to_string(),
            },
            Err(e) => err("SelectProgram", e),
        }),
        HostCommand::TakeParameterEdits => with(plugin, |p| HostResponse::ParameterEdits {
            edits: p.take_parameter_edits(),
        }),
        HostCommand::TakeParameterChanges => with(plugin, |p| HostResponse::ParameterChanges {
            changes: p.get_parameter_changes(),
        }),
        HostCommand::TakeHostNotifications => with(plugin, |p| HostResponse::HostNotifications {
            notifications: p.take_host_notifications(),
        }),
        HostCommand::TakeDataExchangeBlocks => with(plugin, |p| HostResponse::DataExchangeBlocks {
            blocks: p.take_data_exchange_blocks(),
        }),
        HostCommand::ExecuteContextMenuItem { menu_id, item_id } => with(plugin, |p| {
            match p.execute_context_menu_item(menu_id, item_id) {
                Ok(()) => HostResponse::Success {
                    message: "context-menu item executed".to_string(),
                },
                Err(error) => err("ExecuteContextMenuItem", error),
            }
        }),
        HostCommand::DismissContextMenu { menu_id } => {
            with(plugin, |p| match p.dismiss_context_menu(menu_id) {
                Ok(()) => HostResponse::Success {
                    message: "context menu dismissed".to_string(),
                },
                Err(error) => err("DismissContextMenu", error),
            })
        }
        HostCommand::TakeRestartFlags => with(plugin, |p| HostResponse::RestartFlags {
            bits: p.take_restart_flags().bits(),
        }),
        HostCommand::ServiceHostRequests => with(plugin, |p| match p.service_host_requests() {
            Ok(flags) => HostResponse::RestartFlags { bits: flags.bits() },
            Err(error) => err("ServiceHostRequests", error),
        }),
        HostCommand::CreateGui | HostCommand::CloseGui => HostResponse::Error {
            message: "GUI commands require the macOS main-thread dispatcher".into(),
        },
        HostCommand::Shutdown => HostResponse::Success {
            message: "shutting down".to_string(),
        },
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSView,
        NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};
    use std::sync::mpsc;

    /// Entry point on macOS: spawn the stdin/command worker, then run the UI event pump on
    /// this (main) thread. The already-claimed protocol channel moves to the worker, which is
    /// the only thread that writes responses.
    pub fn run(plugin: SharedPlugin, mut protocol: ProtocolChannel) {
        let (control_tx, control_rx) = mpsc::sync_channel::<MainRequest>(1);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        // Worker: read stdin, process audio, and delegate controls to the main loop.
        let worker = {
            let plugin = plugin.clone();
            std::thread::spawn(move || {
                let stdin = io::stdin();
                let mut sample_rate = 44100.0;
                for line in stdin.lock().lines() {
                    let Some(command) = parse_line(line, &mut protocol) else {
                        continue;
                    };
                    if matches!(command, HostCommand::Shutdown) {
                        eprintln!("Shutting down helper process");
                        let _ = shutdown_tx.send(());
                        break;
                    }
                    let requested_sample_rate = match &command {
                        HostCommand::LoadPlugin { sample_rate, .. }
                        | HostCommand::Reconfigure { sample_rate, .. } => Some(*sample_rate),
                        _ => None,
                    };
                    let response = if matches!(
                        command,
                        HostCommand::Process { .. } | HostCommand::ProcessBuses { .. }
                    ) {
                        handle(command, &plugin, &mut sample_rate)
                    } else {
                        let (reply, response) = mpsc::channel();
                        if control_tx.send(MainRequest { command, reply }).is_err() {
                            break;
                        }
                        match response.recv() {
                            Ok(response) => response,
                            Err(_) => break,
                        }
                    };
                    if !matches!(response, HostResponse::Error { .. }) {
                        if let Some(sr) = requested_sample_rate {
                            sample_rate = sr;
                        }
                    }
                    respond(&mut protocol, &response);
                }
                // stdin closed → ask the main loop to exit too.
                let _ = shutdown_tx.send(());
            })
        };

        run_event_loop(&plugin, &control_rx, &shutdown_rx);
        let _ = worker.join();
    }

    /// The main-thread native event pump. Interleaves AppKit event dispatch with polling
    /// the control-request and shutdown channels.
    fn run_event_loop(
        plugin: &SharedPlugin,
        control_rx: &mpsc::Receiver<MainRequest>,
        shutdown_rx: &mpsc::Receiver<()>,
    ) {
        let mtm = MainThreadMarker::new().expect("helper UI loop must run on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        // Accessory: no Dock icon / menu bar for the (usually headless) helper.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();

        let mut window: Option<Retained<NSWindow>> = None;
        let mut sample_rate = 44100.0;

        loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }

            // Bounded dispatch leaves time for native events. UI/controller calls and native
            // editor callbacks now share this thread; the worker only enters process().
            for _ in 0..32 {
                let Ok(req) = control_rx.try_recv() else {
                    break;
                };
                let response = match req.command {
                    HostCommand::CreateGui => match open_editor_window(plugin, mtm, &app) {
                        Ok((w, width, height)) => {
                            window = Some(w);
                            HostResponse::GuiCreated { width, height }
                        }
                        Err(e) => HostResponse::Error { message: e },
                    },
                    HostCommand::CloseGui => {
                        close_editor_window(plugin, window.take());
                        HostResponse::Success {
                            message: "editor closed".into(),
                        }
                    }
                    command => {
                        if matches!(command, HostCommand::LoadPlugin { .. }) {
                            close_editor_window(plugin, window.take());
                        }
                        handle(command, plugin, &mut sample_rate)
                    }
                };
                let _ = req.reply.send(response);
            }

            // Pump native events, waking at least every 20 ms to re-check the channels.
            let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
            while let Some(event) = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&until),
                    NSDefaultRunLoopMode,
                    true,
                )
            } {
                app.sendEvent(&event);
            }
        }

        close_editor_window(plugin, window.take());
    }

    /// Create a top-level window owned by this (helper) process and attach the plugin's
    /// editor into it. Returns the window plus its content size.
    fn open_editor_window(
        plugin: &SharedPlugin,
        mtm: MainThreadMarker,
        app: &NSApplication,
    ) -> std::result::Result<(Retained<NSWindow>, i32, i32), String> {
        let mut guard = plugin
            .lock()
            .map_err(|_| "plugin lock poisoned".to_string())?;
        let p = guard
            .as_mut()
            .ok_or_else(|| "No plugin loaded".to_string())?;
        if !p.has_editor() {
            return Err("Plugin does not have a GUI editor".to_string());
        }

        let (width, height) = p.get_editor_size().unwrap_or((800, 600));
        let title = format!("{} - VST3", p.info().name);

        let frame = NSRect::new(
            NSPoint::new(120.0, 120.0),
            NSSize::new(width as f64, height as f64),
        );
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;
        // SAFETY: standard AppKit window/view construction on the main thread.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // We own the window's lifetime via `Retained`; opt out of release-on-close to avoid
        // a double-free when the editor window is closed.
        // SAFETY: standard AppKit setter on the main thread.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(&title));

        let container = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        );
        if let Some(content) = window.contentView() {
            content.addSubview(&container);
        }

        // SAFETY: `container` is a live NSView held by the window this function returns, which
        // the helper keeps alive until it closes the editor.
        let handle = unsafe {
            vst3_host::WindowHandle::from_nsview(
                Retained::as_ptr(&container) as *mut std::ffi::c_void
            )
        };
        p.open_editor(handle).map_err(|e| e.to_string())?;

        window.setContentSize(frame.size);
        window.center();
        window.makeKeyAndOrderFront(None);
        // Bring the helper forward so the editor is usable.
        app.activate();

        Ok((window, width, height))
    }

    fn close_editor_window(plugin: &SharedPlugin, window: Option<Retained<NSWindow>>) {
        if let Ok(mut guard) = plugin.lock() {
            if let Some(p) = guard.as_mut() {
                let _ = p.close_editor();
            }
        }
        if let Some(w) = window {
            w.close();
        }
    }
}
