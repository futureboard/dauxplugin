//! The error type of the DAUx object model, and its mapping onto ABI status
//! codes.

use core::fmt;
use std::borrow::Cow;

use daux_state::{StateError, StateErrorKind};

/// The `DauxStatus` values of `docs/specifications/abi-v1.md` §2.
///
/// `daux-core` deliberately does not depend on `daux-abi` — the object model
/// must stay free of the binary contract — so the numbers are restated here.
/// They are part of the ABI and can never change; `error::tests` asserts every
/// one of them against the specification.
///
/// `[any-thread]`
pub mod status {
    /// Success. Mirrors `DAUX_OK`.
    pub const OK: i32 = 0;
    /// Unclassified failure. Mirrors `DAUX_ERR_UNKNOWN`. No [`ErrorKind`]
    /// produces it; it is what a caller reports when it has nothing better.
    ///
    /// [`ErrorKind`]: super::ErrorKind
    pub const UNKNOWN: i32 = -1;
    /// Mirrors `DAUX_ERR_INVALID_ARG`.
    pub const INVALID_ARG: i32 = -2;
    /// Mirrors `DAUX_ERR_UNSUPPORTED`.
    pub const UNSUPPORTED: i32 = -3;
    /// Mirrors `DAUX_ERR_OUT_OF_MEMORY`.
    pub const OUT_OF_MEMORY: i32 = -4;
    /// Mirrors `DAUX_ERR_INVALID_STATE`.
    pub const INVALID_STATE: i32 = -5;
    /// Mirrors `DAUX_ERR_WRONG_THREAD`.
    pub const WRONG_THREAD: i32 = -6;
    /// Mirrors `DAUX_ERR_NOT_REALTIME`.
    pub const NOT_REALTIME: i32 = -7;
    /// Mirrors `DAUX_ERR_ABI_MISMATCH`.
    pub const ABI_MISMATCH: i32 = -8;
    /// Mirrors `DAUX_ERR_VERSION`.
    pub const VERSION: i32 = -9;
    /// Mirrors `DAUX_ERR_NOT_FOUND`.
    pub const NOT_FOUND: i32 = -10;
    /// Mirrors `DAUX_ERR_IO`.
    pub const IO: i32 = -11;
    /// Mirrors `DAUX_ERR_GRAPHICS`.
    pub const GRAPHICS: i32 = -12;
    /// Mirrors `DAUX_ERR_HOST`.
    pub const HOST: i32 = -13;
    /// Mirrors `DAUX_ERR_PLUGIN`.
    pub const PLUGIN: i32 = -14;
    /// A panic was caught at an FFI boundary. Mirrors `DAUX_ERR_PANIC`.
    ///
    /// No [`ErrorKind`] maps to it: a panic is not a modelled error, it is the
    /// adapter's `catch_unwind` reporting that the instance is poisoned
    /// (abi-v1 §17).
    ///
    /// [`ErrorKind`]: super::ErrorKind
    pub const PANIC: i32 = -15;
    /// Mirrors `DAUX_ERR_INTERNAL`.
    pub const INTERNAL: i32 = -16;
}

/// What went wrong, in terms the whole SDK shares. `[any-thread]`
///
/// Each variant maps to exactly one ABI status code (see
/// [`status_code`](ErrorKind::status_code)), so an adapter converts a
/// [`DauxError`] into a `DauxStatus` without a lookup table and without losing
/// meaning. The mapping is a fixed part of the contract: adding a variant is a
/// design change, and renumbering one is a breaking change to every host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorKind {
    /// A caller passed something the callee cannot accept: an out-of-range
    /// index, a malformed id, a NaN sample rate, a corrupt state blob.
    InvalidArgument,
    /// The operation is understood but not implemented — an unsupported bus
    /// layout, `f64` processing in an `f32`-only plug-in, an unknown extension.
    Unsupported,
    /// An allocation failed, or a bounded resource is exhausted.
    OutOfMemory,
    /// The object is in the wrong lifecycle state: `process` before `activate`,
    /// `activate` twice, `prepare` while processing (abi-v1 §7).
    InvalidState,
    /// The call arrived on a thread that is not allowed to make it.
    WrongThread,
    /// The operation cannot be performed under real-time constraints — it would
    /// allocate, lock or block, so it was refused rather than attempted.
    NotRealtimeSafe,
    /// A module speaks a different ABI: bad magic, wrong major version, a
    /// structure smaller than its minimum v1.0 size.
    AbiMismatch,
    /// A version could not be reconciled: state written by a newer build, a
    /// migration chain that does not reach the current schema (abi-v1 §12).
    VersionMismatch,
    /// The thing asked for does not exist: a plug-in id no factory knows, a
    /// missing parameter, a missing state key or resource.
    NotFound,
    /// The filesystem, a stream or a bundle failed.
    Io,
    /// The editor, its window or its GPU surface failed.
    Graphics,
    /// The host violated the contract, or a host service failed.
    Host,
    /// The plug-in violated the contract, or a plug-in callback failed.
    Plugin,
    /// A bug on this side of the boundary — a broken invariant, not a
    /// recoverable condition.
    Internal,
}

