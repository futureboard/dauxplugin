//! Processing configuration, audio buffers and the process block (`abi-v1` §8).

use crate::compat::{impl_abi_default, impl_abi_struct};
use crate::events::DauxEventListV1;
use crate::transport::DauxTransportV1;

/// The host schedules the plug-in on a real-time thread.
pub const DAUX_PROCESS_MODE_REALTIME: u32 = 0;
/// The host renders faster (or slower) than real time.
pub const DAUX_PROCESS_MODE_OFFLINE: u32 = 1;
/// The host is pre-rolling to warm internal state.
pub const DAUX_PROCESS_MODE_PREFETCH: u32 = 2;
/// The host is analysing rather than producing output.
pub const DAUX_PROCESS_MODE_ANALYSIS: u32 = 3;

/// Outputs are undefined; the host SHOULD silence them.
pub const DAUX_PROCESS_ERROR: i32 = 0;
/// Keep calling `process`.
pub const DAUX_PROCESS_CONTINUE: i32 = 1;
/// Keep calling `process` while the output is non-silent.
pub const DAUX_PROCESS_CONTINUE_IF_LOUD: i32 = 2;
/// Input finished, tail still ringing out.
pub const DAUX_PROCESS_TAIL: i32 = 3;
/// Output is silent and will remain so.
pub const DAUX_PROCESS_SLEEP: i32 = 4;

/// Configuration handed to [`DauxPluginApiV1::activate`](crate::DauxPluginApiV1).
///
/// `max_block_size` is an upper bound, **not** a promise: every `process` call MAY pass any
/// `frame_count` in `1 ..= max_block_size`, so a plug-in MUST NOT assume a constant block
/// size.
///
/// [main-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxProcessConfigV1 {
    /// `size_of::<DauxProcessConfigV1>()` as written by the producer.
    pub size: u32,
    /// Exactly one `DAUX_SAMPLE_FORMAT_*` bit.
    pub sample_format: u32,
    /// One of the `DAUX_PROCESS_MODE_*` constants.
    pub process_mode: u32,
    /// Smallest block the host will ever request.
    pub min_block_size: u32,
    /// Largest block the host will ever request.
    pub max_block_size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Sample rate in Hz.
    pub sample_rate: f64,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 6],
}

impl DauxProcessConfigV1 {
    /// [main-thread] An all-zero configuration with `size` set.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        // SAFETY: every field is a plain integer, a float or an array of `usize`; there is
        // no pointer, reference, function pointer or enum among them, so the all-zero bit
        // pattern is a valid, fully initialised value. Zeroing also clears any implicit
        // padding, which keeps the bytes crossing the boundary deterministic and satisfies
        // "a writer MUST zero every field it does not populate" (`abi-v1` §3).
        let mut this: Self = unsafe { core::mem::zeroed() };
        this.size = Self::SIZE;
        this
    }
}

impl_abi_struct!(DauxProcessConfigV1);
impl_abi_default!(DauxProcessConfigV1);

/// One audio bus for one block.
///
/// Exactly one of `data32`/`data64` is non-null and MUST match
/// [`DauxProcessConfigV1::sample_format`]. Buffers MAY alias between input and output
/// (in-place processing); input buffers MUST be treated as read-only, the `*mut` type
/// existing only so hosts can hand out one allocation for both directions.
///
/// This structure has no `size` field: it is fixed for the whole v1 generation.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxAudioBufferV1 {
    /// Number of channel pointers in `data32`/`data64`.
    pub channel_count: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Array of `channel_count` pointers, each to `frame_count` `f32` samples.
    pub data32: *const *mut f32,
    /// Array of `channel_count` pointers, each to `frame_count` `f64` samples.
    pub data64: *const *mut f64,
    /// Bit `c` set means channel `c` is constant for the whole block (usually silence).
    /// Purely an optimisation hint; readers MUST tolerate a zero mask.
    pub constant_mask: u64,
}

impl DauxAudioBufferV1 {
    /// [audio-thread] An all-zero, channel-less buffer.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            channel_count: 0,
            _pad0: 0,
            data32: core::ptr::null(),
            data64: core::ptr::null(),
            constant_mask: 0,
        }
    }

    /// [audio-thread] Alias of [`Self::new`].
    #[inline]
    #[must_use]
    pub const fn empty() -> Self {
        Self::new()
    }

    /// [audio-thread] `true` when the host flagged channel `index` as constant.
    ///
    /// Channels at index 64 and above are always reported as non-constant, because the
    /// mask cannot address them.
    #[inline]
    #[must_use]
    pub const fn is_channel_constant(&self, index: u32) -> bool {
        if index >= u64::BITS {
            return false;
        }
        (self.constant_mask >> index) & 1 != 0
    }
}

impl Default for DauxAudioBufferV1 {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// Everything one `process` call may touch.
///
/// All pointers inside this structure — audio, events, transport, SysEx payloads — are
/// borrowed for exactly the duration of that call (`abi-v1` §16.3).
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxProcessV1 {
    /// `size_of::<DauxProcessV1>()` as written by the producer.
    pub size: u32,
    /// Frames in this block, in `1 ..= max_block_size`.
    pub frame_count: u32,

    /// Monotonic sample counter since processing started, or `-1` if unavailable.
    pub steady_time: i64,

    /// Null when the host exposes no transport.
    pub transport: *const DauxTransportV1,

    /// Number of entries in `audio_inputs`.
    pub audio_input_count: u32,
    /// Number of entries in `audio_outputs`.
    pub audio_output_count: u32,
    /// Array of `audio_input_count` input buses.
    pub audio_inputs: *const DauxAudioBufferV1,
    /// Array of `audio_output_count` output buses.
    pub audio_outputs: *mut DauxAudioBufferV1,

    /// Never null. Empty lists are represented by `count() == 0`.
    pub in_events: *const DauxEventListV1,
    /// Never null. Empty lists are represented by `count() == 0`.
    pub out_events: *const DauxEventListV1,

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 6],
}

impl DauxProcessV1 {
    /// [audio-thread] An all-zero block with `size` set and `steady_time` marked
    /// unavailable.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        // SAFETY: every field is a plain integer or a raw pointer, and the all-zero bit
        // pattern is valid for both (`null` for the pointers, which the specification
        // already uses to mean "absent" for `transport`). No field is a reference, a
        // function pointer or an enum, so nothing has a niche this could violate. Zeroing
        // also clears any implicit padding.
        let mut this: Self = unsafe { core::mem::zeroed() };
        this.size = Self::SIZE;
        this.steady_time = -1;
        this
    }
}

impl_abi_struct!(DauxProcessV1);
impl_abi_default!(DauxProcessV1);
