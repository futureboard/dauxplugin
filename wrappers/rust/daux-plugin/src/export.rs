//! The bridge from the safe `DauxPlugin` trait to the raw C ABI.
//!
//! These generic functions are monomorphized per plugin type by the
//! [`crate::daux_export_plugin!`] macro. They are `unsafe extern "C"` because
//! the host calls them directly through the vtable / factory. Plugin authors
//! never call anything here by hand.

use core::ffi::{c_char, c_void};

use daux_plugin_sys as sys;

use crate::buffer::{Buffers, ProcessContext};
use crate::DauxPlugin;

/// Copy a `&str` into a fixed-size C char buffer, NUL-terminating and
/// truncating as needed.
fn write_cstr(dst: &mut [c_char], s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(dst.len().saturating_sub(1));
    for (i, b) in bytes.iter().take(n).enumerate() {
        dst[i] = *b as c_char;
    }
    dst[n] = 0;
}

/// Build the raw descriptor from the trait's `descriptor()`.
pub fn build_descriptor<P: DauxPlugin>() -> sys::daux_plugin_descriptor {
    let d = P::descriptor();
    let mut raw: sys::daux_plugin_descriptor = unsafe { core::mem::zeroed() };
    write_cstr(&mut raw.id, &d.id);
    write_cstr(&mut raw.name, &d.name);
    write_cstr(&mut raw.vendor, &d.vendor);
    raw.version = sys::daux_make_version(d.version.0, d.version.1, d.version.2);
    raw.category = d.category.to_raw();
    raw.capabilities = d.caps.0;
    raw.audio_input_buses = d.audio_input_buses;
    raw.audio_output_buses = d.audio_output_buses;
    raw.default_channels_per_bus = d.default_channels_per_bus;
    raw
}

/// Build the per-instance vtable, wiring every entry to a monomorphized thunk.
pub fn build_vtable<P: DauxPlugin>() -> sys::daux_plugin_vtable {
    sys::daux_plugin_vtable {
        struct_size: core::mem::size_of::<sys::daux_plugin_vtable>() as u32,
        prepare: Some(prepare::<P>),
        activate: Some(activate::<P>),
        deactivate: Some(deactivate::<P>),
        reset: Some(reset::<P>),
        process: Some(process::<P>),
        get_parameter_count: Some(get_parameter_count::<P>),
        get_parameter_info: Some(get_parameter_info::<P>),
        get_parameter_value: Some(get_parameter_value::<P>),
        set_parameter_value: Some(set_parameter_value::<P>),
        // Linear params: leave conversions null so the host uses its default
        // normalized<->plain linear mapping. TODO: expose via the trait.
        normalized_to_plain: None,
        plain_to_normalized: None,
        format_parameter: None,
        parse_parameter: None,
        get_state: Some(get_state::<P>),
        set_state: Some(set_state::<P>),
        // Editor from Rust (GPUI) is a future milestone.
        create_editor: None,
        destroy_editor: None,
        reserved: [core::ptr::null_mut(); 8],
    }
}

/// Allocate a plugin instance and fill the out-instance struct.
///
/// # Safety
/// `out` must be a valid, writable `daux_plugin_instance`. `vt` must outlive
/// the instance (it is a `'static` vtable owned by the macro).
pub unsafe fn create_instance<P: DauxPlugin>(
    _host: *const sys::daux_host_callbacks,
    out: *mut sys::daux_plugin_instance,
    vt: *const sys::daux_plugin_vtable,
) -> sys::daux_result {
    if out.is_null() {
        return sys::DAUX_ERR_INVALID_ARG;
    }
    // TODO: stash the host callbacks in the instance so the plugin can notify
    // the host of parameter changes from a future editor.
    let boxed = Box::new(P::new());
    (*out).handle = Box::into_raw(boxed) as *mut c_void;
    (*out).vtable = vt;
    sys::DAUX_OK
}

/// Destroy a previously created instance.
///
/// # Safety
/// `h` must be a handle returned by `create_instance::<P>`.
pub unsafe fn destroy_instance<P: DauxPlugin>(h: sys::daux_plugin_handle) -> sys::daux_result {
    if !h.is_null() {
        drop(Box::from_raw(h as *mut P));
    }
    sys::DAUX_OK
}

// --- handle casting helpers -------------------------------------------------
#[inline]
unsafe fn as_ref<'a, P>(h: sys::daux_plugin_handle) -> &'a P {
    &*(h as *const P)
}
#[inline]
unsafe fn as_mut<'a, P>(h: sys::daux_plugin_handle) -> &'a mut P {
    &mut *(h as *mut P)
}

