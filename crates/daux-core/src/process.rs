//! The audio-thread call: its configuration, its borrowed context and its result.

use core::fmt;

use daux_audio::SampleFormat;
use daux_events::{InputEvents, OutputEvents, TransportSnapshot};
use daux_host_services::RtHostServices;
use daux_transport::Transport;

use crate::{DauxError, DauxResult, ErrorKind};

/// Why the host is calling [`process`](crate::DauxProcessor::process).
///
/// Mirrors `DAUX_PROCESS_MODE_*` (abi-v1 §8). The mode is fixed for the lifetime of one
/// [`prepare`](crate::DauxProcessor::prepare)/`activate` cycle: a host that switches between
/// real-time and offline rendering must deactivate, re-`prepare` and re-activate.
///
/// Only [`ProcessMode::Realtime`] carries the hard no-allocation, no-locking guarantee.
/// The other modes still forbid *unbounded* work per block, but a plug-in may legitimately
/// trade latency for quality in them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProcessMode {
    /// Live playback. The deadline is real and missing it is audible.
    #[default]
    Realtime,
    /// Faster-than-real-time bounce or freeze. Quality may be raised.
    Offline,
    /// The host is filling a cache ahead of playback; output is kept, timing is not critical.
    Prefetch,
    /// The host wants measurements, not audio: loudness scanning, waveform generation.
    Analysis,
}

impl ProcessMode {
    /// [any-thread] The ABI code for this mode (abi-v1 §8).
    pub const fn code(self) -> u32 {
        match self {
            ProcessMode::Realtime => 0,
            ProcessMode::Offline => 1,
            ProcessMode::Prefetch => 2,
            ProcessMode::Analysis => 3,
        }
    }

    /// [any-thread] Decodes an ABI mode code.
    ///
    /// An unrecognised code becomes [`ProcessMode::Realtime`], which is the strictest mode:
    /// if we cannot tell what the host meant, we must assume the deadline is real.
    pub const fn from_code(code: u32) -> Self {
        match code {
            1 => ProcessMode::Offline,
            2 => ProcessMode::Prefetch,
            3 => ProcessMode::Analysis,
            _ => ProcessMode::Realtime,
        }
    }

    /// [any-thread] `true` when the audio-thread rules of `docs/architecture/realtime.md`
    /// apply with no exceptions.
    pub const fn is_realtime(self) -> bool {
        matches!(self, ProcessMode::Realtime)
    }
}

/// What the host has committed to for this activation.
///
/// A plug-in does every allocation it will ever need from
/// [`prepare`](crate::DauxProcessor::prepare), sized by this struct. The host must not exceed
/// [`ProcessConfig::max_block_size`] in any later `process` call; doing so is a host bug, and
/// a plug-in is entitled to clamp rather than allocate.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ProcessConfig {
    /// Sample rate in Hz. Strictly positive and finite.
    pub sample_rate: f64,
    /// Smallest block the host will ask for. May be `0` for "no lower bound".
    pub min_block_size: u32,
    /// Largest block the host will ask for. Strictly positive; sizes every scratch buffer.
    pub max_block_size: u32,
    /// The sample format the host will pass in.
    pub sample_format: SampleFormat,
    /// Why the host is processing.
    pub process_mode: ProcessMode,
}

impl Default for ProcessConfig {
    /// 48 kHz, blocks of at most 512 frames, `f32`, real-time.
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            min_block_size: 0,
            max_block_size: 512,
            sample_format: SampleFormat::F32,
            process_mode: ProcessMode::Realtime,
        }
    }
}

impl ProcessConfig {
    /// [main-thread] The common case: a rate, a maximum block size, `f32`, real-time.
    pub fn new(sample_rate: f64, max_block_size: u32) -> Self {
        Self {
            sample_rate,
            max_block_size,
            ..Self::default()
        }
    }

    /// Returns this config with `format` as the sample format.
    #[must_use]
    pub const fn with_sample_format(mut self, format: SampleFormat) -> Self {
        self.sample_format = format;
        self
    }

