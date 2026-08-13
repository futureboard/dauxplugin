//! Driving one `process` block, allocating nothing.
//!
//! A test that wants to measure allocations must assemble the whole call — buses, context,
//! event ports — without allocating itself, or the measurement is of the harness rather
//! than of the plug-in. Everything here builds its views on the stack from storage the
//! caller already owns.
//!
//! The input and output storages passed to [`run_effect_block`] must be **distinct
//! objects**. In-place processing is legal at the ABI level (`abi-v1` §8) but a Rust
//! fixture cannot hold `&[T]` and `&mut [T]` over the same memory, so the harness never
//! aliases them.

use daux_audio::{AudioBufferRef, AudioBuses, AudioStorage};
use daux_core::daux_host_services::RtHostServices;
use daux_core::{DauxProcessor, ProcessConfig, ProcessContext, ProcessEvents, ProcessStatus};
use daux_events::EventBuffer;

/// [audio-thread] Runs one block through an effect: one input bus, one output bus.
///
/// `frames` is clamped to what both storages actually hold, so a test cannot accidentally
/// ask a processor to read past the end of its own fixture. A short block over long
/// storage becomes a sub-block view, which is what a real host hands out and what
/// [`AudioBuses`] insists on.
///
/// # Panics
///
/// If a sub-block of `frames` cannot be taken from either storage, which after the clamp
/// above can only mean a bug in this function.
pub fn run_effect_block(
    processor: &mut dyn DauxProcessor,
    config: &ProcessConfig,
    host: &RtHostServices,
    input: &AudioStorage<f32>,
    output: &mut AudioStorage<f32>,
    events_in: &EventBuffer,
    events_out: &mut EventBuffer,
    frames: usize,
) -> ProcessStatus {
    let frames = frames.min(input.frames()).min(output.frames());
    let whole_input = input.as_ref();
    let inputs = [whole_input
        .sub_block(0, frames)
        .expect("a sub-block of a clamped length always exists")];
    let mut whole_output = output.as_mut();
    let mut outputs = [whole_output
        .sub_block_mut(0, frames)
        .expect("a sub-block of a clamped length always exists")];
    let mut buses = AudioBuses::new(&inputs, &mut outputs, frames);
    let ctx = ProcessContext::new(frames, config, host);
    let mut events = ProcessEvents::new(events_in, events_out);
    processor.process(&ctx, &mut buses, &mut events)
}

