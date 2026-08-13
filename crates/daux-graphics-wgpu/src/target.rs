//! Turning a [`WindowTarget`] into something wgpu can create a surface on.

use daux_graphics::{DauxGraphicResult, GraphicError, GraphicErrorKind, WindowTarget};

/// [main-thread] Converts a host window into a wgpu surface target.
///
/// # What this checks
///
/// `wgpu` takes raw handles and dereferences them inside the driver, so this is the last place
/// a bad handle can be caught cheaply. Two things are rejected here:
///
/// * a null or zero window handle — a zeroed `DauxWindowV1` from a host that failed to fill it
///   in, which would otherwise reach the driver as a null `HWND`;
/// * a window whose API needs a display connection that was not supplied — a `wl_surface`
///   without its `wl_display`, which is a compositor crash rather than an error.
///
/// # Errors
///
/// [`GraphicErrorKind::WindowApi`] for a handle wgpu cannot be given.
///
/// # Safety of the result
///
/// The returned [`wgpu::SurfaceTargetUnsafe`] carries raw pointers the **host** owns. Creating
/// a surface from it is `unsafe` for exactly one reason: the window must outlive the surface.
/// `GraphicContext` documents the host's window as valid from `open` until `close` returns, so
/// a surface created here must be dropped no later than `close`.
pub fn surface_target(target: WindowTarget) -> DauxGraphicResult<wgpu::SurfaceTargetUnsafe> {
    if target.is_null() {
        return Err(GraphicError::new_static(
            GraphicErrorKind::WindowApi,
            "the host provided a null window handle",
        ));
    }
    let raw_window_handle = target.raw_window_handle().ok_or_else(|| {
        GraphicError::new(
            GraphicErrorKind::WindowApi,
            format!("the host's {} window cannot be rendered into", target.api()),
        )
    })?;
    let raw_display_handle = target.raw_display_handle().ok_or_else(|| {
        GraphicError::new(
            GraphicErrorKind::WindowApi,
            format!(
                "the host's {} window came without the display connection it needs",
                target.api()
            ),
        )
    })?;

    Ok(wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(raw_display_handle),
        raw_window_handle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::c_void;

    /// A stand-in for a real window handle. Never dereferenced: this module only classifies
    /// and forwards handles, which is precisely the property these tests rely on.
    fn fake(value: usize) -> *mut c_void {
        value as *mut c_void
    }

    #[test]
    fn a_win32_window_becomes_a_raw_handle_target() {
        let target = surface_target(WindowTarget::win32(0x1234).expect("non-null"))
            .expect("a valid HWND is a surface target");
        match target {
            wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_window_handle,
                raw_display_handle,
            } => {
                assert!(matches!(
                    raw_window_handle,
                    raw_window_handle::RawWindowHandle::Win32(_)
                ));
                assert!(
                    raw_display_handle.is_some(),
                    "Win32 has an empty but present display handle"
                );
            }
            _ => panic!("expected a raw-handle target"),
        }
    }

    #[test]
    fn every_window_api_daux_names_converts() {
        let targets = [
            WindowTarget::win32(0x10).expect("valid"),
            WindowTarget::cocoa(fake(0x20)).expect("valid"),
            WindowTarget::x11(0x30, fake(0x31)).expect("valid"),
            WindowTarget::wayland(fake(0x40), fake(0x41)).expect("valid"),
        ];
        for target in targets {
            assert!(
                surface_target(target).is_ok(),
                "{} should convert to a surface target",
                target.api()
            );
        }
    }

    #[test]
    fn a_zeroed_window_struct_is_refused_instead_of_reaching_the_driver() {
        // A host that failed to fill in `DauxWindowV1` sends zeros. Passing a null HWND to
        // wgpu is a crash inside the driver, with a stack trace that blames the plug-in.
        let null = WindowTarget::Win32 {
            hwnd: core::ptr::null_mut(),
        };
        let err = surface_target(null).expect_err("a null HWND must be refused");
        assert_eq!(err.kind(), GraphicErrorKind::WindowApi);

        let null_view = WindowTarget::Cocoa {
            ns_view: core::ptr::null_mut(),
        };
        assert!(surface_target(null_view).is_err());

        let no_window = WindowTarget::X11 {
            window: 0,
            display: fake(1),
        };
        assert!(surface_target(no_window).is_err());
    }

    #[test]
    fn a_wayland_surface_without_its_display_is_refused() {
        // Guessing a `wl_display` is how compositor crashes start; there is no default
        // connection to fall back on.
        let orphan = WindowTarget::Wayland {
            surface: fake(0x11),
            display: core::ptr::null_mut(),
        };
        let err = surface_target(orphan).expect_err("a surface without a display is unusable");
        assert_eq!(err.kind(), GraphicErrorKind::WindowApi);
        assert!(
            err.message().contains("display"),
            "the message should say what is missing: {}",
            err.message()
        );
    }

    #[test]
    fn an_x11_window_id_wider_than_an_xid_is_refused() {
        // An XID is 32 bits. A wider value came from somewhere that was not X11, and
        // truncating it would name a different window — possibly one belonging to another
        // process.
        let bogus = WindowTarget::X11 {
            window: u64::from(u32::MAX) + 1,
            display: core::ptr::null_mut(),
        };
        let err = surface_target(bogus).expect_err("that is not an XID");
        assert_eq!(err.kind(), GraphicErrorKind::WindowApi);
    }

    #[test]
    fn an_x11_window_may_use_the_default_display() {
        // Unlike Wayland, X11 has a default display, and a null pointer means exactly that.
        let target = WindowTarget::X11 {
            window: 0x2A,
            display: core::ptr::null_mut(),
        };
        assert!(surface_target(target).is_ok());
    }
}
