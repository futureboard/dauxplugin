//! What a host has to re-read after a plug-in changed its parameter model.

use core::fmt;
use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign};

/// Which parts of the parameter model the host must re-read. `[main-thread]`
///
/// Handed to [`HostParams::rescan`](crate::HostParams::rescan) after a plug-in
/// changes something a host has already cached — a display string, a range, or
/// the parameter list itself.
///
/// # Relationship to the ABI
///
/// `DauxHostParamsApiV1::rescan` takes a `u32` bitset whose meaning ABI v1.0
/// leaves **reserved**: it defines no `DAUX_PARAM_RESCAN_*` constants, a plug-in
/// may pass `0`, and a host must treat any non-zero value as "rescan
/// everything". The bits below are therefore a DAUx-side refinement that lets a
/// plug-in state its intent precisely, that maps losslessly onto richer host
/// APIs (VST3's `restartComponent`, CLAP's `clap_host_params::rescan`), and that
/// still degrades correctly across the C ABI, where any non-empty set is simply
/// non-zero.
///
/// ```
/// use daux_host_services::RescanFlags;
///
/// let flags = RescanFlags::VALUES | RescanFlags::TEXT;
/// assert!(flags.contains(RescanFlags::VALUES));
/// assert!(!flags.contains(RescanFlags::INFO));
/// assert!(!flags.is_empty());
/// assert_ne!(flags.bits(), 0, "any non-empty set is non-zero across the ABI");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RescanFlags(u32);

impl RescanFlags {
    /// Nothing to do. Sending this is legal but pointless.
    pub const NONE: Self = Self(0);
    /// One or more parameter **values** changed behind the host's back, so every
    /// cached value is stale.
    pub const VALUES: Self = Self(1 << 0);
    /// Display **text** changed — a unit, a formatter, an enum label — while
    /// values and ranges stayed put.
    pub const TEXT: Self = Self(1 << 1);
    /// Parameter **metadata** changed: name, group, range, default, step count
    /// or flags. Ids and ordering are unaffected.
    pub const INFO: Self = Self(1 << 2);
    /// The parameter **list** itself changed: parameters appeared, disappeared
    /// or moved. This is the heavy one — most hosts respond by re-scanning the
    /// whole plug-in, and some require a restart.
    pub const LIST: Self = Self(1 << 3);
    /// Every defined bit.
    pub const ALL: Self = Self(0b1111);

    /// Names of the defined bits, least significant first.
    const NAMES: [&'static str; 4] = ["VALUES", "TEXT", "INFO", "LIST"];

    /// Wraps a raw bit pattern, preserving bits this version does not define.
    /// `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// The raw bit pattern to write into `DauxHostParamsApiV1::rescan`.
    /// `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// `true` when **every** bit of `other` is set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// `true` when **any** bit of `other` is set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// `true` when no bit is set. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The union of two sets. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The intersection of two sets. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// `self` with every bit of `other` cleared. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Bits that are set but not defined here — a plug-in built against a newer
    /// SDK. Informational, never an error. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn unknown_bits(self) -> u32 {
        self.0 & !Self::ALL.0
    }
}

impl BitOr for RescanFlags {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for RescanFlags {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for RescanFlags {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

impl BitAndAssign for RescanFlags {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl fmt::Debug for RescanFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RescanFlags(")?;
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
    fn the_empty_set_is_the_only_zero() {
        assert!(RescanFlags::NONE.is_empty());
        assert_eq!(RescanFlags::NONE.bits(), 0);
        assert_eq!(RescanFlags::default(), RescanFlags::NONE);
        for flag in [
            RescanFlags::VALUES,
            RescanFlags::TEXT,
            RescanFlags::INFO,
            RescanFlags::LIST,
        ] {
            assert!(!flag.is_empty());
            assert_ne!(flag.bits(), 0);
        }
    }

    #[test]
    fn the_bits_are_distinct_and_all_covers_them() {
        let each = [
            RescanFlags::VALUES,
            RescanFlags::TEXT,
            RescanFlags::INFO,
            RescanFlags::LIST,
        ];
        let mut union = RescanFlags::NONE;
        for flag in each {
            assert!(!union.intersects(flag), "bit {flag:?} is not distinct");
            union |= flag;
        }
        assert_eq!(union, RescanFlags::ALL);
        assert_eq!(RescanFlags::ALL.bits(), 0b1111);
    }

    #[test]
    fn set_algebra() {
        let f = RescanFlags::VALUES | RescanFlags::TEXT;
        assert!(f.contains(RescanFlags::VALUES));
        assert!(f.contains(RescanFlags::VALUES | RescanFlags::TEXT));
        assert!(!f.contains(RescanFlags::ALL));
        assert!(f.intersects(RescanFlags::TEXT | RescanFlags::LIST));
        assert_eq!(f.without(RescanFlags::TEXT), RescanFlags::VALUES);
        assert_eq!(f & RescanFlags::ALL, f);

        let mut g = RescanFlags::ALL;
        g &= RescanFlags::INFO;
        assert_eq!(g, RescanFlags::INFO);
        // `contains` of the empty set is vacuously true; `intersects` is not.
        assert!(f.contains(RescanFlags::NONE));
        assert!(!f.intersects(RescanFlags::NONE));
    }

    #[test]
    fn unknown_bits_survive_a_round_trip_and_are_reported() {
        let raw = RescanFlags::ALL.bits() | (1 << 20);
        let f = RescanFlags::from_bits(raw);
        assert_eq!(f.bits(), raw);
        assert_eq!(f.unknown_bits(), 1 << 20);
        assert!(f.contains(RescanFlags::ALL));
        assert_eq!(RescanFlags::ALL.unknown_bits(), 0);
    }

    #[test]
    fn debug_lists_names_and_leftovers() {
        assert_eq!(format!("{:?}", RescanFlags::NONE), "RescanFlags(NONE)");
        assert_eq!(
            format!("{:?}", RescanFlags::VALUES | RescanFlags::LIST),
            "RescanFlags(VALUES | LIST)"
        );
        assert_eq!(
            format!("{:?}", RescanFlags::from_bits(1 << 20)),
            "RescanFlags(0x100000)"
        );
        assert_eq!(
            format!("{:?}", RescanFlags::from_bits(1 | (1 << 20))),
            "RescanFlags(VALUES | 0x100000)"
        );
    }
}
