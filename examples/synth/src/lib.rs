//! Polyphonic subtractive synthesizer with sample-accurate MIDI.
//!
//! Sixteen voices, one oscillator each, an ADSR envelope and a resonant low-pass filter.
//! Small enough to read in one sitting, complete enough to be a real starting point.
//!
//! # What it shows
//!
//! | Topic | Where |
//! |---|---|
//! | **sample-accurate events** — the block is split at every event offset | [`Synth::process`] |
//! | fixed voice pool with deterministic stealing | [`Synth::allocate_voice`] |
//! | note on / off / choke, and the same notes arriving as raw MIDI 1.0 | [`Synth::apply_event`] |
//! | per-note expression (pressure, tuning, brightness) | [`Voice::apply_expression`] |
//! | ADSR and a per-voice biquad | [`Adsr`], [`Voice::render_into`] |
//! | telling the host a voice ended, and coping when the output is full | [`Synth::render`] |
//!
//! # Sample accuracy is the point
//!
//! A block is 128 to 2048 samples — 3 to 40 ms. Applying every event at the start of the
//! block quantises the performance to that grid, which is audible as slop on fast passages
//! and as a *chord* where the player meant an arpeggio. [`Synth::process`] therefore never
//! renders a whole block in one go. It walks the event list, renders the audio *up to* each
//! event's offset, applies the event, and continues:
//!
//! ```text
//! events:        ▼ 0          ▼ 137                 ▼ 400
//! block:   ├──────┼────────────┼─────────────────────┼──────────────┤
//!          render │   render   │       render        │    render    │
//!          0..0   │   0..137   │      137..400       │   400..512   │
//! ```
//!
//! The same loop applies parameter automation, so a filter sweep is smooth rather than
//! stepped, and it costs nothing but an index.
//!
//! # The audio thread never allocates
//!
//! Everything is preallocated:
//!
//! * the voices are a fixed `[Voice; 16]` array built in `Default`, so a note-on searches an
//!   array rather than pushing to a `Vec` — running out of voices *steals* one, which is what
//!   a synth should do anyway;
//! * the mix buffer and the level ramp are sized in [`Synth::prepare`] from
//!   `max_block_size`;
//! * the note-end output is the host's bounded buffer, and a full one is counted, not grown.
//!
//! # What a shipping synth would add
//!
//! Band-limited oscillators (these alias above a few kHz), a filter envelope, an LFO,
//! oversampling, and per-voice panning. None of them change the shape of anything here.

use daux_plugin::dsp::{Biquad, db_to_gain, flush_denormal};
use daux_plugin::prelude::*;

/// How many notes can sound at once.
///
/// Fixed at compile time on purpose: the voice pool is an array, so a note-on is a search
/// rather than an allocation. Sixteen is enough for two hands and a sustain pedal.
pub const VOICE_COUNT: usize = 16;

/// Concert A, the pitch every other note is derived from.
const A4_HZ: f64 = 440.0;

/// MIDI key number of concert A.
const A4_KEY: f64 = 69.0;

/// The permanent parameter ids. Renaming a parameter is free; renumbering one silently
/// corrupts every saved project that used the old number.
mod param_id {
    /// Oscillator waveform.
    pub const WAVEFORM: u32 = 1;
    /// Envelope attack time, milliseconds.
    pub const ATTACK: u32 = 2;
    /// Envelope decay time, milliseconds.
    pub const DECAY: u32 = 3;
    /// Envelope sustain level, `0..=1`.
    pub const SUSTAIN: u32 = 4;
    /// Envelope release time, milliseconds.
    pub const RELEASE: u32 = 5;
    /// Filter cutoff, hertz.
    pub const CUTOFF: u32 = 6;
    /// Filter resonance, Q.
    pub const RESONANCE: u32 = 7;
    /// Output level, decibels.
    pub const LEVEL: u32 = 8;
}

/// The state schema version [`Synth::save_state`] writes.
const STATE_VERSION: u32 = 1;

/// The oscillator shapes.
///
/// The variant order is part of the saved format: an [`EnumParam`] stores the *index*, so
/// reordering these rewrites what old sessions recall. Append at the end instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Waveform {
    /// A pure sine — no harmonics for the filter to work on, but the reference.
    Sine,
    /// A falling sawtooth: every harmonic, the classic subtractive starting point.
    #[default]
    Saw,
    /// A square wave: odd harmonics only, hollow.
    Square,
    /// A triangle: odd harmonics, falling off fast, soft.
    Triangle,
}

impl ParamEnum for Waveform {
    const VARIANTS: &'static [Self] = &[Self::Sine, Self::Saw, Self::Square, Self::Triangle];

    fn name(self) -> &'static str {
        match self {
            Self::Sine => "Sine",
            Self::Saw => "Saw",
            Self::Square => "Square",
            Self::Triangle => "Triangle",
        }
    }

    fn index(self) -> u32 {
        self as u32
    }

    fn from_index(i: u32) -> Option<Self> {
        Self::VARIANTS.get(i as usize).copied()
    }
}

impl Waveform {
    /// `[audio-thread]` One sample of this shape at `phase`, which is `0.0..1.0`.
    ///
    /// Deliberately naive: these are not band-limited and they alias above a few kHz. A
    /// shipping synth replaces this one function with PolyBLEP or a wavetable and changes
    /// nothing else.
    fn sample(self, phase: f32) -> f32 {
        match self {
            Self::Sine => (phase * core::f32::consts::TAU).sin(),
            Self::Saw => 1.0 - 2.0 * phase,
            Self::Square => {
                if phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            Self::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
        }
    }
}

/// Everything the host can turn.
#[derive(DauxParams)]
pub struct SynthParams {
    /// Oscillator shape.
    #[param(id = param_id::WAVEFORM, name = "Waveform", default = Waveform::Saw)]
    pub waveform: EnumParam<Waveform>,

    /// Time from note-on to full level.
    #[param(id = param_id::ATTACK, name = "Attack", range = 0.5..=5_000.0, default = 5.0,
            unit = "ms", curve = "log", decimals = 1, group = "Envelope")]
    pub attack: FloatParam,

    /// Time from full level down to the sustain level.
    #[param(id = param_id::DECAY, name = "Decay", range = 0.5..=5_000.0, default = 250.0,
            unit = "ms", curve = "log", decimals = 1, group = "Envelope")]
    pub decay: FloatParam,

    /// The level a held note settles at.
    #[param(id = param_id::SUSTAIN, name = "Sustain", range = 0.0..=1.0, default = 0.7,
            decimals = 2, group = "Envelope")]
    pub sustain: FloatParam,

    /// Time from note-off to silence.
    #[param(id = param_id::RELEASE, name = "Release", range = 1.0..=10_000.0, default = 300.0,
            unit = "ms", curve = "log", decimals = 1, group = "Envelope")]
    pub release: FloatParam,

    /// Low-pass corner frequency.
    #[param(id = param_id::CUTOFF, name = "Cutoff", range = 20.0..=20_000.0, default = 4_000.0,
            unit = "Hz", curve = "log", decimals = 1, group = "Filter")]
    pub cutoff: FloatParam,

    /// Filter Q. Above about 4 the corner starts to sing.
    #[param(id = param_id::RESONANCE, name = "Resonance", range = 0.5..=16.0, default = 0.9,
            curve = "log", decimals = 2, group = "Filter")]
    pub resonance: FloatParam,

    /// Output level.
    #[param(id = param_id::LEVEL, name = "Level", range = -60.0..=6.0, default = -6.0,
            unit = "dB", decimals = 1, smoothing = "exponential(10.0)", group = "Output")]
    pub level: FloatParam,
}

