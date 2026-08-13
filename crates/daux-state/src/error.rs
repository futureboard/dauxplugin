//! Errors produced while writing, parsing or migrating state.

use core::fmt;
use std::error::Error;
use std::io;

use crate::value::ValueType;

/// Result alias used throughout this crate.
pub type StateResult<T> = Result<T, StateError>;

/// What went wrong. [main-thread]
///
/// Adapters map these onto the ABI status codes of `docs/specifications/abi-v1.md` §2:
/// [`StateErrorKind::UnsupportedVersion`] is `DAUX_ERR_VERSION`, everything else is an
/// invalid-argument or I/O failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StateErrorKind {
    /// The underlying reader or writer failed.
    Io,
    /// The bytes are not a well-formed state container: bad magic, a length prefix that
    /// runs past the end of the input, an unknown type tag, unbalanced groups, invalid
    /// UTF-8, trailing garbage.
    Corrupt,
    /// The document was written by a newer build than this one can read.
    UnsupportedVersion {
        /// The version found in the document.
        found: u32,
        /// The newest version this build understands.
        supported: u32,
    },
    /// The requested key is not in the document.
    MissingField,
    /// The key exists but holds a different type.
    TypeMismatch {
        /// The type the caller asked for.
        expected: ValueType,
        /// The type actually stored.
        found: ValueType,
    },
    /// A key was empty or contained the [`crate::format::PATH_SEPARATOR`]. An over-long
    /// key is a [`StateErrorKind::LimitExceeded`] instead, so that the writer and the
    /// reader agree on the kind.
    InvalidKey,
    /// A [`StateLimits`](crate::StateLimits) bound would have been exceeded. Hitting this
    /// on untrusted input is the intended outcome, not a bug.
    LimitExceeded,
    /// A [`MigrationChain`](crate::MigrationChain) could not bring the document up to the
    /// current version.
    Migration,
}

impl StateErrorKind {
    /// Short, stable identifier for logs. [main-thread]
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Corrupt => "corrupt",
            Self::UnsupportedVersion { .. } => "unsupported-version",
            Self::MissingField => "missing-field",
            Self::TypeMismatch { .. } => "type-mismatch",
            Self::InvalidKey => "invalid-key",
            Self::LimitExceeded => "limit-exceeded",
            Self::Migration => "migration",
        }
    }
}

/// A state error, carrying enough context to be acted on: which key, which byte, and what
/// the limit or expected type was. [main-thread]
///
/// Messages are built lazily by [`fmt::Display`], so constructing one on a non-fatal path
/// is cheap; nothing here is ever produced on the audio thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateError {
    kind: StateErrorKind,
    key: Option<String>,
    offset: Option<usize>,
    detail: Option<String>,
}

