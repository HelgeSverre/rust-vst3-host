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
        RealtimeProcessError,
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
/// repeated mixed audio/MIDI/parameter blocks including Mapping 2 fan-out. A seek and playing
/// transport must survive the round trip and continue through ordinary processing; stopped
/// transport must remain stationary during a second realtime tenure. Build the fixture before
/// this ignored test; a missing bundle is treated as a skip so the ordinary suite stays portable.
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
    let (mut plugin, _) = failure.into_parts();
    assert_eq!(plugin.info().uid, original_uid);
    plugin
        .set_transport_position(vst3_host::TransportPosition {
            samples: -512,
            quarter_notes: -0.5,
        })
        .expect("seek before realtime ownership");
    plugin.set_tempo(60.0).expect("set transport tempo");
    plugin.set_playing(true).expect("start project transport");

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
                event: MidiEvent::ControlChange {
                    channel,
                    controller: 75,
                    value: (step % 128) as u8,
                },
                sample_offset: 180,
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
    // pitch bend, channel aftertouch, and root-unit program changes to four parameter ids;
    // CC75 adds points for cutoff and resonance, overlapping the explicit resonance parameter.
    for step in 0..16 {
        let report = realtime
            .process(&mut buffers, &make_midi(step), &[make_parameter(step)])
            .expect("warm hard-realtime process graph");
        assert_eq!(report.input_events, 2);
        assert_eq!(report.parameter_changes, 7);
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

    // The fixture applies whole parameter queues, so a program change can overwrite cutoff
    // regardless of offsets in a different queue. Observe fan-out in a final program-free block.
    let report = realtime
        .process(
            &mut buffers,
            &[RealtimeMidiEvent {
                event: MidiEvent::ControlChange {
                    channel,
                    controller: 75,
                    value: 63,
                },
                sample_offset: 180,
            }],
            &[RealtimeParameterChange {
                id: 4,
                value: last_resonance,
                sample_offset: 200,
            }],
        )
        .unwrap();
    assert_eq!(report.parameter_changes, 3);
    assert_eq!(report.distinct_parameters, 2);

    let (mut plugin, exit) = match realtime.try_into_plugin() {
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
    assert!(
        (plugin.get_parameter(0).unwrap() - 63.0 / 127.0).abs() < 1e-6,
        "Mapping 2 must deliver cutoff as well as its final resonance assignment"
    );
    let frames = 217 * 512;
    let position = plugin.transport_position().unwrap();
    assert_eq!(position.samples, -512 + frames);
    assert!((position.quarter_notes - (-0.5 + frames as f64 / 48_000.0)).abs() < 1e-9);
    plugin.start_processing().unwrap();
    plugin.process_audio(&mut buffers).unwrap();
    assert_eq!(
        plugin.transport_position().unwrap().samples,
        position.samples + 512
    );
    plugin.stop_processing().unwrap();
    plugin.set_playing(false).unwrap();
    let stopped_position = plugin.transport_position().unwrap();
    let mut realtime = match plugin.try_into_realtime(capacities) {
        Ok(realtime) => realtime,
        Err(failure) => panic!("reenter realtime ownership: {}", failure.error()),
    };
    realtime.process(&mut buffers, &[], &[]).unwrap();
    let (mut plugin, _) = match realtime.try_into_plugin() {
        Ok(restored) => restored,
        Err(failure) => panic!("restore stopped transport: {}", failure.error()),
    };
    assert_eq!(plugin.transport_position().unwrap(), stopped_position);
    plugin.reconfigure(44_100.0, 256).unwrap();
    assert_eq!(plugin.transport_position().unwrap(), stopped_position);
}

/// The queued runner API and the direct slice API are two presentations of one prepared process
/// engine. They must therefore produce the same TestSynth block from a cold start, including a
/// sample-offset note, Mapping 2's CC75 two-parameter fan-out, an explicit parameter point, and
/// queued transport setup. This also makes the allocation contract stronger than a warmed-up
/// loop: the first block after `start_with_capacities` and the first block after a restart must
/// not allocate, reallocate, or free host memory.
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn realtime_runner_queue_and_direct_share_the_prepared_engine() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    const BLOCK: usize = 512;
    const SAMPLE_RATE: f64 = 48_000.0;
    const RESONANCE: f64 = 0.73;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test_plugins/TestSynth.vst3"
    );
    assert!(
        std::path::Path::new(path).exists(),
        "build TestSynth before running this regression: just test-plugin"
    );

    let capacities = RealtimeCapacities {
        max_block_frames: BLOCK,
        max_input_events: 3,
        max_output_events: 8,
        // CC75 expands to cutoff and resonance, then the explicit resonance point joins that
        // queue. Leave room for the two same-id resonance points as distinct queue points.
        max_parameter_changes: 3,
        max_distinct_parameters: 2,
    };
    let position = vst3_host::TransportPosition {
        samples: -512,
        quarter_notes: -0.5,
    };
    let midi = [
        RealtimeMidiEvent {
            event: MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 100,
            },
            sample_offset: 64,
        },
        RealtimeMidiEvent {
            event: MidiEvent::ControlChange {
                channel: MidiChannel::Ch1,
                controller: 75,
                value: 63,
            },
            sample_offset: 180,
        },
        RealtimeMidiEvent {
            event: MidiEvent::NoteOff {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 0,
            },
            sample_offset: 400,
        },
    ];
    let parameters = [RealtimeParameterChange {
        id: 4,
        value: RESONANCE,
        sample_offset: 0,
    }];

    // The direct caller configures transport before ownership crosses into the audio runner.
    // The queued caller applies the exact same configuration as its first control batch below.
    let mut direct_host = Vst3Host::builder()
        .sample_rate(SAMPLE_RATE)
        .block_size(BLOCK)
        .build()
        .expect("build direct host");
    let mut direct_plugin = direct_host
        .load_plugin(path)
        .expect("load direct TestSynth");
    direct_plugin
        .set_transport_position(position)
        .expect("seed direct position");
    direct_plugin.set_tempo(60.0).expect("seed direct tempo");
    direct_plugin
        .set_playing(true)
        .expect("seed direct playing state");
    let (mut direct, _direct_control) = RealtimePluginRunner::new(direct_plugin, 8);
    direct
        .start_with_capacities(capacities)
        .expect("prepare direct runner");
    let mut direct_buffers = AudioBuffers::new(0, 2, BLOCK, SAMPLE_RATE);

    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    let direct_first = direct.process_direct(&mut direct_buffers, &midi, &parameters);
    ON.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "the first direct prepared block allocated, reallocated, or freed memory"
    );
    let direct_first = direct_first.expect("process first direct block");
    assert_eq!(direct_first.input_events, 2);
    assert_eq!(direct_first.parameter_changes, 3);
    assert_eq!(direct_first.distinct_parameters, 2);
    assert!(
        (direct
            .plugin()
            .get_parameter(4)
            .expect("read direct resonance after processing")
            - 63.0 / 127.0)
            .abs()
            < 1e-9,
        "the direct path must synchronize CC75's later resonance point before exit"
    );
    assert!(
        (direct
            .plugin()
            .get_parameter(0)
            .expect("read direct cutoff after processing")
            - 63.0 / 127.0)
            .abs()
            < 1e-9,
        "the direct path must synchronize CC75's cutoff fan-out before exit"
    );
    let direct_audio = direct_buffers.outputs.clone();
    assert!(
        direct_audio
            .iter()
            .flatten()
            .take(64)
            .all(|sample| *sample == 0.0),
        "the direct path lost the note's sample offset"
    );
    assert!(
        direct_audio
            .iter()
            .flatten()
            .skip(64)
            .any(|sample| sample.abs() > 1e-5),
        "the direct path produced no audio after the scheduled note"
    );

    direct.stop().expect("stop direct runner");
    direct
        .start_with_capacities(capacities)
        .expect("restart direct runner");
    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    let direct_restart = direct.process_direct(&mut direct_buffers, &[], &[]);
    ON.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "the first direct block after restart allocated, reallocated, or freed memory"
    );
    direct_restart.expect("process direct block after restart");
    let direct_plugin = direct.into_plugin();
    let direct_position = direct_plugin.transport_position().expect("direct position");
    let direct_cutoff = direct_plugin.get_parameter(0).expect("direct cutoff");
    let direct_resonance = direct_plugin.get_parameter(4).expect("direct resonance");
    // TestSynth's instrumentation is process-global, so destroy the first COM instance before
    // loading the independently initialized queued comparison instance.
    drop(direct_plugin);
    drop(direct_host);

    let mut queued_host = Vst3Host::builder()
        .sample_rate(SAMPLE_RATE)
        .block_size(BLOCK)
        .build()
        .expect("build queued host");
    let queued_plugin = queued_host
        .load_plugin(path)
        .expect("load queued TestSynth");
    let (mut queued, mut control) = RealtimePluginRunner::new(queued_plugin, 8);
    queued
        .start_with_capacities(capacities)
        .expect("prepare queued runner");
    assert!(control.set_transport_position(position));
    assert!(control.set_tempo(60.0));
    assert!(control.set_playing(true));
    assert!(control.set_parameter(4, RESONANCE));
    for scheduled in midi {
        assert!(control.send_midi_at(
            scheduled.event,
            scheduled.sample_offset.try_into().expect("offset fits i32"),
        ));
    }
    let mut queued_buffers = AudioBuffers::new(0, 2, BLOCK, SAMPLE_RATE);

    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    let queued_first = queued.process(&mut queued_buffers);
    ON.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "the first queued prepared block allocated, reallocated, or freed memory"
    );
    queued_first.expect("process first queued block");
    assert!(
        (queued
            .plugin()
            .get_parameter(4)
            .expect("read queued resonance after processing")
            - 63.0 / 127.0)
            .abs()
            < 1e-9,
        "the queued path must synchronize CC75's later resonance point before exit"
    );
    assert!(
        (queued
            .plugin()
            .get_parameter(0)
            .expect("read queued cutoff after processing")
            - 63.0 / 127.0)
            .abs()
            < 1e-9,
        "the queued path must synchronize CC75's cutoff fan-out before exit"
    );
    assert_eq!(
        queued_buffers.outputs, direct_audio,
        "queued and direct processing must render the same first block"
    );

    queued.stop().expect("stop queued runner");
    queued
        .start_with_capacities(capacities)
        .expect("restart queued runner");
    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    let queued_restart = queued.process(&mut queued_buffers);
    ON.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "the first queued block after restart allocated, reallocated, or freed memory"
    );
    queued_restart.expect("process queued block after restart");
    let queued_plugin = queued.into_plugin();
    assert_eq!(
        queued_plugin.transport_position().expect("queued position"),
        direct_position,
        "queued transport setup must reach the same recovered position as direct processing"
    );
    assert!(
        (queued_plugin.get_parameter(0).expect("queued cutoff") - direct_cutoff).abs() < 1e-9,
        "CC75 fan-out must survive queued processing and recovery"
    );
    assert!(
        (queued_plugin.get_parameter(4).expect("queued resonance") - direct_resonance).abs() < 1e-9,
        "explicit parameter changes must survive queued processing and recovery"
    );
}

