//! Minimal stereo gain effect: one parameter, no GUI.
//!
//! This is the "hello world" of DAUxPlug and the shortest complete plug-in the framework
//! can express. Read it first; every other example is this one plus a topic.
//!
//! # What it shows
//!
//! | Piece | Where |
//! |---|---|
//! | `#[derive(DauxParams)]` — a parameter bank from attributes | [`GainParams`] |
//! | `#[derive(DauxPlugin)]` — the static descriptor | [`Gain`] |
//! | `prepare` allocates, `process` never does | [`Gain::prepare`] / [`Gain::process`] |
//! | sample-accurate automation | the ramp loop in `process` |
//! | versioned state that survives a schema bump | [`Gain::save_state`] / [`Gain::load_state`] |
//! | one line that exports `.axt`, VST3 and CLAP | [`export_plugin!`] at the bottom |
//!
//! # The one rule
//!
//! `process` and everything it calls must not allocate — no `Vec::push`, no `format!`, no
//! `Mutex::lock`. The naive way to write a per-sample gain ramp is
//! `let ramp: Vec<f32> = (0..frames).map(..).collect()`, and it is a real-time bug. The
//! preallocated alternative is [`Gain::gain_ramp`], sized once in `prepare` from
//! [`ProcessConfig::max_block_size`] and only ever *written* in `process`. That trade —
//! decide the size on the main thread, fill the buffer on the audio thread — is the whole
//! technique, and it is what every other example does too.
//!
//! # Build it
//!
//! ```text
//! cargo build -p daux-example-gain --release
//! cargo run -p daux-cli -- bundle --package daux-example-gain
//! ```

use daux_plugin::dsp::db_to_gain;
use daux_plugin::prelude::*;

/// The permanent id of the gain parameter.
///
/// Named rather than repeated as a literal, because it appears in three places: the
/// `#[param(..)]` attribute, the event filter in `process` and the state key. Renaming the
/// parameter is free; renumbering it silently corrupts every saved project that used the old
/// number, so the number is fixed forever the day the plug-in ships.
const GAIN_ID: u32 = 1;

/// The state key the gain value is stored under.
///
/// Also permanent, and deliberately *not* the same string as the display name: the display
/// name is free to change with a translation or a rewording.
const GAIN_KEY: &str = "gain_db";

/// The schema version [`Gain::save_state`] writes.
///
/// Bump it when the meaning of a key changes; [`Gain::load_state`] then has a version to
/// branch on. Adding a key does not need a bump, because a reader treats a missing key as
/// "written by an older version" (see `load_state`).
const STATE_VERSION: u32 = 1;

/// The gain parameter, as a bank of exactly one.
///
/// `#[derive(DauxParams)]` generates `impl Params` and — because every field is fully
/// described by its attribute — an inherent `new()`. `[main-thread]`
#[derive(DauxParams)]
pub struct GainParams {
    /// Output gain in decibels.
    #[param(
        id = GAIN_ID,
        name = "Gain",
        range = -60.0..=12.0,
        default = 0.0,
        unit = "dB",
        decimals = 1,
        smoothing = "exponential(15.0)",
        flags(automatable, modulatable)
    )]
    pub gain: FloatParam,
}

/// A stereo gain: multiply, and nothing else.
///
/// The type is its own processor **and** its own controller, which is the right shape when
/// the two halves share only the parameters. A plug-in whose DSP owns significant state that
/// the main thread must not touch should split them into two types instead, and connect them
/// with a `daux-rt` queue rather than a lock.
#[derive(DauxPlugin)]
#[plugin(
    id = "studio.futureboard.daux.example.gain",
    name = "DAUx Gain",
    vendor = "Futureboard Studio",
    version = "1.0.0",
    description = "Minimal stereo gain effect.",
    license = "MIT OR Apache-2.0",
    category = "effect",
    capabilities(audio_effect, sample_accurate_auto, offline_render, sandbox_safe),
    features("utility", "gain"),
    state_schema_version = STATE_VERSION
)]
pub struct Gain {
    /// The parameter bank, shared with the host and with any editor.
    params: GainParams,
    /// Ramps the gain over the block so an automation jump does not click.
    smoother: Smoother,
    /// One linear gain coefficient per sample of the largest block the host promised.
    ///
    /// Allocated in [`prepare`](Gain::prepare) and only written in
    /// [`process`](Gain::process). This is the buffer the module documentation is about.
    gain_ramp: Vec<f32>,
}