/// Which segment of its envelope a voice is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Stage {
    /// Silent and available.
    #[default]
    Idle,
    /// Rising to full level.
    Attack,
    /// Falling to the sustain level.
    Decay,
    /// Holding while the key is down.
    Sustain,
    /// Falling to silence after note-off.
    Release,
}

/// A linear-segment ADSR.
///
/// Linear rather than exponential because it is three lines of arithmetic and its behaviour
/// is obvious from a test: after `n` samples of a 1-second attack at 48 kHz the level is
/// exactly `n / 48000`. A shipping synth usually wants exponential decay and release.
#[derive(Clone, Copy, Debug, Default)]
struct Adsr {
    stage: Stage,
    level: f32,
    /// Level gained per sample while rising.
    attack_step: f32,
    /// Level lost per sample while decaying.
    decay_step: f32,
    /// Level held while the key is down.
    sustain: f32,
    /// Level lost per sample while releasing.
    release_step: f32,
}

impl Adsr {
    /// `[audio-thread]` Adopts new times without disturbing where the envelope already is.
    fn set_shape(&mut self, shape: &EnvelopeShape) {
        self.attack_step = shape.attack_step;
        self.decay_step = shape.decay_step;
        self.sustain = shape.sustain;
        self.release_step = shape.release_step;
    }

    /// `[audio-thread]` Restarts from silence.
    fn start(&mut self, shape: &EnvelopeShape) {
        self.set_shape(shape);
        self.level = 0.0;
        self.stage = Stage::Attack;
    }

    /// `[audio-thread]` The key came up. Idempotent: releasing twice is not louder.
    fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    /// `[audio-thread]` Cuts to silence immediately, for a choke.
    fn choke(&mut self) {
        self.level = 0.0;
        self.stage = Stage::Idle;
    }

    /// `[audio-thread]` `true` once the envelope has finished and the voice can be recycled.
    const fn is_finished(&self) -> bool {
        matches!(self.stage, Stage::Idle)
    }

    /// `[audio-thread]` Advances one sample and returns the new level.
    fn next(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => self.level = 0.0,
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level -= self.decay_step;
                if self.level <= self.sustain {
                    self.level = self.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = self.sustain,
            Stage::Release => {
                self.level -= self.release_step;
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }
}

/// One sounding note.
///
/// Identified the way the host identifies it: by `note_id` when the host assigns one, and by
/// `(channel, key)` when it does not. Getting this wrong is how a note-off silences the wrong
/// voice when the same key is played twice on different channels.
#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    /// `false` when this slot is free.
    active: bool,
    /// The host's voice id, or `-1` when it does not track voices.
    note_id: i32,
    /// MIDI channel `0..=15`.
    channel: i16,
    /// Key number `0..=127`.
    key: i16,
    /// Value of the synth's monotonic voice counter when this note started; the oldest voice
    /// is the one with the smallest value, which is what makes stealing deterministic.
    started_at: u64,
    /// Oscillator phase, `0.0..1.0`.
    phase: f32,
    /// Phase advanced per sample.
    phase_increment: f32,
    /// Note velocity, `0.0..=1.0`.
    velocity: f32,
    /// Per-note pressure from note expression, `0.0..=1.0`. Starts at `1.0`, so a host that
    /// never sends expression gets the velocity unchanged.
    pressure: f32,
    /// Per-note detune in cents, from the note event and from tuning expression.
    tuning_cents: f64,
    /// Per-note brightness from note expression, `0.0..=1.0`, `0.5` being neutral. Scales the
    /// voice's filter cutoff over four octaves.
    brightness: f64,
    /// The amplitude envelope.
    env: Adsr,
    /// The per-voice low-pass.
    filter: Biquad,
}

impl Voice {
    /// `[audio-thread]` `true` when this voice answers to the note the event names.
    ///
    /// `-1` is the wildcard the event model uses for "any", on all three fields, and each
    /// field narrows the match independently. Two consequences are worth stating, because
    /// getting either wrong silences the wrong note:
    ///
    /// * a host that assigns voice ids can address one of two voices that share a channel and
    ///   a key, because `note_id` alone distinguishes them;
    /// * a MIDI 1.0 note-off, which has no voice id, still releases every voice on that
    ///   channel and key — including voices the host *did* give an id to.
    fn matches(&self, note_id: i32, channel: i16, key: i16) -> bool {
        self.active
            && (note_id < 0 || note_id == self.note_id)
            && (channel < 0 || channel == self.channel)
            && (key < 0 || key == self.key)
    }

    /// `[audio-thread]` Starts this voice on a note.
    fn start(&mut self, note: &NoteEvent, started_at: u64, settings: &BlockSettings) {
        self.active = true;
        self.note_id = note.note_id;
        self.channel = note.channel;
        self.key = note.key;
        self.started_at = started_at;
        self.phase = 0.0;
        self.velocity = (note.velocity as f32).clamp(0.0, 1.0);
        self.pressure = 1.0;
        self.tuning_cents = if note.tuning.is_finite() {
            note.tuning
        } else {
            0.0
        };
        self.brightness = 0.5;
        self.env.start(&settings.envelope);
        self.filter.reset();
        self.retune(settings.sample_rate);
        self.update_filter(settings);
    }

    /// `[audio-thread]` Recomputes the phase increment from key, tuning and sample rate.
    fn retune(&mut self, sample_rate: f64) {
        let semitones = f64::from(self.key) - A4_KEY + self.tuning_cents / 100.0;
        let hz = A4_HZ * (semitones / 12.0).exp2();
        // Nothing above Nyquist: a phase increment past 0.5 folds down as a wrong pitch.
        let hz = hz.clamp(0.0, sample_rate * 0.49);
        self.phase_increment = (hz / sample_rate) as f32;
    }

    /// `[audio-thread]` Recomputes this voice's filter from the block settings and its own
    /// brightness expression.
    fn update_filter(&mut self, settings: &BlockSettings) {
        // Brightness is `0..1` with `0.5` neutral, mapped over ±2 octaves.
        let octaves = (self.brightness - 0.5) * 4.0;
        let cutoff = (settings.cutoff_hz * octaves.exp2()).clamp(20.0, settings.sample_rate * 0.45);
        self.filter = Biquad::lowpass(settings.sample_rate, cutoff, settings.resonance);
    }

    /// `[audio-thread]` Applies one note-expression event.
    fn apply_expression(&mut self, event: &NoteExpressionEvent, settings: &BlockSettings) {
        if !event.value.is_finite() {
            return;
        }
        match event.expression {
            NoteExpression::Pressure => self.pressure = (event.value as f32).clamp(0.0, 1.0),
            NoteExpression::Volume => self.velocity = (event.value as f32).clamp(0.0, 1.0),
            NoteExpression::Tuning => {
                self.tuning_cents = event.value.clamp(-4_800.0, 4_800.0);
                self.retune(settings.sample_rate);
            }
            NoteExpression::Brightness => {
                self.brightness = event.value.clamp(0.0, 1.0);
                self.update_filter(settings);
            }
            // Pan, vibrato and expression are meaningful, but this synth is mono per voice
            // and has no modulation, so it would be dishonest to pretend to act on them.
            NoteExpression::Pan | NoteExpression::Vibrato | NoteExpression::Expression => {}
        }
    }

