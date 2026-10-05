//! MIDI types and utilities for VST3 host

use serde::{Deserialize, Serialize};
use std::fmt;

/// MIDI channel enumeration (1-16)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MidiChannel {
    /// Channel 1
    Ch1,
    /// Channel 2
    Ch2,
    /// Channel 3
    Ch3,
    /// Channel 4
    Ch4,
    /// Channel 5
    Ch5,
    /// Channel 6
    Ch6,
    /// Channel 7
    Ch7,
    /// Channel 8
    Ch8,
    /// Channel 9
    Ch9,
    /// Channel 10 (often drums in GM)
    Ch10,
    /// Channel 11
    Ch11,
    /// Channel 12
    Ch12,
    /// Channel 13
    Ch13,
    /// Channel 14
    Ch14,
    /// Channel 15
    Ch15,
    /// Channel 16
    Ch16,
}

impl MidiChannel {
    /// Get the channel as a 0-based index (0-15)
    pub fn as_index(&self) -> u8 {
        match self {
            MidiChannel::Ch1 => 0,
            MidiChannel::Ch2 => 1,
            MidiChannel::Ch3 => 2,
            MidiChannel::Ch4 => 3,
            MidiChannel::Ch5 => 4,
            MidiChannel::Ch6 => 5,
            MidiChannel::Ch7 => 6,
            MidiChannel::Ch8 => 7,
            MidiChannel::Ch9 => 8,
            MidiChannel::Ch10 => 9,
            MidiChannel::Ch11 => 10,
            MidiChannel::Ch12 => 11,
            MidiChannel::Ch13 => 12,
            MidiChannel::Ch14 => 13,
            MidiChannel::Ch15 => 14,
            MidiChannel::Ch16 => 15,
        }
    }

    /// Create from 0-based index (0-15)
    pub fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(MidiChannel::Ch1),
            1 => Some(MidiChannel::Ch2),
            2 => Some(MidiChannel::Ch3),
            3 => Some(MidiChannel::Ch4),
            4 => Some(MidiChannel::Ch5),
            5 => Some(MidiChannel::Ch6),
            6 => Some(MidiChannel::Ch7),
            7 => Some(MidiChannel::Ch8),
            8 => Some(MidiChannel::Ch9),
            9 => Some(MidiChannel::Ch10),
            10 => Some(MidiChannel::Ch11),
            11 => Some(MidiChannel::Ch12),
            12 => Some(MidiChannel::Ch13),
            13 => Some(MidiChannel::Ch14),
            14 => Some(MidiChannel::Ch15),
            15 => Some(MidiChannel::Ch16),
            _ => None,
        }
    }
}

impl fmt::Display for MidiChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ch{}", self.as_index() + 1)
    }
}

/// High-level MIDI event types.
///
/// Marked `#[non_exhaustive]`: match with a wildcard arm, as new event kinds (e.g. SysEx)
/// may be added in future versions without it being a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MidiEvent {
    /// Note On event
    NoteOn {
        /// MIDI channel (1-16)
        channel: MidiChannel,
        /// Note number (0-127)
        note: u8,
        /// Velocity (0-127)
        velocity: u8,
    },
    /// Note Off event
    NoteOff {
        /// MIDI channel (1-16)
        channel: MidiChannel,
        /// Note number (0-127)
        note: u8,
        /// Velocity (0-127)
        velocity: u8,
    },
    /// Control Change event
    ControlChange {
        /// MIDI channel (1-16)
        channel: MidiChannel,
        /// Controller number (0-127)
        controller: u8,
        /// Value (0-127)
        value: u8,
    },
    /// Program Change event
    ProgramChange {
        /// MIDI channel (1-16)
        channel: MidiChannel,
        /// Program number (0-127)
        program: u8,
    },
    /// Pitch Bend event
    PitchBend {
        /// MIDI channel (1-16)
        channel: MidiChannel,
        /// Pitch bend value (0-16383, center is 8192)
        value: u16,
    },
    /// Channel Aftertouch event
    ChannelAftertouch {
        /// MIDI channel (1-16)
        channel: MidiChannel,
        /// Pressure value (0-127)
        pressure: u8,
    },
    /// Polyphonic Aftertouch event
    PolyAftertouch {
        /// MIDI channel (1-16)
        channel: MidiChannel,
        /// Note number (0-127)
        note: u8,
        /// Pressure value (0-127)
        pressure: u8,
    },
}

