#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)] // Output is for plugin-emitted MIDI monitoring (not yet wired)
pub enum MidiDirection {
    Input,
    Output,
}
