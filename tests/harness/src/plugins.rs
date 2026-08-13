//! Fixture plug-ins built directly on the `daux-core` object model.
//!
//! Three of them, chosen because between them they cover every shape `process` can take:
//!
//! | Fixture | Shape | What it exercises |
//! | --- | --- | --- |
//! | [`GainPlugin`] | audio in → audio out | sample-accurate parameter automation, smoothing |
//! | [`SynthPlugin`] | notes → audio out | voice allocation, note events, output events |
//! | [`EchoPlugin`] | events → events | the bounded output queue and its overflow path |
//!
//! # Every `process` here is allocation-free
//!
//! That is not incidental — it is the property the real-time suite measures. Each
//! processor allocates its scratch memory in `prepare` (`[main-thread]`, while inactive)
//! and touches nothing but preallocated storage afterwards. Concretely:
//!
//! * Parameter values are read through the concrete [`daux_parameter::FloatParam`] /
//!   [`daux_parameter::BoolParam`] fields, which are atomic loads. `Params::param_refs`
//!   returns a `Vec` and is never called from `process`.
//! * Voices live in a fixed-size array, so a note storm steals rather than grows.
//! * Output events go through [`daux_events::OutputEvents::try_push`], whose
//!   [`daux_events::EventOverflow`] is handled by counting and dropping.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use daux_audio::{AudioBuses, BusInfo, BusLayout, ChannelLayout};
use daux_core::daux_host_services::{HostServices, TaskId};
use daux_core::{
    Capabilities, Category, DauxController, DauxError, DauxFactory, DauxPlugin, DauxProcessor,
    DauxResult, ErrorKind, EventPortLayout, Latency, PluginDescriptor, ProcessConfig,
    ProcessContext, ProcessEvents, ProcessStatus, Tail,
};
use daux_events::{DauxEvent, EventHeader, NoteEvent};
use daux_parameter::{
    BoolParam, FloatParam, Param, ParamId, ParamRange, Params, Smoother, Smoothing,
};
use daux_rt::{FixedVec, ScratchBuffers};
use daux_state::{StateReader, StateWriter};

// ---------------------------------------------------------------------------------------
// Shared identifiers. Permanent by rule (abi-v1 §14): renaming is free, renumbering is not.
// ---------------------------------------------------------------------------------------

/// Permanent id of the gain fixture's gain parameter.
pub const GAIN_PARAM: ParamId = ParamId(1);
/// Permanent id of the gain fixture's bypass parameter.
pub const BYPASS_PARAM: ParamId = ParamId(2);
/// Permanent id of the synth fixture's level parameter.
pub const LEVEL_PARAM: ParamId = ParamId(1);

/// Permanent id of the gain fixture.
pub const GAIN_ID: &str = "studio.futureboard.tests.gain";
/// Permanent id of the synth fixture.
pub const SYNTH_ID: &str = "studio.futureboard.tests.synth";
/// Permanent id of the event-echo fixture.
pub const ECHO_ID: &str = "studio.futureboard.tests.echo";

/// [audio-thread] Decibels to a linear gain factor, without pulling in `daux-dsp`.
#[must_use]
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -120.0 {
        0.0
    } else {
        10.0f32.powf(db / 20.0)
    }
}

// ---------------------------------------------------------------------------------------
// Gain
// ---------------------------------------------------------------------------------------

/// The gain fixture's parameters, shared by its processor, controller and any editor.
#[derive(Debug)]
pub struct GainParams {
    /// Output gain in decibels.
    pub gain_db: FloatParam,
    /// When set, the plug-in passes its input through untouched.
    pub bypass: BoolParam,
}

impl Default for GainParams {
    fn default() -> Self {
        Self {
            gain_db: FloatParam::new(
                GAIN_PARAM,
                "Gain",
                0.0,
                ParamRange::Linear {
                    min: -60.0,
                    max: 12.0,
                },
            )
            .with_unit("dB")
            .with_smoothing(Smoothing::Linear { ms: 5.0 }),
            bypass: BoolParam::new(BYPASS_PARAM, "Bypass", false),
        }
    }
}

