//! The transport value itself, plus musical/temporal conversions.

use crate::flags::TransportFlags;
use crate::signature::TimeSignature;

/// Seconds per minute — the constant that turns BPM into beats per second.
const SECONDS_PER_MINUTE: f64 = 60.0;

/// Host transport state for one processing block. [any-thread]
///
/// A plain-Rust mirror of `DauxTransportV1` (`docs/specifications/abi-v1.md` §10). The
/// struct is `Copy`, `Send` and `Sync`; it holds no references and is safe to snapshot
/// into an event or hand to the UI.
///
/// **Every field is only meaningful when its `HAS_*` flag is set.** The fields are public
/// so hosts and ABI adapters can fill them in, but plug-ins should read them exclusively
/// through the `Option`-returning accessors — a plug-in must never be able to read a value
/// the host did not provide.
///
/// Positions describe the **first sample of the current block**. Musical positions are
/// measured in quarter-note beats, matching the ABI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transport {
    /// Which fields are valid, and the play/record/loop status. Mirrors `flags`.
    pub flags: TransportFlags,
    /// Continuous sample position of the first frame of this block on the host timeline.
    /// Always meaningful; there is no `HAS_*` bit for it.
    pub song_pos_samples: i64,
    /// Musical position in quarter-note beats. Valid with [`TransportFlags::HAS_BEATS`].
    pub song_pos_beats: f64,
    /// Wall-clock position on the timeline in seconds. Valid with
    /// [`TransportFlags::HAS_SECONDS`].
    pub song_pos_seconds: f64,
    /// Tempo in BPM at the first sample of the block. Valid with
    /// [`TransportFlags::HAS_TEMPO`].
    pub tempo: f64,
    /// Linear tempo ramp in BPM **per sample**, `0.0` when the tempo is steady across the
    /// block. Valid with [`TransportFlags::HAS_TEMPO`].
    pub tempo_increment: f64,
    /// Musical position of the first beat of the current bar, in quarter-note beats.
    /// Valid with [`TransportFlags::HAS_BAR`].
    pub bar_start_beats: f64,
    /// Number of the current bar as the host displays it. Valid with
    /// [`TransportFlags::HAS_BAR`].
    pub bar_number: i32,
    /// The current time signature. Valid with [`TransportFlags::HAS_TIME_SIG`].
    pub time_signature: TimeSignature,
    /// Start of the loop region in quarter-note beats. Valid with
    /// [`TransportFlags::HAS_LOOP`].
    pub loop_start_beats: f64,
    /// End of the loop region in quarter-note beats. Valid with
    /// [`TransportFlags::HAS_LOOP`].
    pub loop_end_beats: f64,
    /// Start of the loop region in seconds. Valid with [`TransportFlags::HAS_LOOP`].
    pub loop_start_seconds: f64,
    /// End of the loop region in seconds. Valid with [`TransportFlags::HAS_LOOP`].
    pub loop_end_seconds: f64,
}

impl Transport {
    /// A transport that promises nothing: no flags, so every optional accessor is `None`.
    ///
    /// This is what a plug-in sees when the host exposes no transport at all, and it is
    /// what [`Default`] returns.
    pub const EMPTY: Self = Self {
        flags: TransportFlags::NONE,
        song_pos_samples: 0,
        song_pos_beats: 0.0,
        song_pos_seconds: 0.0,
        tempo: 0.0,
        tempo_increment: 0.0,
        bar_start_beats: 0.0,
        bar_number: 0,
        time_signature: TimeSignature::COMMON,
        loop_start_beats: 0.0,
        loop_end_beats: 0.0,
        loop_start_seconds: 0.0,
        loop_end_seconds: 0.0,
    };

    // ---------------------------------------------------------------- status ----

