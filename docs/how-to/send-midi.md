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

### These become parameter changes, not MIDI events

VST3 has no MIDI controller event. A plugin instead declares, through `IMidiMapping`, which
of *its own parameters* each controller drives. So control change, pitch bend and channel
aftertouch are translated: the library looks the controller up in the plugin's mapping table
and queues a sample-accurate change to the mapped parameter (mirrored to the controller, so
the plugin's editor follows along). Poly aftertouch is different — it is a first-class VST3
event and is always delivered as one.

| You send | The plugin receives |
|---|---|
| `ControlChange { controller: n }` | the parameter `IMidiMapping` maps controller `n` to, set to `value / 127` |
| `PitchBend` | the parameter mapped to `kPitchBend`, set to `value / 16383` |
| `ChannelAftertouch` | the parameter mapped to `kAfterTouch`, set to `pressure / 127` |
| `PolyAftertouch` | a `kPolyPressureEvent` — a real VST3 event, no mapping needed |

**A controller with no mapping is dropped.** If the plugin does not implement `IMidiMapping`,
or implements it but returns nothing for that controller/channel, the event is discarded and
`send_midi_cc` still returns `Ok(())` — there is nothing to send it as, and VST3 forbids
putting legacy controller events on a component's input event list. Earlier versions of this
library queued such events anyway; well-behaved plugins ignored them and the rest could
misbehave.

Check before you send, if it matters:

```rust
# use vst3_host::{simple, midi::cc};
# fn main() -> vst3_host::Result<()> {
# let plugin = simple::load_plugin("/x.vst3")?;
// bus 0, channel 0 (zero-based), mod wheel
match plugin.midi_cc_to_parameter(0, 0, cc::MODULATION as u16) {
    Some(id) => println!("mod wheel drives parameter {id}"),
    None => println!("this plugin does not map the mod wheel"),
}
# Ok(())
# }
```

Mappings are cached at load and refreshed when the plugin asks for a restart, so the lookup
costs nothing on the send path.

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

## Per-note expression (MPE)

VST3 carries per-voice expression (pitch, volume, timbre…) keyed to a note, not a channel —
the foundation for MPE-style control. Start a note to get a [`NoteId`], send expression
against that id, then end it:

```rust
# use vst3_host::{simple, midi::MidiChannel, NoteExpressionType};
# fn main() -> vst3_host::Result<()> {
# let mut plugin = simple::load_plugin("/x.vst3")?;
let id = plugin.note_on(MidiChannel::Ch1, 60, 100)?;            // returns a NoteId
plugin.send_note_expression(id, NoteExpressionType::Tuning, 0.6)?; // bend up (0.5 = centered)
plugin.note_off(id)?;
# Ok(())
# }
```

Expression values are normalized `0.0–1.0`. `Tuning` is bipolar (`0.5` centered); `Volume`,
`Pan`, `Vibrato`, `Expression`, `Brightness`, and `Custom(id)` round out the set. `_at`
variants (`note_on_at`, `note_off_at`, `send_note_expression_at`) place the event at a sample
offset within the next block.

To discover which dimensions a plugin actually advertises, query `note_expressions()`:

```rust
# use vst3_host::simple;
# fn main() -> vst3_host::Result<()> {
# let plugin = simple::load_plugin("/x.vst3")?;
for info in plugin.note_expressions()? {
    println!("{:?}", info);
}
# Ok(())
# }
```

Note expression works both in-process and under
[process isolation](../explanation/process-isolation.md) — the calls marshal across the
boundary.

## While playing

If the plugin is inside an [`AudioHandle`](https://docs.rs/vst3-host/latest/vst3_host/playback/struct.AudioHandle.html),
the lock-free path is `audio.send_midi(event)` — it queues the event for the next block
without touching the audio mutex (returns `false` if the queue is full). The full-lock
alternative is `audio.lock().send_midi_note(...)` when you need a `Plugin` method that has
no queued equivalent.

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

It maps note on/off (velocity-0 note-on becomes note-off), control change, pitch bend,
aftertouch, and program change, and returns `None` for messages the library doesn't carry
(system/realtime, SysEx). Do the device I/O on its own thread and hand events to the audio
thread through a channel — never call the plugin from the device callback. To bind a port
without writing that plumbing yourself, enable the `midi-input` feature and use
[`midi_input::bind_to_handle`](https://docs.rs/vst3-host/latest/vst3_host/midi_input/fn.bind_to_handle.html),
which forwards parsed events into a running `AudioHandle`. (The inspector's "MIDI Input
Device" picker does the same, via the `midir` crate.)

## SysEx

`Plugin::send_sysex` queues a System Exclusive message as an owned `Vec<u8>`; it allocates,
which is fine from a control thread. A host that forwards SysEx *from its audio thread* uses
the reserved path instead: call
[`Plugin::reserve_sysex(messages, bytes)`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.reserve_sysex)
before processing starts (it allocates; calls add up, so a few large buffers can sit beside
many small ones), then
[`Plugin::send_sysex_from_slice_at(&bytes, offset)`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.send_sysex_from_slice_at)
copies each message into the smallest free reserved buffer. `process_audio` hands the buffer
back to the reserve when the block is done, so neither call allocates or frees. It returns
`false`, queuing nothing, when no reserved buffer that fits is free or the event list is
full — size the reservation for the most SysEx one block can carry. Across process isolation
the slice path falls back to an owned copy.

## Program change

`MidiEvent::ProgramChange` selects a program from the plugin's `IUnitInfo` program list. It
targets the **root unit** (id 0) — VST3 has no MIDI-channel→unit mapping, so the channel is
ignored. For explicit control, call [`Plugin::select_program(unit_id, program_index)`](https://docs.rs/vst3-host/latest/vst3_host/plugin/struct.Plugin.html#method.select_program)
after enumerating units with `Plugin::get_units`. Plugins without a program list ignore it.