    /// Returns this config with `mode` as the process mode.
    #[must_use]
    pub const fn with_process_mode(mut self, mode: ProcessMode) -> Self {
        self.process_mode = mode;
        self
    }

    /// Returns this config with a lower bound on the block size.
    #[must_use]
    pub const fn with_min_block_size(mut self, frames: u32) -> Self {
        self.min_block_size = frames;
        self
    }

    /// [main-thread] Rejects a configuration a plug-in cannot safely size itself from.
    ///
    /// A plug-in should call this at the top of `prepare` rather than trusting the host: a
    /// NaN sample rate or a zero block size turns every derived coefficient into a silent
    /// source of NaNs downstream.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidArgument`] when the rate is not finite and positive, when
    /// `max_block_size` is zero, or when `min_block_size > max_block_size`.
    pub fn validate(&self) -> DauxResult<()> {
        if !self.sample_rate.is_finite() || self.sample_rate <= 0.0 {
            return Err(DauxError::new(
                ErrorKind::InvalidArgument,
                format!(
                    "sample rate {} is not finite and positive",
                    self.sample_rate
                ),
            ));
        }
        if self.max_block_size == 0 {
            return Err(DauxError::new(
                ErrorKind::InvalidArgument,
                "max block size must be at least 1 frame",
            ));
        }
        if self.min_block_size > self.max_block_size {
            return Err(DauxError::new(
                ErrorKind::InvalidArgument,
                format!(
                    "min block size {} exceeds max block size {}",
                    self.min_block_size, self.max_block_size
                ),
            ));
        }
        Ok(())
    }

    /// [audio-thread] Seconds per sample, the usual starting point for a coefficient.
    pub fn seconds_per_sample(&self) -> f64 {
        1.0 / self.sample_rate
    }

    /// [main-thread] `frames` expressed as a whole number of samples, clamped to at least 1.
    ///
    /// Useful for sizing a delay line or a smoothing ramp from a duration in seconds.
    pub fn samples_for(&self, seconds: f64) -> u32 {
        if !seconds.is_finite() || seconds <= 0.0 {
            return 1;
        }
        let samples = (seconds * self.sample_rate).ceil();
        if samples >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            (samples as u32).max(1)
        }
    }
}

/// What a plug-in tells the host after a block.
///
/// Mirrors `DAUX_PROCESS_*` (abi-v1 §8). The host uses this to decide whether it may stop
/// calling the plug-in, which is what makes a large session affordable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProcessStatus {
    /// Processing failed. The host should treat the output as undefined and may bypass or
    /// disable the plug-in. A plug-in must still have left its output buffers in a defined
    /// state — silence is the safe choice.
    Error,
    /// Keep calling. The plug-in has, or may have, more to produce.
    #[default]
    Continue,
    /// Keep calling only while the input is not silent.
    ///
    /// The plug-in is a pure function of its input with no internal energy: once the host
    /// sees constant silence in, it may stop calling and assume silence out.
    ContinueIfNotQuiet,
    /// The input has stopped but a tail is still ringing out; call
    /// [`tail`](crate::DauxProcessor::tail) for how long.
    Tail,
    /// Nothing left to produce. The host may stop calling until something changes — a new
    /// event, a parameter change, or non-silent input.
    Sleep,
}

impl ProcessStatus {
    /// [any-thread] The ABI code for this status (abi-v1 §8).
    pub const fn code(self) -> i32 {
        match self {
            ProcessStatus::Error => 0,
            ProcessStatus::Continue => 1,
            ProcessStatus::ContinueIfNotQuiet => 2,
            ProcessStatus::Tail => 3,
            ProcessStatus::Sleep => 4,
        }
    }

    /// [any-thread] Decodes an ABI status code.
    ///
    /// An unrecognised code becomes [`ProcessStatus::Continue`]: the conservative reading is
    /// that the plug-in still has work to do, since falsely putting it to sleep would cut off
    /// audio.
    pub const fn from_code(code: i32) -> Self {
        match code {
            0 => ProcessStatus::Error,
            2 => ProcessStatus::ContinueIfNotQuiet,
            3 => ProcessStatus::Tail,
            4 => ProcessStatus::Sleep,
            _ => ProcessStatus::Continue,
        }
    }

