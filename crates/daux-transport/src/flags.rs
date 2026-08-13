//! Transport capability / status bits.

use core::fmt;
use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign};

/// Bit set describing which transport fields the host actually filled in, plus the
/// play / record / loop status of the timeline. [any-thread]
///
/// The bit values mirror the `DAUX_TRANSPORT_*` constants of
/// `docs/specifications/abi-v1.md` §10 exactly, so
/// `TransportFlags::from_bits(raw.flags)` is a lossless conversion from the C ABI struct.
///
/// A `HAS_*` bit is the host's promise that the corresponding field is meaningful.
/// A plug-in must not read a field whose bit is clear; the accessors on
/// [`Transport`](crate::Transport) enforce that by returning `None`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TransportFlags(u32);

impl TransportFlags {
    /// The host provided nothing at all: every optional accessor returns `None`.
    pub const NONE: Self = Self(0);
    /// `tempo` and `tempo_increment` are meaningful. Mirrors `DAUX_TRANSPORT_HAS_TEMPO`.
    pub const HAS_TEMPO: Self = Self(1 << 0);
    /// `song_pos_beats` is meaningful. Mirrors `DAUX_TRANSPORT_HAS_BEATS`.
    pub const HAS_BEATS: Self = Self(1 << 1);
    /// `song_pos_seconds` is meaningful. Mirrors `DAUX_TRANSPORT_HAS_SECONDS`.
    pub const HAS_SECONDS: Self = Self(1 << 2);
    /// `time_signature` is meaningful. Mirrors `DAUX_TRANSPORT_HAS_TIME_SIG`.
    pub const HAS_TIME_SIG: Self = Self(1 << 3);
    /// The four `loop_*` fields are meaningful. Mirrors `DAUX_TRANSPORT_HAS_LOOP`.
    pub const HAS_LOOP: Self = Self(1 << 4);
    /// `bar_start_beats` and `bar_number` are meaningful. Mirrors `DAUX_TRANSPORT_HAS_BAR`.
    pub const HAS_BAR: Self = Self(1 << 5);
    /// The timeline is rolling. Mirrors `DAUX_TRANSPORT_IS_PLAYING`.
    pub const IS_PLAYING: Self = Self(1 << 6);
    /// The host is recording. Mirrors `DAUX_TRANSPORT_IS_RECORDING`.
    pub const IS_RECORDING: Self = Self(1 << 7);
    /// Loop playback is armed. Mirrors `DAUX_TRANSPORT_IS_LOOPING`.
    pub const IS_LOOPING: Self = Self(1 << 8);
    /// The block is pre-roll (count-in) and precedes the recorded region.
    /// Mirrors `DAUX_TRANSPORT_IS_PREROLL`.
    pub const IS_PREROLL: Self = Self(1 << 9);

    /// Every bit defined by ABI v1. Bits outside this mask are reserved.
    pub const ALL: Self = Self(0b11_1111_1111);

    /// Names of the defined bits, least significant first. Used by [`fmt::Debug`].
    const NAMES: [&'static str; 10] = [
        "HAS_TEMPO",
        "HAS_BEATS",
        "HAS_SECONDS",
        "HAS_TIME_SIG",
        "HAS_LOOP",
        "HAS_BAR",
        "IS_PLAYING",
        "IS_RECORDING",
        "IS_LOOPING",
        "IS_PREROLL",
    ];

    /// Wraps a raw `DAUX_TRANSPORT_*` bit pattern, preserving unknown/reserved bits. [any-thread]
    #[inline]
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// The raw bit pattern, suitable for writing back into `DauxTransportV1::flags`. [any-thread]
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// `true` when **every** bit of `other` is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// `true` when **any** bit of `other` is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// `true` when no bit at all is set. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The union of two flag sets. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The intersection of two flag sets. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// `self` with every bit of `other` cleared. [audio-thread]
    #[inline]
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Sets every bit of `other`. [audio-thread]
    #[inline]
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Clears every bit of `other`. [audio-thread]
    #[inline]
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// Sets or clears every bit of `other` depending on `on`. [audio-thread]
    #[inline]
    pub fn set(&mut self, other: Self, on: bool) {
        if on {
            self.insert(other);
        } else {
            self.remove(other);
        }
    }

    /// The bits that are set but not defined by ABI v1. A non-zero result means the
    /// host speaks a newer revision; it is informational, never an error. [any-thread]
    #[inline]
    #[must_use]
    pub const fn unknown_bits(self) -> u32 {
        self.0 & !Self::ALL.0
    }
}

impl BitOr for TransportFlags {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for TransportFlags {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.insert(rhs);
    }
}