    /// `true` when the host timeline is rolling. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn is_playing(&self) -> bool {
        self.flags.contains(TransportFlags::IS_PLAYING)
    }

    /// `true` when the host is recording. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn is_recording(&self) -> bool {
        self.flags.contains(TransportFlags::IS_RECORDING)
    }

    /// `true` when loop playback is armed. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn is_looping(&self) -> bool {
        self.flags.contains(TransportFlags::IS_LOOPING)
    }

    /// `true` when this block is pre-roll / count-in. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn is_preroll(&self) -> bool {
        self.flags.contains(TransportFlags::IS_PREROLL)
    }

    // ------------------------------------------------------------ accessors ----

    /// Tempo in BPM at the first sample of the block, or `None` unless
    /// [`TransportFlags::HAS_TEMPO`] is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn tempo(&self) -> Option<f64> {
        if self.flags.contains(TransportFlags::HAS_TEMPO) {
            Some(self.tempo)
        } else {
            None
        }
    }

    /// Tempo ramp in BPM per sample, or `None` unless [`TransportFlags::HAS_TEMPO`] is
    /// set. `0.0` means the tempo is steady for the whole block. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn tempo_increment(&self) -> Option<f64> {
        if self.flags.contains(TransportFlags::HAS_TEMPO) {
            Some(self.tempo_increment)
        } else {
            None
        }
    }

    /// Instantaneous tempo `sample_offset` samples into the block, following the linear
    /// ramp `tempo + tempo_increment * sample_offset`. [audio-thread]
    ///
    /// `None` unless [`TransportFlags::HAS_TEMPO`] is set or if `sample_offset` is not
    /// finite. A long enough ramp can drive the result to zero or below; callers that
    /// divide by it must check.
    #[inline]
    #[must_use]
    pub fn tempo_at(&self, sample_offset: f64) -> Option<f64> {
        if !sample_offset.is_finite() {
            return None;
        }
        let tempo = self.tempo()?;
        let value = tempo + self.tempo_increment * sample_offset;
        value.is_finite().then_some(value)
    }

    /// Musical position of the first sample of the block in quarter-note beats, or `None`
    /// unless [`TransportFlags::HAS_BEATS`] is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn beats(&self) -> Option<f64> {
        if self.flags.contains(TransportFlags::HAS_BEATS) {
            Some(self.song_pos_beats)
        } else {
            None
        }
    }

    /// Timeline position of the first sample of the block in seconds, or `None` unless
    /// [`TransportFlags::HAS_SECONDS`] is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn seconds(&self) -> Option<f64> {
        if self.flags.contains(TransportFlags::HAS_SECONDS) {
            Some(self.song_pos_seconds)
        } else {
            None
        }
    }

    /// Continuous sample position of the first frame of the block. [audio-thread]
    ///
    /// Always available: ABI v1 defines no `HAS_*` bit for it.
    #[inline]
    #[must_use]
    pub const fn samples(&self) -> i64 {
        self.song_pos_samples
    }

    /// The current time signature, or `None` unless [`TransportFlags::HAS_TIME_SIG`] is
    /// set. [audio-thread]
    ///
    /// A flagged but degenerate signature (a zero numerator or denominator) is rejected
    /// too, so the result is always usable.
    #[inline]
    #[must_use]
    pub fn time_signature(&self) -> Option<TimeSignature> {
        if self.flags.contains(TransportFlags::HAS_TIME_SIG) && self.time_signature.is_valid() {
            Some(self.time_signature)
        } else {
            None
        }
    }

    /// Beat position of the start of the current bar, or `None` unless
    /// [`TransportFlags::HAS_BAR`] is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn bar_start_beats(&self) -> Option<f64> {
        if self.flags.contains(TransportFlags::HAS_BAR) {
            Some(self.bar_start_beats)
        } else {
            None
        }
    }

    /// The host's number for the current bar, or `None` unless [`TransportFlags::HAS_BAR`]
    /// is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn bar_number(&self) -> Option<i32> {
        if self.flags.contains(TransportFlags::HAS_BAR) {
            Some(self.bar_number)
        } else {
            None
        }
    }

    /// How far the block start lies inside the current bar, in quarter-note beats.
    /// [audio-thread]
    ///
    /// Requires both [`TransportFlags::HAS_BEATS`] and [`TransportFlags::HAS_BAR`].
    #[inline]
    #[must_use]
    pub fn beats_into_bar(&self) -> Option<f64> {
        Some(self.beats()? - self.bar_start_beats()?)
    }

    /// The loop region as `(start, end)` in quarter-note beats, or `None` unless
    /// [`TransportFlags::HAS_LOOP`] is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn loop_range_beats(&self) -> Option<(f64, f64)> {
        if self.flags.contains(TransportFlags::HAS_LOOP) {
            Some((self.loop_start_beats, self.loop_end_beats))
        } else {
            None
        }
    }

    /// The loop region as `(start, end)` in seconds, or `None` unless
    /// [`TransportFlags::HAS_LOOP`] is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn loop_range_seconds(&self) -> Option<(f64, f64)> {
        if self.flags.contains(TransportFlags::HAS_LOOP) {
            Some((self.loop_start_seconds, self.loop_end_seconds))
        } else {
            None
        }
    }

    /// Length of the loop region in quarter-note beats, or `None` unless
    /// [`TransportFlags::HAS_LOOP`] is set. Negative when the host reported an inverted
    /// range. [audio-thread]
    #[inline]
    #[must_use]
    pub fn loop_length_beats(&self) -> Option<f64> {
        let (start, end) = self.loop_range_beats()?;
        Some(end - start)
    }

    // ---------------------------------------------------------- conversions ----

    /// Beats spanned by `samples` frames starting at the block boundary. [audio-thread]
    ///
    /// With a linear tempo ramp the instantaneous tempo at offset `t` is
    /// `T(t) = tempo + tempo_increment * t` BPM, so the musical position advances by
    /// `T(t) / (60 * sample_rate)` beats per sample and the span of `n` samples is the
    /// integral
    ///
    /// ```text
    /// B(n) = ∫₀ⁿ (tempo + tempo_increment · t) / (60 · sample_rate) dt
    ///      = (tempo · n + tempo_increment · n² / 2) / (60 · sample_rate)
    /// ```
    ///
    /// which reduces to the familiar `n · tempo / (60 · sample_rate)` when
    /// `tempo_increment` is `0.0`. `n` may be negative to look backwards.
    ///
    /// Returns `None` when [`TransportFlags::HAS_TEMPO`] is clear, when `sample_rate` is
    /// not finite and positive, when `samples` is not finite, or when the tempo is not a
    /// finite positive number.
    #[inline]
    #[must_use]
    pub fn samples_to_beats(&self, samples: f64, sample_rate: f64) -> Option<f64> {
        let (tempo, increment) = self.usable_tempo()?;
        let rate = usable_rate(sample_rate)?;
        if !samples.is_finite() {
            return None;
        }
        let beats =
            (tempo * samples + 0.5 * increment * samples * samples) / (SECONDS_PER_MINUTE * rate);
        beats.is_finite().then_some(beats)
    }

    /// Samples needed to advance by `beats` quarter notes from the block boundary — the
    /// inverse of [`Transport::samples_to_beats`]. [audio-thread]
    ///
    /// Inverting `B(n)` means solving the quadratic
    /// `½·tempo_increment·n² + tempo·n − 60·sample_rate·beats = 0`. The branch that stays
    /// continuous with the steady-tempo case is evaluated in the cancellation-free form
    ///
    /// ```text
    /// n = 2c / (tempo + √(tempo² + 2 · tempo_increment · c)),   c = 60 · sample_rate · beats
    /// ```
    ///
    /// which also reproduces `n = c / tempo` exactly when `tempo_increment` is `0.0`. The
    /// chosen root is the one whose instantaneous tempo `tempo + tempo_increment·n` is
    /// non-negative, i.e. the one that does not require running time through a tempo
    /// reversal.
    ///
    /// Returns `None` under the same conditions as [`Transport::samples_to_beats`], and
    /// additionally when the ramp never reaches `beats` (a decelerating ramp that hits
    /// zero BPM first), which shows up as a negative discriminant.
    #[inline]
    #[must_use]
    pub fn beats_to_samples(&self, beats: f64, sample_rate: f64) -> Option<f64> {
        let (tempo, increment) = self.usable_tempo()?;
        let rate = usable_rate(sample_rate)?;
        if !beats.is_finite() {
            return None;
        }
        let c = SECONDS_PER_MINUTE * rate * beats;
        if increment == 0.0 {
            let samples = c / tempo;
            return samples.is_finite().then_some(samples);
        }
        let discriminant = tempo * tempo + 2.0 * increment * c;
        if discriminant.is_nan() || discriminant < 0.0 {
            // The ramp decelerates to a standstill before reaching this musical position.
            return None;
        }
        let denominator = tempo + discriminant.sqrt();
        if denominator <= 0.0 {
            return None;
        }
        let samples = 2.0 * c / denominator;
        samples.is_finite().then_some(samples)
    }

    /// Seconds spanned by `samples` frames. [audio-thread]
    ///
    /// Pure sample-clock arithmetic — no tempo needed — so this only fails on an unusable
    /// `sample_rate` or a non-finite `samples`.
    #[inline]
    #[must_use]
    pub fn samples_to_seconds(&self, samples: f64, sample_rate: f64) -> Option<f64> {
        let rate = usable_rate(sample_rate)?;
        if !samples.is_finite() {
            return None;
        }
        let seconds = samples / rate;
        seconds.is_finite().then_some(seconds)
    }

    /// Frames spanned by `seconds`. [audio-thread]
    #[inline]
    #[must_use]
    pub fn seconds_to_samples(&self, seconds: f64, sample_rate: f64) -> Option<f64> {
        let rate = usable_rate(sample_rate)?;
        if !seconds.is_finite() {
            return None;
        }
        let samples = seconds * rate;
        samples.is_finite().then_some(samples)
    }

    /// Seconds needed to advance by `beats` quarter notes from the block boundary.
    /// [audio-thread]
    ///
    /// Routed through [`Transport::beats_to_samples`], so the tempo ramp is integrated
    /// exactly as documented there. `sample_rate` is required because `tempo_increment` is
    /// expressed per *sample*, not per second.
    #[inline]
    #[must_use]
    pub fn beats_to_seconds(&self, beats: f64, sample_rate: f64) -> Option<f64> {
        let samples = self.beats_to_samples(beats, sample_rate)?;
        self.samples_to_seconds(samples, sample_rate)
    }

    /// Beats spanned by `seconds` of wall-clock time from the block boundary.
    /// [audio-thread]
    ///
    /// Routed through [`Transport::samples_to_beats`]; see it for the ramp integral.
    #[inline]
    #[must_use]
    pub fn seconds_to_beats(&self, seconds: f64, sample_rate: f64) -> Option<f64> {
        let samples = self.seconds_to_samples(seconds, sample_rate)?;
        self.samples_to_beats(samples, sample_rate)
    }

    /// Absolute musical position `sample_offset` frames into the block. [audio-thread]
    ///
    /// This is the accessor a sample-accurate plug-in wants for an event at
    /// `EventHeader::time`. Requires [`TransportFlags::HAS_BEATS`] **and**
    /// [`TransportFlags::HAS_TEMPO`].
    #[inline]
    #[must_use]
    pub fn beats_at(&self, sample_offset: f64, sample_rate: f64) -> Option<f64> {
        let base = self.beats()?;
        let delta = self.samples_to_beats(sample_offset, sample_rate)?;
        let value = base + delta;
        value.is_finite().then_some(value)
    }

    /// Absolute timeline position in seconds `sample_offset` frames into the block.
    /// [audio-thread]
    ///
    /// Requires [`TransportFlags::HAS_SECONDS`].
    #[inline]
    #[must_use]
    pub fn seconds_at(&self, sample_offset: f64, sample_rate: f64) -> Option<f64> {
        let base = self.seconds()?;
        let delta = self.samples_to_seconds(sample_offset, sample_rate)?;
        let value = base + delta;
        value.is_finite().then_some(value)
    }

    /// Absolute sample position `sample_offset` frames into the block, saturating instead
    /// of overflowing. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn samples_at(&self, sample_offset: i64) -> i64 {
        self.song_pos_samples.saturating_add(sample_offset)
    }

    /// Tempo and ramp, but only when both are present and usable for division.
    #[inline]
    fn usable_tempo(&self) -> Option<(f64, f64)> {
        if !self.flags.contains(TransportFlags::HAS_TEMPO) {
            return None;
        }
        if !self.tempo.is_finite() || self.tempo <= 0.0 || !self.tempo_increment.is_finite() {
            return None;
        }
        Some((self.tempo, self.tempo_increment))
    }
}

