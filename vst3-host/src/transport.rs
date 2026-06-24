//! A sample-accurate musical timeline: schedule MIDI clips and parameter-automation lanes on
//! a beat grid and drive them into a plugin block by block.
//!
//! [`Timeline`] owns a tempo (BPM) and sample rate and a sample clock. Each call to
//! [`Timeline::advance_block`] returns the events that fall in the next block as
//! sample-accurate offsets (`(event, offset)` / `(param_id, offset, value)`), then advances the
//! clock. [`Timeline::drive_block`] is the convenience that pushes those into a [`Plugin`] and
//! renders one block.
//!
//! **Timebase:** clips and lanes are authored in **beats**. Slice 1 uses a single constant
//! tempo (`bpm`); a varying tempo curve is future work, so beat↔sample conversion here is the
//! constant-tempo `samples_per_beat = sample_rate * 60 / bpm`.
//!
//! ```no_run
//! use vst3_host::{simple, transport::{Timeline, MidiClip}, midi::{MidiEvent, MidiChannel}};
//! # fn main() -> vst3_host::Result<()> {
//! let mut plugin = simple::load_plugin("/path/synth.vst3")?;
//! plugin.start_processing()?;
//!
//! let clip = MidiClip::new()
//!     .with(0.0, MidiEvent::NoteOn { channel: MidiChannel::Ch1, note: 60, velocity: 100 })
//!     .with(2.0, MidiEvent::NoteOff { channel: MidiChannel::Ch1, note: 60, velocity: 0 });
//! let mut timeline = Timeline::new(48_000.0, 120.0).with_clip(clip);
//!
//! let mut buffers = vst3_host::audio::AudioBuffers::new(0, 2, 512, 48_000.0);
//! for _ in 0..96 {
//!     timeline.drive_block(&mut plugin, &mut buffers)?;
//! }
//! # Ok(())
//! # }
//! ```

use crate::audio::AudioBuffers;
use crate::error::Result;
use crate::midi::MidiEvent;
use crate::parameters::ParameterAutomation;
use crate::plugin::Plugin;

/// A clip of MIDI events placed at beat positions on the timeline.
#[derive(Debug, Clone, Default)]
pub struct MidiClip {
    /// `(beat, event)`, not required to be sorted — [`Timeline::advance_block`] windows by frame.
    events: Vec<(f64, MidiEvent)>,
}

impl MidiClip {
    /// An empty clip.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an event at `beat` (fluent).
    pub fn with(mut self, beat: f64, event: MidiEvent) -> Self {
        self.events.push((beat, event));
        self
    }

    /// Add an event at `beat`.
    pub fn add(&mut self, beat: f64, event: MidiEvent) {
        self.events.push((beat, event));
    }
}

/// A parameter-automation lane: a parameter id plus its [`ParameterAutomation`] curve, whose
/// point times are interpreted in **beats** (so the lane follows the timeline's tempo).
#[derive(Debug, Clone)]
pub struct AutomationLane {
    /// Target parameter id.
    pub param_id: u32,
    /// The automation curve; point times are in beats.
    pub automation: ParameterAutomation,
    /// How many automation points to emit per block (denser = smoother, more events).
    pub points_per_block: usize,
}

impl AutomationLane {
    /// A lane targeting `param_id` driven by `automation` (point times in beats), emitting
    /// `points_per_block` points per processed block.
    pub fn new(param_id: u32, automation: ParameterAutomation, points_per_block: usize) -> Self {
        Self {
            param_id,
            automation,
            points_per_block,
        }
    }
}

/// The events a single block should deliver, as sample offsets within that block.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockEvents {
    /// MIDI events with their sample offset within the block, in scheduled order.
    pub midi: Vec<(MidiEvent, i32)>,
    /// Parameter changes as `(param_id, sample_offset, value)`.
    pub params: Vec<(u32, i32, f64)>,
}

/// A sample-accurate musical timeline driving MIDI clips and automation lanes into a plugin.
#[derive(Debug, Clone)]
pub struct Timeline {
    sample_rate: f64,
    bpm: f64,
    sample_clock: u64,
    clips: Vec<MidiClip>,
    lanes: Vec<AutomationLane>,
}

