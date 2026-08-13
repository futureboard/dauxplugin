//! Platform transports: declared, not implemented.
//!
//! `docs/architecture/sandboxing.md` commits to running plug-ins out of process over
//! Windows named pipes, Unix domain sockets and OS shared memory. None of those ship in v1.
//! They are declared here anyway, for two reasons that are worth more than the code would
//! be:
//!
//! 1. **The shape is pinned.** Each type implements the same
//!    [`ControlTransport`](crate::ControlTransport) the loopback does, so the compiler
//!    checks that the trait is actually implementable over an operating-system handle, and
//!    host code can name the type it will eventually construct.
//! 2. **The failure is honest.** Every constructor reports
//!    [`IpcErrorKind::Unsupported`](crate::IpcErrorKind::Unsupported). Nothing here panics,
//!    nothing silently succeeds, and nothing pretends to have moved bytes it did not move.
//!    A host that tries to sandbox on this build is told so, cleanly, and can fall back to
//!    running the plug-in in process.
//!
//! Arguments are still validated before the refusal, so the preconditions a real
//! implementation will need are written down — and tested — from the start.

#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

pub mod shared_memory;
