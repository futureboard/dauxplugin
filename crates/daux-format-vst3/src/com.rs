//! The hand-rolled COM layer VST3 is built on.
//!
//! VST3 is a C++ ABI in name only: what actually crosses the boundary is a pointer to a
//! struct whose first field is a pointer to a table of function pointers, the first three of
//! which are `queryInterface`, `addRef` and `release`. That is expressible in `#[repr(C)]`
//! Rust without a single line of C++, which is what this module does.
//!
//! # The three things that are easy to get wrong
//!
//! 1. **Calling convention.** VST3 methods are `SMTG_STDMETHODCALLTYPE`, i.e. `__stdcall` on
//!    Windows and the platform default everywhere else. Rust's [`extern "system"`] is exactly
//!    that mapping, so every function pointer here uses it. Using `extern "C"` would work on
//!    every 64-bit target and corrupt the stack on 32-bit Windows.
//! 2. **Result codes are platform-dependent.** On Windows VST3 uses the Win32 `HRESULT`
//!    values (`S_OK`, `E_NOINTERFACE`, …); everywhere else it uses a small enum starting at
//!    `-1`. A hard-coded `0` for "ok" happens to be right on both, but a hard-coded `-1` for
//!    "no interface" is wrong on Windows. See [`result`].
//! 3. **Interface ids are platform-dependent too.** Steinberg's `INLINE_UID` macro emits a
//!    different byte order on COM-compatible platforms (Windows) than elsewhere, so the same
//!    logical id is a different 16-byte array depending on the target. [`uid`] reproduces
//!    both branches; getting it wrong makes every `queryInterface` fail on exactly one
//!    platform.
//!
//! Class ids are deliberately *not* subject to that rule — see [`crate::cid`].
//!
//! [`extern "system"`]: https://doc.rust-lang.org/reference/items/external-blocks.html

use core::ffi::c_void;

/// A VST3 interface or class identifier: sixteen raw bytes.
///
/// In C this is `typedef int8 TUID[16]`, which decays to a pointer when passed, so every
/// place the SDK writes `const TUID` this crate writes `*const TUid`.
///
/// `[any-thread]`
pub type TUid = [u8; 16];

/// A VST3 result code. See [`result`] for the values.
///
/// `[any-thread]`
pub type TResult = i32;

/// VST3's boolean: one byte, `0` false, non-zero true.
///
/// `[any-thread]`
pub type TBool = u8;

/// A UTF-16 code unit, Steinberg's `char16`.
///
/// `[any-thread]`
pub type Char16 = u16;

/// A null-terminated ASCII string owned by the caller, Steinberg's `FIDString`.
///
/// `[any-thread]`
pub type FidString = *const core::ffi::c_char;

/// Result codes, whose numeric values differ between Windows and everything else.
///
/// On Windows VST3 is COM-compatible and reuses the Win32 `HRESULT` values; elsewhere it
/// uses a plain enumeration. Both are transcribed here from `pluginterfaces/base/funknown.h`
/// so that a caller never has to care.
///
/// `[any-thread]`
pub mod result {
    use super::TResult;

    /// Whether this target uses the COM-compatible (Win32 `HRESULT`) encoding.
    pub const COM_COMPATIBLE: bool = cfg!(target_os = "windows");

    /// The requested interface is not implemented (`E_NOINTERFACE`).
    pub const NO_INTERFACE: TResult = if COM_COMPATIBLE {
        0x8000_4002_u32 as TResult
    } else {
        -1
    };
    /// Success (`S_OK`).
    pub const OK: TResult = 0;
    /// Success, and the answer is "yes". The same value as [`OK`], as in COM.
    pub const TRUE: TResult = OK;
    /// Success, and the answer is "no" (`S_FALSE`). One on both platforms, by coincidence
    /// rather than by design: `S_FALSE` is `0x00000001` and the plain enum's second value is
    /// also `1`.
    pub const FALSE: TResult = 1;
    /// A pointer was null or a value out of range (`E_INVALIDARG`).
    pub const INVALID_ARGUMENT: TResult = if COM_COMPATIBLE {
        0x8007_0057_u32 as TResult
    } else {
        2
    };
    /// The method exists but this plug-in does not implement it (`E_NOTIMPL`).
    pub const NOT_IMPLEMENTED: TResult = if COM_COMPATIBLE {
        0x8000_4001_u32 as TResult
    } else {
        3
    };
    /// Something went wrong inside the plug-in (`E_FAIL`). This is what a caught panic
    /// becomes (abi-v1 §17.2).
    pub const INTERNAL_ERROR: TResult = if COM_COMPATIBLE {
        0x8000_4005_u32 as TResult
    } else {
        4
    };
    /// The call arrived in a state that does not allow it (`E_UNEXPECTED`). This is what a
    /// poisoned instance answers to everything (abi-v1 §17.3).
    pub const NOT_INITIALIZED: TResult = if COM_COMPATIBLE {
        0x8000_FFFF_u32 as TResult
    } else {
        5
    };
    /// Allocation failed (`E_OUTOFMEMORY`).
    pub const OUT_OF_MEMORY: TResult = if COM_COMPATIBLE {
        0x8007_000E_u32 as TResult
    } else {
        6
    };