/// Maximum pointer-backed payload accepted for one VST3 event.
///
/// This bounds both in-process plugin output and process-isolation messages. A SysEx message
/// larger than this is rejected instead of allowing an untrusted plugin or IPC peer to force an
/// unbounded allocation in the host.
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Maximum UTF-16 code units accepted from a pointer-backed VST3 event.
pub const MAX_EVENT_TEXT_UNITS: usize = 16 * 1024;

/// A fully owned VST3 event.
///
/// Unlike the SDK's raw `Event` union, pointer-backed payloads live in this value and remain safe
/// to queue, move between threads, or serialize across process isolation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginEvent {
    /// Event-bus index.
    pub bus_index: i32,
    /// Sample offset within the next process block.
    pub sample_offset: i32,
    /// Musical position in quarter notes, when known.
    pub ppq_position: f64,
    /// Raw VST3 event flags.
    pub flags: u16,
    /// Event payload.
    pub data: PluginEventData,
}

/// An event emitted by a plugin.
///
/// Input and output use the same owned representation, so this alias mainly documents direction
/// at API boundaries such as [`Plugin::take_output_events`](crate::Plugin::take_output_events).
pub type OutputEvent = PluginEvent;

/// The owned payload of a [`PluginEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum PluginEventData {
    /// VST3 note-on event.
    NoteOn {
        channel: i16,
        pitch: i16,
        tuning: f32,
        velocity: f32,
        length: i32,
        note_id: i32,
    },
    /// VST3 note-off event.
    NoteOff {
        channel: i16,
        pitch: i16,
        velocity: f32,
        note_id: i32,
        tuning: f32,
    },
    /// Pointer-backed data event. VST3 currently defines data type `0` as MIDI SysEx.
    Data { data_type: u32, bytes: Vec<u8> },
    /// Polyphonic pressure.
    PolyPressure {
        channel: i16,
        pitch: i16,
        pressure: f32,
        note_id: i32,
    },
    /// Floating-point per-note expression.
    NoteExpressionValue {
        type_id: u32,
        note_id: i32,
        value: f64,
    },
    /// UTF-16 per-note expression text.
    NoteExpressionText {
        type_id: u32,
        note_id: i32,
        text: Vec<u16>,
    },
    /// Integer per-note expression.
    NoteExpressionIntValue {
        type_id: u32,
        note_id: i32,
        value: u64,
    },
    /// Chord event with owned UTF-16 display text.
    Chord {
        root: i16,
        bass_note: i16,
        mask: i16,
        text: Vec<u16>,
    },
    /// Scale event with owned UTF-16 display text.
    Scale {
        root: i16,
        mask: i16,
        text: Vec<u16>,
    },
    /// Legacy MIDI output event. This is valid only as plugin output.
    LegacyMidiCcOut {
        control_number: u8,
        channel: i8,
        value: u8,
        value2: u8,
    },
}

impl PluginEvent {
    /// Construct a MIDI SysEx data event on bus 0 at block start.
    pub fn sysex(bytes: Vec<u8>) -> Self {
        Self {
            bus_index: 0,
            sample_offset: 0,
            ppq_position: 0.0,
            flags: 0,
            data: PluginEventData::Data {
                data_type: 0,
                bytes,
            },
        }
    }

    /// Set the sample offset for this event.
    pub fn at(mut self, sample_offset: i32) -> Self {
        self.sample_offset = sample_offset;
        self
    }

