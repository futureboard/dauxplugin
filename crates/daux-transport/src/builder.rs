//! Construction of valid [`Transport`] values.

use crate::flags::TransportFlags;
use crate::signature::TimeSignature;
use crate::transport::Transport;

/// Builds a [`Transport`] whose flags always agree with the fields that were actually
/// supplied. [any-thread]
///
/// Setting a value is what turns its `HAS_*` bit on, so a transport produced here can
/// never claim to know something it was not told. Values the builder considers unusable —
/// a non-finite or non-positive tempo, a time signature with a zero term, a non-finite
/// position — are ignored and leave the corresponding flag clear rather than producing a
/// transport a plug-in would have to defend against.
///
/// The builder itself allocates nothing (a [`Transport`] is `Copy`), so it is usable from
/// a host's audio callback as well as from tests.
///
/// ```
/// use daux_transport::{TransportBuilder, TransportFlags};
///
/// let t = TransportBuilder::new()
///     .playing(true)
///     .tempo(120.0)
///     .beats(8.0)
///     .time_signature(3, 4)
///     .build();
///
/// assert!(t.is_playing());
/// assert_eq!(t.tempo(), Some(120.0));
/// assert_eq!(t.seconds(), None); // never supplied, never readable
/// assert!(t.flags.contains(TransportFlags::HAS_BEATS));
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransportBuilder {
    transport: Transport,
}