impl ErrorKind {
    /// Every kind, in declaration order. `[any-thread]`
    pub const ALL: [ErrorKind; 14] = [
        ErrorKind::InvalidArgument,
        ErrorKind::Unsupported,
        ErrorKind::OutOfMemory,
        ErrorKind::InvalidState,
        ErrorKind::WrongThread,
        ErrorKind::NotRealtimeSafe,
        ErrorKind::AbiMismatch,
        ErrorKind::VersionMismatch,
        ErrorKind::NotFound,
        ErrorKind::Io,
        ErrorKind::Graphics,
        ErrorKind::Host,
        ErrorKind::Plugin,
        ErrorKind::Internal,
    ];

    /// The ABI status code for this kind. `[any-thread]`
    ///
    /// Always negative, and never [`status::OK`].
    #[must_use]
    pub const fn status_code(self) -> i32 {
        match self {
            ErrorKind::InvalidArgument => status::INVALID_ARG,
            ErrorKind::Unsupported => status::UNSUPPORTED,
            ErrorKind::OutOfMemory => status::OUT_OF_MEMORY,
            ErrorKind::InvalidState => status::INVALID_STATE,
            ErrorKind::WrongThread => status::WRONG_THREAD,
            ErrorKind::NotRealtimeSafe => status::NOT_REALTIME,
            ErrorKind::AbiMismatch => status::ABI_MISMATCH,
            ErrorKind::VersionMismatch => status::VERSION,
            ErrorKind::NotFound => status::NOT_FOUND,
            ErrorKind::Io => status::IO,
            ErrorKind::Graphics => status::GRAPHICS,
            ErrorKind::Host => status::HOST,
            ErrorKind::Plugin => status::PLUGIN,
            ErrorKind::Internal => status::INTERNAL,
        }
    }

    /// The kind a status code denotes, or `None`. `[any-thread]`
    ///
    /// `None` covers [`status::OK`] (not an error at all), [`status::UNKNOWN`]
    /// and [`status::PANIC`] (no modelled kind), and any code from a newer ABI.
    /// Treat `None` from a negative code as [`ErrorKind::Internal`] if you need
    /// a total function.
    #[must_use]
    pub const fn from_status_code(code: i32) -> Option<Self> {
        match code {
            status::INVALID_ARG => Some(ErrorKind::InvalidArgument),
            status::UNSUPPORTED => Some(ErrorKind::Unsupported),
            status::OUT_OF_MEMORY => Some(ErrorKind::OutOfMemory),
            status::INVALID_STATE => Some(ErrorKind::InvalidState),
            status::WRONG_THREAD => Some(ErrorKind::WrongThread),
            status::NOT_REALTIME => Some(ErrorKind::NotRealtimeSafe),
            status::ABI_MISMATCH => Some(ErrorKind::AbiMismatch),
            status::VERSION => Some(ErrorKind::VersionMismatch),
            status::NOT_FOUND => Some(ErrorKind::NotFound),
            status::IO => Some(ErrorKind::Io),
            status::GRAPHICS => Some(ErrorKind::Graphics),
            status::HOST => Some(ErrorKind::Host),
            status::PLUGIN => Some(ErrorKind::Plugin),
            status::INTERNAL => Some(ErrorKind::Internal),
            _ => None,
        }
    }

