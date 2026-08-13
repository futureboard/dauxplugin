//! Reading state, including from untrusted sources.

use std::io::Read;

use crate::doc::{StateDoc, StateGroup};
use crate::error::{StateError, StateResult};
use crate::limits::StateLimits;
use crate::value::{StateEntry, Value};
use crate::version::StateVersion;

/// A parsed, read-only state document. [main-thread]
///
/// Everything a `.daw` session hands a plug-in is untrusted input, so parsing happens
/// once, up front, with every length checked against both [`StateLimits`] and the bytes
/// actually present. A truncated, hostile or simply foreign blob produces a
/// [`StateError`] — never a panic, never an allocation sized by a number the attacker
/// chose.
///
/// ```
/// use daux_state::{StateReader, StateVersion, StateWriter};
///
/// let mut w = StateWriter::new(StateVersion(2));
/// w.put_bool("bypass", true);
/// let bytes = w.try_finish().unwrap();
///
/// let r = StateReader::from_bytes(&bytes).unwrap();
/// assert_eq!(r.version(), StateVersion(2));
/// assert_eq!(r.bool("bypass"), Ok(true));
/// assert_eq!(r.opt_bool("nope"), None);
///
/// // Garbage is rejected, not tolerated.
/// assert!(StateReader::from_bytes(b"not a state blob").is_err());
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct StateReader {
    format_version: u32,
    doc: StateDoc,
}

impl StateReader {
    /// Parses `bytes` with the default limits. [main-thread]
    pub fn from_bytes(bytes: &[u8]) -> StateResult<Self> {
        Self::from_bytes_with_limits(bytes, &StateLimits::DEFAULT)
    }

    /// Parses `bytes` with explicit limits. [main-thread]
    pub fn from_bytes_with_limits(bytes: &[u8], limits: &StateLimits) -> StateResult<Self> {
        let decoded = crate::codec::decode(bytes, limits)?;
        Ok(Self {
            format_version: decoded.format_version,
            doc: StateDoc::from_entries(decoded.schema_version, decoded.entries),
        })
    }

    /// Reads a whole document from `r` with the default limits. [main-thread]
    ///
    /// Reading is bounded: at most [`StateLimits::max_blob_bytes`] + 1 bytes are pulled
    /// from the reader, so an endless or hostile stream cannot exhaust memory.
    pub fn read_from(r: &mut dyn Read) -> StateResult<Self> {
        Self::read_from_with_limits(r, &StateLimits::DEFAULT)
    }

    /// [`StateReader::read_from`] with explicit limits. [main-thread]
    pub fn read_from_with_limits(r: &mut dyn Read, limits: &StateLimits) -> StateResult<Self> {
        let cap = limits.max_blob_bytes;
        // `+ 1` so an over-long stream is detected rather than silently truncated.
        let over = cap.saturating_add(1) as u64;
        let mut bytes = Vec::new();
        Read::take(&mut *r, over).read_to_end(&mut bytes)?;
        if bytes.len() > cap {
            return Err(StateError::limit_exceeded(format!(
                "stream carries more than the maximum of {cap} bytes"
            )));
        }
        Self::from_bytes_with_limits(&bytes, limits)
    }

    /// The plug-in schema version the document declares. [main-thread]
    #[inline]
    #[must_use]
    pub const fn version(&self) -> StateVersion {
        self.doc.version()
    }

    /// The container format version the document was written with — always between `1`
    /// and [`crate::format::FORMAT_VERSION`]. [main-thread]
    #[inline]
    #[must_use]
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// The parsed document. [main-thread]
    #[inline]
    #[must_use]
    pub const fn doc(&self) -> &StateDoc {
        &self.doc
    }

    /// Consumes the reader and yields the mutable document, ready for a
    /// [`MigrationChain`](crate::MigrationChain). [main-thread]
    #[inline]
    #[must_use]
    pub fn into_doc(self) -> StateDoc {
        self.doc
    }

    /// The top-level entries, in the order they were written. [main-thread]
    #[inline]
    #[must_use]
    pub fn entries(&self) -> &[StateEntry] {
        self.doc.entries()
    }

    /// Reads the floating-point number at `path`. [main-thread]
    pub fn f64(&self, path: &str) -> StateResult<f64> {
        self.doc.f64(path)
    }

