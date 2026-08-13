//! A tiny generator for the "set of enum variants" types used by capability
//! negotiation.
//!
//! Capability sets are compared, unioned and intersected on the main thread while an
//! editor is being opened, but they also end up inside `Copy` descriptors that a
//! plug-in may build in a `const`. A `u32` bitmask keeps them allocation-free,
//! `const`-constructible and cheap to compare, which a `Vec` could not.

/// Generates a `Copy` bitset newtype over a fieldless enum.
///
/// Each variant is given an explicit bit index so that the numbering is a deliberate,
/// stable decision rather than a side effect of declaration order.
macro_rules! define_bit_set {
    (
        $(#[$set_meta:meta])*
        $set:ident : $item:ty { $( $variant:ident = $bit:expr ),+ $(,)? }
    ) => {
        $(#[$set_meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $set(u32);

        impl $set {
            /// `[any-thread]` Every variant this set can hold, in bit order.
            pub const VARIANTS: &'static [$item] = &[ $( <$item>::$variant ),+ ];

            /// `[any-thread]` The set containing nothing.
            pub const EMPTY: Self = Self(0);

            /// `[any-thread]` The set containing every variant.
            pub const ALL: Self = Self( $( (1u32 << $bit) )|+ );

            /// `[any-thread]` The bit that represents `item`.
            const fn bit(item: $item) -> u32 {
                match item {
                    $( <$item>::$variant => 1u32 << $bit, )+
                }
            }

            /// `[any-thread]` An empty set.
            #[must_use]
            pub const fn new() -> Self {
                Self::EMPTY
            }

            /// `[any-thread]` A set holding exactly one variant.
            #[must_use]
            pub const fn only(item: $item) -> Self {
                Self(Self::bit(item))
            }

            /// `[any-thread]` This set plus `item`. Adding twice changes nothing.
            #[must_use]
            pub const fn with(self, item: $item) -> Self {
                Self(self.0 | Self::bit(item))
            }

            /// `[any-thread]` This set minus `item`. Removing an absent item is a no-op.
            #[must_use]
            pub const fn without(self, item: $item) -> Self {
                Self(self.0 & !Self::bit(item))
            }

            /// `[any-thread]` Adds `item` in place.
            pub const fn insert(&mut self, item: $item) {
                self.0 |= Self::bit(item);
            }

            /// `[any-thread]` Removes `item` in place.
            pub const fn remove(&mut self, item: $item) {
                self.0 &= !Self::bit(item);
            }

            /// `[any-thread]` Whether `item` is a member.
            #[must_use]
            pub const fn contains(self, item: $item) -> bool {
                self.0 & Self::bit(item) != 0
            }

            /// `[any-thread]` Whether the set holds nothing at all.
            #[must_use]
            pub const fn is_empty(self) -> bool {
                self.0 == 0
            }

            /// `[any-thread]` Number of members.
            #[must_use]
            pub const fn len(self) -> u32 {
                self.0.count_ones()
            }

            /// `[any-thread]` Members present in either set.
            #[must_use]
            pub const fn union(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }

            /// `[any-thread]` Members present in both sets.
            #[must_use]
            pub const fn intersection(self, other: Self) -> Self {
                Self(self.0 & other.0)
            }

            /// `[any-thread]` Whether the two sets share at least one member.
            #[must_use]
            pub const fn intersects(self, other: Self) -> bool {
                self.0 & other.0 != 0
            }

            /// `[any-thread]` The raw mask, for logging and wire formats.
            #[must_use]
            pub const fn bits(self) -> u32 {
                self.0
            }

            /// `[any-thread]` Rebuilds a set from a raw mask, dropping unknown bits so a
            /// future peer's extra capabilities can never be misread as ours.
            #[must_use]
            pub const fn from_bits_truncate(bits: u32) -> Self {
                Self(bits & Self::ALL.0)
            }

            /// `[any-thread]` Iterates the members in bit order.
            pub fn iter(self) -> impl Iterator<Item = $item> {
                Self::VARIANTS
                    .iter()
                    .copied()
                    .filter(move |item| self.contains(*item))
            }
        }

        impl FromIterator<$item> for $set {
            /// `[any-thread]` Collects variants into a set; duplicates are harmless.
            fn from_iter<I: IntoIterator<Item = $item>>(iter: I) -> Self {
                iter.into_iter().fold(Self::EMPTY, Self::with)
            }
        }

        impl core::ops::BitOr for $set {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self {
                self.union(rhs)
            }
        }

        impl core::ops::BitOrAssign for $set {
            fn bitor_assign(&mut self, rhs: Self) {
                self.0 |= rhs.0;
            }
        }

        impl core::ops::BitAnd for $set {
            type Output = Self;
            fn bitand(self, rhs: Self) -> Self {
                self.intersection(rhs)
            }
        }

        impl core::fmt::Debug for $set {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_set().entries(self.iter()).finish()
            }
        }
    };
}

pub(crate) use define_bit_set;
