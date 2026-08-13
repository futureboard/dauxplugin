//! Borrowed string views and fixed text buffers (`abi-v1` §2, §2.1).
//!
//! Two text representations cross the boundary and only two:
//!
//! * [`DauxStrView`] — borrowed UTF-8 passed *into* a call, valid only for that call.
//! * [`DauxName`] / [`DauxText`] / [`DauxId`] / [`DauxPath`] — fixed-size, NUL-padded
//!   UTF-8 buffers the callee fills in caller-owned memory.
//!
//! Neither form ever transfers ownership of an allocation, so there is no cross-module
//! `free` (`abi-v1` §16.2).

use core::ffi::c_void;
use core::ptr;

/// Capacity of a [`DauxName`] in bytes.
pub const DAUX_NAME_SIZE: usize = 64;
/// Capacity of a [`DauxText`] in bytes.
pub const DAUX_TEXT_SIZE: usize = 256;
/// Capacity of a [`DauxPath`] in bytes.
pub const DAUX_PATH_SIZE: usize = 1024;
/// Capacity of a [`DauxId`] in bytes.
pub const DAUX_ID_SIZE: usize = 128;

/// Borrowed UTF-8 text. **Not** NUL-terminated; `len` is a byte count.
///
/// A `DauxStrView` passed as an argument is valid only for the duration of the call.
/// `ptr` MAY be null if and only if `len == 0`.
///
/// The type deliberately carries no lifetime: it is a raw view over memory owned by the
/// caller, exactly like the C structure it mirrors.
///
/// [any-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxStrView {
    /// Pointer to the first byte, or null when `len == 0`.
    pub ptr: *const u8,
    /// Length in bytes.
    pub len: usize,
}

impl DauxStrView {
    /// [any-thread] The empty view: null pointer, zero length.
    #[inline]
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            ptr: ptr::null(),
            len: 0,
        }
    }

    /// [any-thread] Borrows `s`.
    ///
    /// The caller is responsible for keeping `s` alive for as long as the view is used;
    /// the returned value does not borrow-check.
    #[inline]
    #[must_use]
    // The specification and `crate-contracts.md` both name this constructor `from_str`;
    // it is not `FromStr::from_str` (no `Result`, no owned output).
    #[allow(clippy::should_implement_trait)]
    pub const fn from_str(s: &str) -> Self {
        Self {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }

    /// [any-thread] `true` when the view carries no bytes.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// [any-thread] Length in bytes.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// [any-thread] Borrows the bytes described by the view.
    ///
    /// Returns `None` when `ptr` is null and `len != 0`, which is a malformed view.
    ///
    /// # Safety
    ///
    /// The caller guarantees that:
    ///
    /// * `ptr` points to `len` consecutive initialised bytes, or `len == 0`;
    /// * that memory stays allocated, immutable and unaliased-by-writers for `'a`;
    /// * `'a` does not outlive the call the view was passed to (`abi-v1` §16.3);
    /// * `len` does not exceed `isize::MAX` and `ptr + len` does not wrap the address
    ///   space — both are guaranteed by any producer that derived the view from a real
    ///   Rust or C string.
    #[inline]
    #[must_use]
    pub unsafe fn as_bytes<'a>(self) -> Option<&'a [u8]> {
        if self.len == 0 {
            return Some(&[]);
        }
        if self.ptr.is_null() {
            return None;
        }
        // SAFETY: `len != 0` and `ptr` is non-null here. The caller's contract above
        // guarantees the pointed-to region holds `len` initialised bytes that stay valid,
        // immutable and unaliased for `'a`, and that the region does not wrap the address
        // space. The lifetime is chosen by the caller precisely because the ABI cannot
        // express it.
        Some(unsafe { core::slice::from_raw_parts(self.ptr, self.len) })
    }

    /// [any-thread] Borrows the view as `&str`.
    ///
    /// Returns `None` when the view is malformed (null pointer with non-zero length) or
    /// the bytes are not valid UTF-8. It never panics.
    ///
    /// # Safety
    ///
    /// Identical to [`DauxStrView::as_bytes`].
    #[inline]
    #[must_use]
    pub unsafe fn as_str<'a>(self) -> Option<&'a str> {
        // SAFETY: forwarded verbatim from this function's own safety contract, which is
        // defined to be the one `as_bytes` requires.
        let bytes = unsafe { self.as_bytes() }?;
        core::str::from_utf8(bytes).ok()
    }
}

/// [any-thread] Reinterprets a `*const c_void` extension table pointer.
///
/// A convenience for the `get_extension` entries of `abi-v1` §5, §7 and §11.6, which are
/// specified as returning `*const c_void`.
///
/// # Safety
///
/// The caller guarantees that `ptr` is either null or points to a live, correctly aligned
/// `T` owned by the providing module, and that `T` is the table type the extension id
/// names. The reference must not outlive the object that provides the table
/// (`abi-v1` §16.1).
#[inline]
#[must_use]
pub unsafe fn extension_table<'a, T>(ptr: *const c_void) -> Option<&'a T> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null was just checked. The caller guarantees the pointee is a live,
    // aligned `T` owned by the providing module and outliving `'a`; extension tables are
    // immutable for as long as their owner lives, so a shared reference cannot race.
    Some(unsafe { &*ptr.cast::<T>() })
}

