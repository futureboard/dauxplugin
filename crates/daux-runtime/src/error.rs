//! What can go wrong between a bundle on disk and a running plug-in instance.
//!
//! Everything in this crate is `[main-thread]` except [`crate::LoadedPlugin::process`],
//! which returns a [`ProcessStatus`](daux_core::ProcessStatus) rather than a `Result`
//! precisely so that no audio-thread failure path has to build a message.

use std::fmt;
use std::path::{Path, PathBuf};

use daux_bundle::{BundleError, BundleErrorKind};
use daux_core::{DauxError, ErrorKind, status};

/// The result of every fallible runtime operation. [main-thread]
pub type RuntimeResult<T> = Result<T, RuntimeError>;

/// Why a runtime operation failed. [main-thread]
///
/// The distinction that matters most is between [`Bundle`](RuntimeErrorKind::Bundle) /
/// [`NotFound`](RuntimeErrorKind::NotFound) — "this machine cannot run this bundle", which
/// is a normal outcome for a cross-platform `.axt` — and
/// [`Protocol`](RuntimeErrorKind::Protocol) / [`AbiMismatch`](RuntimeErrorKind::AbiMismatch),
/// which mean the module is not a DAUx module a v1 host may call into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RuntimeErrorKind {
    /// The bundle layer refused: no manifest, no binary directory, more than one
    /// candidate binary, a path that escapes the bundle.
    Bundle,
    /// The operating system refused to map the dynamic library — a missing dependency,
    /// the wrong architecture, a corrupt image.
    Library,
    /// The binary loaded but does not export `daux_plugin_entry_v1` (`abi-v1` §3, rule 1).
    MissingEntry,
    /// Bad magic, a major version this host does not implement, or a structure smaller
    /// than its minimum v1.0 size (`abi-v1` §3, rules 2–4).
    AbiMismatch,
    /// The module broke the contract in a way the specification does not allow: a null
    /// interface pointer, a null entry in a non-optional function table, a success status
    /// with nothing written.
    Protocol,
    /// The module returned a negative `DauxStatus`. The code itself is on
    /// [`RuntimeError::status`].
    Status,
    /// The host called something out of the lifecycle order of `abi-v1` §7 — `process`
    /// before `start_processing`, `activate` twice, `destroy` while active.
    InvalidState,
    /// The instance reported `DAUX_ERR_PANIC` and is poisoned: it is unloadable-but-safe
    /// and refuses further work (`abi-v1` §17).
    Poisoned,
    /// The module does not provide the extension that was asked for. Unknown extension
    /// ids return null rather than failing, so this is a normal answer, not a defect.
    Unsupported,
    /// Nothing in the module answers to that plug-in id, parameter id or index.
    NotFound,
    /// The host passed the runtime something it cannot use: a block longer than the
    /// activated maximum, an unbound audio channel, an index past `u32::MAX`.
    InvalidArgument,
}

impl RuntimeErrorKind {
    /// Short, stable identifier for logs and tests. [any-thread]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bundle => "bundle",
            Self::Library => "library",
            Self::MissingEntry => "missing-entry",
            Self::AbiMismatch => "abi-mismatch",
            Self::Protocol => "protocol",
            Self::Status => "status",
            Self::InvalidState => "invalid-state",
            Self::Poisoned => "poisoned",
            Self::Unsupported => "unsupported",
            Self::NotFound => "not-found",
            Self::InvalidArgument => "invalid-argument",
        }
    }

    /// The [`daux_core::ErrorKind`] this maps onto. [any-thread]
    #[must_use]
    pub const fn error_kind(self) -> ErrorKind {
        match self {
            Self::Bundle | Self::Library => ErrorKind::Io,
            Self::MissingEntry | Self::AbiMismatch => ErrorKind::AbiMismatch,
            Self::Protocol | Self::Status => ErrorKind::Plugin,
            Self::InvalidState | Self::Poisoned => ErrorKind::InvalidState,
            Self::Unsupported => ErrorKind::Unsupported,
            Self::NotFound => ErrorKind::NotFound,
            Self::InvalidArgument => ErrorKind::InvalidArgument,
        }
    }
}

