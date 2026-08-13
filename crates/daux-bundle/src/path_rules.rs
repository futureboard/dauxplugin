//! Logical-path validation and bundle-confined resolution (`axt-v1` §10.2).
//!
//! Everything here runs **before** the filesystem is touched, and the containment check
//! runs on the *canonicalised* result so that a symlink or junction cannot smuggle a read
//! out of the bundle. Rejection, not translation, is deliberate: a loader that helpfully
//! rewrote `\` to `/` would let `..\..` slip past a filter written against `/`.

use std::path::{Component, Path, PathBuf};

use crate::error::{BundleError, BundleErrorKind, BundleResult};
use crate::limits::{MAX_LOGICAL_PATH_BYTES, MAX_PATH_COMPONENT_BYTES};

/// Windows reserved device names. Reserved with **or without** an extension and in any
/// case, on **every** platform: a bundle that loads on Linux and fails on Windows because
/// of a `COM1` resource is a broken bundle, and making the rule universal turns a
/// user-visible platform bug into a build-time validation error (`axt-v1` §10.2).
const DEVICE_NAMES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "COM", "LPT"];

/// Characters Windows reserves in a file name (`axt-v1` §10.2 rule 9), plus the separators
/// and the colon handled by rules 4 and 5.
const RESERVED_CHARS: [char; 9] = ['<', '>', ':', '"', '|', '?', '*', '\\', '/'];

fn escape(detail: impl Into<String>) -> BundleError {
    BundleError::new(BundleErrorKind::PathEscape, detail)
}

/// [main-thread] Whether `stem` names a Windows character device.
///
/// The check is on the component's stem — everything before the first `.` — because
/// `nul.txt` opens the null device just as `NUL` does.
#[must_use]
pub fn is_device_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    for name in DEVICE_NAMES {
        if upper == name {
            return true;
        }
        // COM0..COM9 and LPT0..LPT9.
        if (name == "COM" || name == "LPT")
            && upper.len() == name.len() + 1
            && upper.starts_with(name)
            && upper.as_bytes()[name.len()].is_ascii_digit()
        {
            return true;
        }
    }
    false
}

/// [main-thread] Validates one path component (a bare file or directory name).
///
/// Used for logical-path components, `dependencies` entries, `resources.dir`,
/// `resources.libraryDir` and `CFBundleExecutable` — every string the format allows to
/// influence a filesystem operation (`manifest-v1` §10.4).
///
/// # Errors
///
/// [`BundleErrorKind::PathEscape`] with a detail naming the rule that was violated.
pub fn validate_component(component: &str) -> BundleResult<()> {
    if component.is_empty() {
        return Err(escape("empty path component"));
    }
    if component.len() > MAX_PATH_COMPONENT_BYTES {
        return Err(escape(format!(
            "path component is {} bytes, limit is {MAX_PATH_COMPONENT_BYTES}",
            component.len()
        )));
    }
    if component == "." || component == ".." {
        return Err(escape(format!("`{component}` component")));
    }
    for ch in component.chars() {
        if ch.is_control() {
            return Err(escape(format!(
                "control character U+{:04X} in `{component}`",
                ch as u32
            )));
        }
        if RESERVED_CHARS.contains(&ch) {
            return Err(escape(format!("reserved character `{ch}` in `{component}`")));
        }
    }
    if component.ends_with('.') || component.ends_with(' ') {
        return Err(escape(format!(
            "`{component}` ends in a dot or a space, which Windows silently strips"
        )));
    }
    if is_device_name(component) {
        return Err(escape(format!(
            "`{component}` is a Windows reserved device name"
        )));
    }
    Ok(())
}