impl Default for Gain {
    /// `[main-thread]` A fresh instance at the parameter's default value.
    fn default() -> Self {
        let params = GainParams::new();
        let smoother = params.gain.smoother();
        Self {
            params,
            smoother,
            // Empty until `prepare`: the host has not said how large a block gets yet, and
            // guessing would either waste memory or force an allocation later.
            gain_ramp: Vec::new(),
        }
    }
}

impl DauxProcessor for Gain {
    /// `[main-thread]` Sizes everything from `config`. The only place this plug-in allocates.
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        // Never trust the host's numbers: a NaN sample rate turns every coefficient below
        // into a silent NaN factory, and a zero block size would size the ramp to nothing.
        config.validate()?;

        let max_block = config.max_block_size as usize;
        self.gain_ramp.clear();
        self.gain_ramp.resize(max_block, 1.0);
        // `resize` may leave spare capacity from an earlier, larger `prepare`. That is fine —
        // it is memory, not a re-allocation, and `process` only ever indexes `..frames`.

        self.smoother.prepare(config.sample_rate);
        self.smoother
            .reset_to(db_to_gain(self.params.gain.value_f32()));
        Ok(())
    }

    /// `[audio-thread]` Clears the ramp's history so a relocate does not glide from the old
    /// value.
    fn reset(&mut self) {
        self.smoother
            .reset_to(db_to_gain(self.params.gain.value_f32()));
    }

    /// `[audio-thread]` Copies input to output and applies the ramped gain.
    ///
    /// Allocation-free from top to bottom. The `process_never_allocates` test at the bottom
    /// of this file proves it rather than asserting it in a comment.
    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        // A conforming host never exceeds the prepared `max_block_size`. Clamping rather than
        // growing the buffer is the only allocation-free answer to a host that does.
        let frames = ctx.frames().min(self.gain_ramp.len());

        // The value the host or an editor left in the parameter is where this block starts.
        // Automation events below override it from their own sample offset onwards.
        self.smoother
            .set_target(db_to_gain(self.params.gain.value_f32()));

        // --- one gain coefficient per sample, automation applied at its exact offset ------
        //
        // The ramp is filled in segments: everything up to the next automation point with the
        // current target, then the target changes, then the next segment. Applying every
        // event at the top of the block instead would quantise automation to the block size,
        // which is audible as stepping on a fast fade.
        let mut filled = 0usize;
        for index in 0..events.input().len() {
            let Some(DauxEvent::ParamValue(event)) = events.input().get(index) else {
                continue;
            };
            if event.param_id != GAIN_ID {
                continue;
            }
            // Events arrive sorted by time (abi-v1 §9); `max` keeps a host that breaks that
            // promise from making the segment length negative.
            let at = (event.header.time as usize).clamp(filled, frames);
            self.smoother.next_block(&mut self.gain_ramp[filled..at]);
            filled = at;

            self.params.gain.set_plain(event.value);
            self.smoother.set_target(db_to_gain(event.value as f32));
        }
        self.smoother
            .next_block(&mut self.gain_ramp[filled..frames]);

        // --- apply it ---------------------------------------------------------------------
        let input = audio.main_input();
        let Some(mut output) = audio.main_output() else {
            // No output bus at all: nothing to write, and nothing went wrong.
            return ProcessStatus::Sleep;
        };

        if let Some(input) = input {
            // A `memmove`, not a `memcpy`: hosts are allowed to hand the same memory in as
            // input and out as output (abi-v1 §8), and this stays correct when they do.
            if output.copy_from(&input).is_err() {
                // The host gave us a layout we did not agree to. Silence is the only defined
                // output we can produce without guessing.
                output.fill_silence();
                return ProcessStatus::Error;
            }
        }

        let ramp = &self.gain_ramp[..frames];
        for channel in output.split_channels_mut() {
            for (sample, gain) in channel.iter_mut().zip(ramp) {
                *sample *= gain;
            }
        }

        // A pure function of its input with no internal energy: once the host sees silence
        // going in, it may stop calling us.
        ProcessStatus::ContinueIfNotQuiet
    }
}

