//! `daux.gui/1` — editor lifecycle (abi-v1 §11.4).
//!
//! Every entry here is `[main-thread]`, without exception, and none of them touches DSP state:
//! an editor may be created and destroyed a hundred times while audio never stops, which is
//! rule 9 of `CLAUDE.md` and the reason [`EditorState`](crate::instance::EditorState) is a
//! separate struct from everything else in an instance.
//!
//! # Mapping onto [`DauxGraphic`](daux_plugin_api::DauxGraphic)
//!
//! | ABI | This adapter |
//! |---|---|
//! | `create` | builds the editor object; **no window exists yet** |
//! | `set_parent` | negotiates a profile and calls `open` with the host's window |
//! | `set_size` | `resize` |
//! | `set_scale` | `scale_factor_changed` |
//! | `show` / `hide` | accepted and remembered; a DAUx editor is visible as soon as it is parented |
//! | `destroy` | `close`, then the editor is dropped |
//!
//! Splitting `create` from `set_parent` is what the ABI's own ordering requires — the host
//! calls `create`, asks for a size, then hands over a window — and it is also what lets
//! `get_size` answer before any GPU resource exists.
//!
//! # Floating windows
//!
//! `is_floating` is refused. A floating editor owns a top-level window, and nothing in
//! `daux-graphics` creates one: an editor draws into the window the host gives it. Refusing is
//! honest; every host that asks also supports the embedded path, because abi-v1 §11.4 makes
//! `set_parent` non-optional.

use daux_abi::{
    DAUX_ERR_GRAPHICS, DAUX_ERR_INVALID_STATE, DAUX_ERR_UNSUPPORTED, DAUX_OK, DauxBool,
    DauxGuiApiV1, DauxPluginHandle, DauxStatus, DauxWindowV1, daux_bool, daux_bool_is_true,
};
use daux_plugin_api::{
    GraphicContext, GraphicDescriptor, HostGraphicCaps, PhysicalSize, ScaleFactor, WindowApi,
    WindowApiSet, WindowTarget,
};

use crate::instance::{AxtInstance, with_instance};
use crate::panic::Refusal;

/// The size an editor gets when it has not been asked to be any particular size yet.
fn preferred_size(descriptor: &GraphicDescriptor, scale: ScaleFactor) -> PhysicalSize {
    descriptor.preferred_size.to_physical(scale)
}

/// The scale in force, defaulting to 1.0 before the host reports one.
fn scale_of(state: &AxtInstance) -> ScaleFactor {
    state
        .editor
        .scale
        .unwrap_or_else(|| ScaleFactor::new_clamped(1.0))
}

/// [main-thread] Whether the plug-in can host an editor for `api` in the requested mode.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn is_api_supported(
    p: DauxPluginHandle,
    api: u32,
    is_floating: DauxBool,
) -> DauxBool {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            if daux_bool_is_true(is_floating) {
                return daux_bool(false);
            }
            let supported = WindowApi::from_bits(api).is_some_and(|api| api == WindowApi::PLATFORM);
            let has_gui = state
                .descriptor
                .as_ref()
                .is_some_and(|d| d.capabilities.is_has_gui());
            daux_bool(supported && has_gui)
        })
    }
}

/// [main-thread] Creates the editor object.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn create(p: DauxPluginHandle, api: u32, is_floating: DauxBool) -> DauxStatus {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            if daux_bool_is_true(is_floating) {
                return DAUX_ERR_UNSUPPORTED;
            }
            let Some(api) = WindowApi::from_bits(api) else {
                return DauxStatus::INVALID_ARG;
            };
            if api != WindowApi::PLATFORM {
                return DAUX_ERR_UNSUPPORTED;
            }
            if state.editor.editor.is_some() {
                // The ABI has no "recreate": the host must destroy first.
                return DAUX_ERR_INVALID_STATE;
            }
            match state.instance.create_editor() {
                // A headless plug-in. The host asked anyway, which is not an error on its
                // part — `daux.gui/1` is only advertised when the descriptor claims a GUI.
                Ok(None) => DAUX_ERR_UNSUPPORTED,
                Ok(Some(editor)) => {
                    let scale = scale_of(state);
                    let size = preferred_size(&editor.descriptor(), scale);
                    state.editor.editor = Some(editor);
                    state.editor.api = Some(api);
                    state.editor.size = Some(size);
                    state.editor.open = false;
                    DAUX_OK
                }
                Err(err) => crate::panic::status_of_error(&err),
            }
        })
    }
}

