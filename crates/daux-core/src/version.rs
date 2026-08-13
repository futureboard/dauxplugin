//! Four-component plug-in version.

use core::fmt;
use core::str::FromStr;

use crate::{DauxError, DauxResult, ErrorKind};

/// A plug-in version. `[any-thread]`
///
/// Mirrors `DauxVersion` (`docs/specifications/abi-v1.md` §2), whose ordering is
/// lexicographic over `(major, minor, patch, build)` — which is exactly what
/// `#[derive(Ord)]` produces for fields in that order.
///
/// `build` is the release-engineering component: CI run number, revision count,
/// anything monotonic. It is `0` when unused and then disappears from
/// [`Display`](fmt::Display).
///
/// ```
/// use daux_core::Version;
///
/// let v = Version::new(1, 4, 2);
/// assert_eq!(v.to_string(), "1.4.2");
/// assert_eq!(v.with_build(77).to_string(), "1.4.2.77");
/// assert!(Version::new(1, 4, 2) < Version::new(1, 10, 0));
/// assert_eq!("2.0.1".parse::<Version>().unwrap(), Version::new(2, 0, 1));
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Breaking changes.
    pub major: u32,
    /// Backwards-compatible additions.
    pub minor: u32,
    /// Fixes.
    pub patch: u32,
    /// Build metadata; `0` when unused.
    pub build: u32,
}

impl Version {
    /// `0.0.0`, the version of something that has not been released.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// `1.0.0`.
    pub const ONE: Self = Self::new(1, 0, 0);

    /// Builds a version with no build component. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            build: 0,
        }
    }

    /// Returns this version with `build` attached. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn with_build(mut self, build: u32) -> Self {
        self.build = build;
        self
    }

    /// Parses `major.minor.patch` or `major.minor.patch.build`.
    /// `[main-thread]`
    ///
    /// Deliberately strict: no `v` prefix, no pre-release or metadata suffix, no
    /// missing components, no leading `+`/`-`. A version string that reaches
    /// here comes from a manifest or a `Cargo.toml`, where being wrong quietly
    /// is worse than failing loudly.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidArgument`] when the string is not exactly three or
    /// four dot-separated `u32`s.
    pub fn parse(text: &str) -> DauxResult<Self> {
        fn bad(text: &str) -> DauxError {
            DauxError::new(
                ErrorKind::InvalidArgument,
                format!("`{text}` is not a version: expected `major.minor.patch[.build]`"),
            )
        }

        /// `u32::from_str` accepts a leading `+`, and `str::trim` is not applied by
        /// `split`, so `"+1.2.3"` would otherwise parse as `1.2.3`. A version component is
        /// digits and nothing else.
        fn component(text: &str, part: &str) -> DauxResult<u32> {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad(text));
            }
            part.parse::<u32>().map_err(|_| bad(text))
        }

        let mut parts = text.split('.');
        let mut next = || -> DauxResult<u32> {
            let part = parts.next().ok_or_else(|| bad(text))?;
            component(text, part)
        };
        let major = next()?;
        let minor = next()?;
        let patch = next()?;
        let build = match parts.next() {
            Some(b) => component(text, b)?,
            None => 0,
        };
        if parts.next().is_some() {
            return Err(bad(text));
        }
        Ok(Self {
            major,
            minor,
            patch,
            build,
        })
    }

    /// The four components in ABI order. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn to_parts(self) -> (u32, u32, u32, u32) {
        (self.major, self.minor, self.patch, self.build)
    }

    /// Builds a version from the four ABI components. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn from_parts(parts: (u32, u32, u32, u32)) -> Self {
        Self {
            major: parts.0,
            minor: parts.1,
            patch: parts.2,
            build: parts.3,
        }
    }

    /// `true` when `self` and `other` share a major component, i.e. when they
    /// are meant to be interchangeable. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if self.build != 0 {
            write!(f, ".{}", self.build)?;
        }
        Ok(())
    }
}

impl FromStr for Version {
    type Err = DauxError;

    fn from_str(s: &str) -> DauxResult<Self> {
        Self::parse(s)
    }
}

