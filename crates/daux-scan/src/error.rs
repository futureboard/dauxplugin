//! Why one artefact could not be catalogued.
//!
//! Nothing in this crate returns an error for the *scan*: a scan of four hundred plug-ins
//! where one is broken must still report the other three hundred and ninety-nine. Failures
//! are per-artefact values carried in the [`ScanReport`](crate::ScanReport), and this is
//! their type.

use std::fmt;
use std::path::{Path, PathBuf};

use daux_bundle::{BundleError, BundleErrorKind};
use daux_runtime::{RuntimeError, RuntimeErrorKind};

/// The result of a single-artefact operation. [main-thread]
pub type ScanResult<T> = Result<T, ScanError>;

/// What went wrong with one artefact. [any-thread]
///
/// The distinction that matters to a user interface is between the kinds that mean *"this
/// is not a plug-in"* — [`NotFound`](ScanErrorKind::NotFound),
/// [`NotABundle`](ScanErrorKind::NotABundle) — which deserve no report at all, and the ones
/// that mean *"this is a plug-in and it is broken"*, which do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ScanErrorKind {
    /// There is nothing at that path.
    NotFound,
    /// The path exists but is not a bundle of the format its name claims.
    NotABundle,
    /// The bundle's metadata is malformed, oversized, or refuses to parse.
    Metadata,
    /// The bundle is well formed but ships no binary this machine can load. A normal
    /// outcome for a cross-platform `.axt`, never corruption (`axt-v1` §9 rule 3).
    NoBinaryForTarget,
    /// The dynamic library refused to load, or the module is not a DAUx module.
    Load,
    /// The module's metadata and its binary disagree about something a saved project
    /// depends on (`manifest-v1` §8.1, rows `DAUX-M100` and `DAUX-M108`).
    Identity,
    /// The plug-in panicked while being enumerated. The scan absorbed it and carried on.
    Panicked,
    /// The plug-in did not answer within the probe timeout and is assumed hung. The scan
    /// abandoned it and carried on.
    Timeout,
    /// The plug-in took the scanner process down the last time it was probed, and is
    /// skipped until the user asks for it explicitly.
    Quarantined,
    /// The scan cache could not be read or written. Never fatal: a scan without a cache is
    /// slow, not wrong.
    Cache,
    /// The filesystem refused something during discovery.
    Io,
}

impl ScanErrorKind {
    /// Short, stable identifier for logs, tests and machine-readable output. [any-thread]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not-found",
            Self::NotABundle => "not-a-bundle",
            Self::Metadata => "metadata",
            Self::NoBinaryForTarget => "no-binary-for-target",
            Self::Load => "load",
            Self::Identity => "identity",
            Self::Panicked => "panicked",
            Self::Timeout => "timeout",
            Self::Quarantined => "quarantined",
            Self::Cache => "cache",
            Self::Io => "io",
        }
    }

    /// Whether an artefact that failed this way should be tried again next scan.
    /// [any-thread]
    ///
    /// A plug-in that crashed or hung stays quarantined until the user intervenes or the
    /// file changes; everything else is retried, because the next scan may find a fixed
    /// bundle, a mounted drive or a freed file handle.
    #[must_use]
    pub const fn is_sticky(self) -> bool {
        matches!(self, Self::Panicked | Self::Timeout | Self::Quarantined)
    }
}

impl fmt::Display for ScanErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One artefact's failure, with the path it is about. [any-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanError {
    kind: ScanErrorKind,
    message: String,
    path: Option<PathBuf>,
}

