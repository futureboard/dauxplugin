//! The platform window an editor draws into, and its conversion to and from
//! `raw-window-handle`.
//!
//! [`WindowTarget`] is the DAUx spelling of `DauxWindowV1` (`abi-v1` §11.4): a tagged
//! union of the four window systems the ABI names. It is deliberately *not* the same
//! type as `raw_window_handle::RawWindowHandle` — that enum is `#[non_exhaustive]` and
//! covers Android, UIKit, Web, DRM and more, none of which a plug-in editor is ever
//! parented into — but conversions in both directions are provided, because every GPU
//! crate in the ecosystem speaks `raw-window-handle`.
//!
//! ```
//! use daux_graphics::{WindowApi, WindowTarget};
//!
//! let target = WindowTarget::win32(0x1234).expect("non-null HWND");
//! assert_eq!(target.api(), WindowApi::Win32);
//! assert!(!target.is_null());
//! ```

use core::ffi::c_void;
use core::fmt;
use core::num::NonZeroIsize;
use core::ptr::NonNull;

use raw_window_handle::{
    AppKitWindowHandle, RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle,
    WaylandWindowHandle, Win32WindowHandle, XlibDisplayHandle, XlibWindowHandle,
};

use crate::bitset::define_bit_set;

/// `[any-thread]` The four window systems `abi-v1` §11.4 names.
///
/// The numeric values are the `DAUX_WINDOW_API_*` constants and are part of the binary
/// contract; they are duplicated here rather than imported because `daux-graphics` must
/// not depend on `daux-abi` (the format adapters sit between the two).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u32)]
pub enum WindowApi {
    /// Windows: the handle is an `HWND`.
    Win32 = 1,
    /// macOS: the handle is an `NSView *`.
    Cocoa = 2,
    /// X11: the handle is a `Window` id and the display is a `Display *`.
    X11 = 3,
    /// Wayland: the handle is a `wl_surface *` and the display a `wl_display *`.
    Wayland = 4,
}

impl WindowApi {
    /// `[any-thread]` The window API native to the platform this binary was built for.
    ///
    /// X11 is chosen for "unix that is not macOS" because it is the API every plug-in
    /// host on Linux can still provide, with or without XWayland; a host that prefers
    /// Wayland says so explicitly through [`WindowApiSet`].
    pub const PLATFORM: Self = if cfg!(target_os = "windows") {
        Self::Win32
    } else if cfg!(target_vendor = "apple") {
        Self::Cocoa
    } else {
        Self::X11
    };

    /// `[any-thread]` The `DAUX_WINDOW_API_*` value.
    #[must_use]
    pub const fn as_bits(self) -> u32 {
        self as u32
    }

    /// `[any-thread]` Reads a `DAUX_WINDOW_API_*` value, rejecting unknown ones.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            1 => Some(Self::Win32),
            2 => Some(Self::Cocoa),
            3 => Some(Self::X11),
            4 => Some(Self::Wayland),
            _ => None,
        }
    }

    /// `[any-thread]` A short, stable, lowercase name used in manifests and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Win32 => "win32",
            Self::Cocoa => "cocoa",
            Self::X11 => "x11",
            Self::Wayland => "wayland",
        }
    }

    /// `[any-thread]` Whether this API needs a separate display/connection pointer
    /// alongside the window handle.
    #[must_use]
    pub const fn needs_display(self) -> bool {
        matches!(self, Self::X11 | Self::Wayland)
    }
}

impl fmt::Display for WindowApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

define_bit_set!(
    /// `[any-thread]` A set of [`WindowApi`]s, as a host advertises.
    WindowApiSet: WindowApi {
        Win32 = 0,
        Cocoa = 1,
        X11 = 2,
        Wayland = 3,
    }
);

impl From<WindowApi> for WindowApiSet {
    fn from(value: WindowApi) -> Self {
        Self::only(value)
    }
}

