//! The module header and the `create_factory`/`destroy_factory` pair (abi-v1 §4).
//!
//! [`export_entry!`](crate::export_entry) puts a [`DauxPluginEntryV1`] built by [`entry_v1`]
//! into a `static` in the plug-in's own crate and returns a pointer to it from
//! `daux_plugin_entry_v1`. Everything the two exported functions do afterwards is generic-free
//! and lives here, so the macro expands to as little code as possible.

use daux_abi::{
    DAUX_ABI_MAGIC, DAUX_ABI_VERSION_MAJOR, DAUX_ABI_VERSION_MINOR, DAUX_ENTRY_SYMBOL, DAUX_OK,
    DauxFactoryHandle, DauxFactoryV1, DauxHostV1, DauxName, DauxPluginEntryV1, DauxStatus,
    DauxVersion,
};
use daux_plugin_api::DauxFactory;

use crate::factory::{FACTORY_API, FactoryState};
use crate::host::HostBridge;
use crate::panic::{Refusal, catch};

/// The SDK name written into [`DauxPluginEntryV1::sdk_name`]. Diagnostics only.
pub const SDK_NAME: &str = "DAUxPlug";

/// The SDK version written into [`DauxPluginEntryV1::sdk_version`], taken from this crate's
/// package version. Diagnostics only.
pub const SDK_VERSION: DauxVersion = DauxVersion {
    major: parse_u32(env!("CARGO_PKG_VERSION_MAJOR")),
    minor: parse_u32(env!("CARGO_PKG_VERSION_MINOR")),
    patch: parse_u32(env!("CARGO_PKG_VERSION_PATCH")),
    build: 0,
};

/// [any-thread] The name of the symbol [`export_entry!`](crate::export_entry) exports.
///
/// Hosts resolve this from the module; tools print it. It is the same constant `daux-abi`
/// defines, re-exported here so a build script or a bundler does not have to depend on the ABI
/// crate to name it.
#[must_use]
pub const fn entry_symbol() -> &'static str {
    DAUX_ENTRY_SYMBOL
}

/// Copies `s` into a fixed-size, NUL-padded buffer, truncating on a UTF-8 character boundary.
///
/// A `const fn` twin of [`DauxName::new`] and friends: the entry header is a `static`, so its
/// text fields have to be built at compile time.
const fn fixed_bytes<const N: usize>(s: &str) -> [u8; N] {
    let src = s.as_bytes();
    let mut end = if src.len() < N { src.len() } else { N };
    // A truncation may have landed inside a multi-byte character; walk back off its
    // continuation bytes so the result is always valid UTF-8 (abi-v1 §2.1).
    if end < src.len() {
        while end > 0 && (src[end] & 0xC0) == 0x80 {
            end -= 1;
        }
    }
    let mut out = [0u8; N];
    let mut i = 0;
    while i < end {
        out[i] = src[i];
        i += 1;
    }
    out
}

/// Parses the leading decimal digits of `s`, stopping at the first byte that is not one.
///
/// Cargo's `CARGO_PKG_VERSION_*` variables are strings, and a pre-release patch component such
/// as `0-beta.3` must not make this fail to compile — it yields `0`, which is right.
const fn parse_u32(s: &str) -> u32 {
    let bytes = s.as_bytes();
    let mut value: u32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let digit = bytes[i];
        if digit < b'0' || digit > b'9' {
            return value;
        }
        value = value * 10 + (digit - b'0') as u32;
        i += 1;
    }
    value
}

/// [main-thread] Builds the module header a `.axt` exposes through `daux_plugin_entry_v1`.
///
/// `size`, `magic` and the ABI version are filled from `daux-abi`, every reserved field is
/// zeroed, and the two function pointers are the ones
/// [`export_entry!`](crate::export_entry) generated. A host validates the result with
/// [`DauxPluginEntryV1::check`].
#[must_use]
pub const fn entry_v1(
    create_factory: unsafe extern "C" fn(
        host: *const DauxHostV1,
        out_factory: *mut DauxFactoryV1,
    ) -> DauxStatus,
    destroy_factory: unsafe extern "C" fn(factory: DauxFactoryV1),
) -> DauxPluginEntryV1 {
    DauxPluginEntryV1 {
        size: DauxPluginEntryV1::SIZE,
        abi_version_major: DAUX_ABI_VERSION_MAJOR,
        abi_version_minor: DAUX_ABI_VERSION_MINOR,
        _pad0: 0,
        magic: DAUX_ABI_MAGIC,
        sdk_name: DauxName(fixed_bytes(SDK_NAME)),
        sdk_version: SDK_VERSION,
        create_factory,
        destroy_factory,
        reserved: [0; 8],
    }
}

