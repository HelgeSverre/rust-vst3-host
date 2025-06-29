//! MIDI types and utilities for VST3 host

use std::fmt;

/// MIDI channel enumeration (1-16)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// High-level MIDI event types
#[derive(Debug, Clone, Copy, PartialEq)]
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
    pub const SOUND_CONTROLLER_7: u8 = 76;
    pub const SOUND_CONTROLLER_8: u8 = 77;
    pub const SOUND_CONTROLLER_9: u8 = 78;
    pub const SOUND_CONTROLLER_10: u8 = 79;
    /// General Purpose Controllers
    pub const GENERAL_PURPOSE_1: u8 = 80;
    pub const GENERAL_PURPOSE_2: u8 = 81;
    pub const GENERAL_PURPOSE_3: u8 = 82;
    pub const GENERAL_PURPOSE_4: u8 = 83;
    /// Portamento Control
    pub const PORTAMENTO_CONTROL: u8 = 84;
    /// Effects Depth
    pub const REVERB_DEPTH: u8 = 91;
    pub const TREMOLO_DEPTH: u8 = 92;
    pub const CHORUS_DEPTH: u8 = 93;
    pub const CELESTE_DEPTH: u8 = 94;
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

/// Convert MIDI note number to note name
/// Using the convention where C3 = MIDI 60
pub fn note_to_name(note: u8) -> String {
    let note_names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let octave = (note as i32 / 12) - 2;
    let note_in_octave = note % 12;
    format!("{}{}", note_names[note_in_octave as usize], octave)
}

/// Convert note name to MIDI note number
/// Accepts formats like "C3", "C#4", "Db3", etc.
/// Using the convention where C3 = MIDI 60
pub fn name_to_note(name: &str) -> Option<u8> {
    let name = name.trim().to_uppercase();
    
    // Extract the note letter and accidental
    let (note_part, octave_str) = if name.contains('#') {
        let parts: Vec<&str> = name.split('#').collect();
        if parts.len() != 2 { return None; }
        (format!("{}#", parts[0]), parts[1])
    } else if name.contains('B') && name.len() > 2 && &name[1..2] == "B" {
        // Handle Bb notation
        (format!("{}B", &name[0..1]), &name[2..])
    } else {
        // Natural note
        let mut chars = name.chars();
        let note = chars.next()?.to_string();
        let octave = chars.as_str();
        (note, octave)
    };
    
    // Parse octave
    let octave: i32 = octave_str.parse().ok()?;
    
    // Convert note to semitone offset within octave
    let semitone = match note_part.as_str() {
        "C" => 0,
        "C#" | "DB" => 1,
        "D" => 2,
        "D#" | "EB" => 3,
        "E" => 4,
        "F" => 5,
        "F#" | "GB" => 6,
        "G" => 7,
        "G#" | "AB" => 8,
        "A" => 9,
        "A#" | "BB" => 10,
        "B" => 11,
        _ => return None,
    };
    
    // Calculate MIDI note number
    // Using the convention where C3 = MIDI 60
    let midi_note = (octave + 2) * 12 + semitone;
    
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
    
    #[test]
    fn test_midi_channel() {
        assert_eq!(MidiChannel::Ch1.as_index(), 0);
        assert_eq!(MidiChannel::Ch16.as_index(), 15);
        assert_eq!(MidiChannel::from_index(0), Some(MidiChannel::Ch1));
        assert_eq!(MidiChannel::from_index(15), Some(MidiChannel::Ch16));
        assert_eq!(MidiChannel::from_index(16), None);
    }
}