impl Params for GainParams {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        vec![
            (GAIN_PARAM, &self.gain_db as &dyn Param),
            (BYPASS_PARAM, &self.bypass as &dyn Param),
        ]
    }

    fn param(&self, id: ParamId) -> Option<&dyn Param> {
        // Overridden so a lookup from the audio thread costs no allocation, as the trait's
        // documentation requires of any implementation `process` can reach.
        if id == GAIN_PARAM {
            Some(&self.gain_db)
        } else if id == BYPASS_PARAM {
            Some(&self.bypass)
        } else {
            None
        }
    }
}

/// The gain fixture's audio-thread half.
#[derive(Debug)]
pub struct GainProcessor {
    params: Arc<GainParams>,
    smoother: Smoother,
    /// One channel of per-sample gain, sized in `prepare` to `max_block_size`.
    ramp: ScratchBuffers<f32>,
    max_block: usize,
    prepared: bool,
}

impl GainProcessor {
    /// [main-thread] Builds a processor sharing `params`.
    #[must_use]
    pub fn new(params: Arc<GainParams>) -> Self {
        let smoother = params.gain_db.smoother();
        Self {
            params,
            smoother,
            ramp: ScratchBuffers::new(1, 0),
            max_block: 0,
            prepared: false,
        }
    }
}

impl DauxProcessor for GainProcessor {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;
        self.max_block = config.max_block_size as usize;
        // The only allocation this processor ever makes.
        self.ramp = ScratchBuffers::new(1, self.max_block);
        self.smoother = self.params.gain_db.smoother();
        self.smoother.prepare(config.sample_rate);
        self.smoother
            .reset_to(db_to_gain(self.params.gain_db.value_f32()));
        self.prepared = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.smoother
            .reset_to(db_to_gain(self.params.gain_db.value_f32()));
    }

    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        let frames = ctx.frames().min(self.max_block).min(audio.frames());
        if !self.prepared {
            audio.silence_outputs();
            return ProcessStatus::Error;
        }

        // 1. Build the per-sample gain ramp, applying parameter events at their exact
        //    sample offsets. Disjoint field borrows, so no clone and no allocation.
        let input_events = events.input();
        let event_count = input_events.len();
        let mut next_event = 0usize;
        let params = &self.params;
        let smoother = &mut self.smoother;
        let ramp = self.ramp.slice_mut(0, frames);
        for (frame, slot) in ramp.iter_mut().enumerate() {
            while next_event < event_count {
                let Some(event) = input_events.get(next_event) else {
                    break;
                };
                if event.time() as usize > frame {
                    break;
                }
                match event {
                    DauxEvent::ParamValue(p) if p.param_id == GAIN_PARAM.0 => {
                        params.gain_db.set(p.value);
                        smoother.set_target(db_to_gain(p.value as f32));
                    }
                    DauxEvent::ParamValue(p) if p.param_id == BYPASS_PARAM.0 => {
                        params.bypass.set(p.value >= 0.5);
                    }
                    _ => {}
                }
                next_event += 1;
            }
            *slot = smoother.next();
        }

        // 2. Apply it. Input and output may be the same memory, so the read of a frame
        //    always precedes the write of that frame.
        let Some(source) = audio.main_input() else {
            audio.silence_outputs();
            return ProcessStatus::Error;
        };
        let bypassed = self.params.bypass.value();
        let ramp = self.ramp.channel(0);
        let Some(mut destination) = audio.main_output() else {
            return ProcessStatus::Error;
        };
        if source.channel_count() == 0 {
            destination.fill_silence();
            return ProcessStatus::Continue;
        }
        for channel in 0..destination.channel_count() {
            let read_from = channel.min(source.channel_count() - 1);
            let input = source.channel(read_from);
            let output = destination.channel_mut(channel);
            for frame in 0..frames.min(input.len()).min(output.len()) {
                output[frame] = if bypassed {
                    input[frame]
                } else {
                    input[frame] * ramp[frame]
                };
            }
        }
        // A constant mask computed for the previous block would be a lie now.
        destination.clear_constant_mask();
        ProcessStatus::ContinueIfNotQuiet
    }

    fn latency(&self) -> Latency {
        Latency::Zero
    }

    fn tail(&self) -> Tail {
        Tail::None
    }
}