/// [audio-thread] Runs one block through an instrument: no input bus, one output bus.
///
/// # Panics
///
/// If a sub-block of `frames` cannot be taken from `output`, which after the clamp can
/// only mean a bug in this function.
pub fn run_instrument_block(
    processor: &mut dyn DauxProcessor,
    config: &ProcessConfig,
    host: &RtHostServices,
    output: &mut AudioStorage<f32>,
    events_in: &EventBuffer,
    events_out: &mut EventBuffer,
    frames: usize,
) -> ProcessStatus {
    let frames = frames.min(output.frames());
    let inputs: [AudioBufferRef<'_, f32>; 0] = [];
    let mut whole_output = output.as_mut();
    let mut outputs = [whole_output
        .sub_block_mut(0, frames)
        .expect("a sub-block of a clamped length always exists")];
    let mut buses = AudioBuses::new(&inputs, &mut outputs, frames);
    let ctx = ProcessContext::new(frames, config, host);
    let mut events = ProcessEvents::new(events_in, events_out);
    processor.process(&ctx, &mut buses, &mut events)
}

/// [audio-thread] Runs one block through an event-only plug-in: no audio at all.
pub fn run_event_block(
    processor: &mut dyn DauxProcessor,
    config: &ProcessConfig,
    host: &RtHostServices,
    events_in: &EventBuffer,
    events_out: &mut EventBuffer,
    frames: usize,
) -> ProcessStatus {
    let mut buses = AudioBuses::<f32>::empty(frames);
    let ctx = ProcessContext::new(frames, config, host);
    let mut events = ProcessEvents::new(events_in, events_out);
    processor.process(&ctx, &mut buses, &mut events)
}

/// [main-thread] Runs `f` and asserts it allocated nothing, refusing to pass vacuously.
///
/// The allocation counters only move when the test binary installs
/// [`daux_rt::CountingAllocator`] as its `#[global_allocator]`. Without it every count is
/// zero and an assertion on the count would be meaningless, so this checks the tripwire
/// itself first.
///
/// # Panics
///
/// If the counting allocator is not installed, or if `f` allocated.
#[track_caller]
pub fn assert_no_alloc<R>(what: &str, f: impl FnOnce() -> R) -> R {
    assert!(
        daux_rt::counting_allocator_installed(),
        "{what}: daux_rt::CountingAllocator is not installed in this test binary, so the \
         allocation assertion would pass without measuring anything. Add \
         `#[global_allocator] static ALLOCATOR: daux_rt::CountingAllocator = \
         daux_rt::CountingAllocator;` to the test's root."
    );
    let (result, allocations) = daux_rt::AllocGuard::scope(f);
    assert_eq!(allocations, 0, "{what}: the audio thread allocated");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{EchoPlugin, GainPlugin, SynthPlugin};
    use daux_core::DauxPlugin;
    use daux_events::{DauxEvent, EventHeader, InputEvents, NoteEvent};

    fn config() -> ProcessConfig {
        ProcessConfig::new(48_000.0, 128)
    }

    #[test]
    fn an_effect_block_reaches_the_processor_and_writes_its_output() {
        let mut plugin = GainPlugin::default();
        let config = config();
        plugin.processor().prepare(&config).expect("prepares");

        let mut input = AudioStorage::<f32>::new(2, 128);
        input.fill(0.5);
        let mut output = AudioStorage::<f32>::new(2, 128);
        let events_in = EventBuffer::with_capacity(8, 64);
        let mut events_out = EventBuffer::with_capacity(8, 64);
        let host = RtHostServices::null();

        let status = run_effect_block(
            plugin.processor(),
            &config,
            &host,
            &input,
            &mut output,
            &events_in,
            &mut events_out,
            128,
        );
        assert_eq!(status, ProcessStatus::ContinueIfNotQuiet);
        // Unity gain by default, so the output is the input.
        assert_eq!(output.as_ref().sample(0, 0), Some(0.5));
        assert_eq!(output.as_ref().sample(1, 127), Some(0.5));
    }

    #[test]
    fn an_instrument_block_sounds_a_note() {
        let mut plugin = SynthPlugin::default();
        let config = config();
        plugin.processor().prepare(&config).expect("prepares");

        let mut output = AudioStorage::<f32>::new(2, 128);
        let mut events_in = EventBuffer::with_capacity(8, 64);
        events_in
            .try_push(&DauxEvent::NoteOn(NoteEvent {
                header: EventHeader::at(0),
                note_id: 1,
                key: 69,
                velocity: 1.0,
                ..NoteEvent::default()
            }))
            .expect("room for one note");
        let mut events_out = EventBuffer::with_capacity(8, 64);
        let host = RtHostServices::null();

        let status = run_instrument_block(
            plugin.processor(),
            &config,
            &host,
            &mut output,
            &events_in,
            &mut events_out,
            128,
        );
        assert_eq!(status, ProcessStatus::Continue);
        crate::assertions::assert_not_silent(
            output.channel(0).expect("a channel"),
            1e-6,
            "the synth fixture",
        );
    }

    #[test]
    fn an_event_block_needs_no_audio_buses_at_all() {
        let mut plugin = EchoPlugin::default();
        let config = config();
        plugin.processor().prepare(&config).expect("prepares");

        let mut events_in = EventBuffer::with_capacity(4, 32);
        for time in 0..3u32 {
            events_in
                .try_push(&DauxEvent::NoteOn(NoteEvent {
                    header: EventHeader::at(time),
                    ..NoteEvent::default()
                }))
                .expect("room");
        }
        let mut events_out = EventBuffer::with_capacity(4, 32);
        let host = RtHostServices::null();

        let status = run_event_block(
            plugin.processor(),
            &config,
            &host,
            &events_in,
            &mut events_out,
            64,
        );
        assert_eq!(status, ProcessStatus::Continue);
        assert_eq!(InputEvents::len(&events_out), 3);
    }

    #[test]
    fn the_no_alloc_helper_returns_the_value_when_nothing_allocated() {
        let value = assert_no_alloc("arithmetic", || 21 * 2);
        assert_eq!(value, 42);
    }

    #[test]
    #[should_panic(expected = "the audio thread allocated")]
    fn the_no_alloc_helper_catches_an_allocation() {
        assert_no_alloc("a deliberate allocation", || {
            core::hint::black_box(Vec::<u8>::with_capacity(64));
        });
    }
}
