//! Schema versioning.

use core::fmt;

/// The **schema** version of a state document — the shape the plug-in gave its data, not
/// the version of the container format. [any-thread]
///
/// A plug-in bumps this whenever it changes what it writes, and registers a
/// [`MigrationChain`](crate::MigrationChain) step so older documents can still be loaded.
/// Per `docs/specifications/abi-v1.md` §12 a plug-in must be able to load *every* schema
/// version it has ever shipped, or fail with no side effects.
///
/// The container format itself is versioned separately by
/// [`format::FORMAT_VERSION`](crate::format::FORMAT_VERSION).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StateVersion(pub u32);

impl StateVersion {
    /// The version a plug-in that has never migrated anything should write: `1`.
    pub const INITIAL: Self = Self(1);

    /// The raw number. [any-thread]
    #[inline]
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The next version up, saturating at [`u32::MAX`]. [any-thread]
    #[inline]
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl From<u32> for StateVersion {
    #[inline]
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<StateVersion> for u32 {
    #[inline]
    fn from(v: StateVersion) -> Self {
        v.0
    }
}

impl fmt::Display for StateVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_and_display() {
        assert!(StateVersion(1) < StateVersion(2));
        assert_eq!(StateVersion::INITIAL, StateVersion(1));
        assert_eq!(StateVersion(3).to_string(), "v3");
        assert_eq!(StateVersion::default(), StateVersion(0));
    }

    #[test]
    fn next_saturates() {
        assert_eq!(StateVersion(1).next(), StateVersion(2));
        assert_eq!(StateVersion(u32::MAX).next(), StateVersion(u32::MAX));
    }

    #[test]
    fn converts_to_and_from_u32() {
        assert_eq!(u32::from(StateVersion::from(7u32)), 7);
        assert_eq!(StateVersion(9).get(), 9);
    }
}
