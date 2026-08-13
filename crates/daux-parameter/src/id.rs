//! Stable parameter identity.

use core::fmt;

/// Permanent identity of one parameter.
///
/// `abi-v1` §14 and the project's hard rules make this value **permanent**: renaming a
/// parameter is free, renumbering silently corrupts every saved project that referenced
/// the old number. When a parameter really has to disappear or move, record the change
/// with [`ParamMigration`](crate::ParamMigration) instead of reusing the id.
///
/// `[any-thread]` — a plain `u32` wrapper, `Copy`, comparable and hashable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ParamId(pub u32);

impl ParamId {
    /// `[any-thread]` Builds an id from its raw wire value.
    #[inline]
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// `[any-thread]` Raw wire value, as it appears in `DauxParamInfoV1::id`.
    #[inline]
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// `[any-thread]` Derives an id from a string with a 32-bit FNV-1a hash.
    ///
    /// This is a convenience for authors who would rather write
    /// `ParamId::from_name("gain")` than maintain a table of literals. The hash is
    /// stable across releases and platforms because the algorithm is fixed here, but it
    /// is **not** collision-free: two names can hash to the same id. The derive macro
    /// checks for duplicates at compile time; hand-written parameter lists should keep
    /// the ids sorted and unique themselves.
    ///
    /// Once shipped, the *name* that produced the id becomes as permanent as the id.
    #[must_use]
    pub const fn from_name(name: &str) -> Self {
        const OFFSET: u32 = 0x811c_9dc5;
        const PRIME: u32 = 0x0100_0193;

        let bytes = name.as_bytes();
        let mut hash = OFFSET;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u32;
            hash = hash.wrapping_mul(PRIME);
            i += 1;
        }
        Self(hash)
    }
}

impl From<u32> for ParamId {
    #[inline]
    fn from(raw: u32) -> Self {
        Self(raw)
    }
}

impl From<ParamId> for u32 {
    #[inline]
    fn from(id: ParamId) -> Self {
        id.0
    }
}

impl fmt::Display for ParamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_u32() {
        let id = ParamId::from(7u32);
        assert_eq!(id.get(), 7);
        assert_eq!(u32::from(id), 7);
        assert_eq!(ParamId::new(7), id);
    }

    #[test]
    fn from_name_is_fnv1a_and_stable() {
        // Known FNV-1a/32 vectors: the algorithm must never drift, saved projects
        // depend on it.
        assert_eq!(ParamId::from_name("").get(), 0x811c_9dc5);
        assert_eq!(ParamId::from_name("a").get(), 0xe40c_292c);
        assert_eq!(ParamId::from_name("foobar").get(), 0xbf9c_f968);
    }

    #[test]
    fn from_name_is_const_evaluable() {
        const GAIN: ParamId = ParamId::from_name("gain");
        assert_eq!(GAIN, ParamId::from_name("gain"));
        assert_ne!(GAIN, ParamId::from_name("pan"));
    }

    #[test]
    fn from_name_handles_non_ascii() {
        // Hashing is over UTF-8 bytes, so multi-byte names must not panic or truncate.
        let a = ParamId::from_name("größe");
        let b = ParamId::from_name("grosse");
        assert_ne!(a, b);
    }

    #[test]
    fn displays_as_number() {
        assert_eq!(ParamId(42).to_string(), "42");
    }

    #[test]
    fn ordering_is_numeric() {
        let mut ids = [ParamId(3), ParamId(1), ParamId(2)];
        ids.sort();
        assert_eq!(ids, [ParamId(1), ParamId(2), ParamId(3)]);
    }
}