/// [main-thread] Validates a logical resource path and splits it into components.
///
/// A logical path is relative, UTF-8, forward-slash separated on every platform including
/// Windows, and case-sensitive. The returned components are borrowed from `logical` and
/// are safe to join onto a root with the platform's path API.
///
/// # Errors
///
/// [`BundleErrorKind::PathEscape`] for every rule of `axt-v1` §10.2: empty paths and empty
/// components, leading or trailing `/`, `.` and `..`, backslashes, colons, Windows device
/// names, components ending in `.` or a space, control characters, reserved characters,
/// over-long components and over-long paths.
pub fn validate_logical(logical: &str) -> BundleResult<Vec<&str>> {
    if logical.is_empty() {
        return Err(escape("empty logical path"));
    }
    if logical.len() > MAX_LOGICAL_PATH_BYTES {
        return Err(escape(format!(
            "logical path is {} bytes, limit is {MAX_LOGICAL_PATH_BYTES}",
            logical.len()
        )));
    }
    if logical.starts_with('/') {
        return Err(escape("logical path is absolute"));
    }
    if logical.ends_with('/') {
        return Err(escape("logical path ends in `/`"));
    }
    let components: Vec<&str> = logical.split('/').collect();
    for component in &components {
        validate_component(component)?;
    }
    Ok(components)
}

