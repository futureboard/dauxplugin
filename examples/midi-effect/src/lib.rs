//! MIDI arpeggiator: note input to note output, no audio.
//!
//! Hold a chord, get a pattern. The plug-in has no audio buses at all — it is a pure event
//! transformer, which is the shape [`EventPortLayout::midi_effect`] describes.
//!
//! # What it shows
//!
//! | Topic | Where |
//! |---|---|
//! | an event-only plug-in: [`EventPortLayout::midi_effect`], no audio buses | [`Arpeggiator::event_ports`] |
//! | sample-accurate emission — every note carries the offset it really happens at | [`Arpeggiator::tick`] |
//! | a **bounded** output, and what to do when it is full | [`Arpeggiator::emit`] |
//! | a bounded held-note set: [`FixedVec`], never a `Vec::push` on the audio thread | [`Arpeggiator::note_on`] |
//! | reading the host's tempo, and running sensibly when there is none | [`Arpeggiator::settings`] |
//!
//! # The output can be full, and that is not an error
//!
//! [`OutputEvents::try_push`] returns [`EventOverflow`](daux_plugin::EventOverflow) when the
//! host's preallocated event list has no room left. The audio thread may not grow it, may not
//! allocate a side buffer, and may not panic. There are exactly two correct answers, and this
//! example uses both:
//!
//! * **Drop** what can be re-derived. A missed *note-on* costs one note of the pattern; the
//!   next step produces another. [`Arpeggiator::tick`] simply does not start the note.
//! * **Defer** what cannot. A missed *note-off* would leave a note sounding forever, so it is
//!   retried — after a short back-off, so a persistently full output costs a handful of
//!   attempts per block rather than one per sample.
//!
//! Both paths count the failure in [`Arpeggiator::dropped_events`] and return to work. There
//! is no `log!`, because formatting a message allocates.
//!
//! # Timing
//!
//! One step is a musical division of the host's tempo, so the pattern follows the session.
//! With no transport — an offline analysis pass, a bare test harness — the arpeggiator free-
//! runs at [`DEFAULT_TEMPO`] rather than stopping, because an arpeggiator that goes silent
//! when the host forgets to send a tempo looks broken.
//!
//! The step clock is a per-sample countdown, which is what makes the emitted offsets exact:
//! a step that falls 137 samples into the block is emitted at `time = 137`, not at `time = 0`
//! and not in the next block.

use daux_plugin::prelude::*;
use daux_plugin::{EventOverflow, FixedVec};

/// The most notes the arpeggiator will hold at once.
///
/// A hard bound rather than a growing `Vec`, because the set is edited from `process`. Ten
/// fingers and a sustain pedal do not reach sixteen; a stuck-note storm does, and dropping
/// the seventeenth note is a far better failure than allocating on the audio thread.
pub const MAX_HELD_NOTES: usize = 16;

/// Tempo used when the host provides no transport.
pub const DEFAULT_TEMPO: f64 = 120.0;

/// Samples to wait before retrying an output push that failed.
///
/// Small enough that a deferred note-off is still musically on time, large enough that a
/// persistently full output costs a handful of attempts per block rather than one per sample.
const RETRY_SAMPLES: u32 = 64;

/// The permanent parameter ids.
mod param_id {
    /// Note division of one step.
    pub const DIVISION: u32 = 1;
    /// Direction the pattern walks in.
    pub const DIRECTION: u32 = 2;
    /// How many octaves the pattern spans.
    pub const OCTAVES: u32 = 3;
    /// Fraction of a step each note sounds for.
    pub const GATE: u32 = 4;
    /// Multiplier applied to the held note's velocity.
    pub const VELOCITY: u32 = 5;
}

/// The state schema version [`Arpeggiator::save_state`] writes.
const STATE_VERSION: u32 = 1;

/// How long one step lasts, as a fraction of a quarter note.
///
/// Ordered slowest to fastest. The order is part of the saved format — an [`EnumParam`]
/// stores the *index* — so a new division goes on the end, never in the middle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Division {
    /// One step per quarter note.
    Quarter,
    /// Three steps per quarter note.
    EighthTriplet,
    /// Two steps per quarter note.
    #[default]
    Eighth,
    /// Six steps per quarter note.
    SixteenthTriplet,
    /// Four steps per quarter note.
    Sixteenth,
    /// Eight steps per quarter note.
    ThirtySecond,
}

impl Division {
    /// `[audio-thread]` Length of one step in quarter-note beats.
    fn beats(self) -> f64 {
        match self {
            Self::Quarter => 1.0,
            Self::EighthTriplet => 1.0 / 3.0,
            Self::Eighth => 0.5,
            Self::SixteenthTriplet => 1.0 / 6.0,
            Self::Sixteenth => 0.25,
            Self::ThirtySecond => 0.125,
        }
    }
}

impl ParamEnum for Division {
    const VARIANTS: &'static [Self] = &[
        Self::Quarter,
        Self::EighthTriplet,
        Self::Eighth,
        Self::SixteenthTriplet,
        Self::Sixteenth,
        Self::ThirtySecond,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Quarter => "1/4",
            Self::EighthTriplet => "1/8T",
            Self::Eighth => "1/8",
            Self::SixteenthTriplet => "1/16T",
            Self::Sixteenth => "1/16",
            Self::ThirtySecond => "1/32",
        }
    }

    fn index(self) -> u32 {
        self as u32
    }

    fn from_index(i: u32) -> Option<Self> {
        Self::VARIANTS.get(i as usize).copied()
    }
}

/// The order the held notes are played in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    /// Lowest to highest, then round again.
    #[default]
    Up,
    /// Highest to lowest.
    Down,
    /// Up then back down, without sounding either end twice.
    UpDown,
    /// A note at random from the held set.
    Random,
}

