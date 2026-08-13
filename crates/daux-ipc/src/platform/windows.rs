//! Windows named-pipe control transport. **Declared, not implemented in v1.**
//!
//! A named pipe opened with `FILE_FLAG_OVERLAPPED` is the right primitive for the control
//! plane on Windows: it is a reliable, ordered byte stream, it survives the peer dying with
//! a clean `ERROR_BROKEN_PIPE` rather than a hang, and its security descriptor can be
//! narrowed so that only the host and the sandbox it spawned can open it.
//!
//! Until that lands, every entry point here reports
//! [`IpcErrorKind::Unsupported`](crate::IpcErrorKind::Unsupported). See the
//! [module documentation](super) for why the type exists at all.

use crate::error::{IpcError, IpcResult};
use crate::transport::ControlTransport;

/// A control connection over a Windows named pipe. [main-thread]
///
/// Cannot be constructed on this build: both constructors report
/// [`IpcErrorKind::Unsupported`](crate::IpcErrorKind::Unsupported). The
/// [`ControlTransport`] implementation is present so that the trait is checked against a
/// handle-based transport, and reports the same thing for every operation.
pub struct NamedPipeTransport {
    _private: (),
}

impl NamedPipeTransport {
    /// Longest pipe name accepted. Windows caps a pipe name at 256 characters.
    pub const MAX_NAME_LEN: usize = 256;

    /// [main-thread] Creates the server end of a pipe and waits for the sandbox to connect.
    ///
    /// # Errors
    ///
    /// [`IpcErrorKind::InvalidArgument`](crate::IpcErrorKind::InvalidArgument) for a name
    /// that is empty or longer than [`NamedPipeTransport::MAX_NAME_LEN`], and
    /// [`IpcErrorKind::Unsupported`](crate::IpcErrorKind::Unsupported) always, on this
    /// build.
    pub fn listen(name: &str) -> IpcResult<Self> {
        check_name(name)?;
        Err(IpcError::unsupported("NamedPipeTransport::listen"))
    }

    /// [main-thread] Opens the client end of a pipe the host is already listening on.
    ///
    /// # Errors
    ///
    /// As [`NamedPipeTransport::listen`].
    pub fn connect(name: &str) -> IpcResult<Self> {
        check_name(name)?;
        Err(IpcError::unsupported("NamedPipeTransport::connect"))
    }
}

/// Rejects a pipe name no `CreateNamedPipeW` call could succeed with.
fn check_name(name: &str) -> IpcResult<()> {
    if name.is_empty() || name.len() > NamedPipeTransport::MAX_NAME_LEN {
        return Err(IpcError::invalid_argument("NamedPipeTransport::name"));
    }
    Ok(())
}

impl ControlTransport for NamedPipeTransport {
    fn send(&mut self, _frame: &[u8]) -> IpcResult<()> {
        Err(IpcError::unsupported("NamedPipeTransport::send"))
    }

    fn recv(&mut self, _buf: &mut Vec<u8>) -> IpcResult<usize> {
        Err(IpcError::unsupported("NamedPipeTransport::recv"))
    }

    fn try_recv(&mut self, _buf: &mut Vec<u8>) -> IpcResult<usize> {
        Err(IpcError::unsupported("NamedPipeTransport::try_recv"))
    }

    fn flush(&mut self) -> IpcResult<()> {
        Err(IpcError::unsupported("NamedPipeTransport::flush"))
    }

    fn is_open(&self) -> bool {
        false
    }

    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::NamedPipeTransport;
    use crate::error::IpcErrorKind;
    use crate::transport::ControlTransport;

    #[test]
    fn both_constructors_refuse_cleanly_rather_than_panicking() {
        let name = r"\\.\pipe\daux-test";
        assert_eq!(
            NamedPipeTransport::listen(name).err().map(|e| e.kind()),
            Some(IpcErrorKind::Unsupported)
        );
        assert_eq!(
            NamedPipeTransport::connect(name).err().map(|e| e.kind()),
            Some(IpcErrorKind::Unsupported)
        );
    }

    #[test]
    fn an_impossible_name_is_rejected_before_the_unsupported_verdict() {
        for bad in ["", &"x".repeat(NamedPipeTransport::MAX_NAME_LEN + 1)] {
            assert_eq!(
                NamedPipeTransport::connect(bad).err().map(|e| e.kind()),
                Some(IpcErrorKind::InvalidArgument),
                "name of length {} should be refused as an argument",
                bad.len()
            );
        }
        // Exactly at the cap is a name, not an argument error.
        let at_cap = "x".repeat(NamedPipeTransport::MAX_NAME_LEN);
        assert_eq!(
            NamedPipeTransport::connect(&at_cap).err().map(|e| e.kind()),
            Some(IpcErrorKind::Unsupported)
        );
    }

    /// Nothing may silently succeed: an unimplemented transport that returned `Ok` from
    /// `send` would look to a host exactly like a delivered message.
    #[test]
    fn every_transport_operation_reports_unsupported_and_never_succeeds() {
        let mut transport = NamedPipeTransport { _private: () };
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
