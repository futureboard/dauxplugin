//! `IPlugView`: the host's window, the plug-in's editor, and the wire between them.
//!
//! # Lifetime
//!
//! A view is created by `IEditController::createView`, lives as a separately reference-counted
//! COM object, and is released by the host when the window closes. It holds **one reference
//! to the component**, so a host that releases the plug-in before its editor — which happens
//! — cannot free the object the view is still calling into.
//!
//! The editor itself belongs to the component, not to the view: it is built once during
//! `initialize` and reused, and `attached`/`removed` bracket each use as
//! [`DauxGraphic::open`](daux_plugin_api::DauxGraphic::open)/[`close`]. That is what makes
//! rule 9 hold — the editor can be opened and closed any number of times while the processor
//! keeps running, and nothing in this file touches DSP state.
//!
//! [`close`]: daux_plugin_api::DauxGraphic::close
//!
//! # Coordinates
//!
//! VST3's `ViewRect` is in physical pixels on Windows and Linux and in points on macOS;
//! DAUx's [`GraphicDescriptor`](daux_plugin_api::GraphicDescriptor) is in logical units. The
//! two agree at a scale factor of 1, which is what this adapter assumes: VST3 reports the
//! display's scale through `IPlugViewContentScaleSupport`, which is not implemented here, so
//! a HiDPI host shows the editor at its logical size.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use daux_plugin_api::{
    GraphicContext, GraphicProfile, HostServices, InputEvent, InputResponse, Key, LogicalPoint,
    LogicalSize, Modifiers, PhysicalSize, ScaleFactor, WindowTarget,
};

use crate::api::{IPlugFrameVtbl, IPlugViewVtbl, ViewRect};
use crate::com::{Char16, FidString, HostPtr, TBool, TResult, TUid, iid_eq, result};
use crate::component::Vst3Component;
use crate::guard::Poison;
use crate::strings;

/// The platform window type this build of the adapter accepts.
#[must_use]
pub const fn platform_type() -> &'static [u8] {
    #[cfg(target_os = "windows")]
    {
        crate::api::PLATFORM_TYPE_HWND
    }
    #[cfg(target_os = "macos")]
    {
        crate::api::PLATFORM_TYPE_NSVIEW
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        crate::api::PLATFORM_TYPE_X11
    }
}

/// The part of a view the UI thread mutates.
///
/// Behind an `UnsafeCell` for the same reason the component's halves are: every method here
/// takes `&Self` so that the poison guard can be read while the body mutates, which a plain
/// `&mut Self` would forbid.
struct ViewState {
    /// `true` between `attached` and `removed`.
    attached: bool,
    /// The size the host last agreed to, in physical pixels.
    size: PhysicalSize,
    /// The services the editor sees, kept alive for as long as the view is.
    host: HostServices,
}

/// One editor window.
#[repr(C)]
pub struct Vst3View {
    vtbl: *const IPlugViewVtbl,
    ref_count: AtomicU32,
    poison: Poison,
    /// An **owned** reference to the component, released when the view is.
    component: *mut Vst3Component,
    /// An **owned** reference to the host's frame, or null.
    frame: HostPtr,
    state: UnsafeCell<ViewState>,
}

static VIEW_VTBL: IPlugViewVtbl = IPlugViewVtbl {
    query_interface: Vst3View::query_interface,
    add_ref: Vst3View::add_ref,
    release: Vst3View::release,
    is_platform_type_supported: Vst3View::is_platform_type_supported,
    attached: Vst3View::attached,
    removed: Vst3View::removed,
    on_wheel: Vst3View::on_wheel,
    on_key_down: Vst3View::on_key_down,
    on_key_up: Vst3View::on_key_up,
    get_size: Vst3View::get_size,
    on_size: Vst3View::on_size,
    on_focus: Vst3View::on_focus,
    set_frame: Vst3View::set_frame,
    can_resize: Vst3View::can_resize,
    check_size_constraint: Vst3View::check_size_constraint,
};