impl Default for Transport {
    /// [`Transport::EMPTY`] — a transport that reports nothing.
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Rejects sample rates that would make every conversion meaningless.
#[inline]
fn usable_rate(sample_rate: f64) -> Option<f64> {
    (sample_rate.is_finite() && sample_rate > 0.0).then_some(sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransportBuilder;

    const SR: f64 = 48_000.0;

    fn assert_close(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() <= eps, "expected {a} ≈ {b} (eps {eps})");
    }

    #[test]
    fn is_send_and_sync_and_copy() {
        const fn assert_traits<T: Send + Sync + Copy>() {}
        assert_traits::<Transport>();
        assert_traits::<TransportFlags>();
        assert_traits::<TimeSignature>();
    }

    #[test]
    fn empty_transport_promises_nothing() {
        let t = Transport::default();
        assert_eq!(t, Transport::EMPTY);
        assert!(!t.is_playing());
        assert!(!t.is_recording());
        assert!(!t.is_looping());
        assert!(!t.is_preroll());
        assert_eq!(t.tempo(), None);
        assert_eq!(t.tempo_increment(), None);
        assert_eq!(t.tempo_at(0.0), None);
        assert_eq!(t.beats(), None);
        assert_eq!(t.seconds(), None);
        assert_eq!(t.time_signature(), None);
        assert_eq!(t.bar_start_beats(), None);
        assert_eq!(t.bar_number(), None);
        assert_eq!(t.beats_into_bar(), None);
        assert_eq!(t.loop_range_beats(), None);
        assert_eq!(t.loop_range_seconds(), None);
        assert_eq!(t.loop_length_beats(), None);
        assert_eq!(t.samples_to_beats(128.0, SR), None);
        assert_eq!(t.beats_to_samples(1.0, SR), None);
        assert_eq!(t.beats_to_seconds(1.0, SR), None);
        assert_eq!(t.seconds_to_beats(1.0, SR), None);
        assert_eq!(t.beats_at(0.0, SR), None);
        assert_eq!(t.seconds_at(0.0, SR), None);
        // The sample clock has no HAS_* bit and is always readable.
        assert_eq!(t.samples(), 0);
    }

    #[test]
    fn a_field_is_unreadable_without_its_flag_even_when_populated() {
        // A hostile or sloppy host filling in values without setting the bits.
        let t = Transport {
            flags: TransportFlags::NONE,
            song_pos_beats: 12.0,
            song_pos_seconds: 6.0,
            tempo: 120.0,
            bar_start_beats: 8.0,
            bar_number: 3,
            time_signature: TimeSignature::new(7, 8),
            loop_start_beats: 0.0,
            loop_end_beats: 16.0,
            ..Transport::EMPTY
        };
        assert_eq!(t.tempo(), None);
        assert_eq!(t.beats(), None);
        assert_eq!(t.seconds(), None);
        assert_eq!(t.time_signature(), None);
        assert_eq!(t.bar_number(), None);
        assert_eq!(t.loop_range_beats(), None);
    }

    #[test]
    fn flags_gate_each_accessor_independently() {
        let t = TransportBuilder::new().tempo(120.0).build();
        assert_eq!(t.tempo(), Some(120.0));
        assert_eq!(t.beats(), None);
        assert_eq!(t.seconds(), None);

        let t = TransportBuilder::new().beats(4.0).build();
        assert_eq!(t.beats(), Some(4.0));
        assert_eq!(t.tempo(), None);

        let t = TransportBuilder::new().seconds(2.5).build();
        assert_eq!(t.seconds(), Some(2.5));
        assert_eq!(t.beats(), None);

        let t = TransportBuilder::new().time_signature(7, 8).build();
        assert_eq!(t.time_signature(), Some(TimeSignature::new(7, 8)));

        let t = TransportBuilder::new().bar(5, 16.0).build();
        assert_eq!(t.bar_number(), Some(5));
        assert_eq!(t.bar_start_beats(), Some(16.0));

        let t = TransportBuilder::new().loop_beats(4.0, 12.0).build();
        assert_eq!(t.loop_range_beats(), Some((4.0, 12.0)));
        assert_eq!(t.loop_length_beats(), Some(8.0));
        // loop_seconds was never supplied, but HAS_LOOP covers both pairs; the host
        // contract is that it fills all four fields.
        assert_eq!(t.loop_range_seconds(), Some((0.0, 0.0)));
    }

    #[test]
    fn degenerate_time_signature_is_rejected_even_when_flagged() {
        let t = Transport {
            flags: TransportFlags::HAS_TIME_SIG,
            time_signature: TimeSignature::new(4, 0),
            ..Transport::EMPTY
        };
        assert_eq!(t.time_signature(), None);
    }

    #[test]
    fn beats_into_bar_needs_both_flags() {
        let both = TransportBuilder::new().beats(18.5).bar(5, 16.0).build();
        assert_eq!(both.beats_into_bar(), Some(2.5));
        let only_beats = TransportBuilder::new().beats(18.5).build();
        assert_eq!(only_beats.beats_into_bar(), None);
        let only_bar = TransportBuilder::new().bar(5, 16.0).build();
        assert_eq!(only_bar.beats_into_bar(), None);
    }

    #[test]
    fn steady_tempo_conversions_are_exact() {
        let t = TransportBuilder::new().tempo(120.0).build();
        // 120 BPM at 48 kHz → 24 000 samples per beat.
        assert_eq!(t.beats_to_samples(1.0, SR), Some(24_000.0));
        assert_eq!(t.samples_to_beats(24_000.0, SR), Some(1.0));
        assert_eq!(t.beats_to_samples(0.0, SR), Some(0.0));
        assert_eq!(t.samples_to_beats(0.0, SR), Some(0.0));
        // Backwards in time.
        assert_eq!(t.beats_to_samples(-2.0, SR), Some(-48_000.0));
        assert_eq!(t.samples_to_beats(-48_000.0, SR), Some(-2.0));
        // Seconds.
        assert_eq!(t.beats_to_seconds(2.0, SR), Some(1.0));
        assert_eq!(t.seconds_to_beats(1.0, SR), Some(2.0));
        assert_eq!(t.samples_to_seconds(48_000.0, SR), Some(1.0));
        assert_eq!(t.seconds_to_samples(0.5, SR), Some(24_000.0));
    }

    #[test]
    fn tempo_at_follows_the_ramp() {
        let t = TransportBuilder::new().tempo_ramp(120.0, 0.001).build();
        assert_eq!(t.tempo_at(0.0), Some(120.0));
        assert_eq!(t.tempo_at(1000.0), Some(121.0));
        assert_eq!(t.tempo_increment(), Some(0.001));
        assert_eq!(t.tempo_at(f64::NAN), None);
        assert_eq!(t.tempo_at(f64::INFINITY), None);
    }

    #[test]
    fn ramp_integral_matches_per_sample_accumulation() {
        // Midpoint accumulation is exact for a linear integrand, so this is a genuine
        // check of the closed form against a naive sample-by-sample host.
        let t = TransportBuilder::new().tempo_ramp(90.0, 0.002).build();
        let n = 4096usize;
        let mut acc = 0.0f64;
        for k in 0..n {
            let mid = k as f64 + 0.5;
            acc += (90.0 + 0.002 * mid) / (60.0 * SR);
        }
        let closed = t.samples_to_beats(n as f64, SR).unwrap();
        assert_close(closed, acc, 1e-9);
    }

    #[test]
    fn ramp_round_trips_beats_and_samples() {
        for increment in [-0.0005, -0.0001, 0.0, 0.0001, 0.003] {
            let t = TransportBuilder::new().tempo_ramp(128.0, increment).build();
            for samples in [1.0, 64.0, 512.0, 4096.0, 44_100.0] {
                let beats = t.samples_to_beats(samples, SR).unwrap();
                let back = t.beats_to_samples(beats, SR).unwrap();
                assert_close(back, samples, samples.abs() * 1e-9 + 1e-6);
            }
            for beats in [-0.25, 0.0, 0.5, 4.0] {
                let samples = t.beats_to_samples(beats, SR).unwrap();
                let back = t.samples_to_beats(samples, SR).unwrap();
                assert_close(back, beats, beats.abs() * 1e-9 + 1e-9);
            }
        }
    }

    #[test]
    fn zero_increment_matches_the_general_formula() {
        let steady = TransportBuilder::new().tempo(140.0).build();
        let ramped = TransportBuilder::new().tempo_ramp(140.0, 1e-18).build();
        let a = steady.beats_to_samples(3.0, SR).unwrap();
        let b = ramped.beats_to_samples(3.0, SR).unwrap();
        assert_close(a, b, 1e-6);
    }

    #[test]
    fn accelerating_ramp_reaches_a_beat_sooner_than_steady_tempo() {
        let steady = TransportBuilder::new().tempo(100.0).build();
        let faster = TransportBuilder::new().tempo_ramp(100.0, 0.01).build();
        let slower = TransportBuilder::new().tempo_ramp(100.0, -0.000_01).build();
        let s = steady.beats_to_samples(4.0, SR).unwrap();
        let f = faster.beats_to_samples(4.0, SR).unwrap();
        let l = slower.beats_to_samples(4.0, SR).unwrap();
        assert!(f < s, "{f} !< {s}");
        assert!(l > s, "{l} !> {s}");
    }

    #[test]
    fn decelerating_ramp_that_stalls_returns_none() {
        // 120 BPM losing 1 BPM every sample reaches 0 BPM after 120 samples and can
        // never advance past the beats accumulated up to that point.
        let t = TransportBuilder::new().tempo_ramp(120.0, -1.0).build();
        let reachable = t.samples_to_beats(120.0, SR).unwrap();
        assert!(t.beats_to_samples(reachable * 0.5, SR).is_some());
        assert_eq!(t.beats_to_samples(reachable * 2.0, SR), None);
    }

    #[test]
    fn chosen_root_never_runs_time_through_a_tempo_reversal() {
        let t = TransportBuilder::new().tempo_ramp(120.0, -1.0).build();
        let peak = t.samples_to_beats(120.0, SR).unwrap();
        let n = t.beats_to_samples(peak, SR).unwrap();
        // The root with non-negative instantaneous tempo is the first crossing, at the
        // moment the tempo reaches exactly zero.
        assert_close(n, 120.0, 1e-6);
        assert!(t.tempo_at(n).unwrap() >= -1e-9);
    }

    #[test]
    fn invalid_sample_rates_are_rejected() {
        let t = TransportBuilder::new().tempo(120.0).build();
        for rate in [0.0, -48_000.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(t.samples_to_beats(100.0, rate), None, "rate {rate}");
            assert_eq!(t.beats_to_samples(1.0, rate), None, "rate {rate}");
            assert_eq!(t.samples_to_seconds(100.0, rate), None, "rate {rate}");
            assert_eq!(t.seconds_to_samples(1.0, rate), None, "rate {rate}");
            assert_eq!(t.beats_to_seconds(1.0, rate), None, "rate {rate}");
            assert_eq!(t.seconds_to_beats(1.0, rate), None, "rate {rate}");
        }
    }

    #[test]
    fn non_finite_inputs_are_rejected() {
        let t = TransportBuilder::new().tempo(120.0).build();
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(t.samples_to_beats(v, SR), None);
            assert_eq!(t.beats_to_samples(v, SR), None);
            assert_eq!(t.samples_to_seconds(v, SR), None);
            assert_eq!(t.seconds_to_samples(v, SR), None);
        }
    }

