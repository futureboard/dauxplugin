//! The mutable in-memory document, group views, and path lookup.

use crate::codec::{self, Encoder};
use crate::error::{StateError, StateResult};
use crate::format;
use crate::limits::StateLimits;
use crate::value::{StateEntry, Value, ValueType};
use crate::version::StateVersion;

// ------------------------------------------------------------------ path handling ---

/// Splits `path` into its first segment and the remainder, validating both.
fn split_head(path: &str) -> StateResult<(&str, Option<&str>)> {
    let (head, rest) = match path.split_once(format::PATH_SEPARATOR) {
        Some((h, r)) => (h, Some(r)),
        None => (path, None),
    };
    if head.is_empty() {
        return Err(StateError::invalid_key(
            path,
            "path segments may not be empty",
        ));
    }
    Ok((head, rest))
}

/// Resolves `path` against `entries`, or returns [`StateErrorKind::MissingField`].
///
/// [`StateErrorKind::MissingField`]: crate::StateErrorKind::MissingField
pub(crate) fn get_value<'a>(entries: &'a [StateEntry], path: &str) -> StateResult<&'a Value> {
    find(entries, path)?.ok_or_else(|| StateError::missing_field(path))
}

fn find<'a>(entries: &'a [StateEntry], path: &str) -> StateResult<Option<&'a Value>> {
    let (head, rest) = split_head(path)?;
    let Some(entry) = entries.iter().find(|e| e.key == head) else {
        return Ok(None);
    };
    match rest {
        None => Ok(Some(&entry.value)),
        Some(rest) => match &entry.value {
            Value::Group(children) => find(children, rest),
            _ => Ok(None),
        },
    }
}

fn find_mut<'a>(entries: &'a mut [StateEntry], path: &str) -> StateResult<Option<&'a mut Value>> {
    let (head, rest) = split_head(path)?;
    let Some(entry) = entries.iter_mut().find(|e| e.key == head) else {
        return Ok(None);
    };
    match rest {
        None => Ok(Some(&mut entry.value)),
        Some(rest) => match &mut entry.value {
            Value::Group(children) => find_mut(children, rest),
            _ => Ok(None),
        },
    }
}

/// Inserts `value` at `path`, creating intermediate groups as needed.
fn insert_at(
    entries: &mut Vec<StateEntry>,
    path: &str,
    full_path: &str,
    value: Value,
    limits: &StateLimits,
    depth: usize,
) -> StateResult<Option<Value>> {
    let (head, rest) = split_head(path)?;
    Encoder::check_key(head, limits)?;
    if depth >= limits.max_depth {
        return Err(StateError::limit_exceeded(format!(
            "path nests deeper than the maximum of {}",
            limits.max_depth
        ))
        .with_key(full_path));
    }
    let index = entries.iter().position(|e| e.key == head);
    match rest {
        None => match index {
            Some(i) => Ok(Some(core::mem::replace(&mut entries[i].value, value))),
            None => {
                entries.push(StateEntry {
                    key: head.to_owned(),
                    value,
                });
                Ok(None)
            }
        },
        Some(rest) => {
            let i = match index {
                Some(i) => i,
                None => {
                    entries.push(StateEntry {
                        key: head.to_owned(),
                        value: Value::Group(Vec::new()),
                    });
                    entries.len() - 1
                }
            };
            match &mut entries[i].value {
                Value::Group(children) => {
                    insert_at(children, rest, full_path, value, limits, depth + 1)
                }
                other => Err(StateError::type_mismatch(
                    head,
                    ValueType::Group,
                    other.value_type(),
                )),
            }
        }
    }
}

/// Removes the value at `path`, if any.
fn remove_at(entries: &mut Vec<StateEntry>, path: &str) -> StateResult<Option<Value>> {
    let (head, rest) = split_head(path)?;
    let Some(i) = entries.iter().position(|e| e.key == head) else {
        return Ok(None);
    };
    match rest {
        None => Ok(Some(entries.remove(i).value)),
        Some(rest) => match &mut entries[i].value {
            Value::Group(children) => remove_at(children, rest),
            _ => Ok(None),
        },
    }
}

