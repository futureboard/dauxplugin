//! Reading a foreign function table without trusting it.
//!
//! Every structure a plug-in module hands the host is written by code the host did not
//! compile. Two things can go wrong before a single field is read:
//!
//! * the module declares a `size` smaller than the v1.0 minimum, so the bytes the host
//!   would read are not there at all (`abi-v1` §3, rejection rule 4);
//! * a non-optional `unsafe extern "C" fn` entry is null, which is not a value the Rust
//!   type admits — materialising the structure would be undefined behaviour before the
//!   host ever got the chance to check it.
//!
//! [`read_table`] does both checks against the raw bytes and only then produces the typed
//! value, so a hostile or merely broken module is refused rather than dereferenced.

use daux_abi::AbiStruct;

use crate::error::{RuntimeError, RuntimeResult};

/// One non-optional function-pointer entry of a table: its byte offset and its name.
///
/// Built with [`core::mem::offset_of!`] in a `const` slice per table, so the offsets can
/// never drift from the struct definition.
pub(crate) type RequiredFn = (usize, &'static str);

/// Validates a foreign ABI structure and copies it into host memory. [main-thread]
///
/// The order of checks is the order of `abi-v1` §3: the `size` word is read first, because
/// nothing else may be read until it says the bytes exist. `required` lists the entries
/// whose Rust type has no null representation; optional entries (`Option<unsafe extern "C"
/// fn(..)>`) are deliberately absent from it, since null is their "not supported" value.
///
/// # Errors
///
/// [`RuntimeErrorKind::Protocol`](crate::RuntimeErrorKind::Protocol) for a null table or a
/// null required entry, [`RuntimeErrorKind::AbiMismatch`](crate::RuntimeErrorKind::AbiMismatch)
/// for an undersized one.
///
/// # Safety
///
/// The caller guarantees that `ptr` is either null or points to at least four readable
/// bytes, and that whenever the `size` word those bytes hold is at least
/// `T::MIN_SIZE_V1_0`, the whole `T` is readable, owned by the producing module and
/// immutable for the duration of this call (`abi-v1` §2.3: function tables are immutable
/// and stay valid while their producer is loaded). Reads are unaligned, so the pointer's
/// alignment is not assumed.
pub(crate) unsafe fn read_table<T: AbiStruct>(
    ptr: *const T,
    what: &str,
    required: &[RequiredFn],
) -> RuntimeResult<T> {
    if ptr.is_null() {
        return Err(RuntimeError::protocol(format!(
            "{what}: the module returned a null function table"
        )));
    }
    let base = ptr.cast::<u8>();

    // SAFETY: `ptr` is non-null and the caller guarantees at least four readable bytes at
    // it. `size` is the first field of every growable ABI structure (`abi-v1` §1), so it
    // lives at offset 0. The read is unaligned and by value, so it neither assumes
    // alignment nor creates a reference into module memory.
    let declared = unsafe { base.cast::<u32>().read_unaligned() };
    if (declared as usize) < T::MIN_SIZE_V1_0 {
        return Err(RuntimeError::abi(format!(
            "{what}: declared size {declared} is below the v1.0 minimum of {}",
            T::MIN_SIZE_V1_0
        )));
    }

    for &(offset, name) in required {
        debug_assert!(offset + size_of::<usize>() <= T::MIN_SIZE_V1_0);
        // SAFETY: `declared >= T::MIN_SIZE_V1_0 == size_of::<T>()` was just established, and
        // every `offset` comes from `offset_of!` on `T`, so `offset + size_of::<usize>()`
        // is inside the object the caller guaranteed is readable. A function pointer is
        // exactly pointer-wide on every target this workspace supports, so reading the slot
        // as `usize` observes the whole entry without materialising an invalid `fn` value.
        let slot = unsafe { base.add(offset).cast::<usize>().read_unaligned() };
        if slot == 0 {
            return Err(RuntimeError::protocol(format!(
                "{what}: required entry `{name}` is null"
            )));
        }
    }

    // SAFETY: the size check proved the producer wrote at least `size_of::<T>()` bytes, and
    // the loop above proved every field with no null representation holds a non-null value,
    // so the bit pattern is a valid `T`. `T: AbiStruct` implies `Copy`, and the read is
    // unaligned and by value, so this copies out of module memory without borrowing it.
    Ok(unsafe { ptr.read_unaligned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RuntimeErrorKind;
    use crate::testing::{Aligned, plant};
    use core::mem::offset_of;
    use daux_abi::DauxFactoryApiV1;

    const REQUIRED: &[RequiredFn] = &[
        (offset_of!(DauxFactoryApiV1, plugin_count), "plugin_count"),
        (offset_of!(DauxFactoryApiV1, descriptor), "descriptor"),
        (offset_of!(DauxFactoryApiV1, create_plugin), "create_plugin"),
    ];

    #[test]
    fn a_well_formed_table_is_accepted() {
        let table = crate::testing::factory_api();
        let mut buffer = Aligned::<256>::new();
        let ptr = plant(&mut buffer, &table);
        // SAFETY: `plant` copied a whole, well-formed `DauxFactoryApiV1` into `buffer`,
        // which outlives this call, so the pointer addresses `size_of::<T>()` readable
        // bytes that nothing else mutates while `read_table` runs.
        let read = unsafe { read_table(ptr, "factory", REQUIRED) }.expect("valid table");
        assert_eq!(read.size, DauxFactoryApiV1::SIZE);
        assert!(read.get_extension.is_some());
    }

    /// A newer module appends fields at the tail and declares a bigger `size`. A v1.0 host
    /// must accept it and ignore the unknown bytes (`abi-v1` §3).
    #[test]
    fn a_larger_declared_size_is_accepted_and_the_tail_ignored() {
        let table = crate::testing::factory_api();
        let mut buffer = Aligned::<256>::new();
        let ptr = plant(&mut buffer, &table);
        buffer.set_declared_size(DauxFactoryApiV1::SIZE + 64);
        // SAFETY: as above; the buffer is 256 bytes, so even the inflated `size` the
        // module declares stays inside memory this test owns.
        let read = unsafe { read_table(ptr, "factory", REQUIRED) }.expect("forward compatible");
        assert_eq!(read.size, DauxFactoryApiV1::SIZE + 64);
    }

    #[test]
    fn a_null_table_is_refused_without_being_read() {
        // SAFETY: a null pointer is explicitly permitted by `read_table`'s contract and is
        // rejected before any read happens.
        let err = unsafe { read_table(core::ptr::null::<DauxFactoryApiV1>(), "factory", REQUIRED) }
            .unwrap_err();
        assert_eq!(err.kind(), RuntimeErrorKind::Protocol);
    }

    /// The whole point of reading `size` first: a module that declares less than the v1.0
    /// minimum must be refused *before* the host reads any field beyond `size` itself.
    #[test]
    fn an_undersized_table_is_refused() {
        let table = crate::testing::factory_api();
        for declared in [0, 1, 8, DauxFactoryApiV1::SIZE - 1] {
            let mut buffer = Aligned::<256>::new();
            let ptr = plant(&mut buffer, &table);
            buffer.set_declared_size(declared);
            // SAFETY: the buffer holds a full table; only the declared `size` lies, which
            // is exactly the case this checks. `read_table` reads nothing past `size`.
            let err = unsafe { read_table(ptr, "factory", REQUIRED) }.unwrap_err();
            assert_eq!(
                err.kind(),
                RuntimeErrorKind::AbiMismatch,
                "size {declared} should be refused"
            );
            assert!(err.message().contains("v1.0 minimum"), "{err}");
        }
    }

    /// A null entry in a non-optional slot is not a value the Rust type can hold, so it
    /// must be caught by the byte-level probe, not by reading the struct.
    #[test]
    fn a_null_required_entry_is_refused_per_field() {
        let table = crate::testing::factory_api();
        for &(offset, name) in REQUIRED {
            let mut buffer = Aligned::<256>::new();
            let ptr = plant(&mut buffer, &table);
            buffer.zero_slot(offset);
            // SAFETY: the buffer holds `size_of::<T>()` readable bytes; one function-pointer
            // slot has been zeroed, which `read_table` must detect without materialising a
            // `DauxFactoryApiV1` that holds a null non-nullable entry.
            let err = unsafe { read_table(ptr, "factory", REQUIRED) }.unwrap_err();
            assert_eq!(err.kind(), RuntimeErrorKind::Protocol);
            assert!(err.message().contains(name), "{err} should name `{name}`");
        }
    }

    /// A null *optional* entry means "not supported" and must not be refused.
    #[test]
    fn a_null_optional_entry_is_not_an_error() {
        let mut table = crate::testing::factory_api();
        table.get_extension = None;
        let mut buffer = Aligned::<256>::new();
        let ptr = plant(&mut buffer, &table);
        // SAFETY: as in `a_well_formed_table_is_accepted`; the only difference is that an
        // `Option<fn>` slot is `None`, which is a legal bit pattern.
        let read = unsafe { read_table(ptr, "factory", REQUIRED) }.expect("optional may be null");
        assert!(read.get_extension.is_none());
    }
}