impl Timeline {
    /// A timeline at `sample_rate` and constant tempo `bpm`. `bpm` must be finite and `> 0`;
    /// an invalid value falls back to `120.0` so beat↔sample conversion can't produce NaN.
    pub fn new(sample_rate: f64, bpm: f64) -> Self {
        let bpm = if bpm.is_finite() && bpm > 0.0 {
            bpm
        } else {
            120.0
        };
        Self {
            sample_rate,
            bpm,
            sample_clock: 0,
            clips: Vec::new(),
            lanes: Vec::new(),
        }
    }

    /// Add a MIDI clip (fluent).
    pub fn with_clip(mut self, clip: MidiClip) -> Self {
        self.clips.push(clip);
        self
    }

    /// Add an automation lane (fluent).
    pub fn with_lane(mut self, lane: AutomationLane) -> Self {
        self.lanes.push(lane);
        self
    }

    /// Add a MIDI clip.
    pub fn add_clip(&mut self, clip: MidiClip) {
        self.clips.push(clip);
    }

    /// Add an automation lane.
    pub fn add_lane(&mut self, lane: AutomationLane) {
        self.lanes.push(lane);
    }

    /// The current playhead position in frames since the start.
    pub fn sample_clock(&self) -> u64 {
        self.sample_clock
    }

    /// Move the playhead to `frame` (e.g. to loop or seek). Does not emit events.
    pub fn seek_frame(&mut self, frame: u64) {
        self.sample_clock = frame;
    }

    /// Samples per beat at the current constant tempo.
    pub fn samples_per_beat(&self) -> f64 {
        self.sample_rate * 60.0 / self.bpm
    }

    /// Convert a beat position to an absolute frame index.
    pub fn beat_to_frame(&self, beat: f64) -> u64 {
        (beat * self.samples_per_beat()).round().max(0.0) as u64
    }

    /// Convert an absolute frame index to a beat position.
    pub fn frame_to_beat(&self, frame: u64) -> f64 {
        frame as f64 / self.samples_per_beat()
    }

    /// Collect the events that fall in the next `frames`-sample block as sample offsets, then
    /// advance the playhead by `frames`. Clip events are windowed by frame index against the
    /// half-open block `[clock, clock + frames)`; automation lanes emit their per-block points
    /// (evaluated in the beat domain) tagged with the lane's parameter id.
    pub fn advance_block(&mut self, frames: usize) -> BlockEvents {
        let start = self.sample_clock;
        let end = start + frames as u64;
        let mut out = BlockEvents::default();

        for clip in &self.clips {
            for (beat, event) in &clip.events {
                let frame = self.beat_to_frame(*beat);
                if frame >= start && frame < end {
                    out.midi.push((*event, (frame - start) as i32));
                }
            }
        }
        // Deliver scheduled events in time order so a NoteOff never precedes its NoteOn within a
        // block when two clips overlap.
        out.midi.sort_by_key(|(_, offset)| *offset);

        if frames > 0 {
            // Drive points_for_block in the beat domain: passing `samples_per_beat` as the
            // "sample rate" makes its internal `offset / rate` term read as beats, so a
            // beat-authored curve is evaluated correctly with sample-accurate offsets.
            let start_beats = self.frame_to_beat(start);
            let spb = self.samples_per_beat();
            for lane in &self.lanes {
                for (offset, value) in lane.automation.points_for_block(
                    start_beats,
                    frames,
                    spb,
                    lane.points_per_block,
                ) {
                    out.params.push((lane.param_id, offset, value));
                }
            }
        }

        self.sample_clock = end;
        out
    }

