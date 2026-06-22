//! Minimal Standard MIDI File (.mid) playback for the inspector.
//!
//! Parses an SMF with `midly`, flattens all tracks into a single time-ordered list of
//! `(seconds, MidiEvent)`, and replays them at UI cadence onto the live plugin. Timing is
//! tempo-aware (honors `Set Tempo` meta events across the whole file) but not sample-accurate
//! — it's driven from the control thread, which is the realistic level for a host UI.

use std::time::Instant;
use vst3_host::midi::{MidiChannel, MidiEvent};

/// Seconds elapsed over `delta_ticks` at `tempo_us_per_quarter` microseconds per quarter note,
/// given the file's `ticks_per_quarter` division.
pub fn ticks_to_seconds(
    delta_ticks: u64,
    ticks_per_quarter: u16,
    tempo_us_per_quarter: u32,
) -> f64 {
    if ticks_per_quarter == 0 {
        return 0.0;
    }
    delta_ticks as f64 * (tempo_us_per_quarter as f64 / 1_000_000.0) / ticks_per_quarter as f64
}

/// Map a midly MIDI message on `channel` (0-based) to a library [`MidiEvent`], or `None` for
/// messages the library doesn't carry (program change, pitch bend, aftertouch, sysex...).
pub fn map_message(channel: u8, msg: &midly::MidiMessage) -> Option<MidiEvent> {
    let ch = MidiChannel::from_index(channel)?;
    Some(match msg {
        // A NoteOn with velocity 0 is the running-status idiom for NoteOff.
        midly::MidiMessage::NoteOn { key, vel } if vel.as_int() == 0 => MidiEvent::NoteOff {
            channel: ch,
            note: key.as_int(),
            velocity: 0,
        },
        midly::MidiMessage::NoteOn { key, vel } => MidiEvent::NoteOn {
            channel: ch,
            note: key.as_int(),
            velocity: vel.as_int(),
        },
        midly::MidiMessage::NoteOff { key, vel } => MidiEvent::NoteOff {
            channel: ch,
            note: key.as_int(),
            velocity: vel.as_int(),
        },
        midly::MidiMessage::Controller { controller, value } => MidiEvent::ControlChange {
            channel: ch,
            controller: controller.as_int(),
            value: value.as_int(),
        },
        _ => return None,
    })
}

/// Convert an absolute tick to seconds using a sorted tempo map (`(abs_tick, us_per_quarter)`,
/// first entry at tick 0).
fn seconds_for_tick(abs_tick: u64, tpq: u16, tempo_map: &[(u64, u32)]) -> f64 {
    let mut secs = 0.0;
    let mut last_tick = 0u64;
    let mut cur_tempo = tempo_map.first().map(|&(_, us)| us).unwrap_or(500_000);
    for &(tick, us) in tempo_map {
        if tick >= abs_tick {
            break;
        }
        if tick > last_tick {
            secs += ticks_to_seconds(tick - last_tick, tpq, cur_tempo);
            last_tick = tick;
        }
        cur_tempo = us;
    }
    secs + ticks_to_seconds(abs_tick - last_tick, tpq, cur_tempo)
}

/// Flatten an SMF into a time-ordered `(seconds, MidiEvent)` list. Errors on SMPTE timecode
/// files (only metrical/PPQ timing is supported).
pub fn flatten(smf: &midly::Smf) -> Result<Vec<(f64, MidiEvent)>, String> {
    let tpq = match smf.header.timing {
        midly::Timing::Metrical(t) => t.as_int(),
        midly::Timing::Timecode(..) => {
            return Err("SMPTE timecode MIDI files are not supported (only PPQ/metrical)".into())
        }
    };
    if tpq == 0 {
        return Err("invalid MIDI file: zero ticks-per-quarter".into());
    }

    // First pass: gather all tempo changes and raw (abs_tick, event) pairs across every track.
    let mut tempo_changes: Vec<(u64, u32)> = vec![(0, 500_000)]; // default 120 BPM at tick 0
    let mut raw: Vec<(u64, MidiEvent)> = Vec::new();
    for track in &smf.tracks {
        let mut abs_tick: u64 = 0;
        for ev in track {
            abs_tick += ev.delta.as_int() as u64;
            match ev.kind {
                midly::TrackEventKind::Meta(midly::MetaMessage::Tempo(us)) => {
                    tempo_changes.push((abs_tick, us.as_int()));
                }
                midly::TrackEventKind::Midi { channel, message } => {
                    if let Some(e) = map_message(channel.as_int(), &message) {
                        raw.push((abs_tick, e));
                    }
                }
                _ => {}
            }
        }
    }
    tempo_changes.sort_by_key(|&(t, _)| t);

    // Second pass: convert each event's tick to seconds and sort by time.
    let mut events: Vec<(f64, MidiEvent)> = raw
        .into_iter()
        .map(|(tick, e)| (seconds_for_tick(tick, tpq, &tempo_changes), e))
        .collect();
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(events)
}

