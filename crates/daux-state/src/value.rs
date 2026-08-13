//! The value model shared by the writer, the reader and migrations.

use core::fmt;

use crate::format;

/// Discriminant of a [`Value`], used in error messages and as the on-disk type tag.
/// [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ValueType {
    /// IEEE-754 binary64.
    F64,
    /// Signed 64-bit integer.
    I64,
    /// Boolean.
    Bool,
    /// UTF-8 string.
    Str,
    /// Opaque byte string.
    Bytes,
    /// Nested group of entries.
    Group,
}

impl ValueType {
    /// The on-disk type tag from [`crate::format`]. [any-thread]
    #[inline]
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::F64 => format::TAG_F64,
            Self::I64 => format::TAG_I64,
            Self::Bool => format::TAG_BOOL,
            Self::Str => format::TAG_STR,
            Self::Bytes => format::TAG_BYTES,
            Self::Group => format::TAG_GROUP_BEGIN,
        }
    }

    /// The type a tag denotes, or `None` for [`format::TAG_GROUP_END`] (which is
    /// structure, not a value) and for unknown tags. [any-thread]
    #[inline]
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            format::TAG_F64 => Some(Self::F64),
            format::TAG_I64 => Some(Self::I64),
            format::TAG_BOOL => Some(Self::Bool),
            format::TAG_STR => Some(Self::Str),
            format::TAG_BYTES => Some(Self::Bytes),
            format::TAG_GROUP_BEGIN => Some(Self::Group),
            _ => None,
        }
    }

    /// Lower-case name used in error messages. [any-thread]
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::F64 => "f64",
            Self::I64 => "i64",
            Self::Bool => "bool",
            Self::Str => "str",
            Self::Bytes => "bytes",
            Self::Group => "group",
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A value stored in a [`StateDoc`](crate::StateDoc). [main-thread]
///
/// State handling is main-thread work by definition (abi-v1 §11.3), so the owned
/// `String` / `Vec` payloads here are not a real-time concern.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Value {
    /// A floating-point number. `NaN` and infinities round-trip bit-exactly.
    F64(f64),
    /// A signed integer.
    I64(i64),
    /// A boolean.
    Bool(bool),
    /// A UTF-8 string, possibly empty.
    Str(String),
    /// An opaque byte string, possibly empty.
    Bytes(Vec<u8>),
    /// A nested group, in insertion order.
    Group(Vec<StateEntry>),
}

impl Value {
    /// The discriminant of this value. [main-thread]
    #[inline]
    #[must_use]
    pub const fn value_type(&self) -> ValueType {
        match self {
            Self::F64(_) => ValueType::F64,
            Self::I64(_) => ValueType::I64,
            Self::Bool(_) => ValueType::Bool,
            Self::Str(_) => ValueType::Str,
            Self::Bytes(_) => ValueType::Bytes,
            Self::Group(_) => ValueType::Group,
        }
    }