impl ScanError {
    /// Builds a failure. [main-thread] — allocates the message.
    #[must_use]
    pub fn new(kind: ScanErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            path: None,
        }
    }

    /// Records the path the failure is about. [main-thread]
    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Records the path only if none is known yet, so the innermost one wins.
    /// [main-thread]
    #[must_use]
    pub fn or_path(mut self, path: impl Into<PathBuf>) -> Self {
        if self.path.is_none() {
            self.path = Some(path.into());
        }
        self
    }

    /// What kind of failure this is. [any-thread]
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> ScanErrorKind {
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
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)?;
        if let Some(path) = &self.path {
            write!(f, " ({})", path.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for ScanError {}

impl From<BundleError> for ScanError {
    /// A bundle failure keeps its meaning: "there is no bundle here" must not become
    /// "this bundle is broken", because the first one is silent and the second one is a
    /// report a user has to act on.
    fn from(error: BundleError) -> Self {
        let kind = match error.kind() {
            BundleErrorKind::Io => ScanErrorKind::Io,
            BundleErrorKind::NotADirectory
            | BundleErrorKind::NotAxtExtension
            | BundleErrorKind::NotABundle => ScanErrorKind::NotABundle,
            BundleErrorKind::NotFound => ScanErrorKind::NotFound,
            BundleErrorKind::NoBinaryForTarget => ScanErrorKind::NoBinaryForTarget,
            _ => ScanErrorKind::Metadata,
        };
        let mut scan = Self::new(kind, error.to_string());
        if let Some(path) = error.path() {
            scan = scan.with_path(path);
        }
        scan
    }
}

impl From<RuntimeError> for ScanError {
    fn from(error: RuntimeError) -> Self {
        let kind = match error.kind() {
            RuntimeErrorKind::NotFound => ScanErrorKind::NoBinaryForTarget,
            RuntimeErrorKind::Bundle => ScanErrorKind::Metadata,
            RuntimeErrorKind::Poisoned => ScanErrorKind::Panicked,
            _ => ScanErrorKind::Load,
        };
        let mut scan = Self::new(kind, error.to_string());
        if let Some(path) = error.path() {
            scan = scan.with_path(path);
        }
        scan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_distinct_stable_name() {
        let kinds = [
            ScanErrorKind::NotFound,
            ScanErrorKind::NotABundle,
            ScanErrorKind::Metadata,
            ScanErrorKind::NoBinaryForTarget,
            ScanErrorKind::Load,
            ScanErrorKind::Identity,
            ScanErrorKind::Panicked,
            ScanErrorKind::Timeout,
            ScanErrorKind::Quarantined,
            ScanErrorKind::Cache,
            ScanErrorKind::Io,
        ];
        let mut seen = Vec::new();
        for kind in kinds {
            assert!(!seen.contains(&kind.as_str()), "{kind} reuses a name");
            seen.push(kind.as_str());
        }
    }

    /// The one classification a user interface acts on: a crash or a hang must not be
    /// retried automatically, and everything else must be.
    #[test]
    fn only_crashes_hangs_and_quarantine_are_sticky() {
        assert!(ScanErrorKind::Panicked.is_sticky());
        assert!(ScanErrorKind::Timeout.is_sticky());
        assert!(ScanErrorKind::Quarantined.is_sticky());
        for kind in [
            ScanErrorKind::NotFound,
            ScanErrorKind::Metadata,
            ScanErrorKind::Load,
            ScanErrorKind::Io,
            ScanErrorKind::Cache,
            ScanErrorKind::NoBinaryForTarget,
            ScanErrorKind::Identity,
        ] {
            assert!(!kind.is_sticky(), "{kind} must be retried");
        }
    }

    /// `axt-v1` §9 rule 3: a bundle that ships nothing for this machine is not corruption,
    /// and a scanner that reported it as such would fill a user's log with false alarms
    /// for every cross-platform plug-in they own.
    #[test]
    fn a_missing_binary_keeps_its_meaning_through_both_conversions() {
        let bundle = BundleError::new(BundleErrorKind::NoBinaryForTarget, "no linux build")
            .with_path("/opt/Gain.axt");
        let scan = ScanError::from(bundle);
        assert_eq!(scan.kind(), ScanErrorKind::NoBinaryForTarget);
        assert_eq!(scan.path(), Some(Path::new("/opt/Gain.axt")));

        let runtime = RuntimeError::new(RuntimeErrorKind::NotFound, "nothing for this target");
        assert_eq!(
            ScanError::from(runtime).kind(),
            ScanErrorKind::NoBinaryForTarget
        );
    }

    #[test]
    fn a_broken_manifest_and_an_absent_one_are_different_answers() {
        let absent = BundleError::new(BundleErrorKind::NotABundle, "manifest.json is missing");
        assert_eq!(ScanError::from(absent).kind(), ScanErrorKind::NotABundle);

        let broken = BundleError::new(BundleErrorKind::Parse, "unexpected `}`");
        assert_eq!(ScanError::from(broken).kind(), ScanErrorKind::Metadata);

        let oversized = BundleError::new(BundleErrorKind::TooLarge, "12 MiB manifest");
        assert_eq!(ScanError::from(oversized).kind(), ScanErrorKind::Metadata);
    }

    /// A module that reported `DAUX_ERR_PANIC` is a crashed plug-in, not a load failure:
    /// the difference decides whether the scanner ever tries it again.
    #[test]
    fn a_poisoned_module_is_reported_as_a_panic() {
        let poisoned = RuntimeError::new(RuntimeErrorKind::Poisoned, "panicked in init");
        assert_eq!(ScanError::from(poisoned).kind(), ScanErrorKind::Panicked);

        let abi = RuntimeError::new(RuntimeErrorKind::AbiMismatch, "magic is 0x0");
        assert_eq!(ScanError::from(abi).kind(), ScanErrorKind::Load);
    }

    #[test]
    fn the_innermost_path_survives_and_display_carries_everything() {
        let error = ScanError::new(ScanErrorKind::Metadata, "duplicate key `id`")
            .with_path("/plugins/Gain.axt/manifest.json")
            .or_path("/plugins/Gain.axt");
        assert_eq!(
            error.path(),
            Some(Path::new("/plugins/Gain.axt/manifest.json")),
            "`or_path` must not overwrite a more precise path"
        );
        let text = error.to_string();
        assert!(text.contains("metadata"), "{text}");
        assert!(text.contains("duplicate key"), "{text}");
        assert!(text.contains("manifest.json"), "{text}");
        let _: &dyn std::error::Error = &error;
    }

    #[test]
    fn failures_cross_threads_so_a_parallel_scan_can_collect_them() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ScanError>();
        assert_send_sync::<ScanErrorKind>();
    }
}