/// The gain fixture's main-thread half.
#[derive(Debug)]
pub struct GainController {
    params: Arc<GainParams>,
    host: Option<HostServices>,
    /// How many times the host drained the main-thread queue.
    pub main_thread_calls: usize,
    /// Every worker task the host ran, in order.
    pub worker_tasks: Vec<TaskId>,
}

impl GainController {
    /// [main-thread] Builds a controller sharing `params`.
    #[must_use]
    pub fn new(params: Arc<GainParams>) -> Self {
        Self {
            params,
            host: None,
            main_thread_calls: 0,
            worker_tasks: Vec::new(),
        }
    }

    /// [main-thread] The host services this controller was handed, if any.
    #[must_use]
    pub fn host(&self) -> Option<&HostServices> {
        self.host.as_ref()
    }
}

impl DauxController for GainController {
    fn params(&self) -> &dyn Params {
        self.params.as_ref()
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        w.put_f64("gain_db", self.params.gain_db.value());
        w.put_bool("bypass", self.params.bypass.value());
        Ok(())
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        // A missing key means an older version never wrote it: default rather than fail, or
        // the user's old project stops opening.
        self.params.gain_db.set(r.opt_f64("gain_db").unwrap_or(0.0));
        self.params
            .bypass
            .set(r.opt_bool("bypass").unwrap_or(false));
        Ok(())
    }

    fn set_host(&mut self, host: HostServices) {
        self.host = Some(host);
    }

    fn on_main_thread(&mut self) {
        self.main_thread_calls += 1;
    }

    fn on_worker(&mut self, task: TaskId) {
        self.worker_tasks.push(task);
    }
}

/// A stereo gain effect: one smoothed gain parameter, one bypass, no editor.
#[derive(Debug)]
pub struct GainPlugin {
    processor: GainProcessor,
    controller: GainController,
    params: Arc<GainParams>,
}

impl Default for GainPlugin {
    fn default() -> Self {
        let params = Arc::new(GainParams::default());
        Self {
            processor: GainProcessor::new(Arc::clone(&params)),
            controller: GainController::new(Arc::clone(&params)),
            params,
        }
    }
}

impl GainPlugin {
    /// [main-thread] The shared parameter bank, for a test that wants to poke it directly.
    #[must_use]
    pub fn params(&self) -> &Arc<GainParams> {
        &self.params
    }
}

impl DauxPlugin for GainPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder(GAIN_ID, "Harness Gain")
            .vendor("Futureboard Studio")
            .category(Category::Effect)
            .capabilities(
                Capabilities::NONE
                    .with_audio_effect()
                    .with_sample_accurate_auto(),
            )
            .build()
            .expect("the gain fixture's descriptor is well-formed")
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::stereo_effect()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        &mut self.processor
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        &mut self.controller
    }
}

// ---------------------------------------------------------------------------------------
// Synth
// ---------------------------------------------------------------------------------------

/// How many voices the synth fixture can sound at once. Fixed, so a note storm steals.
pub const SYNTH_VOICES: usize = 8;

/// The synth fixture's parameters.
#[derive(Debug)]
pub struct SynthParams {
    /// Output level, linear.
    pub level: FloatParam,
}

impl Default for SynthParams {
    fn default() -> Self {
        Self {
            level: FloatParam::new(
                LEVEL_PARAM,
                "Level",
                0.5,
                ParamRange::Linear { min: 0.0, max: 1.0 },
            ),
        }
    }
}

