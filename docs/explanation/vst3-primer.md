# VST3 primer

A short mental model of VST3, for readers new to the format. You don't need this to use the
library — it hides these details — but it explains the vocabulary that shows up in plugin
metadata and the API.

## A plugin is a bundle of classes

A `.vst3` file is a bundle (a directory on macOS/Windows, a shared library inside). It
exports a **factory** that lists one or more **classes**. The one that matters for hosting
is the *Audio Module Class* — the actual effect or instrument.
`get_detailed_plugin_info` reports the factory identity and the full class list.

## Two halves: processor and controller

A VST3 plugin separates audio processing from its UI and parameter model:

- The **component / audio processor** (`IComponent` / `IAudioProcessor`) does the DSP. It
  has audio and event (MIDI) **buses** and a `process` call.
- The **edit controller** (`IEditController`) owns the parameters and the editor view.

Some plugins implement both halves in one class ("single component"); others ship them
separately. The library handles both and presents one `Plugin` either way.

## Everything is normalized

VST3 parameters are always `0.0–1.0` on the wire. The plugin maps that to its real range
(Hz, dB, an enum) internally. That's why `Parameter::value` is normalized and why you ask
the plugin to render a display string with `format_parameter` rather than computing it
yourself. See [Control parameters](../how-to/control-parameters.md).

## Buses and channels

A plugin declares audio **buses** (each with a channel count) and event buses for MIDI. An
instrument typically has one audio output bus and one event input bus; an effect has audio
input and output buses. `PluginInfo` summarizes the counts; `get_detailed_plugin_info`
gives the full per-bus layout.

## MIDI is mostly events, but not entirely

Note on/off and polyphonic pressure are first-class VST3 events. Control change, pitch
bend, and channel pressure are delivered through a legacy-controller mechanism (the library
does this for you). Program changes are *not* MIDI events in VST3 — they go through program
lists (`IUnitInfo`), which is why `MidiEvent::ProgramChange` isn't supported. See
[Send MIDI](../how-to/send-midi.md).

## Further reading

- [Architecture](architecture.md) — how this library wraps the above safely.
- Implementation notes on the VST3 SDK internals live in
  [`docs/internal/vst3-notes/`](../internal/vst3-notes/) (developer-facing).