    /// `[audio-thread]` Adds this voice's output to `mix`, one sample per element.
    ///
    /// Returns the sample index within `mix` at which the envelope finished, or `None` when
    /// the voice is still sounding at the end.
    fn render_into(&mut self, mix: &mut [f32], waveform: Waveform) -> Option<usize> {
        for (index, slot) in mix.iter_mut().enumerate() {
            let raw = waveform.sample(self.phase);
            self.phase += self.phase_increment;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }

            let envelope = self.env.next();
            let filtered = self.filter.process(raw);
            // A voice in a long release decays towards a denormal, and denormals cost a
            // hundred times a normal multiply on some CPUs — which is a real-time bug that
            // only shows up as crackling when a chord fades out.
            *slot += flush_denormal(filtered * envelope * self.velocity * self.pressure);

            if self.env.is_finished() {
                self.active = false;
                return Some(index);
            }
        }
        None
    }
}

/// Everything derived once per segment from the parameters.
///
/// Recomputed whenever a parameter event arrives, so automation is applied at its own sample
/// offset like everything else. Deriving it costs a handful of divisions, not an allocation.
#[derive(Clone, Copy, Debug)]
struct BlockSettings {
    sample_rate: f64,
    waveform: Waveform,
    envelope: EnvelopeShape,
    cutoff_hz: f64,
    resonance: f64,
    level: f32,
}

/// The per-sample envelope steps, in the form the voices consume.
#[derive(Clone, Copy, Debug, Default)]
struct EnvelopeShape {
    attack_step: f32,
    decay_step: f32,
    sustain: f32,
    release_step: f32,
}

/// Converts a time in milliseconds into the level change per sample of a full-scale segment.
///
/// Clamped to at least one sample, so a zero-length attack is instantaneous rather than a
/// division by zero.
fn step_per_sample(milliseconds: f64, sample_rate: f64) -> f32 {
    let samples = (milliseconds / 1_000.0 * sample_rate).max(1.0);
    (1.0 / samples) as f32
}

/// A sixteen-voice subtractive synthesizer.
#[derive(DauxPlugin)]
#[plugin(
    id = "studio.futureboard.daux.example.synth",
    name = "DAUx Synth",
    vendor = "Futureboard Studio",
    version = "1.0.0",
    description = "Sixteen-voice subtractive synthesizer with sample-accurate MIDI.",
    license = "MIT OR Apache-2.0",
    category = "instrument",
    capabilities(
        instrument,
        midi_input,
        midi_output,
        note_expression,
        sample_accurate_auto,
        offline_render,
        sandbox_safe
    ),
    features("synthesizer", "subtractive", "polyphonic"),
    state_schema_version = STATE_VERSION
)]
pub struct Synth {
    /// The parameter bank.
    params: SynthParams,
    /// The fixed voice pool.
    voices: [Voice; VOICE_COUNT],
    /// Monotonic counter used to date voices for stealing.
    next_voice_stamp: u64,
    /// One accumulator per sample of the largest block the host promised. Allocated in
    /// `prepare`; the audio thread only ever writes into it.
    mix: Vec<f32>,
    /// One output-level coefficient per sample, same story.
    level_ramp: Vec<f32>,
    /// Ramps the output level so an automated fade does not step.
    level: Smoother,
    /// The rate `prepare` was called with, so `tail` can answer in samples.
    sample_rate: f64,
    /// Note-end events the host's output buffer had no room for.
    ///
    /// A full output is a *normal* condition, not a bug: the buffer is preallocated and the
    /// audio thread may not grow it. Counting is all a real-time thread may do about it; the
    /// number is for a developer reading it in a debugger, never for a log line in `process`.
    dropped_note_ends: u64,
}

impl Default for Synth {
    /// `[main-thread]` A silent, unprepared instance.
    fn default() -> Self {
        let params = SynthParams::new();
        let level = params.level.smoother();
        Self {
            params,
            voices: [Voice::default(); VOICE_COUNT],
            next_voice_stamp: 0,
            // Empty until `prepare`: only the host knows how large a block gets.
            mix: Vec::new(),
            level_ramp: Vec::new(),
            level,
            sample_rate: 48_000.0,
            dropped_note_ends: 0,
        }
    }
}

impl Synth {
    /// `[audio-thread]` Derives the per-segment settings from the current parameter values.
    fn settings(&self, sample_rate: f64) -> BlockSettings {
        BlockSettings {
            sample_rate,
            waveform: self.params.waveform.value(),
            envelope: EnvelopeShape {
                attack_step: step_per_sample(self.params.attack.value(), sample_rate),
                decay_step: step_per_sample(self.params.decay.value(), sample_rate),
                sustain: self.params.sustain.value() as f32,
                release_step: step_per_sample(self.params.release.value(), sample_rate),
            },
            cutoff_hz: self.params.cutoff.value(),
            resonance: self.params.resonance.value(),
            level: db_to_gain(self.params.level.value_f32()),
        }
    }

    /// `[audio-thread]` Picks the voice a new note should use.
    ///
    /// A free slot when there is one; otherwise the oldest *released* voice, which is the
    /// least likely to be missed; otherwise the oldest voice of all. Returning `None` and
    /// dropping the note would be the one unacceptable answer — a player who holds the
    /// sustain pedal would simply stop hearing new notes.
    fn allocate_voice(&mut self) -> usize {
        let mut free: Option<usize> = None;
        let mut oldest_released: Option<(u64, usize)> = None;
        let mut oldest: Option<(u64, usize)> = None;

        for (index, voice) in self.voices.iter().enumerate() {
            if !voice.active {
                free = Some(index);
                break;
            }
            let stamp = voice.started_at;
            if voice.env.stage == Stage::Release
                && oldest_released.is_none_or(|(best, _)| stamp < best)
            {
                oldest_released = Some((stamp, index));
            }
            if oldest.is_none_or(|(best, _)| stamp < best) {
                oldest = Some((stamp, index));
            }
        }

        free.or(oldest_released.map(|(_, i)| i))
            .or(oldest.map(|(_, i)| i))
            // The pool is a non-empty array, so `oldest` is always `Some` once it is full.
            .unwrap_or(0)
    }

    /// `[audio-thread]` Starts a note.
    fn note_on(&mut self, note: &NoteEvent, settings: &BlockSettings) {
        // Velocity zero is the MIDI 1.0 spelling of a note-off, and hosts pass it through.
        if note.velocity <= 0.0 {
            self.note_off(note);
            return;
        }
        let index = self.allocate_voice();
        let stamp = self.next_voice_stamp;
        self.next_voice_stamp = self.next_voice_stamp.wrapping_add(1);
        self.voices[index].start(note, stamp, settings);
    }

    /// `[audio-thread]` Releases every voice the event names.
    fn note_off(&mut self, note: &NoteEvent) {
        for voice in &mut self.voices {
            if voice.matches(note.note_id, note.channel, note.key) {
                voice.env.release();
            }
        }
    }