    /// Convert a channel-voice event into the compatibility [`MidiEvent`] model.
    ///
    /// SysEx, note-expression, chord, and scale events intentionally return `None`; callers that
    /// need those events should use the owned event API.
    pub fn to_midi(&self) -> Option<MidiEvent> {
        let byte = |value: f32| (value * 127.0).round().clamp(0.0, 127.0) as u8;
        match &self.data {
            PluginEventData::NoteOn {
                channel,
                pitch,
                velocity,
                ..
            } => Some(MidiEvent::NoteOn {
                channel: MidiChannel::from_index(u8::try_from(*channel).ok()?)?,
                note: u8::try_from(*pitch).ok().filter(|pitch| *pitch <= 127)?,
                velocity: byte(*velocity),
            }),
            PluginEventData::NoteOff {
                channel,
                pitch,
                velocity,
                ..
            } => Some(MidiEvent::NoteOff {
                channel: MidiChannel::from_index(u8::try_from(*channel).ok()?)?,
                note: u8::try_from(*pitch).ok().filter(|pitch| *pitch <= 127)?,
                velocity: byte(*velocity),
            }),
            PluginEventData::PolyPressure {
                channel,
                pitch,
                pressure,
                ..
            } => Some(MidiEvent::PolyAftertouch {
                channel: MidiChannel::from_index(u8::try_from(*channel).ok()?)?,
                note: u8::try_from(*pitch).ok().filter(|pitch| *pitch <= 127)?,
                pressure: byte(*pressure),
            }),
            PluginEventData::LegacyMidiCcOut {
                control_number,
                channel,
                value,
                value2,
            } => {
                let channel = MidiChannel::from_index(u8::try_from(*channel).ok()?)?;
                match u32::from(*control_number) {
                    129 => Some(MidiEvent::PitchBend {
                        channel,
                        value: (u16::from(*value2 & 0x7f) << 7) | u16::from(*value & 0x7f),
                    }),
                    128 => Some(MidiEvent::ChannelAftertouch {
                        channel,
                        pressure: *value & 0x7f,
                    }),
                    130 => Some(MidiEvent::ProgramChange {
                        channel,
                        program: *value & 0x7f,
                    }),
                    cc if cc < 128 => Some(MidiEvent::ControlChange {
                        channel,
                        controller: cc as u8,
                        value: *value & 0x7f,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Bytes owned by pointer-backed fields in this event.
    pub(crate) fn payload_bytes(&self) -> usize {
        match &self.data {
            PluginEventData::Data { bytes, .. } => bytes.len(),
            PluginEventData::NoteExpressionText { text, .. }
            | PluginEventData::Chord { text, .. }
            | PluginEventData::Scale { text, .. } => text.len().saturating_mul(2),
            _ => 0,
        }
    }
}

impl From<MidiEvent> for PluginEvent {
    fn from(event: MidiEvent) -> Self {
        let data = match event {
            MidiEvent::NoteOn {
                channel,
                note,
                velocity,
            } => PluginEventData::NoteOn {
                channel: i16::from(channel.as_index()),
                pitch: i16::from(note),
                tuning: 0.0,
                velocity: f32::from(velocity) / 127.0,
                length: 0,
                note_id: -1,
            },
            MidiEvent::NoteOff {
                channel,
                note,
                velocity,
            } => PluginEventData::NoteOff {
                channel: i16::from(channel.as_index()),
                pitch: i16::from(note),
                velocity: f32::from(velocity) / 127.0,
                note_id: -1,
                tuning: 0.0,
            },
            MidiEvent::ControlChange {
                channel,
                controller,
                value,
            } => PluginEventData::LegacyMidiCcOut {
                control_number: controller,
                channel: channel.as_index() as i8,
                value,
                value2: 0,
            },
            MidiEvent::ProgramChange { channel, program } => PluginEventData::LegacyMidiCcOut {
                control_number: 130,
                channel: channel.as_index() as i8,
                value: program,
                value2: 0,
            },
            MidiEvent::PitchBend { channel, value } => PluginEventData::LegacyMidiCcOut {
                control_number: 129,
                channel: channel.as_index() as i8,
                value: (value & 0x7f) as u8,
                value2: ((value >> 7) & 0x7f) as u8,
            },
            MidiEvent::ChannelAftertouch { channel, pressure } => {
                PluginEventData::LegacyMidiCcOut {
                    control_number: 128,
                    channel: channel.as_index() as i8,
                    value: pressure,
                    value2: 0,
                }
            }
            MidiEvent::PolyAftertouch {
                channel,
                note,
                pressure,
            } => PluginEventData::PolyPressure {
                channel: i16::from(channel.as_index()),
                pitch: i16::from(note),
                pressure: f32::from(pressure) / 127.0,
                note_id: -1,
            },
        };
        Self {
            bus_index: 0,
            sample_offset: 0,
            ppq_position: 0.0,
            flags: 0,
            data,
        }
    }
}

/// An opaque per-voice handle returned by [`Plugin::note_on`](crate::Plugin::note_on), used to
/// target note-expression events (and the note-off) at a specific sounding note — the basis for
/// MPE-style per-note control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NoteId(pub(crate) i32);

impl NoteId {
    /// The raw VST3 note id.
    pub fn raw(self) -> i32 {
        self.0
    }

    /// Reconstruct a [`NoteId`] from a raw VST3 note id.
    ///
    /// A `NoteId` is normally minted by [`Plugin::note_on`](crate::Plugin::note_on); this is
    /// the inverse of [`raw`](Self::raw), used to carry an id across the process-isolation
    /// boundary (the helper owns the plugin and allocates the id; the host re-wraps it).
    pub fn from_raw(raw: i32) -> Self {
        NoteId(raw)
    }
}

/// A VST3 per-note expression dimension. Values are normalized `0.0..=1.0`; the bipolar
/// dimensions (Pan, Tuning) center at `0.5`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum NoteExpressionType {
    /// Per-note volume (`kVolumeTypeID`).
    Volume,
    /// Per-note pan, bipolar (`kPanTypeID`).
    Pan,
    /// Per-note tuning / pitch, bipolar (`kTuningTypeID`).
    Tuning,
    /// Per-note vibrato (`kVibratoTypeID`).
    Vibrato,
    /// Per-note expression (`kExpressionTypeID`).
    Expression,
    /// Per-note brightness / timbre (`kBrightnessTypeID`).
    Brightness,
    /// A plugin-defined custom expression type id (`kCustomStart..kCustomEnd`).
    Custom(u32),
}

impl NoteExpressionType {
    /// The VST3 `NoteExpressionTypeID` for this dimension.
    pub(crate) fn type_id(self) -> u32 {
        match self {
            NoteExpressionType::Volume => 0,
            NoteExpressionType::Pan => 1,
            NoteExpressionType::Tuning => 2,
            NoteExpressionType::Vibrato => 3,
            NoteExpressionType::Expression => 4,
            NoteExpressionType::Brightness => 5,
            NoteExpressionType::Custom(id) => id,
        }
    }

    /// Map a VST3 `NoteExpressionTypeID` back to a type (unknown ids become `Custom`).
    pub(crate) fn from_type_id(id: u32) -> Self {
        match id {
            0 => NoteExpressionType::Volume,
            1 => NoteExpressionType::Pan,
            2 => NoteExpressionType::Tuning,
            3 => NoteExpressionType::Vibrato,
            4 => NoteExpressionType::Expression,
            5 => NoteExpressionType::Brightness,
            other => NoteExpressionType::Custom(other),
        }
    }
}

/// A note-expression dimension a plugin advertises via `INoteExpressionController`
/// (from [`Plugin::note_expressions`](crate::Plugin::note_expressions)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteExpressionInfo {
    /// Which expression dimension this is.
    pub kind: NoteExpressionType,
    /// Display title (e.g. "Tuning").
    pub title: String,
    /// Short title.
    pub short_title: String,
    /// Units string (may be empty).
    pub units: String,
    /// Default normalized value.
    pub default_value: f64,
    /// Minimum normalized value.
    pub min: f64,
    /// Maximum normalized value.
    pub max: f64,
    /// Discrete step count (0 = continuous).
    pub step_count: i32,
    /// Whether the dimension is bipolar (centered at 0.5).
    pub is_bipolar: bool,
    /// Whether it's a one-shot (applied once at note start).
    pub is_one_shot: bool,
    /// Whether the value is absolute (vs relative to the note's base).
    pub is_absolute: bool,
}

impl MidiEvent {
    /// Parse a single channel-voice MIDI message from raw bytes (status + data), as delivered
    /// by a MIDI input device.
    ///
    /// Maps Note On/Off (a Note On with velocity 0 becomes a Note Off), Control Change,
    /// Pitch Bend (14-bit), channel/poly aftertouch, and Program Change. Returns `None` for
    /// empty/truncated input, running-status messages (no leading status byte), and
    /// system/realtime/SysEx messages.
    pub fn from_midi_bytes(bytes: &[u8]) -> Option<MidiEvent> {
        let status = *bytes.first()?;
        // Require a channel-voice status byte (0x80..=0xEF); reject data bytes (running status)
        // and system/realtime messages (0xF0..=0xFF).
        if !(0x80..0xF0).contains(&status) {
            return None;
        }
        let channel = MidiChannel::from_index(status & 0x0F)?;
        let d1 = || bytes.get(1).map(|b| b & 0x7F);
        let d2 = || bytes.get(2).map(|b| b & 0x7F);
        match status & 0xF0 {
            0x90 => {
                let note = d1()?;
                let velocity = d2()?;
                Some(if velocity == 0 {
                    MidiEvent::NoteOff {
                        channel,
                        note,
                        velocity: 0,
                    }
                } else {
                    MidiEvent::NoteOn {
                        channel,
                        note,
                        velocity,
                    }
                })
            }
            0x80 => Some(MidiEvent::NoteOff {
                channel,
                note: d1()?,
                velocity: d2()?,
            }),
            0xB0 => Some(MidiEvent::ControlChange {
                channel,
                controller: d1()?,
                value: d2()?,
            }),
            0xA0 => Some(MidiEvent::PolyAftertouch {
                channel,
                note: d1()?,
                pressure: d2()?,
            }),
            0xD0 => Some(MidiEvent::ChannelAftertouch {
                channel,
                pressure: d1()?,
            }),
            0xE0 => {
                let value = (d2()? as u16) << 7 | d1()? as u16;
                Some(MidiEvent::PitchBend { channel, value })
            }
            0xC0 => Some(MidiEvent::ProgramChange {
                channel,
                program: d1()?,
            }),
            _ => None,
        }
    }
}

/// Common MIDI control change numbers
pub mod cc {
    /// Bank Select MSB
    pub const BANK_SELECT_MSB: u8 = 0;
    /// Modulation Wheel
    pub const MODULATION: u8 = 1;
    /// Breath Controller
    pub const BREATH: u8 = 2;
    /// Foot Controller
    pub const FOOT: u8 = 4;
    /// Portamento Time
    pub const PORTAMENTO_TIME: u8 = 5;
    /// Data Entry MSB
    pub const DATA_ENTRY_MSB: u8 = 6;
    /// Channel Volume
    pub const VOLUME: u8 = 7;
    /// Balance
    pub const BALANCE: u8 = 8;
    /// Pan
    pub const PAN: u8 = 10;
    /// Expression
    pub const EXPRESSION: u8 = 11;
    /// Sustain Pedal
    pub const SUSTAIN: u8 = 64;
    /// Portamento On/Off
    pub const PORTAMENTO: u8 = 65;
    /// Sostenuto
    pub const SOSTENUTO: u8 = 66;
    /// Soft Pedal
    pub const SOFT_PEDAL: u8 = 67;
    /// Legato Footswitch
    pub const LEGATO: u8 = 68;
    /// Hold 2
    pub const HOLD_2: u8 = 69;
    /// Sound Controller 1 (default: Sound Variation)
    pub const SOUND_CONTROLLER_1: u8 = 70;
    /// Sound Controller 2 (default: Timbre/Harmonic Content)
    pub const SOUND_CONTROLLER_2: u8 = 71;
    /// Sound Controller 3 (default: Release Time)
    pub const SOUND_CONTROLLER_3: u8 = 72;
    /// Sound Controller 4 (default: Attack Time)
    pub const SOUND_CONTROLLER_4: u8 = 73;
    /// Sound Controller 5 (default: Brightness)
    pub const SOUND_CONTROLLER_5: u8 = 74;
    /// Sound Controller 6-10
    pub const SOUND_CONTROLLER_6: u8 = 75;
    /// Sound controller 7
    pub const SOUND_CONTROLLER_7: u8 = 76;
    /// Sound controller 8
    pub const SOUND_CONTROLLER_8: u8 = 77;
    /// Sound controller 9
    pub const SOUND_CONTROLLER_9: u8 = 78;
    /// Sound controller 10
    pub const SOUND_CONTROLLER_10: u8 = 79;
    /// General Purpose Controllers
    pub const GENERAL_PURPOSE_1: u8 = 80;
    /// General purpose controller 2
    pub const GENERAL_PURPOSE_2: u8 = 81;
    /// General purpose controller 3
    pub const GENERAL_PURPOSE_3: u8 = 82;
    /// General purpose controller 4
    pub const GENERAL_PURPOSE_4: u8 = 83;
    /// Portamento Control
    pub const PORTAMENTO_CONTROL: u8 = 84;
    /// Effects Depth
    pub const REVERB_DEPTH: u8 = 91;
    /// Tremolo depth
    pub const TREMOLO_DEPTH: u8 = 92;
    /// Chorus depth
    pub const CHORUS_DEPTH: u8 = 93;
    /// Celeste depth
    pub const CELESTE_DEPTH: u8 = 94;
    /// Phaser depth
    pub const PHASER_DEPTH: u8 = 95;
    /// Data Increment
    pub const DATA_INCREMENT: u8 = 96;
    /// Data Decrement
    pub const DATA_DECREMENT: u8 = 97;
    /// NRPN LSB
    pub const NRPN_LSB: u8 = 98;
    /// NRPN MSB
    pub const NRPN_MSB: u8 = 99;
    /// RPN LSB
    pub const RPN_LSB: u8 = 100;
    /// RPN MSB
    pub const RPN_MSB: u8 = 101;
    /// All Sounds Off
    pub const ALL_SOUNDS_OFF: u8 = 120;
    /// Reset All Controllers
    pub const RESET_ALL_CONTROLLERS: u8 = 121;
    /// Local Control On/Off
    pub const LOCAL_CONTROL: u8 = 122;
    /// All Notes Off
    pub const ALL_NOTES_OFF: u8 = 123;
    /// Omni Mode Off
    pub const OMNI_MODE_OFF: u8 = 124;
    /// Omni Mode On
    pub const OMNI_MODE_ON: u8 = 125;
    /// Mono Mode On
    pub const MONO_MODE_ON: u8 = 126;
    /// Poly Mode On
    pub const POLY_MODE_ON: u8 = 127;
}

/// Convert a MIDI note number to its note name, using the convention where C3 = MIDI 60.
///
/// The MIDI note domain is `0..=127`; the parameter is a `u8`, so larger values are
/// representable and are rendered as `"Invalid(<n>)"`. They used to be given a fabricated
/// name (`"D#19"` for 255) that [`name_to_note`] correctly rejects, silently breaking the
/// round trip at the boundary where a note number becomes text and back.
pub fn note_to_name(note: u8) -> String {
    if note > 127 {
        return format!("Invalid({note})");
    }
    let note_names = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = (note as i32 / 12) - 2;
    let note_in_octave = note % 12;
    format!("{}{}", note_names[note_in_octave as usize], octave)
}

/// Convert note name to MIDI note number
/// Accepts formats like "C3", "C#4", "Db3", etc.
/// Using the convention where C3 = MIDI 60
pub fn name_to_note(name: &str) -> Option<u8> {
    let name = name.trim().to_uppercase();

    // Parse by chars, never by byte index: `to_uppercase` can produce multi-byte chars (an
    // accented letter, say), and slicing those at byte 1 would panic rather than returning None.
    let mut chars = name.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }

    // An optional accidental follows the letter: '#' (sharp) or 'B' (flat, as in "Db").
    // A bare "B..." is the note B, not a flat — only treat 'B' as an accidental when it is the
    // *second* char.
    let rest = chars.as_str();
    let (accidental, octave_str) = match rest.chars().next() {
        Some('#') => (Some('#'), &rest[1..]),
        Some('B') => (Some('B'), &rest[1..]),
        _ => (None, rest),
    };

    // Parse octave
    let octave: i32 = octave_str.parse().ok()?;

    // Convert note to semitone offset within octave
    let semitone = match (letter, accidental) {
        ('C', None) => 0,
        ('C', Some('#')) | ('D', Some('B')) => 1,
        ('D', None) => 2,
        ('D', Some('#')) | ('E', Some('B')) => 3,
        ('E', None) => 4,
        ('F', None) => 5,
        ('F', Some('#')) | ('G', Some('B')) => 6,
        ('G', None) => 7,
        ('G', Some('#')) | ('A', Some('B')) => 8,
        ('A', None) => 9,
        ('A', Some('#')) | ('B', Some('B')) => 10,
        ('B', None) => 11,
        _ => return None,
    };

    // Calculate MIDI note number, using the convention where C3 = MIDI 60.
    // Checked: the octave comes from parsing arbitrary text, and `(octave + 2) * 12` overflows for
    // extremes like "C2147483647" — a panic in debug, a wrapped (wrong) note in release.
    let midi_note = octave
        .checked_add(2)
        .and_then(|o| o.checked_mul(12))
        .and_then(|base| base.checked_add(semitone))?;

    if (0..=127).contains(&midi_note) {
        Some(midi_note as u8)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_expression_type_ids_round_trip() {
        for kind in [
            NoteExpressionType::Volume,
            NoteExpressionType::Pan,
            NoteExpressionType::Tuning,
            NoteExpressionType::Vibrato,
            NoteExpressionType::Expression,
            NoteExpressionType::Brightness,
            NoteExpressionType::Custom(100_001),
        ] {
            assert_eq!(NoteExpressionType::from_type_id(kind.type_id()), kind);
        }
        // The well-known VST3 type ids.
        assert_eq!(NoteExpressionType::Tuning.type_id(), 2);
        assert_eq!(
            NoteExpressionType::from_type_id(5),
            NoteExpressionType::Brightness
        );
    }

    #[test]
    fn from_midi_bytes_maps_channel_voice_messages() {
        // Note on (ch 1, note 60, vel 100).
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0x90, 60, 100]),
            Some(MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 100
            })
        );
        // Note on velocity 0 => note off.
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0x90, 60, 0]),
            Some(MidiEvent::NoteOff {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 0
            })
        );
        // Note off on channel 10.
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0x89, 64, 40]),
            Some(MidiEvent::NoteOff {
                channel: MidiChannel::Ch10,
                note: 64,
                velocity: 40
            })
        );
        // CC.
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0xB0, 1, 64]),
            Some(MidiEvent::ControlChange {
                channel: MidiChannel::Ch1,
                controller: 1,
                value: 64
            })
        );
        // Channel + poly aftertouch.
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0xD0, 90]),
            Some(MidiEvent::ChannelAftertouch {
                channel: MidiChannel::Ch1,
                pressure: 90
            })
        );
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0xA0, 60, 70]),
            Some(MidiEvent::PolyAftertouch {
                channel: MidiChannel::Ch1,
                note: 60,
                pressure: 70
            })
        );
    }

    #[test]
    fn from_midi_bytes_pitch_bend_is_14_bit() {
        // Center: LSB 0, MSB 64 -> 8192.
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0xE0, 0, 64]),
            Some(MidiEvent::PitchBend {
                channel: MidiChannel::Ch1,
                value: 8192
            })
        );
        // Max: LSB 127, MSB 127 -> 16383.
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0xE0, 127, 127]),
            Some(MidiEvent::PitchBend {
                channel: MidiChannel::Ch1,
                value: 16383
            })
        );
    }

    #[test]
    fn from_midi_bytes_rejects_unsupported_and_junk() {
        assert_eq!(MidiEvent::from_midi_bytes(&[]), None); // empty
        assert_eq!(MidiEvent::from_midi_bytes(&[0x60]), None); // data byte, not status
        assert_eq!(MidiEvent::from_midi_bytes(&[0xF8]), None); // realtime clock
        assert_eq!(MidiEvent::from_midi_bytes(&[0xF0, 1, 2]), None); // sysex
        assert_eq!(MidiEvent::from_midi_bytes(&[0x90, 60]), None); // truncated note on
    }

    #[test]
    fn from_midi_bytes_maps_program_change() {
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0xC0, 5]),
            Some(MidiEvent::ProgramChange {
                channel: MidiChannel::Ch1,
                program: 5
            })
        );
        // Channel is taken from the low nibble; the program byte is masked to 7 bits.
        assert_eq!(
            MidiEvent::from_midi_bytes(&[0xC9, 0xFF]),
            Some(MidiEvent::ProgramChange {
                channel: MidiChannel::Ch10,
                program: 127
            })
        );
        // Truncated (no program byte) is rejected.
        assert_eq!(MidiEvent::from_midi_bytes(&[0xC0]), None);
    }

    #[test]
    fn test_midi_conversions() {
        // Test some known values using C3=60 convention
        assert_eq!(name_to_note("C3"), Some(60));
        assert_eq!(name_to_note("C2"), Some(48));
        assert_eq!(name_to_note("A3"), Some(69)); // Concert A
        assert_eq!(name_to_note("C-2"), Some(0));
        assert_eq!(name_to_note("G8"), Some(127));

        // Test reverse conversion
        assert_eq!(note_to_name(60), "C3");
        assert_eq!(note_to_name(48), "C2");
        assert_eq!(note_to_name(69), "A3");
        assert_eq!(note_to_name(0), "C-2");
        assert_eq!(note_to_name(127), "G8");

        // Test accidentals
        assert_eq!(name_to_note("C#3"), Some(61));
        assert_eq!(name_to_note("Db3"), Some(61));
        assert_eq!(name_to_note("F#3"), Some(66));
    }

    /// `name_to_note` is a safe `Option`-returning parser fed by UI text fields and config files,
    /// so every input has to come back as `None` rather than a panic. It used to slice at byte
    /// index 1 to detect a flat, which panics whenever the uppercased first char is multi-byte.
    #[test]
    fn name_to_note_rejects_junk_without_panicking() {
        for junk in [
            "éB3",    // multi-byte first char — used to panic on a non-char-boundary slice
            "ÉB3",    //
            "日本語", // no ASCII at all
            "",       // empty
            "3",      // no note letter
            "H3",     // not a note name
            "C",      // no octave
            "C#",     // accidental, no octave
            "Cb3",    // Cb is not one of the accidentals we map
            "C##3",   // double sharp
            "CB3",    // uppercase input is normalised, but Cb still isn't mapped
            "C99",    // octave out of MIDI range
            "C-99",   //
            "#3",     // accidental with no letter
            // Octave arithmetic has to be checked: `(octave + 2) * 12` overflows on these, which
            // panics in a debug build and silently returns a wrong note in a release one.
            "C2147483647",
            "C-2147483648",
            "Bb2147483647",
            "C#2147483647",
        ] {
            assert_eq!(name_to_note(junk), None, "expected None for {junk:?}");
        }
    }

    /// A bare "B" is the note B, not a flat — the flat form only applies when 'B' is the *second*
    /// character. Both readings go through the same branch, so pin them together.
    #[test]
    fn name_to_note_distinguishes_b_natural_from_flats() {
        assert_eq!(name_to_note("B3"), Some(71));
        assert_eq!(name_to_note("Bb3"), Some(70));
        assert_eq!(name_to_note("bb3"), Some(70)); // case-insensitive
        assert_eq!(name_to_note("Eb3"), Some(63));
        assert_eq!(name_to_note("Ab3"), Some(68));
        // Round-trip every note through its own name.
        for n in 0..=127u8 {
            assert_eq!(name_to_note(&note_to_name(n)), Some(n), "round-trip {n}");
        }
    }

    /// `note_to_name` takes a `u8`, so 128..=255 are representable but outside the MIDI note
    /// domain. They used to be given a fabricated name ("D#19") that `name_to_note` rejects,
    /// so the two functions disagreed about what a valid note name is.
    #[test]
    fn note_to_name_marks_out_of_domain_notes_instead_of_fabricating_one() {
        for n in 128..=255u8 {
            let name = note_to_name(n);
            assert_eq!(name, format!("Invalid({n})"));
            assert_eq!(
                name_to_note(&name),
                None,
                "{name} must not parse back as a note"
            );
        }
        // And the domain that does round-trip is unchanged, in both directions.
        for n in 0..=127u8 {
            let name = note_to_name(n);
            assert!(!name.starts_with("Invalid"), "note {n} rendered as {name}");
            assert_eq!(name_to_note(&name), Some(n), "round-trip {n}");
        }
    }

    #[test]
    fn test_midi_channel() {
        assert_eq!(MidiChannel::Ch1.as_index(), 0);
        assert_eq!(MidiChannel::Ch16.as_index(), 15);
        assert_eq!(MidiChannel::from_index(0), Some(MidiChannel::Ch1));
        assert_eq!(MidiChannel::from_index(15), Some(MidiChannel::Ch16));
        assert_eq!(MidiChannel::from_index(16), None);
    }
}