/// `[main-thread]` The platform window an editor draws into or parents itself to.
///
/// # Safety and ownership
///
/// The handles are raw pointers the **host** owns. This crate never dereferences them;
/// it only carries them from the ABI boundary to whichever backend crate knows what to
/// do with them. Consequently `WindowTarget` is neither `Send` nor `Sync`: a window
/// handle is only valid on the thread that owns the window, which for every window
/// system here is the host's main thread (`abi-v1` §11.4 makes every GUI call
/// `[main-thread]` without exception).
///
/// The handle is valid only for as long as the host says it is — from
/// [`DauxGraphic::open`](crate::DauxGraphic::open) until
/// [`DauxGraphic::close`](crate::DauxGraphic::close) returns. Storing a copy that
/// outlives the editor is a use-after-free waiting to happen.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WindowTarget {
    /// A Windows `HWND`.
    Win32 {
        /// The `HWND`, as an opaque pointer.
        hwnd: *mut c_void,
    },
    /// A macOS `NSView *` (a view, never a window: hosts parent editors into views).
    Cocoa {
        /// The `NSView *`.
        ns_view: *mut c_void,
    },
    /// An X11 window id plus the `Display *` it belongs to.
    X11 {
        /// The X11 `Window` id, widened to 64 bits so the same struct works on every
        /// platform.
        window: u64,
        /// The Xlib `Display *`, or null when the host expects the default display.
        display: *mut c_void,
    },
    /// A Wayland `wl_surface *` plus its `wl_display *`.
    Wayland {
        /// The `wl_surface *`.
        surface: *mut c_void,
        /// The `wl_display *`.
        display: *mut c_void,
    },
}

impl WindowTarget {
    /// `[main-thread]` A Win32 target, rejecting a null `HWND`.
    #[must_use]
    pub fn win32(hwnd: isize) -> Option<Self> {
        (hwnd != 0).then_some(Self::Win32 {
            hwnd: hwnd as *mut c_void,
        })
    }

    /// `[main-thread]` A Cocoa target, rejecting a null `NSView *`.
    #[must_use]
    pub fn cocoa(ns_view: *mut c_void) -> Option<Self> {
        (!ns_view.is_null()).then_some(Self::Cocoa { ns_view })
    }

    /// `[main-thread]` An X11 target, rejecting window id `0`. A null `display` is
    /// allowed and means "the default display".
    #[must_use]
    pub fn x11(window: u64, display: *mut c_void) -> Option<Self> {
        (window != 0).then_some(Self::X11 { window, display })
    }

    /// `[main-thread]` A Wayland target, rejecting a null surface or display: unlike
    /// X11, Wayland has no default connection to fall back on.
    #[must_use]
    pub fn wayland(surface: *mut c_void, display: *mut c_void) -> Option<Self> {
        (!surface.is_null() && !display.is_null()).then_some(Self::Wayland { surface, display })
    }

    /// `[any-thread]` Which window system this target belongs to.
    #[must_use]
    pub const fn api(self) -> WindowApi {
        match self {
            Self::Win32 { .. } => WindowApi::Win32,
            Self::Cocoa { .. } => WindowApi::Cocoa,
            Self::X11 { .. } => WindowApi::X11,
            Self::Wayland { .. } => WindowApi::Wayland,
        }
    }

    /// `[any-thread]` Whether the window handle itself is null or zero.
    ///
    /// A null display is not "null" here: X11 tolerates a default display, and the
    /// constructors already reject the combinations that are genuinely unusable. This
    /// exists for targets built by hand from ABI structs, where a host may have sent a
    /// zeroed `DauxWindowV1`.
    #[must_use]
    pub fn is_null(self) -> bool {
        match self {
            Self::Win32 { hwnd } => hwnd.is_null(),
            Self::Cocoa { ns_view } => ns_view.is_null(),
            Self::X11 { window, .. } => window == 0,
            Self::Wayland { surface, .. } => surface.is_null(),
        }
    }

    /// `[main-thread]` The window handle as an opaque pointer.
    ///
    /// For [`X11`](Self::X11) this is the window id reinterpreted as a pointer, exactly
    /// as `DauxWindowV1::handle` carries it.
    #[must_use]
    pub fn handle_ptr(self) -> *mut c_void {
        match self {
            Self::Win32 { hwnd } => hwnd,
            Self::Cocoa { ns_view } => ns_view,
            Self::X11 { window, .. } => window as usize as *mut c_void,
            Self::Wayland { surface, .. } => surface,
        }
    }

