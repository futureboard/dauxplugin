//! The error type every fallible editor operation returns.

use core::fmt;
use std::borrow::Cow;

/// `[any-thread]` What went wrong, coarsely enough that a host can react to it.
///
/// The kinds mirror the subset of `daux_core::ErrorKind` an editor can produce. This
/// crate cannot depend on `daux-core` (the dependency runs the other way), so the format
/// adapters translate these into `DAUX_ERR_*` status codes at the ABI boundary.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GraphicErrorKind {
    /// The editor cannot do what was asked at all — a framework, renderer or
    /// presentation mode it does not implement.
    Unsupported,
    /// A value handed in was out of range, malformed or degenerate.
    InvalidArgument,
    /// The call is fine but the editor is in the wrong state for it, e.g. `resize`
    /// before `open` or a second `open` without a `close`.
    InvalidState,
    /// Host and plug-in capabilities do not intersect, so nothing could be agreed.
    Negotiation,
    /// The window handle is missing, null or of an API this editor does not speak.
    WindowApi,
    /// A rendering device, context or swapchain could not be created or was lost.
    Renderer,
    /// A GPU or platform resource (texture, surface, fence) could not be obtained.
    Resource,
    /// A bounded collection is full; nothing was added.
    CapacityExceeded,
    /// A failure the editor cannot classify. Prefer any other kind.
    Internal,
}

impl GraphicErrorKind {
    /// `[any-thread]` A short, stable, machine-friendly name for the kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::InvalidArgument => "invalid-argument",
            Self::InvalidState => "invalid-state",
            Self::Negotiation => "negotiation",
            Self::WindowApi => "window-api",
            Self::Renderer => "renderer",
            Self::Resource => "resource",
            Self::CapacityExceeded => "capacity-exceeded",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for GraphicErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[main-thread]` A failure from an editor operation.
///
/// The message is a [`Cow`] so the common case — a fixed explanation known at compile
/// time — costs nothing, while a backend that genuinely needs to interpolate a device
/// name can still do so. Editors run on the main thread, so an occasional allocation here
/// is not a real-time concern; nothing in this crate is reachable from `process`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GraphicError {
    kind: GraphicErrorKind,
    message: Cow<'static, str>,
}

impl GraphicError {
    /// `[any-thread]` Builds an error whose message is known at compile time.
    ///
    /// `const`, allocation-free, and therefore usable from anywhere.
    #[must_use]
    pub const fn new_static(kind: GraphicErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message: Cow::Borrowed(message),
        }
    }

    /// `[main-thread]` Builds an error with an owned or borrowed message.
    #[must_use]
    pub fn new(kind: GraphicErrorKind, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// `[any-thread]` The error's classification.
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> GraphicErrorKind {
        self.kind
    }

    /// `[any-thread]` The human-readable explanation.
    #[inline]
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// `[any-thread]` The editor cannot do what was asked.
    #[must_use]
    pub const fn unsupported(message: &'static str) -> Self {
        Self::new_static(GraphicErrorKind::Unsupported, message)
    }

    /// `[any-thread]` A value handed in was out of range or malformed.
    #[must_use]
    pub const fn invalid_argument(message: &'static str) -> Self {
        Self::new_static(GraphicErrorKind::InvalidArgument, message)
    }

    /// `[any-thread]` The editor is in the wrong state for this call.
    #[must_use]
    pub const fn invalid_state(message: &'static str) -> Self {
        Self::new_static(GraphicErrorKind::InvalidState, message)
    }

    /// `[any-thread]` Host and plug-in capabilities do not intersect.
    #[must_use]
    pub const fn negotiation(message: &'static str) -> Self {
        Self::new_static(GraphicErrorKind::Negotiation, message)
    }
}

impl fmt::Display for GraphicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for GraphicError {}

/// `[main-thread]` The result type every fallible editor operation returns.
pub type DauxGraphicResult<T> = Result<T, GraphicError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_have_distinct_stable_names() {
        let kinds = [
            GraphicErrorKind::Unsupported,
            GraphicErrorKind::InvalidArgument,
            GraphicErrorKind::InvalidState,
            GraphicErrorKind::Negotiation,
            GraphicErrorKind::WindowApi,
            GraphicErrorKind::Renderer,
            GraphicErrorKind::Resource,
            GraphicErrorKind::CapacityExceeded,
            GraphicErrorKind::Internal,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(a.as_str(), b.as_str(), "{a:?} and {b:?} share a name");
            }
            assert_eq!(a.to_string(), a.as_str());
        }
    }

    #[test]
    fn static_construction_keeps_the_message_borrowed() {
        const ERR: GraphicError = GraphicError::unsupported("no software renderer");
        assert_eq!(ERR.kind(), GraphicErrorKind::Unsupported);
        assert_eq!(ERR.message(), "no software renderer");
        assert!(matches!(ERR.message, Cow::Borrowed(_)));
        assert_eq!(ERR.to_string(), "unsupported: no software renderer");
    }

    #[test]
    fn owned_messages_survive_their_source() {
        let device = String::from("Adapter 7");
        let err = GraphicError::new(
            GraphicErrorKind::Renderer,
            format!("device lost: {device}"),
        );
        drop(device);
        assert_eq!(err.message(), "device lost: Adapter 7");
        assert_eq!(err.kind(), GraphicErrorKind::Renderer);
    }

    #[test]
    fn the_error_is_a_std_error() {
        fn as_dyn(e: &GraphicError) -> &dyn std::error::Error {
            e
        }
        let err = GraphicError::invalid_state("not open");
        assert_eq!(as_dyn(&err).to_string(), "invalid-state: not open");
        assert!(as_dyn(&err).source().is_none());
    }

    #[test]
    fn constructors_pick_the_right_kind() {
        assert_eq!(
            GraphicError::invalid_argument("x").kind(),
            GraphicErrorKind::InvalidArgument
        );
        assert_eq!(
            GraphicError::negotiation("x").kind(),
            GraphicErrorKind::Negotiation
        );
    }
}
