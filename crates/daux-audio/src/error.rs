//! Errors returned by the checked helpers in this crate.

use core::fmt;

/// Failure of a checked buffer or bus-layout operation.
///
/// Every variant is a programming or host-integration error, never a normal condition,
/// and constructing one never allocates — so returning an [`AudioError`] from the audio
/// thread is real-time safe. `[any-thread]`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioError {
    /// A caller-provided flat buffer was too small.
    SizeMismatch {
        /// Number of samples the operation needed.
        expected: usize,
        /// Number of samples the caller supplied.
        found: usize,
    },
    /// Two buffers disagreed on the number of channels.
    ChannelCountMismatch {
        /// Channel count of the destination.
        expected: usize,
        /// Channel count of the source.
        found: usize,
    },
    /// Two buffers disagreed on the number of frames.
    FrameCountMismatch {
        /// Frame count of the destination.
        expected: usize,
        /// Frame count of the source.
        found: usize,
    },
    /// An interleaved buffer length was not a whole number of frames.
    NotDivisible {
        /// Length of the interleaved buffer, in samples.
        len: usize,
        /// Channel count it was supposed to be divisible by.
        channels: usize,
    },
    /// An interleaved conversion was requested with zero channels.
    ZeroChannels,
    /// The same bus id appeared twice in one direction of a [`BusLayout`]; the payload is
    /// the offending id.
    ///
    /// [`BusLayout`]: crate::BusLayout
    DuplicateBusId(u32),
    /// More than one bus in one direction carried [`BusFlags::IS_MAIN`].
    ///
    /// [`BusFlags::IS_MAIN`]: crate::BusFlags::IS_MAIN
    MultipleMainBuses,
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::SizeMismatch { expected, found } => {
                write!(f, "buffer too small: need {expected} samples, got {found}")
            }
            Self::ChannelCountMismatch { expected, found } => {
                write!(
                    f,
                    "channel count mismatch: expected {expected}, got {found}"
                )
            }
            Self::FrameCountMismatch { expected, found } => {
                write!(f, "frame count mismatch: expected {expected}, got {found}")
            }
            Self::NotDivisible { len, channels } => write!(
                f,
                "interleaved length {len} is not a multiple of {channels} channels"
            ),
            Self::ZeroChannels => f.write_str("interleaved conversion needs at least one channel"),
            Self::DuplicateBusId(id) => write!(f, "duplicate bus id {id}"),
            Self::MultipleMainBuses => f.write_str("more than one main bus in one direction"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Convenient alias for fallible operations in this crate.
pub type AudioResult<T> = Result<T, AudioError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_non_empty_for_every_variant() {
        let all = [
            AudioError::SizeMismatch {
                expected: 4,
                found: 2,
            },
            AudioError::ChannelCountMismatch {
                expected: 2,
                found: 1,
            },
            AudioError::FrameCountMismatch {
                expected: 8,
                found: 9,
            },
            AudioError::NotDivisible {
                len: 5,
                channels: 2,
            },
            AudioError::ZeroChannels,
            AudioError::DuplicateBusId(7),
            AudioError::MultipleMainBuses,
        ];
        for e in all {
            assert!(!e.to_string().is_empty());
            // Round-trips through the `Error` trait object without allocating a message.
            let dynamic: &dyn std::error::Error = &e;
            assert!(dynamic.source().is_none());
        }
    }

    #[test]
    fn errors_compare_by_value() {
        assert_eq!(
            AudioError::SizeMismatch {
                expected: 1,
                found: 0
            },
            AudioError::SizeMismatch {
                expected: 1,
                found: 0
            }
        );
        assert_ne!(
            AudioError::ZeroChannels,
            AudioError::DuplicateBusId(u32::MAX)
        );
    }
}
