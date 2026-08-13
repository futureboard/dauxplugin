//! Sample types and sample formats.
//!
//! DAUx processes audio as `f32` or `f64` and nothing else; the [`Sample`] trait is sealed
//! so that a plug-in cannot introduce a third representation that the ABI has no code for
//! (`abi-v1` §6.3).

use core::fmt;
use core::ops::{BitAnd, BitOr, BitOrAssign};

/// Implementation detail that seals [`Sample`]; not nameable from other crates.
#[doc(hidden)]
pub mod sealed {
    /// Sealing marker implemented for `f32` and `f64` only.
    pub trait Sealed {}
    impl Sealed for f32 {}
    impl Sealed for f64 {}
}

/// The sample representation of a buffer. `[any-thread]`
///
/// Sealed: only `f32` and `f64` implement it, matching `DAUX_SAMPLE_FORMAT_*`. Every method
/// is a total function on all inputs (including infinities and NaN) and never allocates,
/// locks or panics, so the whole trait is callable from the audio thread.
pub trait Sample:
    sealed::Sealed + Copy + PartialEq + PartialOrd + fmt::Debug + Send + Sync + 'static
{
    /// Digital silence for this representation. `[audio-thread]`
    const ZERO: Self;
    /// The [`SampleFormat`] discriminant that describes this type. `[audio-thread]`
    const FORMAT: SampleFormat;
    /// Number of bytes one sample occupies in a buffer. `[audio-thread]`
    const BYTES: usize;

    /// Converts from `f64`, rounding to the nearest representable value. `[audio-thread]`
    fn from_f64(v: f64) -> Self;
    /// Widens to `f64` without loss. `[audio-thread]`
    fn to_f64(self) -> f64;
    /// Converts from `f32` without loss. `[audio-thread]`
    fn from_f32(v: f32) -> Self;
    /// Converts to `f32`, rounding to the nearest representable value. `[audio-thread]`
    fn to_f32(self) -> f32;

    /// `true` when the value is exactly `+0.0` or `-0.0`. `[audio-thread]`
    #[inline]
    fn is_silent(self) -> bool {
        self == Self::ZERO
    }
}

impl Sample for f32 {
    const ZERO: Self = 0.0;
    const FORMAT: SampleFormat = SampleFormat::F32;
    const BYTES: usize = core::mem::size_of::<Self>();

    #[inline]
    fn from_f64(v: f64) -> Self {
        v as Self
    }
    #[inline]
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
    #[inline]
    fn from_f32(v: f32) -> Self {
        v
    }
    #[inline]
    fn to_f32(self) -> f32 {
        self
    }
}

impl Sample for f64 {
    const ZERO: Self = 0.0;
    const FORMAT: SampleFormat = SampleFormat::F64;
    const BYTES: usize = core::mem::size_of::<Self>();

    #[inline]
    fn from_f64(v: f64) -> Self {
        v
    }
    #[inline]
    fn to_f64(self) -> f64 {
        self
    }
    #[inline]
    fn from_f32(v: f32) -> Self {
        Self::from(v)
    }
    #[inline]
    fn to_f32(self) -> f32 {
        self as f32
    }
}

/// Which sample representation a buffer or a processing configuration uses.
///
/// The discriminants deliberately do **not** match the ABI values; use [`as_bits`] and
/// [`from_bits`] to cross the boundary. `[any-thread]`
///
/// [`as_bits`]: SampleFormat::as_bits
/// [`from_bits`]: SampleFormat::from_bits
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum SampleFormat {
    /// 32-bit float, `DAUX_SAMPLE_FORMAT_F32`. The default everywhere.
    #[default]
    F32,
    /// 64-bit float, `DAUX_SAMPLE_FORMAT_F64`.
    F64,
}

impl SampleFormat {
    /// `DAUX_SAMPLE_FORMAT_F32` (`1 << 0`).
    pub const F32_BIT: u32 = 1 << 0;
    /// `DAUX_SAMPLE_FORMAT_F64` (`1 << 1`).
    pub const F64_BIT: u32 = 1 << 1;

    /// The single `DAUX_SAMPLE_FORMAT_*` bit for this format. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn as_bits(self) -> u32 {
        match self {
            Self::F32 => Self::F32_BIT,
            Self::F64 => Self::F64_BIT,
        }
    }

    /// Parses a `DAUX_SAMPLE_FORMAT_*` value. `[any-thread]`
    ///
    /// Returns `None` unless exactly one known bit is set, so a hostile or newer host
    /// cannot smuggle an unknown format past this function.
    #[inline]
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            Self::F32_BIT => Some(Self::F32),
            Self::F64_BIT => Some(Self::F64),
            _ => None,
        }
    }

    /// Bytes occupied by one sample. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::F32 => core::mem::size_of::<f32>(),
            Self::F64 => core::mem::size_of::<f64>(),
        }
    }

    /// Short stable name, useful for logs and CLI output. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of [`SampleFormat`]s, matching the `DAUX_SAMPLE_FORMAT_*` bitset a plug-in
