//! The error type shared by every fallible entry point of this crate.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Result alias used throughout this crate.
pub type BundleResult<T> = Result<T, BundleError>;

/// What went wrong while opening, parsing, resolving or writing a bundle. [main-thread]
///
/// Every variant is reachable from hostile input, and every one of them is returned
/// instead of panicking: `axt-v1` §14 requires that "parsing every metadata file with
/// hostile input produces an error, never a panic".
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BundleErrorKind {
    /// The underlying filesystem operation failed.
    Io,
    /// The path is not a directory. A regular file named `*.axt` is rejected: a v1 bundle
    /// is a directory, never an archive (`axt-v1` §1).
    NotADirectory,
    /// The directory name does not end in `.axt` (ASCII case-insensitive, `axt-v1` §2).
    NotAxtExtension,
    /// Neither `manifest.json` nor `Contents/Info.plist` is present (`axt-v1` §4 rule 4).
    NotABundle,
    /// Both `manifest.json` and `Contents/Info.plist` are present (`axt-v1` §4).
    AmbiguousLayout,
    /// The metadata file exceeds [`MAX_METADATA_BYTES`](crate::limits::MAX_METADATA_BYTES).
    TooLarge,
    /// The metadata file is not valid UTF-8, or carries a UTF-16/UTF-32 byte-order mark.
    Encoding,
    /// The document is not well-formed JSON or is not a well-formed property list.
    Parse,
    /// One object or dictionary carries the same key twice. "Last one wins" is a
    /// parser-differential bug, so it is rejected (`manifest-v1` §10.3).
    DuplicateKey,
    /// Nesting exceeds [`MAX_DEPTH`](crate::limits::MAX_DEPTH).
    DepthExceeded,
    /// A count or length limit from [`crate::limits`] would have been exceeded.
    LimitExceeded,
    /// `format` is not `"DAUx Audio Extension"`.
    WrongFormat,
    /// The bundle declares a format version this build does not implement.
    UnsupportedFormatVersion {
        /// The version the bundle declares.
        found: u32,
        /// The newest version this build understands.
        supported: u32,
    },
    /// A required key is absent.
    MissingField,
    /// A key is present but holds the wrong type, an unknown enum value, or a number
    /// outside its documented range.
    InvalidField,
    /// The plug-in id is not a well-formed reverse-DNS identifier (`manifest-v1` §3.4).
    InvalidId,
    /// A version string is not `major.minor.patch[.build]` (`manifest-v1` §3.5).
    InvalidVersion,
    /// A target identifier is malformed (`manifest-v1` §3.7).
    InvalidTarget,
    /// The `<BundleName>` violates `axt-v1` §2.
    InvalidBundleName,
    /// The bundle declares no loadable binary for the requested target. This is the normal
    /// outcome for a cross-platform bundle on a platform it does not ship, and must not be
    /// reported as corruption (`axt-v1` §9 rule 3).
    NoBinaryForTarget,
    /// More than one candidate library sits in `Content/{target}/`; a bundle must not be
    /// ambiguous about which library the host opens (`manifest-v1` §4.3).
    AmbiguousBinary,
    /// A logical path is syntactically illegal, or resolves outside the bundle after
    /// canonicalisation (`axt-v1` §10.2).
    PathEscape,
    /// The requested file does not exist inside the bundle. Deliberately distinct from
    /// [`BundleErrorKind::PathEscape`] (`axt-v1` §10.1).
    NotFound,
    /// The resolved path exists but is not a regular file. Opening a FIFO, device or
    /// socket can block indefinitely, so it is refused (`axt-v1` §10.2 rule 14).
    NotRegularFile,
    /// The requested combination is legal in the format but not supported by this build.
    Unsupported,
}

