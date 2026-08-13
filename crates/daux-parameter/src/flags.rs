//! Parameter behaviour flags.

use core::fmt;
use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not, Sub, SubAssign};

/// Bit set describing what a host may do with a parameter.
///
/// The bit values are a literal mirror of `DAUX_PARAM_FLAG_*` in
/// `docs/specifications/abi-v1.md` §11.2, so [`ParamFlags::bits`] can be written
/// straight into `DauxParamInfoV1::flags` and [`ParamFlags::from_bits_truncate`] can
/// read it back.
///
/// `[any-thread]` — a `Copy` bit set; every method is `const` and allocation-free.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ParamFlags(u32);

impl ParamFlags {
    /// No flags. The parameter is visible but the host may neither automate nor
    /// modulate it.
    pub const EMPTY: Self = Self(0);
    /// The host may record and play back automation for this parameter.
    pub const AUTOMATABLE: Self = Self(1 << 0);
    /// The parameter accepts modulation offsets (`ParamMod` events) on top of its value.
    pub const MODULATABLE: Self = Self(1 << 1);
    /// The parameter can be addressed per note (polyphonic modulation).
    pub const PER_NOTE: Self = Self(1 << 2);
    /// The parameter is discrete; `step_count` in [`ParamInfo`](crate::ParamInfo) says
    /// how many intervals it has.
    pub const STEPPED: Self = Self(1 << 3);
    /// The host must not write this parameter; the plug-in owns the value.
    pub const READ_ONLY: Self = Self(1 << 4);
    /// The parameter exists but should not be shown in generic UIs.
    pub const HIDDEN: Self = Self(1 << 5);
    /// This is the plug-in's bypass parameter (at most one per plug-in).
    pub const BYPASS: Self = Self(1 << 6);
    /// Changing the value only takes effect through `process`, so the host must keep
    /// calling it while the parameter moves.
    pub const REQUIRES_PROCESS: Self = Self(1 << 7);
    /// The parameter is an output meter: written by the plug-in, polled by the UI.
    pub const IS_METER: Self = Self(1 << 8);

    /// Every flag defined by ABI v1; used to mask unknown bits.
    pub const ALL: Self = Self(
        Self::AUTOMATABLE.0
            | Self::MODULATABLE.0
            | Self::PER_NOTE.0
            | Self::STEPPED.0
            | Self::READ_ONLY.0
            | Self::HIDDEN.0
            | Self::BYPASS.0
            | Self::REQUIRES_PROCESS.0
            | Self::IS_METER.0,
    );

    /// The flags a plain, host-visible control gets unless the author says otherwise.
    pub const DEFAULT: Self = Self::AUTOMATABLE;

    /// The flags a meter gets: owned by the plug-in, never automated.
    pub const METER_DEFAULT: Self = Self(Self::READ_ONLY.0 | Self::IS_METER.0);

    /// `[any-thread]` Raw bits, ready for `DauxParamInfoV1::flags`.
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// `[any-thread]` Reads raw ABI bits, dropping any bit this version does not know.
    ///
    /// Unknown bits are dropped rather than rejected so that a newer host cannot make
    /// an older plug-in fail; the plug-in simply ignores what it does not understand.
    #[inline]
    #[must_use]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// `[any-thread]` True when no flag is set.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `[any-thread]` True when **every** flag in `other` is set.
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// `[any-thread]` True when **any** flag in `other` is set.
    #[inline]
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    /// `[any-thread]` Returns `self` with the flags of `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `[any-thread]` Returns `self` with the flags of `other` removed.
    #[inline]
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// `[main-thread]` Adds the flags of `other` in place.
    #[inline]
    pub const fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// `[main-thread]` Removes the flags of `other` in place.
    #[inline]
    pub const fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// `[any-thread]` Convenience for `contains(AUTOMATABLE)`.
    #[inline]
    #[must_use]
    pub const fn is_automatable(self) -> bool {
        self.contains(Self::AUTOMATABLE)
    }

    /// `[any-thread]` Convenience for `contains(READ_ONLY)`.
    #[inline]
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        self.contains(Self::READ_ONLY)
    }

    /// `[any-thread]` Convenience for `contains(STEPPED)`.
    #[inline]
    #[must_use]
    pub const fn is_stepped(self) -> bool {
        self.contains(Self::STEPPED)
    }
}

impl BitOr for ParamFlags {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for ParamFlags {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for ParamFlags {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for ParamFlags {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl Sub for ParamFlags {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        self.without(rhs)
    }
}

impl SubAssign for ParamFlags {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.remove(rhs);
    }
}

impl Not for ParamFlags {
    type Output = Self;
    /// Complement **within the flags ABI v1 defines**, so the result never carries a
    /// bit a host would have to reject.
    #[inline]
    fn not(self) -> Self {
        Self(!self.0 & Self::ALL.0)
    }
}

impl fmt::Debug for ParamFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMES: [(ParamFlags, &str); 9] = [
            (ParamFlags::AUTOMATABLE, "AUTOMATABLE"),
            (ParamFlags::MODULATABLE, "MODULATABLE"),
            (ParamFlags::PER_NOTE, "PER_NOTE"),
            (ParamFlags::STEPPED, "STEPPED"),
            (ParamFlags::READ_ONLY, "READ_ONLY"),
            (ParamFlags::HIDDEN, "HIDDEN"),
            (ParamFlags::BYPASS, "BYPASS"),
            (ParamFlags::REQUIRES_PROCESS, "REQUIRES_PROCESS"),
            (ParamFlags::IS_METER, "IS_METER"),
        ];

