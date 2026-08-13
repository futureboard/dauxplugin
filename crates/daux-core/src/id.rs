//! The permanent identity of a plug-in.

use core::fmt;
use core::str::FromStr;

use crate::{DauxError, DauxResult, ErrorKind};

/// A validated, permanent plug-in identifier. `[main-thread]`
///
/// # The format is normative
///
/// `docs/specifications/manifest-v1.md` §3.4 defines it, and `daux-cli`
/// validates bundles against exactly this grammar:
///
/// ```text
/// id      ::= label ( "." label )+
/// label   ::= alnum *( alnum / "-" / "_" )
/// alnum   ::= lower-case ASCII letter or digit
/// ```
///
/// * 1..=[`PluginId::MAX_BYTES`] bytes, so it always fits `DauxId[128]` with
///   room for the NUL padding ABI v1 §2.1 expects;
/// * at least one `.`, and at least one ASCII letter overall;
/// * no leading or trailing `.`, no `..`, no empty label;
/// * every label starts with a lower-case letter or a digit and continues with
///   those, `-` or `_`;
/// * **lower-case only**. Ids are compared byte for byte, so a differing case
///   would be a different plug-in — and case-insensitive filesystems and
///   case-sensitive registries would then disagree about identity. Rejecting
///   upper case here is what stops that from ever happening.
///
/// Reverse-DNS is required by convention: use a domain you own.
///
/// # It is permanent
///
/// abi-v1 §14: changing the id creates a *different* plug-in and silently breaks
/// every saved project that referenced the old one. Renaming the product is
/// free; renaming the id is not.
///
/// ```
/// use daux_core::PluginId;
///
/// let id = PluginId::new("studio.futureboard.equzx").unwrap();
/// assert_eq!(id.as_str(), "studio.futureboard.equzx");
/// assert_eq!(id.namespace(), "studio.futureboard");
/// assert_eq!(id.label(), "equzx");
///
/// // The rules bite.
/// assert!(PluginId::new("Gain").is_err());                    // no dot, upper case
/// assert!(PluginId::new("studio..gain").is_err());            // empty label
/// assert!(PluginId::new("studio.Futureboard.gain").is_err()); // upper case
/// assert!(PluginId::new("studio.-gain").is_err());            // label starts with '-'
/// ```
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginId(String);

impl PluginId {
    /// Longest id the ABI can carry: `DauxId` is 128 bytes and the value is
    /// NUL-padded.
    pub const MAX_BYTES: usize = 127;

    /// Validates `id` and takes ownership of it. `[main-thread]`
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidArgument`] with a message naming the specific rule
    /// that was broken.
    pub fn new(id: impl Into<String>) -> DauxResult<Self> {
        let id = id.into();
        Self::validate(&id)?;
        Ok(Self(id))
    }

    /// Checks `id` against the grammar without building one. `[main-thread]`
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidArgument`], with a message that names the rule and
    /// the offending label, so `daux build` can print something actionable.
    pub fn validate(id: &str) -> DauxResult<()> {
        fn bad(message: impl Into<String>) -> DauxError {
            DauxError::new(ErrorKind::InvalidArgument, message)
        }

        if id.is_empty() {
            return Err(bad("plug-in id is empty"));
        }
        if id.len() > Self::MAX_BYTES {
            return Err(bad(format!(
                "plug-in id is {} bytes, the limit is {}",
                id.len(),
                Self::MAX_BYTES
            )));
        }
        if !id.contains('.') {
            return Err(bad(format!(
                "plug-in id `{id}` has no `.`; reverse-DNS needs at least two labels"
            )));
        }

        for label in id.split('.') {
            if label.is_empty() {
                return Err(bad(format!(
                    "plug-in id `{id}` has an empty label: no leading or trailing `.`, and no `..`"
                )));
            }
            let mut bytes = label.bytes();
            // `split` never yields an empty label here, so the first byte exists.
            let first = bytes.next().unwrap_or(b'\0');
            if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
                return Err(bad(format!(
                    "plug-in id label `{label}` must start with a lower-case ASCII letter or a digit"
                )));
            }
            for byte in bytes {
                if !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && byte != b'-' && byte != b'_'
                {
                    return Err(bad(format!(
                        "plug-in id label `{label}` contains `{}`; only lower-case ASCII letters, \
                         digits, `-` and `_` are allowed",
                        char::from(byte).escape_debug()
                    )));
                }
            }
        }

        if !id.bytes().any(|b| b.is_ascii_lowercase()) {
            return Err(bad(format!(
                "plug-in id `{id}` contains no ASCII letter"
            )));
        }

        Ok(())
    }

    /// `true` when `id` satisfies the grammar. `[main-thread]`
    #[must_use]
    pub fn is_valid(id: &str) -> bool {
        Self::validate(id).is_ok()
    }

    /// The id as text. `[any-thread]`
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Length in bytes, always `1..=127`. `[any-thread]`
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always `false`; present because a `len` without an `is_empty` is a trap.
    /// `[any-thread]`
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The labels, left to right. Always at least two. `[main-thread]`
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }

    /// Everything before the final label — the vendor's namespace.
    /// `[main-thread]`
    ///
    /// `"studio.futureboard.equzx"` → `"studio.futureboard"`.
    #[must_use]
    pub fn namespace(&self) -> &str {
        match self.0.rfind('.') {
            Some(dot) => &self.0[..dot],
            // Unreachable for a validated id, which always contains a `.`.
            None => "",
        }
    }

    /// The final label — the plug-in's own name within the namespace.
    /// `[main-thread]`
    ///
    /// `"studio.futureboard.equzx"` → `"equzx"`.
    #[must_use]
    pub fn label(&self) -> &str {
        match self.0.rfind('.') {
            Some(dot) => &self.0[dot + 1..],
            None => &self.0,
        }
    }

    /// Unwraps the owned string. `[main-thread]`
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PluginId({:?})", self.0)
    }
}

