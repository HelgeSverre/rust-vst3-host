//! Regression coverage for pointer-backed VST3 output in prepared realtime processing.
//!
//! TestSynth reserves NoteOn Ch16/119 as a non-audible probe: velocity 1 emits one SysEx,
//! note-expression text, chord text, and scale text event; velocity 2 emits a 17-byte SysEx.
//! Each source buffer is stack-backed and overwritten immediately after `IEventList::addEvent`.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};

use vst3_host::{
    audio::AudioBuffers,
    hard_realtime::{RealtimeCapacities, RealtimeMidiEvent},
    midi::{MidiChannel, MidiEvent, PluginEventData},
    Vst3Host,
};

struct Counting;

static ON: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static SERIAL: Mutex<()> = Mutex::new(());

// The test binary has its own allocator, so its accounting cannot affect the pre-existing
// allocation regressions. It intentionally counts frees too: dropping an owned payload from the
// audio callback violates the same guarantee as allocating one there.
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

const BLOCK: usize = 64;
const TEST_SYNTH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../test_plugins/TestSynth.vst3"
);

fn capacities(payload_bytes: usize) -> RealtimeCapacities {
    RealtimeCapacities {
        max_block_frames: BLOCK,
        max_input_events: 1,
        max_output_events: 4,
        max_output_payload_bytes_per_event: payload_bytes,
        max_parameter_changes: 1,
        max_distinct_parameters: 1,
    }
}

fn payload_probe(velocity: u8) -> [RealtimeMidiEvent; 1] {
    [RealtimeMidiEvent {
        event: MidiEvent::NoteOn {
            channel: MidiChannel::Ch16,
            note: 119,
            velocity,
        },
        sample_offset: 0,
    }]
}

fn host() -> Vst3Host {
    assert!(
        Path::new(TEST_SYNTH).exists(),
        "build TestSynth before running this regression: just test-plugin"
    );
    Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(BLOCK)
        .build()
        .expect("build host")
}

fn enter_realtime(
    plugin: vst3_host::Plugin,
    capacities: RealtimeCapacities,
) -> vst3_host::RealtimePlugin {
    match plugin.try_into_realtime(capacities) {
        Ok(realtime) => realtime,
        Err(failure) => panic!("enter realtime: {}", failure.error()),
    }
}

fn leave_realtime(realtime: vst3_host::RealtimePlugin) -> vst3_host::Plugin {
    match realtime.try_into_plugin() {
        Ok((plugin, _)) => plugin,
        Err(failure) => panic!("leave realtime: {}", failure.error()),
    }
}

fn assert_normal_payload_bundle(events: &[vst3_host::midi::PluginEvent]) {
    assert_eq!(events.len(), 4);
    assert!(matches!(
        &events[0].data,
        PluginEventData::Data { data_type: 0, bytes }
            if bytes == &[0xf0, 0x7d, 0x54, 0x53, 0x01, 0xf7]
    ));
    assert!(matches!(
        &events[1].data,
        PluginEventData::NoteExpressionText { type_id: 0x5453_0001, note_id: 71, text }
            if text == &[b'N' as u16, b'E' as u16, b'X' as u16, b'T' as u16]
    ));
    assert!(matches!(
        &events[2].data,
        PluginEventData::Chord { root: 0, bass_note: 0, mask, text }
            if *mask == 0b0000_0100_1001 && text == &[b'C' as u16, b'7' as u16, 0]
    ));
    assert!(matches!(
        &events[3].data,
        PluginEventData::Scale { root: 0, mask, text }
            if *mask == 0b0000_1010_1101 && text == &[b'M' as u16, b'a' as u16]
    ));
}

