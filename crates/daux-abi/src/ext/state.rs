//! `daux.state/1` — save and load (`abi-v1` §11.3).
//!
//! The host owns the stream and therefore the allocation: no memory crosses the module
//! boundary in either direction (`abi-v1` §16.2).
//!
//! `load` is atomic from the host's point of view — a plug-in MUST be able to load every
//! schema version it has ever shipped, or return
//! [`DAUX_ERR_VERSION`](crate::DAUX_ERR_VERSION) with no side effects (`abi-v1` §12).

use core::ffi::c_void;

use crate::compat::{impl_abi_default, impl_abi_struct};
use crate::handle::DauxPluginHandle;
use crate::status::DauxStatus;

/// A byte stream owned by the caller.
///
/// `read`/`write` return the number of bytes transferred, or a negative
/// [`DauxStatus`] code on failure. A short read means end of stream.
///
/// [main-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxStreamV1 {
    /// `size_of::<DauxStreamV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Opaque context passed back to `read`/`write`.
    pub ctx: *mut c_void,
    /// Reads up to `len` bytes; null on a write-only stream.
    pub read: Option<unsafe extern "C" fn(ctx: *mut c_void, buf: *mut u8, len: usize) -> isize>,
    /// Writes up to `len` bytes; null on a read-only stream.
    pub write: Option<unsafe extern "C" fn(ctx: *mut c_void, buf: *const u8, len: usize) -> isize>,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 4],
}

impl DauxStreamV1 {
    /// [main-thread] An all-zero stream with `size` set: no context, and neither `read`
    /// nor `write` available.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        // SAFETY: the only non-integer fields are `ctx`, a raw pointer for which null is
        // meaningful, and the two `Option<unsafe extern "C" fn(..)>` entries. Guaranteed
        // null-pointer optimisation makes an all-zero `Option<fn>` exactly `None`, so the
        // all-zero bit pattern is a valid, fully initialised value with no niche violated.
        let mut this: Self = unsafe { core::mem::zeroed() };
        this.size = Self::SIZE;
        this
    }
}

impl_abi_struct!(DauxStreamV1);
impl_abi_default!(DauxStreamV1);

/// Function table of the `daux.state/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxStateApiV1 {
    /// `size_of::<DauxStateApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Writes the plug-in's state into the host-owned stream. [main-thread]
    pub save: unsafe extern "C" fn(p: DauxPluginHandle, s: *const DauxStreamV1) -> DauxStatus,
    /// Restores the plug-in's state from the host-owned stream. [main-thread]
    pub load: unsafe extern "C" fn(p: DauxPluginHandle, s: *const DauxStreamV1) -> DauxStatus,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 4],
}

impl_abi_struct!(DauxStateApiV1);
