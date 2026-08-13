//! Opaque handles and interface pairs (`abi-v1` §2.2, §2.3).
//!
//! A handle is meaningful only to the module that produced it. The receiving module MUST
//! treat it as an opaque token, MUST NOT dereference it, and MUST NOT let it outlive the
//! object it names (`abi-v1` §16).
//!
//! An *interface* is a handle plus a pointer to a function table owned by the producing
//! module. Function tables are immutable and remain valid for as long as the producing
//! module is loaded.

use core::ffi::c_void;
use core::ptr;

use crate::factory::DauxFactoryApiV1;
use crate::host::DauxHostApiV1;
use crate::plugin::DauxPluginApiV1;

/// Declares one of the opaque `#[repr(transparent)]` handle types.
macro_rules! opaque_handle {
    (
        $(#[$meta:meta])*
        $name:ident
    ) => {
        $(#[$meta])*
        ///
        /// [any-thread]
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub *mut c_void);

        impl $name {
            /// [any-thread] The null handle.
            #[inline]
            #[must_use]
            pub const fn null() -> Self {
                Self(ptr::null_mut())
            }

            /// [any-thread] Wraps a raw token.
            #[inline]
            #[must_use]
            pub const fn from_ptr(ptr: *mut c_void) -> Self {
                Self(ptr)
            }

            /// [any-thread] The raw token.
            #[inline]
            #[must_use]
            pub const fn as_ptr(self) -> *mut c_void {
                self.0
            }

            /// [any-thread] `true` when the handle is null.
            #[inline]
            #[must_use]
            pub const fn is_null(self) -> bool {
                self.0.is_null()
            }
        }

        impl Default for $name {
            #[inline]
            fn default() -> Self {
                Self::null()
            }
        }
    };
}

opaque_handle! {
    /// Opaque token identifying a factory inside the plug-in module.
    DauxFactoryHandle
}

opaque_handle! {
    /// Opaque token identifying one plug-in instance inside the plug-in module.
    DauxPluginHandle
}

opaque_handle! {
    /// Opaque token identifying the host inside the host module.
    DauxHostHandle
}

/// Declares one of the `handle + api` interface pairs.
macro_rules! interface_pair {
    (
        $(#[$meta:meta])*
        $name:ident, $handle:ty, $api:ty
    ) => {
        $(#[$meta])*
        ///
        /// [any-thread]
        #[repr(C)]
        #[derive(Debug, Clone, Copy)]
        pub struct $name {
            /// Opaque token owned by the producing module.
            pub handle: $handle,
            /// Immutable function table owned by the producing module.
            pub api: *const $api,
        }

        impl $name {
            /// [any-thread] An interface with a null handle and a null table.
            #[inline]
            #[must_use]
            pub const fn null() -> Self {
                Self { handle: <$handle>::null(), api: ptr::null() }
            }

            /// [any-thread] Pairs a handle with a table.
            #[inline]
            #[must_use]
            pub const fn new(handle: $handle, api: *const $api) -> Self {
                Self { handle, api }
            }

            /// [any-thread] `true` when the function table pointer is null, i.e. the
            /// interface was never populated.
            #[inline]
            #[must_use]
            pub const fn is_null(&self) -> bool {
                self.api.is_null()
            }

            /// [any-thread] Borrows the function table.
            ///
            /// Returns `None` when `api` is null.
            ///
            /// # Safety
            ///
            /// The caller guarantees that `api` is either null or points to a live,
            /// correctly aligned table owned by the producing module, and that the module
            /// stays loaded and the owning object alive for `'a` (`abi-v1` §16.1). Tables
            /// are immutable for their whole lifetime, so a shared reference cannot race.
            #[inline]
            #[must_use]
            pub unsafe fn api<'a>(&self) -> Option<&'a $api> {
                if self.api.is_null() {
                    return None;
                }
                // SAFETY: non-null was just checked; the caller's contract guarantees the
                // pointee is a live, aligned, immutable table outliving `'a`.
                Some(unsafe { &*self.api })
            }
        }

        impl Default for $name {
            #[inline]
            fn default() -> Self {
                Self::null()
            }
        }
    };
}

interface_pair! {
    /// The factory interface a plug-in module hands to the host.
    DauxFactoryV1, DauxFactoryHandle, DauxFactoryApiV1
}

interface_pair! {
    /// The instance interface a plug-in module hands to the host.
    DauxPluginV1, DauxPluginHandle, DauxPluginApiV1
}

interface_pair! {
    /// The host interface a host hands to a plug-in module.
    DauxHostV1, DauxHostHandle, DauxHostApiV1
}