impl ParamEnum for Direction {
    const VARIANTS: &'static [Self] = &[Self::Up, Self::Down, Self::UpDown, Self::Random];

    fn name(self) -> &'static str {
        match self {
            Self::Up => "Up",
            Self::Down => "Down",
            Self::UpDown => "Up/Down",
            Self::Random => "Random",
        }
    }

    fn index(self) -> u32 {
        self as u32
    }

    fn from_index(i: u32) -> Option<Self> {
        Self::VARIANTS.get(i as usize).copied()
    }
}

/// Everything the host can turn.
#[derive(DauxParams)]
pub struct ArpParams {
    /// Length of one step.
    #[param(id = param_id::DIVISION, name = "Division", default = Division::Eighth)]
    pub division: EnumParam<Division>,

    /// Order the held notes are played in.
    #[param(id = param_id::DIRECTION, name = "Direction", default = Direction::Up)]
    pub direction: EnumParam<Direction>,

    /// How many octaves the pattern spans.
    #[param(id = param_id::OCTAVES, name = "Octaves", range = 1..=4, default = 1)]
    pub octaves: IntParam,

    /// Fraction of one step that each note sounds for.
    #[param(id = param_id::GATE, name = "Gate", range = 0.05..=1.0, default = 0.5,
            decimals = 2)]
    pub gate: FloatParam,

    /// Multiplier applied to the held note's velocity.
    #[param(id = param_id::VELOCITY, name = "Velocity", range = 0.1..=2.0, default = 1.0,
            decimals = 2)]
    pub velocity: FloatParam,
}

/// One key the player is holding down.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HeldNote {
    /// MIDI channel `0..=15`.
    channel: i16,
    /// Key number `0..=127`. The held set is kept sorted by this.
    key: i16,
    /// Velocity `0.0..=1.0`, as played.
    velocity: f64,
}

/// The note the arpeggiator is currently sounding.
#[derive(Clone, Copy, Debug)]
struct Sounding {
    channel: i16,
    key: i16,
    /// Samples until the note-off is due. `0` means "due now, or overdue because the output
    /// was full when it was last attempted".
    remaining: u32,
}

/// Everything derived from the parameters and the transport for the current segment.
#[derive(Clone, Copy, Debug)]
struct Settings {
    /// Length of one step in samples. Never below one, so the step loop always advances.
    samples_per_step: f64,
    /// Length of one note in samples.
    gate_samples: u32,
    direction: Direction,
    octaves: i64,
    velocity_scale: f64,
}

/// A tempo-synced arpeggiator.
#[derive(DauxPlugin)]
#[plugin(
    id = "studio.futureboard.daux.example.arpeggiator",
    name = "DAUx Arpeggiator",
    vendor = "Futureboard Studio",
    version = "1.0.0",
    description = "Tempo-synced arpeggiator: notes in, a pattern out, no audio.",
    license = "MIT OR Apache-2.0",
    category = "midi-effect",
    capabilities(
        midi_effect,
        midi_input,
        midi_output,
        sample_accurate_auto,
        offline_render,
        sandbox_safe
    ),
    features("arpeggiator", "note-effect"),
    state_schema_version = STATE_VERSION
)]
pub struct Arpeggiator {
    /// The parameter bank.
    params: ArpParams,
    /// The keys currently held, sorted ascending by key. Bounded, and edited from `process`.
    held: FixedVec<HeldNote>,
    /// The note currently sounding, if any.
    sounding: Option<Sounding>,
    /// Which step of the pattern comes next.
    step: u64,
    /// Samples until the next step. Counts down once per sample.
    samples_to_step: f64,
    /// State of the tiny xorshift used by [`Direction::Random`].
    ///
    /// A plug-in-local generator rather than `rand`: a thread-local RNG is a lazily
    /// initialised global, which means an allocation and a `TLS` check on the audio thread.
    rng: u32,
    /// Events the host's output list had no room for.
    ///
    /// Visible so a test can assert the overflow path really ran, and so a developer can see
    /// it in a debugger. Never logged from `process` — formatting a message allocates.
    pub dropped_events: u64,
}

impl Default for Arpeggiator {
    /// `[main-thread]` A fresh arpeggiator with nothing held.
    fn default() -> Self {
        Self {
            params: ArpParams::new(),
            // The one allocation this plug-in makes, and it happens here rather than in
            // `process`. From now on the set can only ever be full, never larger.
            held: FixedVec::with_capacity(MAX_HELD_NOTES),
            sounding: None,
            step: 0,
            samples_to_step: 0.0,
            rng: 0x2545_f491,
            dropped_events: 0,
        }
    }
}

impl Arpeggiator {
    /// `[audio-thread]` Derives the step timing from the parameters and the host's tempo.
    fn settings(&self, sample_rate: f64, tempo: f64) -> Settings {
        let seconds_per_beat = 60.0 / tempo;
        let step_seconds = self.params.division.value().beats() * seconds_per_beat;
        // Never below one sample: the step countdown must make progress every sample, or the
        // loop that drives it would never terminate.
        let samples_per_step = (step_seconds * sample_rate).clamp(1.0, f64::from(u32::MAX));
        let gate = self.params.gate.value().clamp(0.01, 1.0);
        Settings {
            samples_per_step,
            gate_samples: (samples_per_step * gate).max(1.0) as u32,
            direction: self.params.direction.value(),
            octaves: self.params.octaves.value().clamp(1, 4),
            velocity_scale: self.params.velocity.value(),
        }
    }

    /// `[audio-thread]` Pushes an event, counting an overflow instead of failing the block.
    ///
    /// Returns `false` when the host's list was full, so the caller can decide whether the
    /// event can be dropped or has to be retried.
    fn emit(&mut self, out: &mut dyn OutputEvents, event: &DauxEvent<'_>) -> bool {
        match out.try_push(event) {
            Ok(()) => true,
            Err(EventOverflow) => {
                self.dropped_events = self.dropped_events.saturating_add(1);
                false
            }
        }
    }