#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn prepared_payloads_are_copied_for_a_preexisting_output_handle() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut host = host();
    let plugin = host.load_plugin(TEST_SYNTH).expect("load TestSynth");
    // This is the handle an application keeps on its UI/control thread before moving plugin
    // ownership into the realtime runner.
    let consumer = plugin
        .output_event_handle()
        .expect("in-process output handle");
    let mut realtime = enter_realtime(plugin, capacities(8));
    let mut buffers = AudioBuffers::new(0, 2, BLOCK, 48_000.0);

    // The first block is the important one: a payload pool that only becomes prepared after its
    // first use would still violate the callback contract even if later saturated blocks pass.
    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    let first_block = realtime.process(&mut buffers, &payload_probe(1), &[]);
    ON.store(false, Ordering::Relaxed);
    let report = first_block.expect("accept one event at the exact eight-byte budget");
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "the first prepared payload block allocated, reallocated, or freed host storage"
    );
    assert_eq!(report.output_event_overflows, 0);
    // The handle must still see the owned copy after later blocks have reused the prepared
    // event arena. TestSynth has already overwritten every original stack source.
    realtime
        .process(&mut buffers, &[], &[])
        .expect("second block");
    realtime
        .process(&mut buffers, &[], &[])
        .expect("third block");
    assert_normal_payload_bundle(&consumer.drain());
}

#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn prepared_payload_budget_rejects_oversize_and_survives_reentry() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut host = host();
    let plugin = host.load_plugin(TEST_SYNTH).expect("load TestSynth");
    let consumer = plugin
        .output_event_handle()
        .expect("in-process output handle");
    let mut realtime = enter_realtime(plugin, capacities(16));
    let mut buffers = AudioBuffers::new(0, 2, BLOCK, 48_000.0);

    let report = realtime
        .process(&mut buffers, &payload_probe(1), &[])
        .expect("accept bundle before exiting its pool generation");
    assert_eq!(report.output_event_overflows, 0);
    let report = realtime
        .process(&mut buffers, &payload_probe(2), &[])
        .expect("process oversize probe");
    assert_eq!(report.output_event_overflows, 1);

    // Keep the first bundle unread across pool teardown. Leaving realtime must materialize it
    // on this non-realtime thread, rather than keeping the old fixed arena alive through the
    // long-lived consumer handle.
    let plugin = leave_realtime(realtime);
    let mut realtime = enter_realtime(plugin, capacities(8));
    let report = realtime
        .process(&mut buffers, &payload_probe(1), &[])
        .expect("process after re-entry");
    assert_eq!(report.output_event_overflows, 0);
    let events = consumer.drain();
    assert_eq!(events.len(), 8, "oversize payload must not escape");
    assert_normal_payload_bundle(&events[..4]);
    assert_normal_payload_bundle(&events[4..]);
}

#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn saturated_payload_output_stays_allocation_free_until_the_consumer_drains() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut host = host();
    let plugin = host.load_plugin(TEST_SYNTH).expect("load TestSynth");
    let consumer = plugin
        .output_event_handle()
        .expect("in-process output handle");
    let mut realtime = enter_realtime(plugin, capacities(8));
    let mut buffers = AudioBuffers::new(0, 2, BLOCK, 48_000.0);

    // Warm the code path then drain it before the measured run. The measured 1025 blocks emit
    // 4100 events, four more than the ordinary lock-free output queue's 4096-event capacity.
    // That exercises replacement of queued payloads as well as repeated prepared-arena reuse.
    realtime
        .process(&mut buffers, &payload_probe(1), &[])
        .expect("warm payload output");
    drop(consumer.drain());

    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    let mut failures = 0usize;
    for _ in 0..1025 {
        if realtime
            .process(&mut buffers, &payload_probe(1), &[])
            .is_err()
        {
            failures += 1;
        }
    }
    ON.store(false, Ordering::Relaxed);

    assert_eq!(failures, 0, "every saturated payload block must process");
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "prepared payload output allocated, reallocated, or freed on the audio path"
    );

    // This Vec creation and payload ownership transfer happen after the audio-path meter is
    // disarmed, on the caller's consumer thread.
    let events = consumer.drain();
    assert_eq!(
        events.len(),
        4096,
        "ordinary output queue keeps newest events"
    );
    assert_normal_payload_bundle(&events[events.len() - 4..]);
}