/// [main-thread] Whether `value` looks like an absolute path, a drive-qualified path or a
/// UNC prefix.
///
/// Used by `daux validate` for `axt.resource.absolute`: metadata must never carry an
/// absolute path reference.
#[must_use]
pub fn looks_absolute(value: &str) -> bool {
    if value.starts_with('/') || value.starts_with('\\') {
        return true;
    }
    // `C:` and `C:\` — a drive-qualified path in any position of the alphabet.
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// [main-thread] Resolves a logical path against `root`, confined to `root`.
///
/// The syntactic pass of [`validate_logical`] runs first, then the components are joined
/// onto `root` and the result is canonicalised. Canonicalisation resolves symlinks,
/// junctions and reparse points, so comparing the canonical result against the canonical
/// root is what catches an escape that every syntactic rule passed.
///
/// # Errors
///
/// * [`BundleErrorKind::PathEscape`] — illegal syntax, or a canonical path outside `root`.
/// * [`BundleErrorKind::NotFound`] — the file does not exist, or `root` does not exist.
///   Deliberately distinct from `PathEscape` (`axt-v1` §10.1).
/// * [`BundleErrorKind::NotRegularFile`] — the target is a directory, FIFO, socket or
///   device. Opening one can block indefinitely (`axt-v1` §10.2 rule 14).
pub fn resolve_within(root: &Path, logical: &str) -> BundleResult<PathBuf> {
    let components = validate_logical(logical)?;

    let canonical_root = root.canonicalize().map_err(|err| {
        BundleError::io(root, &err).or_path(root)
    })?;

    let mut joined = canonical_root.clone();
    for component in components {
        joined.push(component);
    }

    let canonical = joined.canonicalize().map_err(|err| {
        BundleError::io(&joined, &err)
    })?;

    if !canonical.starts_with(&canonical_root) {
        return Err(escape(format!(
            "`{logical}` resolves to `{}`, outside the bundle",
            canonical.display()
        ))
        .with_path(&canonical));
    }

    let meta = std::fs::symlink_metadata(&canonical)
        .map_err(|err| BundleError::io(&canonical, &err))?;
    if !meta.file_type().is_file() {
        return Err(
            BundleError::new(BundleErrorKind::NotRegularFile, format!("`{logical}`"))
                .with_path(&canonical),
        );
    }

    Ok(canonical)
}

/// [main-thread] Whether `child` is inside `parent` once both are canonicalised.
///
/// Returns `false` when either path cannot be canonicalised, which is the conservative
/// answer for a containment question.
#[must_use]
pub fn is_contained(parent: &Path, child: &Path) -> bool {
    match (parent.canonicalize(), child.canonicalize()) {
        (Ok(parent), Ok(child)) => child.starts_with(parent),
        _ => false,
    }
}

/// [main-thread] Whether `path` contains a `..`, a root or a prefix component.
///
/// Used when copying a source tree into a bundle: a builder must not be talked into
/// writing outside the directory it was told to write into.
#[must_use]
pub fn has_traversal_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_logical_paths() {
        for good in [
            "a",
            "Shaders/spectrum.wgsl",
            "Presets/Default.dauxpreset",
            "Images/ui/knob@2x.png",
            "Data/ünïcode.json",
            "a/b/c/d/e/f/g",
            "_leading-underscore",
            "file.tar.gz",
        ] {
            validate_logical(good).unwrap_or_else(|err| panic!("{good}: {err}"));
        }
    }

    #[test]
    fn rejects_every_traversal_rule() {
        let cases = [
            ("", "rule 1: empty"),
            ("a//b", "rule 1: empty component"),
            ("/Shaders/a.wgsl", "rule 2: leading slash"),
            ("Shaders/", "rule 2: trailing slash"),
            ("../../outside", "rule 3: parent"),
            ("a/./b", "rule 3: current"),
            ("..", "rule 3: bare parent"),
            ("..\\..\\outside", "rule 4: backslash"),
            ("a\\b", "rule 4: backslash"),
            ("C:/x", "rule 5: colon"),
            ("file.txt:stream", "rule 5: alternate data stream"),
            ("CON", "rule 6: device"),
            ("nul.txt", "rule 6: device with extension"),
            ("dir/COM1", "rule 6: device in a subdirectory"),
            ("LPT9.dat", "rule 6: device"),
            ("aux", "rule 6: device, lower case"),
            ("PRN.tar.gz", "rule 6: device, double extension"),
            ("evil. ", "rule 7: trailing space"),
            ("dir./x", "rule 7: trailing dot"),
            ("a\u{0}b", "rule 8: NUL"),
            ("a\u{1f}b", "rule 8: control"),
            ("a\u{7f}b", "rule 8: DEL"),
            ("a?b", "rule 9: reserved"),
            ("a*b", "rule 9: reserved"),
            ("a<b", "rule 9: reserved"),
            ("a>b", "rule 9: reserved"),
            ("a\"b", "rule 9: reserved"),
            ("a|b", "rule 9: reserved"),
            ("\\\\?\\C:\\Windows", "UNC extended-length prefix"),
            ("\\\\server\\share\\x", "UNC path"),
        ];
        for (bad, why) in cases {
            let err = validate_logical(bad).expect_err(why);
            assert_eq!(err.kind(), &BundleErrorKind::PathEscape, "{why}");
            assert_eq!(err.code(), "DAUX-M055", "{why}");
        }
    }

    #[test]
    fn rejects_over_long_components_and_paths() {
        let long_component = "a".repeat(MAX_PATH_COMPONENT_BYTES + 1);
        assert!(validate_logical(&long_component).is_err());
        let ok_component = "a".repeat(MAX_PATH_COMPONENT_BYTES);
        assert!(validate_logical(&ok_component).is_ok());

        let deep = std::iter::repeat_n("ab", MAX_LOGICAL_PATH_BYTES).collect::<Vec<_>>().join("/");
        assert!(deep.len() > MAX_LOGICAL_PATH_BYTES);
        assert!(validate_logical(&deep).is_err());
    }

    #[test]
    fn device_name_detection() {
        for name in [
            "CON", "con", "Con", "PRN", "AUX", "NUL", "COM0", "COM9", "LPT0", "LPT9", "nul.txt",
            "com1.dll",
        ] {
            assert!(is_device_name(name), "{name}");
        }
        for name in ["CONSOLE", "COM10", "NULL", "PRNT", "AUXILIARY", "com1x"] {
            assert!(!is_device_name(name), "{name}");
        }
        // Bare `COM` and `LPT` are not reserved by Windows itself, but they are the stems of
        // the reserved names and nothing legitimately needs a bundle directory called that.
        // Rejecting them costs nothing and removes a whole class of near-miss.
        assert!(is_device_name("COM"));
        assert!(is_device_name("LPT"));
    }

    #[test]
    fn absolute_detection() {
        for abs in ["/etc/passwd", "\\Windows", "C:", "c:/x", "Z:\\y"] {
            assert!(looks_absolute(abs), "{abs}");
        }
        for rel in ["Shaders/a.wgsl", "a", "", "ab", "1:2"] {
            assert!(!looks_absolute(rel), "{rel}");
        }
    }

    #[test]
    fn traversal_component_detection() {
        assert!(has_traversal_component(Path::new("../x")));
        assert!(has_traversal_component(Path::new("/x")));
        assert!(!has_traversal_component(Path::new("a/b")));
        assert!(!has_traversal_component(Path::new("./a")));
    }

    #[test]
    fn component_validation_matches_logical_validation() {
        assert!(validate_component("normal.txt").is_ok());
        assert!(validate_component("a/b").is_err());
        assert!(validate_component("").is_err());
        assert!(validate_component(".").is_err());
        assert!(validate_component("..").is_err());
    }
}