impl fmt::Display for RuntimeErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A runtime failure, with the path and the module status code when there is one.
/// [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeError {
    kind: RuntimeErrorKind,
    message: String,
    path: Option<PathBuf>,
    status: Option<i32>,
}

impl RuntimeError {
    /// Builds an error. [main-thread] — allocates the message.
    #[must_use]
    pub fn new(kind: RuntimeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            path: None,
            status: None,
        }
    }

    /// Records the path the failure is about. [main-thread]
    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Records the raw `DauxStatus` the module returned. [main-thread]
    #[must_use]
    pub fn with_status(mut self, status: i32) -> Self {
        self.status = Some(status);
        self
    }

    /// What kind of failure this is. [any-thread]
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> RuntimeErrorKind {
        self.kind
    }

    /// The human-readable detail. [any-thread]
    #[inline]
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The path the failure is about, when one is known. [any-thread]
    #[inline]
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The raw `DauxStatus` the module returned, when the failure came from one.
    /// [any-thread]
    #[inline]
    #[must_use]
    pub const fn status(&self) -> Option<i32> {
        self.status
    }

    /// The ABI status code a host reports for this failure. [any-thread]
    ///
    /// A failure that carries a module status reports that status verbatim, so a code the
    /// runtime does not model is never laundered into a different one.
    #[must_use]
    pub const fn status_code(&self) -> i32 {
        match self.status {
            Some(code) => code,
            None => self.kind.error_kind().status_code(),
        }
    }

    /// Builds a [`RuntimeErrorKind::Protocol`] failure. [main-thread]
    #[must_use]
    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::new(RuntimeErrorKind::Protocol, message)
    }

    /// Builds a [`RuntimeErrorKind::AbiMismatch`] failure. [main-thread]
    #[must_use]
    pub(crate) fn abi(message: impl Into<String>) -> Self {
        Self::new(RuntimeErrorKind::AbiMismatch, message)
    }

    /// Turns a negative `DauxStatus` from a module call into an error. [main-thread]
    #[must_use]
    pub(crate) fn from_status(what: &str, code: i32) -> Self {
        let kind = match code {
            status::PANIC => RuntimeErrorKind::Poisoned,
            status::NOT_FOUND => RuntimeErrorKind::NotFound,
            status::UNSUPPORTED => RuntimeErrorKind::Unsupported,
            status::INVALID_STATE => RuntimeErrorKind::InvalidState,
            _ => RuntimeErrorKind::Status,
        };
        Self::new(kind, format!("`{what}` returned status {code}")).with_status(code)
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)?;
        if let Some(path) = &self.path {
            write!(f, " ({})", path.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for RuntimeError {}

impl From<BundleError> for RuntimeError {
    /// A bundle that simply has no binary for this machine becomes
    /// [`RuntimeErrorKind::NotFound`], because `axt-v1` §9 rule 3 forbids reporting it as
    /// corruption. Everything else stays [`RuntimeErrorKind::Bundle`].
    fn from(e: BundleError) -> Self {
        let kind = match e.kind() {
            BundleErrorKind::NoBinaryForTarget | BundleErrorKind::NotFound => {
                RuntimeErrorKind::NotFound
            }
            _ => RuntimeErrorKind::Bundle,
        };
        let mut error = Self::new(kind, e.to_string());
        if let Some(path) = e.path() {
            error = error.with_path(path);
        }
        error
    }
}

impl From<libloading::Error> for RuntimeError {
    fn from(e: libloading::Error) -> Self {
        Self::new(RuntimeErrorKind::Library, e.to_string())
    }
}

impl From<RuntimeError> for DauxError {
    fn from(e: RuntimeError) -> Self {
        Self::new(e.kind.error_kind(), e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_distinct_name_and_a_negative_status() {
        let kinds = [
            RuntimeErrorKind::Bundle,
            RuntimeErrorKind::Library,
            RuntimeErrorKind::MissingEntry,
            RuntimeErrorKind::AbiMismatch,
            RuntimeErrorKind::Protocol,
            RuntimeErrorKind::Status,
            RuntimeErrorKind::InvalidState,
            RuntimeErrorKind::Poisoned,
            RuntimeErrorKind::Unsupported,
            RuntimeErrorKind::NotFound,
            RuntimeErrorKind::InvalidArgument,
        ];
        let mut names = Vec::new();
        for kind in kinds {
            assert!(!names.contains(&kind.as_str()), "{kind} reuses a name");
            names.push(kind.as_str());
            let error = RuntimeError::new(kind, "x");
            assert!(
                error.status_code() < 0,
                "{kind} produced a non-error status code"
            );
        }
    }

    /// `abi-v1` §17: a poisoned instance refuses further work with
    /// `DAUX_ERR_INVALID_STATE`, and a host must never treat it as a reason to abort.
    #[test]
    fn a_panic_status_poisons_rather_than_becoming_a_generic_failure() {
        let error = RuntimeError::from_status("process", status::PANIC);
        assert_eq!(error.kind(), RuntimeErrorKind::Poisoned);
        assert_eq!(error.status(), Some(status::PANIC));
        // The verbatim code survives, so a host reporting it upward does not launder
        // `DAUX_ERR_PANIC` into `DAUX_ERR_INVALID_STATE`.
        assert_eq!(error.status_code(), status::PANIC);
        assert_eq!(
            DauxError::from(error).kind(),
            ErrorKind::InvalidState,
            "the modelled kind is still `invalid state`"
        );
    }

    #[test]
    fn module_statuses_keep_their_meaning() {
        assert_eq!(
            RuntimeError::from_status("create_plugin", status::NOT_FOUND).kind(),
            RuntimeErrorKind::NotFound
        );
        assert_eq!(
            RuntimeError::from_status("get_extension", status::UNSUPPORTED).kind(),
            RuntimeErrorKind::Unsupported
        );
        assert_eq!(
            RuntimeError::from_status("activate", status::INVALID_STATE).kind(),
            RuntimeErrorKind::InvalidState
        );
        // Anything the runtime does not model stays a raw status, code included.
        let other = RuntimeError::from_status("activate", status::OUT_OF_MEMORY);
        assert_eq!(other.kind(), RuntimeErrorKind::Status);
        assert_eq!(other.status_code(), status::OUT_OF_MEMORY);
    }

    /// A cross-platform bundle on a platform it does not ship is a normal outcome, not
    /// corruption (`axt-v1` §9 rule 3).
    #[test]
    fn a_missing_binary_for_this_machine_is_not_reported_as_corruption() {
        let bundle = BundleError::new(BundleErrorKind::NoBinaryForTarget, "no linux build")
            .with_path("C:/plugins/Gain.axt");
        let error = RuntimeError::from(bundle);
        assert_eq!(error.kind(), RuntimeErrorKind::NotFound);
        assert_eq!(error.path(), Some(Path::new("C:/plugins/Gain.axt")));

        let broken = BundleError::new(BundleErrorKind::AmbiguousBinary, "two candidates");
        assert_eq!(
            RuntimeError::from(broken).kind(),
            RuntimeErrorKind::Bundle,
            "an ambiguous bundle *is* a defect"
        );
    }

    #[test]
    fn display_carries_kind_message_and_path() {
        let error = RuntimeError::abi("magic is 0x0").with_path("C:/x/Gain.axt");
        let text = error.to_string();
        assert!(text.contains("abi-mismatch"), "{text}");
        assert!(text.contains("magic"), "{text}");
        assert!(text.contains("Gain.axt"), "{text}");
        let _: &dyn std::error::Error = &error;
    }

    #[test]
    fn the_error_type_crosses_threads() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RuntimeError>();
        assert_send_sync::<RuntimeErrorKind>();
    }
}