impl From<(u32, u32, u32)> for Version {
    fn from((major, minor, patch): (u32, u32, u32)) -> Self {
        Self::new(major, minor, patch)
    }
}

impl From<(u32, u32, u32, u32)> for Version {
    fn from(parts: (u32, u32, u32, u32)) -> Self {
        Self::from_parts(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_and_defaults() {
        let v = Version::new(1, 2, 3);
        assert_eq!(v.to_parts(), (1, 2, 3, 0));
        assert_eq!(Version::from_parts((1, 2, 3, 0)), v);
        assert_eq!(Version::from((1, 2, 3)), v);
        assert_eq!(Version::from((1, 2, 3, 4)), v.with_build(4));
        assert_eq!(Version::default(), Version::ZERO);
        assert_eq!(Version::ONE, Version::new(1, 0, 0));
    }

    #[test]
    fn ordering_is_lexicographic_over_all_four_components() {
        assert!(Version::new(0, 9, 9) < Version::new(1, 0, 0));
        assert!(Version::new(1, 2, 3) < Version::new(1, 2, 4));
        assert!(Version::new(1, 2, 3) < Version::new(1, 3, 0));
        assert!(Version::new(1, 2, 3) < Version::new(1, 2, 3).with_build(1));
        // Not string ordering: 10 sorts after 9.
        assert!(Version::new(1, 9, 0) < Version::new(1, 10, 0));
        assert_eq!(
            Version::new(1, 2, 3).cmp(&Version::new(1, 2, 3)),
            core::cmp::Ordering::Equal
        );
    }

    #[test]
    fn display_hides_a_zero_build() {
        assert_eq!(Version::new(1, 0, 0).to_string(), "1.0.0");
        assert_eq!(Version::new(1, 0, 0).with_build(0).to_string(), "1.0.0");
        assert_eq!(Version::new(1, 0, 0).with_build(9).to_string(), "1.0.0.9");
        assert_eq!(
            Version::from_parts((u32::MAX, u32::MAX, u32::MAX, u32::MAX)).to_string(),
            "4294967295.4294967295.4294967295.4294967295"
        );
    }

    #[test]
    fn parsing_accepts_three_or_four_components() {
        assert_eq!(Version::parse("0.0.0").unwrap(), Version::ZERO);
        assert_eq!(Version::parse("1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(
            Version::parse("1.2.3.4").unwrap(),
            Version::new(1, 2, 3).with_build(4)
        );
        assert_eq!(
            Version::parse("4294967295.0.0").unwrap().major,
            u32::MAX,
            "the full u32 range parses"
        );
    }

    #[test]
    fn parsing_rejects_everything_else() {
        for text in [
            "",
            "1",
            "1.2",
            "1.2.3.4.5",
            "v1.2.3",
            "1.2.3-beta",
            "1.2.3+meta",
            " 1.2.3",
            "1.2.3 ",
            "1..3",
            "1.2.",
            ".1.2",
            "-1.2.3",
            "+1.2.3",
            "1.2.x",
            "4294967296.0.0",
            "1.2.3.4.",
            "one.two.three",
        ] {
            let err = Version::parse(text).expect_err(text);
            assert_eq!(err.kind(), ErrorKind::InvalidArgument, "`{text}`");
            assert!(err.message().contains("major.minor.patch"));
        }
    }

    #[test]
    fn display_and_parse_round_trip() {
        for v in [
            Version::ZERO,
            Version::new(1, 2, 3),
            Version::new(1, 2, 3).with_build(4),
            Version::from_parts((u32::MAX, 0, 7, 1)),
        ] {
            assert_eq!(Version::parse(&v.to_string()).unwrap(), v);
            assert_eq!(v.to_string().parse::<Version>().unwrap(), v);
        }
    }

    #[test]
    fn compatibility_is_major_only() {
        assert!(Version::new(1, 0, 0).is_compatible_with(Version::new(1, 9, 9)));
        assert!(!Version::new(1, 0, 0).is_compatible_with(Version::new(2, 0, 0)));
        assert!(Version::ZERO.is_compatible_with(Version::new(0, 1, 0)));
    }
}