    /// Short, stable identifier for logs and CLI output. `[any-thread]`
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorKind::InvalidArgument => "invalid-argument",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::OutOfMemory => "out-of-memory",
            ErrorKind::InvalidState => "invalid-state",
            ErrorKind::WrongThread => "wrong-thread",
            ErrorKind::NotRealtimeSafe => "not-realtime-safe",
            ErrorKind::AbiMismatch => "abi-mismatch",
            ErrorKind::VersionMismatch => "version-mismatch",
            ErrorKind::NotFound => "not-found",
            ErrorKind::Io => "io",
            ErrorKind::Graphics => "graphics",
            ErrorKind::Host => "host",
            ErrorKind::Plugin => "plugin",
            ErrorKind::Internal => "internal",
        }
    }

    /// Builds an error of this kind. `[main-thread]` — allocates the message.
    #[must_use]
    pub fn error(self, message: impl Into<String>) -> DauxError {
        DauxError::new(self, message)
    }

    /// Builds an error of this kind from a literal, without allocating.
    /// `[any-thread]`
    #[must_use]
    pub const fn with_static(self, message: &'static str) -> DauxError {
        DauxError::from_static(self, message)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The error type every fallible DAUx operation returns. `[main-thread]`
///
/// It pairs a machine-readable [`ErrorKind`] with a human-readable message.
/// Adapters turn it into an integer at the ABI boundary with
/// [`status_code`](DauxError::status_code); the message stays on this side of
/// the boundary, where it can be logged, because ABI v1 has no mechanism for
/// returning error text and inventing one would mean allocating across a module
/// boundary (abi-v1 §16.2).
///
/// # Never on the audio thread
///
/// `process` returns [`ProcessStatus`](crate::ProcessStatus), not a `Result`,
/// precisely so that no error path on the audio thread has to build a message.
/// If you must construct one from real-time-adjacent code, use
/// [`DauxError::from_static`], which stores a `&'static str` and allocates
/// nothing.
///
/// ```
/// use daux_core::{DauxError, ErrorKind, status};
///
/// let e = DauxError::new(ErrorKind::Unsupported, "64-bit processing");
/// assert_eq!(e.kind(), ErrorKind::Unsupported);
/// assert_eq!(e.status_code(), status::UNSUPPORTED);
/// assert_eq!(e.to_string(), "unsupported: 64-bit processing");
///
/// // Allocation-free construction from a literal.
/// const REFUSED: DauxError = DauxError::from_static(ErrorKind::WrongThread, "not the main thread");
/// assert_eq!(REFUSED.message(), "not the main thread");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DauxError {
    kind: ErrorKind,
    message: Cow<'static, str>,
}

impl DauxError {
    /// Builds an error. `[main-thread]` — allocates unless the message is
    /// already owned.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: Cow::Owned(message.into()),
        }
    }

    /// Builds an error from a literal without allocating. `[any-thread]`
    #[must_use]
    pub const fn from_static(kind: ErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message: Cow::Borrowed(message),
        }
    }

    /// What kind of failure this is. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The human-readable detail. May be empty. `[any-thread]`
    #[inline]
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The ABI status code to return across the boundary. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn status_code(&self) -> i32 {
        self.kind.status_code()
    }

    /// Replaces the kind, keeping the message. `[any-thread]`
    ///
    /// Useful when a lower layer's classification is wrong for the caller — a
    /// missing state key is [`ErrorKind::NotFound`] to `daux-state` but
    /// [`ErrorKind::InvalidArgument`] to a plug-in that required it.
    #[must_use]
    pub fn with_kind(mut self, kind: ErrorKind) -> Self {
        self.kind = kind;
        self
    }

    /// Prefixes the message with `context`. `[main-thread]` — allocates.
    #[must_use]
    pub fn context(self, context: impl fmt::Display) -> Self {
        Self::new(self.kind, format!("{context}: {}", self.message))
    }
}

impl fmt::Display for DauxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            f.write_str(self.kind.as_str())
        } else {
            write!(f, "{}: {}", self.kind.as_str(), self.message)
        }
    }
}

impl std::error::Error for DauxError {}

impl From<ErrorKind> for DauxError {
    /// An error with no detail beyond its kind.
    fn from(kind: ErrorKind) -> Self {
        Self::from_static(kind, "")
    }
}

impl From<std::io::Error> for DauxError {
    /// Filesystem and stream failures become [`ErrorKind::Io`], except
    /// `NotFound`, which is more useful as [`ErrorKind::NotFound`].
    fn from(e: std::io::Error) -> Self {
        let kind = match e.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::NotFound,
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => {
                ErrorKind::InvalidArgument
            }
            std::io::ErrorKind::OutOfMemory => ErrorKind::OutOfMemory,
            _ => ErrorKind::Io,
        };
        Self::new(kind, e.to_string())
    }
}