    /// `[main-thread]` The display/connection pointer, or null when the API has none.
    #[must_use]
    pub fn display_ptr(self) -> *mut c_void {
        match self {
            Self::Win32 { .. } | Self::Cocoa { .. } => core::ptr::null_mut(),
            Self::X11 { display, .. } | Self::Wayland { display, .. } => display,
        }
    }

    /// `[main-thread]` Converts a `raw-window-handle` pair into a target.
    ///
    /// Returns `None` for any handle DAUx cannot parent an editor into (Android, UIKit,
    /// Web, DRM, GBM, Haiku, Orbital, OpenHarmony), and for an X11 or Wayland window
    /// whose matching display handle is missing — a surface without its connection is
    /// unusable, and guessing one is how compositor crashes start.
    ///
    /// Both XCB and Xlib window handles map onto [`WindowTarget::X11`]: an X11 `Window`
    /// id is the same integer either way, and the display pointer is taken from whichever
    /// display handle the host provided.
    #[must_use]
    pub fn from_raw_window_handle(
        window: RawWindowHandle,
        display: Option<RawDisplayHandle>,
    ) -> Option<Self> {
        match window {
            RawWindowHandle::Win32(handle) => Some(Self::Win32 {
                hwnd: handle.hwnd.get() as *mut c_void,
            }),
            RawWindowHandle::AppKit(handle) => Some(Self::Cocoa {
                ns_view: handle.ns_view.as_ptr(),
            }),
            RawWindowHandle::Xlib(handle) => Some(Self::X11 {
                // `c_ulong` is 32 bits on Windows and 64 elsewhere; `From` covers both.
                window: u64::from(handle.window),
                display: x11_display_ptr(display?)?,
            }),
            RawWindowHandle::Xcb(handle) => Some(Self::X11 {
                window: u64::from(handle.window.get()),
                display: x11_display_ptr(display?)?,
            }),
            RawWindowHandle::Wayland(handle) => match display? {
                RawDisplayHandle::Wayland(d) => Some(Self::Wayland {
                    surface: handle.surface.as_ptr(),
                    display: d.display.as_ptr(),
                }),
                _ => None,
            },
            _ => None,
        }
    }

    /// `[main-thread]` Converts back into a `raw-window-handle` window handle, for
    /// handing to wgpu, glutin or any other crate that speaks that vocabulary.
    ///
    /// Returns `None` when the handle is null or zero, because every
    /// `raw-window-handle` payload for these platforms is a `NonNull`/`NonZero` and a
    /// null one cannot be represented — which is exactly the check a caller would
    /// otherwise forget.
    ///
    /// X11 targets are reported as [`RawWindowHandle::Xlib`], matching the display
    /// handle produced by [`raw_display_handle`](Self::raw_display_handle).
    #[must_use]
    pub fn raw_window_handle(self) -> Option<RawWindowHandle> {
        match self {
            Self::Win32 { hwnd } => {
                let hwnd = NonZeroIsize::new(hwnd as isize)?;
                Some(RawWindowHandle::Win32(Win32WindowHandle::new(hwnd)))
            }
            Self::Cocoa { ns_view } => Some(RawWindowHandle::AppKit(AppKitWindowHandle::new(
                NonNull::new(ns_view)?,
            ))),
            Self::X11 { window, .. } => {
                if window == 0 {
                    return None;
                }
                // An X11 `Window` is an XID: 32 bits on the wire, carried in a
                // `c_ulong` by Xlib. Anything wider than 32 bits is not a valid XID.
                let id = u32::try_from(window).ok()?;
                Some(RawWindowHandle::Xlib(XlibWindowHandle::new(
                    core::ffi::c_ulong::from(id),
                )))
            }
            Self::Wayland { surface, .. } => Some(RawWindowHandle::Wayland(
                WaylandWindowHandle::new(NonNull::new(surface)?),
            )),
        }
    }

