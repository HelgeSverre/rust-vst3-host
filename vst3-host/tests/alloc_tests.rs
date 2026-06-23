//! RT hardening: the steady-state audio path must not allocate.
//!
//! Uses a counting global allocator (toggled on only around the measured window) to assert
//! that `process_audio`, once warmed up, performs zero heap allocations per block. Needs the
//! bundled Dexed plugin, so it's `#[ignore]`d by default:
//!   cargo test -p vst3-host --test alloc_tests -- --ignored --nocapture

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use vst3_host::{
    audio::AudioBuffers,
    midi::{MidiChannel, MidiEvent},
    realtime::RealtimePluginRunner,
    Vst3Host,
};

struct Counting;
static ON: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
// The counting allocator and its arming flag are process-global, so the measured tests must
// not run concurrently (libtest runs tests in parallel by default). Each measured test holds
// this lock for its whole body, so only one arms the allocator at a time.
static SERIAL: Mutex<()> = Mutex::new(());

// Count alloc + realloc + dealloc while armed: a dealloc inside the measured window means
// something owned was Dropped there, so a zero count proves the path is alloc-free AND Drop-free.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ON.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ON.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ON.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
#[ignore = "Requires the bundled test plugin"]
fn steady_state_process_is_allocation_free() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(path).exists() {
        println!("Test plugin not found, skipping");
        return;
    }
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .unwrap();
    let mut plugin = host.load_plugin(path).unwrap();
    plugin.start_processing().unwrap();
    plugin.send_midi_note(60, 110, MidiChannel::Ch1).unwrap();

    let mut buf = AudioBuffers::new(0, 2, 512, 48000.0);
    // Warm up: the first blocks set up process data and let the synth settle.
    for _ in 0..8 {
        plugin.process_audio(&mut buf).unwrap();
    }

    // Measure the steady state.
    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    for _ in 0..100 {
        plugin.process_audio(&mut buf).unwrap();
    }
    ON.store(false, Ordering::Relaxed);

    let n = ALLOCS.load(Ordering::Relaxed);
    println!("steady-state allocations over 100 blocks: {n}");
    assert_eq!(
        n, 0,
        "steady-state process() should not allocate; saw {n} allocations over 100 blocks"
    );
}

/// The lock-free [`RealtimePluginRunner`] path must be allocation-free AND Drop-free in steady
/// state even while parameter changes and MIDI are flowing — the realistic RT case (a sequencer
/// or controller driving the synth). This is stricter than the held-note test above because it
/// exercises the queued-parameter path (`set_parameter` -> pending changes -> the processor's
/// input parameter queue) every few blocks, which is where the steady-state allocations lived.
#[test]
#[ignore = "Requires the bundled test plugin"]
fn realtime_runner_steady_state_is_allocation_free() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_plugins/Dexed.vst3");
    if !std::path::Path::new(path).exists() {
        println!("Test plugin not found, skipping");
        return;
    }
    let mut host = Vst3Host::builder()
        .sample_rate(48000.0)
        .block_size(512)
        .build()
        .unwrap();
    let plugin = host.load_plugin(path).unwrap();
    let (mut runner, mut control) = RealtimePluginRunner::new(plugin, 1024);
    runner.start().unwrap();

    let mut buf = AudioBuffers::new(0, 2, 512, 48000.0);

    // Parameter ids automated in the measured window; warm each one up so its backing queue
    // object is created once, before arming.
    let param_ids: [u32; 3] = [0, 1, 2];

    // Warm up so every path that runs in the armed window first reaches its steady-state
    // capacity: two simultaneous note events in one block (sizes the input event list for 2),
    // one parameter change per id (creates each param queue), plus plain blocks to settle.
    control.send_midi(MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 110,
    });
    control.send_midi(MidiEvent::NoteOff {
        channel: MidiChannel::Ch1,
        note: 48,
        velocity: 0,
    });
    for &id in &param_ids {
        control.set_parameter(id, 0.5);
    }
    for _ in 0..16 {
        runner.process(&mut buf).unwrap();
    }

    // Measure: a parameter change every 8th block (cycling the warmed ids) and a note on/off
    // every 16th block. All buffers are already at capacity, so any count is a real per-block
    // allocation / realloc / Drop on the runner's hot path.
    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    for i in 0..200usize {
        if i % 8 == 0 {
            let id = param_ids[(i / 8) % param_ids.len()];
            control.set_parameter(id, ((i % 7) as f64) / 7.0);
        }
        if i % 16 == 0 {
            control.send_midi(MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 100,
            });
            control.send_midi(MidiEvent::NoteOff {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 0,
            });
        }
        runner.process(&mut buf).unwrap();
    }
    ON.store(false, Ordering::Relaxed);

    let n = ALLOCS.load(Ordering::Relaxed);
    println!("realtime runner steady-state allocations over 200 blocks (param every 8th, note every 16th): {n}");
    assert_eq!(
        n, 0,
        "runner steady-state process() should not allocate/realloc/free; saw {n} over 200 blocks"
    );
}