// ------------------------------------------------------------------ typed getters ---

macro_rules! typed_getters {
    ($($fn_name:ident, $opt_name:ident, $ty:ty, $variant:ident, $conv:expr, $doc:literal;)*) => {
        $(
            #[doc = concat!("Reads the ", $doc, " at `path`. [main-thread]")]
            ///
            /// Fails with `MissingField` when the path is absent and with `TypeMismatch`
            /// when it holds something else. No numeric coercion happens: a value written
            /// with `put_i64` is not readable as an `f64`.
            pub fn $fn_name(&self, path: &str) -> StateResult<$ty> {
                let value = get_value(self.entries(), path)?;
                match $conv(value) {
                    Some(v) => Ok(v),
                    None => Err(StateError::type_mismatch(
                        path,
                        ValueType::$variant,
                        value.value_type(),
                    )),
                }
            }

            #[doc = concat!("Reads the ", $doc, " at `path`, or `None` if it is absent or of another type. [main-thread]")]
            #[must_use]
            pub fn $opt_name(&self, path: &str) -> Option<$ty> {
                $conv(find(self.entries(), path).ok()??)
            }
        )*
    };
}

/// Generates the shared read API on anything exposing `entries()`.
macro_rules! impl_readers {
    () => {
        typed_getters! {
            f64, opt_f64, f64, F64, Value::as_f64, "floating-point number";
            i64, opt_i64, i64, I64, Value::as_i64, "integer";
            bool, opt_bool, bool, Bool, Value::as_bool, "boolean";
            str, opt_str, &str, Str, Value::as_str, "string";
            bytes, opt_bytes, &[u8], Bytes, Value::as_bytes, "byte string";
        }

        /// Borrows the nested group at `path` so its members can be read with short,
        /// relative keys. [main-thread]
        pub fn group(&self, path: &str) -> StateResult<StateGroup<'_>> {
            let value = get_value(self.entries(), path)?;
            value.as_group().map(StateGroup::new).ok_or_else(|| {
                StateError::type_mismatch(path, ValueType::Group, value.value_type())
            })
        }

        /// Borrows the nested group at `path`, or `None` if it is absent or not a group.
        /// [main-thread]
        #[must_use]
        pub fn opt_group(&self, path: &str) -> Option<StateGroup<'_>> {
            find(self.entries(), path)
                .ok()??
                .as_group()
                .map(StateGroup::new)
        }

        /// The raw value at `path`, or `None` if the path does not resolve. [main-thread]
        #[must_use]
        pub fn get(&self, path: &str) -> Option<&Value> {
            find(self.entries(), path).ok().flatten()
        }

        /// `true` when `path` resolves to a value. [main-thread]
        #[must_use]
        pub fn contains(&self, path: &str) -> bool {
            self.get(path).is_some()
        }

        /// The keys of this level, in insertion order. [main-thread]
        pub fn keys(&self) -> impl Iterator<Item = &str> {
            self.entries().iter().map(|e| e.key.as_str())
        }

        /// Number of entries at this level. Nested members are not counted.
        /// [main-thread]
        #[must_use]
        pub fn len(&self) -> usize {
            self.entries().len()
        }

        /// `true` when this level holds no entries. [main-thread]
        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.entries().is_empty()
        }
    };
}

// -------------------------------------------------------------------- StateGroup ---

/// A borrowed view of one nested group. [main-thread]
///
/// Paths are relative to the group, so a plug-in can hand a sub-document to the component
/// that owns it without that component knowing where it sits. Error messages name the
/// relative key.
#[derive(Clone, Copy, Debug)]
pub struct StateGroup<'a> {
    entries: &'a [StateEntry],
}

impl<'a> StateGroup<'a> {
    #[inline]
    pub(crate) const fn new(entries: &'a [StateEntry]) -> Self {
        Self { entries }
    }

    /// The entries of this group, in insertion order. [main-thread]
    #[inline]
    #[must_use]
    pub const fn entries(&self) -> &'a [StateEntry] {
        self.entries
    }

    impl_readers!();
}

// ---------------------------------------------------------------------- StateDoc ---