impl Params for SynthParams {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        vec![(LEVEL_PARAM, &self.level as &dyn Param)]
    }

    fn param(&self, id: ParamId) -> Option<&dyn Param> {
        (id == LEVEL_PARAM).then_some(&self.level as &dyn Param)
    }
}

/// One sounding voice.
#[derive(Clone, Copy, Debug)]
struct Voice {
    note_id: i32,
    key: i16,
    channel: i16,
    phase: f32,
    increment: f32,
    amplitude: f32,
    /// `true` once the key is released; the voice then fades and reports its end.
    releasing: bool,
    /// Monotonic order of arrival, used to pick the oldest voice to steal.
    age: u64,
}

/// The synth fixture's audio-thread half: eight sine voices and note-end reporting.
#[derive(Debug)]
pub struct SynthProcessor {
    params: Arc<SynthParams>,
    voices: FixedVec<Voice>,
    sample_rate: f64,
    max_block: usize,
    next_age: u64,
    prepared: bool,
    /// Note-end events the bounded output queue refused. A counter, never a `Vec`.
    dropped_note_ends: AtomicUsize,
    /// Voices stolen because all eight were busy.
    stolen: AtomicUsize,
}

impl SynthProcessor {
    /// [main-thread] Builds a processor sharing `params`.
    #[must_use]
    pub fn new(params: Arc<SynthParams>) -> Self {
        Self {
            params,
            voices: FixedVec::with_capacity(SYNTH_VOICES),
            sample_rate: 48_000.0,
            max_block: 0,
            next_age: 0,
            prepared: false,
            dropped_note_ends: AtomicUsize::new(0),
            stolen: AtomicUsize::new(0),
        }
    }

    /// [any-thread] How many note-end events the output queue refused.
    #[must_use]
    pub fn dropped_note_ends(&self) -> usize {
        self.dropped_note_ends.load(Ordering::Relaxed)
    }

    /// [any-thread] How many voices were stolen because all of them were busy.
    #[must_use]
    pub fn stolen_voices(&self) -> usize {
        self.stolen.load(Ordering::Relaxed)
    }

    /// [any-thread] How many voices are sounding right now.
    #[must_use]
    pub fn active_voices(&self) -> usize {
        self.voices.len()
    }

    /// [audio-thread] Starts a note, stealing the oldest voice when all are busy.
    fn note_on(&mut self, note: &NoteEvent) {
        let increment = midi_key_to_increment(note.key, self.sample_rate);
        let voice = Voice {
            note_id: note.note_id,
            key: note.key,
            channel: note.channel,
            phase: 0.0,
            increment,
            amplitude: note.velocity as f32,
            releasing: false,
            age: self.next_age,
        };
        self.next_age = self.next_age.wrapping_add(1);

        if self.voices.push(voice).is_err() && !self.voices.is_empty() {
            // Full: steal the oldest. Never grow — that would be an allocation.
            self.stolen.fetch_add(1, Ordering::Relaxed);
            let mut oldest = 0usize;
            let mut oldest_age = u64::MAX;
            for (index, candidate) in self.voices.as_slice().iter().enumerate() {
                if candidate.age < oldest_age {
                    oldest_age = candidate.age;
                    oldest = index;
                }
            }
            self.voices.as_mut_slice()[oldest] = voice;
        }
    }

    /// [audio-thread] Releases every voice matching the note-off, wildcards included.
    fn note_off(&mut self, note: &NoteEvent) {
        for voice in self.voices.as_mut_slice() {
            let id_matches = note.note_id < 0 || note.note_id == voice.note_id;
            let key_matches = note.key < 0 || note.key == voice.key;
            if id_matches && key_matches {
                voice.releasing = true;
            }
        }
    }
}

/// [audio-thread] Phase increment per sample for a MIDI key at `sample_rate`.
fn midi_key_to_increment(key: i16, sample_rate: f64) -> f32 {
    let clamped = key.clamp(0, 127);
    let hz = 440.0 * 2.0f64.powf((f64::from(clamped) - 69.0) / 12.0);
    (hz / sample_rate) as f32
}