/// advertises. `[any-thread]`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SampleFormats(u32);

impl SampleFormats {
    /// The empty set. A plug-in must never advertise this.
    pub const NONE: Self = Self(0);
    /// `f32` only — the DAUx baseline every plug-in must support.
    pub const F32: Self = Self(SampleFormat::F32_BIT);
    /// `f64` only.
    pub const F64: Self = Self(SampleFormat::F64_BIT);
    /// Both representations.
    pub const BOTH: Self = Self(SampleFormat::F32_BIT | SampleFormat::F64_BIT);

    /// Raw `DAUX_SAMPLE_FORMAT_*` bitset. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Builds a set from raw bits, silently dropping bits this ABI version does not
    /// define. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::BOTH.0)
    }

    /// `true` when `format` is in the set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn contains(self, format: SampleFormat) -> bool {
        self.0 & format.as_bits() != 0
    }

    /// Returns the set with `format` added. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn with(self, format: SampleFormat) -> Self {
        Self(self.0 | format.as_bits())
    }

    /// `true` when no format is set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for SampleFormats {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for SampleFormats {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for SampleFormats {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl From<SampleFormat> for SampleFormats {
    #[inline]
    fn from(value: SampleFormat) -> Self {
        Self(value.as_bits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_round_trips() {
        assert_eq!(f32::ZERO, 0.0);
        assert_eq!(f32::FORMAT, SampleFormat::F32);
        assert_eq!(f32::BYTES, 4);
        assert_eq!(<f32 as Sample>::from_f32(0.5), 0.5);
        assert_eq!(<f32 as Sample>::from_f64(0.5), 0.5);
        assert_eq!(Sample::to_f64(0.5f32), 0.5f64);
        assert_eq!(Sample::to_f32(0.5f32), 0.5f32);
    }

    #[test]
    fn f64_round_trips() {
        assert_eq!(f64::ZERO, 0.0);
        assert_eq!(f64::FORMAT, SampleFormat::F64);
        assert_eq!(f64::BYTES, 8);
        assert_eq!(<f64 as Sample>::from_f32(0.25), 0.25);
        assert_eq!(Sample::to_f32(0.25f64), 0.25f32);
    }

    #[test]
    fn narrowing_is_total_on_extremes() {
        // No panic, no UB: saturating float casts are guaranteed since Rust 1.45.
        assert!(Sample::to_f32(f64::MAX).is_infinite());
        assert!(<f32 as Sample>::from_f64(f64::MIN).is_infinite());
        assert!(<f32 as Sample>::from_f64(f64::NAN).is_nan());
        assert_eq!(<f32 as Sample>::from_f64(-0.0), 0.0);
        assert!(<f32 as Sample>::from_f64(-0.0).is_sign_negative());
    }

    #[test]
    fn silence_predicate_treats_both_zeroes_as_silent() {
        assert!(Sample::is_silent(0.0f32));
        assert!(Sample::is_silent(-0.0f32));
        assert!(!Sample::is_silent(f32::MIN_POSITIVE));
        assert!(!Sample::is_silent(f32::NAN));
        assert!(Sample::is_silent(-0.0f64));
    }

    #[test]
    fn format_bits_match_the_abi() {
        assert_eq!(SampleFormat::F32.as_bits(), 1);
        assert_eq!(SampleFormat::F64.as_bits(), 2);
        assert_eq!(SampleFormat::from_bits(1), Some(SampleFormat::F32));
        assert_eq!(SampleFormat::from_bits(2), Some(SampleFormat::F64));
        // Zero, unknown, and "both bits at once" are all rejected.
        assert_eq!(SampleFormat::from_bits(0), None);
        assert_eq!(SampleFormat::from_bits(3), None);
        assert_eq!(SampleFormat::from_bits(u32::MAX), None);
        assert_eq!(SampleFormat::default(), SampleFormat::F32);
        assert_eq!(SampleFormat::F64.bytes_per_sample(), 8);
        assert_eq!(SampleFormat::F32.to_string(), "f32");
    }

    #[test]
    fn format_set_arithmetic() {
        let mut s = SampleFormats::NONE;
        assert!(s.is_empty());
        assert!(!s.contains(SampleFormat::F32));
        s |= SampleFormats::F32;
        assert!(s.contains(SampleFormat::F32));
        assert!(!s.contains(SampleFormat::F64));
        let both = s | SampleFormats::F64;
        assert_eq!(both, SampleFormats::BOTH);
        assert_eq!(both.bits(), 0b11);
        assert_eq!(both & SampleFormats::F64, SampleFormats::F64);
        assert_eq!(SampleFormats::from(SampleFormat::F64), SampleFormats::F64);
        assert_eq!(
            SampleFormats::from_bits_truncate(u32::MAX),
            SampleFormats::BOTH
        );
        assert_eq!(
            SampleFormats::NONE.with(SampleFormat::F32),
            SampleFormats::F32
        );
    }
}
