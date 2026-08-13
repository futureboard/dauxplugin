//! `daux.audio-ports/1` — bus topology (`abi-v1` §11.1).

use crate::compat::{impl_abi_default, impl_abi_struct};
use crate::handle::DauxPluginHandle;
use crate::status::{DauxBool, DauxStatus};
use crate::string::DauxName;

/// Layout is unknown or vendor-specific.
pub const DAUX_LAYOUT_UNKNOWN: u32 = 0;
/// Single channel.
pub const DAUX_LAYOUT_MONO: u32 = 1;
/// Left, right.
pub const DAUX_LAYOUT_STEREO: u32 = 2;
/// Left, right, centre.
pub const DAUX_LAYOUT_L_R_C: u32 = 3;
/// Four-channel quadraphonic.
pub const DAUX_LAYOUT_QUAD: u32 = 4;
/// 2.1 surround.
pub const DAUX_LAYOUT_SURROUND_2_1: u32 = 5;
/// 5.1 surround.
pub const DAUX_LAYOUT_SURROUND_5_1: u32 = 6;
/// 7.1 surround.
pub const DAUX_LAYOUT_SURROUND_7_1: u32 = 7;
/// 7.1.4 immersive.
pub const DAUX_LAYOUT_ATMOS_7_1_4: u32 = 8;
/// First-order ambisonics.
pub const DAUX_LAYOUT_AMBISONIC_1ST: u32 = 9;
/// Second-order ambisonics.
pub const DAUX_LAYOUT_AMBISONIC_2ND: u32 = 10;
/// Third-order ambisonics.
pub const DAUX_LAYOUT_AMBISONIC_3RD: u32 = 11;
/// Discrete, unrelated channels.
pub const DAUX_LAYOUT_DISCRETE: u32 = 12;
/// Custom layout described elsewhere.
pub const DAUX_LAYOUT_CUSTOM: u32 = 13;

/// Primary signal path.
pub const DAUX_PORT_PURPOSE_MAIN: u32 = 0;
/// Auxiliary send or return.
pub const DAUX_PORT_PURPOSE_AUX: u32 = 1;
/// Sidechain / key input.
pub const DAUX_PORT_PURPOSE_SIDECHAIN: u32 = 2;
/// Monitoring output.
pub const DAUX_PORT_PURPOSE_MONITOR: u32 = 3;
/// Measurement-only path.
pub const DAUX_PORT_PURPOSE_ANALYSIS: u32 = 4;
/// Reference signal for comparison.
pub const DAUX_PORT_PURPOSE_REFERENCE: u32 = 5;
/// Control voltage.
pub const DAUX_PORT_PURPOSE_CV: u32 = 6;
/// Control-rate data.
pub const DAUX_PORT_PURPOSE_CONTROL: u32 = 7;

/// This is the plug-in's main bus in its direction.
pub const DAUX_PORT_FLAG_IS_MAIN: u32 = 1 << 0;
/// The host may deactivate this bus.
pub const DAUX_PORT_FLAG_OPTIONAL: u32 = 1 << 1;
/// The bus carries control voltage rather than audio.
pub const DAUX_PORT_FLAG_CV: u32 = 1 << 2;
/// The bus can be processed in 64-bit precision.
pub const DAUX_PORT_FLAG_SUPPORTS_64: u32 = 1 << 3;

/// Description of one audio bus.
///
/// [main-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxAudioPortInfoV1 {
    /// `size_of::<DauxAudioPortInfoV1>()` as written by the producer.
    pub size: u32,
    /// Permanent port id, stable across plug-in versions (§14).
    pub id: u32,
    /// Display name.
    pub name: DauxName,
    /// Number of channels on this bus.
    pub channel_count: u32,
    /// One of the `DAUX_LAYOUT_*` constants.
    pub layout: u32,
    /// One of the `DAUX_PORT_PURPOSE_*` constants.
    pub purpose: u32,
    /// Bitset of `DAUX_PORT_FLAG_*`.
    pub flags: u32,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 4],
}

impl DauxAudioPortInfoV1 {
    /// [main-thread] An all-zero port description with `size` set.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        // SAFETY: every field is a plain integer, a byte array or an array of `usize`;
        // there is no pointer, reference, function pointer or enum among them, so the
        // all-zero bit pattern is a valid, fully initialised value. Zeroing also clears
        // any implicit padding, keeping the bytes that cross the boundary deterministic.
        let mut this: Self = unsafe { core::mem::zeroed() };
        this.size = Self::SIZE;
        this
    }
}

impl_abi_struct!(DauxAudioPortInfoV1);
impl_abi_default!(DauxAudioPortInfoV1);

/// Function table of the `daux.audio-ports/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxAudioPortsApiV1 {
    /// `size_of::<DauxAudioPortsApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,

    /// Number of input or output buses. [main-thread]
    pub count: unsafe extern "C" fn(p: DauxPluginHandle, is_input: DauxBool) -> u32,

    /// Fills `out` with the bus at `index` in the given direction. [main-thread]
    pub get: unsafe extern "C" fn(
        p: DauxPluginHandle,
        index: u32,
        is_input: DauxBool,
        out: *mut DauxAudioPortInfoV1,
    ) -> DauxStatus,

    /// Activates or deactivates an optional bus; null when the plug-in has none.
    /// [main-thread]
    pub set_active: Option<
        unsafe extern "C" fn(
            p: DauxPluginHandle,
            index: u32,
            is_input: DauxBool,
            active: DauxBool,
        ) -> DauxStatus,
    >,

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 4],
}

impl_abi_struct!(DauxAudioPortsApiV1);