impl DauxController for Gain {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    /// `[main-thread]` Writes the one value that is not reproducible from anything else.
    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        w.put_f64(GAIN_KEY, self.params.gain.value());
        Ok(())
    }

    /// `[main-thread]` Restores it, tolerating a blob an older version wrote.
    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        // `opt_f64` rather than `f64`: a key that is missing was written by a version that
        // did not have it yet, and refusing to load would stop the user's old project from
        // opening at all. Keeping the current default is the graceful answer.
        if let Some(db) = r.opt_f64(GAIN_KEY) {
            self.params.gain.set_plain(db);
        }
        Ok(())
    }
}

impl DauxPlugin for Gain {
    /// `[main-thread]` The descriptor `#[derive(DauxPlugin)]` generated.
    ///
    /// The inherent `Self::descriptor()` shadows this trait method inside the impl, so this
    /// is a delegation rather than a recursion.
    fn descriptor() -> PluginDescriptor {
        Self::descriptor()
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::stereo_effect()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        self
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        self
    }

    /// `[main-thread]` Mono and stereo are both fine; anything else is not.
    ///
    /// The descriptor does not advertise `DYNAMIC_BUSES`, so a host will only ever propose a
    /// layout once, before activation — but answering honestly is what stops it proposing
    /// 7.1 and then being surprised by the result.
    fn accepts_bus_layout(&self, layout: &BusLayout) -> bool {
        let channels = |bus: Option<&BusInfo>| bus.map_or(0, BusInfo::channel_count);
        let inputs = channels(layout.main_input());
        let outputs = channels(layout.main_output());
        layout.inputs.len() <= 1
            && layout.outputs.len() == 1
            && matches!(outputs, 1 | 2)
            && (inputs == 0 || inputs == outputs)
    }
}

// One line, three formats. With `features = ["axt", "vst3", "clap"]` this emits
// `daux_plugin_entry_v1`, `GetPluginFactory` and `clap_entry`; nothing in the code above is
// specific to any of them.
export_plugin!(SingleFactory<Gain>);