    /// `[audio-thread]` Adds a key to the held set, keeping it sorted by pitch.
    ///
    /// A repeated key replaces the old entry rather than adding a second one — a host that
    /// sends two note-ons without a note-off in between must not be able to fill the set.
    fn note_on(&mut self, channel: i16, key: i16, velocity: f64) {
        let was_empty = self.held.is_empty();
        let note = HeldNote {
            channel,
            key,
            velocity: velocity.clamp(0.0, 1.0),
        };

        match self
            .held
            .as_slice()
            .binary_search_by(|held| (held.key, held.channel).cmp(&(key, channel)))
        {
            Ok(index) => self.held.as_mut_slice()[index] = note,
            Err(index) => {
                // `insert` on a full `FixedVec` returns the value back rather than growing:
                // the seventeenth simultaneous note is ignored, and nothing allocates.
                let _ = self.held.insert(index, note);
            }
        }

        if was_empty && !self.held.is_empty() {
            // The chord just started. Firing the first step immediately — at this very
            // sample — is what makes the arpeggiator feel responsive rather than late.
            self.step = 0;
            self.samples_to_step = 0.0;
        }
    }

    /// `[audio-thread]` Removes a key from the held set.
    fn note_off(&mut self, channel: i16, key: i16) {
        if let Some(index) = self
            .held
            .as_slice()
            .iter()
            .position(|held| held.key == key && (channel < 0 || held.channel == channel))
        {
            self.held.remove(index);
        }
    }

    /// `[audio-thread]` The note the pattern plays at `step`, or `None` when nothing is held.
    ///
    /// `&mut self` because [`Direction::Random`] advances the generator.
    fn pattern_note(&mut self, settings: &Settings) -> Option<(i16, i16, f64)> {
        let held = self.held.len();
        if held == 0 {
            return None;
        }
        let octaves = settings.octaves.max(1) as usize;
        let total = held * octaves;

        let position = match settings.direction {
            Direction::Up => (self.step % total as u64) as usize,
            Direction::Down => total - 1 - (self.step % total as u64) as usize,
            Direction::UpDown => {
                // A bounce that sounds each end once: `total + (total - 2)` positions when
                // there is more than one note, and a single position when there is not.
                let cycle = if total > 1 { 2 * total - 2 } else { 1 };
                let raw = (self.step % cycle as u64) as usize;
                if raw < total { raw } else { cycle - raw }
            }
            Direction::Random => {
                // xorshift32: no state beyond one word, no allocation, and deterministic
                // from the seed, which is what makes it testable.
                self.rng ^= self.rng << 13;
                self.rng ^= self.rng >> 17;
                self.rng ^= self.rng << 5;
                self.rng as usize % total
            }
        };

        let note = self.held.as_slice()[position % held];
        let octave = (position / held) as i16;
        let key = note.key + 12 * octave;
        // Above key 127 there is no note to play; folding back keeps the pattern going
        // instead of producing a silent step the player cannot explain.
        let key = if key > 127 { note.key } else { key };
        Some((note.channel, key, note.velocity * settings.velocity_scale))
    }

    /// `[audio-thread]` Advances one sample of the pattern clock at `frame`.
    ///
    /// Everything it emits carries `frame` as its offset, which is what makes the output
    /// sample-accurate: a step that falls in the middle of the block is *in* the middle of
    /// the block, not at its start.
    fn tick(&mut self, frame: u32, settings: &Settings, out: &mut dyn OutputEvents) {
        // --- end the sounding note when its gate expires ----------------------------------
        //
        // The countdown is decremented *before* the due check, so a note started at frame `f`
        // with a gate of `n` samples ends at frame `f + n` exactly. Checking first would put
        // every note-off one sample late — which is inaudible, and still wrong in a way that
        // compounds over a long pattern.
        if let Some(pending) = self.sounding.as_mut()
            && pending.remaining > 0
        {
            pending.remaining -= 1;
        }
        if let Some(sounding) = self.sounding
            && sounding.remaining == 0
        {
            if self.send_note_off(sounding, frame, out) {
                self.sounding = None;
            } else if let Some(pending) = self.sounding.as_mut() {
                // Deferred, not dropped: a lost note-off hangs the note forever. Back off
                // rather than retrying every single sample.
                pending.remaining = RETRY_SAMPLES;
            }
        }

        // --- fire a step ------------------------------------------------------------------
        if self.held.is_empty() {
            return;
        }
        if self.samples_to_step <= 0.0 {
            // A new note must never overlap the previous one, so an overdue note-off goes
            // first and at the same sample offset.
            if let Some(sounding) = self.sounding
                && self.send_note_off(sounding, frame, out)
            {
                self.sounding = None;
            }

            if let Some((channel, key, velocity)) = self.pattern_note(settings) {
                let on = DauxEvent::NoteOn(NoteEvent {
                    header: EventHeader::at(frame),
                    note_id: -1,
                    channel,
                    key,
                    velocity,
                    tuning: 0.0,
                });
                if self.emit(out, &on) {
                    self.sounding = Some(Sounding {
                        channel,
                        key,
                        remaining: settings.gate_samples,
                    });
                }
                // If the push failed the step is simply skipped. A missed note-on costs one
                // note of a repeating pattern, which the next step replaces — nothing to
                // defer.
            }

            self.step = self.step.wrapping_add(1);
            // `+=` rather than `=`: the remainder of a fractional step length carries into
            // the next one, so a triplet pattern does not drift over a long session.
            self.samples_to_step += settings.samples_per_step;
        }
        self.samples_to_step -= 1.0;
    }

