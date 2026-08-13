//! The plug-in instance function table (`abi-v1` §7).

use core::ffi::c_void;

use crate::compat::impl_abi_struct;
use crate::handle::DauxPluginHandle;
use crate::process::{DauxProcessConfigV1, DauxProcessV1};
use crate::status::DauxStatus;
use crate::string::DauxStrView;

/// Function table of one plug-in instance.
///
/// Lifecycle (`abi-v1` §7) — any other transition is a host error and the plug-in MUST
/// return [`DAUX_ERR_INVALID_STATE`](crate::DAUX_ERR_INVALID_STATE) rather than misbehave:
///
/// ```text
/// created ──init──> inactive ──activate──> active ──start_processing──> processing
///                      ^                      |                              |
///                      └──── deactivate ──────┘<──── stop_processing ────────┘
/// inactive ──destroy──> gone
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxPluginApiV1 {
    /// `size_of::<DauxPluginApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,

    /// Late initialisation. The instance is created but not yet usable until this returns
    /// [`DAUX_OK`](crate::DAUX_OK). Extensions MAY be queried after this point.
    /// [main-thread]
    pub init: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxStatus,

    /// Destroys the instance. MUST be preceded by `deactivate` if activated. [main-thread]
    pub destroy: unsafe extern "C" fn(p: DauxPluginHandle),

    /// Allocates DSP resources for the given configuration. [main-thread]
    pub activate:
        unsafe extern "C" fn(p: DauxPluginHandle, config: *const DauxProcessConfigV1) -> DauxStatus,

    /// Releases DSP resources. [main-thread]
    pub deactivate: unsafe extern "C" fn(p: DauxPluginHandle),

    /// Called on the audio thread before the first `process` of a run. [audio-thread]
    pub start_processing: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxStatus,

    /// Called on the audio thread after the last `process` of a run. [audio-thread]
    pub stop_processing: unsafe extern "C" fn(p: DauxPluginHandle),

    /// Clears all internal audio state (delay lines, filters, voices).
    /// [audio-thread, only while not processing]
    pub reset: unsafe extern "C" fn(p: DauxPluginHandle),

    /// The real-time entry point (§8). Returns one of the `DAUX_PROCESS_*` codes.
    /// [audio-thread]
    pub process: unsafe extern "C" fn(p: DauxPluginHandle, process: *const DauxProcessV1) -> i32,

    /// Extension lookup. Only valid after `init`; unknown ids MUST return null.
    /// [any-thread]
    pub get_extension: unsafe extern "C" fn(p: DauxPluginHandle, id: DauxStrView) -> *const c_void,

    /// Drains work queued for the main thread after `request_callback`. [main-thread]
    pub on_main_thread: unsafe extern "C" fn(p: DauxPluginHandle),

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 6],
}

impl_abi_struct!(DauxPluginApiV1);
