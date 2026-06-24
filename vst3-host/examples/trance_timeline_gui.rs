//! LOCAL DEMO GUI (not shipped): a crude live trance player.
//!
//!   cargo run --example trance_timeline_gui                 # in-repo TestSynth (build with `just test-plugin`)
//!   cargo run --example trance_timeline_gui -- "/path/to/Synth.vst3"
//!
//! Plays an embedded Nu-NRG riff live through the `transport::Timeline` into a VST3 synth, with
//! a tempo-synced ping-pong delay. A crude egui window shows the MIDI notes scrolling under a
//! playhead and exposes live knobs (sliders) for super-saw Detune, filter Cutoff, and the delay
//! Feedback / Mix. Audio runs on a cpal output stream whose callback drives the timeline
//! sample-synced (so the playhead and the sound stay together).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use vst3_host::{
    audio::AudioBuffers,
    midi::{MidiChannel, MidiEvent},
    transport::{MidiClip, Timeline},
    Vst3Host,
};

const MIDI_BYTES: &[u8] = include_bytes!("assets/moon-loves-the-sun.mid");
const DEFAULT_PLUGIN: &str = "test_plugins/TestSynth.vst3";

/// A note as drawn in the piano roll.
struct NoteSpan {
    on_beat: f64,
    off_beat: f64,
    pitch: u8,
}

/// Live controls shared between the UI thread and the audio callback (f32 stored as bits).
struct Shared {
    detune: AtomicU32,
    cutoff: AtomicU32,
    delay_feedback: AtomicU32,
    delay_wet: AtomicU32,
    playhead_beats: AtomicU32,
}

impl Shared {
    fn new() -> Self {
        Self {
            detune: AtomicU32::new(0.75f32.to_bits()),
            cutoff: AtomicU32::new(0.6f32.to_bits()),
            delay_feedback: AtomicU32::new(0.6f32.to_bits()),
            delay_wet: AtomicU32::new(0.5f32.to_bits()),
            playhead_beats: AtomicU32::new(0),
        }
    }
}

fn getf(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}
fn setf(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

/// Parse the SMF into timeline note events, drawable spans, and the tempo (BPM).
fn load_midi(bytes: &[u8]) -> (Vec<(f64, MidiEvent)>, Vec<NoteSpan>, f64) {
    let smf = Smf::parse(bytes).expect("parse midi");
    let tpq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as f64,
        Timing::Timecode(..) => 480.0,
    };
    let mut bpm = 140.0;
    let mut events = Vec::new();
    let mut spans = Vec::new();
    let mut open: Vec<(u8, f64)> = Vec::new(); // (pitch, on_beat) awaiting note-off

    for track in &smf.tracks {
        let mut tick: u64 = 0;
        for ev in track {
            tick += ev.delta.as_int() as u64;
            let beat = tick as f64 / tpq;
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    bpm = 60_000_000.0 / us.as_int() as f64;
                }
                TrackEventKind::Midi { channel, message } if channel.as_int() != 9 => match message
                {
                    MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                        let note = key.as_int();
                        events.push((
                            beat,
                            MidiEvent::NoteOn {
                                channel: MidiChannel::Ch1,
                                note,
                                velocity: vel.as_int(),
                            },
                        ));
                        open.push((note, beat));
                    }
                    MidiMessage::NoteOff { key, .. } | MidiMessage::NoteOn { key, .. } => {
                        let note = key.as_int();
                        events.push((
                            beat,
                            MidiEvent::NoteOff {
                                channel: MidiChannel::Ch1,
                                note,
                                velocity: 0,
                            },
                        ));
                        if let Some(pos) = open.iter().rposition(|(p, _)| *p == note) {
                            let (pitch, on_beat) = open.remove(pos);
                            spans.push(NoteSpan {
                                on_beat,
                                off_beat: beat,
                                pitch,
                            });
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
    (events, spans, bpm)
}

/// Stateful tempo-synced ping-pong delay (cross-fed echoes), processed in the audio callback.
struct PingPong {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    w: usize,
}

impl PingPong {
    fn new(samples: usize) -> Self {
        let n = samples.max(1);
        Self {
            buf_l: vec![0.0; n],
            buf_r: vec![0.0; n],
            w: 0,
        }
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32], feedback: f32, wet: f32) {
        let n = self.buf_l.len();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (dl, dr) = (self.buf_l[self.w], self.buf_r[self.w]);
            let (in_l, in_r) = (*l, *r);
            self.buf_l[self.w] = in_l + dr * feedback;
            self.buf_r[self.w] = in_r + dl * feedback;
            *l = in_l + dl * wet;
            *r = in_r + dr * wet;
            self.w = (self.w + 1) % n;
        }
    }
}

fn find_param(plugin: &vst3_host::Plugin, name: &str) -> Option<u32> {
    plugin
        .get_parameters()
        .ok()?
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| p.id)
}

