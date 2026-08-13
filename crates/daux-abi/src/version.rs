//! Versions, magic and negotiation rules (`abi-v1` §2, §3).

use crate::status::{DAUX_ERR_ABI_MISMATCH, DAUX_OK, DauxStatus};

/// Major ABI version implemented by this crate.
///
/// `daux_plugin_entry_v1` always implies `abi_version_major == 1`.
pub const DAUX_ABI_VERSION_MAJOR: u32 = 1;

/// Minor ABI version implemented by this crate.
///
/// The minor version identifies tail extensions of v1 structures. A host MUST accept a
/// plug-in with a lower *or* higher minor version and MUST ignore unknown tail bytes.
pub const DAUX_ABI_VERSION_MINOR: u32 = 0;

/// Magic word stamped into [`DauxPluginEntryV1`](crate::DauxPluginEntryV1): `"DAUXABI1"`
/// read as a big-endian `u64`.
pub const DAUX_ABI_MAGIC: u64 = 0x4441_5558_4142_4931;

/// Four-component version. Ordering is lexicographic over `(major, minor, patch, build)`.
///
/// [any-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DauxVersion {
    /// Incompatible changes.
    pub major: u32,
    /// Backwards-compatible additions.
    pub minor: u32,
    /// Backwards-compatible fixes.
    pub patch: u32,
    /// Build counter; not part of the public identity of a release.
    pub build: u32,
}

impl DauxVersion {
    /// The all-zero version.
    pub const ZERO: Self = Self::new(0, 0, 0, 0);

    /// [any-thread] Builds a version.
    #[inline]
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32, build: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            build,
        }
    }
}

/// [any-thread] Applies the rejection rules of `abi-v1` §3 to an entry header's fields.
///
/// Returns [`DAUX_OK`] when the module may be loaded, [`DAUX_ERR_ABI_MISMATCH`] otherwise.
/// The `size` check uses `min_size`, which callers take from
/// `DauxPluginEntryV1::MIN_SIZE_V1_0`.
///
/// The minor version is deliberately *not* checked: a host MUST accept both older and
/// newer minor revisions and validate individual fields with `field_present` instead.
#[inline]
#[must_use]
pub const fn check_entry_header(
    magic: u64,
    abi_version_major: u32,
    size: u32,
    min_size: usize,
) -> DauxStatus {
    if magic != DAUX_ABI_MAGIC {
        return DAUX_ERR_ABI_MISMATCH;
    }
    if abi_version_major != DAUX_ABI_VERSION_MAJOR {
        return DAUX_ERR_ABI_MISMATCH;
    }
    if (size as usize) < min_size {
        return DAUX_ERR_ABI_MISMATCH;
    }
    DAUX_OK
}