/// Queue commands must be validated against the prepared engine's event capacity instead of
/// being silently truncated by a runner-side scratch vector. A rejected batch clears cleanly so
/// the next single command can still be rendered. The final CC75 block also proves an input-event
/// capacity of one does not reject Mapping 2 fan-out: its two resulting parameter points use the
/// parameter capacities.
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn realtime_runner_queue_reports_capacity_and_recovers() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    const BLOCK: usize = 512;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test_plugins/TestSynth.vst3"
    );
    assert!(
        std::path::Path::new(path).exists(),
        "build TestSynth before running this regression: just test-plugin"
    );
    let mut host = Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(BLOCK)
        .build()
        .expect("build host");
    let plugin = host.load_plugin(path).expect("load TestSynth");
    let (mut runner, mut control) = RealtimePluginRunner::new(plugin, 8);
    runner
        .start_with_capacities(RealtimeCapacities {
            max_block_frames: BLOCK,
            max_input_events: 1,
            max_output_events: 8,
            max_parameter_changes: 2,
            max_distinct_parameters: 2,
        })
        .expect("prepare runner");
    let mut buffers = AudioBuffers::new(0, 2, BLOCK, 48_000.0);

    assert!(control.send_midi(MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 100,
    }));
    assert!(control.send_midi(MidiEvent::NoteOff {
        channel: MidiChannel::Ch1,
        note: 60,
        velocity: 0,
    }));
    assert!(
        runner.process(&mut buffers).is_err(),
        "a two-event queued batch must be rejected when only one input event was prepared"
    );
    assert!(
        buffers
            .outputs
            .iter()
            .flatten()
            .all(|sample| *sample == 0.0),
        "a rejected batch must clear output rather than render a truncated prefix"
    );

    // A stopped runner rejects on the audio side before draining its control ring. The note must
    // therefore remain queued for the first block after restart instead of disappearing while
    // the output is being cleared for the rejected call.
    runner.stop().expect("stop runner");
    assert!(control.send_midi(MidiEvent::NoteOn {
        channel: MidiChannel::Ch1,
        note: 64,
        velocity: 100,
    }));
    assert!(matches!(
        runner.process(&mut buffers),
        Err(vst3_host::Error::RealtimeProcess(
            RealtimeProcessError::NotProcessing
        ))
    ));
    assert!(
        buffers
            .outputs
            .iter()
            .flatten()
            .all(|sample| *sample == 0.0),
        "a stopped runner must clear output"
    );
    runner
        .start_with_capacities(RealtimeCapacities {
            max_block_frames: BLOCK,
            max_input_events: 1,
            max_output_events: 8,
            max_parameter_changes: 2,
            max_distinct_parameters: 2,
        })
        .expect("restart runner");
    runner
        .process(&mut buffers)
        .expect("the queued note must render after restart");
    assert!(
        buffers
            .outputs
            .iter()
            .flatten()
            .any(|sample| sample.abs() > 1e-5),
        "the stopped runner must retain, not discard, queued input"
    );

    assert!(control.send_midi(MidiEvent::ControlChange {
        channel: MidiChannel::Ch1,
        controller: 75,
        value: 63,
    }));
    runner
        .process(&mut buffers)
        .expect("a valid queued CC75 block must recover after capacity rejection");
    let plugin = runner.into_plugin();
    assert!(
        (plugin.get_parameter(0).expect("cutoff") - 63.0 / 127.0).abs() < 1e-6,
        "CC75's cutoff target must fit in parameter capacity independently of event capacity"
    );
    assert!(
        (plugin.get_parameter(4).expect("resonance") - 63.0 / 127.0).abs() < 1e-6,
        "CC75's resonance target must fit in parameter capacity independently of event capacity"
    );
}