struct App {
    shared: Arc<Shared>,
    spans: Vec<NoteSpan>,
    total_beats: f64,
    min_pitch: u8,
    max_pitch: u8,
    _stream: cpal::Stream, // kept alive for the lifetime of the window
}

impl eframe::App for App {
    // eframe 0.34 requires `ui`; we build everything on the `Context` in `update` instead.
    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // egui 0.34: a single root `Ui` that panels are shown inside (see the inspector).
        let mut root_ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("trance_gui_root"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(ctx.content_rect()),
        );
        root_ui.set_clip_rect(ctx.content_rect());

        egui::Panel::top("controls").show_inside(&mut root_ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let knob = |ui: &mut egui::Ui, label: &str, a: &AtomicU32| {
                    let mut v = getf(a);
                    if ui
                        .add(egui::Slider::new(&mut v, 0.0..=1.0).text(label))
                        .changed()
                    {
                        setf(a, v);
                    }
                };
                knob(ui, "Detune", &self.shared.detune);
                knob(ui, "Cutoff", &self.shared.cutoff);
                knob(ui, "Delay FB", &self.shared.delay_feedback);
                knob(ui, "Delay Mix", &self.shared.delay_wet);
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show_inside(&mut root_ui, |ui| {
            let (resp, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::hover());
            let rect = resp.rect;
            painter.rect_filled(rect, 4.0, egui::Color32::from_gray(18));

            let pitch_range = (self.max_pitch - self.min_pitch).max(1) as f32;
            let row_h = rect.height() / (pitch_range + 1.0);
            let beat_to_x =
                |b: f64| rect.left() + (b / self.total_beats.max(1.0)) as f32 * rect.width();
            let pitch_to_y = |p: u8| rect.bottom() - (p - self.min_pitch) as f32 * row_h - row_h;

            for s in &self.spans {
                let x0 = beat_to_x(s.on_beat);
                let x1 = beat_to_x(s.off_beat).max(x0 + 2.0);
                let y = pitch_to_y(s.pitch);
                painter.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, y + row_h * 0.9)),
                    2.0,
                    egui::Color32::from_rgb(90, 170, 255),
                );
            }