impl Vst3View {
    /// `[main-thread]` Builds a view onto `component`'s editor.
    ///
    /// The returned pointer carries one reference the host owns, and the view carries one
    /// reference to the component.
    ///
    /// # Safety
    ///
    /// `component` must be a live [`Vst3Component`] whose editor is present and not already
    /// held by another view — which `createView` has just checked.
    #[must_use]
    pub unsafe fn create(component: *mut Vst3Component) -> *mut c_void {
        // SAFETY: the caller promises a live component.
        let owner = unsafe { &*component };
        let size = owner
            .with_editor_descriptor()
            .map_or(PhysicalSize::new(640, 480), |d| {
                logical_to_physical(d.preferred_size)
            });
        let host = owner.services();

        // The view owns a reference to the component: a host that releases the plug-in while
        // its window is open must not free the object the view is about to call into.
        // SAFETY: `component` is live, and its `IComponent` head is its COM identity.
        unsafe { crate::com::add_ref(Vst3Component::as_com(component)) };

        let view = Box::new(Self {
            vtbl: &raw const VIEW_VTBL,
            ref_count: AtomicU32::new(1),
            poison: Poison::new(),
            component,
            frame: HostPtr::null(),
            state: UnsafeCell::new(ViewState {
                attached: false,
                size,
                host,
            }),
        });
        Box::into_raw(view).cast::<c_void>()
    }

    /// # Safety
    ///
    /// `this` must be a live pointer returned by [`Vst3View::create`].
    unsafe fn from_this<'a>(this: *mut c_void) -> &'a Self {
        // SAFETY: the vtable is the first field, so the head's address is the object's. The
        // borrow is shared; everything mutable is an atomic or behind the `UnsafeCell`.
        unsafe { &*this.cast::<Self>() }
    }

    /// `[main-thread]` The mutable half.
    ///
    /// # Safety
    ///
    /// The caller must be inside an `IPlugView` method. VST3 drives one view from the UI
    /// thread only, and nothing else reaches this state.
    #[allow(clippy::mut_from_ref)]
    unsafe fn state(&self) -> &mut ViewState {
        // SAFETY: see the method's contract.
        unsafe { &mut *self.state.get() }
    }

    /// The component this view belongs to.
    fn owner(&self) -> &Vst3Component {
        // SAFETY: the view holds a reference to the component for its whole life, so it
        // cannot have been freed.
        unsafe { &*self.component }
    }

    /// `[main-thread]` Asks the host's frame to resize the window a view is in.
    ///
    /// Returns `false` when there is no view, no frame, or the host refuses.
    #[must_use]
    pub fn request_resize(component: &Vst3Component, width: u32, height: u32) -> bool {
        let view = component.current_view();
        if view.is_null() {
            return false;
        }
        // SAFETY: `current_view` is non-null only while a view is open, and a view outlives
        // the window it belongs to.
        let view = unsafe { &*view };
        let frame = view.frame.get();
        if frame.is_null() {
            return false;
        }
        // SAFETY: `frame` is an owned reference this view holds, so it is alive.
        let vtbl = unsafe { *frame.cast::<*const IPlugFrameVtbl>() };
        if vtbl.is_null() {
            return false;
        }
        let mut rect = ViewRect::sized(
            i32::try_from(width).unwrap_or(i32::MAX),
            i32::try_from(height).unwrap_or(i32::MAX),
        );
        // SAFETY: `rect` is a live local the host reads during the call; the `IPlugView` the
        // frame was given is this view's own head, which is its address.
        let status = unsafe {
            ((*vtbl).resize_view)(
                frame,
                core::ptr::from_ref(view).cast_mut().cast::<c_void>(),
                &raw mut rect,
            )
        };
        if result::is_ok(status) {
            // SAFETY: a UI-thread call, like every other path into a view.
            unsafe { view.state() }.size = PhysicalSize::new(width, height);
            true
        } else {
            false
        }
    }

