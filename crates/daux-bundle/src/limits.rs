//! Hard bounds every parser in this crate enforces **before** allocating.
//!
//! A bundle is untrusted input (`axt-v1` §1). Each constant here comes from
//! `docs/specifications/axt-v1.md` §7.7 or `docs/specifications/manifest-v1.md` §10.1;
//! where the two disagree the stricter value is used, because a reader that accepts less
//! can never mis-parse a document a stricter reader rejects.
//!
//! Every limit is checked against a length that is already known (a directory entry's
//! size, a token's span) rather than after materialising the value: the point is to
//! *refuse* rather than to *allocate and then complain*.

/// Largest `manifest.json` / `Info.plist` this crate will read, in bytes.
///
/// `axt-v1` §7.7, `manifest-v1` §10.1 (`DAUX-M002`). Checked against the directory
/// entry's length before the first byte is read.
pub const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

/// Hard cap on any single parsed string value, in bytes (`DAUX-M009`).
///
/// Individual fields have tighter limits ([`MAX_NAME_BYTES`], [`MAX_TEXT_BYTES`], …);
/// this one bounds *every* string in the document, including ones this version does not
/// recognise.
pub const MAX_STRING_BYTES: usize = 4096;

/// Largest number of entries in `targets` (`DAUX-M012`).
pub const MAX_TARGETS: usize = 256;

/// Largest number of entries in `dependencies` (`manifest-v1` §10.1).
pub const MAX_DEPENDENCIES: usize = 256;

/// Largest combined number of `resources.required` + `resources.optional` entries
/// (`axt-v1` §7.7).
pub const MAX_RESOURCE_ENTRIES: usize = 4096;

/// Largest number of entries in the optional `plugins` array (`axt-v1` §7.7).
pub const MAX_PLUGIN_ENTRIES: usize = 1024;

/// Largest number of keys in the `capabilities` object (`manifest-v1` §3.8).
pub const MAX_CAPABILITY_KEYS: usize = 256;

/// Largest number of entries in `plugin.features` (`manifest-v1` §3.2).
pub const MAX_FEATURES: usize = 32;

/// Largest number of keys in any one JSON object or plist dictionary (`DAUX-M008`).
pub const MAX_OBJECT_KEYS: usize = 1024;

/// Largest number of elements in any one JSON or plist array (`DAUX-M008`).
pub const MAX_ARRAY_ELEMENTS: usize = 1024;

/// Largest JSON / plist nesting depth (`DAUX-M018`).
pub const MAX_DEPTH: usize = 16;

/// Largest logical resource path, in bytes. Matches `DAUX_PATH_SIZE` (`axt-v1` §10.2).
pub const MAX_LOGICAL_PATH_BYTES: usize = daux_abi::DAUX_PATH_SIZE;

/// Largest single path component, in bytes (`axt-v1` §10.2 rule 10).
pub const MAX_PATH_COMPONENT_BYTES: usize = 255;

/// Largest plug-in id, in bytes. Fits `DauxId` with room for a NUL (`manifest-v1` §3.2).
pub const MAX_ID_BYTES: usize = daux_abi::DAUX_ID_SIZE - 1;

/// Largest `name`, `vendor`, `versionString`, `license`, in bytes. Fits `DauxName`.
pub const MAX_NAME_BYTES: usize = daux_abi::DAUX_NAME_SIZE - 1;

/// Largest `description`, `url`, `supportUrl`, `copyright`, in bytes. Fits `DauxText`.
pub const MAX_TEXT_BYTES: usize = daux_abi::DAUX_TEXT_SIZE - 1;

/// Largest single `features` tag, in bytes (`manifest-v1` §3.2).
pub const MAX_FEATURE_BYTES: usize = 31;

/// Largest `<BundleName>`, in bytes (`axt-v1` §2).
pub const MAX_BUNDLE_NAME_BYTES: usize = 64;

/// Largest target identifier, in bytes (`manifest-v1` §3.7).
pub const MAX_TARGET_ID_BYTES: usize = 32;

/// Largest `resources.dir` / `resources.libraryDir` value, in bytes (`manifest-v1` §3.11).
pub const MAX_DIRECTORY_NAME_BYTES: usize = 64;

/// Default ceiling applied by [`ResourceDir::read`](crate::ResourceDir::read).
///
/// Resources are plug-in-supplied payloads rather than metadata, so the metadata limit
/// would be far too tight; this bound exists so that a host reading a resource on behalf
/// of a plug-in cannot be talked into a multi-gigabyte allocation by a crafted bundle.
/// Adjust it with [`ResourceDir::set_max_read_bytes`](crate::ResourceDir::set_max_read_bytes).
pub const DEFAULT_MAX_RESOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Bundle format version implemented by this crate (`axt-v1` §12).
pub const FORMAT_VERSION: u32 = 1;

/// The `format` sentinel every `manifest.json` carries (`manifest-v1` §3.1).
pub const FORMAT_SENTINEL: &str = "DAUx Audio Extension";