    /// `[audio-thread]` Cuts every voice the event names without a release.
    fn note_choke(&mut self, note: &NoteEvent) {
        for voice in &mut self.voices {
            if voice.matches(note.note_id, note.channel, note.key) {
                voice.env.choke();
                voice.active = false;
            }
        }
    }

    /// `[audio-thread]` `true` while any voice is still producing sound.
    fn is_sounding(&self) -> bool {
        self.voices.iter().any(|v| v.active)
    }

    /// `[audio-thread]` Applies one input event.
    ///
    /// `settings` is `&mut` because a parameter event changes it from this sample onwards —
    /// that is what makes automation sample-accurate rather than block-accurate.
    fn apply_event(&mut self, event: &DauxEvent<'_>, settings: &mut BlockSettings) {
        match event {
            DauxEvent::NoteOn(note) => self.note_on(note, settings),
            DauxEvent::NoteOff(note) => self.note_off(note),
            DauxEvent::NoteChoke(note) => self.note_choke(note),
            DauxEvent::NoteExpression(expression) => {
                for voice in &mut self.voices {
                    if voice.matches(expression.note_id, expression.channel, expression.key) {
                        voice.apply_expression(expression, settings);
                    }
                }
            }
            DauxEvent::ParamValue(change) => self.apply_param(change, settings),
            // The same notes may arrive as raw MIDI 1.0 from a host that does not translate.
            DauxEvent::Midi1(midi) => self.apply_midi1(midi, settings),
            // NoteEnd is plug-in to host, and the rest do not concern a synthesizer.
            _ => {}
        }
    }

    /// `[audio-thread]` Applies a parameter change and re-derives everything that depends on
    /// it, including every live voice's filter.
    fn apply_param(&mut self, change: &ParamEvent, settings: &mut BlockSettings) {
        if !change.value.is_finite() {
            return;
        }
        // The derive lowers this lookup to a `match` on the raw id: no allocation, no map.
        let Some(param) = self.params.param(ParamId::new(change.param_id)) else {
            return;
        };
        param.set_plain(change.value);

        *settings = self.settings(settings.sample_rate);
        match change.param_id {
            param_id::CUTOFF | param_id::RESONANCE => {
                for voice in &mut self.voices {
                    if voice.active {
                        voice.update_filter(settings);
                    }
                }
            }
            param_id::ATTACK | param_id::DECAY | param_id::SUSTAIN | param_id::RELEASE => {
                for voice in &mut self.voices {
                    voice.env.set_shape(&settings.envelope);
                }
            }
            _ => {}
        }
    }

    /// `[audio-thread]` Turns a MIDI 1.0 note message into the same voice action a DAUx note
    /// event would produce.
    fn apply_midi1(&mut self, midi: &Midi1Event, settings: &BlockSettings) {
        let message = midi.message;
        let note = NoteEvent {
            header: midi.header,
            // MIDI 1.0 has no voice ids, so `(channel, key)` identifies the voice.
            note_id: -1,
            channel: i16::from(message.channel()),
            key: i16::from(message.data1()),
            velocity: f64::from(message.data2()) / 127.0,
            tuning: 0.0,
        };
        match message.kind() {
            Midi1Kind::NoteOn => self.note_on(&note, settings),
            Midi1Kind::NoteOff => self.note_off(&note),
            _ => {}
        }
    }

    /// `[audio-thread]` Renders `mix[start..end]` from every active voice.
    ///
    /// A voice that finishes inside the segment tells the host so, at the exact sample it
    /// finished, so the host can recycle the note id.
    fn render(&mut self, start: usize, end: usize, waveform: Waveform, out: &mut dyn OutputEvents) {
        // Destructured so the borrow checker can see that the voices, the mix buffer and the
        // counter are three separate fields rather than one `&mut self`.
        let Self {
            voices,
            mix,
            dropped_note_ends,
            ..
        } = self;
        let segment = &mut mix[start..end];

        for voice in voices.iter_mut().filter(|v| v.active) {
            let note_id = voice.note_id;
            let channel = voice.channel;
            let key = voice.key;
            let Some(offset) = voice.render_into(segment, waveform) else {
                continue;
            };

            let ended = DauxEvent::NoteEnd(NoteEvent {
                header: EventHeader::at((start + offset) as u32),
                note_id,
                channel,
                key,
                velocity: 0.0,
                tuning: 0.0,
            });
            if out.try_push(&ended).is_err() {
                // Normal, not fatal: the host's list is preallocated and we may not grow it.
                // Counting is the whole of what a real-time thread may do here.
                *dropped_note_ends = dropped_note_ends.saturating_add(1);
            }
        }
    }
}

impl DauxProcessor for Synth {
    /// `[main-thread]` Sizes the mix and ramp buffers. The only place this plug-in allocates.
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;

        let max_block = config.max_block_size as usize;
        self.mix.clear();
        self.mix.resize(max_block, 0.0);
        self.level_ramp.clear();
        self.level_ramp.resize(max_block, 1.0);

        self.sample_rate = config.sample_rate;
        self.level.prepare(config.sample_rate);
        self.level
            .reset_to(db_to_gain(self.params.level.value_f32()));
        self.reset();
        Ok(())
    }

    /// `[audio-thread]` Silences every voice. Called when the host relocates the playhead.
    fn reset(&mut self) {
        for voice in &mut self.voices {
            *voice = Voice::default();
        }
        self.level
            .reset_to(db_to_gain(self.params.level.value_f32()));
    }

    /// `[audio-thread]` Renders one block, splitting it at every event offset.
    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        // A conforming host stays within the prepared `max_block_size`; clamping is the only
        // allocation-free answer to one that does not.
        let frames = ctx.frames().min(self.mix.len());
        self.mix[..frames].fill(0.0);

        let mut settings = self.settings(ctx.config().sample_rate);
        let (input, output) = events.split();

        // --- the sample-accurate walk -----------------------------------------------------
        //
        // Render up to the next event, apply it, repeat. `cursor` is the first sample not yet
        // rendered, and `clamp(cursor, frames)` keeps a host that sends unsorted or
        // out-of-range events from producing a backwards or oversized segment.
        let mut cursor = 0usize;
        for index in 0..input.len() {
            let Some(event) = input.get(index) else {
                continue;
            };
            let at = (event.time() as usize).clamp(cursor, frames);
            if at > cursor {
                self.render(cursor, at, settings.waveform, output);
                cursor = at;
            }
            self.apply_event(&event, &mut settings);
        }
        if cursor < frames {
            self.render(cursor, frames, settings.waveform, output);
        }

        // --- output level -----------------------------------------------------------------
        self.level.set_target(settings.level);
        self.level.next_block(&mut self.level_ramp[..frames]);
        for (sample, gain) in self.mix[..frames]
            .iter_mut()
            .zip(&self.level_ramp[..frames])
        {
            *sample *= gain;
        }

        // --- write it to every output channel ---------------------------------------------
        let Some(mut out) = audio.main_output() else {
            // An instrument with nowhere to play. Not an error, just nothing to do.
            return ProcessStatus::Sleep;
        };
        for channel in out.split_channels_mut() {
            let n = channel.len().min(frames);
            channel[..n].copy_from_slice(&self.mix[..n]);
            // Only reachable when the host broke its own `max_block_size` promise; leaving
            // stale samples there would be worse than the silence.
            channel[n..].fill(0.0);
        }

        if self.is_sounding() {
            ProcessStatus::Continue
        } else {
            // Nothing is sounding and nothing will until an event arrives. The host may stop
            // calling us, which is what makes a session with a hundred instruments affordable.
            ProcessStatus::Sleep
        }
    }

    /// `[audio-thread]` How long a released note keeps ringing.
    fn tail(&self) -> Tail {
        let samples = self.params.release.value() / 1_000.0 * self.sample_rate;
        if samples.is_finite() && samples >= 1.0 {
            Tail::Samples(samples.min(f64::from(u32::MAX - 2)) as u32)
        } else {
            Tail::None
        }
    }
}

