//! The byte-level encoder and the bounds-checked decoder.
//!
//! Both directions are deliberately in one module so the two halves of the format cannot
//! drift apart. See [`crate::format`] for the layout they implement.

use crate::error::{StateError, StateResult};
use crate::format;
use crate::limits::StateLimits;
use crate::value::{StateEntry, Value};
use crate::version::StateVersion;

// ---------------------------------------------------------------------------- encode ---

/// Streaming encoder shared by [`StateWriter`](crate::StateWriter) and
/// [`StateDoc::to_bytes`](crate::StateDoc::to_bytes).
///
/// Errors are *latched* rather than returned per call: the writer API is infallible so
/// plug-in authors can write straight-line save code, and the first failure surfaces at
/// [`Encoder::finish`]. Once latched, further entries are dropped, so a poisoned encoder
/// cannot silently produce a half-written document that still looks structurally valid.
pub(crate) struct Encoder {
    buf: Vec<u8>,
    entries: u32,
    depth: usize,
    limits: StateLimits,
    error: Option<StateError>,
}

impl Encoder {
    /// Starts a document, writing the header with a placeholder entry count.
    pub(crate) fn new(version: StateVersion, limits: StateLimits) -> Self {
        let mut buf = Vec::with_capacity(format::HEADER_LEN);
        buf.extend_from_slice(&format::MAGIC);
        buf.extend_from_slice(&format::FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&version.get().to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        debug_assert_eq!(buf.len(), format::HEADER_LEN);
        Self {
            buf,
            entries: 0,
            depth: 0,
            limits,
            error: None,
        }
    }

    /// The first error latched, if any.
    pub(crate) fn error(&self) -> Option<&StateError> {
        self.error.as_ref()
    }

    /// Number of entries accepted so far, including group markers.
    pub(crate) const fn entry_count(&self) -> u32 {
        self.entries
    }

    /// Current group nesting depth.
    pub(crate) const fn depth(&self) -> usize {
        self.depth
    }

    fn latch(&mut self, e: StateError) {
        if self.error.is_none() {
            self.error = Some(e);
        }
    }

    /// Validates a key for use as an entry name.
    pub(crate) fn check_key(key: &str, limits: &StateLimits) -> StateResult<()> {
        if key.is_empty() {
            return Err(StateError::invalid_key(key, "a state key may not be empty"));
        }
        if key.len() > limits.max_key_bytes {
            // Reported as a limit, not a malformed key, so that a writer and a reader
            // rejecting the same over-long key agree on the error kind.
            return Err(StateError::limit_exceeded(format!(
                "key is {} bytes, the maximum is {}",
                key.len(),
                limits.max_key_bytes
            ))
            .with_key(key));
        }
        if key.contains(format::PATH_SEPARATOR) {
            return Err(StateError::invalid_key(
                key,
                "a state key may not contain '/', which separates path segments",
            ));
        }
        Ok(())
    }

    /// Reserves room for `extra` more bytes, latching if that would blow the blob budget.
    fn reserve(&mut self, extra: usize) -> bool {
        let Some(total) = self.buf.len().checked_add(extra) else {
            self.latch(StateError::limit_exceeded(
                "state blob size overflowed usize",
            ));
            return false;
        };
        if total > self.limits.max_blob_bytes {
            self.latch(StateError::limit_exceeded(format!(
                "writing this entry would grow the blob to {total} bytes, the maximum is {}",
                self.limits.max_blob_bytes
            )));
            return false;
        }
        true
    }

    /// Undoes a partially written entry so the buffer never contains a torn record.
    fn rollback(&mut self, mark: usize) {
        self.buf.truncate(mark);
        self.entries = self.entries.saturating_sub(1);
    }

    /// Writes `key` + `tag` and counts the entry. Returns `false` if anything was latched.
    fn begin_entry(&mut self, key: &str, tag: u8) -> bool {
        if self.error.is_some() {
            return false;
        }
        if let Err(e) = Self::check_key(key, &self.limits) {
            self.latch(e);
            return false;
        }
        self.write_entry_head(key, tag)
    }

    /// Writes an entry head without the key or poison checks. Used for the empty key of a
    /// group-end marker, including the ones [`Encoder::finish`] emits to balance groups a
    /// caller forgot to close.
    fn write_entry_head(&mut self, key: &str, tag: u8) -> bool {
        if self.entries as usize >= self.limits.max_entries {
            self.latch(StateError::limit_exceeded(format!(
                "entry count would exceed the maximum of {}",
                self.limits.max_entries
            )));
            return false;
        }
        if !self.reserve(4 + key.len() + 1) {
            return false;
        }
        let Ok(key_len) = u32::try_from(key.len()) else {
            self.latch(StateError::invalid_key(
                key,
                "key length does not fit in u32",
            ));
            return false;
        };
        self.buf.extend_from_slice(&key_len.to_le_bytes());
        self.buf.extend_from_slice(key.as_bytes());
        self.buf.push(tag);
        self.entries += 1;
        true
    }

    /// Appends a `u32`-length-prefixed payload. Returns `false` if it did not fit.
    fn push_blob(&mut self, key: &str, payload: &[u8]) -> bool {
        let Ok(len) = u32::try_from(payload.len()) else {
            self.latch(
                StateError::limit_exceeded("value is larger than u32::MAX bytes").with_key(key),
            );
            return false;
        };
        if !self.reserve(4 + payload.len()) {
            return false;
        }
        self.buf.extend_from_slice(&len.to_le_bytes());
        self.buf.extend_from_slice(payload);
        true
    }

    /// Writes a fixed-width payload, rolling the entry back if the budget is exhausted.
    fn put_fixed(&mut self, key: &str, tag: u8, payload: &[u8]) {
        let mark = self.buf.len();
        if !self.begin_entry(key, tag) {
            return;
        }
        if self.reserve(payload.len()) {
            self.buf.extend_from_slice(payload);
        } else {
            self.rollback(mark);
        }
    }

    pub(crate) fn put_f64(&mut self, key: &str, v: f64) {
        self.put_fixed(key, format::TAG_F64, &v.to_le_bytes());
    }

    pub(crate) fn put_i64(&mut self, key: &str, v: i64) {
        self.put_fixed(key, format::TAG_I64, &v.to_le_bytes());
    }

    pub(crate) fn put_bool(&mut self, key: &str, v: bool) {
        self.put_fixed(key, format::TAG_BOOL, &[u8::from(v)]);
    }

    pub(crate) fn put_str(&mut self, key: &str, v: &str) {
        self.put_blob_entry(key, format::TAG_STR, v.as_bytes());
    }

    pub(crate) fn put_bytes(&mut self, key: &str, v: &[u8]) {
        self.put_blob_entry(key, format::TAG_BYTES, v);
    }

    fn put_blob_entry(&mut self, key: &str, tag: u8, payload: &[u8]) {
        let mark = self.buf.len();
        if !self.begin_entry(key, tag) {
            return;
        }
        if !self.push_blob(key, payload) {
            self.rollback(mark);
        }
    }

    pub(crate) fn begin_group(&mut self, key: &str) {
        if self.depth >= self.limits.max_depth {
            self.latch(
                StateError::limit_exceeded(format!(
                    "group nesting deeper than the maximum of {}",
                    self.limits.max_depth
                ))
                .with_key(key),
            );
            return;
        }
        if self.begin_entry(key, format::TAG_GROUP_BEGIN) {
            self.depth += 1;
        }
    }

    pub(crate) fn end_group(&mut self) {
        if self.depth == 0 {
            self.latch(StateError::corrupt("end_group() called with no group open"));
            return;
        }
        if self.error.is_some() {
            return;
        }
        if self.write_entry_head("", format::TAG_GROUP_END) {
            self.depth -= 1;
        }
    }

    /// Encodes a whole subtree, used by [`crate::StateDoc::to_bytes`].
    pub(crate) fn put_entries(&mut self, entries: &[StateEntry]) {
        for entry in entries {
            match &entry.value {
                Value::F64(v) => self.put_f64(&entry.key, *v),
                Value::I64(v) => self.put_i64(&entry.key, *v),
                Value::Bool(v) => self.put_bool(&entry.key, *v),
                Value::Str(v) => self.put_str(&entry.key, v),
                Value::Bytes(v) => self.put_bytes(&entry.key, v),
                Value::Group(children) => {
                    self.begin_group(&entry.key);
                    self.put_entries(children);
                    self.end_group();
                }
            }
        }
    }

    /// Closes any group left open, patches the entry count into the header and returns the
    /// bytes together with the first latched error.
    pub(crate) fn finish(mut self) -> (Vec<u8>, Option<StateError>) {
        if self.depth > 0 {
            let unclosed = self.depth;
            // Emit the missing markers first — bypassing the poison check on purpose — so
            // the bytes stay parseable even for a caller that ignores the error, then latch.
            while self.depth > 0 {
                if !self.write_entry_head("", format::TAG_GROUP_END) {
                    break;
                }
                self.depth -= 1;
            }
            self.latch(StateError::corrupt(format!(
                "{unclosed} group(s) were never closed with end_group()"
            )));
        }
        let count = self.entries.to_le_bytes();
        self.buf[format::OFFSET_ENTRY_COUNT..format::OFFSET_ENTRY_COUNT + 4]
            .copy_from_slice(&count);
        (self.buf, self.error)
    }
}

// ---------------------------------------------------------------------------- decode ---

/// A bounds-checked forward cursor over the input. Every read validates against the bytes
/// actually present before it is performed; nothing is ever allocated on the strength of a
/// length field alone.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    const fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, n: usize, what: &str) -> StateResult<&'a [u8]> {
        if n > self.remaining() {
            return Err(StateError::corrupt(format!(
                "{what} needs {n} bytes but only {} remain",
                self.remaining()
            ))
            .at_offset(self.pos));
        }
        let start = self.pos;
        self.pos += n;
        Ok(&self.bytes[start..self.pos])
    }

    fn u8(&mut self, what: &str) -> StateResult<u8> {
        Ok(self.take(1, what)?[0])
    }

    fn u32(&mut self, what: &str) -> StateResult<u32> {
        let b = self.take(4, what)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self, what: &str) -> StateResult<u64> {
        let b = self.take(8, what)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Reads a `u32` length prefix and rejects it unless it is within both `max` and the
    /// bytes still available. This is the single choke point that makes a hostile length
    /// prefix harmless.
    fn length(&mut self, what: &str, max: usize) -> StateResult<usize> {
        let at = self.pos;
        let raw = self.u32(what)? as usize;
        if raw > max {
            return Err(StateError::limit_exceeded(format!(
                "{what} is {raw} bytes, the maximum is {max}"
            ))
            .at_offset(at));
        }
        if raw > self.remaining() {
            return Err(StateError::corrupt(format!(
                "{what} claims {raw} bytes but only {} remain in the blob",
                self.remaining()
            ))
            .at_offset(at));
        }
        Ok(raw)
    }
}

