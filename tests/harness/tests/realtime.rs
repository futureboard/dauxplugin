//! The no-allocation guarantee, measured rather than asserted.
//!
//! `CLAUDE.md` hard rule #1 is that the audio thread never allocates. It is the rule most
//! easily broken by accident — a `format!` in a log line, a `Vec::push` in a voice
//! allocator, a `collect` in an event loop — and the breakage is invisible until a user
//! hears a dropout under load on a machine you do not own.
//!
//! Every other test in this repository would still pass with a `Box::new` in the middle of
//! `process`. This one would not.
//!
//! # How it measures
//!
//! `daux_rt::CountingAllocator` is installed as this binary's global allocator, so every
//! allocation anywhere in the process moves a counter. [`rig::assert_no_alloc`] runs a
//! closure inside a [`daux_rt::AllocGuard`] scope and asserts the counter did not move —
//! and first asserts that the allocator is actually installed, so the test cannot pass
//! vacuously if this attribute is ever lost.
//!
//! # What is deliberately outside the guard
//!
//! `prepare` allocates: that is the whole point of the lifecycle split, and it runs on the
//! main thread while the plug-in is inactive. Building fixtures, formatting failure
//! messages and enumerating parameters all allocate too. Only the calls that a real host
//! makes from its audio callback go inside the guard.

use std::sync::Arc;

use daux_core::daux_audio::AudioStorage;
use daux_core::daux_events::{DauxEvent, EventBuffer, EventHeader, NoteEvent, ParamEvent};
use daux_core::daux_host_services::RtHostServices;
use daux_core::{DauxProcessor, ProcessConfig, ProcessStatus};
use daux_tests::plugins::{
    EchoProcessor, GAIN_PARAM, GainParams, GainProcessor, SynthParams, SynthProcessor,
};
use daux_tests::rig::{self, run_effect_block, run_event_block, run_instrument_block};
use daux_tests::signal::{self, Rng};

/// Without this, every allocation count is zero and every assertion below passes without
/// measuring anything. `rig::assert_no_alloc` checks for it explicitly.
#[global_allocator]
static ALLOCATOR: daux_rt::CountingAllocator = daux_rt::CountingAllocator;

const SAMPLE_RATE: f64 = 48_000.0;
const MAX_BLOCK: usize = 512;

fn config() -> ProcessConfig {
    ProcessConfig::new(SAMPLE_RATE, MAX_BLOCK as u32)
}

/// A prepared gain processor and the storage a block needs, all allocated up front.
struct EffectRig {
    processor: GainProcessor,
    config: ProcessConfig,
    host: RtHostServices,
    input: AudioStorage<f32>,
    output: AudioStorage<f32>,
    events_in: EventBuffer,
    events_out: EventBuffer,
}

impl EffectRig {
    /// [main-thread] Allocates everything, so the audio-thread half can allocate nothing.
    fn new() -> Self {
        let params = Arc::new(GainParams::default());
        // The processor holds its own Arc; the rig does not need to keep one.
        // Automation is driven through param events, not by touching the params directly.
        let mut processor = GainProcessor::new(params);
        let config = config();
        processor
            .prepare(&config)
            .expect("the fixture prepares at 48 kHz");
        processor.activate().expect("the fixture activates");

        let mut input = AudioStorage::<f32>::new(2, MAX_BLOCK);
        let mut rng = Rng::new(0x5EED);
        for channel in 0..2 {
            signal::fill_noise(
                input
                    .channel_mut(channel)
                    .expect("the fixture has two channels"),
                &mut rng,
            );
        }

        Self {
            processor,
            config,
            host: RtHostServices::null(),
            input,
            output: AudioStorage::<f32>::new(2, MAX_BLOCK),
            events_in: EventBuffer::with_capacity(256, 8 * 1024),
            events_out: EventBuffer::with_capacity(256, 8 * 1024),
        }
    }

    /// [audio-thread] One block, exactly as a host would call it.
    fn block(&mut self, frames: usize) -> ProcessStatus {
        run_effect_block(
            &mut self.processor,
            &self.config,
            &self.host,
            &self.input,
            &mut self.output,
            &self.events_in,
            &mut self.events_out,
            frames,
        )
    }
}

#[test]
fn an_effect_block_allocates_nothing() {
    let mut rig = EffectRig::new();
    // Warm up outside the guard: a first call may touch lazily-initialised statics that a
    // real host would have touched long before the audio thread started.
    rig.block(MAX_BLOCK);

    rig::assert_no_alloc("gain effect, full block", || {
        for _ in 0..16 {
            rig.block(MAX_BLOCK);
        }
    });
}