    #[test]
    fn nonsense_tempo_values_are_rejected_even_when_flagged() {
        for tempo in [0.0, -120.0, f64::NAN, f64::INFINITY] {
            let t = Transport {
                flags: TransportFlags::HAS_TEMPO,
                tempo,
                ..Transport::EMPTY
            };
            assert_eq!(t.samples_to_beats(64.0, SR), None, "tempo {tempo}");
            assert_eq!(t.beats_to_samples(1.0, SR), None, "tempo {tempo}");
        }
        let t = Transport {
            flags: TransportFlags::HAS_TEMPO,
            tempo: 120.0,
            tempo_increment: f64::NAN,
            ..Transport::EMPTY
        };
        assert_eq!(t.samples_to_beats(64.0, SR), None);
        assert_eq!(t.beats_to_samples(1.0, SR), None);
    }

    #[test]
    fn absolute_positions_add_the_block_offset() {
        let t = TransportBuilder::new()
            .tempo(120.0)
            .beats(8.0)
            .seconds(4.0)
            .sample_position(192_000)
            .build();
        assert_eq!(t.beats_at(0.0, SR), Some(8.0));
        assert_eq!(t.beats_at(24_000.0, SR), Some(9.0));
        assert_eq!(t.seconds_at(48_000.0, SR), Some(5.0));
        assert_eq!(t.samples_at(128), 192_128);
        assert_eq!(t.samples_at(-192_000), 0);
    }

    #[test]
    fn samples_at_saturates() {
        let t = Transport {
            song_pos_samples: i64::MAX,
            ..Transport::EMPTY
        };
        assert_eq!(t.samples_at(1), i64::MAX);
        let t = Transport {
            song_pos_samples: i64::MIN,
            ..Transport::EMPTY
        };
        assert_eq!(t.samples_at(-1), i64::MIN);
    }

    #[test]
    fn beats_at_needs_tempo_as_well_as_beats() {
        let t = TransportBuilder::new().beats(8.0).build();
        assert_eq!(t.beats_at(0.0, SR), None);
        let t = TransportBuilder::new().tempo(120.0).build();
        assert_eq!(t.beats_at(0.0, SR), None);
    }

    #[test]
    fn huge_values_do_not_produce_infinities() {
        let t = TransportBuilder::new().tempo(f64::MAX / 4.0).build();
        // Flagged, positive and finite — but the arithmetic overflows, so the result is
        // rejected rather than handed to the plug-in as an infinity.
        assert_eq!(t.samples_to_beats(f64::MAX, 1e-300), None);
        let t = TransportBuilder::new().tempo(1e-300).build();
        assert_eq!(t.beats_to_samples(f64::MAX, f64::MAX), None);
    }
}