    /// Reads the integer at `path`. [main-thread]
    pub fn i64(&self, path: &str) -> StateResult<i64> {
        self.doc.i64(path)
    }

    /// Reads the boolean at `path`. [main-thread]
    pub fn bool(&self, path: &str) -> StateResult<bool> {
        self.doc.bool(path)
    }

    /// Reads the string at `path`. [main-thread]
    pub fn str(&self, path: &str) -> StateResult<&str> {
        self.doc.str(path)
    }

    /// Reads the byte string at `path`. [main-thread]
    pub fn bytes(&self, path: &str) -> StateResult<&[u8]> {
        self.doc.bytes(path)
    }

    /// Borrows the nested group at `path`. [main-thread]
    pub fn group(&self, path: &str) -> StateResult<StateGroup<'_>> {
        self.doc.group(path)
    }

    /// The floating-point number at `path`, or `None` if it is absent or another type.
    /// [main-thread]
    #[must_use]
    pub fn opt_f64(&self, path: &str) -> Option<f64> {
        self.doc.opt_f64(path)
    }

    /// The integer at `path`, or `None` if it is absent or another type. [main-thread]
    #[must_use]
    pub fn opt_i64(&self, path: &str) -> Option<i64> {
        self.doc.opt_i64(path)
    }

    /// The boolean at `path`, or `None` if it is absent or another type. [main-thread]
    #[must_use]
    pub fn opt_bool(&self, path: &str) -> Option<bool> {
        self.doc.opt_bool(path)
    }

    /// The string at `path`, or `None` if it is absent or another type. [main-thread]
    #[must_use]
    pub fn opt_str(&self, path: &str) -> Option<&str> {
        self.doc.opt_str(path)
    }

    /// The byte string at `path`, or `None` if it is absent or another type.
    /// [main-thread]
    #[must_use]
    pub fn opt_bytes(&self, path: &str) -> Option<&[u8]> {
        self.doc.opt_bytes(path)
    }

    /// The nested group at `path`, or `None` if it is absent or another type.
    /// [main-thread]
    #[must_use]
    pub fn opt_group(&self, path: &str) -> Option<StateGroup<'_>> {
        self.doc.opt_group(path)
    }

    /// The raw value at `path`, or `None`. [main-thread]
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&Value> {
        self.doc.get(path)
    }

    /// `true` when `path` resolves. [main-thread]
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        self.doc.contains(path)
    }

    /// The top-level keys, in order. [main-thread]
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.doc.keys()
    }

    /// Number of top-level entries. [main-thread]
    #[must_use]
    pub fn len(&self) -> usize {
        self.doc.len()
    }

    /// `true` when the document has no top-level entries. [main-thread]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.doc.is_empty()
    }
}

impl From<StateReader> for StateDoc {
    #[inline]
    fn from(r: StateReader) -> Self {
        r.into_doc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StateErrorKind, StateWriter, format};

    fn sample_bytes() -> Vec<u8> {
        let mut w = StateWriter::new(StateVersion(2));
        w.put_f64("gain", -6.0);
        w.put_i64("count", 3);
        w.put_bool("bypass", false);
        w.put_str("name", "Lead");
        w.put_bytes("curve", &[4, 5, 6]);
        w.begin_group("filter");
        w.put_str("mode", "lowpass");
        w.begin_group("env");
        w.put_f64("attack", 0.01);
        w.end_group();
        w.end_group();
        w.try_finish().expect("valid")
    }

    #[test]
    fn round_trips_every_type_including_nested_groups() {
        let r = StateReader::from_bytes(&sample_bytes()).expect("decodes");
        assert_eq!(r.version(), StateVersion(2));
        assert_eq!(r.format_version(), format::FORMAT_VERSION);
        assert_eq!(r.f64("gain"), Ok(-6.0));
        assert_eq!(r.i64("count"), Ok(3));
        assert_eq!(r.bool("bypass"), Ok(false));
        assert_eq!(r.str("name"), Ok("Lead"));
        assert_eq!(r.bytes("curve"), Ok(&[4u8, 5, 6][..]));
        assert_eq!(r.str("filter/mode"), Ok("lowpass"));
        assert_eq!(r.f64("filter/env/attack"), Ok(0.01));
        assert_eq!(r.len(), 6);
        assert!(!r.is_empty());
        assert_eq!(
            r.keys().collect::<Vec<_>>(),
            vec!["gain", "count", "bypass", "name", "curve", "filter"]
        );
        assert_eq!(r.entries().len(), 6);
    }