#[test]
fn every_block_size_allocates_nothing() {
    let mut rig = EffectRig::new();
    rig.block(MAX_BLOCK);

    // A host is free to vary the block size call to call, and a processor that sizes
    // anything from `frames` rather than from `max_block_size` allocates on the way up.
    rig::assert_no_alloc("gain effect, varying block sizes", || {
        for frames in [1, 2, 3, 7, 15, 16, 17, 63, 64, 65, 127, 255, 511, MAX_BLOCK] {
            rig.block(frames);
        }
    });
}

#[test]
fn a_block_of_zero_frames_allocates_nothing_and_does_not_panic() {
    let mut rig = EffectRig::new();
    rig.block(MAX_BLOCK);

    // Hosts do send empty blocks. A processor that divides by `frames` or indexes `[0]`
    // dies here, and one that allocates a zero-length buffer still allocates.
    rig::assert_no_alloc("gain effect, zero frames", || {
        rig.block(0);
    });
}

#[test]
fn parameter_changes_during_a_block_allocate_nothing() {
    let mut rig = EffectRig::new();
    rig.block(MAX_BLOCK);

    // Sample-accurate automation is the path where a naive implementation reaches for a
    // Vec of pending changes.
    for i in 0..64u32 {
        let value = -60.0 + f64::from(i);
        rig.events_in
            .try_push(&DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(i * 8),
                param_id: GAIN_PARAM.0,
                value,
                ..ParamEvent::default()
            }))
            .expect("the fixture buffer holds 256 events");
    }

    rig::assert_no_alloc("gain effect, 64 automation points", || {
        rig.block(MAX_BLOCK);
    });
}

#[test]
fn reset_and_deactivate_allocate_nothing() {
    let mut rig = EffectRig::new();
    rig.block(MAX_BLOCK);

    // `reset` runs on the audio thread when the user relocates the playhead — mid-playback,
    // under the same deadline as `process`.
    rig::assert_no_alloc("gain effect, reset", || {
        rig.processor.reset();
        rig.processor.reset();
    });

    rig::assert_no_alloc("gain effect, deactivate", || {
        rig.processor.deactivate();
    });
}

#[test]
fn latency_and_tail_queries_allocate_nothing() {
    let rig = EffectRig::new();
    // A host polls these from the audio thread between blocks.
    rig::assert_no_alloc("gain effect, latency and tail", || {
        let _ = rig.processor.latency();
        let _ = rig.processor.tail();
    });
}

#[test]
fn an_instrument_rendering_notes_allocates_nothing() {
    let params = Arc::new(SynthParams::default());
    let mut processor = SynthProcessor::new(Arc::clone(&params));
    let config = config();
    processor.prepare(&config).expect("the fixture prepares");
    processor.activate().expect("the fixture activates");

    let host = RtHostServices::null();
    let mut output = AudioStorage::<f32>::new(2, MAX_BLOCK);
    let mut events_in = EventBuffer::with_capacity(256, 8 * 1024);
    let mut events_out = EventBuffer::with_capacity(256, 8 * 1024);

    // Voice allocation is where an instrument most often allocates: a `Vec<Voice>` that
    // grows on the first chord is the classic bug this test exists to catch.
    for i in 0..16u8 {
        events_in
            .try_push(&DauxEvent::NoteOn(NoteEvent {
                header: EventHeader::at(u32::from(i) * 16),
                key: 36 + i16::from(i),
                velocity: 0.8,
                ..NoteEvent::default()
            }))
            .expect("the fixture buffer holds 256 events");
    }

    let mut block = |events_in: &EventBuffer, events_out: &mut EventBuffer| {
        run_instrument_block(
            &mut processor,
            &config,
            &host,
            &mut output,
            events_in,
            events_out,
            MAX_BLOCK,
        )
    };
    block(&events_in, &mut events_out);

    rig::assert_no_alloc("instrument, 16 simultaneous notes", || {
        for _ in 0..8 {
            block(&events_in, &mut events_out);
        }
    });
}

