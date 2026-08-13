//! `daux.params/1` — the parameter model (`abi-v1` §11.2).
//!
//! Values crossing the ABI are always **plain** (real-world) values, never normalised.
//! Normalisation is a plug-in-side concern so that curve changes never break automation.

use core::ffi::c_void;

use crate::compat::{impl_abi_default, impl_abi_struct};
use crate::events::DauxEventListV1;
use crate::handle::DauxPluginHandle;
use crate::status::DauxStatus;
use crate::string::{DauxName, DauxStrView, DauxText};

/// The host may record and play back automation for this parameter.
pub const DAUX_PARAM_FLAG_AUTOMATABLE: u32 = 1 << 0;
/// The parameter accepts `DAUX_EVENT_PARAM_MOD` offsets.
pub const DAUX_PARAM_FLAG_MODULATABLE: u32 = 1 << 1;
/// The parameter can be addressed per voice.
pub const DAUX_PARAM_FLAG_PER_NOTE: u32 = 1 << 2;
/// The parameter is quantised to `step_count + 1` values.
pub const DAUX_PARAM_FLAG_STEPPED: u32 = 1 << 3;
/// The host must not write this parameter.
pub const DAUX_PARAM_FLAG_READ_ONLY: u32 = 1 << 4;
/// The parameter should not be shown in a generic editor.
pub const DAUX_PARAM_FLAG_HIDDEN: u32 = 1 << 5;
/// The parameter is the plug-in's bypass switch.
pub const DAUX_PARAM_FLAG_BYPASS: u32 = 1 << 6;
/// Changing the value requires `process` to run to take effect.
pub const DAUX_PARAM_FLAG_REQUIRES_PROCESS: u32 = 1 << 7;
/// The parameter is an output meter written by the plug-in.
pub const DAUX_PARAM_FLAG_IS_METER: u32 = 1 << 8;

/// Description of one parameter.
///
/// [main-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxParamInfoV1 {
    /// `size_of::<DauxParamInfoV1>()` as written by the producer.
    pub size: u32,
    /// Permanent parameter id; stable forever, never reused (§14).
    pub id: u32,
    /// Bitset of `DAUX_PARAM_FLAG_*`.
    pub flags: u32,
    /// Number of steps, `0` for continuous.
    pub step_count: u32,
    /// Display name.
    pub name: DauxName,
    /// Group path: `""` for top level, `"/"`-separated otherwise.
    pub group: DauxName,
    /// Unit suffix: `"dB"`, `"Hz"`, `"%"`, `""`.
    pub unit: DauxName,
    /// Lowest plain value.
    pub min_value: f64,
    /// Highest plain value.
    pub max_value: f64,
    /// Plain value a fresh instance starts at.
    pub default_value: f64,
    /// Plug-in private accelerator, echoed back on parameter events. May be null.
    pub cookie: *mut c_void,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 4],
}

impl DauxParamInfoV1 {
    /// [main-thread] An all-zero parameter description with `size` set.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        // SAFETY: every field is a plain integer, a float, a byte array or a raw pointer
        // (`cookie`, for which null is the specified "absent" value). No field is a
        // reference, function pointer or enum, so the all-zero bit pattern is a valid,
        // fully initialised value. Zeroing also clears any implicit padding.
        let mut this: Self = unsafe { core::mem::zeroed() };
        this.size = Self::SIZE;
        this
    }
}

impl_abi_struct!(DauxParamInfoV1);
impl_abi_default!(DauxParamInfoV1);

/// Function table of the `daux.params/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxParamsApiV1 {
    /// `size_of::<DauxParamsApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,

    /// Number of parameters. [main-thread]
    pub count: unsafe extern "C" fn(p: DauxPluginHandle) -> u32,

    /// Fills `out` with the parameter at `index`. [main-thread]
    pub get_info: unsafe extern "C" fn(
        p: DauxPluginHandle,
        index: u32,
        out: *mut DauxParamInfoV1,
    ) -> DauxStatus,

    /// Reads the current plain value of parameter `id`. [main-thread]
    pub get_value: unsafe extern "C" fn(p: DauxPluginHandle, id: u32, out: *mut f64) -> DauxStatus,

    /// Formats `value` into `out` (capacity [`DAUX_TEXT_SIZE`](crate::DAUX_TEXT_SIZE)).
    /// [main-thread]
    pub value_to_text: unsafe extern "C" fn(
        p: DauxPluginHandle,
        id: u32,
        value: f64,
        out: *mut DauxText,
    ) -> DauxStatus,

    /// Parses `text` into a plain value. [main-thread]
    pub text_to_value: unsafe extern "C" fn(
        p: DauxPluginHandle,
        id: u32,
        text: DauxStrView,
        out: *mut f64,
    ) -> DauxStatus,

    /// Applies parameter events while the plug-in is not processing.
    /// [main-thread when inactive, audio-thread otherwise]
    pub flush: unsafe extern "C" fn(
        p: DauxPluginHandle,
        in_events: *const DauxEventListV1,
        out_events: *const DauxEventListV1,
    ),

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 4],
}

impl_abi_struct!(DauxParamsApiV1);