/// Defines one of the fixed-size, NUL-padded UTF-8 buffers of `abi-v1` §2.1.
macro_rules! fixed_buffer {
    (
        $(#[$meta:meta])*
        $name:ident, $cap:ident
    ) => {
        $(#[$meta])*
        ///
        /// The buffer is NUL-padded and *not* necessarily NUL-terminated when full.
        /// Writers truncate on a UTF-8 character boundary; readers stop at the first NUL
        /// and tolerate invalid UTF-8 without panicking. Trailing NUL bytes are not part
        /// of the value.
        ///
        /// [any-thread]
        #[repr(C)]
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub [u8; $cap]);

        impl $name {
            /// [any-thread] Capacity of the buffer in bytes.
            pub const CAPACITY: usize = $cap;

            /// [any-thread] An all-zero buffer.
            #[inline]
            #[must_use]
            pub const fn empty() -> Self {
                Self([0u8; $cap])
            }

            /// [any-thread] Copies `s`, truncating on a UTF-8 character boundary.
            ///
            /// Never allocates and never panics. Truncation can only remove whole
            /// characters, so the result is always valid UTF-8.
            #[inline]
            #[must_use]
            pub fn new(s: &str) -> Self {
                let mut out = Self::empty();
                out.set(s);
                out
            }

            /// [any-thread] Replaces the contents with `s`, truncating on a UTF-8
            /// character boundary and NUL-padding the remainder.
            pub fn set(&mut self, s: &str) {
                self.0 = [0u8; $cap];
                let mut end = if s.len() < $cap { s.len() } else { $cap };
                while end > 0 && !s.is_char_boundary(end) {
                    end -= 1;
                }
                self.0[..end].copy_from_slice(&s.as_bytes()[..end]);
            }

            /// [any-thread] Clears the buffer to all zeros.
            #[inline]
            pub fn clear(&mut self) {
                self.0 = [0u8; $cap];
            }

            /// [any-thread] Number of bytes before the first NUL, i.e. the stored length.
            #[inline]
            #[must_use]
            pub const fn len(&self) -> usize {
                let mut i = 0;
                while i < $cap {
                    if self.0[i] == 0 {
                        return i;
                    }
                    i += 1;
                }
                $cap
            }

            /// [any-thread] `true` when the buffer stores no bytes.
            #[inline]
            #[must_use]
            pub const fn is_empty(&self) -> bool {
                self.0[0] == 0
            }

            /// [any-thread] `true` when the value fills the buffer, so it may have been
            /// truncated by the writer.
            #[inline]
            #[must_use]
            pub const fn is_full(&self) -> bool {
                self.len() == $cap
            }

            /// [any-thread] The stored bytes, up to but not including the first NUL.
            #[inline]
            #[must_use]
            pub fn as_bytes(&self) -> &[u8] {
                self.0.get(..self.len()).unwrap_or(&[])
            }

            /// [any-thread] The stored text.
            ///
            /// Reading stops at the first NUL. If the bytes are not valid UTF-8 — which a
            /// conforming writer never produces, but a hostile or buggy module might — the
            /// longest valid prefix is returned. This never allocates and never panics.
            #[must_use]
            pub fn as_str(&self) -> &str {
                let bytes = self.as_bytes();
                match core::str::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(e) => {
                        let valid = bytes.get(..e.valid_up_to()).unwrap_or(&[]);
                        core::str::from_utf8(valid).unwrap_or("")
                    }
                }
            }

            /// [any-thread] Borrows the whole raw buffer, NUL padding included.
            #[inline]
            #[must_use]
            pub const fn as_raw(&self) -> &[u8; $cap] {
                &self.0
            }
        }

        impl Default for $name {
            #[inline]
            fn default() -> Self {
                Self::empty()
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.as_str()).finish()
            }
        }
    };
}

fixed_buffer! {
    /// A short UTF-8 name, at most [`DAUX_NAME_SIZE`] bytes.
    DauxName, DAUX_NAME_SIZE
}

fixed_buffer! {
    /// A UTF-8 text field, at most [`DAUX_TEXT_SIZE`] bytes.
    DauxText, DAUX_TEXT_SIZE
}

fixed_buffer! {
    /// A stable reverse-DNS identifier, at most [`DAUX_ID_SIZE`] bytes.
    DauxId, DAUX_ID_SIZE
}

fixed_buffer! {
    /// A filesystem path in UTF-8, at most [`DAUX_PATH_SIZE`] bytes.
    DauxPath, DAUX_PATH_SIZE
}