#[test]
fn an_instrument_overflowing_its_voices_allocates_nothing() {
    let params = Arc::new(SynthParams::default());
    let mut processor = SynthProcessor::new(Arc::clone(&params));
    let config = config();
    processor.prepare(&config).expect("the fixture prepares");
    processor.activate().expect("the fixture activates");

    let host = RtHostServices::null();
    let mut output = AudioStorage::<f32>::new(2, MAX_BLOCK);
    let mut events_in = EventBuffer::with_capacity(256, 8 * 1024);
    let mut events_out = EventBuffer::with_capacity(256, 8 * 1024);

    // Far more notes than any fixed voice pool holds. Voice stealing must happen without
    // growing anything.
    for i in 0..128u8 {
        events_in
            .try_push(&DauxEvent::NoteOn(NoteEvent {
                header: EventHeader::at(u32::from(i) % 512),
                key: i16::from(i),
                velocity: 0.9,
                ..NoteEvent::default()
            }))
            .expect("the fixture buffer holds 256 events");
    }

    rig::assert_no_alloc("instrument, 128 notes into a fixed voice pool", || {
        run_instrument_block(
            &mut processor,
            &config,
            &host,
            &mut output,
            &events_in,
            &mut events_out,
            MAX_BLOCK,
        );
    });
}

#[test]
fn an_event_effect_allocates_nothing_including_when_its_output_is_full() {
    let mut processor = EchoProcessor::default();
    let config = config();
    processor.prepare(&config).expect("the fixture prepares");
    processor.activate().expect("the fixture activates");

    let host = RtHostServices::null();
    let mut events_in = EventBuffer::with_capacity(256, 8 * 1024);
    for i in 0..64u8 {
        events_in
            .try_push(&DauxEvent::NoteOn(NoteEvent {
                header: EventHeader::at(u32::from(i)),
                key: 60 + i16::from(i % 12),
                velocity: 0.7,
                ..NoteEvent::default()
            }))
            .expect("the fixture buffer holds 256 events");
    }

    // Deliberately far too small: `try_push` will return `EventOverflow` part-way through.
    // Handling that must not allocate and must not panic — the overflow path is the one a
    // plug-in author is least likely to have exercised.
    let mut tiny_out = EventBuffer::with_capacity(4, 128);
    let mut roomy_out = EventBuffer::with_capacity(256, 8 * 1024);

    run_event_block(
        &mut processor,
        &config,
        &host,
        &events_in,
        &mut roomy_out,
        MAX_BLOCK,
    );

    rig::assert_no_alloc("event effect, output overflows", || {
        tiny_out.clear();
        run_event_block(
            &mut processor,
            &config,
            &host,
            &events_in,
            &mut tiny_out,
            MAX_BLOCK,
        );
    });

    rig::assert_no_alloc("event effect, output has room", || {
        roomy_out.clear();
        run_event_block(
            &mut processor,
            &config,
            &host,
            &events_in,
            &mut roomy_out,
            MAX_BLOCK,
        );
    });
}

#[test]
fn the_null_host_services_allocate_nothing() {
    let host = RtHostServices::null();
    // A processor calls these from `process`. `log` in particular takes a `&str` and must
    // not format or box it.
    rig::assert_no_alloc("null host services", || {
        host.log(daux_rt::LogLevel::Warn, "a fixed message");
        host.request_callback();
        host.request_process();
        host.request_restart();
    });
}

#[test]
fn the_signal_fixtures_themselves_allocate_nothing() {
    // If the fixtures allocated, every assertion above would be measuring the fixture
    // rather than the plug-in, and a real allocation in `process` could hide inside the
    // noise. This is the test that makes the others trustworthy.
    let mut storage = AudioStorage::<f32>::new(2, MAX_BLOCK);
    let mut rng = Rng::new(1);

    rig::assert_no_alloc("signal fixtures", || {
        signal::fill_noise(storage.channel_mut(0).expect("channel 0 exists"), &mut rng);
        // `fill_sine` returns the phase to continue from on the next block.
        let _phase = signal::fill_sine(
            storage.channel_mut(1).expect("channel 1 exists"),
            440.0,
            SAMPLE_RATE,
            0.0,
        );
        signal::fill_ramp(storage.channel_mut(0).expect("channel 0 exists"), -1.0, 1.0);
        signal::fill_impulse(storage.channel_mut(1).expect("channel 1 exists"), 0);
    });
}

#[test]
fn the_tripwire_actually_trips() {
    // A guarantee whose detector does not work is worse than no guarantee, because it
    // reads as evidence. This asserts the counter moves when something *does* allocate, so
    // the passing tests above mean what they say.
    assert!(
        daux_rt::counting_allocator_installed(),
        "the counting allocator must be installed for this suite to measure anything"
    );

    let (value, allocations) = daux_rt::AllocGuard::scope(|| {
        let v: Vec<u8> = Vec::with_capacity(1024);
        v.capacity()
    });
    assert_eq!(value, 1024);
    assert!(
        allocations > 0,
        "AllocGuard did not observe an obvious heap allocation, so every other assertion \
         in this file is vacuous"
    );
}
