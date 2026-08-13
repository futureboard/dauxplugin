//! Bounds applied to everything that arrives from the peer process.
//!
//! A sandboxed plug-in host talks to a process it does not control. That process may have
//! crashed halfway through a write, may have been built against a different revision, or
//! may be deliberately hostile — a plug-in binary is third-party code by definition. The
//! decoder therefore never sizes an allocation from a length field alone: every length is
//! checked against both the bytes actually available *and* the bounds below, before a
//! single byte is reserved.

/// Hard bounds applied when decoding — and, symmetrically, when encoding — protocol data.
/// [any-thread]
///
/// Encoding checks the same limits so that a peer never emits a frame its counterpart is
/// obliged to reject; a bug shows up on the sender, where the context to diagnose it
/// still exists.
///
/// | Limit                                        | Default   |
/// | -------------------------------------------- | --------- |
/// | [`max_frame_bytes`](Self::max_frame_bytes)   | 16 MiB    |
/// | [`max_string_bytes`](Self::max_string_bytes) | 4 KiB     |
/// | [`max_blob_bytes`](Self::max_blob_bytes)     | 8 MiB     |
/// | [`max_channels`](Self::max_channels)         | 512       |
/// | [`max_frames`](Self::max_frames)             | 65 536    |
/// | [`max_events`](Self::max_events)             | 65 536    |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ProtocolLimits {
    /// Largest accepted control frame, header included.
    ///
    /// This is the single most important number in the crate: it is the cap that stops a
    /// hostile 4 GiB length prefix from becoming a 4 GiB allocation.
    pub max_frame_bytes: usize,
    /// Longest accepted string field, in bytes (not characters).
    pub max_string_bytes: usize,
    /// Longest accepted opaque byte field, such as a serialised plug-in state.
    pub max_blob_bytes: usize,
    /// Largest accepted channel count in a shared-memory audio block, per direction.
    pub max_channels: usize,
    /// Largest accepted block size in frames.
    pub max_frames: usize,
    /// Largest accepted event count in one shared-memory block, per direction.
    pub max_events: usize,
}

impl ProtocolLimits {
    /// 16 MiB.
    pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
    /// 4 KiB.
    pub const DEFAULT_MAX_STRING_BYTES: usize = 4 * 1024;
    /// 8 MiB.
    pub const DEFAULT_MAX_BLOB_BYTES: usize = 8 * 1024 * 1024;
    /// 512 channels.
    pub const DEFAULT_MAX_CHANNELS: usize = 512;
    /// 65 536 frames.
    pub const DEFAULT_MAX_FRAMES: usize = 65_536;
    /// 65 536 events.
    pub const DEFAULT_MAX_EVENTS: usize = 65_536;

    /// The default limits, usable in `const` context. [any-thread]
    pub const DEFAULT: Self = Self {
        max_frame_bytes: Self::DEFAULT_MAX_FRAME_BYTES,
        max_string_bytes: Self::DEFAULT_MAX_STRING_BYTES,
        max_blob_bytes: Self::DEFAULT_MAX_BLOB_BYTES,
        max_channels: Self::DEFAULT_MAX_CHANNELS,
        max_frames: Self::DEFAULT_MAX_FRAMES,
        max_events: Self::DEFAULT_MAX_EVENTS,
    };

    /// The default limits. [any-thread]
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self::DEFAULT
    }

    /// [any-thread] Returns the limits with a different maximum frame size.
    ///
    /// The value is clamped to at least [`FRAME_HEADER_LEN`](crate::FRAME_HEADER_LEN) + 1
    /// so that a limit can never make an empty frame undecodable.
    #[inline]
    #[must_use]
    pub const fn with_max_frame_bytes(mut self, bytes: usize) -> Self {
        let floor = crate::framing::FRAME_HEADER_LEN + 1;
        self.max_frame_bytes = if bytes < floor { floor } else { bytes };
        self
    }

    /// [any-thread] Returns the limits with a different maximum string length.
    #[inline]
    #[must_use]
    pub const fn with_max_string_bytes(mut self, bytes: usize) -> Self {
        self.max_string_bytes = bytes;
        self
    }

    /// [any-thread] Returns the limits with a different maximum blob length.
    #[inline]
    #[must_use]
    pub const fn with_max_blob_bytes(mut self, bytes: usize) -> Self {
        self.max_blob_bytes = bytes;
        self
    }

    /// [any-thread] Returns the limits with different audio-block bounds.
    #[inline]
    #[must_use]
    pub const fn with_audio_bounds(mut self, channels: usize, frames: usize, events: usize) -> Self {
        self.max_channels = channels;
        self.max_frames = frames;
        self.max_events = events;
        self
    }

    /// [any-thread] Largest payload that fits in a frame under these limits.
    #[inline]
    #[must_use]
    pub const fn max_payload_bytes(&self) -> usize {
        self.max_frame_bytes.saturating_sub(crate::framing::FRAME_HEADER_LEN)
    }
}

impl Default for ProtocolLimits {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::ProtocolLimits;
    use crate::framing::FRAME_HEADER_LEN;

    #[test]
    fn defaults_are_the_documented_numbers() {
        let l = ProtocolLimits::default();
        assert_eq!(l.max_frame_bytes, 16 * 1024 * 1024);
        assert_eq!(l.max_string_bytes, 4 * 1024);
        assert_eq!(l.max_blob_bytes, 8 * 1024 * 1024);
        assert_eq!(l.max_channels, 512);
        assert_eq!(l.max_frames, 65_536);
        assert_eq!(l.max_events, 65_536);
    }

    #[test]
    fn a_frame_limit_below_the_header_is_clamped_rather_than_making_decoding_impossible() {
        let l = ProtocolLimits::new().with_max_frame_bytes(0);
        assert_eq!(l.max_frame_bytes, FRAME_HEADER_LEN + 1);
        assert_eq!(l.max_payload_bytes(), 1);
    }

    #[test]
    fn builders_do_not_disturb_the_other_fields() {
        let l = ProtocolLimits::new()
            .with_max_string_bytes(7)
            .with_max_blob_bytes(9)
            .with_audio_bounds(2, 4, 8);
        assert_eq!(l.max_frame_bytes, ProtocolLimits::DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(l.max_string_bytes, 7);
        assert_eq!(l.max_blob_bytes, 9);
        assert_eq!((l.max_channels, l.max_frames, l.max_events), (2, 4, 8));
    }
}