/// Plays a loaded SMF by handing out events as their scheduled time arrives.
#[derive(Default)]
pub struct MidiFilePlayer {
    events: Vec<(f64, MidiEvent)>,
    next_idx: usize,
    start: Option<Instant>,
    playing: bool,
    loaded_name: Option<String>,
}

impl MidiFilePlayer {
    /// Load and flatten a `.mid` file. Replaces any previously loaded file (stops playback).
    pub fn load(&mut self, path: &std::path::Path) -> Result<(), String> {
        let data = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
        let smf = midly::Smf::parse(&data).map_err(|e| format!("parse failed: {e}"))?;
        self.events = flatten(&smf)?;
        self.next_idx = 0;
        self.start = None;
        self.playing = false;
        self.loaded_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());
        Ok(())
    }

    /// The loaded file's name, if any.
    pub fn loaded_name(&self) -> Option<&str> {
        self.loaded_name.as_deref()
    }

    /// Number of scheduled events in the loaded file.
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Start (or restart) playback from the beginning.
    pub fn play(&mut self, now: Instant) {
        if self.events.is_empty() {
            return;
        }
        self.next_idx = 0;
        self.start = Some(now);
        self.playing = true;
    }

    /// Stop playback. The caller should send an all-notes-off afterward to kill ringing notes.
    pub fn stop(&mut self) {
        self.playing = false;
        self.start = None;
        self.next_idx = 0;
    }

    /// Whether playback is active.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Return every event due by `now`, advancing the cursor. Sets `playing = false` when the
    /// file finishes.
    pub fn tick(&mut self, now: Instant) -> Vec<MidiEvent> {
        let Some(start) = self.start else {
            return Vec::new();
        };
        let elapsed = now.saturating_duration_since(start).as_secs_f64();
        let mut due = Vec::new();
        while self.next_idx < self.events.len() && self.events[self.next_idx].0 <= elapsed {
            due.push(self.events[self.next_idx].1);
            self.next_idx += 1;
        }
        if self.next_idx >= self.events.len() {
            self.playing = false;
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_to_seconds_at_120bpm() {
        // 480 tpq, 500000 us/qn (120 BPM): a quarter note (480 ticks) == 0.5 s.
        assert!((ticks_to_seconds(480, 480, 500_000) - 0.5).abs() < 1e-9);
        assert!((ticks_to_seconds(960, 480, 500_000) - 1.0).abs() < 1e-9);
        // Half the tempo number (250000 us = 240 BPM) halves the time.
        assert!((ticks_to_seconds(480, 480, 250_000) - 0.25).abs() < 1e-9);
        assert_eq!(ticks_to_seconds(100, 0, 500_000), 0.0); // guard div-by-zero
    }

    #[test]
    fn note_on_zero_velocity_maps_to_note_off() {
        let on0 = midly::MidiMessage::NoteOn {
            key: 60.into(),
            vel: 0.into(),
        };
        assert!(matches!(
            map_message(0, &on0),
            Some(MidiEvent::NoteOff { note: 60, .. })
        ));
        let on = midly::MidiMessage::NoteOn {
            key: 60.into(),
            vel: 100.into(),
        };
        assert!(matches!(
            map_message(0, &on),
            Some(MidiEvent::NoteOn {
                note: 60,
                velocity: 100,
                ..
            })
        ));
    }

    #[test]
    fn program_change_is_skipped() {
        let pc = midly::MidiMessage::ProgramChange { program: 5.into() };
        assert!(map_message(0, &pc).is_none());
    }

    #[test]
    fn seconds_for_tick_honors_tempo_change() {
        // 480 tpq; tempo 500000 (120 BPM) until tick 480, then 250000 (240 BPM).
        let map = vec![(0u64, 500_000u32), (480, 250_000)];
        // First quarter at 120 BPM = 0.5 s.
        assert!((seconds_for_tick(480, 480, &map) - 0.5).abs() < 1e-9);
        // Next quarter at 240 BPM = 0.25 s → total 0.75 s at tick 960.
        assert!((seconds_for_tick(960, 480, &map) - 0.75).abs() < 1e-9);
    }
}