/// The allocation tripwire, installed only while this crate's tests are compiled. Production
/// builds of the plug-in are untouched.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_plugin::CountingAllocator = daux_plugin::CountingAllocator;

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin::EventBuffer;
    use daux_plugin::daux_rt::{AllocGuard, counting_allocator_installed};

    /// 48 kHz, blocks of at most 512 frames.
    fn config() -> ProcessConfig {
        ProcessConfig::new(48_000.0, 512)
    }

    /// Runs one block through a prepared plug-in and hands back the stereo output.
    ///
    /// The input is a constant `1.0` in both channels, so the output *is* the gain the
    /// plug-in applied, sample by sample.
    fn run(gain: &mut Gain, frames: usize, events: &EventBuffer) -> AudioStorage<f32> {
        let mut input = AudioStorage::<f32>::new(2, frames);
        input.fill(1.0);
        let mut output = AudioStorage::<f32>::new(2, frames);

        let config = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(frames, &config, &host);

        {
            let inputs = [input.as_ref()];
            let mut outputs = [output.as_mut()];
            let mut buses = AudioBuses::new(&inputs, &mut outputs, frames);
            let mut sink = EventBuffer::with_capacity(16, 256);
            let mut ports = ProcessEvents::new(events, &mut sink);

            let status = gain.process(&ctx, &mut buses, &mut ports);
            assert_ne!(status, ProcessStatus::Error, "the block must not fail");
        }
        output
    }

    /// A prepared plug-in with the gain parameter at `db` and its ramp already settled there,
    /// so a test measures the gain rather than the smoother's ramp-in.
    fn prepared(db: f64) -> Gain {
        let mut gain = Gain::default();
        gain.params.gain.set_plain(db);
        gain.prepare(&config()).expect("a valid config");
        gain
    }

    /// A prepared plug-in with smoothing switched off, so a test measures the sample-accuracy
    /// of the event handling rather than the shape of the smoother's ramp.
    fn prepared_unsmoothed() -> Gain {
        let mut gain = Gain {
            smoother: Smoother::new(Smoothing::None),
            ..Gain::default()
        };
        gain.prepare(&config()).expect("a valid config");
        gain
    }

    #[test]
    fn unity_gain_passes_the_signal_through_unchanged() {
        let mut gain = prepared(0.0);
        let out = run(&mut gain, 256, &EventBuffer::with_capacity(1, 16));
        for (i, &sample) in out.as_slice().iter().enumerate() {
            assert!(
                (sample - 1.0).abs() < 1e-6,
                "sample {i} is {sample}, expected 1.0"
            );
        }
    }

    #[test]
    fn the_gain_actually_scales_the_signal() {
        // -6 dB is a factor of ~0.501, +6 dB a factor of ~1.995. A plug-in that forgot to
        // multiply, or multiplied by the decibel value itself, fails both.
        for db in [-60.0, -12.0, -6.0, 6.0, 12.0] {
            let mut gain = prepared(db);
            let out = run(&mut gain, 128, &EventBuffer::with_capacity(1, 16));
            let expected = db_to_gain(db as f32);
            let last = out.channel(0).expect("channel 0")[127];
            assert!(
                (last - expected).abs() < 1e-4,
                "{db} dB produced {last}, expected {expected}"
            );
        }
    }

    #[test]
    fn every_channel_gets_the_same_gain() {
        let mut gain = prepared(-6.0);
        let out = run(&mut gain, 64, &EventBuffer::with_capacity(1, 16));
        let left = out.channel(0).expect("channel 0");
        let right = out.channel(1).expect("channel 1");
        assert_eq!(left, right, "the two channels diverged");
    }

    #[test]
    fn automation_is_applied_at_its_own_sample_offset() {
        // The smoother would blur an instantaneous jump, so this test drives a plug-in with
        // smoothing switched off: what is left is purely the sample-accuracy of the event
        // handling, which is what is under test.
        let mut gain = prepared_unsmoothed();

        let mut events = EventBuffer::with_capacity(4, 64);
        events
            .try_push(&DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(100),
                param_id: GAIN_ID,
                value: -6.0,
                ..ParamEvent::default()
            }))
            .expect("the buffer has room");

        let out = run(&mut gain, 256, &events);
        let left = out.channel(0).expect("channel 0");

        // Before the event: still unity.
        assert!((left[0] - 1.0).abs() < 1e-6, "sample 0 is {}", left[0]);
        assert!((left[99] - 1.0).abs() < 1e-6, "sample 99 is {}", left[99]);
        // From the event's own sample onwards: the new value, not one block later.
        let expected = db_to_gain(-6.0);
        assert!(
            (left[100] - expected).abs() < 1e-6,
            "sample 100 is {}, expected {expected}",
            left[100]
        );
        assert!((left[255] - expected).abs() < 1e-6);
    }

    #[test]
    fn several_automation_points_in_one_block_are_all_honoured() {
        let mut gain = prepared_unsmoothed();

        let mut events = EventBuffer::with_capacity(4, 64);
        for (time, db) in [(0u32, -12.0f64), (64, 0.0), (192, 12.0)] {
            events
                .try_push(&DauxEvent::ParamValue(ParamEvent {
                    header: EventHeader::at(time),
                    param_id: GAIN_ID,
                    value: db,
                    ..ParamEvent::default()
                }))
                .expect("the buffer has room");
        }

        let out = run(&mut gain, 256, &events);
        let left = out.channel(0).expect("channel 0");
        for (frame, db) in [
            (0usize, -12.0f32),
            (63, -12.0),
            (64, 0.0),
            (191, 0.0),
            (192, 12.0),
            (255, 12.0),
        ] {
            let expected = db_to_gain(db);
            assert!(
                (left[frame] - expected).abs() < 1e-6,
                "frame {frame} is {}, expected {expected} ({db} dB)",
                left[frame]
            );
        }
        // The last event is what the parameter is left holding, so the next block starts there.
        assert_eq!(gain.params.gain.value(), 12.0);
    }

    #[test]
    fn an_event_for_another_parameter_is_ignored() {
        let mut gain = prepared(0.0);
        let mut events = EventBuffer::with_capacity(4, 64);
        events
            .try_push(&DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(0),
                param_id: GAIN_ID + 1_000,
                value: -60.0,
                ..ParamEvent::default()
            }))
            .expect("the buffer has room");

        let out = run(&mut gain, 32, &events);
        assert!((out.channel(0).expect("channel 0")[31] - 1.0).abs() < 1e-6);
        assert_eq!(gain.params.gain.value(), 0.0);
    }

    #[test]
    fn an_event_past_the_end_of_the_block_does_not_index_out_of_bounds() {
        // A host bug, but one that must not panic on the audio thread.
        let mut gain = prepared(0.0);
        let mut events = EventBuffer::with_capacity(4, 64);
        events
            .try_push(&DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(9_999),
                param_id: GAIN_ID,
                value: -6.0,
                ..ParamEvent::default()
            }))
            .expect("the buffer has room");
        let out = run(&mut gain, 32, &events);
        assert_eq!(out.channel(0).expect("channel 0").len(), 32);
    }

    #[test]
    fn a_block_larger_than_the_prepared_maximum_is_clamped_rather_than_grown() {
        // Also a host bug. The alternative — growing `gain_ramp` — is an allocation on the
        // audio thread, which is worse than the wrong answer for a few samples.
        let mut gain = prepared(-6.0);
        let capacity = gain.gain_ramp.capacity();
        let out = run(&mut gain, 1_024, &EventBuffer::with_capacity(1, 16));
        assert_eq!(
            gain.gain_ramp.capacity(),
            capacity,
            "process must not have re-allocated the ramp"
        );
        assert_eq!(out.channel(0).expect("channel 0").len(), 1_024);
    }

    #[test]
    fn process_never_allocates() {
        assert!(
            counting_allocator_installed(),
            "the tripwire is not installed, so this test would pass vacuously"
        );

        let mut gain = prepared(-3.0);
        let frames = 512;
        let mut input = AudioStorage::<f32>::new(2, frames);
        input.fill(0.25);
        let mut output = AudioStorage::<f32>::new(2, frames);

        let mut events = EventBuffer::with_capacity(8, 128);
        for (time, db) in [(0u32, -6.0f64), (128, 0.0), (400, 6.0)] {
            events
                .try_push(&DauxEvent::ParamValue(ParamEvent {
                    header: EventHeader::at(time),
                    param_id: GAIN_ID,
                    value: db,
                    ..ParamEvent::default()
                }))
                .expect("the buffer has room");
        }

        let config = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(frames, &config, &host);
        let inputs = [input.as_ref()];
        let mut outputs = [output.as_mut()];
        let mut buses = AudioBuses::new(&inputs, &mut outputs, frames);
        let mut sink = EventBuffer::with_capacity(8, 128);
        let mut ports = ProcessEvents::new(&events, &mut sink);

        let (status, allocations) =
            AllocGuard::scope(|| gain.process(&ctx, &mut buses, &mut ports));
        assert_eq!(status, ProcessStatus::ContinueIfNotQuiet);
        assert_eq!(allocations, 0, "process allocated {allocations} time(s)");
    }

    #[test]
    fn state_round_trips_through_a_blob() {
        let gain = Gain::default();
        gain.params.gain.set_plain(-13.5);

        let mut writer = StateWriter::new(StateVersion(STATE_VERSION));
        gain.save_state(&mut writer).expect("saving cannot fail");
        let blob = writer.finish();

        let mut restored = Gain::default();
        let reader = StateReader::from_bytes(&blob).expect("the blob we just wrote parses");
        restored.load_state(&reader).expect("loading cannot fail");
        assert_eq!(restored.params.gain.value(), -13.5);
    }

    #[test]
    fn a_blob_from_an_older_version_loads_with_the_default_kept() {
        // Version 0 of this plug-in did not have a gain parameter at all. Loading its state
        // must not fail, or the user's project stops opening.
        let writer = StateWriter::new(StateVersion(STATE_VERSION));
        let blob = writer.finish();

        let mut gain = Gain::default();
        gain.params.gain.set_plain(-20.0);
        let reader = StateReader::from_bytes(&blob).expect("an empty blob still parses");
        gain.load_state(&reader)
            .expect("a missing key is not an error");
        assert_eq!(
            gain.params.gain.value(),
            -20.0,
            "the value must be untouched"
        );
    }

    #[test]
    fn the_descriptor_is_valid_and_says_what_the_code_does() {
        let d = <Gain as DauxPlugin>::descriptor();
        d.validate().expect("the descriptor must be valid");
        assert_eq!(d.id.as_str(), "studio.futureboard.daux.example.gain");
        assert_eq!(d.category, Category::Effect);
        assert!(d.capabilities.is_audio_effect());
        assert!(
            d.capabilities.is_sample_accurate_auto(),
            "process really does apply automation per sample, so the flag is honest"
        );
        assert!(
            !d.capabilities.is_has_gui(),
            "this example is headless; advertising a GUI would show the user an empty window"
        );
        assert_eq!(d.state_schema_version, STATE_VERSION);
    }

    #[test]
    fn the_factory_exports_exactly_this_plug_in() {
        let factory = SingleFactory::<Gain>::new();
        assert_eq!(factory.plugin_count(), 1);
        assert!(
            factory
                .create("studio.futureboard.daux.example.gain")
                .is_ok()
        );
        assert!(
            factory
                .create("studio.futureboard.daux.example.other")
                .is_err()
        );
    }

    #[test]
    fn the_parameter_id_and_range_are_the_ones_the_descriptor_promises() {
        let params = GainParams::new();
        let refs = params.param_refs();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, ParamId::new(GAIN_ID));

        let info = params.gain.info();
        assert_eq!(info.min, -60.0);
        assert_eq!(info.max, 12.0);
        assert_eq!(info.default, 0.0);
        assert_eq!(info.unit, "dB");
        // The lookup the audio thread uses must find it by permanent id.
        assert!(params.param(ParamId::new(GAIN_ID)).is_some());
        assert!(params.param(ParamId::new(GAIN_ID + 1)).is_none());
    }

    #[test]
    fn only_mono_and_stereo_layouts_are_accepted() {
        let gain = Gain::default();
        assert!(gain.accepts_bus_layout(&BusLayout::stereo_effect()));
        assert!(gain.accepts_bus_layout(&BusLayout::mono_effect()));
        assert!(!gain.accepts_bus_layout(&BusLayout::effect(ChannelLayout::Surround5_1)));
        // Mismatched in/out channel counts would make `copy_from` fail every block.
        let lopsided = BusLayout::new()
            .with_input(BusInfo::main("In", ChannelLayout::Mono))
            .with_output(BusInfo::main("Out", ChannelLayout::Stereo));
        assert!(!gain.accepts_bus_layout(&lopsided));
    }

    #[test]
    fn prepare_refuses_a_configuration_it_cannot_size_from() {
        let mut gain = Gain::default();
        assert!(gain.prepare(&ProcessConfig::new(f64::NAN, 512)).is_err());
        assert!(gain.prepare(&ProcessConfig::new(48_000.0, 0)).is_err());
        assert!(
            gain.gain_ramp.is_empty(),
            "a rejected config must not have sized anything"
        );
    }

    #[test]
    fn re_preparing_with_a_smaller_block_does_not_leave_a_stale_ramp() {
        let mut gain = prepared(0.0);
        assert_eq!(gain.gain_ramp.len(), 512);
        gain.prepare(&ProcessConfig::new(96_000.0, 64))
            .expect("a valid config");
        assert_eq!(gain.gain_ramp.len(), 64);
    }
}
