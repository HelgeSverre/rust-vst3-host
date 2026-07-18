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
    hard_realtime::{
        ControllerSyncStatus, RealtimeCapacities, RealtimeMidiEvent, RealtimeParameterChange,
    },
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

/// The exclusive hard-realtime lifecycle keeps the same TestSynth instance, translates its
/// controller-derived MIDI mappings through fixed parameter storage, supports an off-thread
/// stop/restart, and performs no host allocation, reallocation, or deallocation while processing
/// repeated mixed audio/MIDI/parameter blocks. The fixture must be built before this ignored test;
/// a missing bundle is treated as a local skip so the ordinary test suite remains portable.
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn exclusive_hard_realtime_lifecycle_is_allocation_free() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test_plugins/TestSynth.vst3"
    );
    if !std::path::Path::new(path).exists() {
        println!("TestSynth.vst3 not found, skipping");
        return;
    }

    let mut host = Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(512)
        .build()
        .expect("build host");
    let plugin = host.load_plugin(path).expect("load TestSynth");
    let original_uid = plugin.info().uid.clone();

    // A failed off-thread preparation must return ownership of the exact ordinary instance.
    let invalid = RealtimeCapacities {
        max_block_frames: 0,
        max_input_events: 16,
        max_output_events: 16,
        max_parameter_changes: 32,
        max_distinct_parameters: 16,
    };
    let failure = match plugin.try_into_realtime(invalid) {
        Err(failure) => failure,
        Ok(_) => panic!("a zero-sized realtime block must be rejected"),
    };
    let (plugin, _) = failure.into_parts();
    assert_eq!(plugin.info().uid, original_uid);

    let capacities = RealtimeCapacities {
        max_block_frames: 512,
        max_input_events: 16,
        max_output_events: 16,
        max_parameter_changes: 32,
        max_distinct_parameters: 16,
    };
    let mut realtime = match plugin.try_into_realtime(capacities) {
        Ok(realtime) => realtime,
        Err(failure) => panic!(
            "prepare and transactionally start TestSynth: {}",
            failure.error()
        ),
    };
    let mut buffers = AudioBuffers::new(0, 2, 512, 48_000.0);
    let channel = MidiChannel::Ch1;
    let make_midi = |step: usize| {
        [
            RealtimeMidiEvent {
                event: MidiEvent::NoteOn {
                    channel,
                    note: 60,
                    velocity: 100,
                },
                sample_offset: 0,
            },
            RealtimeMidiEvent {
                event: MidiEvent::ControlChange {
                    channel,
                    controller: 74,
                    value: (step % 128) as u8,
                },
                sample_offset: 32,
            },
            RealtimeMidiEvent {
                event: MidiEvent::PitchBend {
                    channel,
                    value: ((step * 97) % 16_384) as u16,
                },
                sample_offset: 64,
            },
            RealtimeMidiEvent {
                event: MidiEvent::ChannelAftertouch {
                    channel,
                    pressure: ((step * 3) % 128) as u8,
                },
                sample_offset: 96,
            },
            RealtimeMidiEvent {
                event: MidiEvent::ProgramChange {
                    channel,
                    program: (step % 4) as u8,
                },
                sample_offset: 128,
            },
            RealtimeMidiEvent {
                event: MidiEvent::NoteOff {
                    channel,
                    note: 60,
                    velocity: 0,
                },
                sample_offset: 400,
            },
        ]
    };
    let make_parameter = |step: usize| RealtimeParameterChange {
        id: 4,
        value: (step % 101) as f64 / 100.0,
        sample_offset: 200,
    };

    // Warm every route, including IDataExchange, before measuring. TestSynth maps CC74,
    // pitch bend, channel aftertouch, and root-unit program changes to four parameter ids.
    for step in 0..16 {
        let report = realtime
            .process(&mut buffers, &make_midi(step), &[make_parameter(step)])
            .expect("warm hard-realtime process graph");
        assert_eq!(report.input_events, 2);
        assert_eq!(report.parameter_changes, 5);
        assert_eq!(report.distinct_parameters, 5);
    }
    realtime.stop().expect("stop off-thread");
    realtime.start().expect("restart off-thread");

    // Keep the armed loop free of formatting, panics, and owned error values. Every input lives
    // on the stack, and the API returns only copyable counters/errors.
    let mut process_failures = 0usize;
    let mut last_resonance = 0.0;
    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    for step in 0..200 {
        let parameter = make_parameter(step);
        last_resonance = parameter.value;
        if realtime
            .process(&mut buffers, &make_midi(step), &[parameter])
            .is_err()
        {
            process_failures += 1;
        }
    }
    ON.store(false, Ordering::Relaxed);

    let allocation_operations = ALLOCS.load(Ordering::Relaxed);
    assert_eq!(process_failures, 0, "all measured blocks must process");
    assert_eq!(
        allocation_operations, 0,
        "exclusive hard-realtime process() allocated, reallocated, or freed {allocation_operations} times"
    );

    let (plugin, exit) = match realtime.try_into_plugin() {
        Ok(restored) => restored,
        Err(failure) => panic!(
            "restore the same ordinary plugin off-thread: {}",
            failure.error()
        ),
    };
    assert_eq!(plugin.info().uid, original_uid);
    assert!(!plugin.is_processing());
    assert_ne!(exit.controller_sync, ControllerSyncStatus::Unsupported);
    let restored_resonance = plugin
        .get_parameter(4)
        .expect("read synchronized TestSynth resonance");
    assert!(
        (restored_resonance - last_resonance).abs() < 1e-6,
        "restored controller value {restored_resonance} did not match {last_resonance}"
    );
}
