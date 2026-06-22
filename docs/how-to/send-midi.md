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

## Sample-accurate timing

`send_midi_event` and `send_midi_note` deliver at the start of the next processed block.
For sample-accurate sequencing, `send_midi_event_at` schedules an event at a sample offset
*within* the next block:

```rust
# use vst3_host::{simple, midi::{MidiEvent, MidiChannel}};
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
let note = MidiEvent::NoteOn { channel: MidiChannel::Ch1, note: 60, velocity: 110 };
plugin.send_midi_event_at(note, 256)?;   // sounds 256 frames into the next block
# Ok(())
# }
```

Keep the offset within the upcoming block's frame count (`plugin.block_size()` is the
maximum). Under [process isolation](../explanation/process-isolation.md) the offset is not
marshalled across the boundary — the event lands at block start.

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
plugin processes, so it only flows while the plugin is playing. This also works for plugins
running under [process isolation](isolate-plugin-crashes.md) — emitted events are returned
alongside each processed audio block.

## Forward MIDI from a hardware controller

To drive a plugin from a MIDI keyboard, parse the raw bytes your MIDI library delivers with
`MidiEvent::from_midi_bytes`, then forward each event:

```rust
# use vst3_host::{simple, midi::MidiEvent};
# fn main() -> vst3_host::Result<()> {
# let audio = simple::play(simple::load_plugin("/x.vst3")?)?;
# let raw: &[u8] = &[0x90, 60, 100];
// `raw` is one MIDI message (status + data) from your device callback.
if let Some(event) = MidiEvent::from_midi_bytes(raw) {
    audio.lock().send_midi_event(event)?;
}
# Ok(())
# }
```

It maps note on/off (velocity-0 note-on becomes note-off), control change, pitch bend, and
aftertouch, and returns `None` for messages the library doesn't carry (program change,
system/realtime, SysEx). Do the device I/O on its own thread and hand events to the audio
thread through a channel — never call the plugin from the device callback. (The inspector's
"MIDI Input Device" picker does exactly this, cross-platform, via the `midir` crate.)

## Caveats

- **Program Change is not supported.** VST3 switches programs through `IUnitInfo` program
  lists, not MIDI events, so `MidiEvent::ProgramChange` returns an error. This is a known
  gap, not a silent no-op.