/// Controller address understood by VST3 MIDI mapping and learning interfaces.
/// MIDI 1 numbers include aftertouch (128), pitch bend (129), and program change (130).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MidiController {
    /// MIDI 1 CC or extended controller number (0..=130).
    Midi1(u16),
    /// MIDI 2 registered controller (MIDI 1 RPN); bank/index are 7-bit values.
    Registered {
        /// Controller bank.
        bank: u8,
        /// Controller index.
        index: u8,
    },
    /// MIDI 2 assignable controller (MIDI 1 NRPN); bank/index are 7-bit values.
    Assignable {
        /// Controller bank.
        bank: u8,
        /// Controller index.
        index: u8,
    },
}

impl MidiController {
    pub(crate) fn is_valid(self) -> bool {
        match self {
            Self::Midi1(cc) => cc <= 130,
            Self::Registered { bank, index } | Self::Assignable { bank, index } => {
                bank < 128 && index < 128
            }
        }
    }
}

/// One controller-to-parameter assignment. A controller may have multiple assignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiControllerAssignment {
    /// Event bus index.
    pub bus: i32,
    /// MIDI channel.
    pub channel: MidiChannel,
    /// Controller address.
    pub controller: MidiController,
    /// Target parameter id.
    pub parameter_id: u32,
}

/// A plugin's advertised articulation/keyswitch, copied from IKeyswitchController.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyswitchInfo {
    /// SDK keyswitch type id (unknown types are preserved).
    pub type_id: u32,
    /// Display name.
    pub title: String,
    /// Abbreviated display name.
    pub short_title: String,
    /// First MIDI key in the inclusive range.
    pub key_min: i32,
    /// Last MIDI key in the inclusive range.
    pub key_max: i32,
    /// Remapped key, or None when the plugin reports no remapping.
    pub remapped_key: Option<i32>,
    /// Owning plugin unit.
    pub unit_id: i32,
    /// SDK flags, preserved for future extensions.
    pub flags: i32,
}

impl MidiEvent {
    pub(crate) fn controller_address(self) -> Option<(MidiChannel, MidiController)> {
        match self {
            Self::ControlChange {
                channel,
                controller,
                ..
            } => Some((channel, MidiController::Midi1(controller as u16))),
            Self::ChannelAftertouch { channel, .. } => Some((channel, MidiController::Midi1(128))),
            Self::PitchBend { channel, .. } => Some((channel, MidiController::Midi1(129))),
            Self::ProgramChange { channel, .. } => Some((channel, MidiController::Midi1(130))),
            _ => None,
        }
    }
}
