//! The plug-in descriptor and its enumerations (`abi-v1` §6).

use crate::compat::{impl_abi_default, impl_abi_struct};
use crate::string::{DauxId, DauxName, DauxText};
use crate::version::DauxVersion;

/// Static metadata describing one plug-in inside a module.
///
/// The host fills this structure by calling
/// [`DauxFactoryApiV1::descriptor`](crate::DauxFactoryApiV1); the memory is owned by the
/// host, so no allocation crosses the boundary.
///
/// [any-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxPluginDescriptorV1 {
    /// `size_of::<DauxPluginDescriptorV1>()` as written by the producer.
    pub size: u32,
    /// Lowest major ABI version this plug-in can be driven with.
    pub min_abi_version_major: u32,
    /// Lowest minor ABI version this plug-in can be driven with.
    pub min_abi_version_minor: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,

    /// Stable, permanent, reverse-DNS id, e.g. `"studio.futureboard.equzx"` (§14).
    pub id: DauxId,
    /// Display name.
    pub name: DauxName,
    /// Vendor display name.
    pub vendor: DauxName,
    /// Machine-comparable version.
    pub version: DauxVersion,
    /// Human-readable version string, e.g. `"1.2.0-beta.3"`.
    pub version_string: DauxName,
    /// One-paragraph description.
    pub description: DauxText,
    /// Product URL.
    pub url: DauxText,
    /// Support URL.
    pub support_url: DauxText,
    /// Copyright notice.
    pub copyright: DauxText,
    /// Licence identifier, e.g. `"MIT OR Apache-2.0"`.
    pub license: DauxName,

    /// One of the `DAUX_CATEGORY_*` constants.
    pub category: u32,
    /// Bitset of `DAUX_SAMPLE_FORMAT_*` the processor can accept.
    pub sample_formats: u32,
    /// Bitset of `DAUX_CAP_*`.
    pub capabilities: u64,
    /// Schema version of the plug-in's persisted state (§12).
    pub state_schema_version: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad1: u32,
    /// Semicolon-separated free-form tags, e.g. `"eq;dynamics;mastering"`.
    pub features: DauxText,

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 8],
}

impl DauxPluginDescriptorV1 {
    /// [any-thread] An all-zero descriptor with `size` set and the minimum ABI version
    /// pinned to v1.0.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        // SAFETY: every field is a plain integer, a byte array or a `#[repr(C)]`
        // aggregate of those; there is no pointer, reference, function pointer or
        // enum among them, so the all-zero bit pattern is a valid, fully initialised
        // value. Zeroing also clears the structure's implicit padding, which satisfies
        // "a writer MUST zero every field it does not populate" (`abi-v1` §3) byte for
        // byte and keeps the bytes that cross the boundary deterministic.
        let mut this: Self = unsafe { core::mem::zeroed() };
        this.size = Self::SIZE;
        this.min_abi_version_major = crate::version::DAUX_ABI_VERSION_MAJOR;
        this.min_abi_version_minor = crate::version::DAUX_ABI_VERSION_MINOR;
        this
    }
}

impl_abi_struct!(DauxPluginDescriptorV1);
impl_abi_default!(DauxPluginDescriptorV1);

/// Category is unknown or does not fit the list.
pub const DAUX_CATEGORY_UNKNOWN: u32 = 0;
/// Processes audio it is given.
pub const DAUX_CATEGORY_EFFECT: u32 = 1;
/// Generates audio from notes.
pub const DAUX_CATEGORY_INSTRUMENT: u32 = 2;
/// Transforms events without producing audio.
pub const DAUX_CATEGORY_MIDI_EFFECT: u32 = 3;
/// Measures without altering the signal.
pub const DAUX_CATEGORY_ANALYZER: u32 = 4;
/// Produces audio without input.
pub const DAUX_CATEGORY_GENERATOR: u32 = 5;
/// Routing, metering and other tooling.
pub const DAUX_CATEGORY_UTILITY: u32 = 6;

/// Processes audio.
pub const DAUX_CAP_AUDIO_EFFECT: u64 = 1 << 0;
/// Generates audio from notes.
pub const DAUX_CAP_INSTRUMENT: u64 = 1 << 1;
/// Transforms events.
pub const DAUX_CAP_MIDI_EFFECT: u64 = 1 << 2;
/// Analyses without altering the signal.
pub const DAUX_CAP_ANALYZER: u64 = 1 << 3;
/// Consumes note/MIDI input.
pub const DAUX_CAP_MIDI_INPUT: u64 = 1 << 4;
/// Produces note/MIDI output.
pub const DAUX_CAP_MIDI_OUTPUT: u64 = 1 << 5;
/// Understands MIDI 2.0 / UMP events.
pub const DAUX_CAP_MIDI2: u64 = 1 << 6;
/// Has at least one sidechain input.
pub const DAUX_CAP_SIDECHAIN: u64 = 1 << 7;
/// Bus topology can change at run time.
pub const DAUX_CAP_DYNAMIC_BUSES: u64 = 1 << 8;
/// Honours sample-accurate automation events.
pub const DAUX_CAP_SAMPLE_ACCURATE_AUTO: u64 = 1 << 9;
/// Honours per-note expression events.
pub const DAUX_CAP_NOTE_EXPRESSION: u64 = 1 << 10;
/// Provides an editor.
pub const DAUX_CAP_HAS_GUI: u64 = 1 << 11;
/// Cannot be used meaningfully without its editor.
pub const DAUX_CAP_REQUIRES_GUI: u64 = 1 << 12;
/// Can present its editor through the shared-texture extension (§13).
pub const DAUX_CAP_SHARED_TEXTURE_GUI: u64 = 1 << 13;
/// Supports faster-than-real-time offline rendering.
pub const DAUX_CAP_OFFLINE_RENDER: u64 = 1 << 14;
/// Requires real-time scheduling even when the host renders offline.
pub const DAUX_CAP_HARD_REALTIME: u64 = 1 << 15;
/// Safe to run inside a sandboxed process.
pub const DAUX_CAP_SANDBOX_SAFE: u64 = 1 << 16;
/// Only supports stereo bus layouts.
pub const DAUX_CAP_STEREO_ONLY: u64 = 1 << 17;
/// Latency may change while active.
pub const DAUX_CAP_LATENCY_DYNAMIC: u64 = 1 << 18;
/// Tail may be infinite (e.g. a feedback network).
pub const DAUX_CAP_TAIL_INFINITE: u64 = 1 << 19;

/// 32-bit float samples.
pub const DAUX_SAMPLE_FORMAT_F32: u32 = 1 << 0;
/// 64-bit float samples.
pub const DAUX_SAMPLE_FORMAT_F64: u32 = 1 << 1;
