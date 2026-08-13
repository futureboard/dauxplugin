//! Reading an OpenGL version string, and picking a GLSL version from it.
//!
//! Drivers report `GL_VERSION` in a format the specification only half-pins down: the leading
//! `major.minor[.release]` is required, everything after it is vendor text, and OpenGL ES
//! prefixes the whole thing with `OpenGL ES `. In practice that means strings like
//! `4.6.0 NVIDIA 566.36`, `3.3.0 - Build 31.0.101.5186`, `OpenGL ES 3.2 Mesa 23.2.1` and
//! `4.1 Metal - 89.3` all have to parse.
//!
//! Getting this wrong is not a cosmetic problem: the GLSL version a shader declares must match
//! what the context supports, and a mismatch is a link failure at editor-open time on
//! precisely the machines the OpenGL path exists to serve.

/// `[any-thread]` An OpenGL or OpenGL ES version.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct GlVersion {
    /// Major version number.
    pub major: u32,
    /// Minor version number.
    pub minor: u32,
    /// `true` for OpenGL ES, `false` for desktop OpenGL.
    pub embedded: bool,
}

impl GlVersion {
    /// `[any-thread]` Builds a version.
    #[must_use]
    pub const fn new(major: u32, minor: u32, embedded: bool) -> Self {
        Self {
            major,
            minor,
            embedded,
        }
    }

    /// `[any-thread]` Reads what a driver put in `GL_VERSION`.
    ///
    /// Returns `None` when there is no leading `major.minor` to find, which is the only part
    /// the specification guarantees. A number too large for a `u32` is treated as unparseable
    /// rather than wrapped: a version of `0.x` would then be compared against the shader
    /// requirements and silently pick the oldest GLSL dialect.
    #[must_use]
    pub fn parse(source: &str) -> Option<Self> {
        let source = source.trim();

        // `OpenGL ES 3.2 Mesa …`, `OpenGL 4.6 …`, `ES 3.0 …` and a bare `4.6 …` all occur.
        let (rest, embedded) = if let Some(rest) = source.strip_prefix("OpenGL ES ") {
            (rest, true)
        } else if let Some(rest) = source.strip_prefix("OpenGL ") {
            (rest, false)
        } else if let Some(rest) = source.strip_prefix("ES ") {
            (rest, true)
        } else {
            (source, false)
        };

        let numbers = rest.split_whitespace().next()?;
        let mut parts = numbers.split('.');
        let major = leading_u32(parts.next()?)?;
        let minor = leading_u32(parts.next()?)?;
        Some(Self {
            major,
            minor,
            embedded,
        })
    }

    /// `[any-thread]` Reads the version `glow` already parsed out of the context.
    ///
    /// Preferred over [`parse`](Self::parse) when a context exists: `glow` has queried the
    /// driver itself and there is no string left to get wrong.
    #[must_use]
    pub fn from_glow(version: &glow::Version) -> Self {
        Self {
            major: version.major,
            minor: version.minor,
            embedded: version.is_embedded,
        }
    }

    /// `[any-thread]` Whether this version is at least `major.minor` *of the same flavour*.
    ///
    /// Desktop and ES version numbers are not comparable — ES 3.0 is roughly desktop 3.3, not
    /// desktop 3.0 — so a desktop version never satisfies an ES requirement or the reverse.
    #[must_use]
    pub const fn at_least(self, major: u32, minor: u32, embedded: bool) -> bool {
        self.embedded == embedded
            && (self.major > major || (self.major == major && self.minor >= minor))
    }

    /// `[any-thread]` The `#version` directive (and, on ES, the precision qualifier) to put at
    /// the top of a shader for this context.
    ///
    /// Returns `None` for a context too old to run the shaders in this crate. The floor is
    /// GLSL 1.40 / ES 3.00, which is where `gl_VertexID` — and therefore the fullscreen
    /// triangle drawn with no vertex buffer at all — becomes available.
    #[must_use]
    pub const fn glsl_header(self) -> Option<&'static str> {
        if self.embedded {
            if self.at_least(3, 0, true) {
                // Every ES fragment shader must declare a float precision; there is no
                // default, and omitting it is a compile error rather than a warning.
                Some("#version 300 es\nprecision mediump float;\n")
            } else {
                None
            }
        } else if self.at_least(3, 3, false) {
            Some("#version 330 core\n")
        } else if self.at_least(3, 2, false) {
            Some("#version 150\n")
        } else if self.at_least(3, 1, false) {
            Some("#version 140\n")
        } else {
            None
        }
    }

    /// `[any-thread]` Whether this context can run the shaders in this crate at all.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        self.glsl_header().is_some()
    }

    /// `[any-thread]` Whether vertex array objects are core rather than an extension.
    ///
    /// A core-profile desktop context *requires* a bound vertex array object before any draw
    /// call, even one that reads no attributes, so this is not an optimisation — it decides
    /// whether the fullscreen triangle draws or raises `GL_INVALID_OPERATION`.
    #[must_use]
    pub const fn has_vertex_array_objects(self) -> bool {
        if self.embedded {
            self.at_least(3, 0, true)
        } else {
            self.at_least(3, 0, false)
        }
    }
}

impl core::fmt::Display for GlVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.embedded {
            write!(f, "OpenGL ES {}.{}", self.major, self.minor)
        } else {
            write!(f, "OpenGL {}.{}", self.major, self.minor)
        }
    }
}