        if self.is_empty() {
            return f.write_str("ParamFlags(EMPTY)");
        }
        f.write_str("ParamFlags(")?;
        let mut first = true;
        for (flag, name) in NAMES {
            if self.contains(flag) {
                if !first {
                    f.write_str(" | ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        let unknown = self.0 & !Self::ALL.0;
        if unknown != 0 {
            if !first {
                f.write_str(" | ")?;
            }
            write!(f, "{unknown:#x}")?;
        }
        f.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_values_match_the_abi() {
        assert_eq!(ParamFlags::AUTOMATABLE.bits(), 1 << 0);
        assert_eq!(ParamFlags::MODULATABLE.bits(), 1 << 1);
        assert_eq!(ParamFlags::PER_NOTE.bits(), 1 << 2);
        assert_eq!(ParamFlags::STEPPED.bits(), 1 << 3);
        assert_eq!(ParamFlags::READ_ONLY.bits(), 1 << 4);
        assert_eq!(ParamFlags::HIDDEN.bits(), 1 << 5);
        assert_eq!(ParamFlags::BYPASS.bits(), 1 << 6);
        assert_eq!(ParamFlags::REQUIRES_PROCESS.bits(), 1 << 7);
        assert_eq!(ParamFlags::IS_METER.bits(), 1 << 8);
        assert_eq!(ParamFlags::ALL.bits(), 0x1ff);
    }

    #[test]
    fn set_algebra() {
        let f = ParamFlags::AUTOMATABLE | ParamFlags::STEPPED;
        assert!(f.contains(ParamFlags::AUTOMATABLE));
        assert!(f.contains(ParamFlags::AUTOMATABLE | ParamFlags::STEPPED));
        assert!(!f.contains(ParamFlags::AUTOMATABLE | ParamFlags::HIDDEN));
        assert!(f.intersects(ParamFlags::AUTOMATABLE | ParamFlags::HIDDEN));
        assert!(!f.intersects(ParamFlags::HIDDEN));
        assert_eq!(f & ParamFlags::STEPPED, ParamFlags::STEPPED);
        assert_eq!(f - ParamFlags::STEPPED, ParamFlags::AUTOMATABLE);
        assert!(ParamFlags::EMPTY.is_empty());
        assert!(!f.is_empty());
    }

    #[test]
    fn in_place_ops() {
        let mut f = ParamFlags::EMPTY;
        f.insert(ParamFlags::BYPASS);
        f |= ParamFlags::HIDDEN;
        assert!(f.contains(ParamFlags::BYPASS | ParamFlags::HIDDEN));
        f.remove(ParamFlags::BYPASS);
        f -= ParamFlags::HIDDEN;
        assert!(f.is_empty());
        f &= ParamFlags::ALL;
        assert!(f.is_empty());
    }

    #[test]
    fn unknown_bits_are_dropped_not_rejected() {
        let raw = ParamFlags::BYPASS.bits() | (1 << 31);
        let flags = ParamFlags::from_bits_truncate(raw);
        assert_eq!(flags, ParamFlags::BYPASS);
        assert_eq!(flags.bits(), ParamFlags::BYPASS.bits());
    }

    #[test]
    fn complement_stays_inside_the_abi() {
        let inverted = !ParamFlags::AUTOMATABLE;
        assert_eq!(inverted.bits(), ParamFlags::ALL.bits() & !1);
        assert!(!inverted.contains(ParamFlags::AUTOMATABLE));
        assert_eq!(!ParamFlags::ALL, ParamFlags::EMPTY);
    }

    #[test]
    fn defaults_are_sane() {
        assert_eq!(ParamFlags::default(), ParamFlags::EMPTY);
        assert!(ParamFlags::DEFAULT.is_automatable());
        assert!(ParamFlags::METER_DEFAULT.is_read_only());
        assert!(!ParamFlags::METER_DEFAULT.is_automatable());
        assert!(ParamFlags::METER_DEFAULT.contains(ParamFlags::IS_METER));
    }

    #[test]
    fn debug_lists_names() {
        let s = format!("{:?}", ParamFlags::AUTOMATABLE | ParamFlags::STEPPED);
        assert_eq!(s, "ParamFlags(AUTOMATABLE | STEPPED)");
        assert_eq!(format!("{:?}", ParamFlags::EMPTY), "ParamFlags(EMPTY)");
    }
}