    /// Advance one block and drive it into `plugin`: schedule its MIDI and parameter changes at
    /// their sample offsets, then render `buffers`. The block length is `buffers`' block size.
    pub fn drive_block(&mut self, plugin: &mut Plugin, buffers: &mut AudioBuffers) -> Result<()> {
        let frames = buffers.block_size;
        let events = self.advance_block(frames);
        for (event, offset) in events.midi {
            plugin.send_midi_event_at(event, offset)?;
        }
        for (id, offset, value) in events.params {
            plugin.set_parameter_at(id, value, offset)?;
        }
        plugin.process_audio(buffers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::MidiChannel;

    fn note_on(n: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            channel: MidiChannel::Ch1,
            note: n,
            velocity: 100,
        }
    }
    fn note_off(n: u8) -> MidiEvent {
        MidiEvent::NoteOff {
            channel: MidiChannel::Ch1,
            note: n,
            velocity: 0,
        }
    }

    #[test]
    fn beat_frame_round_trip_at_120_and_140_bpm() {
        // 120 bpm @ 48k: 1 beat = 0.5s = 24000 frames.
        let t = Timeline::new(48_000.0, 120.0);
        assert_eq!(t.beat_to_frame(0.0), 0);
        assert_eq!(t.beat_to_frame(1.0), 24_000);
        assert_eq!(t.beat_to_frame(0.5), 12_000);
        assert_eq!(t.frame_to_beat(24_000), 1.0);

        // 140 bpm @ 48k: 1 beat = 60/140 s ≈ 20571.43 frames → rounds to 20571.
        let t = Timeline::new(48_000.0, 140.0);
        assert_eq!(t.beat_to_frame(1.0), 20_571);
    }

    #[test]
    fn invalid_bpm_falls_back_to_120() {
        for bad in [0.0, -10.0, f64::NAN, f64::INFINITY] {
            let t = Timeline::new(48_000.0, bad);
            assert_eq!(
                t.beat_to_frame(1.0),
                24_000,
                "bpm {bad} should fall back to 120"
            );
        }
    }

    #[test]
    fn slices_clip_and_lane_into_block_offsets() {
        // 120 bpm @ 48k. NoteOn @ beat 0 (frame 0); NoteOff @ beat 0.02 (=0.01s=480 frames).
        let clip = MidiClip::new()
            .with(0.0, note_on(60))
            .with(0.02, note_off(60));
        let lane = AutomationLane::new(
            7,
            ParameterAutomation::new()
                .add_point(0.0, 0.0)
                .add_point(4.0, 1.0),
            1,
        );
        let mut t = Timeline::new(48_000.0, 120.0)
            .with_clip(clip)
            .with_lane(lane);

        // Block 0: [0, 512). NoteOn @ offset 0, NoteOff @ offset 480; one lane point @ offset 0.
        let b0 = t.advance_block(512);
        assert_eq!(b0.midi, vec![(note_on(60), 0), (note_off(60), 480)]);
        assert_eq!(b0.params.len(), 1);
        assert_eq!(b0.params[0].0, 7);
        assert_eq!(b0.params[0].1, 0);
        assert_eq!(t.sample_clock(), 512);

        // Block 1: [512, 1024). No MIDI (both events were before 512); lane still emits a point.
        let b1 = t.advance_block(512);
        assert!(b1.midi.is_empty());
        assert_eq!(b1.params.len(), 1);
        assert_eq!(t.sample_clock(), 1024);
    }

    #[test]
    fn event_on_block_boundary_lands_in_the_next_block() {
        // An event whose frame index == clock + frames must fall in the NEXT block (the window
        // is half-open `[clock, clock + frames)`), guarding the off-by-one.
        // 120 bpm @ 48k: beat 0.0213333.. → frame 512 exactly.
        let boundary_beat = 512.0 / (48_000.0 * 60.0 / 120.0);
        let clip = MidiClip::new().with(boundary_beat, note_on(60));
        let mut t = Timeline::new(48_000.0, 120.0).with_clip(clip);

        let b0 = t.advance_block(512); // [0, 512)
        assert!(b0.midi.is_empty(), "frame 512 must not be in block [0,512)");
        let b1 = t.advance_block(512); // [512, 1024)
        assert_eq!(
            b1.midi,
            vec![(note_on(60), 0)],
            "lands at offset 0 of the next block"
        );
    }
}
