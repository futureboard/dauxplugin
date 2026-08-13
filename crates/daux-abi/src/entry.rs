//! The module entry point (`abi-v1` §4).

use crate::compat::impl_abi_struct;
use crate::handle::{DauxFactoryV1, DauxHostV1};
use crate::status::DauxStatus;
use crate::string::DauxName;
use crate::version::DauxVersion;

/// Name of the exported entry symbol every `.axt` binary must provide.
pub const DAUX_ENTRY_SYMBOL: &str = "daux_plugin_entry_v1";

/// The entry symbol name as a NUL-terminated byte string, ready for `dlsym`/`GetProcAddress`.
pub const DAUX_ENTRY_SYMBOL_CSTR: &[u8] = b"daux_plugin_entry_v1\0";

/// Signature of the exported entry symbol.
///
/// ```ignore
/// #[unsafe(no_mangle)]
/// pub extern "C" fn daux_plugin_entry_v1() -> *const DauxPluginEntryV1;
/// ```
///
/// The returned pointer MUST be non-null, MUST point at storage with `'static` lifetime,
/// and MUST be identical across calls. The function MUST be callable before any other
/// DAUx symbol, MUST NOT block, MUST NOT allocate unbounded memory, and MUST NOT touch the
/// filesystem, the network, GPU devices or any GUI subsystem. [main-thread]
pub type DauxPluginEntryFn = unsafe extern "C" fn() -> *const DauxPluginEntryV1;

/// Static header every `.axt` module exposes through [`DAUX_ENTRY_SYMBOL`].
///
/// [main-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxPluginEntryV1 {
    /// `size_of::<DauxPluginEntryV1>()` as written by the producer.
    pub size: u32,
    /// Always [`DAUX_ABI_VERSION_MAJOR`](crate::DAUX_ABI_VERSION_MAJOR) for this symbol.
    pub abi_version_major: u32,
    /// Minor revision of the v1 structures this module was built against.
    pub abi_version_minor: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// MUST equal [`DAUX_ABI_MAGIC`](crate::DAUX_ABI_MAGIC).
    pub magic: u64,

    /// Identifies the SDK that produced the binary. Diagnostics only.
    pub sdk_name: DauxName,
    /// Version of the SDK that produced the binary. Diagnostics only.
    pub sdk_version: DauxVersion,

    /// Called once after the host has validated the header. `host` MUST remain valid
    /// until `destroy_factory` returns. [main-thread]
    pub create_factory: unsafe extern "C" fn(
        host: *const DauxHostV1,
        out_factory: *mut DauxFactoryV1,
    ) -> DauxStatus,

    /// Releases the factory. All plug-in instances created from it MUST already be
    /// destroyed. [main-thread]
    pub destroy_factory: unsafe extern "C" fn(factory: DauxFactoryV1),

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 8],
}

impl_abi_struct!(DauxPluginEntryV1);

impl DauxPluginEntryV1 {
    /// [main-thread] Validates the header against the rejection rules of `abi-v1` §3.
    ///
    /// Returns [`DAUX_OK`](crate::DAUX_OK) when the module may be loaded and
    /// [`DAUX_ERR_ABI_MISMATCH`](crate::DAUX_ERR_ABI_MISMATCH) otherwise. Callers must
    /// still check that the entry pointer itself was non-null (rule 1).
    #[inline]
    #[must_use]
    pub const fn check(&self) -> DauxStatus {
        crate::version::check_entry_header(
            self.magic,
            self.abi_version_major,
            self.size,
            Self::MIN_SIZE_V1_0,
        )
    }
}