    /// `[audio-thread]` Keeps the step countdown inside the current step length.
    ///
    /// Switching from whole notes to thirty-seconds must speed the pattern up now, not after
    /// the remaining seconds of the old, slower step have elapsed.
    fn clamp_step_clock(&mut self, settings: &Settings) {
        if self.samples_to_step > settings.samples_per_step {
            self.samples_to_step = settings.samples_per_step;
        }
    }

    /// `[audio-thread]` Emits the note-off for a sounding note; `false` when the output was
    /// full.
    fn send_note_off(&mut self, note: Sounding, frame: u32, out: &mut dyn OutputEvents) -> bool {
        let off = DauxEvent::NoteOff(NoteEvent {
            header: EventHeader::at(frame),
            note_id: -1,
            channel: note.channel,
            key: note.key,
            velocity: 0.0,
            tuning: 0.0,
        });
        self.emit(out, &off)
    }

    /// `[audio-thread]` Applies one input event at `frame`.
    ///
    /// Notes are consumed — they become the held set — and everything else is forwarded
    /// unchanged, at its own offset. An arpeggiator that swallowed the sustain pedal or the
    /// mod wheel would be a very annoying arpeggiator.
    fn apply_event(
        &mut self,
        event: &DauxEvent<'_>,
        frame: u32,
        settings: &mut Settings,
        sample_rate: f64,
        tempo: f64,
        out: &mut dyn OutputEvents,
    ) {
        match event {
            DauxEvent::NoteOn(note) if note.velocity > 0.0 => {
                self.note_on(note.channel, note.key, note.velocity);
            }
            // A note-on at velocity zero is the MIDI 1.0 spelling of a note-off.
            DauxEvent::NoteOn(note) | DauxEvent::NoteOff(note) | DauxEvent::NoteChoke(note) => {
                self.note_off(note.channel, note.key);
            }
            DauxEvent::Midi1(midi) => {
                let message = midi.message;
                let channel = i16::from(message.channel());
                let key = i16::from(message.data1());
                match message.kind() {
                    Midi1Kind::NoteOn if message.data2() > 0 => {
                        self.note_on(channel, key, f64::from(message.data2()) / 127.0);
                    }
                    Midi1Kind::NoteOn | Midi1Kind::NoteOff => self.note_off(channel, key),
                    // Controllers, pitch bend, aftertouch and clock all belong downstream.
                    _ => {
                        self.emit(out, event);
                    }
                }
            }
            DauxEvent::ParamValue(change) if change.value.is_finite() => {
                if let Some(param) = self.params.param(ParamId::new(change.param_id)) {
                    param.set_plain(change.value);
                }
                // Re-derived here rather than once per block, so a division change lands on
                // the sample the host asked for.
                *settings = self.settings(sample_rate, tempo);
                self.clamp_step_clock(settings);
            }
            _ => {
                let _ = frame;
                self.emit(out, event);
            }
        }
    }

    /// `[audio-thread]` `true` while a note is sounding or keys are held.
    fn is_active(&self) -> bool {
        self.sounding.is_some() || !self.held.is_empty()
    }
}

impl DauxProcessor for Arpeggiator {
    /// `[main-thread]` Nothing to size: the held set and the pattern state are fixed.
    ///
    /// The config is still validated, because a NaN sample rate would turn every step length
    /// below into a NaN and stop the pattern silently.
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;
        self.reset();
        Ok(())
    }

    /// `[audio-thread]` Forgets the pattern. The held set survives, because the player's
    /// fingers are still on the keys.
    fn reset(&mut self) {
        self.sounding = None;
        self.step = 0;
        self.samples_to_step = 0.0;
    }

    /// `[audio-thread]` Walks the block one sample at a time, applying input events at their
    /// own offsets and emitting steps at theirs.
    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        // A MIDI effect declares no audio buses, but a host is entitled to hand one over
        // anyway. Leaving whatever was in it there would be a defect; silence is defined.
        audio.silence_outputs();

        let frames = ctx.frames();
        if frames == 0 {
            return ProcessStatus::Continue;
        }

        // `Transport::tempo` is an `Option` precisely so a plug-in cannot read a field the
        // host never set. Free-running is a better answer than going silent.
        let sample_rate = ctx.config().sample_rate;
        let tempo = ctx
            .transport()
            .and_then(Transport::tempo)
            .filter(|t| t.is_finite() && *t > 0.0)
            .unwrap_or(DEFAULT_TEMPO);
        let mut settings = self.settings(sample_rate, tempo);
        // The tempo or a parameter may have changed since the last block without an event —
        // an editor writes the parameter directly. Keep the countdown inside the new step.
        self.clamp_step_clock(&settings);

        let (input, out) = events.split();
        let mut next = 0usize;

        for frame in 0..frames {
            // Apply everything the host scheduled at this exact sample. An event past the end
            // of the block is a host bug; clamping it into the last sample is better than
            // dropping it and much better than indexing out of bounds.
            while let Some(event) = input.get(next) {
                let at = (event.time() as usize).min(frames - 1);
                if at > frame {
                    break;
                }
                self.apply_event(&event, frame as u32, &mut settings, sample_rate, tempo, out);
                next += 1;
            }

            self.tick(frame as u32, &settings, out);
        }

        if self.is_active() {
            ProcessStatus::Continue
        } else {
            // Nothing held and nothing sounding: the host may stop calling until a note
            // arrives.
            ProcessStatus::Sleep
        }
    }
}

impl DauxController for Arpeggiator {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        w.begin_group("params");
        for (id, param) in self.params.param_refs() {
            w.put_f64(&id.get().to_string(), param.plain());
        }
        w.end_group();
        Ok(())
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        for (id, param) in self.params.param_refs() {
            if let Some(value) = r.opt_f64(&format!("params/{}", id.get())) {
                param.set_plain(value);
            }
        }
        Ok(())
    }
}

impl DauxPlugin for Arpeggiator {
    fn descriptor() -> PluginDescriptor {
        Self::descriptor()
    }