    /// `[any-thread]` `true` when `r` reports success.
    ///
    /// COM's rule, not a comparison against [`OK`]: on Windows a negative value is a failure
    /// and everything else is a success, which is how [`FALSE`] can mean "no" without
    /// meaning "failed".
    #[must_use]
    pub const fn is_ok(r: TResult) -> bool {
        if COM_COMPATIBLE {
            r >= 0
        } else {
            r == OK || r == FALSE
        }
    }

    /// `[any-thread]` Turns a boolean answer into [`TRUE`] or [`FALSE`].
    #[must_use]
    pub const fn from_bool(yes: bool) -> TResult {
        if yes { TRUE } else { FALSE }
    }
}

/// Builds an interface id from the four 32-bit words Steinberg's `DECLARE_CLASS_IID` uses.
///
/// This is `INLINE_UID` from `funknown.h`, including its `COM_COMPATIBLE` branch: on Windows
/// the first three words are laid out like the fields of a Win32 `GUID` (little-endian
/// `Data1`, then two little-endian 16-bit halves of `Data2`/`Data3` in a peculiar order),
/// and everywhere else all four words are simply big-endian. The two are *not* the same
/// bytes, which is why a table of hand-copied literals is not good enough.
///
/// `[any-thread]`
#[must_use]
pub const fn uid(l1: u32, l2: u32, l3: u32, l4: u32) -> TUid {
    if result::COM_COMPATIBLE {
        [
            l1 as u8,
            (l1 >> 8) as u8,
            (l1 >> 16) as u8,
            (l1 >> 24) as u8,
            (l2 >> 16) as u8,
            (l2 >> 24) as u8,
            l2 as u8,
            (l2 >> 8) as u8,
            (l3 >> 24) as u8,
            (l3 >> 16) as u8,
            (l3 >> 8) as u8,
            l3 as u8,
            (l4 >> 24) as u8,
            (l4 >> 16) as u8,
            (l4 >> 8) as u8,
            l4 as u8,
        ]
    } else {
        [
            (l1 >> 24) as u8,
            (l1 >> 16) as u8,
            (l1 >> 8) as u8,
            l1 as u8,
            (l2 >> 24) as u8,
            (l2 >> 16) as u8,
            (l2 >> 8) as u8,
            l2 as u8,
            (l3 >> 24) as u8,
            (l3 >> 16) as u8,
            (l3 >> 8) as u8,
            l3 as u8,
            (l4 >> 24) as u8,
            (l4 >> 16) as u8,
            (l4 >> 8) as u8,
            l4 as u8,
        ]
    }
}

/// Compares a host-supplied interface id against one of ours.
///
/// # Safety
///
/// `iid` must be null or point to sixteen readable bytes for the duration of the call. A
/// null pointer compares equal to nothing, which is the answer a broken host deserves.
///
/// `[any-thread]`
#[must_use]
pub unsafe fn iid_eq(iid: *const TUid, ours: &TUid) -> bool {
    if iid.is_null() {
        return false;
    }
    // SAFETY: the caller promises `iid` points to sixteen readable bytes; `TUid` is
    // `[u8; 16]`, which has alignment 1, so any non-null address is correctly aligned, and
    // the read is a plain copy of sixteen bytes that borrows nothing.
    let theirs = unsafe { core::ptr::read_unaligned(iid) };
    theirs == *ours
}