/// [main-thread] Body of the ABI's `create_factory` (abi-v1 §4).
///
/// `make` is called inside the unwind guard, so a factory constructor that panics — which
/// `PluginRegistry::register` does on a duplicate plug-in id — reports `DAUX_ERR_PANIC`
/// instead of tearing down the host.
///
/// # Safety
///
/// * `host` is either null or a valid [`DauxHostV1`] whose function table and handle stay
///   valid until `destroy_factory` returns (abi-v1 §16.1). It is read but never freed.
/// * `out_factory` is null or points to a writable, aligned [`DauxFactoryV1`] the caller owns.
///   It is written only on success.
pub unsafe fn create_factory(
    host: *const DauxHostV1,
    out_factory: *mut DauxFactoryV1,
    make: fn() -> Box<dyn DauxFactory>,
) -> DauxStatus {
    catch(|| {
        if out_factory.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        // SAFETY: the caller guarantees `host` is null or a live interface pair that outlives
        // this factory; the bridge copies the handle and table pointer and dereferences the
        // table only through the same guarantee.
        let bridge = unsafe { HostBridge::from_abi(host) };
        let state = Box::new(FactoryState::new(make(), bridge));
        let handle = Box::into_raw(state);
        // SAFETY: `out_factory` is non-null and, per the caller's contract, points at a
        // writable, aligned `DauxFactoryV1`. `handle` is a fresh, uniquely owned allocation
        // whose ownership passes to the host until it calls `destroy_factory`; `FACTORY_API` is
        // a `static`, so its address is valid for as long as this module is loaded.
        unsafe {
            out_factory.write(DauxFactoryV1::new(
                DauxFactoryHandle::from_ptr(handle.cast()),
                &raw const FACTORY_API,
            ));
        }
        DAUX_OK
    })
}

/// [main-thread] Body of the ABI's `destroy_factory` (abi-v1 §4).
///
/// Every instance created from the factory must already have been destroyed; the ABI puts that
/// obligation on the host and this crate cannot check it, because an instance's handle is not
/// reachable from the factory.
///
/// A `Drop` implementation in the plug-in that panics is caught here: the factory's memory is
/// released either way, since the unwind happens inside `Box`'s drop glue.
///
/// # Safety
///
/// `factory` is the interface pair a previous [`create_factory`] wrote, passed back at most
/// once. A pair with a foreign `api` pointer is ignored rather than freed.
pub unsafe fn destroy_factory(factory: DauxFactoryV1) {
    catch(|| {
        if factory.handle.is_null() || factory.api != &raw const FACTORY_API {
            // Not a handle this module produced — or a double free. Either way, touching it
            // would be worse than leaking it.
            return;
        }
        // SAFETY: the handle was produced by `Box::into_raw` in `create_factory` for exactly
        // this `FACTORY_API` table, ownership was transferred to the host, and the host is
        // returning it exactly once. Reconstructing the `Box` reclaims it.
        drop(unsafe { Box::from_raw(factory.handle.as_ptr().cast::<FactoryState>()) });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_bytes_truncates_on_a_character_boundary() {
        // Four bytes of text into a four-byte buffer: nothing is lost.
        assert_eq!(fixed_bytes::<4>("abcd"), *b"abcd");
        // Padding is NUL, never leftover stack.
        assert_eq!(fixed_bytes::<6>("ab"), [b'a', b'b', 0, 0, 0, 0]);
        // "é" is two bytes; a three-byte buffer must not keep half of it.
        assert_eq!(fixed_bytes::<3>("aé"), [b'a', 0xC3, 0xA9]);
        assert_eq!(fixed_bytes::<2>("aé"), [b'a', 0]);
        // The result is always valid UTF-8, which is what abi-v1 §2.1 demands of a writer.
        let buf = fixed_bytes::<5>("€uro");
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        assert!(core::str::from_utf8(&buf[..end]).is_ok());
    }

    #[test]
    fn parse_u32_stops_at_a_pre_release_suffix() {
        assert_eq!(parse_u32("0"), 0);
        assert_eq!(parse_u32("12"), 12);
        assert_eq!(parse_u32("3-beta.1"), 3);
        assert_eq!(parse_u32(""), 0);
        assert_eq!(parse_u32("beta"), 0);
    }

    /// The four rejection rules of abi-v1 §3 applied to what this crate produces.
    #[test]
    fn the_generated_header_passes_the_hosts_rejection_rules() {
        unsafe extern "C" fn create(
            _host: *const DauxHostV1,
            _out: *mut DauxFactoryV1,
        ) -> DauxStatus {
            DAUX_OK
        }
        unsafe extern "C" fn destroy(_factory: DauxFactoryV1) {}

        let entry = entry_v1(create, destroy);
        assert_eq!(entry.magic, 0x4441_5558_4142_4931);
        assert_eq!(entry.abi_version_major, 1);
        assert_eq!(entry.size, DauxPluginEntryV1::SIZE);
        assert!(entry.is_v1_0_compatible());
        assert!(entry.check().is_ok());
        assert_eq!(entry._pad0, 0);
        assert_eq!(entry.reserved, [0; 8]);
        assert_eq!(entry.sdk_name.as_str(), SDK_NAME);
        assert_eq!(entry_symbol(), "daux_plugin_entry_v1");
    }
}