    /// `[main-thread]` The matching `raw-window-handle` display handle.
    ///
    /// Win32 and Cocoa have empty display handles, so those always succeed. X11 accepts
    /// a null `Display *` and reports it as "use the default display". Wayland requires
    /// a real `wl_display *` and returns `None` without one.
    #[must_use]
    pub fn raw_display_handle(self) -> Option<RawDisplayHandle> {
        match self {
            Self::Win32 { .. } => Some(RawDisplayHandle::Windows(
                raw_window_handle::WindowsDisplayHandle::new(),
            )),
            Self::Cocoa { .. } => Some(RawDisplayHandle::AppKit(
                raw_window_handle::AppKitDisplayHandle::new(),
            )),
            Self::X11 { display, .. } => Some(RawDisplayHandle::Xlib(XlibDisplayHandle::new(
                NonNull::new(display),
                0,
            ))),
            Self::Wayland { display, .. } => Some(RawDisplayHandle::Wayland(
                WaylandDisplayHandle::new(NonNull::new(display)?),
            )),
        }
    }

    /// `[main-thread]` Rebuilds a target from the raw fields of a `DauxWindowV1`
    /// (`abi-v1` §11.4).
    ///
    /// Returns `None` for an unknown `api` value or a handle that fails that API's
    /// validity rules, which is the check every adapter would otherwise write itself.
    #[must_use]
    pub fn from_abi_parts(api: u32, handle: *mut c_void, display: *mut c_void) -> Option<Self> {
        match WindowApi::from_bits(api)? {
            WindowApi::Win32 => Self::win32(handle as isize),
            WindowApi::Cocoa => Self::cocoa(handle),
            WindowApi::X11 => Self::x11(handle as usize as u64, display),
            WindowApi::Wayland => Self::wayland(handle, display),
        }
    }
}

/// Extracts an Xlib-style `Display *` from either X11 display handle, treating "no
/// pointer" as the default display (which is what both handles document).
fn x11_display_ptr(display: RawDisplayHandle) -> Option<*mut c_void> {
    match display {
        RawDisplayHandle::Xlib(d) => Some(d.display.map_or(core::ptr::null_mut(), NonNull::as_ptr)),
        RawDisplayHandle::Xcb(d) => {
            Some(d.connection.map_or(core::ptr::null_mut(), NonNull::as_ptr))
        }
        _ => None,
    }
}

