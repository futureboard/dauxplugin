//! Transport / musical timeline (`abi-v1` §10).
//!
//! A field is meaningful only when its `DAUX_TRANSPORT_HAS_*` flag is set. Hosts MUST NOT
//! fabricate values; plug-ins MUST NOT read unflagged fields.

use crate::compat::{impl_abi_default, impl_abi_struct};

/// `tempo` and `tempo_increment` are valid.
pub const DAUX_TRANSPORT_HAS_TEMPO: u32 = 1 << 0;
/// `song_pos_beats` is valid.
pub const DAUX_TRANSPORT_HAS_BEATS: u32 = 1 << 1;
/// `song_pos_seconds` is valid.
pub const DAUX_TRANSPORT_HAS_SECONDS: u32 = 1 << 2;
/// `time_sig_numerator` and `time_sig_denominator` are valid.
pub const DAUX_TRANSPORT_HAS_TIME_SIG: u32 = 1 << 3;
/// The four `loop_*` fields are valid.
pub const DAUX_TRANSPORT_HAS_LOOP: u32 = 1 << 4;
/// `bar_start_beats` and `bar_number` are valid.
pub const DAUX_TRANSPORT_HAS_BAR: u32 = 1 << 5;
/// The transport is rolling.
pub const DAUX_TRANSPORT_IS_PLAYING: u32 = 1 << 6;
/// The host is recording.
pub const DAUX_TRANSPORT_IS_RECORDING: u32 = 1 << 7;
/// Loop playback is enabled.
pub const DAUX_TRANSPORT_IS_LOOPING: u32 = 1 << 8;
/// The host is playing pre-roll before the punch-in point.
pub const DAUX_TRANSPORT_IS_PREROLL: u32 = 1 << 9;

/// Host transport state for the current block.
///
/// [audio-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxTransportV1 {
    /// `size_of::<DauxTransportV1>()` as written by the producer.
    pub size: u32,
    /// Bitset of `DAUX_TRANSPORT_*`.
    pub flags: u32,

    /// Playhead position in samples.
    pub song_pos_samples: i64,
    /// Playhead position in quarter notes. Requires [`DAUX_TRANSPORT_HAS_BEATS`].
    pub song_pos_beats: f64,
    /// Playhead position in seconds. Requires [`DAUX_TRANSPORT_HAS_SECONDS`].
    pub song_pos_seconds: f64,

    /// Tempo in BPM. Requires [`DAUX_TRANSPORT_HAS_TEMPO`].
    pub tempo: f64,
    /// Tempo change in BPM per sample, `0.0` when steady. Requires
    /// [`DAUX_TRANSPORT_HAS_TEMPO`].
    pub tempo_increment: f64,

    /// Beat position of the current bar's downbeat. Requires [`DAUX_TRANSPORT_HAS_BAR`].
    pub bar_start_beats: f64,
    /// Index of the current bar. Requires [`DAUX_TRANSPORT_HAS_BAR`].
    pub bar_number: i32,
    /// Time signature numerator. Requires [`DAUX_TRANSPORT_HAS_TIME_SIG`].
    pub time_sig_numerator: u16,
    /// Time signature denominator. Requires [`DAUX_TRANSPORT_HAS_TIME_SIG`].
    pub time_sig_denominator: u16,

    /// Loop start in quarter notes. Requires [`DAUX_TRANSPORT_HAS_LOOP`].
    pub loop_start_beats: f64,
    /// Loop end in quarter notes. Requires [`DAUX_TRANSPORT_HAS_LOOP`].
    pub loop_end_beats: f64,
    /// Loop start in seconds. Requires [`DAUX_TRANSPORT_HAS_LOOP`].
    pub loop_start_seconds: f64,
    /// Loop end in seconds. Requires [`DAUX_TRANSPORT_HAS_LOOP`].
    pub loop_end_seconds: f64,

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 6],
}

impl DauxTransportV1 {
    /// [audio-thread] An all-zero transport with `size` set and no `HAS_*` flag raised,
    /// i.e. a host that exposes no timeline information at all.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        // SAFETY: every field is a plain integer, a float or an array of `usize`; there is
        // no pointer, reference, function pointer or enum among them, so the all-zero bit
        // pattern is a valid, fully initialised value. Zeroing also clears any implicit
        // padding, which satisfies "a writer MUST zero every field it does not populate"
        // (`abi-v1` §3) byte for byte.
        let mut this: Self = unsafe { core::mem::zeroed() };
        this.size = Self::SIZE;
        this
    }

    /// [audio-thread] `true` when every flag bit in `flags` is set.
    ///
    /// ```
    /// # use daux_abi::{DauxTransportV1, DAUX_TRANSPORT_HAS_TEMPO};
    /// let mut t = DauxTransportV1::new();
    /// assert!(!t.has_flags(DAUX_TRANSPORT_HAS_TEMPO));
    /// t.flags |= DAUX_TRANSPORT_HAS_TEMPO;
    /// t.tempo = 120.0;
    /// assert!(t.has_flags(DAUX_TRANSPORT_HAS_TEMPO));
    /// ```
    #[inline]
    #[must_use]
    pub const fn has_flags(&self, flags: u32) -> bool {
        self.flags & flags == flags
    }
}

impl_abi_struct!(DauxTransportV1);
impl_abi_default!(DauxTransportV1);
