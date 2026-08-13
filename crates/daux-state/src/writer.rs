//! Writing state.

use std::io::Write;

use crate::codec::Encoder;
use crate::error::{StateError, StateResult};
use crate::limits::StateLimits;
use crate::version::StateVersion;

/// Serialises a plug-in's state into the deterministic DAUx state container.
/// [main-thread]
///
/// The `put_*` calls are infallible so that save code stays straight-line, exactly the
/// shape `DauxController::save_state` wants. Anything that goes wrong — an empty or
/// over-long key, a key containing `'/'`, a value that would blow the size budget, an
/// unbalanced group — is **latched**: the first error is remembered, later entries are
/// dropped, and the failure surfaces at [`StateWriter::try_finish`] or
/// [`StateWriter::write_to`]. [`StateWriter::error`] can be consulted at any point.
///
/// Output is deterministic: the same sequence of calls always produces the same bytes.
/// Entries are stored in insertion order, integers and floats little-endian, and nothing
/// about the machine or the moment leaks into the blob.
///
/// ```
/// use daux_state::{StateReader, StateVersion, StateWriter};
///
/// let mut w = StateWriter::new(StateVersion(1));
/// w.put_f64("gain", -6.0);
/// w.begin_group("filter");
/// w.put_str("mode", "lowpass");
/// w.put_f64("cutoff", 1_000.0);
/// w.end_group();
/// let bytes = w.try_finish().expect("valid");
///
/// let r = StateReader::from_bytes(&bytes).expect("round trip");
/// assert_eq!(r.f64("gain").unwrap(), -6.0);
/// assert_eq!(r.str("filter/mode").unwrap(), "lowpass");
/// ```
pub struct StateWriter {
    version: StateVersion,
    encoder: Encoder,
}

impl StateWriter {
    /// Starts a document that declares schema version `version`. [main-thread]
    ///
    /// Allocates the output buffer; never call this from the audio thread.
    #[must_use]
    pub fn new(version: StateVersion) -> Self {
        Self::with_limits(version, StateLimits::DEFAULT)
    }

    /// [`StateWriter::new`] with explicit limits. [main-thread]
    ///
    /// Applying the same bounds on the way out as on the way in means a plug-in cannot
    /// write a document it would later refuse to read.
    #[must_use]
    pub fn with_limits(version: StateVersion, limits: StateLimits) -> Self {
        Self {
            version,
            encoder: Encoder::new(version, limits),
        }
    }

    /// The schema version this document declares. [main-thread]
    #[inline]
    #[must_use]
    pub const fn version(&self) -> StateVersion {
        self.version
    }

    /// Writes a floating-point value. `NaN` and infinities are preserved bit-exactly.
    /// [main-thread]
    pub fn put_f64(&mut self, key: &str, v: f64) {
        self.encoder.put_f64(key, v);
    }

    /// Writes a signed integer. [main-thread]
    pub fn put_i64(&mut self, key: &str, v: i64) {
        self.encoder.put_i64(key, v);
    }

    /// Writes a boolean. [main-thread]
    pub fn put_bool(&mut self, key: &str, v: bool) {
        self.encoder.put_bool(key, v);
    }

    /// Writes a UTF-8 string, which may be empty. [main-thread]
    pub fn put_str(&mut self, key: &str, v: &str) {
        self.encoder.put_str(key, v);
    }

    /// Writes an opaque byte string, which may be empty. [main-thread]
    pub fn put_bytes(&mut self, key: &str, v: &[u8]) {
        self.encoder.put_bytes(key, v);
    }

    /// Opens a nested group. Every call must be balanced by [`StateWriter::end_group`].
    /// [main-thread]
    pub fn begin_group(&mut self, key: &str) {
        self.encoder.begin_group(key);
    }

    /// Closes the innermost open group. Calling this with no group open latches an error.
    /// [main-thread]
    pub fn end_group(&mut self) {
        self.encoder.end_group();
    }