    /// [audio-thread] `true` when the host must keep calling `process` unconditionally.
    pub const fn must_keep_calling(self) -> bool {
        matches!(self, ProcessStatus::Continue | ProcessStatus::Tail)
    }

    /// [audio-thread] `true` when the block failed.
    pub const fn is_error(self) -> bool {
        matches!(self, ProcessStatus::Error)
    }
}

/// How long a plug-in keeps producing sound after its input goes silent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tail {
    /// Output is silent as soon as input is. The host may stop calling immediately.
    #[default]
    None,
    /// The tail is bounded and this long, in samples at the prepared rate.
    Samples(u32),
    /// The tail never ends — a self-oscillating filter, an infinite reverb, a drone.
    Infinite,
    /// The plug-in cannot say. The host must keep calling until it reports otherwise.
    Unknown,
}

impl Tail {
    /// The ABI sentinel for [`Tail::Infinite`] — `DAUX_TAIL_INFINITE`, abi-v1 §11.5.
    pub const INFINITE_SAMPLES: u32 = u32::MAX;
    /// The ABI sentinel for [`Tail::Unknown`] — `DAUX_TAIL_UNKNOWN`, abi-v1 §11.5.
    ///
    /// A host that does not distinguish the two must read this as infinite, never as a
    /// finite tail; an adapter exporting to a format with one sentinel maps both onto it.
    pub const UNKNOWN_SAMPLES: u32 = u32::MAX - 1;

    /// [audio-thread] Encodes the tail as the ABI's single `u32`.
    pub const fn samples(self) -> u32 {
        match self {
            Tail::None => 0,
            Tail::Samples(n) => n,
            Tail::Infinite => Self::INFINITE_SAMPLES,
            Tail::Unknown => Self::UNKNOWN_SAMPLES,
        }
    }

    /// [audio-thread] Decodes the ABI's single `u32`.
    pub const fn from_samples(samples: u32) -> Self {
        match samples {
            0 => Tail::None,
            Self::INFINITE_SAMPLES => Tail::Infinite,
            Self::UNKNOWN_SAMPLES => Tail::Unknown,
            n => Tail::Samples(n),
        }
    }

    /// [audio-thread] `true` when the host may ever stop calling this plug-in.
    pub const fn is_bounded(self) -> bool {
        matches!(self, Tail::None | Tail::Samples(_))
    }
}

impl fmt::Display for Tail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tail::None => f.write_str("none"),
            Tail::Samples(n) => write!(f, "{n} samples"),
            Tail::Infinite => f.write_str("infinite"),
            Tail::Unknown => f.write_str("unknown"),
        }
    }
}

/// How far a plug-in delays its output relative to its input.
///
/// The host compensates by delaying every other path by the same amount, so a wrong value is
/// audible as a phase or timing error rather than as a failure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Latency {
    /// Output is aligned with input. Nothing to compensate.
    #[default]
    Zero,
    /// Output lags input by this many samples at the prepared rate.
    Samples(u32),
}

impl Latency {
    /// [audio-thread] The latency in samples; [`Latency::Zero`] is `0`.
    pub const fn samples(self) -> u32 {
        match self {
            Latency::Zero => 0,
            Latency::Samples(n) => n,
        }
    }

    /// [audio-thread] Builds a latency from a sample count, normalising `0`.
    pub const fn from_samples(samples: u32) -> Self {
        if samples == 0 {
            Latency::Zero
        } else {
            Latency::Samples(samples)
        }
    }
}

impl fmt::Display for Latency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} samples", self.samples())
    }
}

/// The borrowed event ports for one block.
///
/// Input and output are separate objects rather than one buffer, because a plug-in must be
/// able to read the whole input list while appending to the output without aliasing.
///
/// [audio-thread]
pub struct ProcessEvents<'a> {
    input: &'a dyn InputEvents,
    output: &'a mut dyn OutputEvents,
}