// --- vtable thunks ----------------------------------------------------------
unsafe extern "C" fn prepare<P: DauxPlugin>(
    h: sys::daux_plugin_handle,
    sample_rate: f64,
    max_block: u32,
) -> sys::daux_result {
    as_mut::<P>(h).prepare(sample_rate, max_block);
    sys::DAUX_OK
}

unsafe extern "C" fn activate<P: DauxPlugin>(h: sys::daux_plugin_handle) -> sys::daux_result {
    as_mut::<P>(h).activate();
    sys::DAUX_OK
}

unsafe extern "C" fn deactivate<P: DauxPlugin>(h: sys::daux_plugin_handle) -> sys::daux_result {
    as_mut::<P>(h).deactivate();
    sys::DAUX_OK
}

unsafe extern "C" fn reset<P: DauxPlugin>(h: sys::daux_plugin_handle) -> sys::daux_result {
    as_mut::<P>(h).reset();
    sys::DAUX_OK
}

unsafe extern "C" fn process<P: DauxPlugin>(
    h: sys::daux_plugin_handle,
    data: *const sys::daux_process_data,
) -> sys::daux_result {
    if data.is_null() {
        return sys::DAUX_ERR_INVALID_ARG;
    }
    let data_ref = &*data;
    let ctx = if data_ref.context.is_null() {
        // Minimal fallback context.
        ProcessContext {
            sample_rate: 0.0,
            block_size: data_ref.num_frames,
            timeline_samples: 0,
            tempo_bpm: 120.0,
            time_sig: (4, 4),
            is_playing: false,
            is_recording: false,
            is_looping: false,
        }
    } else {
        ProcessContext::from_raw(&*data_ref.context)
    };
    let mut buffers = Buffers::from_raw(data_ref);
    as_mut::<P>(h).process(&ctx, &mut buffers);
    sys::DAUX_OK
}

unsafe extern "C" fn get_parameter_count<P: DauxPlugin>(_h: sys::daux_plugin_handle) -> u32 {
    P::parameters().len() as u32
}

unsafe extern "C" fn get_parameter_info<P: DauxPlugin>(
    _h: sys::daux_plugin_handle,
    index: u32,
    out: *mut sys::daux_param_info,
) -> sys::daux_result {
    if out.is_null() {
        return sys::DAUX_ERR_INVALID_ARG;
    }
    let params = P::parameters();
    let Some(pi) = params.get(index as usize) else {
        return sys::DAUX_ERR_NOT_FOUND;
    };
    let mut info: sys::daux_param_info = core::mem::zeroed();
    info.id = pi.id;
    write_cstr(&mut info.name, &pi.name);
    write_cstr(&mut info.short_name, &pi.short_name);
    write_cstr(&mut info.unit, &pi.unit);
    info.default_value = pi.default;
    info.min_value = pi.min;
    info.max_value = pi.max;
    info.step_count = pi.step_count;
    info.flags = pi.flags.0;
    *out = info;
    sys::DAUX_OK
}

unsafe extern "C" fn get_parameter_value<P: DauxPlugin>(
    h: sys::daux_plugin_handle,
    id: sys::daux_param_id,
    out: *mut f64,
) -> sys::daux_result {
    if out.is_null() {
        return sys::DAUX_ERR_INVALID_ARG;
    }
    *out = as_ref::<P>(h).get_param(id);
    sys::DAUX_OK
}

unsafe extern "C" fn set_parameter_value<P: DauxPlugin>(
    h: sys::daux_plugin_handle,
    id: sys::daux_param_id,
    value: f64,
) -> sys::daux_result {
    as_mut::<P>(h).set_param(id, value);
    sys::DAUX_OK
}

unsafe extern "C" fn get_state<P: DauxPlugin>(
    h: sys::daux_plugin_handle,
    out_buf: *mut c_void,
    buf_size: u32,
    out_size: *mut u32,
) -> sys::daux_result {
    let blob = as_ref::<P>(h).get_state();
    if !out_size.is_null() {
        *out_size = blob.len() as u32;
    }
    if out_buf.is_null() || (buf_size as usize) < blob.len() {
        return sys::DAUX_ERR_BUFFER_TOO_SMALL;
    }
    core::ptr::copy_nonoverlapping(blob.as_ptr(), out_buf as *mut u8, blob.len());
    sys::DAUX_OK
}

unsafe extern "C" fn set_state<P: DauxPlugin>(
    h: sys::daux_plugin_handle,
    data: *const c_void,
    size: u32,
) -> sys::daux_result {
    if data.is_null() {
        return sys::DAUX_ERR_INVALID_ARG;
    }
    let slice = core::slice::from_raw_parts(data as *const u8, size as usize);
    if as_mut::<P>(h).set_state(slice) {
        sys::DAUX_OK
    } else {
        sys::DAUX_ERR_INVALID_STATE
    }
}