    // ---- FUnknown --------------------------------------------------------------------

    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        iid: *const TUid,
        obj: *mut *mut c_void,
    ) -> TResult {
        if this.is_null() || obj.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: `obj` was checked non-null and is caller-owned.
        unsafe { *obj = core::ptr::null_mut() };
        // SAFETY: `iid` is the host's; `iid_eq` tolerates null.
        let wanted = unsafe {
            iid_eq(iid, &crate::api::IPLUG_VIEW_IID) || iid_eq(iid, &crate::api::IFUNKNOWN_IID)
        };
        if !wanted {
            return result::NO_INTERFACE;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.ref_count.fetch_add(1, Ordering::AcqRel);
        // SAFETY: `obj` was checked non-null.
        unsafe { *obj = this };
        result::OK
    }

    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        if this.is_null() {
            return 0;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.ref_count.fetch_add(1, Ordering::AcqRel) + 1
    }

    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        if this.is_null() {
            return 0;
        }
        // SAFETY: a live view the caller owns a reference to.
        let me = unsafe { Self::from_this(this) };
        let remaining = me.ref_count.fetch_sub(1, Ordering::AcqRel) - 1;
        if remaining > 0 {
            return remaining;
        }

        // Last reference: close the editor if the host forgot to, give the frame back, tell
        // the component its editor is free again, and only then drop.
        // SAFETY: a UI-thread call.
        let state = unsafe { me.state() };
        if state.attached {
            // SAFETY: the UI thread owns the editor while a view holds it open.
            if let Some(editor) = unsafe { me.owner().editor() } {
                editor.close();
            }
            state.attached = false;
        }
        let frame = me.frame.swap(core::ptr::null_mut());
        // SAFETY: the view owns one reference to the frame, taken in `set_frame`.
        unsafe { crate::com::release(frame) };

        let component = me.component;
        // SAFETY: this view is the one holding the editor open.
        unsafe { (*component).release_view() };
        // SAFETY: `this` came from `Box::into_raw` in `create`, and this is the last
        // reference to it.
        drop(unsafe { Box::from_raw(this.cast::<Self>()) });
        // SAFETY: the view owned one reference to the component; this gives it back. It is
        // the last thing done because it may free the component.
        unsafe { crate::com::release(Vst3Component::as_com(component)) };
        0
    }

    // ---- IPlugView -------------------------------------------------------------------

    unsafe extern "system" fn is_platform_type_supported(
        this: *mut c_void,
        platform: FidString,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: `platform` is null or a null-terminated host string.
        let supported = unsafe { strings::c_str_eq(platform, platform_type()) };
        result::from_bool(supported)
    }

    unsafe extern "system" fn attached(
        this: *mut c_void,
        parent: *mut c_void,
        platform: FidString,
    ) -> TResult {
        if this.is_null() || parent.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            // SAFETY: `platform` is null or a null-terminated host string.
            if !unsafe { strings::c_str_eq(platform, platform_type()) } {
                return result::INVALID_ARGUMENT;
            }
            // SAFETY: a UI-thread call.
            let state = unsafe { me.state() };
            if state.attached {
                return result::NOT_INITIALIZED;
            }
            let Some(target) = window_target(parent) else {
                return result::INVALID_ARGUMENT;
            };

            // SAFETY: a UI-thread call; the view holds the editor open.
            let Some(editor) = (unsafe { me.owner().editor() }) else {
                return result::NOT_INITIALIZED;
            };
            let profile = editor
                .descriptor()
                .capabilities
                .profiles()
                .first()
                .copied()
                .unwrap_or_else(fallback_profile);
            let mut ctx = GraphicContext::new(
                target,
                state.size,
                ScaleFactor::new_clamped(1.0),
                profile,
                &state.host,
            );
            match editor.open(&mut ctx) {
                Ok(()) => {
                    state.attached = true;
                    result::OK
                }
                Err(_) => result::INTERNAL_ERROR,
            }
        })
    }

    unsafe extern "system" fn removed(this: *mut c_void) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            // SAFETY: a UI-thread call.
            let state = unsafe { me.state() };
            if !state.attached {
                return result::OK;
            }
            // SAFETY: a UI-thread call; the view holds the editor open.
            if let Some(editor) = unsafe { me.owner().editor() } {
                editor.close();
            }
            state.attached = false;
            result::OK
        })
    }

    unsafe extern "system" fn on_wheel(this: *mut c_void, distance: f32) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            me.deliver(&InputEvent::Scroll {
                position: LogicalPoint::new(0.0, 0.0),
                delta_x: 0.0,
                delta_y: f64::from(distance),
                modifiers: Modifiers::NONE,
            })
        })
    }

    unsafe extern "system" fn on_key_down(
        this: *mut c_void,
        key: Char16,
        code: i16,
        modifiers: i16,
    ) -> TResult {
        // SAFETY: `this` is null or a live view; `key` checks.
        unsafe { Self::key(this, key, code, modifiers, true) }
    }

    unsafe extern "system" fn on_key_up(
        this: *mut c_void,
        key: Char16,
        code: i16,
        modifiers: i16,
    ) -> TResult {
        // SAFETY: `this` is null or a live view; `key` checks.
        unsafe { Self::key(this, key, code, modifiers, false) }
    }

    /// # Safety
    ///
    /// `this` must be null or a live view.
    unsafe fn key(
        this: *mut c_void,
        character: Char16,
        code: i16,
        modifiers: i16,
        pressed: bool,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: the caller promises a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            let event = InputEvent::Key {
                key: key_from_vst3(character, code),
                pressed,
                repeat: false,
                modifiers: modifiers_from_vst3(modifiers),
            };
            me.deliver(&event)
        })
    }

    unsafe extern "system" fn get_size(this: *mut c_void, rect: *mut ViewRect) -> TResult {
        if this.is_null() || rect.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            // SAFETY: a UI-thread call.
            let size = unsafe { me.state() }.size;
            let out = ViewRect::sized(
                i32::try_from(size.width).unwrap_or(i32::MAX),
                i32::try_from(size.height).unwrap_or(i32::MAX),
            );
            // SAFETY: `rect` was checked non-null and is caller-owned.
            unsafe { *rect = out };
            result::OK
        })
    }

    unsafe extern "system" fn on_size(this: *mut c_void, rect: *mut ViewRect) -> TResult {
        if this.is_null() || rect.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            // SAFETY: `rect` was checked non-null and the host owns it for the call.
            let rect = unsafe { *rect };
            let size = PhysicalSize::new(
                u32::try_from(rect.width()).unwrap_or(0),
                u32::try_from(rect.height()).unwrap_or(0),
            );
            if size.is_empty() {
                return result::INVALID_ARGUMENT;
            }
            // SAFETY: a UI-thread call.
            unsafe { me.state() }.size = size;
            // SAFETY: a UI-thread call; the view holds the editor open.
            let Some(editor) = (unsafe { me.owner().editor() }) else {
                return result::OK;
            };
            match editor.resize(size) {
                Ok(()) => result::OK,
                Err(_) => result::INTERNAL_ERROR,
            }
        })
    }

    unsafe extern "system" fn on_focus(this: *mut c_void, state: TBool) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison
            .call(|| me.deliver(&InputEvent::Focus(state != 0)))
    }

    unsafe extern "system" fn set_frame(this: *mut c_void, frame: *mut c_void) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            // Retain before swapping and release afterwards, so being handed the same frame
            // twice cannot free it in between.
            // SAFETY: `frame` is null or a live COM object the host is handing over.
            unsafe { crate::com::add_ref(frame) };
            let previous = me.frame.swap(frame);
            // SAFETY: `previous` was retained by an earlier `set_frame`.
            unsafe { crate::com::release(previous) };
            result::OK
        })
    }

    unsafe extern "system" fn can_resize(this: *mut c_void) -> TResult {
        if this.is_null() {
            return result::FALSE;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison.call_value(result::FALSE, || {
            let resizable = me
                .owner()
                .with_editor_descriptor()
                .is_some_and(|d| d.resizable);
            result::from_bool(resizable)
        })
    }

    unsafe extern "system" fn check_size_constraint(
        this: *mut c_void,
        rect: *mut ViewRect,
    ) -> TResult {
        if this.is_null() || rect.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live view.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            // SAFETY: `rect` was checked non-null and the host owns it for the call.
            let proposed = unsafe { *rect };
            let Some(descriptor) = me.owner().with_editor_descriptor() else {
                return result::FALSE;
            };
            let clamped = descriptor.clamp(LogicalSize::new(
                f64::from(proposed.width()),
                f64::from(proposed.height()),
            ));
            let out = ViewRect::sized(clamped.width as i32, clamped.height as i32);
            // SAFETY: `rect` was checked non-null.
            unsafe { *rect = out };
            result::from_bool(out == proposed)
        })
    }

    /// Hands one input event to the editor.
    fn deliver(&self, event: &InputEvent) -> TResult {
        // SAFETY: every caller is an `IPlugView` method, which VST3 makes UI-thread-only.
        if !unsafe { self.state() }.attached {
            return result::FALSE;
        }
        // SAFETY: as above; the view holds the editor open.
        let Some(editor) = (unsafe { self.owner().editor() }) else {
            return result::FALSE;
        };
        match editor.on_input(event) {
            InputResponse::Consumed => result::OK,
            InputResponse::Ignored => result::FALSE,
        }
    }
}