impl BundleErrorKind {
    /// [main-thread] Short, stable identifier for logs and tests.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::NotADirectory => "not-a-directory",
            Self::NotAxtExtension => "not-axt-extension",
            Self::NotABundle => "not-a-bundle",
            Self::AmbiguousLayout => "ambiguous-layout",
            Self::TooLarge => "too-large",
            Self::Encoding => "encoding",
            Self::Parse => "parse",
            Self::DuplicateKey => "duplicate-key",
            Self::DepthExceeded => "depth-exceeded",
            Self::LimitExceeded => "limit-exceeded",
            Self::WrongFormat => "wrong-format",
            Self::UnsupportedFormatVersion { .. } => "unsupported-format-version",
            Self::MissingField => "missing-field",
            Self::InvalidField => "invalid-field",
            Self::InvalidId => "invalid-id",
            Self::InvalidVersion => "invalid-version",
            Self::InvalidTarget => "invalid-target",
            Self::InvalidBundleName => "invalid-bundle-name",
            Self::NoBinaryForTarget => "no-binary-for-target",
            Self::AmbiguousBinary => "ambiguous-binary",
            Self::PathEscape => "path-escape",
            Self::NotFound => "not-found",
            Self::NotRegularFile => "not-regular-file",
            Self::Unsupported => "unsupported",
        }
    }

    /// [main-thread] The `manifest-v1` §10.5 diagnostic code this kind reports as.
    ///
    /// Codes are stable strings; tooling and tests may match on them.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Io | Self::Unsupported => "DAUX-M001",
            Self::NotABundle => "DAUX-M001",
            Self::TooLarge => "DAUX-M002",
            Self::Encoding => "DAUX-M003",
            Self::Parse => "DAUX-M004",
            Self::WrongFormat => "DAUX-M005",
            Self::UnsupportedFormatVersion { .. } => "DAUX-M006",
            Self::MissingField => "DAUX-M007",
            Self::InvalidField | Self::LimitExceeded => "DAUX-M008",
            Self::InvalidId => "DAUX-M010",
            Self::InvalidVersion => "DAUX-M011",
            Self::InvalidTarget => "DAUX-M013",
            Self::DepthExceeded => "DAUX-M018",
            Self::DuplicateKey => "DAUX-M019",
            Self::NoBinaryForTarget | Self::NotFound | Self::NotRegularFile => "DAUX-M050",
            Self::AmbiguousBinary => "DAUX-M052",
            Self::PathEscape => "DAUX-M055",
            Self::AmbiguousLayout => "DAUX-M056",
            Self::NotADirectory | Self::NotAxtExtension => "DAUX-M057",
            Self::InvalidBundleName => "DAUX-M058",
        }
    }
}

impl fmt::Display for BundleErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormatVersion { found, supported } => write!(
                f,
                "unsupported bundle format version {found} (this build implements {supported})"
            ),
            other => f.write_str(other.as_str()),
        }
    }
}

/// A bundle error, carrying the path and a human-readable detail. [main-thread]
///
/// Nothing in this crate is reachable from the audio thread, so allocating a message here
/// is free of real-time concerns. Hosts that need a status code use
/// [`BundleErrorKind::code`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleError {
    kind: BundleErrorKind,
    path: Option<PathBuf>,
    detail: Option<String>,
}

impl BundleError {
    /// [main-thread] Builds an error of `kind` with a free-form detail message.
    #[must_use]
    pub fn new(kind: BundleErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            path: None,
            detail: Some(detail.into()),
        }
    }

    /// [main-thread] Builds an error of `kind` with no detail.
    #[must_use]
    pub const fn bare(kind: BundleErrorKind) -> Self {
        Self {
            kind,
            path: None,
            detail: None,
        }
    }

    /// [main-thread] Attaches the path the error is about.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// [main-thread] Attaches the path only if one is not already recorded.
    #[must_use]
    pub fn or_path(mut self, path: impl Into<PathBuf>) -> Self {
        if self.path.is_none() {
            self.path = Some(path.into());
        }
        self
    }

    /// [main-thread] Wraps an [`io::Error`] raised while touching `path`.
    #[must_use]
    pub fn io(path: impl Into<PathBuf>, source: &io::Error) -> Self {
        let kind = match source.kind() {
            io::ErrorKind::NotFound => BundleErrorKind::NotFound,
            _ => BundleErrorKind::Io,
        };
        Self {
            kind,
            path: Some(path.into()),
            detail: Some(source.to_string()),
        }
    }

    /// [main-thread] The classification of this failure.
    #[must_use]
    pub const fn kind(&self) -> &BundleErrorKind {
        &self.kind
    }

    /// [main-thread] The stable `manifest-v1` §10.5 code for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// [main-thread] The path the error is about, when one is known.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// [main-thread] The free-form detail, when one was recorded.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.kind.code(), self.kind)?;
        if let Some(detail) = &self.detail {
            write!(f, ": {detail}")?;
        }
        if let Some(path) = &self.path {
            write!(f, " ({})", path.display())?;
        }
        Ok(())
    }
}