impl DauxController for Synth {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    /// `[main-thread]` Writes every parameter under its permanent id.
    ///
    /// Keyed by id rather than by name, because the name is display text and is free to
    /// change; the id is not.
    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        w.begin_group("params");
        for (id, param) in self.params.param_refs() {
            // `itoa`-free: the key is built once per save on the main thread.
            w.put_f64(&id.get().to_string(), param.plain());
        }
        w.end_group();
        Ok(())
    }

    /// `[main-thread]` Restores what `save_state` wrote, keeping the default for anything a
    /// older version did not write.
    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        for (id, param) in self.params.param_refs() {
            if let Some(value) = r.opt_f64(&format!("params/{}", id.get())) {
                param.set_plain(value);
            }
        }
        Ok(())
    }
}

impl DauxPlugin for Synth {
    fn descriptor() -> PluginDescriptor {
        Self::descriptor()
    }

    /// `[main-thread]` No audio in, stereo out.
    fn bus_layout(&self) -> BusLayout {
        BusLayout::instrument(ChannelLayout::Stereo)
    }

    /// `[main-thread]` Notes in, and note-ends out so the host can recycle its voice ids.
    fn event_ports(&self) -> EventPortLayout {
        EventPortLayout::instrument().with_output(EventPortInfo::main("Note Out"))
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        self
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        self
    }

    /// `[main-thread]` Mono or stereo out, and no audio input.
    fn accepts_bus_layout(&self, layout: &BusLayout) -> bool {
        layout.inputs.is_empty()
            && layout.outputs.len() == 1
            && matches!(
                layout.main_output().map_or(0, BusInfo::channel_count),
                1 | 2
            )
    }
}

export_plugin!(SingleFactory<Synth>);