            // Playhead.
            let px = beat_to_x(getf(&self.shared.playhead_beats) as f64);
            painter.line_segment(
                [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 90, 90)),
            );
        });

        ctx.request_repaint(); // keep the playhead moving
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let plugin_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_PLUGIN.to_string());
    if !std::path::Path::new(&plugin_path).exists() {
        eprintln!("Synth not found: {plugin_path}");
        eprintln!("Build the bundled synth with `just test-plugin`, or pass a VST3 synth path.");
        return Ok(());
    }

    // Audio device config drives the sample rate the plugin is set up at.
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("no default output device")?;
    let supported = device.default_output_config()?;
    let sr = supported.sample_rate() as f64; // cpal 0.18: SampleRate is a u32 alias
    let channels = supported.channels() as usize;
    let max_block = 8192usize;

    let (events, spans, bpm) = load_midi(MIDI_BYTES);
    let last_beat = events.iter().map(|(b, _)| *b).fold(0.0, f64::max);
    let total_beats = last_beat + 1.0;
    let min_pitch = spans.iter().map(|s| s.pitch).min().unwrap_or(48);
    let max_pitch = spans.iter().map(|s| s.pitch).max().unwrap_or(72);

    let mut host_builder = Vst3Host::builder()
        .sample_rate(sr)
        .block_size(max_block)
        .build()?;
    let mut plugin = host_builder.load_plugin(&plugin_path)?;
    println!(
        "loaded: {} by {} @ {sr} Hz",
        plugin.info().name,
        plugin.info().vendor
    );

    // Prefer a super-saw waveform if the synth offers one.
    if let Some(w) = find_param(&plugin, "Waveform") {
        let _ = plugin.set_parameter(w, 1.0);
    }
    let cutoff_id = find_param(&plugin, "Cutoff");
    let detune_id = find_param(&plugin, "Detune");
    plugin.start_processing()?;

    let mut timeline = Timeline::new(sr, bpm).with_clip({
        let mut clip = MidiClip::new();
        for (beat, ev) in events {
            clip.add(beat, ev);
        }
        clip
    });
    let total_frames = timeline.beat_to_frame(total_beats).max(1);

    let shared = Arc::new(Shared::new());
    let shared_audio = shared.clone();
    let mut delay = PingPong::new((0.375 * 60.0 / bpm * sr) as usize); // dotted 1/16
    let samples_per_beat = sr * 60.0 / bpm;

    let config = cpal::StreamConfig {
        channels: channels as u16,
        sample_rate: sr as u32,
        buffer_size: cpal::BufferSize::Default,
    };
    let stream = device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / channels.max(1);
            if frames == 0 {
                return;
            }
            // Loop the riff.
            if timeline.sample_clock() >= total_frames {
                timeline.seek_frame(0);
                let _ = plugin.send_midi_event(MidiEvent::NoteOff {
                    channel: MidiChannel::Ch1,
                    note: 0,
                    velocity: 0,
                }); // all-notes-off (noteId -1)
            }
            // Live knobs → plugin params.
            if let Some(id) = cutoff_id {
                let _ = plugin.set_parameter(id, getf(&shared_audio.cutoff) as f64);
            }
            if let Some(id) = detune_id {
                let _ = plugin.set_parameter(id, getf(&shared_audio.detune) as f64);
            }
            // Drive the timeline for this block.
            let block = timeline.advance_block(frames);
            for (ev, off) in block.midi {
                let _ = plugin.send_midi_event_at(ev, off);
            }
            let mut buf = AudioBuffers::new(0, 2, frames, sr);
            if plugin.process_audio(&mut buf).is_ok() {
                let (fb, wet) = (
                    getf(&shared_audio.delay_feedback),
                    getf(&shared_audio.delay_wet),
                );
                let (mut l, mut r) = (buf.outputs[0].clone(), buf.outputs[1].clone());
                delay.process(&mut l, &mut r, fb, wet);
                for (i, frame) in data.chunks_mut(channels.max(1)).enumerate() {
                    let (lv, rv) = (
                        l.get(i).copied().unwrap_or(0.0),
                        r.get(i).copied().unwrap_or(0.0),
                    );
                    for (ch, out) in frame.iter_mut().enumerate() {
                        *out = if ch % 2 == 0 { lv } else { rv };
                    }
                }
            } else {
                data.fill(0.0);
            }
            let beats = timeline.sample_clock() as f64 / samples_per_beat;
            setf(&shared_audio.playhead_beats, beats as f32);
        },
        move |err| eprintln!("audio stream error: {err}"),
        None,
    )?;
    stream.play()?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 460.0])
            .with_title("Trance timeline — live"),
        ..Default::default()
    };
    eframe::run_native(
        "Trance timeline — live",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(App {
                shared,
                spans,
                total_beats,
                min_pitch,
                max_pitch,
                _stream: stream,
            }))
        }),
    )?;
    Ok(())
}