    /// The first error latched so far, if any. [main-thread]
    #[inline]
    #[must_use]
    pub fn error(&self) -> Option<&StateError> {
        self.encoder.error()
    }

    /// `true` while nothing has gone wrong. [main-thread]
    #[inline]
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.encoder.error().is_none()
    }

    /// Number of entries written so far, counting group markers. [main-thread]
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.encoder.entry_count() as usize
    }

    /// `true` when nothing has been written yet. [main-thread]
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.encoder.entry_count() == 0
    }

    /// How many groups are currently open. [main-thread]
    #[inline]
    #[must_use]
    pub const fn open_groups(&self) -> usize {
        self.encoder.depth()
    }

    /// Finishes the document and returns its bytes, discarding any latched error.
    /// [main-thread]
    ///
    /// Groups the caller forgot to close are closed automatically, so the result parses.
    /// Entries dropped after a latched error are, however, simply missing: use
    /// [`StateWriter::try_finish`] unless the caller has already inspected
    /// [`StateWriter::error`].
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.encoder.finish().0
    }

    /// Finishes the document, returning the first latched error instead of bytes.
    /// [main-thread]
    pub fn try_finish(self) -> StateResult<Vec<u8>> {
        let (bytes, error) = self.encoder.finish();
        error.map_or(Ok(bytes), Err)
    }

    /// Finishes the document and writes it to `w`. [main-thread]
    ///
    /// Fails with the latched error if there is one — nothing is written in that case —
    /// and otherwise with [`StateErrorKind::Io`](crate::StateErrorKind::Io).
    pub fn write_to(self, w: &mut dyn Write) -> StateResult<()> {
        let bytes = self.try_finish()?;
        w.write_all(&bytes)?;
        w.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StateDoc, StateErrorKind, StateReader};

    #[test]
    fn tracks_progress() {
        let mut w = StateWriter::new(StateVersion(2));
        assert!(w.is_empty());
        assert!(w.is_ok());
        assert_eq!(w.version(), StateVersion(2));
        w.put_f64("a", 1.0);
        w.begin_group("g");
        assert_eq!(w.open_groups(), 1);
        w.put_bool("b", true);
        w.end_group();
        assert_eq!(w.open_groups(), 0);
        assert_eq!(w.len(), 4);
        assert!(!w.is_empty());
        assert!(w.try_finish().is_ok());
    }

    #[test]
    fn identical_call_sequences_produce_identical_bytes() {
        let build = || {
            let mut w = StateWriter::new(StateVersion(1));
            w.put_str("name", "Ünïcødé ☃");
            w.put_i64("count", -9_000_000_000);
            w.put_bytes("blob", &[0, 255, 128]);
            w.begin_group("nested");
            w.put_bool("flag", false);
            w.end_group();
            w.try_finish().expect("valid")
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn matches_the_document_encoder_byte_for_byte() {
        let mut w = StateWriter::new(StateVersion(3));
        w.put_f64("gain", 0.5);
        w.begin_group("filter");
        w.put_str("mode", "peak");
        w.end_group();
        let from_writer = w.try_finish().expect("valid");

        let doc = StateDoc::from_bytes(&from_writer).expect("decodes");
        assert_eq!(doc.try_to_bytes().expect("encodes"), from_writer);
    }

    #[test]
    fn an_invalid_key_is_latched_and_named() {
        let mut w = StateWriter::new(StateVersion(1));
        w.put_f64("with/slash", 1.0);
        assert!(!w.is_ok());
        let err = w.error().expect("latched").clone();
        assert_eq!(err.kind(), &StateErrorKind::InvalidKey);
        assert_eq!(err.key(), Some("with/slash"));
        assert!(err.to_string().contains("with/slash"));
        assert_eq!(
            w.try_finish().expect_err("fails").kind(),
            &StateErrorKind::InvalidKey
        );
    }

    #[test]
    fn an_empty_key_is_rejected() {
        let mut w = StateWriter::new(StateVersion(1));
        w.put_bool("", true);
        assert_eq!(
            w.try_finish().expect_err("fails").kind(),
            &StateErrorKind::InvalidKey
        );
    }

    #[test]
    fn a_key_at_exactly_the_limit_is_accepted_and_one_over_is_not() {
        let at_limit = "k".repeat(StateLimits::DEFAULT_MAX_KEY_BYTES);
        let mut w = StateWriter::new(StateVersion(1));
        w.put_bool(&at_limit, true);
        let bytes = w.try_finish().expect("exactly at the limit is fine");
        assert!(StateReader::from_bytes(&bytes).is_ok());

        let over = "k".repeat(StateLimits::DEFAULT_MAX_KEY_BYTES + 1);
        let mut w = StateWriter::new(StateVersion(1));
        w.put_bool(&over, true);
        assert_eq!(
            w.try_finish().expect_err("fails").kind(),
            &StateErrorKind::LimitExceeded
        );
    }

    #[test]
    fn unbalanced_end_group_is_latched() {
        let mut w = StateWriter::new(StateVersion(1));
        w.end_group();
        assert!(!w.is_ok());
        assert_eq!(
            w.try_finish().expect_err("fails").kind(),
            &StateErrorKind::Corrupt
        );
    }

    #[test]
    fn unclosed_group_is_latched_but_finish_still_parses() {
        let mut w = StateWriter::new(StateVersion(1));
        w.begin_group("g");
        w.put_i64("x", 1);
        assert!(w.is_ok());
        let bytes = w.finish();
        let r = StateReader::from_bytes(&bytes).expect("auto-closed");
        assert_eq!(r.i64("g/x").expect("x"), 1);

        let mut w = StateWriter::new(StateVersion(1));
        w.begin_group("g");
        assert_eq!(
            w.try_finish().expect_err("fails").kind(),
            &StateErrorKind::Corrupt
        );
    }

    #[test]
    fn writes_to_an_io_sink() {
        let mut w = StateWriter::new(StateVersion(1));
        w.put_i64("n", 7);
        let mut sink: Vec<u8> = Vec::new();
        w.write_to(&mut sink).expect("writes");
        assert_eq!(
            StateReader::from_bytes(&sink).expect("decodes").i64("n"),
            Ok(7)
        );
    }

    #[test]
    fn write_to_reports_the_latched_error_without_writing() {
        let mut w = StateWriter::new(StateVersion(1));
        w.put_i64("", 7);
        let mut sink: Vec<u8> = Vec::new();
        assert!(w.write_to(&mut sink).is_err());
        assert!(sink.is_empty());
    }

    #[test]
    fn write_to_reports_io_failure() {
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut w = StateWriter::new(StateVersion(1));
        w.put_i64("n", 7);
        let err = w.write_to(&mut Failing).expect_err("io failure");
        assert_eq!(err.kind(), &StateErrorKind::Io);
        assert!(err.to_string().contains("gone"));
    }

    #[test]
    fn a_writer_cannot_produce_a_blob_it_would_refuse_to_read() {
        let limits = StateLimits::default().with_max_blob_bytes(64);
        let mut w = StateWriter::with_limits(StateVersion(1), limits);
        for i in 0..100 {
            w.put_i64(&format!("key{i}"), i);
        }
        assert_eq!(
            w.try_finish().expect_err("fails").kind(),
            &StateErrorKind::LimitExceeded
        );
    }

    #[test]
    fn rolled_back_entries_leave_a_parseable_blob() {
        let limits = StateLimits::default().with_max_blob_bytes(40);
        let mut w = StateWriter::with_limits(StateVersion(1), limits);
        w.put_i64("a", 1);
        w.put_bytes("b", &[0u8; 64]); // does not fit; must be rolled back whole
        let bytes = w.finish();
        let r = StateReader::from_bytes(&bytes).expect("still parses");
        assert_eq!(r.i64("a"), Ok(1));
        assert!(!r.contains("b"));
    }
}
