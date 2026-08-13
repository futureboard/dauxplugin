//! `daux.latency/1`, `daux.tail/1` and `daux.render/1` (`abi-v1` §11.5).

use crate::compat::impl_abi_struct;
use crate::handle::DauxPluginHandle;
use crate::status::{DauxBool, DauxStatus};

/// Tail length returned by [`DauxTailApiV1::get`] for a tail that never ends.
pub const DAUX_TAIL_INFINITE: u32 = u32::MAX;

/// Tail length returned by [`DauxTailApiV1::get`] when the plug-in cannot say yet.
///
/// Distinct from [`DAUX_TAIL_INFINITE`] (`abi-v1` §11.5): "infinite" is a settled answer, and
/// "unknown" may change on a later call. Both mean the host must keep calling `process`, and
/// a host that does not distinguish them MUST treat this as infinite — never as a finite tail
/// of 4 294 967 294 samples, which is over a day of audio at 48 kHz.
pub const DAUX_TAIL_UNKNOWN: u32 = u32::MAX - 1;

/// Function table of the `daux.latency/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxLatencyApiV1 {
    /// `size_of::<DauxLatencyApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Latency introduced by the plug-in, in samples. [main-thread]
    pub get: unsafe extern "C" fn(p: DauxPluginHandle) -> u32,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 2],
}

impl_abi_struct!(DauxLatencyApiV1);

/// Function table of the `daux.tail/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxTailApiV1 {
    /// `size_of::<DauxTailApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Samples of tail, [`DAUX_TAIL_INFINITE`] or [`DAUX_TAIL_UNKNOWN`]. [any-thread]
    pub get: unsafe extern "C" fn(p: DauxPluginHandle) -> u32,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 2],
}

impl_abi_struct!(DauxTailApiV1);

/// Function table of the `daux.render/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxRenderApiV1 {
    /// `size_of::<DauxRenderApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Whether the plug-in needs real-time scheduling even when rendering offline.
    /// [any-thread]
    pub has_hard_realtime_requirement: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxBool,
    /// Switches between the `DAUX_PROCESS_MODE_*` modes.
    /// [main-thread, inactive only]
    pub set_mode: unsafe extern "C" fn(p: DauxPluginHandle, mode: u32) -> DauxStatus,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 2],
}

impl_abi_struct!(DauxRenderApiV1);
