//! Size-based forward compatibility (`abi-v1` §1, §3).
//!
//! Every growable structure in the DAUx ABI carries `size: u32` as its first field and is
//! **append-only**: a future minor revision may add fields at the tail, but never reorders,
//! resizes, repurposes or removes an existing one. A reader therefore validates `size`
//! before touching any field beyond the ones it knows:
//!
//! > a field at offset `O` of width `W` is present iff `size >= O + W`.
//!
//! The helpers here implement exactly that rule and nothing more.

/// [any-thread] `true` when a structure whose `size` field holds `size` carries a field of
/// `width` bytes at byte offset `offset`.
///
/// This is the raw form used when only the `size` word has been read, for example while
/// validating a structure that a foreign module wrote into caller memory. The addition is
/// saturating, so absurd offsets can never wrap into a false positive.
#[inline]
#[must_use]
pub const fn has_field(size: u32, offset: usize, width: usize) -> bool {
    (size as usize) >= offset.saturating_add(width)
}

pub(crate) mod sealed {
    /// Private supertrait that keeps [`AbiStruct`](super::AbiStruct) closed.
    pub trait Sealed {}
}

/// [any-thread] Common shape of every growable `#[repr(C)]` structure in the DAUx ABI.
///
/// The trait is sealed: it is implemented by `daux-abi` for the structures defined in
/// `docs/specifications/abi-v1.md` and cannot be implemented downstream.
pub trait AbiStruct: Copy + sealed::Sealed {
    /// Byte size of the v1.0 revision of the structure on this target.
    ///
    /// Hosts reject a structure whose `size` is smaller than this (`abi-v1` §3, rejection
    /// rule 4). When a future minor revision appends fields, this constant stays frozen at
    /// the v1.0 value while `size_of::<Self>()` grows.
    const MIN_SIZE_V1_0: usize;

    /// The `size` value the producer actually wrote.
    fn declared_size(&self) -> u32;
}

/// [any-thread] Minimum byte size a conforming v1.0 writer produces for `T`.
///
/// ```
/// # use daux_abi::{size_of_v1_0, DauxProcessV1};
/// assert!(size_of_v1_0::<DauxProcessV1>() >= 4);
/// ```
#[inline]
#[must_use]
pub fn size_of_v1_0<T: AbiStruct>() -> usize {
    T::MIN_SIZE_V1_0
}

/// [any-thread] `true` when `value`'s `size` covers the whole v1.0 revision of `T`.
#[inline]
#[must_use]
pub fn is_v1_0_compatible<T: AbiStruct>(value: &T) -> bool {
    value.declared_size() as usize >= T::MIN_SIZE_V1_0
}

/// Implements the size/compatibility surface shared by every growable ABI structure.
///
/// `$ty` must have a `size: u32` field (`self.size`), or, with the `header` form, an
/// embedded [`DauxEventHeaderV1`](crate::DauxEventHeaderV1) whose `size` covers the whole
/// record.
macro_rules! impl_abi_struct {
    ($($ty:ty),+ $(,)?) => {
        $(impl_abi_struct!(@imp $ty, size);)+
    };
    (header: $($ty:ty),+ $(,)?) => {
        $(impl_abi_struct!(@imp $ty, header.size);)+
    };
    (@imp $ty:ty, $($field:ident).+) => {
        impl $ty {
            /// [any-thread] The value this build writes into the `size` field.
            pub const SIZE: u32 = ::core::mem::size_of::<$ty>() as u32;

            /// [any-thread] Byte size of the v1.0 revision of this structure.
            ///
            /// A reader MUST reject a structure whose `size` is smaller than this
            /// (`abi-v1` §3). The constant is frozen per revision: when a future minor
            /// version appends tail fields, `SIZE` grows and this does not.
            pub const MIN_SIZE_V1_0: usize = ::core::mem::size_of::<$ty>();

            /// [any-thread] `true` when `size` covers every v1.0 field.
            #[inline]
            #[must_use]
            pub const fn is_v1_0_compatible(&self) -> bool {
                (self.$($field).+ as usize) >= Self::MIN_SIZE_V1_0
            }

            /// [any-thread] `true` when the field of `width` bytes at byte `offset` was
            /// written by the producer.
            ///
            /// Use it with [`core::mem::offset_of!`] before reading any field that a
            /// revision newer than the reader's may have appended.
            #[inline]
            #[must_use]
            pub const fn field_present(&self, offset: usize, width: usize) -> bool {
                $crate::compat::has_field(self.$($field).+, offset, width)
            }
        }

        impl $crate::compat::sealed::Sealed for $ty {}

        impl $crate::compat::AbiStruct for $ty {
            const MIN_SIZE_V1_0: usize = ::core::mem::size_of::<$ty>();

            #[inline]
            fn declared_size(&self) -> u32 {
                self.$($field).+
            }
        }
    };
}

/// Implements `empty()` and `Default` in terms of an inherent `pub const fn new()`.
macro_rules! impl_abi_default {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl $ty {
                /// [any-thread] Alias of [`Self::new`]: an all-zero value with `size` set.
                #[inline]
                #[must_use]
                pub const fn empty() -> Self {
                    Self::new()
                }
            }

            impl Default for $ty {
                #[inline]
                fn default() -> Self {
                    Self::new()
                }
            }
        )+
    };
}

pub(crate) use {impl_abi_default, impl_abi_struct};