/// A whole state document held in memory: a schema version plus an ordered tree of
/// entries. [main-thread]
///
/// This is the form migrations operate on. It is produced by
/// [`StateReader::into_doc`](crate::StateReader::into_doc) or built from scratch, and
/// serialised with [`StateDoc::to_bytes`], which produces byte-for-byte the same output as
/// driving a [`StateWriter`](crate::StateWriter) through the same sequence of calls.
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StateDoc {
    version: StateVersion,
    entries: Vec<StateEntry>,
}

impl StateDoc {
    /// An empty document at `version`. [main-thread]
    #[inline]
    #[must_use]
    pub const fn new(version: StateVersion) -> Self {
        Self {
            version,
            entries: Vec::new(),
        }
    }

    /// Builds a document from ready-made entries. [main-thread]
    #[inline]
    #[must_use]
    pub const fn from_entries(version: StateVersion, entries: Vec<StateEntry>) -> Self {
        Self { version, entries }
    }

    /// The schema version this document claims. [main-thread]
    #[inline]
    #[must_use]
    pub const fn version(&self) -> StateVersion {
        self.version
    }

    /// Overwrites the schema version. [`MigrationChain`](crate::MigrationChain) does this
    /// after each successful step; migration functions themselves should not. [main-thread]
    #[inline]
    pub const fn set_version(&mut self, version: StateVersion) {
        self.version = version;
    }

    /// The top-level entries, in insertion order. [main-thread]
    #[inline]
    #[must_use]
    pub fn entries(&self) -> &[StateEntry] {
        &self.entries
    }

    /// The top-level entries, mutable. Order is significant: it is the serialisation
    /// order. [main-thread]
    #[inline]
    pub const fn entries_mut(&mut self) -> &mut Vec<StateEntry> {
        &mut self.entries
    }

    impl_readers!();

    /// The value at `path`, mutable, or `None` if the path does not resolve.
    /// [main-thread]
    #[must_use]
    pub fn get_mut(&mut self, path: &str) -> Option<&mut Value> {
        find_mut(&mut self.entries, path).ok().flatten()
    }

    /// Inserts `value` at `path`, replacing and returning any previous value.
    /// [main-thread]
    ///
    /// Missing intermediate groups are created, so `insert("filter/env/attack", …)` works
    /// on an empty document. New entries are appended, so insertion order — and therefore
    /// the serialised byte order — stays deterministic.
    ///
    /// Fails with `InvalidKey` for an empty or over-long segment, `TypeMismatch` when an
    /// intermediate segment exists but is not a group, and `LimitExceeded` when the path
    /// nests deeper than [`StateLimits::max_depth`].
    pub fn insert(&mut self, path: &str, value: impl Into<Value>) -> StateResult<Option<Value>> {
        self.insert_with_limits(path, value, &StateLimits::DEFAULT)
    }

    /// [`StateDoc::insert`] with explicit limits. [main-thread]
    pub fn insert_with_limits(
        &mut self,
        path: &str,
        value: impl Into<Value>,
        limits: &StateLimits,
    ) -> StateResult<Option<Value>> {
        insert_at(&mut self.entries, path, path, value.into(), limits, 0)
    }

    /// Removes and returns the value at `path`. [main-thread]
    pub fn remove(&mut self, path: &str) -> Option<Value> {
        remove_at(&mut self.entries, path).ok().flatten()
    }

    /// Moves the value at `from` to `to`. [main-thread]
    ///
    /// The bread-and-butter migration operation. When both paths share a parent the entry
    /// is renamed **in place**, preserving order; otherwise it is removed and appended at
    /// the destination. Fails with `MissingField` when `from` does not resolve.
    pub fn rename(&mut self, from: &str, to: &str) -> StateResult<()> {
        self.rename_with_limits(from, to, &StateLimits::DEFAULT)
    }

