//! Bounds applied to untrusted input.

/// Hard bounds applied when parsing — and, symmetrically, when writing — a state blob.
/// [main-thread]
///
/// Project files are attacker-controlled from a plug-in's point of view: a `.daw` session
/// can be shared, downloaded or generated. The parser therefore refuses to allocate on the
/// strength of a length field alone. Every limit below is checked *before* any allocation,
/// and every length prefix is additionally checked against the bytes actually remaining in
/// the input, so a hostile blob can never make the reader reserve more memory than it
/// supplied.
///
/// The defaults are generous for real plug-ins and hostile-input-safe:
///
/// | Limit                                | Default   |
/// | ------------------------------------ | --------- |
/// | [`max_blob_bytes`](Self::max_blob_bytes)   | 64 MiB    |
/// | [`max_key_bytes`](Self::max_key_bytes)     | 4 KiB     |
/// | [`max_entries`](Self::max_entries)         | 1 048 576 |
/// | [`max_depth`](Self::max_depth)             | 64        |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct StateLimits {
    /// Largest accepted blob, in bytes. Also caps what a writer will produce.
    pub max_blob_bytes: usize,
    /// Longest accepted key, in bytes (not characters).
    pub max_key_bytes: usize,
    /// Largest accepted entry count, counting group begin/end markers.
    pub max_entries: usize,
    /// Deepest accepted group nesting. Bounds both parser state and the recursive drop of
    /// the resulting tree.
    pub max_depth: usize,
}

impl StateLimits {
    /// 64 MiB.
    pub const DEFAULT_MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;
    /// 4 KiB.
    pub const DEFAULT_MAX_KEY_BYTES: usize = 4 * 1024;
    /// 1 048 576 entries.
    pub const DEFAULT_MAX_ENTRIES: usize = 1024 * 1024;
    /// 64 levels of nesting.
    pub const DEFAULT_MAX_DEPTH: usize = 64;

    /// The default limits, as a `const`. [any-thread]
    pub const DEFAULT: Self = Self {
        max_blob_bytes: Self::DEFAULT_MAX_BLOB_BYTES,
        max_key_bytes: Self::DEFAULT_MAX_KEY_BYTES,
        max_entries: Self::DEFAULT_MAX_ENTRIES,
        max_depth: Self::DEFAULT_MAX_DEPTH,
    };

    /// The default limits. [any-thread]
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self::DEFAULT
    }

    /// Overrides the maximum blob size. [any-thread]
    #[inline]
    #[must_use]
    pub const fn with_max_blob_bytes(mut self, bytes: usize) -> Self {
        self.max_blob_bytes = bytes;
        self
    }

    /// Overrides the maximum key length. [any-thread]
    #[inline]
    #[must_use]
    pub const fn with_max_key_bytes(mut self, bytes: usize) -> Self {
        self.max_key_bytes = bytes;
        self
    }

    /// Overrides the maximum entry count. [any-thread]
    #[inline]
    #[must_use]
    pub const fn with_max_entries(mut self, entries: usize) -> Self {
        self.max_entries = entries;
        self
    }

    /// Overrides the maximum group nesting depth. [any-thread]
    #[inline]
    #[must_use]
    pub const fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }
}

impl Default for StateLimits {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_numbers() {
        let l = StateLimits::default();
        assert_eq!(l, StateLimits::new());
        assert_eq!(l.max_blob_bytes, 64 * 1024 * 1024);
        assert_eq!(l.max_key_bytes, 4096);
        assert_eq!(l.max_entries, 1_048_576);
        assert_eq!(l.max_depth, 64);
    }

    #[test]
    fn overrides_are_independent() {
        let l = StateLimits::new()
            .with_max_blob_bytes(1024)
            .with_max_key_bytes(8)
            .with_max_entries(4)
            .with_max_depth(2);
        assert_eq!(l.max_blob_bytes, 1024);
        assert_eq!(l.max_key_bytes, 8);
        assert_eq!(l.max_entries, 4);
        assert_eq!(l.max_depth, 2);
    }

    #[test]
    fn zero_limits_are_representable() {
        let l = StateLimits::new().with_max_entries(0).with_max_depth(0);
        assert_eq!(l.max_entries, 0);
        assert_eq!(l.max_depth, 0);
    }
}