/// A parsed document: the header fields plus the entry tree.
#[derive(Debug)]
pub(crate) struct Decoded {
    pub(crate) format_version: u32,
    pub(crate) schema_version: StateVersion,
    pub(crate) entries: Vec<StateEntry>,
}

/// Parses a complete state blob.
///
/// Never panics and never allocates more than the input it was given: every length is
/// checked against [`StateLimits`] *and* against the bytes remaining before use.
pub(crate) fn decode(bytes: &[u8], limits: &StateLimits) -> StateResult<Decoded> {
    if bytes.len() > limits.max_blob_bytes {
        return Err(StateError::limit_exceeded(format!(
            "blob is {} bytes, the maximum is {}",
            bytes.len(),
            limits.max_blob_bytes
        )));
    }
    let mut cur = Cursor::new(bytes);
    let magic = cur.take(format::MAGIC.len(), "header magic")?;
    if magic != format::MAGIC {
        return Err(StateError::corrupt("wrong magic; this is not a DAUx state blob").at_offset(0));
    }
    let format_version = cur.u32("header format version")?;
    if format_version == 0 || format_version > format::FORMAT_VERSION {
        return Err(
            StateError::unsupported_version(format_version, format::FORMAT_VERSION)
                .at_offset(format::OFFSET_FORMAT_VERSION),
        );
    }
    let schema_version = StateVersion(cur.u32("header schema version")?);
    let entry_count = cur.u32("header entry count")? as usize;
    if entry_count > limits.max_entries {
        return Err(StateError::limit_exceeded(format!(
            "blob declares {entry_count} entries, the maximum is {}",
            limits.max_entries
        ))
        .at_offset(format::OFFSET_ENTRY_COUNT));
    }
    // An entry is at least 4 (key length) + 1 (tag) bytes, so a declared count that could
    // not possibly fit is rejected before a single allocation.
    if entry_count > cur.remaining() / 5 {
        return Err(StateError::corrupt(format!(
            "blob declares {entry_count} entries but only {} bytes follow the header",
            cur.remaining()
        ))
        .at_offset(format::OFFSET_ENTRY_COUNT));
    }

    let mut root: Vec<StateEntry> = Vec::new();
    let mut open: Vec<(String, Vec<StateEntry>)> = Vec::new();

    for _ in 0..entry_count {
        let key_at = cur.pos;
        let key_len = cur.length("key length", limits.max_key_bytes)?;
        let key_bytes = cur.take(key_len, "key")?;
        let key = core::str::from_utf8(key_bytes)
            .map_err(|e| {
                StateError::corrupt(format!("key is not valid UTF-8: {e}")).at_offset(key_at)
            })?
            .to_owned();
        let tag = cur.u8("type tag")?;

        if tag == format::TAG_GROUP_END {
            if !key.is_empty() {
                return Err(
                    StateError::corrupt("group-end marker must carry an empty key")
                        .with_key(&key)
                        .at_offset(key_at),
                );
            }
            let Some((group_key, children)) = open.pop() else {
                return Err(
                    StateError::corrupt("group-end marker with no group open").at_offset(key_at)
                );
            };
            let parent = open.last_mut().map_or(&mut root, |(_, items)| items);
            parent.push(StateEntry {
                key: group_key,
                value: Value::Group(children),
            });
            continue;
        }

        if key.is_empty() {
            return Err(
                StateError::corrupt("only a group-end marker may carry an empty key")
                    .at_offset(key_at),
            );
        }
        if key.contains(format::PATH_SEPARATOR) {
            return Err(StateError::invalid_key(
                &key,
                "a state key may not contain '/', which separates path segments",
            )
            .at_offset(key_at));
        }

        if tag == format::TAG_GROUP_BEGIN {
            if open.len() >= limits.max_depth {
                return Err(StateError::limit_exceeded(format!(
                    "group nesting deeper than the maximum of {}",
                    limits.max_depth
                ))
                .with_key(&key)
                .at_offset(key_at));
            }
            open.push((key, Vec::new()));
            continue;
        }

        let value = read_value(&mut cur, tag, &key, limits)?;
        let parent = open.last_mut().map_or(&mut root, |(_, items)| items);
        parent.push(StateEntry { key, value });
    }

    if let Some((key, _)) = open.last() {
        return Err(
            StateError::corrupt(format!("{} group(s) were never closed", open.len()))
                .with_key(key)
                .at_offset(cur.pos),
        );
    }
    if cur.remaining() != 0 {
        return Err(StateError::corrupt(format!(
            "{} trailing byte(s) after the last declared entry",
            cur.remaining()
        ))
        .at_offset(cur.pos));
    }

    Ok(Decoded {
        format_version,
        schema_version,
        entries: root,
    })
}