    /// [`StateDoc::rename`] with explicit limits. [main-thread]
    pub fn rename_with_limits(
        &mut self,
        from: &str,
        to: &str,
        limits: &StateLimits,
    ) -> StateResult<()> {
        let (from_parent, from_leaf) = split_parent(from)?;
        let (to_parent, to_leaf) = split_parent(to)?;
        Encoder::check_key(to_leaf, limits)?;

        if from_parent == to_parent {
            let entries = match from_parent {
                None => &mut self.entries,
                Some(parent) => match find_mut(&mut self.entries, parent)? {
                    Some(Value::Group(children)) => children,
                    _ => return Err(StateError::missing_field(from)),
                },
            };
            let Some(i) = entries.iter().position(|e| e.key == from_leaf) else {
                return Err(StateError::missing_field(from));
            };
            if from_leaf != to_leaf && entries.iter().any(|e| e.key == to_leaf) {
                let value = entries.remove(i).value;
                let j = entries
                    .iter()
                    .position(|e| e.key == to_leaf)
                    .expect("checked above");
                entries[j].value = value;
            } else {
                to_leaf.clone_into(&mut entries[i].key);
            }
            return Ok(());
        }

        let value =
            remove_at(&mut self.entries, from)?.ok_or_else(|| StateError::missing_field(from))?;
        insert_at(&mut self.entries, to, to, value, limits, 0)?;
        Ok(())
    }

    /// Serialises the document with the default limits, dropping any entry the limits
    /// reject. [main-thread]
    ///
    /// Prefer [`StateDoc::try_to_bytes`] when the document came from somewhere the caller
    /// does not control.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode(&StateLimits::DEFAULT).0
    }

    /// Serialises the document, failing if any limit is exceeded or a key is invalid.
    /// [main-thread]
    pub fn try_to_bytes(&self) -> StateResult<Vec<u8>> {
        self.try_to_bytes_with_limits(&StateLimits::DEFAULT)
    }

    /// [`StateDoc::try_to_bytes`] with explicit limits. [main-thread]
    pub fn try_to_bytes_with_limits(&self, limits: &StateLimits) -> StateResult<Vec<u8>> {
        let (bytes, error) = self.encode(limits);
        error.map_or(Ok(bytes), Err)
    }

    fn encode(&self, limits: &StateLimits) -> (Vec<u8>, Option<StateError>) {
        let mut encoder = Encoder::new(self.version, *limits);
        encoder.put_entries(&self.entries);
        encoder.finish()
    }

    /// Parses a document with the default limits. [main-thread]
    pub fn from_bytes(bytes: &[u8]) -> StateResult<Self> {
        Self::from_bytes_with_limits(bytes, &StateLimits::DEFAULT)
    }

    /// Parses a document with explicit limits. [main-thread]
    ///
    /// Every length in `bytes` is checked against both the limits and the input actually
    /// present, so a hostile blob cannot cause an oversized allocation or a panic.
    pub fn from_bytes_with_limits(bytes: &[u8], limits: &StateLimits) -> StateResult<Self> {
        let decoded = codec::decode(bytes, limits)?;
        Ok(Self {
            version: decoded.schema_version,
            entries: decoded.entries,
        })
    }
}