impl fmt::Display for WindowTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Win32 { hwnd } => write!(f, "win32 hwnd {hwnd:p}"),
            Self::Cocoa { ns_view } => write!(f, "cocoa ns_view {ns_view:p}"),
            Self::X11 { window, display } => write!(f, "x11 window {window:#x} on {display:p}"),
            Self::Wayland { surface, display } => {
                write!(f, "wayland surface {surface:p} on {display:p}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the round-trip tests exercise the XCB path; XCB is never produced by
    // `raw_window_handle`, only accepted, so the main module has no use for these.
    use core::num::NonZeroU32;
    use raw_window_handle::XcbWindowHandle;

    /// A stand-in for a real window handle. Never dereferenced anywhere in this crate,
    /// which is precisely the property under test.
    fn fake_ptr(value: usize) -> *mut c_void {
        value as *mut c_void
    }

    #[test]
    fn api_values_match_the_abi_constants() {
        assert_eq!(WindowApi::Win32.as_bits(), 1);
        assert_eq!(WindowApi::Cocoa.as_bits(), 2);
        assert_eq!(WindowApi::X11.as_bits(), 3);
        assert_eq!(WindowApi::Wayland.as_bits(), 4);
        for api in WindowApiSet::ALL.iter() {
            assert_eq!(WindowApi::from_bits(api.as_bits()), Some(api));
        }
        assert_eq!(WindowApi::from_bits(0), None);
        assert_eq!(WindowApi::from_bits(5), None);
        assert_eq!(WindowApi::from_bits(u32::MAX), None);
        assert!(WindowApi::X11.needs_display());
        assert!(!WindowApi::Win32.needs_display());
        assert_eq!(WindowApi::Wayland.to_string(), "wayland");
    }

    #[test]
    fn constructors_reject_unusable_handles() {
        assert!(WindowTarget::win32(0).is_none());
        assert!(WindowTarget::win32(0x1234).is_some());
        assert!(WindowTarget::cocoa(core::ptr::null_mut()).is_none());
        assert!(WindowTarget::cocoa(fake_ptr(0x20)).is_some());
        assert!(WindowTarget::x11(0, fake_ptr(0x30)).is_none());
        assert!(
            WindowTarget::x11(42, core::ptr::null_mut()).is_some(),
            "X11 tolerates the default display"
        );
        assert!(
            WindowTarget::wayland(fake_ptr(0x40), core::ptr::null_mut()).is_none(),
            "Wayland has no default connection"
        );
        assert!(WindowTarget::wayland(core::ptr::null_mut(), fake_ptr(0x50)).is_none());
        assert!(WindowTarget::wayland(fake_ptr(0x40), fake_ptr(0x50)).is_some());
    }

    #[test]
    fn abi_parts_round_trip_for_every_platform() {
        let cases = [
            (WindowApi::Win32, fake_ptr(0x1000), core::ptr::null_mut()),
            (WindowApi::Cocoa, fake_ptr(0x2000), core::ptr::null_mut()),
            (WindowApi::X11, fake_ptr(0x3000), fake_ptr(0x3001)),
            (WindowApi::Wayland, fake_ptr(0x4000), fake_ptr(0x4001)),
        ];
        for (api, handle, display) in cases {
            let target = WindowTarget::from_abi_parts(api.as_bits(), handle, display)
                .unwrap_or_else(|| panic!("{api} should convert"));
            assert_eq!(target.api(), api);
            assert_eq!(target.handle_ptr(), handle);
            assert_eq!(target.display_ptr(), display);
            assert!(!target.is_null());
        }
    }

    #[test]
    fn abi_parts_reject_malformed_input() {
        assert!(WindowTarget::from_abi_parts(0, fake_ptr(1), core::ptr::null_mut()).is_none());
        assert!(WindowTarget::from_abi_parts(99, fake_ptr(1), core::ptr::null_mut()).is_none());
        assert!(
            WindowTarget::from_abi_parts(
                WindowApi::Win32.as_bits(),
                core::ptr::null_mut(),
                core::ptr::null_mut()
            )
            .is_none(),
            "a zeroed DauxWindowV1 must not become a target"
        );
        assert!(
            WindowTarget::from_abi_parts(
                WindowApi::Wayland.as_bits(),
                fake_ptr(1),
                core::ptr::null_mut()
            )
            .is_none()
        );
    }

    #[test]
    fn a_hand_built_null_target_reports_itself() {
        assert!(
            WindowTarget::Win32 {
                hwnd: core::ptr::null_mut()
            }
            .is_null()
        );
        assert!(WindowTarget::X11 {
            window: 0,
            display: fake_ptr(1)
        }
        .is_null());
        assert!(!WindowTarget::X11 {
            window: 7,
            display: core::ptr::null_mut()
        }
        .is_null());
    }

    #[test]
    fn win32_handles_convert_both_ways() {
        let target = WindowTarget::win32(0xDEAD).expect("non-null");
        let raw = target.raw_window_handle().expect("representable");
        match raw {
            RawWindowHandle::Win32(h) => assert_eq!(h.hwnd.get(), 0xDEAD),
            other => panic!("unexpected handle {other:?}"),
        }
        let back = WindowTarget::from_raw_window_handle(raw, target.raw_display_handle());
        assert_eq!(back, Some(target));
    }

    #[test]
    fn cocoa_handles_convert_both_ways() {
        let target = WindowTarget::cocoa(fake_ptr(0xBEEF)).expect("non-null");
        let raw = target.raw_window_handle().expect("representable");
        assert!(matches!(raw, RawWindowHandle::AppKit(_)));
        let back = WindowTarget::from_raw_window_handle(raw, target.raw_display_handle());
        assert_eq!(back, Some(target));
    }

    #[test]
    fn x11_handles_convert_both_ways_from_xlib_and_xcb() {
        let display = fake_ptr(0x77);
        let target = WindowTarget::x11(0x2A, display).expect("non-zero window");
        let raw = target.raw_window_handle().expect("representable");
        assert!(matches!(raw, RawWindowHandle::Xlib(_)));
        assert_eq!(
            WindowTarget::from_raw_window_handle(raw, target.raw_display_handle()),
            Some(target)
        );

        let xcb = RawWindowHandle::Xcb(XcbWindowHandle::new(
            NonZeroU32::new(0x2A).expect("non-zero"),
        ));
        let xcb_display = RawDisplayHandle::Xcb(raw_window_handle::XcbDisplayHandle::new(
            NonNull::new(display),
            0,
        ));
        assert_eq!(
            WindowTarget::from_raw_window_handle(xcb, Some(xcb_display)),
            Some(target),
            "xcb and xlib describe the same X11 window"
        );
    }

    #[test]
    fn wayland_handles_convert_both_ways() {
        let target = WindowTarget::wayland(fake_ptr(0x11), fake_ptr(0x22)).expect("non-null");
        let raw = target.raw_window_handle().expect("representable");
        assert!(matches!(raw, RawWindowHandle::Wayland(_)));
        assert_eq!(
            WindowTarget::from_raw_window_handle(raw, target.raw_display_handle()),
            Some(target)
        );
    }

    #[test]
    fn conversion_refuses_windows_daux_cannot_host() {
        let android = RawWindowHandle::AndroidNdk(raw_window_handle::AndroidNdkWindowHandle::new(
            NonNull::new(fake_ptr(1)).expect("non-null"),
        ));
        assert!(WindowTarget::from_raw_window_handle(android, None).is_none());

        let web = RawWindowHandle::Web(raw_window_handle::WebWindowHandle::new(1));
        assert!(WindowTarget::from_raw_window_handle(web, None).is_none());
    }

    #[test]
    fn conversion_refuses_a_surface_without_its_display() {
        let wayland = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(fake_ptr(0x11)).expect("non-null"),
        ));
        assert!(
            WindowTarget::from_raw_window_handle(wayland, None).is_none(),
            "a wl_surface without its wl_display is unusable"
        );
        assert!(
            WindowTarget::from_raw_window_handle(
                wayland,
                Some(RawDisplayHandle::Windows(
                    raw_window_handle::WindowsDisplayHandle::new()
                ))
            )
            .is_none(),
            "a mismatched display handle is not an answer either"
        );

        let xlib = RawWindowHandle::Xlib(XlibWindowHandle::new(7));
        assert!(WindowTarget::from_raw_window_handle(xlib, None).is_none());
    }

    #[test]
    fn xlib_accepts_the_default_display() {
        let xlib = RawWindowHandle::Xlib(XlibWindowHandle::new(7));
        let target = WindowTarget::from_raw_window_handle(
            xlib,
            Some(RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0))),
        );
        assert_eq!(
            target,
            Some(WindowTarget::X11 {
                window: 7,
                display: core::ptr::null_mut()
            })
        );
    }

    #[test]
    fn null_targets_cannot_be_expressed_as_raw_handles() {
        assert!(
            WindowTarget::Win32 {
                hwnd: core::ptr::null_mut()
            }
            .raw_window_handle()
            .is_none()
        );
        assert!(
            WindowTarget::Cocoa {
                ns_view: core::ptr::null_mut()
            }
            .raw_window_handle()
            .is_none()
        );
        assert!(
            WindowTarget::X11 {
                window: 0,
                display: core::ptr::null_mut()
            }
            .raw_window_handle()
            .is_none()
        );
        assert!(
            WindowTarget::X11 {
                window: u64::from(u32::MAX) + 1,
                display: core::ptr::null_mut()
            }
            .raw_window_handle()
            .is_none(),
            "an XID wider than 32 bits is not an XID"
        );
        assert!(
            WindowTarget::Wayland {
                surface: core::ptr::null_mut(),
                display: fake_ptr(1)
            }
            .raw_window_handle()
            .is_none()
        );
        assert!(
            WindowTarget::Wayland {
                surface: fake_ptr(1),
                display: core::ptr::null_mut()
            }
            .raw_display_handle()
            .is_none()
        );
    }

    #[test]
    fn the_platform_api_is_one_of_the_four() {
        assert!(WindowApiSet::ALL.contains(WindowApi::PLATFORM));
        #[cfg(target_os = "windows")]
        assert_eq!(WindowApi::PLATFORM, WindowApi::Win32);
    }

    #[test]
    fn display_form_names_the_api() {
        let target = WindowTarget::win32(0x10).expect("non-null");
        assert!(target.to_string().starts_with("win32 hwnd "));
        assert!(
            WindowTarget::x11(0x2A, core::ptr::null_mut())
                .expect("valid")
                .to_string()
                .starts_with("x11 window 0x2a")
        );
    }
}