    /// `[main-thread]` No audio at all: not one bus, in either direction.
    fn bus_layout(&self) -> BusLayout {
        BusLayout::new()
    }

    /// `[main-thread]` One event input, one event output — the MIDI-effect shape.
    fn event_ports(&self) -> EventPortLayout {
        EventPortLayout::midi_effect()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        self
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        self
    }

    /// `[main-thread]` Only the empty layout. A host that proposes audio buses has
    /// misunderstood what this plug-in is.
    fn accepts_bus_layout(&self, layout: &BusLayout) -> bool {
        layout.inputs.is_empty() && layout.outputs.is_empty()
    }
}

export_plugin!(SingleFactory<Arpeggiator>);

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
    /// 480 BPM makes a sixteenth exactly 1500 samples at 48 kHz, so the expected offsets in
    /// these tests are whole numbers a reader can check by hand.
    const TEMPO: f64 = 480.0;
    /// Samples per sixteenth-note step at [`TEMPO`].
    const STEP: u32 = 1_500;

    fn config() -> ProcessConfig {
        ProcessConfig::new(SAMPLE_RATE, 8_192)
    }

    fn transport() -> Transport {
        Transport {
            flags: TransportFlags::HAS_TEMPO | TransportFlags::IS_PLAYING,
            tempo: TEMPO,
            ..Transport::EMPTY
        }
    }

    /// A prepared arpeggiator: sixteenths, up, one octave, half gate.
    fn arp() -> Arpeggiator {
        let mut arp = Arpeggiator::default();
        arp.params.division.set(Division::Sixteenth);
        arp.params.direction.set(Direction::Up);
        arp.params.octaves.set(1);
        arp.params.gate.set_plain(0.5);
        arp.prepare(&config()).expect("a valid config");
        arp
    }

    fn note_on(time: u32, key: i16) -> DauxEvent<'static> {
        DauxEvent::NoteOn(NoteEvent {
            header: EventHeader::at(time),
            note_id: -1,
            channel: 0,
            key,
            velocity: 0.8,
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

    fn buffer(events: &[DauxEvent<'_>]) -> EventBuffer {
        let mut buf = EventBuffer::with_capacity(64, 1_024);
        for event in events {
            buf.try_push(event).expect("the buffer has room");
        }
        buf
    }

    /// Runs one block with a transport and returns the events the arpeggiator produced.
    fn run(arp: &mut Arpeggiator, frames: usize, input: &EventBuffer) -> EventBuffer {
        run_with(arp, frames, input, Some(transport()), 256)
    }

    /// Runs one block with full control over the transport and the size of the output list.
    fn run_with(
        arp: &mut Arpeggiator,
        frames: usize,
        input: &EventBuffer,
        transport: Option<Transport>,
        sink_capacity: usize,
    ) -> EventBuffer {
        let config = config();
        let host = RtHostServices::null();
        let mut sink = EventBuffer::with_capacity(sink_capacity, 4_096);
        let mut buses = AudioBuses::<f32>::empty(frames);
        let mut ports = ProcessEvents::new(input, &mut sink);
        let status = match &transport {
            Some(t) => {
                let ctx = ProcessContext::new(frames, &config, &host).with_transport(t);
                arp.process(&ctx, &mut buses, &mut ports)
            }
            None => {
                let ctx = ProcessContext::new(frames, &config, &host);
                arp.process(&ctx, &mut buses, &mut ports)
            }
        };
        assert_ne!(status, ProcessStatus::Error);
        sink
    }

    /// `(time, key)` of every note-on in a buffer, in order.
    fn note_ons(events: &EventBuffer) -> Vec<(u32, i16)> {
        (0..events.len())
            .filter_map(|i| match events.get(i) {
                Some(DauxEvent::NoteOn(n)) => Some((n.header.time, n.key)),
                _ => None,
            })
            .collect()
    }

    /// `(time, key)` of every note-off in a buffer, in order.
    fn note_offs(events: &EventBuffer) -> Vec<(u32, i16)> {
        (0..events.len())
            .filter_map(|i| match events.get(i) {
                Some(DauxEvent::NoteOff(n)) => Some((n.header.time, n.key)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn steps_are_emitted_at_exactly_the_right_sample_offsets() {
        // The reason this example exists. One held note, sixteenths at 480 BPM: a step every
        // 1500 samples starting at the note-on's own offset, and a note-off half a step later.
        let mut arp = arp();
        let out = run(&mut arp, 4_096, &buffer(&[note_on(0, 60)]));

        assert_eq!(note_ons(&out), [(0, 60), (STEP, 60), (2 * STEP, 60)]);
        assert_eq!(
            note_offs(&out),
            [
                (STEP / 2, 60),
                (STEP + STEP / 2, 60),
                (2 * STEP + STEP / 2, 60)
            ]
        );
    }

    #[test]
    fn the_first_step_lands_on_the_note_on_not_at_the_top_of_the_block() {
        let mut arp = arp();
        let out = run(&mut arp, 4_096, &buffer(&[note_on(777, 60)]));
        let ons = note_ons(&out);
        assert_eq!(ons[0], (777, 60), "the arpeggiator started early or late");
        assert_eq!(ons[1], (777 + STEP, 60));
    }

    #[test]
    fn the_clock_carries_across_block_boundaries() {
        // A step that falls past the end of one block must land at the right offset in the
        // next one, not at its start.
        let mut arp = arp();
        let first = run(&mut arp, 1_000, &buffer(&[note_on(0, 60)]));
        assert_eq!(note_ons(&first), [(0, 60)]);

        let second = run(&mut arp, 1_000, &buffer(&[]));
        // The second step is due at absolute sample 1500, which is offset 500 of this block.
        assert_eq!(note_ons(&second), [(500, 60)]);
    }

    #[test]
    fn up_walks_the_held_notes_from_low_to_high() {
        let mut arp = arp();
        let out = run(
            &mut arp,
            8 * STEP as usize,
            // Held out of order on purpose: the pattern is by pitch, not by arrival.
            &buffer(&[note_on(0, 67), note_on(0, 60), note_on(0, 64)]),
        );
        let keys: Vec<i16> = note_ons(&out).into_iter().map(|(_, k)| k).collect();
        assert_eq!(&keys[..6], &[60, 64, 67, 60, 64, 67]);
    }

    #[test]
    fn down_walks_from_high_to_low() {
        let mut arp = arp();
        arp.params.direction.set(Direction::Down);
        let out = run(
            &mut arp,
            6 * STEP as usize,
            &buffer(&[note_on(0, 60), note_on(0, 64), note_on(0, 67)]),
        );
        let keys: Vec<i16> = note_ons(&out).into_iter().map(|(_, k)| k).collect();
        assert_eq!(&keys[..6], &[67, 64, 60, 67, 64, 60]);
    }

    #[test]
    fn updown_bounces_without_sounding_the_ends_twice() {
        let mut arp = arp();
        arp.params.direction.set(Direction::UpDown);
        let out = run(
            &mut arp,
            8 * STEP as usize,
            &buffer(&[note_on(0, 60), note_on(0, 64), note_on(0, 67)]),
        );
        let keys: Vec<i16> = note_ons(&out).into_iter().map(|(_, k)| k).collect();
        assert_eq!(&keys[..8], &[60, 64, 67, 64, 60, 64, 67, 64]);
    }

    #[test]
    fn random_only_ever_plays_a_held_note() {
        let mut arp = arp();
        arp.params.direction.set(Direction::Random);
        arp.params.octaves.set(2);
        let out = run(
            &mut arp,
            16 * STEP as usize,
            &buffer(&[note_on(0, 60), note_on(0, 64)]),
        );
        let keys: Vec<i16> = note_ons(&out).into_iter().map(|(_, k)| k).collect();
        assert!(keys.len() >= 8);
        for key in keys {
            assert!(
                [60, 64, 72, 76].contains(&key),
                "the pattern played {key}, which is not in the chord"
            );
        }
    }

    #[test]
    fn octaves_extend_the_pattern_upwards() {
        let mut arp = arp();
        arp.params.octaves.set(3);
        let out = run(
            &mut arp,
            7 * STEP as usize,
            &buffer(&[note_on(0, 60), note_on(0, 64)]),
        );
        let keys: Vec<i16> = note_ons(&out).into_iter().map(|(_, k)| k).collect();
        assert_eq!(&keys[..6], &[60, 64, 72, 76, 84, 88]);
    }

    #[test]
    fn a_pattern_note_above_key_127_folds_back_rather_than_going_silent() {
        let mut arp = arp();
        arp.params.octaves.set(4);
        let out = run(&mut arp, 5 * STEP as usize, &buffer(&[note_on(0, 120)]));
        let keys: Vec<i16> = note_ons(&out).into_iter().map(|(_, k)| k).collect();
        assert!(
            keys.iter().all(|&k| (0..=127).contains(&k)),
            "an out-of-range key escaped: {keys:?}"
        );
        assert!(keys.contains(&120));
    }

    #[test]
    fn the_gate_parameter_sets_how_long_each_note_sounds() {
        for (gate, expected) in [(0.25, STEP / 4), (0.5, STEP / 2), (1.0, STEP)] {
            let mut arp = arp();
            arp.params.gate.set_plain(gate);
            let out = run(&mut arp, 2 * STEP as usize, &buffer(&[note_on(0, 60)]));
            let offs = note_offs(&out);
            assert_eq!(
                offs[0].0, expected,
                "gate {gate} produced a note-off at {}",
                offs[0].0
            );
        }
    }

    #[test]
    fn releasing_the_last_key_stops_the_pattern_and_ends_the_sounding_note() {
        let mut arp = arp();
        // Gate 1.0 so a note is definitely still sounding when the key comes up.
        arp.params.gate.set_plain(1.0);
        run(&mut arp, 100, &buffer(&[note_on(0, 60)]));
        assert!(arp.sounding.is_some());

        let out = run(&mut arp, 4 * STEP as usize, &buffer(&[note_off(0, 60)]));
        assert!(arp.held.is_empty());
        assert_eq!(
            note_ons(&out),
            [],
            "no new notes may start once the keys are up"
        );
        assert_eq!(note_offs(&out).len(), 1, "the sounding note must be ended");
        assert!(arp.sounding.is_none());
    }

    #[test]
    fn an_idle_arpeggiator_lets_the_host_stop_calling_it() {
        let mut arp = arp();
        let config = config();
        let host = RtHostServices::null();
        let t = transport();
        let ctx = ProcessContext::new(512, &config, &host).with_transport(&t);
        let input = buffer(&[]);
        let mut sink = EventBuffer::with_capacity(16, 256);
        let mut buses = AudioBuses::<f32>::empty(512);
        let mut ports = ProcessEvents::new(&input, &mut sink);
        assert_eq!(
            arp.process(&ctx, &mut buses, &mut ports),
            ProcessStatus::Sleep
        );
    }

    #[test]
    fn a_full_output_defers_the_note_off_instead_of_hanging_the_note() {
        let mut arp = arp();
        // Room for exactly one event: the note-on fits, the note-off does not.
        let out = run_with(
            &mut arp,
            STEP as usize,
            &buffer(&[note_on(0, 60)]),
            Some(transport()),
            1,
        );
        assert_eq!(out.len(), 1, "the sink only had room for one event");
        assert_eq!(note_ons(&out), [(0, 60)]);
        assert!(
            arp.dropped_events > 0,
            "the overflow must be counted, not ignored"
        );
        assert!(
            arp.sounding.is_some(),
            "a note whose note-off was refused must stay pending, or it hangs forever"
        );

        // The next block has room, and the deferred note-off is delivered rather than lost.
        let recovered = run(&mut arp, STEP as usize, &buffer(&[note_off(0, 60)]));
        assert!(
            !note_offs(&recovered).is_empty(),
            "the deferred note-off never arrived"
        );
        assert!(arp.sounding.is_none());
    }

    #[test]
    fn a_full_output_skips_a_note_on_without_panicking_or_allocating() {
        assert!(counting_allocator_installed());
        let mut arp = arp();
        let config = config();
        let host = RtHostServices::null();
        let t = transport();
        let ctx = ProcessContext::new(4 * STEP as usize, &config, &host).with_transport(&t);
        let input = buffer(&[note_on(0, 60)]);
        // No room at all: every push fails.
        let mut sink = EventBuffer::with_capacity(0, 0);
        let mut buses = AudioBuses::<f32>::empty(4 * STEP as usize);
        let mut ports = ProcessEvents::new(&input, &mut sink);

        let (status, allocations) = AllocGuard::scope(|| arp.process(&ctx, &mut buses, &mut ports));
        assert_eq!(status, ProcessStatus::Continue);
        assert_eq!(sink.len(), 0);
        assert_eq!(allocations, 0, "the overflow path allocated");
        assert!(arp.dropped_events > 0);
        assert!(
            arp.sounding.is_none(),
            "a note-on that was never sent must not leave a note pending"
        );
    }

    #[test]
    fn notes_are_consumed_and_everything_else_is_forwarded_at_its_own_offset() {
        let mut arp = arp();
        let bend = DauxEvent::Midi1(Midi1Event {
            header: EventHeader::at(321),
            message: Midi1Message::control_change(0, 1, 64),
        });
        let out = run(&mut arp, 1_000, &buffer(&[note_on(0, 60), bend]));

        let forwarded: Vec<(u32, u8)> = (0..out.len())
            .filter_map(|i| match out.get(i) {
                Some(DauxEvent::Midi1(m)) => Some((m.header.time, m.message.data1())),
                _ => None,
            })
            .collect();
        assert_eq!(
            forwarded,
            [(321, 1)],
            "the controller was not passed through"
        );
        // …and the note the player held did not appear on the output as itself.
        assert_eq!(
            note_ons(&out),
            [(0, 60)],
            "only the pattern's note may appear"
        );
    }

    #[test]
    fn midi_1_notes_drive_the_pattern_too() {
        let mut arp = arp();
        let events = buffer(&[DauxEvent::Midi1(Midi1Event {
            header: EventHeader::at(0),
            message: Midi1Message::note_on(0, 60, 100),
        })]);
        let out = run(&mut arp, STEP as usize, &events);
        assert_eq!(note_ons(&out), [(0, 60)]);

        let off = buffer(&[DauxEvent::Midi1(Midi1Event {
            header: EventHeader::at(0),
            message: Midi1Message::note_on(0, 60, 0),
        })]);
        run(&mut arp, 16, &off);
        assert!(
            arp.held.is_empty(),
            "velocity 0 is the MIDI 1.0 spelling of a note-off"
        );
    }

    #[test]
    fn more_held_notes_than_the_bound_are_ignored_rather_than_allocated() {
        assert!(counting_allocator_installed());
        let mut arp = arp();
        let mut events = EventBuffer::with_capacity(64, 2_048);
        for key in 0..(MAX_HELD_NOTES as i16 + 8) {
            events.try_push(&note_on(0, 40 + key)).expect("room");
        }

        let config = config();
        let host = RtHostServices::null();
        let t = transport();
        let ctx = ProcessContext::new(64, &config, &host).with_transport(&t);
        let mut sink = EventBuffer::with_capacity(64, 2_048);
        let mut buses = AudioBuses::<f32>::empty(64);
        let mut ports = ProcessEvents::new(&events, &mut sink);
        let (_, allocations) = AllocGuard::scope(|| arp.process(&ctx, &mut buses, &mut ports));

        assert_eq!(arp.held.len(), MAX_HELD_NOTES);
        assert_eq!(allocations, 0, "the held set grew on the audio thread");
    }

    #[test]
    fn a_repeated_note_on_replaces_rather_than_duplicates() {
        let mut arp = arp();
        run(
            &mut arp,
            16,
            &buffer(&[note_on(0, 60), note_on(1, 60), note_on(2, 60)]),
        );
        assert_eq!(arp.held.len(), 1, "the same key was held three times");
    }

    #[test]
    fn without_a_transport_the_pattern_free_runs_at_the_default_tempo() {
        let mut arp = arp();
        // 120 BPM, sixteenths: 0.125 s, which is 6000 samples at 48 kHz.
        let out = run_with(&mut arp, 13_000, &buffer(&[note_on(0, 60)]), None, 64);
        assert_eq!(note_ons(&out), [(0, 60), (6_000, 60), (12_000, 60)]);
    }

    #[test]
    fn a_nonsense_tempo_falls_back_instead_of_producing_nan() {
        let mut arp = arp();
        let broken = Transport {
            flags: TransportFlags::HAS_TEMPO,
            tempo: f64::NAN,
            ..Transport::EMPTY
        };
        let out = run_with(
            &mut arp,
            13_000,
            &buffer(&[note_on(0, 60)]),
            Some(broken),
            64,
        );
        assert_eq!(note_ons(&out), [(0, 60), (6_000, 60), (12_000, 60)]);
    }

    #[test]
    fn a_division_change_takes_effect_at_its_own_sample_offset() {
        let mut arp = arp();
        arp.params.division.set(Division::Quarter); // 6000 samples at 480 BPM
        let events = buffer(&[
            note_on(0, 60),
            DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(100),
                param_id: param_id::DIVISION,
                value: f64::from(Division::ThirtySecond.index()),
                ..ParamEvent::default()
            }),
        ]);
        // A thirty-second at 480 BPM is 750 samples, so after the change the steps come fast.
        // The countdown left over from the quarter note is clamped to the new, shorter step,
        // which is why the second step lands 750 samples after the change rather than 6000
        // after the first step.
        let out = run(&mut arp, 4_000, &events);
        let ons = note_ons(&out);
        assert_eq!(ons[0], (0, 60));
        assert!(
            ons.len() >= 4,
            "the faster division did not take effect inside the block: {ons:?}"
        );
        assert_eq!(ons[1].0, 850, "one 1/32 after the parameter change at 100");
        assert_eq!(ons[2].0, 1_600);
    }

    #[test]
    fn an_event_past_the_end_of_the_block_does_not_panic() {
        let mut arp = arp();
        let out = run(&mut arp, 64, &buffer(&[note_on(99_999, 60)]));
        // Clamped into the last sample rather than dropped or indexed out of bounds.
        assert_eq!(note_ons(&out), [(63, 60)]);
    }

    #[test]
    fn process_never_allocates() {
        assert!(
            counting_allocator_installed(),
            "the tripwire is not installed, so this test would pass vacuously"
        );
        let mut arp = arp();
        let events = buffer(&[
            note_on(0, 60),
            note_on(10, 64),
            note_on(20, 67),
            DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(64),
                param_id: param_id::GATE,
                value: 0.9,
                ..ParamEvent::default()
            }),
            DauxEvent::Midi1(Midi1Event {
                header: EventHeader::at(128),
                message: Midi1Message::control_change(0, 74, 100),
            }),
            note_off(2_000, 64),
        ]);

        // Warm up, so the measured block is a steady-state one.
        run(&mut arp, 4_096, &events);

        let config = config();
        let host = RtHostServices::null();
        let t = transport();
        let ctx = ProcessContext::new(4_096, &config, &host).with_transport(&t);
        let mut sink = EventBuffer::with_capacity(256, 4_096);
        let mut buses = AudioBuses::<f32>::empty(4_096);
        let mut ports = ProcessEvents::new(&events, &mut sink);

        let (_, allocations) = AllocGuard::scope(|| arp.process(&ctx, &mut buses, &mut ports));
        assert_eq!(allocations, 0, "process allocated {allocations} time(s)");
        assert!(!sink.is_empty());
    }

    #[test]
    fn reset_forgets_the_pattern_but_not_the_keys() {
        let mut arp = arp();
        run(&mut arp, 100, &buffer(&[note_on(0, 60), note_on(0, 64)]));
        assert_eq!(arp.held.len(), 2);
        arp.reset();
        assert_eq!(
            arp.held.len(),
            2,
            "the player's fingers are still on the keys"
        );
        assert!(arp.sounding.is_none());
        assert_eq!(arp.step, 0);
    }

    #[test]
    fn the_ports_and_descriptor_describe_an_event_only_plug_in() {
        let d = <Arpeggiator as DauxPlugin>::descriptor();
        d.validate().expect("the descriptor must be valid");
        assert_eq!(d.category, Category::MidiEffect);
        assert!(d.capabilities.is_midi_effect());
        assert!(d.capabilities.is_midi_input());
        assert!(d.capabilities.is_midi_output());
        assert!(!d.capabilities.is_audio_effect());

        let arp = Arpeggiator::default();
        assert_eq!(arp.event_ports(), EventPortLayout::midi_effect());
        assert!(arp.bus_layout().inputs.is_empty());
        assert!(arp.bus_layout().outputs.is_empty());
        assert!(arp.accepts_bus_layout(&BusLayout::new()));
        assert!(!arp.accepts_bus_layout(&BusLayout::stereo_effect()));
    }

    #[test]
    fn state_round_trips_every_parameter() {
        let arp = Arpeggiator::default();
        arp.params.division.set(Division::ThirtySecond);
        arp.params.direction.set(Direction::UpDown);
        arp.params.octaves.set(3);
        arp.params.gate.set_plain(0.9);

        let mut writer = StateWriter::new(StateVersion(STATE_VERSION));
        arp.save_state(&mut writer).expect("saving cannot fail");
        let blob = writer.finish();

        let mut restored = Arpeggiator::default();
        let reader = StateReader::from_bytes(&blob).expect("the blob we wrote parses");
        restored.load_state(&reader).expect("loading cannot fail");

        assert_eq!(restored.params.division.value(), Division::ThirtySecond);
        assert_eq!(restored.params.direction.value(), Direction::UpDown);
        assert_eq!(restored.params.octaves.value(), 3);
        assert!((restored.params.gate.value() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn every_parameter_is_reachable_by_its_permanent_id() {
        let params = ArpParams::new();
        for id in [
            param_id::DIVISION,
            param_id::DIRECTION,
            param_id::OCTAVES,
            param_id::GATE,
            param_id::VELOCITY,
        ] {
            assert!(
                params.param(ParamId::new(id)).is_some(),
                "id {id} is missing"
            );
        }
        assert_eq!(params.param_refs().len(), 5);
        assert!(params.param(ParamId::new(99)).is_none());
    }

    #[test]
    fn the_division_table_is_the_one_the_names_promise() {
        for (division, beats) in [
            (Division::Quarter, 1.0),
            (Division::Eighth, 0.5),
            (Division::Sixteenth, 0.25),
            (Division::ThirtySecond, 0.125),
        ] {
            assert!((division.beats() - beats).abs() < 1e-12, "{division:?}");
        }
        // A triplet is two thirds of its straight sibling.
        assert!(
            (Division::EighthTriplet.beats() - Division::Eighth.beats() * 2.0 / 3.0).abs() < 1e-12
        );
        assert!(
            (Division::SixteenthTriplet.beats() - Division::Sixteenth.beats() * 2.0 / 3.0).abs()
                < 1e-12
        );
    }
}