impl Error for BundleError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_stable_for_every_kind() {
        // Every kind maps to a documented `manifest-v1` §10.5 code.
        let kinds = [
            BundleErrorKind::Io,
            BundleErrorKind::NotADirectory,
            BundleErrorKind::NotAxtExtension,
            BundleErrorKind::NotABundle,
            BundleErrorKind::AmbiguousLayout,
            BundleErrorKind::TooLarge,
            BundleErrorKind::Encoding,
            BundleErrorKind::Parse,
            BundleErrorKind::DuplicateKey,
            BundleErrorKind::DepthExceeded,
            BundleErrorKind::LimitExceeded,
            BundleErrorKind::WrongFormat,
            BundleErrorKind::UnsupportedFormatVersion {
                found: 2,
                supported: 1,
            },
            BundleErrorKind::MissingField,
            BundleErrorKind::InvalidField,
            BundleErrorKind::InvalidId,
            BundleErrorKind::InvalidVersion,
            BundleErrorKind::InvalidTarget,
            BundleErrorKind::InvalidBundleName,
            BundleErrorKind::NoBinaryForTarget,
            BundleErrorKind::AmbiguousBinary,
            BundleErrorKind::PathEscape,
            BundleErrorKind::NotFound,
            BundleErrorKind::NotRegularFile,
            BundleErrorKind::Unsupported,
        ];
        for kind in kinds {
            let code = kind.code();
            assert!(code.starts_with("DAUX-M"), "{code}");
            // `DAUX-M` plus exactly three digits, e.g. `DAUX-M055`.
            assert_eq!(code.len(), 9, "{code}");
            assert!(
                code[6..].bytes().all(|b| b.is_ascii_digit()),
                "{code} must end in three digits"
            );
            assert!(!kind.as_str().is_empty());
        }
    }

    #[test]
    fn display_includes_code_detail_and_path() {
        let err = BundleError::new(BundleErrorKind::PathEscape, "`..` component")
            .with_path("C:/plugins/Gain.axt");
        let text = err.to_string();
        assert!(text.contains("DAUX-M055"), "{text}");
        assert!(text.contains("`..` component"), "{text}");
        assert!(text.contains("Gain.axt"), "{text}");
        assert_eq!(err.kind(), &BundleErrorKind::PathEscape);
        assert_eq!(err.detail(), Some("`..` component"));
        assert!(err.path().is_some());
    }

    #[test]
    fn io_not_found_maps_to_not_found_kind() {
        let source = io::Error::new(io::ErrorKind::NotFound, "nope");
        let err = BundleError::io("x", &source);
        assert_eq!(err.kind(), &BundleErrorKind::NotFound);

        let source = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        let err = BundleError::io("x", &source);
        assert_eq!(err.kind(), &BundleErrorKind::Io);
    }

    #[test]
    fn or_path_does_not_overwrite() {
        let err = BundleError::bare(BundleErrorKind::Parse)
            .with_path("first")
            .or_path("second");
        assert_eq!(err.path(), Some(Path::new("first")));
        assert_eq!(err.detail(), None);
    }

    #[test]
    fn unsupported_version_displays_both_numbers() {
        let err = BundleError::bare(BundleErrorKind::UnsupportedFormatVersion {
            found: 7,
            supported: 1,
        });
        let text = err.to_string();
        assert!(text.contains('7') && text.contains('1'), "{text}");
    }
}
