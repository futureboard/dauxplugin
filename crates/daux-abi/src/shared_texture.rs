//! `com.futureboard.daux.shared-texture/1` — GPU surface hand-off (`abi-v1` §13).
//!
//! The extension lets a plug-in render its editor into a GPU surface the host composites
//! directly, instead of a nested native child window.
//!
//! Negotiation is mandatory: the host advertises the handle kinds it can import, the
//! plug-in picks one or declines, and **both sides MUST have a working native-window
//! fallback**. A plug-in MUST NOT require this extension in order to show an editor.

use core::ffi::c_void;

use crate::compat::{impl_abi_default, impl_abi_struct};

/// `HANDLE` obtained from `IDXGIResource1`.
pub const DAUX_TEXTURE_HANDLE_D3D11_SHARED: u32 = 1;
/// Direct3D 12 shared heap handle.
pub const DAUX_TEXTURE_HANDLE_D3D12_HEAP: u32 = 2;
/// macOS `IOSurfaceRef`.
pub const DAUX_TEXTURE_HANDLE_IOSURFACE: u32 = 3;
/// Linux DMA-BUF file descriptor.
pub const DAUX_TEXTURE_HANDLE_DMABUF: u32 = 4;
/// Vulkan external memory file descriptor.
pub const DAUX_TEXTURE_HANDLE_VULKAN_FD: u32 = 5;
/// Vulkan external memory Win32 handle.
pub const DAUX_TEXTURE_HANDLE_VULKAN_WIN32: u32 = 6;

/// A GPU surface shared between the plug-in and the host.
///
/// ABI v1.0 does not enumerate the `DAUX_TEXTURE_FORMAT_*` values `format` names: the two
/// sides agree on a format during negotiation and MUST treat unrecognised values as
/// "decline" rather than guess. `fence_kind` is likewise negotiated.
///
/// [main-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxSharedTextureV1 {
    /// `size_of::<DauxSharedTextureV1>()` as written by the producer.
    pub size: u32,
    /// One of the `DAUX_TEXTURE_HANDLE_*` constants.
    pub handle_kind: u32,
    /// The shared handle itself, interpreted according to `handle_kind`.
    pub handle: *mut c_void,
    /// Negotiated `DAUX_TEXTURE_FORMAT_*` value.
    pub format: u32,
    /// Surface width in pixels.
    pub width: u32,
    /// Surface height in pixels.
    pub height: u32,
    /// Bytes per row, `0` when the importer should derive it.
    pub row_pitch: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,
    /// Optional cross-API synchronisation primitive; null when unused.
    pub fence: *mut c_void,
    /// Negotiated kind of `fence`; `0` when unused.
    pub fence_kind: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad1: u32,
    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 6],
}

impl DauxSharedTextureV1 {
    /// [main-thread] An all-zero descriptor with `size` set: no handle, no fence.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        // SAFETY: every field is a plain integer, an array of `usize`, or a raw pointer
        // (`handle`/`fence`) for which null is the specified "absent" value. No field is a
        // reference, function pointer or enum, so the all-zero bit pattern is a valid,
        // fully initialised value. Zeroing also clears the four bytes of implicit padding
        // this layout carries before `fence`, so the bytes crossing the boundary are
        // deterministic.
        let mut this: Self = unsafe { core::mem::zeroed() };
        this.size = Self::SIZE;
        this
    }
}

impl_abi_struct!(DauxSharedTextureV1);
impl_abi_default!(DauxSharedTextureV1);
