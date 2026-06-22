#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MidiDirection {
    /// MIDI the host sends into the plugin.
    Input,
    /// MIDI the plugin emits (arpeggiators, MPE, …), shown in the monitor.
    Output,
}
