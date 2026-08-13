//! `daux.gui/1` — editor lifecycle (`abi-v1` §11.4).
//!
//! All GUI calls are **[main-thread]**, without exception. Sizes are physical pixels;
//! `set_scale` reports the HiDPI factor that maps logical to physical units.

use core::ffi::c_void;

use crate::compat::{impl_abi_default, impl_abi_struct};
use crate::handle::DauxPluginHandle;
use crate::status::{DauxBool, DauxStatus};

/// Win32 `HWND`.
pub const DAUX_WINDOW_API_WIN32: u32 = 1;
/// Cocoa `NSView*`.
pub const DAUX_WINDOW_API_COCOA: u32 = 2;
/// X11 `Window`.
pub const DAUX_WINDOW_API_X11: u32 = 3;
/// Wayland `wl_surface*`.
pub const DAUX_WINDOW_API_WAYLAND: u32 = 4;

/// A parent window handed to the plug-in's editor.
///
/// [main-thread]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxWindowV1 {
    /// `size_of::<DauxWindowV1>()` as written by the producer.
    pub size: u32,
    /// One of the `DAUX_WINDOW_API_*` constants.
    pub api: u32,
    /// `HWND` / `NSView*` / X11 `Window` (as `usize`) / `wl_surface*`.
    pub handle: *mut c_void,
    /// X11 `Display*` / `wl_display*`, else null.
    pub display: *mut c_void,
}

impl DauxWindowV1 {
    /// [main-thread] An all-zero window with `size` set.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            size: Self::SIZE,
            api: 0,
            handle: core::ptr::null_mut(),
            display: core::ptr::null_mut(),
        }
    }
}

impl_abi_struct!(DauxWindowV1);
impl_abi_default!(DauxWindowV1);

/// Function table of the `daux.gui/1` extension.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DauxGuiApiV1 {
    /// `size_of::<DauxGuiApiV1>()` as written by the producer.
    pub size: u32,
    /// Reserved for alignment; MUST be zero.
    pub _pad0: u32,

    /// Whether the plug-in can host an editor for `api` in the requested mode.
    /// [main-thread]
    pub is_api_supported:
        unsafe extern "C" fn(p: DauxPluginHandle, api: u32, is_floating: DauxBool) -> DauxBool,

    /// Creates the editor. [main-thread]
    pub create:
        unsafe extern "C" fn(p: DauxPluginHandle, api: u32, is_floating: DauxBool) -> DauxStatus,

    /// Destroys the editor. The DSP side is unaffected. [main-thread]
    pub destroy: unsafe extern "C" fn(p: DauxPluginHandle),

    /// Reports the HiDPI scale factor; null when the plug-in ignores it. [main-thread]
    pub set_scale: Option<unsafe extern "C" fn(p: DauxPluginHandle, scale: f64) -> DauxStatus>,

    /// Reads the editor size in physical pixels. [main-thread]
    pub get_size:
        unsafe extern "C" fn(p: DauxPluginHandle, width: *mut u32, height: *mut u32) -> DauxStatus,

    /// Whether the editor can be resized by the host. [main-thread]
    pub can_resize: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxBool,

    /// Rounds a proposed size to one the editor accepts; null when any size is accepted.
    /// [main-thread]
    pub adjust_size: Option<
        unsafe extern "C" fn(p: DauxPluginHandle, width: *mut u32, height: *mut u32) -> DauxStatus,
    >,

    /// Applies a new editor size in physical pixels. [main-thread]
    pub set_size: unsafe extern "C" fn(p: DauxPluginHandle, width: u32, height: u32) -> DauxStatus,

    /// Embeds the editor in the host's window. [main-thread]
    pub set_parent:
        unsafe extern "C" fn(p: DauxPluginHandle, window: *const DauxWindowV1) -> DauxStatus,

    /// Makes the editor visible. [main-thread]
    pub show: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxStatus,

    /// Hides the editor without destroying it. [main-thread]
    pub hide: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxStatus,

    /// Reserved for future minor revisions; MUST be all zero.
    pub reserved: [usize; 6],
}

impl_abi_struct!(DauxGuiApiV1);