/// Splits a path into `(parent, leaf)`; the parent is `None` for a top-level key.
fn split_parent(path: &str) -> StateResult<(Option<&str>, &str)> {
    if path.is_empty() {
        return Err(StateError::invalid_key(path, "a path may not be empty"));
    }
    match path.rsplit_once(format::PATH_SEPARATOR) {
        Some((parent, leaf)) => {
            if parent.is_empty() || leaf.is_empty() {
                Err(StateError::invalid_key(
                    path,
                    "path segments may not be empty",
                ))
            } else {
                Ok((Some(parent), leaf))
            }
        }
        None => Ok((None, path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StateDoc {
        let mut doc = StateDoc::new(StateVersion(1));
        doc.insert("gain", -6.0f64).expect("insert");
        doc.insert("bypass", false).expect("insert");
        doc.insert("filter/mode", "lowpass").expect("insert");
        doc.insert("filter/cutoff", 1000.0f64).expect("insert");
        doc.insert("filter/env/attack", 5i64).expect("insert");
        doc.insert("curve", Value::Bytes(vec![1, 2, 3]))
            .expect("insert");
        doc
    }

    #[test]
    fn typed_getters_read_nested_paths() {
        let doc = sample();
        assert_eq!(doc.f64("gain").expect("gain"), -6.0);
        assert!(!doc.bool("bypass").expect("bypass"));
        assert_eq!(doc.str("filter/mode").expect("mode"), "lowpass");
        assert_eq!(doc.f64("filter/cutoff").expect("cutoff"), 1000.0);
        assert_eq!(doc.i64("filter/env/attack").expect("attack"), 5);
        assert_eq!(doc.bytes("curve").expect("curve"), &[1, 2, 3]);
    }

    #[test]
    fn missing_paths_name_the_key() {
        let doc = sample();
        let err = doc.f64("filter/resonance").expect_err("absent");
        assert_eq!(err.kind(), &crate::StateErrorKind::MissingField);
        assert_eq!(err.key(), Some("filter/resonance"));
        assert!(err.to_string().contains("filter/resonance"));
        assert_eq!(doc.opt_f64("filter/resonance"), None);
        assert!(!doc.contains("nope"));
    }

    #[test]
    fn type_mismatches_name_the_key_and_both_types() {
        let doc = sample();
        let err = doc.i64("gain").expect_err("wrong type");
        assert_eq!(
            err.kind(),
            &crate::StateErrorKind::TypeMismatch {
                expected: ValueType::I64,
                found: ValueType::F64
            }
        );
        assert!(err.to_string().contains("gain"));
        assert_eq!(doc.opt_i64("gain"), None);
        // A group is not a scalar and vice versa.
        assert!(doc.f64("filter").is_err());
        assert!(doc.group("gain").is_err());
    }

    #[test]
    fn descending_through_a_scalar_is_a_miss_not_a_panic() {
        let doc = sample();
        assert_eq!(doc.opt_f64("gain/more"), None);
        assert!(doc.f64("gain/more").is_err());
    }

    #[test]
    fn empty_and_malformed_paths_are_rejected() {
        let mut doc = sample();
        assert!(doc.f64("").is_err());
        assert!(doc.f64("//x").is_err());
        assert!(doc.f64("filter//cutoff").is_err());
        assert_eq!(doc.opt_f64(""), None);
        assert!(doc.insert("", 1.0f64).is_err());
        assert!(doc.insert("a//b", 1.0f64).is_err());
        assert!(doc.rename("", "x").is_err());
        assert!(doc.rename("gain", "").is_err());
    }

    #[test]
    fn group_view_uses_relative_paths() {
        let doc = sample();
        let filter = doc.group("filter").expect("group");
        assert_eq!(filter.str("mode").expect("mode"), "lowpass");
        assert_eq!(filter.i64("env/attack").expect("attack"), 5);
        assert_eq!(filter.len(), 3);
        assert!(!filter.is_empty());
        assert_eq!(
            filter.keys().collect::<Vec<_>>(),
            vec!["mode", "cutoff", "env"]
        );
        assert!(filter.contains("env"));
        assert!(doc.opt_group("filter").is_some());
        assert!(doc.opt_group("gain").is_none());
        assert!(doc.opt_group("nope").is_none());
        let env = filter.group("env").expect("nested group");
        assert_eq!(env.i64("attack").expect("attack"), 5);
    }

    #[test]
    fn insert_creates_intermediate_groups_and_preserves_order() {
        let mut doc = StateDoc::new(StateVersion(1));
        doc.insert("a/b/c", 1i64).expect("insert");
        doc.insert("a/b/d", 2i64).expect("insert");
        doc.insert("z", 3i64).expect("insert");
        assert_eq!(doc.keys().collect::<Vec<_>>(), vec!["a", "z"]);
        let b = doc.group("a/b").expect("group");
        assert_eq!(b.keys().collect::<Vec<_>>(), vec!["c", "d"]);
    }

    #[test]
    fn insert_replaces_in_place_and_returns_the_old_value() {
        let mut doc = sample();
        let old = doc.insert("gain", 0.0f64).expect("insert");
        assert_eq!(old, Some(Value::F64(-6.0)));
        assert_eq!(doc.f64("gain").expect("gain"), 0.0);
        // Order is unchanged by a replacement.
        assert_eq!(
            doc.keys().collect::<Vec<_>>(),
            vec!["gain", "bypass", "filter", "curve"]
        );
    }

    #[test]
    fn insert_through_a_scalar_is_a_type_mismatch() {
        let mut doc = sample();
        let err = doc
            .insert("gain/more", 1i64)
            .expect_err("scalar in the way");
        assert!(matches!(
            err.kind(),
            crate::StateErrorKind::TypeMismatch { .. }
        ));
    }

    #[test]
    fn insert_respects_the_depth_limit() {
        let mut doc = StateDoc::new(StateVersion(1));
        let limits = StateLimits::default().with_max_depth(2);
        assert!(doc.insert_with_limits("a/b", 1i64, &limits).is_ok());
        let err = doc
            .insert_with_limits("a/b2/c", 1i64, &limits)
            .expect_err("too deep");
        assert_eq!(err.kind(), &crate::StateErrorKind::LimitExceeded);
    }

    #[test]
    fn insert_rejects_a_segment_containing_the_separator_by_splitting_it() {
        let mut doc = StateDoc::new(StateVersion(1));
        // "a/b" is a path, not a key; the leaf key itself can never contain '/'.
        doc.insert("a/b", 1i64).expect("insert");
        assert!(doc.group("a").is_ok());
        let long = "k".repeat(5000);
        assert!(doc.insert(&long, 1i64).is_err());
    }

    #[test]
    fn remove_returns_the_value_and_closes_the_gap() {
        let mut doc = sample();
        assert_eq!(doc.remove("bypass"), Some(Value::Bool(false)));
        assert_eq!(doc.remove("bypass"), None);
        assert_eq!(
            doc.keys().collect::<Vec<_>>(),
            vec!["gain", "filter", "curve"]
        );
        assert_eq!(doc.remove("filter/cutoff"), Some(Value::F64(1000.0)));
        assert_eq!(doc.remove("filter/nope"), None);
        assert_eq!(doc.remove("gain/nope"), None);
        assert!(doc.remove("filter").is_some());
        assert!(!doc.contains("filter/mode"));
    }

    #[test]
    fn rename_within_a_parent_keeps_position() {
        let mut doc = sample();
        doc.rename("gain", "output_gain").expect("rename");
        assert_eq!(
            doc.keys().collect::<Vec<_>>(),
            vec!["output_gain", "bypass", "filter", "curve"]
        );
        assert_eq!(doc.f64("output_gain").expect("renamed"), -6.0);

        doc.rename("filter/mode", "filter/kind").expect("rename");
        let filter = doc.group("filter").expect("group");
        assert_eq!(
            filter.keys().collect::<Vec<_>>(),
            vec!["kind", "cutoff", "env"]
        );
    }

    #[test]
    fn rename_across_parents_moves_the_value() {
        let mut doc = sample();
        doc.rename("gain", "filter/gain").expect("rename");
        assert!(!doc.contains("gain"));
        assert_eq!(doc.f64("filter/gain").expect("moved"), -6.0);

        doc.rename("filter/env/attack", "attack").expect("rename");
        assert_eq!(doc.i64("attack").expect("moved"), 5);
    }

    #[test]
    fn rename_onto_an_existing_key_overwrites_it_in_place() {
        let mut doc = sample();
        doc.rename("gain", "bypass").expect("rename");
        assert_eq!(
            doc.keys().collect::<Vec<_>>(),
            vec!["bypass", "filter", "curve"]
        );
        assert_eq!(doc.f64("bypass").expect("overwritten"), -6.0);
    }

    #[test]
    fn rename_to_the_same_name_is_a_no_op() {
        let mut doc = sample();
        doc.rename("gain", "gain").expect("rename");
        assert_eq!(doc.f64("gain").expect("gain"), -6.0);
        assert_eq!(doc.len(), 4);
    }

    #[test]
    fn rename_of_a_missing_key_fails() {
        let mut doc = sample();
        let err = doc.rename("nope", "other").expect_err("missing");
        assert_eq!(err.kind(), &crate::StateErrorKind::MissingField);
        let err = doc
            .rename("filter/nope", "filter/other")
            .expect_err("missing");
        assert_eq!(err.kind(), &crate::StateErrorKind::MissingField);
        let err = doc.rename("nope/deep", "other").expect_err("missing");
        assert_eq!(err.kind(), &crate::StateErrorKind::MissingField);
    }

    #[test]
    fn get_mut_allows_in_place_edits() {
        let mut doc = sample();
        if let Some(Value::F64(v)) = doc.get_mut("filter/cutoff") {
            *v = 200.0;
        }
        assert_eq!(doc.f64("filter/cutoff").expect("cutoff"), 200.0);
        assert!(doc.get_mut("nope").is_none());
        assert!(doc.get_mut("gain/nope").is_none());
        doc.entries_mut().push(StateEntry::new("extra", 1i64));
        assert_eq!(doc.i64("extra").expect("extra"), 1);
    }

    #[test]
    fn round_trips_through_bytes_including_nested_groups() {
        let doc = sample();
        let bytes = doc.try_to_bytes().expect("encodes");
        let back = StateDoc::from_bytes(&bytes).expect("decodes");
        assert_eq!(back, doc);
        assert_eq!(back.version(), StateVersion(1));
        assert_eq!(back.try_to_bytes().expect("re-encodes"), bytes);
    }

    #[test]
    fn encoding_is_deterministic() {
        let a = sample().to_bytes();
        let b = sample().to_bytes();
        assert_eq!(a, b);
        // A different insertion order is a different document, on purpose.
        let mut reordered = StateDoc::new(StateVersion(1));
        reordered.insert("bypass", false).expect("insert");
        reordered.insert("gain", -6.0f64).expect("insert");
        assert_ne!(reordered.to_bytes(), a);
    }

    #[test]
    fn empty_document_round_trips() {
        let doc = StateDoc::new(StateVersion(4));
        assert!(doc.is_empty());
        assert_eq!(doc.len(), 0);
        let bytes = doc.try_to_bytes().expect("encodes");
        let back = StateDoc::from_bytes(&bytes).expect("decodes");
        assert_eq!(back, doc);
        assert_eq!(back.version(), StateVersion(4));
    }

    #[test]
    fn empty_group_round_trips() {
        let mut doc = StateDoc::new(StateVersion(1));
        doc.entries_mut()
            .push(StateEntry::new("empty", Value::Group(Vec::new())));
        let bytes = doc.try_to_bytes().expect("encodes");
        let back = StateDoc::from_bytes(&bytes).expect("decodes");
        assert_eq!(back, doc);
        assert!(back.group("empty").expect("group").is_empty());
    }

    #[test]
    fn try_to_bytes_reports_an_invalid_key_instead_of_dropping_it() {
        let mut doc = StateDoc::new(StateVersion(1));
        doc.entries_mut().push(StateEntry::new("", 1i64));
        let err = doc.try_to_bytes().expect_err("invalid key");
        assert_eq!(err.kind(), &crate::StateErrorKind::InvalidKey);
    }

    #[test]
    fn version_can_be_changed() {
        let mut doc = StateDoc::new(StateVersion(1));
        doc.set_version(StateVersion(3));
        assert_eq!(doc.version(), StateVersion(3));
        let bytes = doc.to_bytes();
        assert_eq!(
            StateDoc::from_bytes(&bytes).expect("decodes").version(),
            StateVersion(3)
        );
    }

    #[test]
    fn from_entries_preserves_input() {
        let doc = StateDoc::from_entries(
            StateVersion(2),
            vec![StateEntry::new("a", 1i64), StateEntry::new("b", "x")],
        );
        assert_eq!(doc.len(), 2);
        assert_eq!(doc.version(), StateVersion(2));
        assert_eq!(doc.str("b").expect("b"), "x");
    }

    #[test]
    fn duplicate_keys_resolve_to_the_first_occurrence() {
        let doc = StateDoc::from_entries(
            StateVersion(1),
            vec![StateEntry::new("k", 1i64), StateEntry::new("k", 2i64)],
        );
        assert_eq!(doc.i64("k").expect("k"), 1);
        // …and both survive a round trip, because order is the format's contract.
        let back = StateDoc::from_bytes(&doc.to_bytes()).expect("decodes");
        assert_eq!(back.len(), 2);
    }
}