impl fmt::Debug for ProcessEvents<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessEvents")
            .field("input_len", &self.input.len())
            .finish_non_exhaustive()
    }
}

impl<'a> ProcessEvents<'a> {
    /// [audio-thread] Binds one block's input list to its output sink.
    pub fn new(input: &'a dyn InputEvents, output: &'a mut dyn OutputEvents) -> Self {
        Self { input, output }
    }

    /// [audio-thread] The host's events for this block, sorted by time.
    pub fn input(&self) -> &dyn InputEvents {
        self.input
    }

    /// [audio-thread] The bounded sink for events this plug-in produces.
    pub fn output(&mut self) -> &mut dyn OutputEvents {
        self.output
    }

    /// [audio-thread] Both halves at once, so a plug-in can forward while it reads.
    ///
    /// The borrow checker will not let you hold `input()` across a call to `output()`,
    /// because the second borrows `self` mutably. This returns the pair in one step.
    pub fn split(&mut self) -> (&dyn InputEvents, &mut dyn OutputEvents) {
        (self.input, self.output)
    }

    /// [audio-thread] Number of events the host queued for this block.
    pub fn len(&self) -> usize {
        self.input.len()
    }

    /// [audio-thread] `true` when the host queued nothing.
    pub fn is_empty(&self) -> bool {
        self.input.len() == 0
    }
}

/// Everything a `process` call may touch that is not audio and not events.
///
/// Every field is borrowed for exactly the duration of the call. Nothing here may be retained
/// past the end of [`process`](crate::DauxProcessor::process); the host is free to invalidate
/// all of it the moment the call returns.
///
/// [audio-thread]
pub struct ProcessContext<'a> {
    frames: usize,
    config: &'a ProcessConfig,
    transport: Option<&'a Transport>,
    steady_time: Option<i64>,
    host: &'a RtHostServices,
}

impl fmt::Debug for ProcessContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessContext")
            .field("frames", &self.frames)
            .field("has_transport", &self.transport.is_some())
            .field("steady_time", &self.steady_time)
            .finish_non_exhaustive()
    }
}

impl<'a> ProcessContext<'a> {
    /// [audio-thread] Builds a context for one block.
    ///
    /// Hosts and adapters construct this; a plug-in only ever receives one.
    pub fn new(frames: usize, config: &'a ProcessConfig, host: &'a RtHostServices) -> Self {
        Self {
            frames,
            config,
            transport: None,
            steady_time: None,
            host,
        }
    }