/// The profile used when an editor advertises none, which no real backend does.
fn fallback_profile() -> GraphicProfile {
    use daux_plugin_api::{GraphicFramework, GraphicRenderer, PresentationMode};
    GraphicProfile::new(
        GraphicFramework::Custom,
        GraphicRenderer::Software,
        PresentationMode::EmbeddedSurface,
    )
}

/// The DAUx window handle for a VST3 parent pointer on this platform.
fn window_target(parent: *mut c_void) -> Option<WindowTarget> {
    #[cfg(target_os = "windows")]
    {
        WindowTarget::win32(parent as isize)
    }
    #[cfg(target_os = "macos")]
    {
        WindowTarget::cocoa(parent)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        // VST3 passes an X11 window id in a pointer-sized field, not a pointer.
        WindowTarget::x11(parent as u64, core::ptr::null_mut())
    }
}

/// A logical editor size as physical pixels, at the scale factor this adapter assumes.
fn logical_to_physical(size: LogicalSize) -> PhysicalSize {
    PhysicalSize::new(
        size.width.max(1.0).round() as u32,
        size.height.max(1.0).round() as u32,
    )
}

/// VST3's `KeyCode` as a DAUx key.
///
/// VST3 sends a character *and* a virtual key code; the code wins when it names a key, and
/// the character is used otherwise so that a plain letter still arrives.
fn key_from_vst3(character: Char16, code: i16) -> Key {
    // `pluginterfaces/base/keycodes.h`.
    match code {
        1 => Key::Backspace,
        2 => Key::Tab,
        6 => Key::Enter,
        7 => Key::PageUp,
        8 => Key::PageDown,
        9 => Key::End,
        10 => Key::Home,
        11 => Key::ArrowLeft,
        12 => Key::ArrowUp,
        13 => Key::ArrowRight,
        14 => Key::ArrowDown,
        16 => Key::Escape,
        17 => Key::Delete,
        _ => match character {
            0x20 => Key::Space,
            0x0D => Key::Enter,
            0x1B => Key::Escape,
            0x09 => Key::Tab,
            0x08 => Key::Backspace,
            other => Key::Unknown(u32::from(other)),
        },
    }
}