impl DauxProcessor for SynthProcessor {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;
        self.sample_rate = config.sample_rate;
        self.max_block = config.max_block_size as usize;
        if self.voices.capacity() != SYNTH_VOICES {
            self.voices = FixedVec::with_capacity(SYNTH_VOICES);
        }
        self.voices.clear();
        self.prepared = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.voices.clear();
    }

    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        let frames = ctx.frames().min(audio.frames());
        if !self.prepared {
            audio.silence_outputs();
            return ProcessStatus::Error;
        }

        let (input_events, output_events) = events.split();
        for index in 0..input_events.len() {
            let Some(event) = input_events.get(index) else {
                break;
            };
            match event {
                DauxEvent::NoteOn(note) => self.note_on(&note),
                DauxEvent::NoteOff(note) | DauxEvent::NoteChoke(note) => self.note_off(&note),
                DauxEvent::ParamValue(p) if p.param_id == LEVEL_PARAM.0 => {
                    self.params.level.set(p.value);
                }
                _ => {}
            }
        }

        audio.silence_outputs();
        let level = self.params.level.value_f32();
        let Some(mut out) = audio.main_output() else {
            return ProcessStatus::Error;
        };
        let channels = out.channel_count();
        for voice in self.voices.as_mut_slice() {
            // Every channel renders the same voice from the same starting state, so the
            // end state of the last channel is the voice's state for the next block.
            let mut end_phase = voice.phase;
            let mut end_amplitude = voice.amplitude;
            for channel in 0..channels {
                let mut phase = voice.phase;
                let mut amplitude = voice.amplitude;
                let samples = out.channel_mut(channel);
                for sample in samples.iter_mut().take(frames) {
                    *sample += (core::f32::consts::TAU * phase).sin() * amplitude * level
                        / SYNTH_VOICES as f32;
                    phase += voice.increment;
                    if phase >= 1.0 {
                        phase -= 1.0;
                    }
                    if voice.releasing {
                        // A short linear fade, so a released note really does stop.
                        amplitude = (amplitude - 1.0 / 256.0).max(0.0);
                    }
                }
                end_phase = phase;
                end_amplitude = amplitude;
            }
            voice.phase = end_phase;
            voice.amplitude = end_amplitude;
        }

        // Report and retire the voices that finished. `retain` on a `FixedVec` would be
        // ideal; walking backwards with `swap_remove` is the allocation-free equivalent.
        let mut index = self.voices.len();
        while index > 0 {
            index -= 1;
            let voice = self.voices.as_slice()[index];
            if voice.releasing && voice.amplitude <= 0.0 {
                let ended = DauxEvent::NoteEnd(NoteEvent {
                    header: EventHeader::at(frames.saturating_sub(1) as u32),
                    note_id: voice.note_id,
                    channel: voice.channel,
                    key: voice.key,
                    velocity: 0.0,
                    tuning: 0.0,
                });
                if output_events.try_push(&ended).is_err() {
                    // A full output queue is normal and non-fatal (abi-v1 §9). Drop the
                    // event and count it; never allocate to work around it.
                    self.dropped_note_ends.fetch_add(1, Ordering::Relaxed);
                }
                self.voices.swap_remove(index);
            }
        }

        if self.voices.is_empty() {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }

    fn tail(&self) -> Tail {
        Tail::Samples(256)
    }
}

/// The synth fixture's main-thread half.
#[derive(Debug)]
pub struct SynthController {
    params: Arc<SynthParams>,
}

impl DauxController for SynthController {
    fn params(&self) -> &dyn Params {
        self.params.as_ref()
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        w.put_f64("level", self.params.level.value());
        Ok(())
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        self.params.level.set(r.opt_f64("level").unwrap_or(0.5));
        Ok(())
    }
}

/// An eight-voice sine instrument that answers note events and reports note ends.
#[derive(Debug)]
pub struct SynthPlugin {
    processor: SynthProcessor,
    controller: SynthController,
}

