//! What can go wrong while driving a plug-in from a test.

use std::fmt;
use std::path::{Path, PathBuf};

use daux_runtime::daux_core::DauxError;
use daux_runtime::{RuntimeError, RuntimeErrorKind};

/// The result of a fallible harness operation. [main-thread]
pub type HostResult<T> = Result<T, HostError>;

/// Why a harness operation failed. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HostErrorKind {
    /// The bundle could not be opened, or its binary could not be loaded.
    Load,
    /// No instance answers to that [`InstanceId`](crate::InstanceId). Either it was never
    /// created, or it was unloaded.
    NoSuchInstance,
    /// The plug-in has no parameter with that id.
    NoSuchParam,
    /// The audio handed to `process` does not describe a block this instance can run:
    /// mismatched frame counts, more frames than the activation allows, or an empty block.
    BadBlock,
    /// The instance is not in a state where the call is legal — processing before
    /// activation, activating twice.
    InvalidState,
    /// The plug-in does not implement the feature the harness asked for; state and
    /// parameters are both optional extensions in `abi-v1` §11.
    Unsupported,
    /// The plug-in refused, or its state blob is not one it can read.
    Plugin,
    /// The plug-in panicked across the ABI boundary and refuses further work
    /// (`abi-v1` §17).
    Poisoned,
}

impl HostErrorKind {
    /// Short, stable identifier for logs and tests. [any-thread]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::NoSuchInstance => "no-such-instance",
            Self::NoSuchParam => "no-such-param",
            Self::BadBlock => "bad-block",
            Self::InvalidState => "invalid-state",
            Self::Unsupported => "unsupported",
            Self::Plugin => "plugin",
            Self::Poisoned => "poisoned",
        }
    }
}

impl fmt::Display for HostErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A harness failure. [any-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    kind: HostErrorKind,
    message: String,
    path: Option<PathBuf>,
}

impl HostError {
    /// Builds a failure. [main-thread] — allocates the message.
    #[must_use]
    pub fn new(kind: HostErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            path: None,
        }
    }

    /// Records the bundle the failure is about. [main-thread]
    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// What kind of failure this is. [any-thread]
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> HostErrorKind {
        self.kind
    }

    /// The human-readable detail. [any-thread]
    #[inline]
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The bundle the failure is about, when one is known. [any-thread]
    #[inline]
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)?;
        if let Some(path) = &self.path {
            write!(f, " ({})", path.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for HostError {}

impl From<RuntimeError> for HostError {
    fn from(error: RuntimeError) -> Self {
        let kind = match error.kind() {
            RuntimeErrorKind::Poisoned => HostErrorKind::Poisoned,
            RuntimeErrorKind::InvalidState => HostErrorKind::InvalidState,
            RuntimeErrorKind::Unsupported => HostErrorKind::Unsupported,
            RuntimeErrorKind::InvalidArgument => HostErrorKind::BadBlock,
            RuntimeErrorKind::NotFound => HostErrorKind::Load,
            _ => HostErrorKind::Load,
        };
        let mut host = Self::new(kind, error.to_string());
        if let Some(path) = error.path() {
            host = host.with_path(path);
        }
        host
    }
}

impl From<DauxError> for HostError {
    fn from(error: DauxError) -> Self {
        use daux_runtime::daux_core::ErrorKind;
        let kind = match error.kind() {
            ErrorKind::Unsupported => HostErrorKind::Unsupported,
            ErrorKind::InvalidState | ErrorKind::WrongThread => HostErrorKind::InvalidState,
            ErrorKind::InvalidArgument => HostErrorKind::BadBlock,
            ErrorKind::NotFound => HostErrorKind::NoSuchParam,
            _ => HostErrorKind::Plugin,
        };
        Self::new(kind, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_runtime::daux_core::ErrorKind;

    #[test]
    fn every_kind_has_a_distinct_name() {
        let kinds = [
            HostErrorKind::Load,
            HostErrorKind::NoSuchInstance,
            HostErrorKind::NoSuchParam,
            HostErrorKind::BadBlock,
            HostErrorKind::InvalidState,
            HostErrorKind::Unsupported,
            HostErrorKind::Plugin,
            HostErrorKind::Poisoned,
        ];
        let mut seen = Vec::new();
        for kind in kinds {
            assert!(!seen.contains(&kind.as_str()), "{kind} reuses a name");
            seen.push(kind.as_str());
        }
    }

    /// A poisoned instance must stay recognisable as poisoned: a harness that reported it
    /// as a plain load failure would send the user looking for a missing file.
    #[test]
    fn runtime_failures_keep_their_meaning() {
        let poisoned = RuntimeError::new(RuntimeErrorKind::Poisoned, "panicked in process");
        assert_eq!(HostError::from(poisoned).kind(), HostErrorKind::Poisoned);

        let state = RuntimeError::new(RuntimeErrorKind::InvalidState, "not activated");
        assert_eq!(HostError::from(state).kind(), HostErrorKind::InvalidState);

        let missing = RuntimeError::new(RuntimeErrorKind::Library, "no such file")
            .with_path("/plugins/Gain.axt");
        let error = HostError::from(missing);
        assert_eq!(error.kind(), HostErrorKind::Load);
        assert_eq!(error.path(), Some(Path::new("/plugins/Gain.axt")));
    }

    #[test]
    fn model_failures_map_onto_the_harnesss_own_vocabulary() {
        assert_eq!(
            HostError::from(DauxError::new(ErrorKind::Unsupported, "no f64")).kind(),
            HostErrorKind::Unsupported
        );
        assert_eq!(
            HostError::from(DauxError::new(ErrorKind::NotFound, "no such parameter")).kind(),
            HostErrorKind::NoSuchParam
        );
        assert_eq!(
            HostError::from(DauxError::new(ErrorKind::Internal, "bad state blob")).kind(),
            HostErrorKind::Plugin
        );
    }

    #[test]
    fn display_carries_kind_message_and_path() {
        let error = HostError::new(HostErrorKind::Load, "not a bundle").with_path("/x/Gain.axt");
        let text = error.to_string();
        assert!(text.contains("load"), "{text}");
        assert!(text.contains("not a bundle"), "{text}");
        assert!(text.contains("Gain.axt"), "{text}");
        let _: &dyn std::error::Error = &error;
    }
}