/// VST3's `KeyModifier` bits as DAUx modifiers.
fn modifiers_from_vst3(bits: i16) -> Modifiers {
    // `kShiftKey = 1<<0, kAlternateKey = 1<<1, kCommandKey = 1<<2, kControlKey = 1<<3`.
    Modifiers {
        shift: bits & (1 << 0) != 0,
        alt: bits & (1 << 1) != 0,
        meta: bits & (1 << 2) != 0,
        ctrl: bits & (1 << 3) != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_type_is_the_one_this_build_can_embed_into() {
        let expected: &[u8] = if cfg!(target_os = "windows") {
            b"HWND\0"
        } else if cfg!(target_os = "macos") {
            b"NSView\0"
        } else {
            b"X11EmbedWindowID\0"
        };
        assert_eq!(platform_type(), expected);
    }

    #[test]
    fn named_keys_beat_the_character_and_unknown_ones_fall_back_to_it() {
        assert_eq!(key_from_vst3(0, 6), Key::Enter);
        assert_eq!(key_from_vst3(0, 16), Key::Escape);
        assert_eq!(key_from_vst3(0, 11), Key::ArrowLeft);
        // No virtual code: the character decides.
        assert_eq!(key_from_vst3(0x20, 0), Key::Space);
        assert_eq!(key_from_vst3(u16::from(b'a'), 0), Key::Unknown(0x61));
    }

    #[test]
    fn modifier_bits_land_on_the_right_keys() {
        assert_eq!(modifiers_from_vst3(0), Modifiers::NONE);
        let all = modifiers_from_vst3(0b1111);
        assert!(all.shift && all.alt && all.meta && all.ctrl);
        let shift_only = modifiers_from_vst3(1);
        assert!(shift_only.shift && !shift_only.ctrl && !shift_only.alt && !shift_only.meta);
        // VST3's "command" bit is Meta, not Control: swapping them makes every macOS
        // shortcut in an editor fire on the wrong key.
        let command = modifiers_from_vst3(1 << 2);
        assert!(command.meta && !command.ctrl);
    }

    #[test]
    fn a_logical_size_never_becomes_an_empty_window() {
        assert_eq!(
            logical_to_physical(LogicalSize::new(640.0, 480.0)),
            PhysicalSize::new(640, 480)
        );
        assert_eq!(
            logical_to_physical(LogicalSize::new(0.0, 0.0)),
            PhysicalSize::new(1, 1)
        );
        assert_eq!(
            logical_to_physical(LogicalSize::new(-5.0, 200.4)),
            PhysicalSize::new(1, 200)
        );
    }
}