impl AsRef<str> for PluginId {
    #[inline]
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for PluginId {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for PluginId {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<PluginId> for str {
    #[inline]
    fn eq(&self, other: &PluginId) -> bool {
        self == other.0
    }
}

impl FromStr for PluginId {
    type Err = DauxError;

    fn from_str(s: &str) -> DauxResult<Self> {
        Self::new(s)
    }
}

impl TryFrom<&str> for PluginId {
    type Error = DauxError;

    fn try_from(s: &str) -> DauxResult<Self> {
        Self::new(s)
    }
}

impl TryFrom<String> for PluginId {
    type Error = DauxError;

    fn try_from(s: String) -> DauxResult<Self> {
        Self::new(s)
    }
}

impl From<PluginId> for String {
    fn from(id: PluginId) -> Self {
        id.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_id_survives_and_splits() {
        let id = PluginId::new("studio.futureboard.equzx").expect("valid");
        assert_eq!(id.as_str(), "studio.futureboard.equzx");
        assert_eq!(id.len(), 24);
        assert!(!id.is_empty());
        assert_eq!(id.namespace(), "studio.futureboard");
        assert_eq!(id.label(), "equzx");
        assert_eq!(
            id.labels().collect::<Vec<_>>(),
            ["studio", "futureboard", "equzx"]
        );
        assert_eq!(id.to_string(), "studio.futureboard.equzx");
        assert_eq!(format!("{id:?}"), "PluginId(\"studio.futureboard.equzx\")");
        assert_eq!(id.as_ref(), "studio.futureboard.equzx");
        assert_eq!(id, *"studio.futureboard.equzx");
        assert_eq!(id, "studio.futureboard.equzx");
        assert_eq!(String::from(id.clone()), "studio.futureboard.equzx");
        assert_eq!(id.into_string(), "studio.futureboard.equzx");
    }

    #[test]
    fn every_accepted_shape_from_the_specification() {
        for id in [
            "a.b",
            "com.example.plug",
            "studio.futureboard.gain2",
            "studio.futureboard.multi-band_eq",
            "org.x.y.z.deeply.nested.name",
            "x0.y1",
            "1a.2b",
        ] {
            assert!(PluginId::is_valid(id), "`{id}` should be valid");
        }
    }

    #[test]
    fn a_maximum_length_id_is_accepted_and_one_more_byte_is_not() {
        let long = format!("studio.{}", "a".repeat(PluginId::MAX_BYTES - 7));
        assert_eq!(long.len(), PluginId::MAX_BYTES);
        assert!(PluginId::new(&long).is_ok());

        let too_long = format!("{long}a");
        let err = PluginId::new(&too_long).expect_err("one byte over");
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(err.message().contains("128"), "{}", err.message());
    }

    #[test]
    fn structural_violations_are_rejected() {
        for (id, needle) in [
            ("", "empty"),
            ("gain", "no `.`"),
            (".gain", "empty label"),
            ("gain.", "empty label"),
            ("studio..gain", "empty label"),
            ("...", "empty label"),
            (".", "empty label"),
        ] {
            let err = PluginId::new(id).expect_err(id);
            assert_eq!(err.kind(), ErrorKind::InvalidArgument);
            assert!(
                err.message().contains(needle),
                "`{id}` reported `{}`, expected it to mention `{needle}`",
                err.message()
            );
        }
    }

    #[test]
    fn character_violations_are_rejected() {
        for id in [
            "Studio.gain",         // upper case in the first label
            "studio.Gain",         // upper case in the last label
            "studio.gAin",         // upper case in the middle
            "studio.-gain",        // label starts with `-`
            "studio._gain",        // label starts with `_`
            "studio.gain plugin",  // space
            "studio.gain/plugin",  // slash
            "studio.gain\\plugin", // backslash
            "studio.gain\0",       // NUL
            "studio.gäin",         // non-ASCII
            "studio.gain\u{202e}", // right-to-left override
            "studio.gain:1",       // colon
            "studio.gain+",        // plus
            "studio.gain%20",      // percent
        ] {
            let err = PluginId::new(id).expect_err(id);
            assert_eq!(err.kind(), ErrorKind::InvalidArgument, "`{id}`");
            assert!(!err.message().is_empty());
        }
    }

    #[test]
    fn an_id_without_a_letter_is_rejected() {
        // Digits alone satisfy the label grammar but make a meaningless id, and
        // the specification requires at least one ASCII letter.
        let err = PluginId::new("1.2").expect_err("no letters");
        assert!(err.message().contains("no ASCII letter"), "{}", err.message());
        assert!(PluginId::new("1.2a").is_ok());
        assert!(PluginId::new("1-2_3.4").is_err());
    }

    #[test]
    fn parsing_goes_through_the_same_validation() {
        assert!("studio.futureboard.gain".parse::<PluginId>().is_ok());
        assert!("Studio.Gain".parse::<PluginId>().is_err());
        assert!(PluginId::try_from("studio.gain").is_ok());
        assert!(PluginId::try_from("studio.gain".to_owned()).is_ok());
        assert!(PluginId::try_from("nope").is_err());
    }

    #[test]
    fn ids_order_and_hash_by_their_bytes() {
        let a = PluginId::new("studio.a").expect("valid");
        let b = PluginId::new("studio.b").expect("valid");
        assert!(a < b);

        let mut set = std::collections::HashSet::new();
        assert!(set.insert(a.clone()));
        assert!(!set.insert(a));
        assert!(set.insert(b));
    }

    #[test]
    fn the_id_type_crosses_threads() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PluginId>();
    }
}