impl BitAnd for TransportFlags {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

impl BitAndAssign for TransportFlags {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl fmt::Debug for TransportFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TransportFlags(")?;
        let mut first = true;
        for (i, name) in Self::NAMES.iter().enumerate() {
            if self.0 & (1 << i) != 0 {
                if !first {
                    f.write_str(" | ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        let unknown = self.unknown_bits();
        if unknown != 0 {
            if !first {
                f.write_str(" | ")?;
            }
            write!(f, "{unknown:#x}")?;
            first = false;
        }
        if first {
            f.write_str("NONE")?;
        }
        f.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_values_match_abi_v1() {
        // docs/specifications/abi-v1.md §10
        assert_eq!(TransportFlags::HAS_TEMPO.bits(), 1 << 0);
        assert_eq!(TransportFlags::HAS_BEATS.bits(), 1 << 1);
        assert_eq!(TransportFlags::HAS_SECONDS.bits(), 1 << 2);
        assert_eq!(TransportFlags::HAS_TIME_SIG.bits(), 1 << 3);
        assert_eq!(TransportFlags::HAS_LOOP.bits(), 1 << 4);
        assert_eq!(TransportFlags::HAS_BAR.bits(), 1 << 5);
        assert_eq!(TransportFlags::IS_PLAYING.bits(), 1 << 6);
        assert_eq!(TransportFlags::IS_RECORDING.bits(), 1 << 7);
        assert_eq!(TransportFlags::IS_LOOPING.bits(), 1 << 8);
        assert_eq!(TransportFlags::IS_PREROLL.bits(), 1 << 9);
        assert_eq!(TransportFlags::ALL.bits(), 0x3ff);
    }

    #[test]
    fn round_trips_through_raw_bits() {
        for raw in [0u32, 1, 0x3ff, 0xffff_ffff, 0x8000_0001] {
            assert_eq!(TransportFlags::from_bits(raw).bits(), raw);
        }
    }

    #[test]
    fn set_algebra() {
        let mut f = TransportFlags::NONE;
        assert!(f.is_empty());
        f.insert(TransportFlags::HAS_TEMPO | TransportFlags::IS_PLAYING);
        assert!(f.contains(TransportFlags::HAS_TEMPO));
        assert!(f.contains(TransportFlags::IS_PLAYING));
        assert!(!f.contains(TransportFlags::HAS_BEATS));
        assert!(f.intersects(TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS));
        assert!(!f.contains(TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS));

        f.remove(TransportFlags::IS_PLAYING);
        assert_eq!(f, TransportFlags::HAS_TEMPO);
        f.set(TransportFlags::HAS_BEATS, true);
        assert_eq!(f, TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS);
        f.set(TransportFlags::HAS_BEATS, false);
        assert_eq!(f, TransportFlags::HAS_TEMPO);

        assert_eq!(
            (TransportFlags::ALL & TransportFlags::HAS_LOOP),
            TransportFlags::HAS_LOOP
        );
        assert_eq!(
            TransportFlags::ALL.difference(TransportFlags::ALL),
            TransportFlags::NONE
        );

        let mut g = TransportFlags::ALL;
        g &= TransportFlags::HAS_BAR;
        assert_eq!(g, TransportFlags::HAS_BAR);

        let mut h = TransportFlags::NONE;
        h |= TransportFlags::IS_PREROLL;
        assert_eq!(h, TransportFlags::IS_PREROLL);
    }

    #[test]
    fn contains_empty_is_always_true() {
        assert!(TransportFlags::NONE.contains(TransportFlags::NONE));
        assert!(!TransportFlags::NONE.intersects(TransportFlags::NONE));
    }

    #[test]
    fn unknown_bits_are_reported_not_rejected() {
        let f = TransportFlags::from_bits(0x3ff | (1 << 20));
        assert_eq!(f.unknown_bits(), 1 << 20);
        assert!(f.contains(TransportFlags::ALL));
        assert_eq!(TransportFlags::ALL.unknown_bits(), 0);
    }

    #[test]
    fn debug_lists_names() {
        assert_eq!(
            format!("{:?}", TransportFlags::NONE),
            "TransportFlags(NONE)"
        );
        assert_eq!(
            format!(
                "{:?}",
                TransportFlags::HAS_TEMPO | TransportFlags::IS_PLAYING
            ),
            "TransportFlags(HAS_TEMPO | IS_PLAYING)"
        );
        assert_eq!(
            format!("{:?}", TransportFlags::from_bits(1 << 20)),
            "TransportFlags(0x100000)"
        );
    }
}