/// The `FUnknown` vtable: the first three slots of *every* VST3 interface.
///
/// Every other vtable in [`crate::api`] repeats these three fields first rather than
/// embedding this struct, because C++ single inheritance lays the base class's slots out
/// inline and a nested `#[repr(C)]` struct would produce the same layout but a clumsier
/// call site.
#[repr(C)]
pub struct FUnknownVtbl {
    /// Asks for another interface on the same object, adding a reference on success.
    pub query_interface: unsafe extern "system" fn(
        this: *mut c_void,
        iid: *const TUid,
        obj: *mut *mut c_void,
    ) -> TResult,
    /// Adds a reference, returning the new count (for debugging only).
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    /// Drops a reference, returning the new count. At zero the object destroys itself.
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

/// An object we only ever reach through `FUnknown` — a host interface whose vtable we know
/// starts with the three canonical slots.
#[repr(C)]
pub struct FUnknown {
    /// Pointer to the object's vtable.
    pub vtbl: *const FUnknownVtbl,
}

/// `[main-thread]` Adds a reference to a host object, tolerating a null pointer.
///
/// # Safety
///
/// `obj` must be null or a live COM object whose vtable begins with `FUnknown`'s three
/// slots, and must stay alive for the duration of the call.
pub unsafe fn add_ref(obj: *mut c_void) -> u32 {
    if obj.is_null() {
        return 0;
    }
    // SAFETY: the caller promises `obj` is a live COM object, so its first word is a valid
    // vtable pointer whose first three entries are `FUnknown`'s. Ownership is unchanged by
    // reading the vtable; the call itself only borrows `obj` for its duration.
    unsafe {
        let vtbl = (*obj.cast::<FUnknown>()).vtbl;
        ((*vtbl).add_ref)(obj)
    }
}

/// `[any-thread]` Drops a reference to a host object, tolerating a null pointer.
///
/// # Safety
///
/// As [`add_ref`], plus: the caller must own the reference being dropped, and must not use
/// `obj` afterwards.
pub unsafe fn release(obj: *mut c_void) -> u32 {
    if obj.is_null() {
        return 0;
    }
    // SAFETY: the caller promises `obj` is a live COM object it owns a reference to. After
    // this call the pointer may dangle, which is why nothing here reads it again.
    unsafe {
        let vtbl = (*obj.cast::<FUnknown>()).vtbl;
        ((*vtbl).release)(obj)
    }
}

/// `[main-thread]` Asks a host object for another interface, returning null when it says no.
///
/// The returned pointer carries a reference the caller must [`release`].
///
/// # Safety
///
/// As [`add_ref`].
pub unsafe fn query_interface(obj: *mut c_void, iid: &TUid) -> *mut c_void {
    if obj.is_null() {
        return core::ptr::null_mut();
    }
    let mut out: *mut c_void = core::ptr::null_mut();
    // SAFETY: the caller promises `obj` is a live COM object; `iid` is a reference so it is
    // non-null, aligned and sixteen bytes long, and `out` is a live local. A conforming
    // `queryInterface` either writes an addRef'd pointer and returns success, or leaves
    // `out` alone — which is why it starts null and is only trusted on success.
    let r = unsafe {
        let vtbl = (*obj.cast::<FUnknown>()).vtbl;
        ((*vtbl).query_interface)(obj, iid, &raw mut out)
    };
    if result::is_ok(r) {
        out
    } else {
        core::ptr::null_mut()
    }
}

/// A raw pointer to a host-owned COM object that this crate holds across calls.
///
/// It is deliberately *not* an owning smart pointer: VST3 hands out borrowed pointers
/// (`ProcessData::inputEvents`) and owned ones (`queryInterface` results) through the same
/// C type, and hiding the difference behind a `Drop` impl is how adapters end up with an
/// off-by-one refcount. Ownership is tracked by which field a pointer is stored in and is
/// documented at each one.
///
/// The wrapper exists for exactly one reason: to let a `*mut c_void` live inside a struct
/// that is shared between the host's UI thread and its audio thread. VST3 requires the
/// interfaces stored this way (`IComponentHandler`, `IPlugFrame`) to be callable from the
/// thread the plug-in was given them on, and this crate only ever calls them from there.
#[derive(Debug)]
pub struct HostPtr(core::sync::atomic::AtomicPtr<c_void>);

// SAFETY: the wrapper only ever moves a raw pointer between threads, which is a plain
// pointer-sized atomic; it dereferences nothing by itself. Every call *through* the stored
// pointer goes via an `unsafe` block whose SAFETY comment states the thread it is made from,
// which is how the host's own threading contract is honoured.
unsafe impl Send for HostPtr {}
// SAFETY: as above — the interior is an `AtomicPtr`, so concurrent `get`/`set` are races only
// in the benign, atomic sense; no dereference happens without an explicit `unsafe` block.
unsafe impl Sync for HostPtr {}

impl HostPtr {
    /// `[any-thread]` An empty slot.
    #[must_use]
    pub const fn null() -> Self {
        Self(core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()))
    }

