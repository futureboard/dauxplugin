//! Unix domain socket control transport. **Declared, not implemented in v1.**
//!
//! A `SOCK_STREAM` `AF_UNIX` socket is the counterpart of the Windows named pipe: a
//! reliable, ordered byte stream, confined to the machine, with filesystem permissions as
//! the access control and a clean `EOF` when the peer dies. It also carries file
//! descriptors over `SCM_RIGHTS`, which is how the shared-memory handle for the data plane
//! will reach the sandbox without either side needing a name in the filesystem.
//!
//! Until that lands, every entry point here reports
//! [`IpcErrorKind::Unsupported`](crate::IpcErrorKind::Unsupported). See the
//! [module documentation](super) for why the type exists at all.

use crate::error::{IpcError, IpcResult};
use crate::transport::ControlTransport;

/// A control connection over a Unix domain socket. [main-thread]
///
/// Cannot be constructed on this build: both constructors report
/// [`IpcErrorKind::Unsupported`](crate::IpcErrorKind::Unsupported). The
/// [`ControlTransport`] implementation is present so that the trait is checked against a
/// descriptor-based transport, and reports the same thing for every operation.
pub struct UnixSocketTransport {
    _private: (),
}

impl UnixSocketTransport {
    /// Longest socket path accepted; `sockaddr_un::sun_path` is 108 bytes on Linux and
    /// shorter still on some platforms, one of which is the terminator.
    pub const MAX_PATH_LEN: usize = 104;

    /// [main-thread] Binds a socket and waits for the sandbox to connect.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) for a path
    /// that is empty, longer than [`UnixSocketTransport::MAX_PATH_LEN`], or contains an
    /// interior NUL, and [`IpcErrorKind::Unsupported`](crate::IpcErrorKind::Unsupported)
    /// always, on this build.
    pub fn listen(path: &str) -> IpcResult<Self> {
        check_path(path)?;
        Err(IpcError::unsupported("UnixSocketTransport::listen"))
    }

    /// [main-thread] Connects to a socket the host is already listening on.
    ///
    /// # Errors
    ///
    /// As [`UnixSocketTransport::listen`].
    pub fn connect(path: &str) -> IpcResult<Self> {
        check_path(path)?;
        Err(IpcError::unsupported("UnixSocketTransport::connect"))
    }
}

/// Rejects a path no `bind`/`connect` call could succeed with.
fn check_path(path: &str) -> IpcResult<()> {
    if path.is_empty()
        || path.len() > UnixSocketTransport::MAX_PATH_LEN
        || path.as_bytes().contains(&0)
    {
        return Err(IpcError::invalid_argument("UnixSocketTransport::path"));
    }
    Ok(())
}

impl ControlTransport for UnixSocketTransport {
    fn send(&mut self, _frame: &[u8]) -> IpcResult<()> {
        Err(IpcError::unsupported("UnixSocketTransport::send"))
    }

    fn recv(&mut self, _buf: &mut Vec<u8>) -> IpcResult<usize> {
        Err(IpcError::unsupported("UnixSocketTransport::recv"))
    }

    fn try_recv(&mut self, _buf: &mut Vec<u8>) -> IpcResult<usize> {
        Err(IpcError::unsupported("UnixSocketTransport::try_recv"))
    }

    fn flush(&mut self) -> IpcResult<()> {
        Err(IpcError::unsupported("UnixSocketTransport::flush"))
    }

    fn is_open(&self) -> bool {
        false
    }

    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::UnixSocketTransport;
    use crate::error::IpcErrorKind;
    use crate::transport::ControlTransport;

    #[test]
    fn both_constructors_refuse_cleanly_rather_than_panicking() {
        let path = "/tmp/daux-test.sock";
        assert_eq!(
            UnixSocketTransport::listen(path).err().map(|e| e.kind()),
            Some(IpcErrorKind::Unsupported)
        );
        assert_eq!(
            UnixSocketTransport::connect(path).err().map(|e| e.kind()),
            Some(IpcErrorKind::Unsupported)
        );
    }

    #[test]
    fn an_impossible_path_is_rejected_before_the_unsupported_verdict() {
        let too_long = "x".repeat(UnixSocketTransport::MAX_PATH_LEN + 1);
        for bad in ["", &too_long, "/tmp/da\0ux"] {
            assert_eq!(
                UnixSocketTransport::connect(bad).err().map(|e| e.kind()),
                Some(IpcErrorKind::InvalidArgument),
                "path {bad:?} should be refused as an argument"
            );
        }
        let at_cap = "x".repeat(UnixSocketTransport::MAX_PATH_LEN);
        assert_eq!(
            UnixSocketTransport::connect(&at_cap)
                .err()
                .map(|e| e.kind()),
            Some(IpcErrorKind::Unsupported)
        );
    }

    /// Nothing may silently succeed: an unimplemented transport that returned `Ok` from
    /// `send` would look to a host exactly like a delivered message.
    #[test]
    fn every_transport_operation_reports_unsupported_and_never_succeeds() {
        let mut transport = UnixSocketTransport { _private: () };
        let mut buf = Vec::new();
        assert!(transport.send(b"frame").unwrap_err().is_unsupported());
        assert!(transport.recv(&mut buf).unwrap_err().is_unsupported());
        assert!(transport.try_recv(&mut buf).unwrap_err().is_unsupported());
        assert!(transport.flush().unwrap_err().is_unsupported());
        assert!(!transport.is_open());
        assert!(
            buf.is_empty(),
            "a refused receive must not touch the buffer"
        );
        transport.close();
        assert!(!transport.is_open());
    }
}