/// Entering the fixed engine with ordinary queued input is intentionally rejected: silently
/// translating unbounded ordinary queues would lose work or allocate on the audio thread. The
/// failed runner must nevertheless return the same ordinary owner with its queued note intact.
#[test]
#[ignore = "Requires the bundled TestSynth (just test-plugin)"]
fn rejected_runner_start_preserves_ordinary_queued_midi() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    const BLOCK: usize = 512;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test_plugins/TestSynth.vst3"
    );
    assert!(
        std::path::Path::new(path).exists(),
        "build TestSynth before running this regression: just test-plugin"
    );
    let mut host = Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(BLOCK)
        .build()
        .expect("build host");
    let mut plugin = host.load_plugin(path).expect("load TestSynth");
    plugin
        .send_midi_note(60, 100, MidiChannel::Ch1)
        .expect("queue ordinary note");

    let (mut runner, _control) = RealtimePluginRunner::new(plugin, 8);
    assert!(
        runner
            .start_with_capacities(RealtimeCapacities {
                max_block_frames: BLOCK,
                max_input_events: 8,
                max_output_events: 8,
                max_parameter_changes: 8,
                max_distinct_parameters: 8,
            })
            .is_err(),
        "the fixed runner must reject pending ordinary input"
    );

    let mut plugin = runner.into_plugin();
    plugin
        .start_processing()
        .expect("start recovered ordinary plugin");
    let mut buffers = AudioBuffers::new(0, 2, BLOCK, 48_000.0);
    plugin
        .process_audio(&mut buffers)
        .expect("process recovered queued note");
    assert!(
        buffers
            .outputs
            .iter()
            .flatten()
            .any(|sample| sample.abs() > 1e-5),
        "the rejected transition lost the ordinary queued note"
    );
}