    /// Attaches the host's transport state for this block.
    #[must_use]
    pub fn with_transport(mut self, transport: &'a Transport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Attaches the host's monotonic sample counter for this block.
    #[must_use]
    pub const fn with_steady_time(mut self, steady_time: i64) -> Self {
        self.steady_time = Some(steady_time);
        self
    }

    /// [audio-thread] How many frames this block covers.
    ///
    /// Always within `[min_block_size, max_block_size]` of the prepared
    /// [`ProcessConfig`] for a conforming host.
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// [audio-thread] The host's musical timeline, or `None` when it has no transport.
    ///
    /// A plug-in that syncs to tempo must handle `None` — an offline analysis pass or a bare
    /// test harness has no timeline, and inventing one is worse than running free.
    pub const fn transport(&self) -> Option<&Transport> {
        self.transport
    }

    /// [audio-thread] A monotonic sample counter that never runs backwards, even when the
    /// user relocates the playhead, or `None` when the host does not provide one.
    ///
    /// This is the right clock for an LFO that must not jump when the user scrubs; the
    /// transport position is the right clock for anything that must line up with the grid.
    pub const fn steady_time(&self) -> Option<i64> {
        self.steady_time
    }

    /// [audio-thread] The configuration this activation was prepared with.
    pub const fn config(&self) -> &ProcessConfig {
        self.config
    }

    /// [audio-thread] The real-time-safe subset of the host's services.
    ///
    /// Only the RT-safe subset is reachable from here. Anything that could allocate, lock or
    /// block lives on the main-thread [`HostServices`](daux_host_services::HostServices),
    /// which `process` cannot see at all — the split is enforced by the type system rather
    /// than by a comment.
    pub const fn host(&self) -> &RtHostServices {
        self.host
    }

    /// [audio-thread] The transport as the flat, `Copy` snapshot the event model uses.
    ///
    /// Returns [`TransportSnapshot::unknown`] when there is no transport, so a plug-in that
    /// only reads flags does not have to branch twice.
    pub fn transport_snapshot(&self) -> TransportSnapshot {
        self.transport
            .map_or_else(TransportSnapshot::unknown, |t| (*t).into())
    }

    /// [audio-thread] Seconds covered by this block at the prepared rate.
    pub fn duration_seconds(&self) -> f64 {
        self.frames as f64 / self.config.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_events::{DauxEvent, EventBuffer};

    #[test]
    fn process_mode_codes_round_trip() {
        for m in [
            ProcessMode::Realtime,
            ProcessMode::Offline,
            ProcessMode::Prefetch,
            ProcessMode::Analysis,
        ] {
            assert_eq!(ProcessMode::from_code(m.code()), m);
        }
        // An unknown mode is read as the strictest one, never as a licence to allocate.
        assert_eq!(ProcessMode::from_code(77), ProcessMode::Realtime);
        assert!(ProcessMode::default().is_realtime());
    }

    #[test]
    fn process_status_codes_round_trip() {
        for s in [
            ProcessStatus::Error,
            ProcessStatus::Continue,
            ProcessStatus::ContinueIfNotQuiet,
            ProcessStatus::Tail,
            ProcessStatus::Sleep,
        ] {
            assert_eq!(ProcessStatus::from_code(s.code()), s);
        }
        assert_eq!(ProcessStatus::from_code(-3), ProcessStatus::Continue);
        assert!(ProcessStatus::Tail.must_keep_calling());
        assert!(!ProcessStatus::Sleep.must_keep_calling());
        assert!(ProcessStatus::Error.is_error());
    }

    #[test]
    fn tail_sentinels_round_trip_and_do_not_collide() {
        for t in [
            Tail::None,
            Tail::Samples(1),
            Tail::Samples(48_000),
            Tail::Infinite,
            Tail::Unknown,
        ] {
            assert_eq!(Tail::from_samples(t.samples()), t, "{t}");
        }
        assert_ne!(Tail::INFINITE_SAMPLES, Tail::UNKNOWN_SAMPLES);
        assert!(Tail::Samples(10).is_bounded());
        assert!(!Tail::Unknown.is_bounded());
        assert_eq!(Tail::Samples(64).to_string(), "64 samples");
    }

    /// Pins both sentinels to the literals abi-v1 §11.5 fixes.
    ///
    /// A round-trip test cannot catch a wrong number: a self-consistent encoding round-trips
    /// perfectly and still tells every non-DAUx host something false. `daux-core` cannot
    /// depend on `daux-abi`, so the constants are restated here and this is what keeps the
    /// two honest — the same arrangement `Category::code` uses for §6.1.
    #[test]
    fn tail_sentinels_match_abi_v1_section_11_5() {
        assert_eq!(Tail::INFINITE_SAMPLES, u32::MAX, "DAUX_TAIL_INFINITE");
        assert_eq!(Tail::UNKNOWN_SAMPLES, u32::MAX - 1, "DAUX_TAIL_UNKNOWN");
        // Everything below the two sentinels is a real, finite tail — including the largest
        // one a plug-in can express, which must not be mistaken for a sentinel.
        assert_eq!(
            Tail::from_samples(u32::MAX - 2),
            Tail::Samples(u32::MAX - 2)
        );
    }

    #[test]
    fn latency_normalises_zero() {
        assert_eq!(Latency::from_samples(0), Latency::Zero);
        assert_eq!(Latency::from_samples(3), Latency::Samples(3));
        assert_eq!(Latency::Zero.samples(), 0);
        assert_eq!(Latency::default(), Latency::Zero);
    }

    #[test]
    fn a_default_config_is_valid() {
        let c = ProcessConfig::default();
        c.validate().unwrap();
        assert_eq!(c.sample_rate, 48_000.0);
        assert_eq!(c.max_block_size, 512);
    }

    #[test]
    fn config_validation_catches_the_ways_a_host_can_lie() {
        let bad_rate = ProcessConfig::new(f64::NAN, 512);
        assert_eq!(
            bad_rate.validate().unwrap_err().kind(),
            ErrorKind::InvalidArgument
        );

        let zero_rate = ProcessConfig::new(0.0, 512);
        assert!(zero_rate.validate().is_err());

        let zero_block = ProcessConfig::new(48_000.0, 0);
        assert!(zero_block.validate().is_err());

        let inverted = ProcessConfig::new(48_000.0, 64).with_min_block_size(128);
        let err = inverted.validate().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(err.message().contains("min block size"));
    }

    #[test]
    fn config_derives_sample_counts_defensively() {
        let c = ProcessConfig::new(48_000.0, 512);
        assert_eq!(c.samples_for(1.0), 48_000);
        assert_eq!(c.samples_for(0.5), 24_000);
        // A ceil, so a fractional duration never rounds down to nothing.
        assert_eq!(c.samples_for(1.0 / 48_000.0 / 2.0), 1);
        // Degenerate durations clamp instead of producing 0 or panicking.
        assert_eq!(c.samples_for(0.0), 1);
        assert_eq!(c.samples_for(-1.0), 1);
        assert_eq!(c.samples_for(f64::NAN), 1);
        assert_eq!(c.samples_for(f64::INFINITY), 1);
        assert_eq!(c.samples_for(1e30), u32::MAX);
    }

    #[test]
    fn config_builders_compose() {
        let c = ProcessConfig::new(96_000.0, 1024)
            .with_sample_format(SampleFormat::F64)
            .with_process_mode(ProcessMode::Offline)
            .with_min_block_size(32);
        assert_eq!(c.sample_format, SampleFormat::F64);
        assert_eq!(c.process_mode, ProcessMode::Offline);
        assert_eq!(c.min_block_size, 32);
        assert!((c.seconds_per_sample() - 1.0 / 96_000.0).abs() < f64::EPSILON);
        c.validate().unwrap();
    }

    #[test]
    fn a_context_without_a_transport_reports_an_unknown_snapshot() {
        let config = ProcessConfig::new(48_000.0, 512);
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(256, &config, &host);
        assert_eq!(ctx.frames(), 256);
        assert!(ctx.transport().is_none());
        assert!(ctx.steady_time().is_none());
        assert_eq!(ctx.transport_snapshot(), TransportSnapshot::unknown());
        assert!((ctx.duration_seconds() - 256.0 / 48_000.0).abs() < 1e-12);
        assert_eq!(ctx.config().max_block_size, 512);
    }

    #[test]
    fn a_context_carries_the_transport_and_the_steady_clock() {
        let config = ProcessConfig::new(48_000.0, 512);
        let host = RtHostServices::null();
        let transport = Transport::EMPTY;
        let ctx = ProcessContext::new(128, &config, &host)
            .with_transport(&transport)
            .with_steady_time(4_096);
        assert!(ctx.transport().is_some());
        assert_eq!(ctx.steady_time(), Some(4_096));
        assert_eq!(ctx.transport_snapshot(), TransportSnapshot::from(transport));
    }

    #[test]
    fn process_events_exposes_both_halves_without_aliasing() {
        let mut input = EventBuffer::with_capacity(8, 256);
        input
            .try_push(&DauxEvent::NoteOn(daux_events::NoteEvent {
                header: daux_events::EventHeader::at(0),
                ..Default::default()
            }))
            .unwrap();
        let mut output = EventBuffer::with_capacity(8, 256);
        let mut events = ProcessEvents::new(&input, &mut output);

        assert_eq!(events.len(), 1);
        assert!(!events.is_empty());

        let (inp, out) = events.split();
        for i in 0..inp.len() {
            let e = inp.get(i).unwrap();
            out.try_push(&e).unwrap();
        }
        assert_eq!(output.len(), 1);
    }
}