impl Default for SynthPlugin {
    fn default() -> Self {
        let params = Arc::new(SynthParams::default());
        Self {
            processor: SynthProcessor::new(Arc::clone(&params)),
            controller: SynthController { params },
        }
    }
}

impl SynthPlugin {
    /// [main-thread] The processor, for a test that wants its voice statistics.
    #[must_use]
    pub fn synth_processor(&self) -> &SynthProcessor {
        &self.processor
    }
}

impl DauxPlugin for SynthPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder(SYNTH_ID, "Harness Synth")
            .vendor("Futureboard Studio")
            .category(Category::Instrument)
            .capabilities(
                Capabilities::NONE
                    .with_instrument()
                    .with_midi_input()
                    .with_midi_output(),
            )
            .build()
            .expect("the synth fixture's descriptor is well-formed")
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::instrument(ChannelLayout::Stereo)
    }

    fn event_ports(&self) -> EventPortLayout {
        EventPortLayout::instrument()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        &mut self.processor
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        &mut self.controller
    }
}

// ---------------------------------------------------------------------------------------
// Event echo
// ---------------------------------------------------------------------------------------

/// The event-echo fixture's audio-thread half: forwards every input event, counting
/// whatever the bounded output queue refuses.
#[derive(Debug, Default)]
pub struct EchoProcessor {
    forwarded: AtomicUsize,
    overflowed: AtomicUsize,
}

impl EchoProcessor {
    /// [any-thread] How many events reached the output.
    #[must_use]
    pub fn forwarded(&self) -> usize {
        self.forwarded.load(Ordering::Relaxed)
    }

    /// [any-thread] How many events the output queue refused.
    #[must_use]
    pub fn overflowed(&self) -> usize {
        self.overflowed.load(Ordering::Relaxed)
    }

    /// [main-thread] Resets both counters.
    pub fn reset_counters(&self) {
        self.forwarded.store(0, Ordering::Relaxed);
        self.overflowed.store(0, Ordering::Relaxed);
    }
}

impl DauxProcessor for EchoProcessor {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()
    }

    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        _audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        let (input, output) = events.split();
        for index in 0..input.len() {
            let Some(event) = input.get(index) else {
                break;
            };
            match output.try_push(&event) {
                Ok(()) => {
                    self.forwarded.fetch_add(1, Ordering::Relaxed);
                }
                Err(daux_events::EventOverflow) => {
                    self.overflowed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        ProcessStatus::Continue
    }
}

/// A parameter bank with nothing in it.
#[derive(Debug, Default)]
pub struct NoParams;

impl Params for NoParams {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        Vec::new()
    }

    fn param(&self, _id: ParamId) -> Option<&dyn Param> {
        None
    }
}

/// The event-echo fixture's main-thread half.
#[derive(Debug, Default)]
pub struct EchoController {
    params: NoParams,
}

impl DauxController for EchoController {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    fn save_state(&self, _w: &mut StateWriter) -> DauxResult<()> {
        Ok(())
    }

    fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> {
        Ok(())
    }
}

/// A MIDI effect that echoes every event it is given, and counts what did not fit.
#[derive(Debug, Default)]
pub struct EchoPlugin {
    processor: EchoProcessor,
    controller: EchoController,
}

impl EchoPlugin {
    /// [main-thread] The processor, for its forwarded/overflowed counters.
    #[must_use]
    pub fn echo_processor(&self) -> &EchoProcessor {
        &self.processor
    }
}

impl DauxPlugin for EchoPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder(ECHO_ID, "Harness Echo")
            .vendor("Futureboard Studio")
            .category(Category::MidiEffect)
            .capabilities(
                Capabilities::NONE
                    .with_midi_effect()
                    .with_midi_input()
                    .with_midi_output(),
            )
            .build()
            .expect("the echo fixture's descriptor is well-formed")
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::new()
    }

    fn event_ports(&self) -> EventPortLayout {
        EventPortLayout::midi_effect()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        &mut self.processor
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        &mut self.controller
    }
}