/// Parses the digits at the start of `text`, ignoring whatever follows.
///
/// `3.3.0 - Build …` gives `0` for the release component and `2)` in
/// `OpenGL ES 3.2) …` gives `2`. An empty digit run, or one that overflows a `u32`, is
/// rejected rather than clamped.
fn leading_u32(text: &str) -> Option<u32> {
    let digits: &str = text.split(|c: char| !c.is_ascii_digit()).next()?;
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_driver_strings_parse() {
        let cases = [
            ("4.6.0 NVIDIA 566.36", GlVersion::new(4, 6, false)),
            ("3.3.0 - Build 31.0.101.5186", GlVersion::new(3, 3, false)),
            ("4.1 Metal - 89.3", GlVersion::new(4, 1, false)),
            ("4.6 (Core Profile) Mesa 24.0", GlVersion::new(4, 6, false)),
            ("OpenGL ES 3.2 Mesa 23.2.1", GlVersion::new(3, 2, true)),
            ("OpenGL ES 2.0 ANGLE", GlVersion::new(2, 0, true)),
            ("OpenGL 4.5", GlVersion::new(4, 5, false)),
            ("  4.6.0  ", GlVersion::new(4, 6, false)),
        ];
        for (text, expected) in cases {
            assert_eq!(GlVersion::parse(text), Some(expected), "parsing {text:?}");
        }
    }

    #[test]
    fn nonsense_strings_are_rejected_rather_than_defaulting_to_version_zero() {
        // A driver string that parses to 0.0 would silently pick the oldest GLSL dialect and
        // fail to link on a context that would have worked.
        for text in [
            "",
            "   ",
            "OpenGL",
            "4",
            "4.",
            ".4",
            "x.y",
            "OpenGL ES",
            "vendor only",
            // Wider than a u32: rejected, not wrapped.
            "99999999999.1",
            "4.99999999999",
        ] {
            assert_eq!(GlVersion::parse(text), None, "{text:?} should not parse");
        }
    }

    #[test]
    fn desktop_and_es_versions_are_never_compared_with_each_other() {
        let desktop = GlVersion::new(4, 6, false);
        let es = GlVersion::new(3, 2, true);
        assert!(desktop.at_least(3, 3, false));
        assert!(
            !desktop.at_least(2, 0, true),
            "a desktop context does not satisfy an ES requirement"
        );
        assert!(es.at_least(3, 0, true));
        assert!(
            !es.at_least(3, 0, false),
            "ES 3.2 is not desktop 3.2 and must not answer for it"
        );
    }

    #[test]
    fn at_least_compares_minor_versions_properly() {
        let v = GlVersion::new(3, 2, false);
        assert!(v.at_least(3, 2, false));
        assert!(v.at_least(3, 1, false));
        assert!(v.at_least(2, 9, false));
        assert!(!v.at_least(3, 3, false));
        assert!(!v.at_least(4, 0, false));
    }

    #[test]
    fn the_glsl_header_matches_the_context_and_es_always_declares_a_precision() {
        assert_eq!(
            GlVersion::new(4, 6, false).glsl_header(),
            Some("#version 330 core\n")
        );
        assert_eq!(
            GlVersion::new(3, 3, false).glsl_header(),
            Some("#version 330 core\n")
        );
        assert_eq!(
            GlVersion::new(3, 2, false).glsl_header(),
            Some("#version 150\n")
        );
        assert_eq!(
            GlVersion::new(3, 1, false).glsl_header(),
            Some("#version 140\n")
        );

        let es = GlVersion::new(3, 0, true)
            .glsl_header()
            .expect("ES 3.0 is supported");
        assert!(es.starts_with("#version 300 es"));
        assert!(
            es.contains("precision"),
            "an ES fragment shader with no precision qualifier does not compile"
        );
    }

    #[test]
    fn contexts_too_old_for_the_shaders_say_so_instead_of_producing_a_link_error() {
        for old in [
            GlVersion::new(2, 1, false),
            GlVersion::new(3, 0, false),
            GlVersion::new(1, 5, false),
            GlVersion::new(2, 0, true),
        ] {
            assert_eq!(old.glsl_header(), None, "{old} should be unsupported");
            assert!(!old.is_supported());
        }
        assert!(GlVersion::new(3, 1, false).is_supported());
        assert!(GlVersion::new(3, 0, true).is_supported());
    }

    #[test]
    fn vertex_array_objects_are_core_from_three_onwards() {
        assert!(GlVersion::new(3, 0, false).has_vertex_array_objects());
        assert!(GlVersion::new(4, 6, false).has_vertex_array_objects());
        assert!(!GlVersion::new(2, 1, false).has_vertex_array_objects());
        assert!(GlVersion::new(3, 0, true).has_vertex_array_objects());
        assert!(!GlVersion::new(2, 0, true).has_vertex_array_objects());
    }

    #[test]
    fn the_display_form_names_the_flavour() {
        assert_eq!(GlVersion::new(4, 6, false).to_string(), "OpenGL 4.6");
        assert_eq!(GlVersion::new(3, 2, true).to_string(), "OpenGL ES 3.2");
    }

    #[test]
    fn glow_versions_convert_without_a_string_in_between() {
        let version = glow::Version {
            major: 3,
            minor: 3,
            is_embedded: false,
            revision: Some(0),
            vendor_info: "Mesa".to_owned(),
        };
        assert_eq!(GlVersion::from_glow(&version), GlVersion::new(3, 3, false));
    }
}
