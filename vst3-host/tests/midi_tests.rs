use vst3_host::midi::*;

#[test]
fn test_midi_channel_conversion() {
    assert_eq!(MidiChannel::Ch1.as_index(), 0);
    assert_eq!(MidiChannel::Ch10.as_index(), 9);
    assert_eq!(MidiChannel::Ch16.as_index(), 15);

    assert_eq!(MidiChannel::from_index(0), Some(MidiChannel::Ch1));
    assert_eq!(MidiChannel::from_index(9), Some(MidiChannel::Ch10));
    assert_eq!(MidiChannel::from_index(15), Some(MidiChannel::Ch16));
    assert_eq!(MidiChannel::from_index(16), None);
}

#[test]
fn test_note_name_conversion() {
    // Test basic notes
    assert_eq!(note_to_name(60), "C3");
    assert_eq!(note_to_name(69), "A3");
    assert_eq!(note_to_name(48), "C2");
    assert_eq!(note_to_name(72), "C4");

    // Test sharps
    assert_eq!(note_to_name(61), "C#3");
    assert_eq!(note_to_name(70), "A#3");

    // Test extremes
    assert_eq!(note_to_name(0), "C-2");
    assert_eq!(note_to_name(127), "G8");
}

#[test]
fn test_name_to_note_conversion() {
    // Test basic notes
    assert_eq!(name_to_note("C3"), Some(60));
    assert_eq!(name_to_note("A3"), Some(69));
    assert_eq!(name_to_note("C2"), Some(48));
    assert_eq!(name_to_note("C4"), Some(72));

    // Test sharps
    assert_eq!(name_to_note("C#3"), Some(61));
    assert_eq!(name_to_note("A#3"), Some(70));

    // Test flats
    assert_eq!(name_to_note("Db3"), Some(61));
    assert_eq!(name_to_note("Bb3"), Some(70));

    // Test case insensitivity
    assert_eq!(name_to_note("c3"), Some(60));
    assert_eq!(name_to_note("C#3"), Some(61));
    assert_eq!(name_to_note("db3"), Some(61));

    // Test extremes
    assert_eq!(name_to_note("C-2"), Some(0));
    assert_eq!(name_to_note("G8"), Some(127));

    // Test invalid inputs
    assert_eq!(name_to_note("H3"), None);
    assert_eq!(name_to_note("C9"), None);
    assert_eq!(name_to_note(""), None);
}

#[test]
fn test_midi_events() {
    let note_on = MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 100,
    };

    match note_on {
        MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        } => {
            assert_eq!(channel, MidiChannel::Ch1);
            assert_eq!(note, 60);
            assert_eq!(velocity, 100);
        }
        _ => panic!("Wrong event type"),
    }

    let cc = MidiEvent::ControlChange {
        channel: MidiChannel::Ch16,
        controller: cc::MODULATION,
        value: 64,
    };

    match cc {
        MidiEvent::ControlChange {
            channel,
            controller,
            value,
        } => {
            assert_eq!(channel, MidiChannel::Ch16);
            assert_eq!(controller, 1);
            assert_eq!(value, 64);
        }
        _ => panic!("Wrong event type"),
    }
}

#[test]
fn test_cc_constants() {
    assert_eq!(cc::MODULATION, 1);
    assert_eq!(cc::VOLUME, 7);
    assert_eq!(cc::PAN, 10);
    assert_eq!(cc::SUSTAIN, 64);
    assert_eq!(cc::ALL_NOTES_OFF, 123);
    assert_eq!(cc::ALL_SOUNDS_OFF, 120);
}