/// [main-thread] Destroys the editor. The processor is untouched.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn destroy(p: DauxPluginHandle) {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            if let Some(mut editor) = state.editor.editor.take() {
                // `close` is documented idempotent, so calling it for an editor that was
                // created but never parented is allowed and expected.
                editor.close();
            }
            state.editor.open = false;
            state.editor.api = None;
            // The size and scale survive: a host that reopens an editor should get the same
            // window back, not the descriptor's default.
        });
    }
}

/// [main-thread] Reports the HiDPI scale factor.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn set_scale(p: DauxPluginHandle, scale: f64) -> DauxStatus {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            let Some(scale) = ScaleFactor::new(scale) else {
                // A zero, negative or NaN scale would make every size derived from it
                // nonsense; refusing is better than rendering into nothing.
                return DauxStatus::INVALID_ARG;
            };
            state.editor.scale = Some(scale);
            if let Some(editor) = state.editor.editor.as_mut() {
                editor.scale_factor_changed(scale);
            }
            DAUX_OK
        })
    }
}

/// [main-thread] The editor size in physical pixels.
///
/// # Safety
///
/// `width` and `height` are null or point at writable, aligned `u32`s. See [`with_instance`].
unsafe extern "C" fn get_size(
    p: DauxPluginHandle,
    width: *mut u32,
    height: *mut u32,
) -> DauxStatus {
    let body = |state: &mut AxtInstance| {
        if width.is_null() || height.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        let scale = scale_of(state);
        let size = match (state.editor.size, state.editor.editor.as_ref()) {
            (Some(size), _) => size,
            (None, Some(editor)) => preferred_size(&editor.descriptor(), scale),
            (None, None) => return DAUX_ERR_INVALID_STATE,
        };
        // SAFETY: both pointers are non-null and, per this function's contract, writable and
        // aligned.
        unsafe {
            width.write(size.width);
            height.write(size.height);
        }
        DAUX_OK
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// [main-thread] Whether the host may resize the editor.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn can_resize(p: DauxPluginHandle) -> DauxBool {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            daux_bool(
                state
                    .editor
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.descriptor().resizable),
            )
        })
    }
}

/// [main-thread] Rounds a proposed size to one the editor accepts.
///
/// # Safety
///
/// `width` and `height` are null or point at readable, writable, aligned `u32`s. See
/// [`with_instance`].
unsafe extern "C" fn adjust_size(
    p: DauxPluginHandle,
    width: *mut u32,
    height: *mut u32,
) -> DauxStatus {
    let body = |state: &mut AxtInstance| {
        if width.is_null() || height.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        let scale = scale_of(state);
        let Some(editor) = state.editor.editor.as_ref() else {
            return DAUX_ERR_INVALID_STATE;
        };
        // SAFETY: both pointers are non-null and, per this function's contract, readable and
        // aligned.
        let proposed = unsafe { PhysicalSize::new(width.read(), height.read()) };
        let clamped = editor
            .descriptor()
            .clamp(proposed.to_logical(scale))
            .to_physical(scale);
        // SAFETY: as above, for writing.
        unsafe {
            width.write(clamped.width);
            height.write(clamped.height);
        }
        DAUX_OK
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// [main-thread] Applies a new editor size in physical pixels.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn set_size(p: DauxPluginHandle, width: u32, height: u32) -> DauxStatus {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            let size = PhysicalSize::new(width, height);
            if size.is_empty() {
                return DauxStatus::INVALID_ARG;
            }
            let Some(editor) = state.editor.editor.as_mut() else {
                return DAUX_ERR_INVALID_STATE;
            };
            match editor.resize(size) {
                Ok(()) => {
                    state.editor.size = Some(size);
                    DAUX_OK
                }
                // The host keeps the previous size, which is why `state.editor.size` is only
                // updated on success.
                Err(_) => DAUX_ERR_GRAPHICS,
            }
        })
    }
}