impl StateError {
    /// Builds an error of `kind` with a free-form detail message. [main-thread]
    #[must_use]
    pub fn new(kind: StateErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            key: None,
            offset: None,
            detail: Some(detail.into()),
        }
    }

    /// The blob is not a well-formed container. [main-thread]
    #[must_use]
    pub fn corrupt(detail: impl Into<String>) -> Self {
        Self::new(StateErrorKind::Corrupt, detail)
    }

    /// A key the caller asked for is not present. [main-thread]
    #[must_use]
    pub fn missing_field(key: &str) -> Self {
        Self {
            kind: StateErrorKind::MissingField,
            key: Some(key.to_owned()),
            offset: None,
            detail: None,
        }
    }

    /// A key exists but holds the wrong type. [main-thread]
    #[must_use]
    pub fn type_mismatch(key: &str, expected: ValueType, found: ValueType) -> Self {
        Self {
            kind: StateErrorKind::TypeMismatch { expected, found },
            key: Some(key.to_owned()),
            offset: None,
            detail: None,
        }
    }

    /// A key is empty, over-long or contains the path separator. [main-thread]
    #[must_use]
    pub fn invalid_key(key: &str, detail: impl Into<String>) -> Self {
        Self {
            kind: StateErrorKind::InvalidKey,
            key: Some(key.to_owned()),
            offset: None,
            detail: Some(detail.into()),
        }
    }

    /// A configured bound would have been exceeded. [main-thread]
    #[must_use]
    pub fn limit_exceeded(detail: impl Into<String>) -> Self {
        Self::new(StateErrorKind::LimitExceeded, detail)
    }

    /// The document is newer than this build. [main-thread]
    #[must_use]
    pub fn unsupported_version(found: u32, supported: u32) -> Self {
        Self {
            kind: StateErrorKind::UnsupportedVersion { found, supported },
            key: None,
            offset: None,
            detail: None,
        }
    }

    /// A migration step failed or is missing. [main-thread]
    #[must_use]
    pub fn migration(detail: impl Into<String>) -> Self {
        Self::new(StateErrorKind::Migration, detail)
    }

    /// Attaches the key this error is about. [main-thread]
    #[must_use]
    pub fn with_key(mut self, key: &str) -> Self {
        self.key = Some(key.to_owned());
        self
    }

    /// Attaches the byte offset in the blob where the problem was found. [main-thread]
    #[must_use]
    pub fn at_offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    /// What kind of failure this is. [main-thread]
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> &StateErrorKind {
        &self.kind
    }

    /// The key involved, when the failure is about one. [main-thread]
    #[inline]
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// The byte offset in the blob, when the failure is about parsing one. [main-thread]
    #[inline]
    #[must_use]
    pub const fn offset(&self) -> Option<usize> {
        self.offset
    }

    /// The free-form detail message, if any. [main-thread]
    #[inline]
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            StateErrorKind::Io => f.write_str("state i/o failed")?,
            StateErrorKind::Corrupt => f.write_str("corrupt state")?,
            StateErrorKind::UnsupportedVersion { found, supported } => write!(
                f,
                "state version {found} is newer than this build understands (up to {supported}); \
                 install a newer version of the plug-in"
            )?,
            StateErrorKind::MissingField => f.write_str("missing state entry")?,
            StateErrorKind::TypeMismatch { expected, found } => {
                write!(f, "state entry holds {found}, expected {expected}")?;
            }
            StateErrorKind::InvalidKey => f.write_str("invalid state key")?,
            StateErrorKind::LimitExceeded => f.write_str("state limit exceeded")?,
            StateErrorKind::Migration => f.write_str("state migration failed")?,
        }
        if let Some(key) = &self.key {
            write!(f, " for key {key:?}")?;
        }
        if let Some(offset) = self.offset {
            write!(f, " at byte {offset}")?;
        }
        if let Some(detail) = &self.detail {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

impl Error for StateError {}

impl From<io::Error> for StateError {
    fn from(e: io::Error) -> Self {
        Self::new(StateErrorKind::Io, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_the_key() {
        let e = StateError::missing_field("filter/cutoff");
        let text = e.to_string();
        assert!(text.contains("filter/cutoff"), "{text}");
        assert!(text.contains("missing"), "{text}");
        assert_eq!(e.key(), Some("filter/cutoff"));
        assert_eq!(e.kind(), &StateErrorKind::MissingField);
    }

    #[test]
    fn display_names_both_types() {
        let e = StateError::type_mismatch("gain", ValueType::F64, ValueType::Bytes);
        let text = e.to_string();
        assert!(text.contains("gain"), "{text}");
        assert!(text.contains("bytes"), "{text}");
        assert!(text.contains("f64"), "{text}");
    }

    #[test]
    fn display_reports_the_byte_offset() {
        let e =
            StateError::corrupt("value length 4000 exceeds the 3 remaining bytes").at_offset(37);
        let text = e.to_string();
        assert!(text.contains("byte 37"), "{text}");
        assert!(text.contains("4000"), "{text}");
        assert_eq!(e.offset(), Some(37));
    }

    #[test]
    fn display_of_every_kind_is_non_empty() {
        let errors = [
            StateError::new(StateErrorKind::Io, "disk full"),
            StateError::corrupt("bad magic"),
            StateError::unsupported_version(9, 1),
            StateError::missing_field("a"),
            StateError::type_mismatch("a", ValueType::Bool, ValueType::Str),
            StateError::invalid_key("a/b", "keys may not contain '/'"),
            StateError::limit_exceeded("too big"),
            StateError::migration("no step from v1"),
        ];
        for e in &errors {
            assert!(!e.to_string().is_empty());
            assert!(!e.kind().as_str().is_empty());
        }
    }

    #[test]
    fn unsupported_version_reports_both_numbers() {
        let text = StateError::unsupported_version(9, 3).to_string();
        assert!(text.contains('9'), "{text}");
        assert!(text.contains('3'), "{text}");
    }

    #[test]
    fn is_a_std_error() {
        fn takes_error(_: &dyn Error) {}
        let e = StateError::corrupt("x");
        takes_error(&e);
        assert!(e.source().is_none());
    }

    #[test]
    fn io_errors_convert() {
        let io = io::Error::new(io::ErrorKind::UnexpectedEof, "short read");
        let e = StateError::from(io);
        assert_eq!(e.kind(), &StateErrorKind::Io);
        assert!(e.to_string().contains("short read"));
    }

    #[test]
    fn with_key_attaches_context() {
        let e = StateError::corrupt("bad bool byte")
            .with_key("bypass")
            .at_offset(4);
        assert_eq!(e.key(), Some("bypass"));
        assert_eq!(e.offset(), Some(4));
        assert_eq!(e.detail(), Some("bad bool byte"));
    }
}