/// Reads one non-structural value.
fn read_value(
    cur: &mut Cursor<'_>,
    tag: u8,
    key: &str,
    limits: &StateLimits,
) -> StateResult<Value> {
    match tag {
        format::TAG_F64 => Ok(Value::F64(f64::from_bits(cur.u64("f64 value")?))),
        format::TAG_I64 => {
            let bits = cur.u64("i64 value")?;
            Ok(Value::I64(bits as i64))
        }
        format::TAG_BOOL => {
            let at = cur.pos;
            match cur.u8("bool value")? {
                0 => Ok(Value::Bool(false)),
                1 => Ok(Value::Bool(true)),
                other => Err(StateError::corrupt(format!(
                    "bool value must be 0 or 1, found {other}"
                ))
                .with_key(key)
                .at_offset(at)),
            }
        }
        format::TAG_STR => {
            let at = cur.pos;
            let len = cur.length("string length", limits.max_blob_bytes)?;
            let raw = cur.take(len, "string value")?;
            let text = core::str::from_utf8(raw).map_err(|e| {
                StateError::corrupt(format!("string value is not valid UTF-8: {e}"))
                    .with_key(key)
                    .at_offset(at)
            })?;
            Ok(Value::Str(text.to_owned()))
        }
        format::TAG_BYTES => {
            let len = cur.length("byte-string length", limits.max_blob_bytes)?;
            let raw = cur.take(len, "byte-string value")?;
            Ok(Value::Bytes(raw.to_vec()))
        }
        other => Err(StateError::corrupt(format!("unknown type tag {other}"))
            .with_key(key)
            .at_offset(cur.pos - 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_pairs() -> Vec<u8> {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.put_f64("gain", 0.5);
        e.begin_group("filter");
        e.put_i64("kind", 2);
        e.end_group();
        let (bytes, err) = e.finish();
        assert!(err.is_none(), "{err:?}");
        bytes
    }

    #[test]
    fn header_layout_is_exactly_as_documented() {
        let bytes = encode_pairs();
        assert_eq!(&bytes[0..8], b"DAUXST\0\0");
        assert_eq!(&bytes[8..12], &1u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &1u32.to_le_bytes());
        // gain, group-begin, kind, group-end
        assert_eq!(&bytes[16..20], &4u32.to_le_bytes());
    }

    #[test]
    fn empty_document_is_header_only() {
        let (bytes, err) = Encoder::new(StateVersion(7), StateLimits::default()).finish();
        assert!(err.is_none());
        assert_eq!(bytes.len(), format::HEADER_LEN);
        let decoded = decode(&bytes, &StateLimits::default()).expect("decodes");
        assert_eq!(decoded.schema_version, StateVersion(7));
        assert!(decoded.entries.is_empty());
    }

    #[test]
    fn decode_rebuilds_the_tree() {
        let bytes = encode_pairs();
        let decoded = decode(&bytes, &StateLimits::default()).expect("decodes");
        assert_eq!(decoded.format_version, 1);
        assert_eq!(decoded.entries.len(), 2);
        assert_eq!(decoded.entries[0].key, "gain");
        assert_eq!(decoded.entries[0].value, Value::F64(0.5));
        assert_eq!(decoded.entries[1].key, "filter");
        let children = decoded.entries[1].value.as_group().expect("group");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].value, Value::I64(2));
    }

    #[test]
    fn unclosed_group_is_latched_but_still_emits_valid_bytes() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.begin_group("a");
        e.put_bool("b", true);
        let (bytes, err) = e.finish();
        let err = err.expect("unclosed group is an error");
        assert!(err.to_string().contains("never closed"), "{err}");
        // …and the bytes still parse, so a caller that ignored the error is not left with
        // a blob that blows up somewhere else.
        let decoded = decode(&bytes, &StateLimits::default()).expect("still parses");
        assert_eq!(decoded.entries.len(), 1);
    }

    #[test]
    fn end_group_without_begin_is_latched() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.end_group();
        assert!(e.error().is_some());
        assert_eq!(e.entry_count(), 0);
        assert_eq!(e.depth(), 0);
    }

    #[test]
    fn once_latched_further_entries_are_dropped() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.put_f64("", 1.0); // invalid key
        e.put_f64("ok", 2.0);
        let (_, err) = e.finish();
        let err = err.expect("latched");
        assert_eq!(err.kind(), &crate::StateErrorKind::InvalidKey);
    }

    #[test]
    fn writer_respects_the_blob_budget() {
        let limits = StateLimits::default().with_max_blob_bytes(format::HEADER_LEN + 8);
        let mut e = Encoder::new(StateVersion(1), limits);
        e.put_f64("k", 1.0); // 4 + 1 + 1 + 8 = 14 bytes > 8
        let (_, err) = e.finish();
        assert_eq!(
            err.expect("latched").kind(),
            &crate::StateErrorKind::LimitExceeded
        );
    }

    #[test]
    fn writer_respects_the_depth_budget() {
        let limits = StateLimits::default().with_max_depth(2);
        let mut e = Encoder::new(StateVersion(1), limits);
        e.begin_group("a");
        e.begin_group("b");
        e.begin_group("c");
        let (_, err) = e.finish();
        assert_eq!(
            err.expect("latched").kind(),
            &crate::StateErrorKind::LimitExceeded
        );
    }

    #[test]
    fn writer_respects_the_entry_budget() {
        let limits = StateLimits::default().with_max_entries(1);
        let mut e = Encoder::new(StateVersion(1), limits);
        e.put_bool("a", true);
        e.put_bool("b", true);
        let (_, err) = e.finish();
        assert_eq!(
            err.expect("latched").kind(),
            &crate::StateErrorKind::LimitExceeded
        );
    }

    #[test]
    fn rejects_a_blob_shorter_than_the_header() {
        for len in 0..format::HEADER_LEN {
            let bytes = vec![0u8; len];
            let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
            assert_eq!(err.kind(), &crate::StateErrorKind::Corrupt, "len {len}");
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = encode_pairs();
        bytes[3] = b'x';
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert!(err.to_string().contains("magic"), "{err}");
    }

    #[test]
    fn rejects_a_newer_format_version() {
        let mut bytes = encode_pairs();
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert_eq!(
            err.kind(),
            &crate::StateErrorKind::UnsupportedVersion {
                found: 99,
                supported: format::FORMAT_VERSION
            }
        );
    }

    #[test]
    fn rejects_format_version_zero() {
        let mut bytes = encode_pairs();
        bytes[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(decode(&bytes, &StateLimits::default()).is_err());
    }

    #[test]
    fn rejects_an_absurd_entry_count_without_allocating() {
        let mut bytes = encode_pairs();
        bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert_eq!(err.kind(), &crate::StateErrorKind::LimitExceeded);

        // Just under the entry limit, still impossible for the bytes present.
        bytes[16..20].copy_from_slice(&1000u32.to_le_bytes());
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert_eq!(err.kind(), &crate::StateErrorKind::Corrupt);
    }

    #[test]
    fn rejects_an_entry_count_lower_than_the_entries_present() {
        let mut bytes = encode_pairs();
        bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert!(err.to_string().contains("trailing"), "{err}");
    }

    #[test]
    fn rejects_a_length_prefix_past_the_end() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.put_bytes("blob", &[1, 2, 3, 4]);
        let (mut bytes, _) = e.finish();
        // The byte-string length sits right after "blob" + tag.
        let at = format::HEADER_LEN + 4 + 4 + 1;
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert!(
            matches!(
                err.kind(),
                crate::StateErrorKind::Corrupt | crate::StateErrorKind::LimitExceeded
            ),
            "{err}"
        );
    }

    #[test]
    fn rejects_an_unknown_tag() {
        let mut bytes = encode_pairs();
        let tag_at = format::HEADER_LEN + 4 + 4;
        bytes[tag_at] = 42;
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert!(err.to_string().contains("unknown type tag 42"), "{err}");
    }

    #[test]
    fn rejects_a_non_boolean_bool_byte() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.put_bool("on", true);
        let (mut bytes, _) = e.finish();
        *bytes.last_mut().expect("non-empty") = 2;
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert!(err.to_string().contains("must be 0 or 1"), "{err}");
        assert_eq!(err.key(), Some("on"));
    }

    #[test]
    fn rejects_invalid_utf8_in_a_key_and_in_a_string() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.put_str("name", "ok");
        let (bytes, _) = e.finish();

        let mut broken_key = bytes.clone();
        broken_key[format::HEADER_LEN + 4] = 0xff;
        let err = decode(&broken_key, &StateLimits::default()).expect_err("must fail");
        assert!(err.to_string().contains("UTF-8"), "{err}");

        let mut broken_value = bytes;
        *broken_value.last_mut().expect("non-empty") = 0xff;
        let err = decode(&broken_value, &StateLimits::default()).expect_err("must fail");
        assert!(err.to_string().contains("UTF-8"), "{err}");
    }

    #[test]
    fn rejects_an_over_long_key_on_read() {
        let long = "k".repeat(40);
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.put_bool(&long, true);
        let (bytes, _) = e.finish();
        let limits = StateLimits::default().with_max_key_bytes(8);
        let err = decode(&bytes, &limits).expect_err("must fail");
        assert_eq!(err.kind(), &crate::StateErrorKind::LimitExceeded);
    }

    #[test]
    fn rejects_a_group_end_with_a_non_empty_key() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.begin_group("g");
        e.end_group();
        let (mut bytes, _) = e.finish();
        // Rewrite the trailing end marker's key length from 0 to 1 and steal the tag byte
        // as the key, leaving the entry structurally wrong.
        let end_at = bytes.len() - 5;
        bytes[end_at..end_at + 4].copy_from_slice(&1u32.to_le_bytes());
        bytes.push(format::TAG_GROUP_END);
        bytes[end_at + 4] = b'g';
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert!(err.to_string().contains("empty key"), "{err}");
    }

    #[test]
    fn rejects_an_unbalanced_group_end() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&format::MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.push(format::TAG_GROUP_END);
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert!(err.to_string().contains("no group open"), "{err}");
    }

    #[test]
    fn rejects_an_empty_key_on_a_value_entry() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&format::MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.push(format::TAG_BOOL);
        bytes.push(1);
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert!(err.to_string().contains("empty key"), "{err}");
    }

    #[test]
    fn rejects_a_key_containing_the_path_separator() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.put_bool("ab", true);
        let (mut bytes, _) = e.finish();
        bytes[format::HEADER_LEN + 4] = b'/';
        let err = decode(&bytes, &StateLimits::default()).expect_err("must fail");
        assert_eq!(err.kind(), &crate::StateErrorKind::InvalidKey);
    }

    #[test]
    fn rejects_nesting_deeper_than_the_limit() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        for i in 0..8 {
            e.begin_group(&format!("g{i}"));
        }
        for _ in 0..8 {
            e.end_group();
        }
        let (bytes, err) = e.finish();
        assert!(err.is_none(), "{err:?}");
        let limits = StateLimits::default().with_max_depth(4);
        let err = decode(&bytes, &limits).expect_err("must fail");
        assert_eq!(err.kind(), &crate::StateErrorKind::LimitExceeded);
    }

    #[test]
    fn rejects_a_blob_over_the_size_limit() {
        let bytes = encode_pairs();
        let limits = StateLimits::default().with_max_blob_bytes(bytes.len() - 1);
        let err = decode(&bytes, &limits).expect_err("must fail");
        assert_eq!(err.kind(), &crate::StateErrorKind::LimitExceeded);
    }

    #[test]
    fn f64_payloads_round_trip_bit_exactly() {
        let specials = [
            0.0f64,
            -0.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
            f64::EPSILON,
            -1.234_567_890_123_456_7e-300,
        ];
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        for (i, v) in specials.iter().enumerate() {
            e.put_f64(&format!("k{i}"), *v);
        }
        let (bytes, err) = e.finish();
        assert!(err.is_none());
        let decoded = decode(&bytes, &StateLimits::default()).expect("decodes");
        for (entry, expected) in decoded.entries.iter().zip(specials) {
            let got = entry.value.as_f64().expect("f64");
            assert_eq!(got.to_bits(), expected.to_bits(), "{}", entry.key);
        }
    }

    #[test]
    fn i64_extremes_round_trip() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.put_i64("min", i64::MIN);
        e.put_i64("max", i64::MAX);
        e.put_i64("neg", -1);
        let (bytes, _) = e.finish();
        let decoded = decode(&bytes, &StateLimits::default()).expect("decodes");
        assert_eq!(decoded.entries[0].value, Value::I64(i64::MIN));
        assert_eq!(decoded.entries[1].value, Value::I64(i64::MAX));
        assert_eq!(decoded.entries[2].value, Value::I64(-1));
    }

    #[test]
    fn empty_strings_and_byte_strings_round_trip() {
        let mut e = Encoder::new(StateVersion(1), StateLimits::default());
        e.put_str("s", "");
        e.put_bytes("b", &[]);
        let (bytes, _) = e.finish();
        let decoded = decode(&bytes, &StateLimits::default()).expect("decodes");
        assert_eq!(decoded.entries[0].value, Value::Str(String::new()));
        assert_eq!(decoded.entries[1].value, Value::Bytes(Vec::new()));
    }

    #[test]
    fn truncating_a_valid_blob_anywhere_is_an_error_never_a_panic() {
        let mut e = Encoder::new(StateVersion(3), StateLimits::default());
        e.put_f64("gain", -6.0);
        e.begin_group("filter");
        e.put_str("mode", "lowpass");
        e.put_bytes("curve", &[9, 8, 7, 6, 5]);
        e.begin_group("nested");
        e.put_bool("on", false);
        e.end_group();
        e.end_group();
        e.put_i64("count", 42);
        let (bytes, err) = e.finish();
        assert!(err.is_none());

        for len in 0..bytes.len() {
            let err = decode(&bytes[..len], &StateLimits::default())
                .err()
                .unwrap_or_else(|| panic!("prefix of {len} bytes must not parse"));
            assert!(!err.to_string().is_empty());
        }
        assert!(decode(&bytes, &StateLimits::default()).is_ok());
    }

    #[test]
    fn corrupting_any_single_byte_never_panics() {
        let mut e = Encoder::new(StateVersion(2), StateLimits::default());
        e.put_f64("gain", 0.25);
        e.begin_group("g");
        e.put_str("mode", "peak");
        e.put_bool("on", true);
        e.end_group();
        let (bytes, _) = e.finish();

        for i in 0..bytes.len() {
            for patch in [0x00u8, 0x01, 0x7f, 0x80, 0xff] {
                let mut corrupted = bytes.clone();
                corrupted[i] = patch;
                // Either outcome is fine; what matters is that neither panics nor hangs.
                let _ = decode(&corrupted, &StateLimits::default());
            }
        }
    }

    #[test]
    fn pseudo_random_garbage_never_panics() {
        // Deterministic xorshift so a failure is reproducible.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..256 {
            let len = (next() % 200) as usize;
            let mut blob = Vec::with_capacity(len);
            for _ in 0..len {
                blob.push((next() & 0xff) as u8);
            }
            // Half the cases get a valid header so the parser reaches the entry loop.
            if case % 2 == 0 && blob.len() >= format::HEADER_LEN {
                blob[..8].copy_from_slice(&format::MAGIC);
                blob[8..12].copy_from_slice(&1u32.to_le_bytes());
            }
            let _ = decode(&blob, &StateLimits::default());
        }
    }
}