/// The allocation tripwire, installed only while this crate's tests are compiled.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_plugin::CountingAllocator = daux_plugin::CountingAllocator;

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin::EventBuffer;
    use daux_plugin::daux_rt::{AllocGuard, counting_allocator_installed};

    const SAMPLE_RATE: f64 = 48_000.0;
    const BLOCK: usize = 512;

    fn config() -> ProcessConfig {
        ProcessConfig::new(SAMPLE_RATE, BLOCK as u32)
    }

    /// A prepared synth with an instantaneous envelope and a wide-open filter, so a test can
    /// measure what it is actually about rather than the envelope's ramp-in.
    fn prepared() -> Synth {
        let mut synth = Synth::default();
        synth.params.attack.set_plain(0.5);
        synth.params.decay.set_plain(0.5);
        synth.params.sustain.set_plain(1.0);
        synth.params.release.set_plain(10.0);
        synth.params.cutoff.set_plain(20_000.0);
        synth.params.level.set_plain(0.0);
        synth.prepare(&config()).expect("a valid config");
        synth
    }

    fn note_on(time: u32, key: i16, velocity: f64) -> DauxEvent<'static> {
        DauxEvent::NoteOn(NoteEvent {
            header: EventHeader::at(time),
            note_id: -1,
            channel: 0,
            key,
            velocity,
            tuning: 0.0,
        })
    }

    fn note_off(time: u32, key: i16) -> DauxEvent<'static> {
        DauxEvent::NoteOff(NoteEvent {
            header: EventHeader::at(time),
            note_id: -1,
            channel: 0,
            key,
            velocity: 0.0,
            tuning: 0.0,
        })
    }

    /// Runs one block and returns the left channel plus whatever the synth sent the host.
    fn run(synth: &mut Synth, frames: usize, events: &EventBuffer) -> (Vec<f32>, EventBuffer) {
        let mut output = AudioStorage::<f32>::new(2, frames);
        let mut sink = EventBuffer::with_capacity(64, 1_024);
        let config = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(frames, &config, &host);
        {
            let mut outputs = [output.as_mut()];
            let mut buses = AudioBuses::new(&[], &mut outputs, frames);
            let mut ports = ProcessEvents::new(events, &mut sink);
            let status = synth.process(&ctx, &mut buses, &mut ports);
            assert_ne!(status, ProcessStatus::Error);
        }
        let left = output.channel(0).expect("channel 0").to_vec();
        (left, sink)
    }

    fn buffer(events: &[DauxEvent<'_>]) -> EventBuffer {
        let mut buf = EventBuffer::with_capacity(64, 1_024);
        for event in events {
            buf.try_push(event).expect("the buffer has room");
        }
        buf
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Counts rising zero crossings, which is a frequency measurement that does not need an
    /// FFT and is exact enough to tell an octave from a fifth.
    fn rising_crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count()
    }

    #[test]
    fn a_note_on_produces_sound() {
        let mut synth = prepared();
        let (left, _) = run(&mut synth, BLOCK, &buffer(&[note_on(0, 60, 1.0)]));
        assert!(
            peak(&left) > 0.01,
            "a note-on produced a peak of {}",
            peak(&left)
        );
    }

    #[test]
    fn silence_without_a_note() {
        let mut synth = prepared();
        let (left, _) = run(&mut synth, BLOCK, &buffer(&[]));
        assert_eq!(peak(&left), 0.0);
    }

    #[test]
    fn a_note_on_is_applied_at_its_own_sample_offset() {
        // The whole reason this example exists. A synth that applies events at the top of the
        // block sounds the note 128 samples early and passes nothing below.
        const OFFSET: usize = 128;
        let mut synth = prepared();
        let (left, _) = run(
            &mut synth,
            BLOCK,
            &buffer(&[note_on(OFFSET as u32, 60, 1.0)]),
        );

        assert_eq!(
            peak(&left[..OFFSET]),
            0.0,
            "the synth sounded before the note-on"
        );
        assert!(
            peak(&left[OFFSET..]) > 0.01,
            "the synth did not sound after the note-on"
        );
    }

    #[test]
    fn two_notes_in_one_block_start_at_their_own_offsets() {
        let mut synth = prepared();
        let (left, _) = run(
            &mut synth,
            BLOCK,
            &buffer(&[note_on(64, 60, 1.0), note_on(320, 72, 1.0)]),
        );

        assert_eq!(peak(&left[..64]), 0.0, "sound before the first note");
        let one_voice = rms(&left[100..300]);
        let two_voices = rms(&left[352..BLOCK]);
        assert!(one_voice > 0.0);
        assert!(
            two_voices > one_voice,
            "the second voice did not join: {one_voice} then {two_voices}"
        );
    }

    #[test]
    fn a_higher_key_sounds_a_higher_pitch() {
        // An octave up is twice the frequency, so twice the zero crossings in one block.
        let count = |key: i16| {
            let mut synth = prepared();
            synth.params.waveform.set(Waveform::Sine);
            let (left, _) = run(&mut synth, BLOCK, &buffer(&[note_on(0, key, 1.0)]));
            rising_crossings(&left)
        };
        let low = count(60);
        let high = count(72);
        assert!(low > 0, "key 60 produced no cycles");
        assert!(
            high >= low * 2 - 1 && high <= low * 2 + 1,
            "key 72 should be an octave above key 60: {low} then {high}"
        );
    }

    #[test]
    fn note_off_releases_the_voice_and_the_synth_eventually_sleeps() {
        let mut synth = prepared();
        synth.params.release.set_plain(5.0);

        let (_, _) = run(&mut synth, BLOCK, &buffer(&[note_on(0, 60, 1.0)]));
        assert!(synth.is_sounding());

        let (_, ended) = run(&mut synth, BLOCK, &buffer(&[note_off(0, 60)]));
        // 5 ms at 48 kHz is 240 samples, well inside one block, so the voice ends here and
        // the host is told at the exact sample it did.
        assert_eq!(ended.len(), 1, "the host was not told the voice ended");
        let Some(DauxEvent::NoteEnd(end)) = ended.get(0) else {
            panic!("the emitted event is not a NoteEnd");
        };
        assert_eq!(end.key, 60);
        assert!(end.header.time > 0 && (end.header.time as usize) < BLOCK);

        assert!(!synth.is_sounding());
        let mut output = AudioStorage::<f32>::new(2, BLOCK);
        let config = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(BLOCK, &config, &host);
        let empty = buffer(&[]);
        let mut sink = EventBuffer::with_capacity(8, 128);
        let mut outputs = [output.as_mut()];
        let mut buses = AudioBuses::new(&[], &mut outputs, BLOCK);
        let mut ports = ProcessEvents::new(&empty, &mut sink);
        assert_eq!(
            synth.process(&ctx, &mut buses, &mut ports),
            ProcessStatus::Sleep,
            "an idle synth must let the host stop calling it"
        );
    }

    #[test]
    fn a_note_off_only_releases_the_note_it_names() {
        let mut synth = prepared();
        run(
            &mut synth,
            BLOCK,
            &buffer(&[note_on(0, 60, 1.0), note_on(0, 64, 1.0)]),
        );
        run(&mut synth, BLOCK, &buffer(&[note_off(0, 60)]));

        let released: Vec<i16> = synth
            .voices
            .iter()
            .filter(|v| v.active && v.env.stage == Stage::Release)
            .map(|v| v.key)
            .collect();
        let held: Vec<i16> = synth
            .voices
            .iter()
            .filter(|v| v.active && v.env.stage != Stage::Release)
            .map(|v| v.key)
            .collect();
        assert!(released.is_empty() || released == [60]);
        assert_eq!(held, [64], "the wrong voice was released");
    }

    #[test]
    fn a_host_that_assigns_note_ids_is_matched_by_id_not_by_key() {
        // Two voices on the same channel and key, which is exactly the case `(channel, key)`
        // matching gets wrong.
        let mut synth = prepared();
        let mut events = EventBuffer::with_capacity(8, 128);
        for note_id in [7, 9] {
            events
                .try_push(&DauxEvent::NoteOn(NoteEvent {
                    header: EventHeader::at(0),
                    note_id,
                    channel: 0,
                    key: 60,
                    velocity: 1.0,
                    tuning: 0.0,
                }))
                .expect("room");
        }
        run(&mut synth, 64, &events);
        assert_eq!(synth.voices.iter().filter(|v| v.active).count(), 2);

        let off = buffer(&[DauxEvent::NoteOff(NoteEvent {
            header: EventHeader::at(0),
            note_id: 7,
            channel: 0,
            key: 60,
            velocity: 0.0,
            tuning: 0.0,
        })]);
        run(&mut synth, 64, &off);

        let releasing = synth
            .voices
            .iter()
            .filter(|v| v.active && v.env.stage == Stage::Release)
            .count();
        assert_eq!(releasing, 1, "note id 9 must be untouched");
    }

    #[test]
    fn a_note_off_without_a_voice_id_still_releases_a_voice_that_has_one() {
        // A MIDI 1.0 note-off carries no voice id. Matching *only* by id would leave a note
        // the host started with an id sounding forever, which is the classic hung note.
        let mut synth = prepared();
        let on = buffer(&[DauxEvent::NoteOn(NoteEvent {
            header: EventHeader::at(0),
            note_id: 42,
            channel: 0,
            key: 60,
            velocity: 1.0,
            tuning: 0.0,
        })]);
        run(&mut synth, 64, &on);
        assert!(synth.is_sounding());

        // `note_id: -1` is the wildcard: every voice on channel 0, key 60.
        run(&mut synth, 64, &buffer(&[note_off(0, 60)]));
        assert!(
            synth
                .voices
                .iter()
                .any(|v| v.active && v.env.stage == Stage::Release),
            "a wildcard note-off must reach a voice that carries an id"
        );
    }

    #[test]
    fn a_choke_silences_the_voice_without_a_release() {
        let mut synth = prepared();
        run(&mut synth, 64, &buffer(&[note_on(0, 60, 1.0)]));
        assert!(synth.is_sounding());

        let choke = buffer(&[DauxEvent::NoteChoke(NoteEvent {
            header: EventHeader::at(0),
            note_id: -1,
            channel: 0,
            key: 60,
            velocity: 0.0,
            tuning: 0.0,
        })]);
        let (left, ended) = run(&mut synth, 64, &choke);
        assert!(!synth.is_sounding(), "a choke must cut the voice");
        assert_eq!(peak(&left), 0.0, "a choke must be immediate");
        assert_eq!(
            ended.len(),
            0,
            "the host chose to end this voice; telling it again is noise"
        );
    }

    #[test]
    fn more_notes_than_voices_steal_rather_than_drop() {
        let mut synth = prepared();
        let mut events = EventBuffer::with_capacity(64, 1_024);
        for key in 0..(VOICE_COUNT as i16 + 8) {
            events
                .try_push(&note_on(0, 40 + key, 1.0))
                .expect("the buffer has room");
        }
        let (left, _) = run(&mut synth, BLOCK, &events);

        assert_eq!(
            synth.voices.iter().filter(|v| v.active).count(),
            VOICE_COUNT,
            "the pool must be full, and no larger"
        );
        assert!(peak(&left) > 0.01, "stealing must not silence the synth");

        // The oldest notes are the ones that went: the eight most recent keys survive.
        let mut keys: Vec<i16> = synth.voices.iter().map(|v| v.key).collect();
        keys.sort_unstable();
        assert!(
            keys.iter().all(|&k| k >= 40 + 8),
            "the newest notes should have survived, got {keys:?}"
        );
    }

    #[test]
    fn a_lower_cutoff_removes_energy_from_a_saw() {
        let measure = |cutoff: f64| {
            let mut synth = prepared();
            synth.params.cutoff.set_plain(cutoff);
            synth.params.waveform.set(Waveform::Saw);
            let (left, _) = run(&mut synth, BLOCK, &buffer(&[note_on(0, 72, 1.0)]));
            rms(&left[128..])
        };
        let open = measure(20_000.0);
        let closed = measure(120.0);
        assert!(open > 0.0, "the open filter produced nothing");
        assert!(
            closed < open * 0.5,
            "a 120 Hz low-pass on a 523 Hz saw should remove most of it: {open} then {closed}"
        );
    }

    #[test]
    fn pressure_expression_scales_the_voice() {
        let mut synth = prepared();
        let (loud, _) = run(&mut synth, 256, &buffer(&[note_on(0, 60, 1.0)]));

        let quiet_events = buffer(&[DauxEvent::NoteExpression(NoteExpressionEvent {
            header: EventHeader::at(0),
            expression: NoteExpression::Pressure,
            note_id: -1,
            channel: 0,
            key: 60,
            value: 0.1,
        })]);
        let (quiet, _) = run(&mut synth, 256, &quiet_events);

        assert!(
            rms(&quiet) < rms(&loud) * 0.5,
            "pressure 0.1 should be much quieter: {} then {}",
            rms(&loud),
            rms(&quiet)
        );
    }

    #[test]
    fn tuning_expression_changes_the_pitch() {
        let mut synth = prepared();
        synth.params.waveform.set(Waveform::Sine);
        let (base, _) = run(&mut synth, BLOCK, &buffer(&[note_on(0, 60, 1.0)]));

        let mut up = prepared();
        up.params.waveform.set(Waveform::Sine);
        let events = buffer(&[
            note_on(0, 60, 1.0),
            DauxEvent::NoteExpression(NoteExpressionEvent {
                header: EventHeader::at(1),
                expression: NoteExpression::Tuning,
                note_id: -1,
                channel: 0,
                key: 60,
                // 1200 cents is one octave.
                value: 1_200.0,
            }),
        ]);
        let (raised, _) = run(&mut up, BLOCK, &events);

        let low = rising_crossings(&base);
        let high = rising_crossings(&raised);
        assert!(low > 0);
        assert!(
            high >= low * 2 - 1,
            "+1200 cents should double the frequency: {low} then {high}"
        );
    }

    #[test]
    fn a_midi_1_note_sounds_like_a_daux_note() {
        let mut synth = prepared();
        let events = buffer(&[DauxEvent::Midi1(Midi1Event {
            header: EventHeader::at(0),
            message: Midi1Message::note_on(0, 60, 100),
        })]);
        let (left, _) = run(&mut synth, 256, &events);
        assert!(peak(&left) > 0.01, "a MIDI 1.0 note-on produced nothing");

        let off = buffer(&[DauxEvent::Midi1(Midi1Event {
            header: EventHeader::at(0),
            message: Midi1Message::note_off(0, 60, 0),
        })]);
        run(&mut synth, 8, &off);
        assert!(
            synth
                .voices
                .iter()
                .any(|v| v.active && v.env.stage == Stage::Release),
            "the MIDI 1.0 note-off did not release the voice"
        );
    }

    #[test]
    fn a_midi_note_on_with_zero_velocity_is_a_note_off() {
        let mut synth = prepared();
        run(&mut synth, 64, &buffer(&[note_on(0, 60, 1.0)]));
        let events = buffer(&[DauxEvent::Midi1(Midi1Event {
            header: EventHeader::at(0),
            message: Midi1Message::note_on(0, 60, 0),
        })]);
        run(&mut synth, 8, &events);
        assert!(
            synth
                .voices
                .iter()
                .any(|v| v.active && v.env.stage == Stage::Release),
            "velocity 0 is the MIDI 1.0 spelling of a note-off"
        );
    }

    #[test]
    fn parameter_automation_is_applied_at_its_own_offset() {
        // The level parameter is smoothed, so the observable difference is that the second
        // half of the block is quieter than the first — not that the whole block is.
        let mut synth = prepared();
        let events = buffer(&[
            note_on(0, 60, 1.0),
            DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(256),
                param_id: param_id::LEVEL,
                value: -60.0,
                ..ParamEvent::default()
            }),
        ]);
        let (left, _) = run(&mut synth, BLOCK, &events);

        let before = rms(&left[64..256]);
        let after = rms(&left[400..BLOCK]);
        assert!(before > 0.0);
        assert!(
            after < before * 0.25,
            "the level drop was not applied inside the block: {before} then {after}"
        );
        assert_eq!(synth.params.level.value(), -60.0);
    }

    #[test]
    fn an_event_past_the_end_of_the_block_does_not_panic() {
        let mut synth = prepared();
        let (left, _) = run(&mut synth, 64, &buffer(&[note_on(99_999, 60, 1.0)]));
        assert_eq!(left.len(), 64);
    }

    #[test]
    fn unsorted_events_do_not_produce_a_backwards_segment() {
        // Hosts promise sorted events (abi-v1 §9). A plug-in that trusts the promise with a
        // subtraction panics in release-mode debug assertions and slices backwards in debug.
        let mut synth = prepared();
        let (left, _) = run(
            &mut synth,
            BLOCK,
            &buffer(&[note_on(400, 60, 1.0), note_on(10, 64, 1.0)]),
        );
        assert_eq!(left.len(), BLOCK);
        assert_eq!(synth.voices.iter().filter(|v| v.active).count(), 2);
    }

    #[test]
    fn a_full_note_end_output_is_counted_rather_than_fatal() {
        let mut synth = prepared();
        synth.params.release.set_plain(1.0);

        // Sixteen voices about to finish, and an output with room for one event.
        let mut on = EventBuffer::with_capacity(32, 512);
        for key in 0..VOICE_COUNT as i16 {
            on.try_push(&note_on(0, 40 + key, 1.0)).expect("room");
        }
        run(&mut synth, 64, &on);

        let mut off = EventBuffer::with_capacity(32, 512);
        for key in 0..VOICE_COUNT as i16 {
            off.try_push(&note_off(0, 40 + key)).expect("room");
        }

        let mut output = AudioStorage::<f32>::new(2, BLOCK);
        let mut sink = EventBuffer::with_capacity(1, 64);
        let config = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(BLOCK, &config, &host);
        {
            let mut outputs = [output.as_mut()];
            let mut buses = AudioBuses::new(&[], &mut outputs, BLOCK);
            let mut ports = ProcessEvents::new(&off, &mut sink);
            synth.process(&ctx, &mut buses, &mut ports);
        }
        assert_eq!(sink.len(), 1, "the sink only had room for one");
        assert!(
            synth.dropped_note_ends >= (VOICE_COUNT - 1) as u64,
            "the overflow must be counted, not ignored: {}",
            synth.dropped_note_ends
        );
        assert!(!synth.is_sounding());
    }

    #[test]
    fn process_never_allocates() {
        assert!(
            counting_allocator_installed(),
            "the tripwire is not installed, so this test would pass vacuously"
        );

        let mut synth = prepared();
        // A busy block: notes, a release, expression and automation, all at different offsets.
        let events = buffer(&[
            note_on(0, 60, 1.0),
            note_on(16, 64, 0.8),
            DauxEvent::NoteExpression(NoteExpressionEvent {
                header: EventHeader::at(32),
                expression: NoteExpression::Brightness,
                note_id: -1,
                channel: 0,
                key: 60,
                value: 0.2,
            }),
            DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(64),
                param_id: param_id::CUTOFF,
                value: 800.0,
                ..ParamEvent::default()
            }),
            note_off(400, 64),
            DauxEvent::Midi1(Midi1Event {
                header: EventHeader::at(480),
                message: Midi1Message::note_on(0, 67, 90),
            }),
        ]);

        // Warm the voices up first, so the measured block is a steady-state one.
        run(&mut synth, BLOCK, &events);

        let mut output = AudioStorage::<f32>::new(2, BLOCK);
        let mut sink = EventBuffer::with_capacity(64, 1_024);
        let config = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(BLOCK, &config, &host);
        let mut outputs = [output.as_mut()];
        let mut buses = AudioBuses::new(&[], &mut outputs, BLOCK);
        let mut ports = ProcessEvents::new(&events, &mut sink);

        let (_, allocations) = AllocGuard::scope(|| synth.process(&ctx, &mut buses, &mut ports));
        assert_eq!(allocations, 0, "process allocated {allocations} time(s)");
    }

    #[test]
    fn state_round_trips_every_parameter() {
        let synth = Synth::default();
        synth.params.attack.set_plain(12.5);
        synth.params.cutoff.set_plain(789.0);
        synth.params.waveform.set(Waveform::Square);
        synth.params.level.set_plain(-3.0);

        let mut writer = StateWriter::new(StateVersion(STATE_VERSION));
        synth.save_state(&mut writer).expect("saving cannot fail");
        let blob = writer.finish();

        let mut restored = Synth::default();
        let reader = StateReader::from_bytes(&blob).expect("the blob we wrote parses");
        restored.load_state(&reader).expect("loading cannot fail");

        assert!((restored.params.attack.value() - 12.5).abs() < 1e-9);
        assert!((restored.params.cutoff.value() - 789.0).abs() < 1e-9);
        assert_eq!(restored.params.waveform.value(), Waveform::Square);
        assert!((restored.params.level.value() - -3.0).abs() < 1e-9);
    }

    #[test]
    fn a_blob_missing_a_parameter_keeps_that_parameter_at_its_default() {
        let blob = StateWriter::new(StateVersion(STATE_VERSION)).finish();
        let mut synth = Synth::default();
        synth.params.cutoff.set_plain(1_234.0);
        let reader = StateReader::from_bytes(&blob).expect("an empty blob still parses");
        synth
            .load_state(&reader)
            .expect("a missing key is not fatal");
        assert!((synth.params.cutoff.value() - 1_234.0).abs() < 1e-9);
    }

    #[test]
    fn the_descriptor_and_ports_describe_an_instrument() {
        let d = <Synth as DauxPlugin>::descriptor();
        d.validate().expect("the descriptor must be valid");
        assert_eq!(d.category, Category::Instrument);
        assert!(d.capabilities.is_instrument());
        assert!(d.capabilities.is_midi_input());
        assert!(d.capabilities.is_note_expression());

        let synth = Synth::default();
        let ports = synth.event_ports();
        assert!(ports.has_input(), "an instrument needs notes");
        assert!(
            ports.has_output(),
            "the NoteEnd events need somewhere to go, or the capability is a lie"
        );
        assert!(
            d.capabilities.is_midi_output(),
            "the plug-in emits NoteEnd, so it must say so"
        );

        let layout = synth.bus_layout();
        assert!(layout.inputs.is_empty());
        assert_eq!(layout.main_output().map(BusInfo::channel_count), Some(2));
        assert!(synth.accepts_bus_layout(&layout));
        assert!(!synth.accepts_bus_layout(&BusLayout::stereo_effect()));
    }

    #[test]
    fn every_parameter_is_reachable_by_its_permanent_id() {
        let params = SynthParams::new();
        let ids = [
            param_id::WAVEFORM,
            param_id::ATTACK,
            param_id::DECAY,
            param_id::SUSTAIN,
            param_id::RELEASE,
            param_id::CUTOFF,
            param_id::RESONANCE,
            param_id::LEVEL,
        ];
        assert_eq!(params.param_refs().len(), ids.len());
        for id in ids {
            assert!(
                params.param(ParamId::new(id)).is_some(),
                "parameter {id} is not reachable"
            );
        }
        assert!(params.param(ParamId::new(999)).is_none());
    }

    #[test]
    fn the_envelope_is_the_shape_it_claims_to_be() {
        let shape = EnvelopeShape {
            attack_step: step_per_sample(10.0, 1_000.0), // 10 samples
            decay_step: step_per_sample(10.0, 1_000.0),
            sustain: 0.5,
            release_step: step_per_sample(10.0, 1_000.0),
        };
        let mut env = Adsr::default();
        env.start(&shape);

        for _ in 0..10 {
            env.next();
        }
        assert!(
            (env.level - 1.0).abs() < 1e-5,
            "attack ended at {}",
            env.level
        );
        assert_eq!(env.stage, Stage::Decay);

        for _ in 0..10 {
            env.next();
        }
        assert!(
            (env.level - 0.5).abs() < 1e-5,
            "decay ended at {}",
            env.level
        );
        assert_eq!(env.stage, Stage::Sustain);

        // Sustain holds indefinitely.
        for _ in 0..1_000 {
            env.next();
        }
        assert!((env.level - 0.5).abs() < 1e-5);
        assert!(!env.is_finished());

        env.release();
        for _ in 0..10 {
            env.next();
        }
        assert_eq!(env.level, 0.0);
        assert!(env.is_finished());
    }

    #[test]
    fn tail_reports_the_release_time_in_samples() {
        let mut synth = Synth::default();
        synth.params.release.set_plain(500.0);
        synth.prepare(&config()).expect("a valid config");
        assert_eq!(synth.tail(), Tail::Samples(24_000));
    }

    #[test]
    fn reset_silences_everything() {
        let mut synth = prepared();
        run(&mut synth, 64, &buffer(&[note_on(0, 60, 1.0)]));
        assert!(synth.is_sounding());
        synth.reset();
        assert!(!synth.is_sounding());
        let (left, _) = run(&mut synth, 64, &buffer(&[]));
        assert_eq!(peak(&left), 0.0);
    }

    #[test]
    fn prepare_refuses_a_configuration_it_cannot_size_from() {
        let mut synth = Synth::default();
        assert!(synth.prepare(&ProcessConfig::new(0.0, 512)).is_err());
        assert!(synth.prepare(&ProcessConfig::new(48_000.0, 0)).is_err());
        assert!(synth.mix.is_empty());
    }

    #[test]
    fn a_block_larger_than_the_prepared_maximum_is_clamped_rather_than_grown() {
        let mut synth = prepared();
        let capacity = synth.mix.capacity();
        let (left, _) = run(&mut synth, BLOCK * 2, &buffer(&[note_on(0, 60, 1.0)]));
        assert_eq!(
            synth.mix.capacity(),
            capacity,
            "process re-allocated the mix"
        );
        assert_eq!(left.len(), BLOCK * 2);
        assert_eq!(
            peak(&left[BLOCK..]),
            0.0,
            "the part beyond the promised block must be defined, and silence is the safe value"
        );
    }
}
