//! Host-side function tables (`abi-v1` §11.6).

use core::ffi::c_void;

use crate::compat::impl_abi_struct;
use crate::handle::DauxHostHandle;
use crate::status::DauxBool;
use crate::string::{DauxName, DauxStrView};
use crate::version::DauxVersion;

/// Verbose tracing.
pub const DAUX_LOG_TRACE: u32 = 0;
/// Developer diagnostics.
pub const DAUX_LOG_DEBUG: u32 = 1;
/// Normal operational messages.
pub const DAUX_LOG_INFO: u32 = 2;
/// Recoverable problems.
pub const DAUX_LOG_WARN: u32 = 3;
/// Failures the user should know about.
pub const DAUX_LOG_ERROR: u32 = 4;
/// Unrecoverable failures; the instance is unusable.
pub const DAUX_LOG_FATAL: u32 = 5;

/// Function table of the `daux.host.log/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxHostLogApiV1 {
    /// `size_of::<DauxHostLogApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Emits a log record. MUST be non-blocking and allocation-free when called from the
    /// audio thread. [any-thread]
    pub log: unsafe extern "C" fn(h: DauxHostHandle, level: u32, msg: DauxStrView),
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 2],
}

impl_abi_struct!(DauxHostLogApiV1);

/// Function table of the `daux.host.params/1` extension.
///
/// The `flags` argument of `rescan` is a bitset whose meaning is reserved: ABI v1.0 defines
/// no `DAUX_PARAM_RESCAN_*` constants, so a plug-in passes `0` and a host treats any
/// non-zero value as "rescan everything".
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxHostParamsApiV1 {
    /// `size_of::<DauxHostParamsApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// The plug-in changed a value itself (e.g. from its editor). [main-thread]
    pub changed: unsafe extern "C" fn(h: DauxHostHandle, id: u32, value: f64),
    /// A user gesture on `id` started. [main-thread]
    pub gesture_begin: unsafe extern "C" fn(h: DauxHostHandle, id: u32),
    /// A user gesture on `id` ended. [main-thread]
    pub gesture_end: unsafe extern "C" fn(h: DauxHostHandle, id: u32),
    /// Parameter metadata changed; the host must re-read it. [main-thread]
    pub rescan: unsafe extern "C" fn(h: DauxHostHandle, flags: u32),
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 4],
}

impl_abi_struct!(DauxHostParamsApiV1);

/// Function table of the `daux.host.worker/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxHostWorkerApiV1 {
    /// `size_of::<DauxHostWorkerApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Requests that `on_worker` run off the audio thread. Real-time safe and
    /// non-blocking; returns [`DAUX_FALSE`](crate::DAUX_FALSE) when the queue is full.
    /// [any-thread]
    pub schedule: unsafe extern "C" fn(h: DauxHostHandle, task_id: u64) -> DauxBool,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 2],
}

impl_abi_struct!(DauxHostWorkerApiV1);

/// Function table of the `daux.host.gui/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxHostGuiApiV1 {
    /// `size_of::<DauxHostGuiApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Asks the host to resize the editor window, in physical pixels. [main-thread]
    pub request_resize: unsafe extern "C" fn(h: DauxHostHandle, w: u32, h_px: u32) -> DauxBool,
    /// Asks the host to show the editor. [main-thread]
    pub request_show: unsafe extern "C" fn(h: DauxHostHandle) -> DauxBool,
    /// Asks the host to hide the editor. [main-thread]
    pub request_hide: unsafe extern "C" fn(h: DauxHostHandle) -> DauxBool,
    /// Tells the host the editor closed itself. [main-thread]
    pub closed: unsafe extern "C" fn(h: DauxHostHandle, was_destroyed: DauxBool),
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 4],
}

impl_abi_struct!(DauxHostGuiApiV1);

/// The host API root, reached through [`DauxHostV1`](crate::DauxHostV1).
///
/// A plug-in MUST NOT retain the `DauxHostV1` pointer after `destroy_factory` returns
/// (`abi-v1` §16.1).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxHostApiV1 {
    /// `size_of::<DauxHostApiV1>()` as written by the producer.
    pub size: u32,
    /// Major ABI version the host implements.
    pub abi_version_major: u32,
    /// Minor ABI version the host implements.
    pub abi_version_minor: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,

    /// Host display name.
    pub name: DauxName,
    /// Host vendor name.
    pub vendor: DauxName,
    /// Host version.
    pub version: DauxVersion,

    /// Extension lookup. Callable from any thread, MUST be cheap and lock-free; unknown
    /// ids return null. [any-thread]
    pub get_extension: unsafe extern "C" fn(h: DauxHostHandle, id: DauxStrView) -> *const c_void,

    /// Ask the host to deactivate and reactivate the plug-in. [any-thread]
    pub request_restart: unsafe extern "C" fn(h: DauxHostHandle),
    /// Ask the host to resume calling `process`. [any-thread]
    pub request_process: unsafe extern "C" fn(h: DauxHostHandle),
    /// Ask the host to call `on_main_thread` soon. Real-time safe. [any-thread]
    pub request_callback: unsafe extern "C" fn(h: DauxHostHandle),

    /// Thread check; null when the host cannot answer. [any-thread]
    pub is_main_thread: Option<unsafe extern "C" fn(h: DauxHostHandle) -> DauxBool>,
    /// Thread check; null when the host cannot answer. [any-thread]
    pub is_audio_thread: Option<unsafe extern "C" fn(h: DauxHostHandle) -> DauxBool>,

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 8],
}

impl_abi_struct!(DauxHostApiV1);