impl TransportBuilder {
    /// A builder that has been told nothing: [`Transport::EMPTY`]. [any-thread]
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            transport: Transport::EMPTY,
        }
    }

    /// Starts from an existing transport, keeping its flags. [any-thread]
    #[inline]
    #[must_use]
    pub const fn from_transport(transport: Transport) -> Self {
        Self { transport }
    }

    /// Sets or clears [`TransportFlags::IS_PLAYING`]. [any-thread]
    #[inline]
    #[must_use]
    pub fn playing(mut self, on: bool) -> Self {
        self.transport.flags.set(TransportFlags::IS_PLAYING, on);
        self
    }

    /// Sets or clears [`TransportFlags::IS_RECORDING`]. [any-thread]
    #[inline]
    #[must_use]
    pub fn recording(mut self, on: bool) -> Self {
        self.transport.flags.set(TransportFlags::IS_RECORDING, on);
        self
    }

    /// Sets or clears [`TransportFlags::IS_LOOPING`]. [any-thread]
    ///
    /// Independent of [`TransportBuilder::loop_beats`]: a host may arm the loop without
    /// telling the plug-in where it is.
    #[inline]
    #[must_use]
    pub fn looping(mut self, on: bool) -> Self {
        self.transport.flags.set(TransportFlags::IS_LOOPING, on);
        self
    }

    /// Sets or clears [`TransportFlags::IS_PREROLL`]. [any-thread]
    #[inline]
    #[must_use]
    pub fn preroll(mut self, on: bool) -> Self {
        self.transport.flags.set(TransportFlags::IS_PREROLL, on);
        self
    }

    /// Sets the continuous sample position of the first frame of the block. [any-thread]
    ///
    /// This field has no `HAS_*` bit; it is always readable.
    #[inline]
    #[must_use]
    pub const fn sample_position(mut self, samples: i64) -> Self {
        self.transport.song_pos_samples = samples;
        self
    }

    /// Sets a steady tempo in BPM and sets [`TransportFlags::HAS_TEMPO`]. [any-thread]
    ///
    /// Ignored (flag left clear) unless `bpm` is finite and greater than zero.
    #[inline]
    #[must_use]
    pub fn tempo(self, bpm: f64) -> Self {
        self.tempo_ramp(bpm, 0.0)
    }

    /// Sets a ramping tempo — `bpm` at the first sample, changing by `increment` BPM per
    /// sample — and sets [`TransportFlags::HAS_TEMPO`]. [any-thread]
    ///
    /// Ignored (flag left clear) unless `bpm` is finite and greater than zero and
    /// `increment` is finite.
    #[inline]
    #[must_use]
    pub fn tempo_ramp(mut self, bpm: f64, increment: f64) -> Self {
        if bpm.is_finite() && bpm > 0.0 && increment.is_finite() {
            self.transport.tempo = bpm;
            self.transport.tempo_increment = increment;
            self.transport.flags.insert(TransportFlags::HAS_TEMPO);
        }
        self
    }

    /// Sets the musical position in quarter-note beats and sets
    /// [`TransportFlags::HAS_BEATS`]. [any-thread]
    ///
    /// Ignored unless `beats` is finite.
    #[inline]
    #[must_use]
    pub fn beats(mut self, beats: f64) -> Self {
        if beats.is_finite() {
            self.transport.song_pos_beats = beats;
            self.transport.flags.insert(TransportFlags::HAS_BEATS);
        }
        self
    }

    /// Sets the timeline position in seconds and sets [`TransportFlags::HAS_SECONDS`].
    /// [any-thread]
    ///
    /// Ignored unless `seconds` is finite.
    #[inline]
    #[must_use]
    pub fn seconds(mut self, seconds: f64) -> Self {
        if seconds.is_finite() {
            self.transport.song_pos_seconds = seconds;
            self.transport.flags.insert(TransportFlags::HAS_SECONDS);
        }
        self
    }

    /// Sets the time signature and sets [`TransportFlags::HAS_TIME_SIG`]. [any-thread]
    ///
    /// Ignored unless both terms are non-zero.
    #[inline]
    #[must_use]
    pub fn time_signature(mut self, numerator: u16, denominator: u16) -> Self {
        if let Some(sig) = TimeSignature::try_new(numerator, denominator) {
            self.transport.time_signature = sig;
            self.transport.flags.insert(TransportFlags::HAS_TIME_SIG);
        }
        self
    }

    /// Sets the current bar number and the beat position of its first beat, and sets
    /// [`TransportFlags::HAS_BAR`]. [any-thread]
    ///
    /// Ignored unless `start_beats` is finite.
    #[inline]
    #[must_use]
    pub fn bar(mut self, number: i32, start_beats: f64) -> Self {
        if start_beats.is_finite() {
            self.transport.bar_number = number;
            self.transport.bar_start_beats = start_beats;
            self.transport.flags.insert(TransportFlags::HAS_BAR);
        }
        self
    }

    /// Sets the loop region in quarter-note beats and sets [`TransportFlags::HAS_LOOP`].
    /// [any-thread]
    ///
    /// Ignored unless both bounds are finite. Hosts should also call
    /// [`TransportBuilder::loop_seconds`]; `HAS_LOOP` covers both pairs of fields.
    #[inline]
    #[must_use]
    pub fn loop_beats(mut self, start: f64, end: f64) -> Self {
        if start.is_finite() && end.is_finite() {
            self.transport.loop_start_beats = start;
            self.transport.loop_end_beats = end;
            self.transport.flags.insert(TransportFlags::HAS_LOOP);
        }
        self
    }

    /// Sets the loop region in seconds and sets [`TransportFlags::HAS_LOOP`]. [any-thread]
    ///
    /// Ignored unless both bounds are finite.
    #[inline]
    #[must_use]
    pub fn loop_seconds(mut self, start: f64, end: f64) -> Self {
        if start.is_finite() && end.is_finite() {
            self.transport.loop_start_seconds = start;
            self.transport.loop_end_seconds = end;
            self.transport.flags.insert(TransportFlags::HAS_LOOP);
        }
        self
    }

    /// Sets raw flag bits on top of everything the builder inferred. [any-thread]
    ///
    /// Escape hatch for ABI adapters that receive a flag word they must forward verbatim,
    /// including bits reserved by a future revision of the spec.
    #[inline]
    #[must_use]
    pub fn raw_flags(mut self, flags: TransportFlags) -> Self {
        self.transport.flags.insert(flags);
        self
    }

    /// Returns the finished transport. [any-thread]
    #[inline]
    #[must_use]
    pub const fn build(self) -> Transport {
        self.transport
    }
}

