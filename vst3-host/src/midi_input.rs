//! Bind a live MIDI input device and forward its messages as [`MidiEvent`]s.
//!
//! Enabled by the `midi-input` feature. Wraps the [`midir`] crate so callers never touch its
//! types directly: [`list_midi_input_ports`] enumerates the available input ports, and
//! [`MidiInputPort`] is a name + opaque handle you pass to [`connect`] or [`bind_to_handle`].
//!
//! Both connection forms return a [`MidiInputConnection`] guard; dropping it closes the port and
//! stops delivery.
//!
//! ## Threading
//!
//! The callback you pass to [`connect`] runs on a thread owned by `midir` (the OS MIDI driver
//! thread), **not** the thread that called `connect`. Treat it like an audio callback: do not
//! block, allocate heavily, or panic in it. [`bind_to_handle`] only calls
//! [`AudioHandle::send_midi`], which is lock-free and non-blocking, so it is safe to use from
//! that thread.
//!
//! ## Example
//!
//! ```no_run
//! use vst3_host::midi_input::{self, MidiInputConnection};
//!
//! # fn main() -> vst3_host::Result<()> {
//! let ports = midi_input::list_midi_input_ports()?;
//! let Some(port) = ports.first() else {
//!     return Ok(());
//! };
//!
//! // Low-level: receive parsed events on midir's thread.
//! let _conn: MidiInputConnection = midi_input::connect(port, |event| {
//!     println!("{event:?}");
//! })?;
//! # Ok(())
//! # }
//! ```

use midir::{MidiInput, MidiInputPort as RawMidiInputPort};

use crate::{
    error::{Error, Result},
    midi::MidiEvent,
    playback::{AudioHandle, MidiSink},
};

/// The client name `midir` advertises to the OS when enumerating or opening ports.
const CLIENT_NAME: &str = "vst3-host";

/// Map a `midir` error into the library's [`Error::MidiError`].
fn midi_err(context: &str, err: impl std::fmt::Display) -> Error {
    Error::MidiError(format!("{context}: {err}"))
}

/// A discovered MIDI input port: its human-readable name plus the opaque handle used to open it.
///
/// Obtain these from [`list_midi_input_ports`]. The handle is tied to the port as the OS reported
/// it at enumeration time; if the device is unplugged, [`connect`] will fail.
#[derive(Clone)]
pub struct MidiInputPort {
    name: String,
    raw: RawMidiInputPort,
}

impl MidiInputPort {
    /// The port's display name (e.g. the device or virtual-port name).
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Debug for MidiInputPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiInputPort")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// An open connection to a MIDI input port.
///
/// Holds the underlying `midir` connection alive; **dropping it closes the port** and stops the
/// callback. Keep it for as long as you want to receive MIDI.
pub struct MidiInputConnection {
    // `midir::MidiInputConnection` is generic over the callback's user-data type; we use `()` and
    // close it via `Drop`. Boxed only so the public type isn't generic.
    _inner: midir::MidiInputConnection<()>,
}

/// List the MIDI input ports currently available on the system.
///
/// Returns an empty vector when no input ports are present (not an error). Fails only if the
/// platform MIDI subsystem cannot be initialized.
pub fn list_midi_input_ports() -> Result<Vec<MidiInputPort>> {
    let input = MidiInput::new(CLIENT_NAME).map_err(|e| midi_err("init MIDI input", e))?;
    let ports = input
        .ports()
        .into_iter()
        .map(|raw| {
            let name = input
                .port_name(&raw)
                .unwrap_or_else(|_| "Unknown MIDI input".to_string());
            MidiInputPort { name, raw }
        })
        .collect();
    Ok(ports)
}

/// Open `port` and deliver each parseable incoming message to `callback` as a [`MidiEvent`].
///
/// Raw bytes are parsed with [`MidiEvent::from_midi_bytes`]; messages that don't parse (SysEx,
/// realtime, program change, truncated data) are silently ignored.
///
/// `callback` runs on a `midir`-owned thread — see the [module docs](self#threading). It must be
/// `Send + 'static`. The returned [`MidiInputConnection`] keeps the port open until dropped.
pub fn connect<F>(port: &MidiInputPort, mut callback: F) -> Result<MidiInputConnection>
where
    F: FnMut(MidiEvent) + Send + 'static,
{
    let input = MidiInput::new(CLIENT_NAME).map_err(|e| midi_err("init MIDI input", e))?;
    let inner = input
        .connect(
            &port.raw,
            &port.name,
            move |_timestamp, bytes, ()| {
                if let Some(event) = parse_midi(bytes) {
                    callback(event);
                }
            },
            (),
        )
        .map_err(|e| midi_err("connect MIDI input", e))?;
    Ok(MidiInputConnection { _inner: inner })
}

/// Open `port` and forward every parseable incoming message into a running [`AudioHandle`].
///
/// A convenience over [`connect`]: each received [`MidiEvent`] is pushed into the plugin's
/// command ring (via the handle's [`MidiSink`]), so notes/CC played on the device reach the
/// plugin. A `Send` sink is captured into the callback, so the connection keeps forwarding as
/// long as the returned guard lives — independent of the `AudioHandle`'s own lifetime.
///
/// Events are dropped silently if the audio command ring is full (the same drop-on-full behavior
/// as [`AudioHandle::send_midi`]). The returned [`MidiInputConnection`] keeps the port open until
/// dropped.
pub fn bind_to_handle(port: &MidiInputPort, handle: &AudioHandle) -> Result<MidiInputConnection> {
    let sink: MidiSink = handle.midi_sink();
    connect(port, move |event| {
        sink.send_midi(event);
    })
}

/// Parse a raw MIDI message into a [`MidiEvent`], returning `None` for messages the library does
/// not forward. Factored out of the `midir` callback so it can be unit-tested without hardware.
fn parse_midi(bytes: &[u8]) -> Option<MidiEvent> {
    MidiEvent::from_midi_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::MidiChannel;

    #[test]
    fn list_midi_input_ports_does_not_error() {
        // May be empty in CI (no devices), but must not fail to enumerate.
        let ports = list_midi_input_ports();
        assert!(ports.is_ok(), "enumeration failed: {:?}", ports.err());
    }

    #[test]
    fn parse_midi_forwards_channel_voice_and_drops_the_rest() {
        assert_eq!(
            parse_midi(&[0x90, 60, 100]),
            Some(MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 100,
            })
        );
        assert_eq!(
            parse_midi(&[0xB0, 1, 64]),
            Some(MidiEvent::ControlChange {
                channel: MidiChannel::Ch1,
                controller: 1,
                value: 64,
            })
        );
        // Program change is forwarded (the host routes it to program selection).
        assert_eq!(
            parse_midi(&[0xC0, 5]),
            Some(MidiEvent::ProgramChange {
                channel: MidiChannel::Ch1,
                program: 5,
            })
        );
        // SysEx, empty, and other non-channel-voice messages are dropped.
        assert_eq!(parse_midi(&[0xF0, 1, 2]), None);
        assert_eq!(parse_midi(&[]), None);
    }
}