/// [main-thread] Embeds the editor in the host's window.
///
/// This is where the editor is really opened: the ABI hands over the window here and nowhere
/// else, and `DauxGraphic::open` needs one.
///
/// # Safety
///
/// `window` is null or points at a readable [`DauxWindowV1`] whose handle stays valid until
/// `destroy`. See [`with_instance`].
unsafe extern "C" fn set_parent(p: DauxPluginHandle, window: *const DauxWindowV1) -> DauxStatus {
    let body = |state: &mut AxtInstance| {
        if window.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        // SAFETY: non-null was checked and this function's contract guarantees the structure is
        // readable for the call. It is copied out; the *handle* inside stays the host's.
        let window = unsafe { *window };
        if !window.is_v1_0_compatible() {
            return daux_abi::DAUX_ERR_ABI_MISMATCH;
        }
        let Some(target) = WindowTarget::from_abi_parts(window.api, window.handle, window.display)
        else {
            return DauxStatus::INVALID_ARG;
        };
        // The host hands over one window API, so that is the only one to negotiate over.
        let Some(api) = WindowApi::from_bits(window.api) else {
            return DauxStatus::INVALID_ARG;
        };

        let AxtInstance { editor, host, .. } = state;
        if editor.open {
            // `open` is called at most once without an intervening `close`.
            return DAUX_ERR_INVALID_STATE;
        }
        let scale = editor
            .scale
            .unwrap_or_else(|| ScaleFactor::new_clamped(1.0));
        let Some(graphic) = editor.editor.as_mut() else {
            return DAUX_ERR_INVALID_STATE;
        };
        let size = editor
            .size
            .unwrap_or_else(|| preferred_size(&graphic.descriptor(), scale));

        let host_caps = HostGraphicCaps::in_process().with_window_apis(WindowApiSet::only(api));
        let Some(profile) = graphic.capabilities().negotiate_with_fallback(&host_caps) else {
            // The editor offers nothing this host can composite. A plug-in must always keep an
            // embedded-surface fallback (abi-v1 §13), so this is a plug-in bug.
            return DAUX_ERR_GRAPHICS;
        };

        let mut ctx = GraphicContext::new(target, size, scale, profile, host.services());
        match graphic.open(&mut ctx) {
            Ok(()) => {
                editor.open = true;
                editor.size = Some(size);
                DAUX_OK
            }
            // "On failure the editor must leave nothing behind", so nothing is recorded and the
            // host may try again or destroy.
            Err(_) => DAUX_ERR_GRAPHICS,
        }
    };
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, body) }
}

/// [main-thread] Makes the editor visible.
///
/// A DAUx editor draws into the host's window and is visible as soon as it is parented, so
/// this succeeds once an editor exists and does nothing otherwise.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn show(p: DauxPluginHandle) -> DauxStatus {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            if state.editor.editor.is_some() {
                DAUX_OK
            } else {
                DAUX_ERR_INVALID_STATE
            }
        })
    }
}

/// [main-thread] Hides the editor without destroying it. See [`show`].
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn hide(p: DauxPluginHandle) -> DauxStatus {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            if state.editor.editor.is_some() {
                DAUX_OK
            } else {
                DAUX_ERR_INVALID_STATE
            }
        })
    }
}

/// The `daux.gui/1` table, offered only by instances whose descriptor advertises a GUI.
pub(crate) static TABLE: DauxGuiApiV1 = DauxGuiApiV1 {
    size: DauxGuiApiV1::SIZE,
    _pad0: 0,
    is_api_supported,
    create,
    destroy,
    set_scale: Some(set_scale),
    get_size,
    can_resize,
    adjust_size: Some(adjust_size),
    set_size,
    set_parent,
    show,
    hide,
    reserved: [0; 6],
};