// ---------------------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------------------

/// A factory publishing all three fixtures, in a fixed order.
///
/// Useful wherever a test needs a multi-plug-in module: enumeration, id lookup, and the
/// "unknown id" failure path.
#[derive(Debug, Default)]
pub struct HarnessFactory;

impl DauxFactory for HarnessFactory {
    fn plugin_count(&self) -> usize {
        3
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        match index {
            0 => Some(GainPlugin::descriptor()),
            1 => Some(SynthPlugin::descriptor()),
            2 => Some(EchoPlugin::descriptor()),
            _ => None,
        }
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        match id {
            GAIN_ID => Ok(Box::new(GainPlugin::default())),
            SYNTH_ID => Ok(Box::new(SynthPlugin::default())),
            ECHO_ID => Ok(Box::new(EchoPlugin::default())),
            other => Err(DauxError::new(
                ErrorKind::NotFound,
                format!("no plug-in `{other}` in the harness module"),
            )),
        }
    }
}

/// [main-thread] A sidechained variant of the stereo effect layout, for bus-negotiation tests.
#[must_use]
pub fn sidechain_layout() -> BusLayout {
    BusLayout::stereo_effect().with_input(
        BusInfo::new(1, "Sidechain", ChannelLayout::Mono)
            .with_purpose(daux_audio::BusPurpose::Sidechain),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixture_descriptor_validates_and_keeps_its_permanent_id() {
        for descriptor in [
            GainPlugin::descriptor(),
            SynthPlugin::descriptor(),
            EchoPlugin::descriptor(),
        ] {
            descriptor.validate().expect("a well-formed descriptor");
        }
        assert_eq!(GainPlugin::descriptor().id.as_str(), GAIN_ID);
        assert_eq!(SynthPlugin::descriptor().id.as_str(), SYNTH_ID);
        assert_eq!(EchoPlugin::descriptor().id.as_str(), ECHO_ID);
    }

    #[test]
    fn the_factory_enumerates_all_three_and_refuses_an_unknown_id() {
        let factory = HarnessFactory;
        assert_eq!(factory.plugin_count(), 3);
        assert_eq!(factory.descriptors().len(), 3);
        assert!(factory.contains(SYNTH_ID));
        assert!(factory.descriptor(3).is_none());
        let Err(error) = factory.create("com.example.absent") else {
            panic!("an unknown id must not produce a plug-in");
        };
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(factory.create(ECHO_ID).is_ok());
    }

    #[test]
    fn the_gain_parameter_bank_looks_up_without_allocating() {
        let params = GainParams::default();
        assert_eq!(params.param_refs().len(), 2);
        let ((), allocations) = daux_rt::AllocGuard::scope(|| {
            assert!(params.param(GAIN_PARAM).is_some());
            assert!(params.param(BYPASS_PARAM).is_some());
            assert!(params.param(ParamId(999)).is_none());
        });
        assert_eq!(allocations, 0, "`Params::param` must not allocate");
    }

    #[test]
    fn db_conversion_matches_the_usual_landmarks() {
        assert!((db_to_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_gain(-6.0) - 0.501_187).abs() < 1e-5);
        assert!((db_to_gain(6.0) - 1.995_262).abs() < 1e-5);
        assert_eq!(db_to_gain(-200.0), 0.0);
    }

    #[test]
    fn a_midi_key_maps_to_the_right_frequency() {
        // A4 = key 69 = 440 Hz.
        let increment = midi_key_to_increment(69, 48_000.0);
        assert!((f64::from(increment) * 48_000.0 - 440.0).abs() < 1e-3);
        // An out-of-range key clamps rather than producing a nonsense increment.
        assert!(midi_key_to_increment(-1, 48_000.0) > 0.0);
        assert!(midi_key_to_increment(9_999, 48_000.0).is_finite());
    }
}
