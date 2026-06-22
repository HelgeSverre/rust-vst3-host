//! Live hardware MIDI input: forward a connected controller's MIDI into the loaded plugin.
//!
//! Cross-platform via `midir` (CoreMIDI on macOS, ALSA on Linux, WinMM on Windows). The midir
//! read callback runs on its own OS thread and only pushes parsed [`MidiEvent`]s into an `mpsc`
//! channel — it never touches the plugin lock. The inspector's `update()` loop drains the
//! channel on the UI thread and forwards events to the plugin (mirroring the virtual keyboard /
//! MIDI-file paths). Events flow device → plugin only, so there is no feedback loop (the
//! plugin's own MIDI output goes to the monitor, never to an output port or back to input).

use midir::{Ignore, MidiInput, MidiInputConnection};
use std::sync::mpsc::{channel, Receiver, Sender};
use vst3_host::midi::MidiEvent;

/// The client name `midir` advertises to the OS.
const CLIENT_NAME: &str = "vst3-inspector";

/// Manages the optional connection to one hardware MIDI input port.
#[derive(Default)]
pub struct MidiInputState {
    /// Active connection — kept alive because dropping it disconnects. `None` = not listening.
    /// The connection owns the `Sender` the callback writes to.
    conn: Option<MidiInputConnection<Sender<MidiEvent>>>,
    /// Events the callback has parsed, drained on the UI thread.
    rx: Option<Receiver<MidiEvent>>,
    /// The connected port's display name (for the UI).
    port_name: Option<String>,
}

impl MidiInputState {
    /// List available MIDI input port names. Empty if there are none or MIDI is unavailable —
    /// never panics, so the UI degrades gracefully with no devices connected.
    pub fn list_ports() -> Vec<String> {
        match MidiInput::new(&format!("{CLIENT_NAME}-list")) {
            Ok(mi) => mi
                .ports()
                .iter()
                .map(|p| mi.port_name(p).unwrap_or_else(|_| "<unknown>".to_string()))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The name of the currently connected port, if any.
    pub fn connected_port(&self) -> Option<&str> {
        self.port_name.as_deref()
    }

    /// Whether a device is currently connected.
    pub fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    /// Connect to the input port at `index` (matching [`list_ports`](Self::list_ports) order),
    /// replacing any existing connection. Active-sensing / timing-clock / SysEx traffic is
    /// ignored so a controller can't flood the plugin with realtime noise.
    pub fn connect(&mut self, index: usize) -> Result<(), String> {
        self.disconnect();

        let mut input =
            MidiInput::new(CLIENT_NAME).map_err(|e| format!("MIDI unavailable: {e}"))?;
        input.ignore(Ignore::All);

        let ports = input.ports();
        let port = ports
            .get(index)
            .ok_or_else(|| "selected MIDI port no longer exists".to_string())?;
        let name = input
            .port_name(port)
            .unwrap_or_else(|_| "MIDI input".to_string());

        let (tx, rx) = channel::<MidiEvent>();
        let conn = input
            .connect(
                port,
                "vst3-inspector-in",
                |_timestamp, bytes, tx: &mut Sender<MidiEvent>| {
                    // Callback thread: parse and queue only. The UI thread forwards to the plugin.
                    if let Some(ev) = MidiEvent::from_midi_bytes(bytes) {
                        let _ = tx.send(ev); // ignore if the receiver was dropped
                    }
                },
                tx,
            )
            .map_err(|e| format!("failed to open MIDI input: {e}"))?;

        self.conn = Some(conn);
        self.rx = Some(rx);
        self.port_name = Some(name);
        Ok(())
    }

    /// Disconnect from the current device (a no-op if not connected).
    pub fn disconnect(&mut self) {
        // Dropping the connection closes the OS port.
        self.conn = None;
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