impl Default for TransportBuilder {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl From<TransportBuilder> for Transport {
    #[inline]
    fn from(builder: TransportBuilder) -> Self {
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_builder_sets_no_flags() {
        assert_eq!(TransportBuilder::new().build(), Transport::EMPTY);
        assert_eq!(
            TransportBuilder::default().build().flags,
            TransportFlags::NONE
        );
    }

    #[test]
    fn each_setter_raises_exactly_its_own_flag() {
        assert_eq!(
            TransportBuilder::new().tempo(120.0).build().flags,
            TransportFlags::HAS_TEMPO
        );
        assert_eq!(
            TransportBuilder::new().beats(1.0).build().flags,
            TransportFlags::HAS_BEATS
        );
        assert_eq!(
            TransportBuilder::new().seconds(1.0).build().flags,
            TransportFlags::HAS_SECONDS
        );
        assert_eq!(
            TransportBuilder::new().time_signature(3, 4).build().flags,
            TransportFlags::HAS_TIME_SIG
        );
        assert_eq!(
            TransportBuilder::new().bar(1, 0.0).build().flags,
            TransportFlags::HAS_BAR
        );
        assert_eq!(
            TransportBuilder::new().loop_beats(0.0, 4.0).build().flags,
            TransportFlags::HAS_LOOP
        );
        assert_eq!(
            TransportBuilder::new().loop_seconds(0.0, 2.0).build().flags,
            TransportFlags::HAS_LOOP
        );
        // Sample position is always readable, so it raises nothing.
        assert_eq!(
            TransportBuilder::new().sample_position(99).build().flags,
            TransportFlags::NONE
        );
    }

    #[test]
    fn status_bits_toggle_both_ways() {
        let t = TransportBuilder::new()
            .playing(true)
            .recording(true)
            .looping(true)
            .preroll(true)
            .build();
        assert!(t.is_playing() && t.is_recording() && t.is_looping() && t.is_preroll());

        let t = TransportBuilder::from_transport(t)
            .playing(false)
            .recording(false)
            .looping(false)
            .preroll(false)
            .build();
        assert!(!t.is_playing() && !t.is_recording() && !t.is_looping() && !t.is_preroll());
        assert_eq!(t.flags, TransportFlags::NONE);
    }

    #[test]
    fn unusable_values_leave_the_flag_clear() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                TransportBuilder::new().tempo(bad).build().tempo(),
                None,
                "tempo {bad}"
            );
        }
        assert_eq!(
            TransportBuilder::new()
                .tempo_ramp(120.0, f64::NAN)
                .build()
                .tempo(),
            None
        );
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(TransportBuilder::new().beats(bad).build().beats(), None);
            assert_eq!(TransportBuilder::new().seconds(bad).build().seconds(), None);
            assert_eq!(
                TransportBuilder::new().bar(1, bad).build().bar_number(),
                None
            );
            assert_eq!(
                TransportBuilder::new()
                    .loop_beats(0.0, bad)
                    .build()
                    .loop_range_beats(),
                None
            );
            assert_eq!(
                TransportBuilder::new()
                    .loop_seconds(bad, 0.0)
                    .build()
                    .loop_range_seconds(),
                None
            );
        }
        assert_eq!(
            TransportBuilder::new()
                .time_signature(0, 4)
                .build()
                .time_signature(),
            None
        );
        assert_eq!(
            TransportBuilder::new()
                .time_signature(4, 0)
                .build()
                .time_signature(),
            None
        );
    }

    #[test]
    fn from_transport_preserves_and_extends() {
        let base = TransportBuilder::new().tempo(90.0).build();
        let extended = TransportBuilder::from_transport(base).beats(2.0).build();
        assert_eq!(extended.tempo(), Some(90.0));
        assert_eq!(extended.beats(), Some(2.0));
    }

    #[test]
    fn raw_flags_forwards_reserved_bits() {
        let reserved = TransportFlags::from_bits(1 << 31);
        let t = TransportBuilder::new()
            .tempo(120.0)
            .raw_flags(reserved)
            .build();
        assert_eq!(t.flags.unknown_bits(), 1 << 31);
        assert_eq!(t.tempo(), Some(120.0));
    }

    #[test]
    fn into_transport_works() {
        let t: Transport = TransportBuilder::new().tempo(60.0).into();
        assert_eq!(t.tempo(), Some(60.0));
    }

    #[test]
    fn last_write_wins() {
        let t = TransportBuilder::new().tempo(120.0).tempo(140.0).build();
        assert_eq!(t.tempo(), Some(140.0));
        // …but a rejected value never clobbers an accepted one's flag.
        let t = TransportBuilder::new().tempo(120.0).tempo(-1.0).build();
        assert_eq!(t.tempo(), Some(120.0));
    }
}
