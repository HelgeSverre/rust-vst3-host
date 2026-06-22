//! Live hardware MIDI input: forward a connected controller's MIDI into the loaded plugin.
//!
//! On macOS this uses the native CoreMIDI binding (no ALSA, so no `links` clash with cpal).
//! Other platforms get a stub for now. The CoreMIDI read callback runs on its own thread and
//! only pushes parsed [`MidiEvent`]s into an `mpsc` channel — it never touches the plugin lock;
//! the inspector's `update()` loop drains the channel on the UI thread and forwards to the
//! plugin (mirroring the virtual keyboard / MIDI-file paths). Events flow device → plugin only,
//! so there is no feedback loop (the plugin's own MIDI output goes to the monitor, never back).

use vst3_host::midi::MidiEvent;

#[cfg(target_os = "macos")]
pub use macos::MidiInputState;
#[cfg(not(target_os = "macos"))]
pub use stub::MidiInputState;

/// Split a raw MIDI byte stream (one CoreMIDI packet may carry several messages, possibly with
/// running status) into channel-voice [`MidiEvent`]s. Realtime bytes and SysEx are dropped.
#[cfg(any(target_os = "macos", test))]
fn split_messages(data: &[u8], out: &mut Vec<MidiEvent>) {
    let mut i = 0;
    let mut status = 0u8; // running status
    while i < data.len() {
        let b = data[i];
        if b >= 0x80 {
            if b >= 0xF8 {
                i += 1; // system realtime (clock/active-sensing) — ignore
                continue;
            }
            if b == 0xF0 {
                // SysEx: skip to the terminating 0xF7.
                i += 1;
                while i < data.len() && data[i] != 0xF7 {
                    i += 1;
                }
                if i < data.len() {
                    i += 1;
                }
                status = 0;
                continue;
            }
            if (0xF1..0xF8).contains(&b) {
                status = 0; // system common — skip (don't forward)
                i += 1;
                continue;
            }
            status = b; // channel-voice status
            i += 1;
        }
        if status < 0x80 {
            i += 1; // stray data byte with no running status
            continue;
        }
        let len = match status & 0xF0 {
            0xC0 | 0xD0 => 1, // program change / channel pressure
            _ => 2,
        };
        if i + len > data.len() {
            break; // truncated
        }
        let mut msg = [0u8; 3];
        msg[0] = status;
        msg[1..1 + len].copy_from_slice(&data[i..i + len]);
        if let Some(ev) = MidiEvent::from_midi_bytes(&msg[..1 + len]) {
            out.push(ev);
        }
        i += len;
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{split_messages, MidiEvent};
    use coremidi::{Client, InputPort, PacketList, Source, Sources};
    use std::sync::mpsc::{channel, Receiver};

    /// Manages the optional connection to one CoreMIDI input source.
    #[derive(Default)]
    pub struct MidiInputState {
        // Client + port are kept alive; dropping the port disconnects. None = not listening.
        client: Option<Client>,
        port: Option<InputPort>,
        rx: Option<Receiver<MidiEvent>>,
        port_name: Option<String>,
    }

    impl MidiInputState {
        /// List available MIDI input source names (empty if none — never panics).
        pub fn list_ports() -> Vec<String> {
            (0..Sources::count())
                .filter_map(Source::from_index)
                .map(|s| s.display_name().unwrap_or_else(|| "<unknown>".to_string()))
                .collect()
        }

        /// The connected source's name, if any.
        pub fn connected_port(&self) -> Option<&str> {
            self.port_name.as_deref()
        }

        /// Whether a device is connected.
        pub fn is_connected(&self) -> bool {
            self.port.is_some()
        }

        /// Connect to the input source at `index` (matching [`list_ports`](Self::list_ports)
        /// order), replacing any existing connection.
        pub fn connect(&mut self, index: usize) -> Result<(), String> {
            self.disconnect();

            let source = Source::from_index(index)
                .ok_or_else(|| "selected MIDI port no longer exists".to_string())?;
            let name = source
                .display_name()
                .unwrap_or_else(|| "MIDI input".to_string());

            let client = Client::new("vst3-inspector")
                .map_err(|_| "could not create MIDI client".to_string())?;
            let (tx, rx) = channel::<MidiEvent>();
            let port = client
                .input_port("vst3-inspector-in", move |packets: &PacketList| {
                    // Callback thread: parse + queue only.
                    let mut events = Vec::new();
                    for packet in packets.iter() {
                        split_messages(packet.data(), &mut events);
                    }
                    for ev in events {
                        let _ = tx.send(ev);
                    }
                })
                .map_err(|_| "could not open MIDI input port".to_string())?;
            port.connect_source(&source)
                .map_err(|_| "could not connect to the MIDI device".to_string())?;

            self.client = Some(client);
            self.port = Some(port);
            self.rx = Some(rx);
            self.port_name = Some(name);
            Ok(())
        }

        /// Disconnect (no-op if not connected).
        pub fn disconnect(&mut self) {
            self.port = None; // drop disconnects the source
            self.client = None;
            self.rx = None;
            self.port_name = None;
        }

        /// Pull all events queued since the last call (call on the UI thread each frame).
        pub fn drain(&mut self) -> Vec<MidiEvent> {
            match &self.rx {
                Some(rx) => rx.try_iter().collect(),
                None => Vec::new(),
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod stub {
    use super::MidiEvent;

    /// Live device input is only implemented on macOS (CoreMIDI) so far. This stub keeps the
    /// inspector building and behaving (no devices) on other platforms.
    #[derive(Default)]
    pub struct MidiInputState;

    impl MidiInputState {
        pub fn list_ports() -> Vec<String> {
            Vec::new()
        }
        pub fn connected_port(&self) -> Option<&str> {
            None
        }
        pub fn is_connected(&self) -> bool {
            false
        }
        pub fn connect(&mut self, _index: usize) -> Result<(), String> {
            Err("live MIDI input is only available on macOS in this build".to_string())
        }
        pub fn disconnect(&mut self) {}
        pub fn drain(&mut self) -> Vec<MidiEvent> {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3_host::midi::MidiChannel;

    #[test]
    fn splits_running_status_and_ignores_realtime() {
        // Note-on then a running-status note-on (no repeated status), with an interleaved
        // realtime clock byte (0xF8) that must be ignored.
        let bytes = [0x90, 60, 100, 0xF8, 62, 80];
        let mut out = Vec::new();
        split_messages(&bytes, &mut out);
        assert_eq!(
            out,
            vec![
                MidiEvent::NoteOn {
                    channel: MidiChannel::Ch1,
                    note: 60,
                    velocity: 100
                },
                MidiEvent::NoteOn {
                    channel: MidiChannel::Ch1,
                    note: 62,
                    velocity: 80
                },
            ]
        );
    }

    #[test]
    fn skips_sysex_and_keeps_following_message() {
        let bytes = [0xF0, 0x7E, 0x01, 0xF7, 0xB0, 1, 64];
        let mut out = Vec::new();
        split_messages(&bytes, &mut out);
        assert_eq!(
            out,
            vec![MidiEvent::ControlChange {
                channel: MidiChannel::Ch1,
                controller: 1,
                value: 64
            }]
        );
    }
}
