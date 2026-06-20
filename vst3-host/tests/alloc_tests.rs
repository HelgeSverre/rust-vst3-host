//! RT hardening: the steady-state audio path must not allocate.
//!
//! Uses a counting global allocator (toggled on only around the measured window) to assert
//! that `process_audio`, once warmed up, performs zero heap allocations per block. Needs the
//! bundled Dexed plugin, so it's `#[ignore]`d by default:
//!   cargo test -p vst3-host --test alloc_tests -- --ignored --nocapture

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use vst3_host::{audio::AudioBuffers, midi::MidiChannel, Vst3Host};

struct Counting;
static ON: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ON.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
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
