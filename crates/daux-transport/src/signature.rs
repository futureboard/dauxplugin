//! Musical time signature.

use core::fmt;

/// A musical time signature, e.g. 7/8. [any-thread]
///
/// Mirrors `DauxTransportV1::time_sig_numerator` / `time_sig_denominator`
/// (`docs/specifications/abi-v1.md` §10). The value is only meaningful when the host set
/// [`TransportFlags::HAS_TIME_SIG`](crate::TransportFlags::HAS_TIME_SIG); use
/// [`Transport::time_signature`](crate::Transport::time_signature) rather than reading the
/// field directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimeSignature {
    /// Beats per bar — the upper number. Zero means "not a valid signature".
    pub numerator: u16,
    /// The note value that counts as one beat — the lower number. Conventionally a power
    /// of two (1, 2, 4, 8, 16, …); zero means "not a valid signature".
    pub denominator: u16,
}

impl TimeSignature {
    /// Common time, 4/4.
    pub const COMMON: Self = Self {
        numerator: 4,
        denominator: 4,
    };

    /// Builds a signature without validating it. [any-thread]
    ///
    /// Use [`TimeSignature::try_new`] when the numbers come from outside the process.
    #[inline]
    #[must_use]
    pub const fn new(numerator: u16, denominator: u16) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// Builds a signature, returning `None` when either number is zero. [any-thread]
    #[inline]
    #[must_use]
    pub const fn try_new(numerator: u16, denominator: u16) -> Option<Self> {
        if numerator == 0 || denominator == 0 {
            None
        } else {
            Some(Self::new(numerator, denominator))
        }
    }

    /// `true` when both numbers are non-zero. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.numerator != 0 && self.denominator != 0
    }

    /// Length of one bar in quarter-note beats — the unit `song_pos_beats` is measured in.
    /// [audio-thread]
    ///
    /// `numerator * 4 / denominator`; for example 7/8 is 3.5 quarter notes.
    /// Returns `None` for an invalid signature.
    #[inline]
    #[must_use]
    pub fn quarter_notes_per_bar(self) -> Option<f64> {
        if self.is_valid() {
            Some(f64::from(self.numerator) * 4.0 / f64::from(self.denominator))
        } else {
            None
        }
    }

    /// Length of one *beat* of this signature in quarter notes. [audio-thread]
    ///
    /// `4 / denominator`; an eighth-note beat is 0.5 quarter notes.
    /// Returns `None` for an invalid signature.
    #[inline]
    #[must_use]
    pub fn quarter_notes_per_beat(self) -> Option<f64> {
        if self.is_valid() {
            Some(4.0 / f64::from(self.denominator))
        } else {
            None
        }
    }
}

impl Default for TimeSignature {
    /// 4/4. Note that a default-constructed [`Transport`](crate::Transport) still reports
    /// `None` from [`Transport::time_signature`](crate::Transport::time_signature),
    /// because the `HAS_TIME_SIG` flag is clear.
    #[inline]
    fn default() -> Self {
        Self::COMMON
    }
}

impl fmt::Display for TimeSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_common_time() {
        assert_eq!(TimeSignature::default(), TimeSignature::new(4, 4));
        assert_eq!(TimeSignature::COMMON.to_string(), "4/4");
    }

    #[test]
    fn validity() {
        assert!(TimeSignature::new(7, 8).is_valid());
        assert!(!TimeSignature::new(0, 4).is_valid());
        assert!(!TimeSignature::new(4, 0).is_valid());
        assert!(!TimeSignature::new(0, 0).is_valid());
        assert_eq!(TimeSignature::try_new(0, 4), None);
        assert_eq!(TimeSignature::try_new(4, 0), None);
        assert_eq!(TimeSignature::try_new(3, 4), Some(TimeSignature::new(3, 4)));
    }

    #[test]
    fn bar_lengths_in_quarter_notes() {
        assert_eq!(TimeSignature::new(4, 4).quarter_notes_per_bar(), Some(4.0));
        assert_eq!(TimeSignature::new(3, 4).quarter_notes_per_bar(), Some(3.0));
        assert_eq!(TimeSignature::new(7, 8).quarter_notes_per_bar(), Some(3.5));
        assert_eq!(TimeSignature::new(6, 8).quarter_notes_per_bar(), Some(3.0));
        assert_eq!(TimeSignature::new(2, 2).quarter_notes_per_bar(), Some(4.0));
        assert_eq!(TimeSignature::new(4, 0).quarter_notes_per_bar(), None);
    }

    #[test]
    fn beat_lengths_in_quarter_notes() {
        assert_eq!(TimeSignature::new(4, 4).quarter_notes_per_beat(), Some(1.0));
        assert_eq!(TimeSignature::new(7, 8).quarter_notes_per_beat(), Some(0.5));
        assert_eq!(
            TimeSignature::new(5, 16).quarter_notes_per_beat(),
            Some(0.25)
        );
        assert_eq!(TimeSignature::new(0, 0).quarter_notes_per_beat(), None);
    }

    #[test]
    fn display_uses_slash() {
        assert_eq!(TimeSignature::new(13, 16).to_string(), "13/16");
    }

    #[test]
    fn extreme_values_do_not_panic() {
        let sig = TimeSignature::new(u16::MAX, u16::MAX);
        assert!(sig.is_valid());
        assert!(sig.quarter_notes_per_bar().is_some_and(f64::is_finite));
    }
}