    /// `[any-thread]` The stored pointer, possibly null.
    #[must_use]
    pub fn get(&self) -> *mut c_void {
        self.0.load(core::sync::atomic::Ordering::Acquire)
    }

    /// `[main-thread]` Replaces the stored pointer, returning the previous one.
    ///
    /// Neither pointer is retained or released here: reference counting is the caller's job,
    /// because only the caller knows whether the incoming pointer was borrowed or owned.
    pub fn swap(&self, new: *mut c_void) -> *mut c_void {
        self.0.swap(new, core::sync::atomic::Ordering::AcqRel)
    }

    /// `[any-thread]` `true` when nothing is stored.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.get().is_null()
    }
}

impl Default for HostPtr {
    fn default() -> Self {
        Self::null()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uid_encoding_matches_steinbergs_macro() {
        // `DECLARE_CLASS_IID (IComponent, 0xE831FF31, 0xF2D54301, 0x928EBBEE, 0x25697802)`
        // expanded by hand from `funknown.h`, both branches.
        let got = uid(0xE831_FF31, 0xF2D5_4301, 0x928E_BBEE, 0x2569_7802);
        // The same value as the Win32 GUID {E831FF31-F2D5-4301-928E-BBEE25697802}: a
        // little-endian `Data1`, two little-endian 16-bit halves, then eight plain bytes.
        let com_compatible: TUid = [
            0x31, 0xFF, 0x31, 0xE8, 0xD5, 0xF2, 0x01, 0x43, 0x92, 0x8E, 0xBB, 0xEE, 0x25, 0x69,
            0x78, 0x02,
        ];
        let plain: TUid = [
            0xE8, 0x31, 0xFF, 0x31, 0xF2, 0xD5, 0x43, 0x01, 0x92, 0x8E, 0xBB, 0xEE, 0x25, 0x69,
            0x78, 0x02,
        ];
        let expected = if result::COM_COMPATIBLE {
            com_compatible
        } else {
            plain
        };
        assert_eq!(got, expected);
        // The two encodings really are different, so picking the wrong one is not harmless.
        assert_ne!(com_compatible, plain);
    }

    #[test]
    fn result_codes_follow_the_platforms_convention() {
        assert_eq!(result::OK, 0);
        assert_eq!(result::TRUE, result::OK);
        assert_ne!(result::FALSE, result::OK);
        assert!(result::is_ok(result::OK));
        assert!(result::is_ok(result::FALSE));
        assert!(!result::is_ok(result::NO_INTERFACE));
        assert!(!result::is_ok(result::INVALID_ARGUMENT));
        assert!(!result::is_ok(result::INTERNAL_ERROR));
        assert!(!result::is_ok(result::NOT_INITIALIZED));
        assert_eq!(result::from_bool(true), result::TRUE);
        assert_eq!(result::from_bool(false), result::FALSE);

        if result::COM_COMPATIBLE {
            // The Win32 values, which a naive `-1` would get wrong.
            assert_eq!(result::NO_INTERFACE, 0x8000_4002_u32 as i32);
        } else {
            assert_eq!(result::NO_INTERFACE, -1);
        }
    }

    #[test]
    fn iid_comparison_rejects_null_and_mismatches() {
        let a = uid(1, 2, 3, 4);
        let b = uid(1, 2, 3, 5);
        // SAFETY: `&a` points to a live `TUid`; the null case never dereferences.
        unsafe {
            assert!(iid_eq(&raw const a, &a));
            assert!(!iid_eq(&raw const b, &a));
            assert!(!iid_eq(core::ptr::null(), &a));
        }
    }

    #[test]
    fn a_host_pointer_slot_starts_empty_and_swaps() {
        let slot = HostPtr::null();
        assert!(slot.is_null());
        let fake = core::ptr::without_provenance_mut::<c_void>(0x1000);
        assert!(slot.swap(fake).is_null());
        assert_eq!(slot.get(), fake);
        assert_eq!(slot.swap(core::ptr::null_mut()), fake);
        assert!(slot.is_null());
    }

    #[test]
    fn null_host_pointers_are_tolerated_rather_than_dereferenced() {
        // SAFETY: every call is made with a null pointer, which each helper checks for
        // before touching a vtable.
        unsafe {
            assert_eq!(add_ref(core::ptr::null_mut()), 0);
            assert_eq!(release(core::ptr::null_mut()), 0);
            assert!(query_interface(core::ptr::null_mut(), &uid(1, 2, 3, 4)).is_null());
        }
    }
}