    #[test]
    fn group_views_are_relative() {
        let r = StateReader::from_bytes(&sample_bytes()).expect("decodes");
        let filter = r.group("filter").expect("group");
        assert_eq!(filter.str("mode"), Ok("lowpass"));
        assert_eq!(filter.f64("env/attack"), Ok(0.01));
        assert!(r.opt_group("filter").is_some());
        assert!(r.opt_group("gain").is_none());
        assert!(r.group("gain").is_err());
    }

    #[test]
    fn optional_reads_never_fail() {
        let r = StateReader::from_bytes(&sample_bytes()).expect("decodes");
        assert_eq!(r.opt_f64("gain"), Some(-6.0));
        assert_eq!(r.opt_f64("nope"), None);
        assert_eq!(r.opt_i64("gain"), None); // present but wrong type
        assert_eq!(r.opt_bool("bypass"), Some(false));
        assert_eq!(r.opt_str("name"), Some("Lead"));
        assert_eq!(r.opt_bytes("curve"), Some(&[4u8, 5, 6][..]));
        assert!(r.contains("filter/env/attack"));
        assert!(!r.contains("filter/env/decay"));
        assert!(r.get("gain").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn missing_and_mistyped_reads_name_the_key() {
        let r = StateReader::from_bytes(&sample_bytes()).expect("decodes");
        let err = r.f64("filter/env/decay").expect_err("absent");
        assert_eq!(err.kind(), &StateErrorKind::MissingField);
        assert!(err.to_string().contains("filter/env/decay"));

        let err = r.str("gain").expect_err("wrong type");
        assert!(err.to_string().contains("gain"));
        assert!(err.to_string().contains("f64"));
    }

    #[test]
    fn rejects_obvious_garbage() {
        assert!(StateReader::from_bytes(b"").is_err());
        assert!(StateReader::from_bytes(b"not a state blob at all").is_err());
        assert!(StateReader::from_bytes(&[0u8; 20]).is_err());
    }

    #[test]
    fn reads_from_a_stream() {
        let bytes = sample_bytes();
        let mut cursor = std::io::Cursor::new(bytes.clone());
        let r = StateReader::read_from(&mut cursor).expect("decodes");
        assert_eq!(r.f64("gain"), Ok(-6.0));
    }

    #[test]
    fn a_stream_over_the_limit_is_rejected_before_parsing() {
        let bytes = sample_bytes();
        let limits = StateLimits::default().with_max_blob_bytes(bytes.len() - 1);
        let mut cursor = std::io::Cursor::new(bytes);
        let err = StateReader::read_from_with_limits(&mut cursor, &limits).expect_err("too big");
        assert_eq!(err.kind(), &StateErrorKind::LimitExceeded);
    }

    #[test]
    fn an_endless_stream_is_bounded() {
        struct Endless;
        impl Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                buf.fill(0xab);
                Ok(buf.len())
            }
        }
        let limits = StateLimits::default().with_max_blob_bytes(4096);
        let err = StateReader::read_from_with_limits(&mut Endless, &limits).expect_err("bounded");
        assert_eq!(err.kind(), &StateErrorKind::LimitExceeded);
    }

    #[test]
    fn stream_io_errors_surface() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "nope",
                ))
            }
        }
        let err = StateReader::read_from(&mut Failing).expect_err("io failure");
        assert_eq!(err.kind(), &StateErrorKind::Io);
    }

    #[test]
    fn into_doc_hands_over_a_mutable_document() {
        let r = StateReader::from_bytes(&sample_bytes()).expect("decodes");
        let mut doc = r.into_doc();
        doc.insert("added", 1i64).expect("insert");
        assert_eq!(doc.i64("added"), Ok(1));

        let r = StateReader::from_bytes(&sample_bytes()).expect("decodes");
        assert_eq!(r.doc().version(), StateVersion(2));
        let doc: StateDoc = r.into();
        assert_eq!(doc.version(), StateVersion(2));
    }

    #[test]
    fn an_empty_document_reads_as_empty() {
        let bytes = StateWriter::new(StateVersion(1))
            .try_finish()
            .expect("valid");
        let r = StateReader::from_bytes(&bytes).expect("decodes");
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert_eq!(r.keys().count(), 0);
        assert_eq!(r.opt_f64("anything"), None);
    }
}