impl From<StateError> for DauxError {
    /// Maps `daux-state`'s classification onto the model's.
    ///
    /// The version cases matter most: abi-v1 §12 requires a plug-in that cannot
    /// load a schema version to fail with `DAUX_ERR_VERSION` and no side
    /// effects, which is exactly what [`ErrorKind::VersionMismatch`] produces.
    fn from(e: StateError) -> Self {
        let kind = match e.kind() {
            StateErrorKind::Io => ErrorKind::Io,
            StateErrorKind::UnsupportedVersion { .. } | StateErrorKind::Migration => {
                ErrorKind::VersionMismatch
            }
            StateErrorKind::MissingField => ErrorKind::NotFound,
            StateErrorKind::Corrupt
            | StateErrorKind::TypeMismatch { .. }
            | StateErrorKind::InvalidKey
            | StateErrorKind::LimitExceeded => ErrorKind::InvalidArgument,
            // `StateErrorKind` is `#[non_exhaustive]`. A kind added later must degrade to
            // something a host can act on rather than stopping this crate from compiling;
            // "the blob was not acceptable" is the honest reading of an unknown state error.
            _ => ErrorKind::InvalidArgument,
        };
        Self::new(kind, e.to_string())
    }
}

/// The result type of every fallible DAUx operation. `[main-thread]`
pub type DauxResult<T> = Result<T, DauxError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the mapping: every kind lands on the number
    /// `docs/specifications/abi-v1.md` §2 assigns it. These literals are
    /// transcribed from the specification, not from `status`, so that a typo in
    /// one place cannot hide behind the other.
    #[test]
    fn every_kind_maps_to_the_documented_status_code() {
        assert_eq!(ErrorKind::InvalidArgument.status_code(), -2);
        assert_eq!(ErrorKind::Unsupported.status_code(), -3);
        assert_eq!(ErrorKind::OutOfMemory.status_code(), -4);
        assert_eq!(ErrorKind::InvalidState.status_code(), -5);
        assert_eq!(ErrorKind::WrongThread.status_code(), -6);
        assert_eq!(ErrorKind::NotRealtimeSafe.status_code(), -7);
        assert_eq!(ErrorKind::AbiMismatch.status_code(), -8);
        assert_eq!(ErrorKind::VersionMismatch.status_code(), -9);
        assert_eq!(ErrorKind::NotFound.status_code(), -10);
        assert_eq!(ErrorKind::Io.status_code(), -11);
        assert_eq!(ErrorKind::Graphics.status_code(), -12);
        assert_eq!(ErrorKind::Host.status_code(), -13);
        assert_eq!(ErrorKind::Plugin.status_code(), -14);
        assert_eq!(ErrorKind::Internal.status_code(), -16);
    }

    #[test]
    fn the_status_constants_match_the_specification() {
        assert_eq!(status::OK, 0);
        assert_eq!(status::UNKNOWN, -1);
        assert_eq!(status::INVALID_ARG, -2);
        assert_eq!(status::UNSUPPORTED, -3);
        assert_eq!(status::OUT_OF_MEMORY, -4);
        assert_eq!(status::INVALID_STATE, -5);
        assert_eq!(status::WRONG_THREAD, -6);
        assert_eq!(status::NOT_REALTIME, -7);
        assert_eq!(status::ABI_MISMATCH, -8);
        assert_eq!(status::VERSION, -9);
        assert_eq!(status::NOT_FOUND, -10);
        assert_eq!(status::IO, -11);
        assert_eq!(status::GRAPHICS, -12);
        assert_eq!(status::HOST, -13);
        assert_eq!(status::PLUGIN, -14);
        assert_eq!(status::PANIC, -15);
        assert_eq!(status::INTERNAL, -16);
    }

    #[test]
    fn status_codes_are_distinct_negative_and_round_trip() {
        let mut seen = Vec::new();
        for kind in ErrorKind::ALL {
            let code = kind.status_code();
            assert!(code < 0, "{kind} produced a non-error code {code}");
            assert!(!seen.contains(&code), "{kind} reuses status {code}");
            seen.push(code);
            assert_eq!(ErrorKind::from_status_code(code), Some(kind));
            assert!(!kind.as_str().is_empty());
        }
        assert_eq!(seen.len(), ErrorKind::ALL.len());
    }

    #[test]
    fn codes_without_a_kind_decode_to_none() {
        // Success, the unclassified failure and the panic code are not kinds.
        assert_eq!(ErrorKind::from_status_code(status::OK), None);
        assert_eq!(ErrorKind::from_status_code(status::UNKNOWN), None);
        assert_eq!(ErrorKind::from_status_code(status::PANIC), None);
        // Nor is anything from a future ABI, or a positive value.
        assert_eq!(ErrorKind::from_status_code(-17), None);
        assert_eq!(ErrorKind::from_status_code(i32::MIN), None);
        assert_eq!(ErrorKind::from_status_code(1), None);
        assert_eq!(ErrorKind::from_status_code(i32::MAX), None);
    }

    #[test]
    fn errors_carry_a_kind_and_a_message() {
        let e = DauxError::new(ErrorKind::NotFound, "no plug-in with that id");
        assert_eq!(e.kind(), ErrorKind::NotFound);
        assert_eq!(e.message(), "no plug-in with that id");
        assert_eq!(e.status_code(), -10);
        assert_eq!(e.to_string(), "not-found: no plug-in with that id");
        let _: &dyn std::error::Error = &e;
    }

    #[test]
    fn a_message_free_error_still_displays() {
        let e = DauxError::from(ErrorKind::Internal);
        assert_eq!(e.message(), "");
        assert_eq!(e.to_string(), "internal");
        assert_eq!(e.status_code(), -16);
    }

    #[test]
    fn a_static_message_costs_nothing_to_build() {
        const E: DauxError = DauxError::from_static(ErrorKind::WrongThread, "audio thread");
        assert_eq!(E.kind(), ErrorKind::WrongThread);
        assert_eq!(E.message(), "audio thread");
        assert_eq!(E.to_string(), "wrong-thread: audio thread");

        let ((), allocations) = daux_rt::AllocGuard::scope(|| {
            let e = DauxError::from_static(ErrorKind::NotRealtimeSafe, "would allocate");
            assert_eq!(e.status_code(), -7);
            let kinded = ErrorKind::NotRealtimeSafe.with_static("would allocate");
            assert_eq!(kinded, e);
        });
        assert_eq!(allocations, 0, "building a static error allocated");
    }

    #[test]
    fn kind_and_context_can_be_adjusted() {
        let e = DauxError::new(ErrorKind::NotFound, "gain")
            .with_kind(ErrorKind::InvalidArgument)
            .context("load_state");
        assert_eq!(e.kind(), ErrorKind::InvalidArgument);
        assert_eq!(e.message(), "load_state: gain");
        assert_eq!(ErrorKind::Host.error("refused").kind(), ErrorKind::Host);
    }

    #[test]
    fn io_errors_keep_their_meaning() {
        use std::io::{Error, ErrorKind as Io};
        assert_eq!(
            DauxError::from(Error::from(Io::NotFound)).kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            DauxError::from(Error::from(Io::InvalidData)).kind(),
            ErrorKind::InvalidArgument
        );
        assert_eq!(
            DauxError::from(Error::from(Io::OutOfMemory)).kind(),
            ErrorKind::OutOfMemory
        );
        assert_eq!(
            DauxError::from(Error::from(Io::PermissionDenied)).kind(),
            ErrorKind::Io
        );
        assert!(!DauxError::from(Error::from(Io::NotFound)).message().is_empty());
    }

    #[test]
    fn state_errors_map_onto_the_abi_version_rule() {
        use daux_state::{StateReader, StateVersion, StateWriter};

        // A blob from another tool is malformed input, not a version problem.
        let corrupt = StateReader::from_bytes(b"{\"gain\": 0}").expect_err("not a DAUx blob");
        assert_eq!(
            DauxError::from(corrupt).kind(),
            ErrorKind::InvalidArgument,
            "a corrupt blob is bad input"
        );

        // A key that was never written is a lookup failure.
        let mut w = StateWriter::new(StateVersion(1));
        w.put_f64("gain", -6.0);
        let bytes = w.try_finish().expect("valid");
        let reader = StateReader::from_bytes(&bytes).expect("decodes");
        let missing = reader.f64("cutoff").expect_err("never written");
        let mapped = DauxError::from(missing);
        assert_eq!(mapped.kind(), ErrorKind::NotFound);
        assert!(!mapped.message().is_empty());

        // A type confusion is bad input too.
        let wrong_type = reader.str("gain").expect_err("gain is an f64");
        assert_eq!(
            DauxError::from(wrong_type).kind(),
            ErrorKind::InvalidArgument
        );
    }

    #[test]
    fn state_version_failures_become_version_mismatches() {
        use daux_state::{MigrationChain, StateDoc, StateVersion};

        // A document from the future: no step can bring it back.
        let chain = MigrationChain::new(StateVersion(1));
        let from_the_future = StateDoc::new(StateVersion(9));
        let err = chain.migrate(from_the_future).expect_err("unreachable");
        let mapped = DauxError::from(err);
        assert_eq!(mapped.kind(), ErrorKind::VersionMismatch);
        assert_eq!(mapped.status_code(), status::VERSION);
    }

    #[test]
    fn the_error_type_crosses_threads() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DauxError>();
        assert_send_sync::<ErrorKind>();
        assert_send_sync::<DauxResult<()>>();
    }
}
