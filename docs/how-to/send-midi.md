# Send MIDI

Send notes and controllers to a loaded plugin. Channels are `MidiChannel::Ch1`–`Ch16`;
notes are `0–127` with C3 = 60.

## Notes

```rust
# use vst3_host::{simple, midi::MidiChannel};
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
plugin.send_midi_note(60, 100, MidiChannel::Ch1)?;       // note on: note, velocity, channel
plugin.send_midi_note_off(60, MidiChannel::Ch1)?;        // note off
# Ok(())
# }
```

## Control change, pitch bend, aftertouch

```rust
# use vst3_host::{simple, midi::{MidiChannel, MidiEvent, cc}};
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
plugin.send_midi_cc(cc::MODULATION, 64, MidiChannel::Ch1)?;     // mod wheel
plugin.send_midi_event(MidiEvent::PitchBend { channel: MidiChannel::Ch1, value: 10000 })?;
plugin.send_midi_event(MidiEvent::ChannelAftertouch { channel: MidiChannel::Ch1, pressure: 80 })?;
plugin.send_midi_event(MidiEvent::PolyAftertouch { channel: MidiChannel::Ch1, note: 60, pressure: 80 })?;
# Ok(())
# }
```

The `cc` module has named constants (`MODULATION`, `VOLUME`, `SUSTAIN`, `PAN`, …). Pitch
bend is a 14-bit value (`0–16383`, center `8192`).

## Panic (all notes off)

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
plugin.midi_panic()?;   // stop every stuck note
# Ok(())
# }
```

## While playing

If the plugin is inside an [`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html),
send through the lock: `audio.lock().send_midi_note(...)`.

## Note names

```rust
use vst3_host::midi::{note_to_name, name_to_note};
assert_eq!(note_to_name(60), "C3");
assert_eq!(name_to_note("C3"), Some(60));
```

## Read MIDI the plugin emits

Some plugins emit MIDI — arpeggiators, MPE controllers, sequencers. While the plugin is
processing, poll `take_output_midi` to drain the events it produced:

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let audio = simple::play(simple::load_plugin("/x.vst3")?)?;
for event in audio.lock().take_output_midi() {
    println!("plugin emitted: {event:?}");
}
# Ok(())
# }
```

Call it regularly (e.g. each UI frame). Output MIDI is captured on the audio thread as the
plugin processes, so it only flows while the plugin is playing. Plugins running under
[process isolation](isolate-plugin-crashes.md) don't capture output MIDI yet.

## Caveats

- **Program Change is not supported.** VST3 switches programs through `IUnitInfo` program
  lists, not MIDI events, so `MidiEvent::ProgramChange` returns an error. This is a known
  gap, not a silent no-op.