    /// The number, or `None` if this is not a [`Value::F64`]. No numeric coercion: a value
    /// written as an integer stays an integer. [main-thread]
    #[inline]
    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            Self::F64(v) => Some(*v),
            _ => None,
        }
    }

    /// The integer, or `None` if this is not a [`Value::I64`]. [main-thread]
    #[inline]
    #[must_use]
    pub const fn as_i64(&self) -> Option<i64> {
        match self {
            Self::I64(v) => Some(*v),
            _ => None,
        }
    }

    /// The boolean, or `None` if this is not a [`Value::Bool`]. [main-thread]
    #[inline]
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// The string, or `None` if this is not a [`Value::Str`]. [main-thread]
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(v) => Some(v.as_str()),
            _ => None,
        }
    }

    /// The bytes, or `None` if this is not a [`Value::Bytes`]. [main-thread]
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// The child entries, or `None` if this is not a [`Value::Group`]. [main-thread]
    #[inline]
    #[must_use]
    pub fn as_group(&self) -> Option<&[StateEntry]> {
        match self {
            Self::Group(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// The child entries for mutation, or `None` if this is not a [`Value::Group`].
    /// [main-thread]
    #[inline]
    #[must_use]
    pub const fn as_group_mut(&mut self) -> Option<&mut Vec<StateEntry>> {
        match self {
            Self::Group(v) => Some(v),
            _ => None,
        }
    }

    /// `true` when this is a [`Value::Group`]. [main-thread]
    #[inline]
    #[must_use]
    pub const fn is_group(&self) -> bool {
        matches!(self, Self::Group(_))
    }
}

impl From<f64> for Value {
    #[inline]
    fn from(v: f64) -> Self {
        Self::F64(v)
    }
}

impl From<i64> for Value {
    #[inline]
    fn from(v: i64) -> Self {
        Self::I64(v)
    }
}

impl From<bool> for Value {
    #[inline]
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<String> for Value {
    #[inline]
    fn from(v: String) -> Self {
        Self::Str(v)
    }
}

impl From<&str> for Value {
    #[inline]
    fn from(v: &str) -> Self {
        Self::Str(v.to_owned())
    }
}

impl From<Vec<u8>> for Value {
    #[inline]
    fn from(v: Vec<u8>) -> Self {
        Self::Bytes(v)
    }
}

impl From<&[u8]> for Value {
    #[inline]
    fn from(v: &[u8]) -> Self {
        Self::Bytes(v.to_vec())
    }
}

/// One `key → value` pair, in the position it was written. [main-thread]
///
/// Documents and groups are ordered sequences of these, never maps: order is part of the
/// format's determinism guarantee.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StateEntry {
    /// The key, relative to the enclosing group. Never empty, never contains
    /// [`format::PATH_SEPARATOR`].
    pub key: String,
    /// The value.
    pub value: Value,
}

impl StateEntry {
    /// Builds an entry. The key is not validated here; [`StateWriter`](crate::StateWriter)
    /// and [`StateDoc::insert`](crate::StateDoc::insert) do that at the point where a
    /// useful error can be produced. [main-thread]
    #[inline]
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<Value>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_round_trip() {
        for ty in [
            ValueType::F64,
            ValueType::I64,
            ValueType::Bool,
            ValueType::Str,
            ValueType::Bytes,
            ValueType::Group,
        ] {
            assert_eq!(ValueType::from_tag(ty.tag()), Some(ty));
        }
        assert_eq!(ValueType::from_tag(format::TAG_GROUP_END), None);
        assert_eq!(ValueType::from_tag(0), None);
        assert_eq!(ValueType::from_tag(200), None);
    }

    #[test]
    fn tag_numbers_are_stable() {
        assert_eq!(ValueType::F64.tag(), 1);
        assert_eq!(ValueType::I64.tag(), 2);
        assert_eq!(ValueType::Bool.tag(), 3);
        assert_eq!(ValueType::Str.tag(), 4);
        assert_eq!(ValueType::Bytes.tag(), 5);
        assert_eq!(ValueType::Group.tag(), 6);
        assert_eq!(format::TAG_GROUP_END, 7);
    }

    #[test]
    fn accessors_do_not_coerce_between_types() {
        let int = Value::I64(3);
        assert_eq!(int.as_i64(), Some(3));
        assert_eq!(int.as_f64(), None);
        assert_eq!(int.as_bool(), None);
        assert_eq!(int.as_str(), None);
        assert_eq!(int.as_bytes(), None);
        assert_eq!(int.as_group(), None);
        assert!(!int.is_group());

        let float = Value::F64(3.0);
        assert_eq!(float.as_f64(), Some(3.0));
        assert_eq!(float.as_i64(), None);
    }

    #[test]
    fn group_accessors() {
        let mut group = Value::Group(vec![StateEntry::new("a", 1.0f64)]);
        assert!(group.is_group());
        assert_eq!(group.as_group().map(<[StateEntry]>::len), Some(1));
        group
            .as_group_mut()
            .expect("is a group")
            .push(StateEntry::new("b", 2i64));
        assert_eq!(group.as_group().map(<[StateEntry]>::len), Some(2));
        assert_eq!(Value::Bool(true).as_group_mut(), None);
    }

    #[test]
    fn value_type_names_are_used_in_messages() {
        assert_eq!(ValueType::Bytes.to_string(), "bytes");
        assert_eq!(Value::Str(String::new()).value_type(), ValueType::Str);
    }

    #[test]
    fn from_conversions() {
        assert_eq!(Value::from(1.5f64), Value::F64(1.5));
        assert_eq!(Value::from(-2i64), Value::I64(-2));
        assert_eq!(Value::from(true), Value::Bool(true));
        assert_eq!(Value::from("hi"), Value::Str("hi".to_owned()));
        assert_eq!(Value::from(String::from("hi")), Value::Str("hi".to_owned()));
        assert_eq!(Value::from(vec![1u8, 2]), Value::Bytes(vec![1, 2]));
        assert_eq!(Value::from(&[1u8, 2][..]), Value::Bytes(vec![1, 2]));
    }
}
