//! Raw FFI bindings to the **DAUx Plugin C ABI**.
//!
//! Hand-written (not bindgen) so the crate has no build dependencies. Layout is
//! audited against `Core/include/DAUx/DAUx.h` and the modular headers under
//! `Core/include/DAUx/{Abi,Audio,Plugin,Parameter,State,Editor,Host}/`.
//! If you change a C header, update this file in lockstep.
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

use core::ffi::{c_char, c_void};

// ===========================================================================
// ABI version (DAUx/Abi/Version.h)
// ===========================================================================
pub const DAUX_ABI_VERSION_MAJOR: u32 = 1;
pub const DAUX_ABI_VERSION_MINOR: u32 = 0;
pub const DAUX_ABI_VERSION_PATCH: u32 = 0;

#[inline]
pub const fn daux_make_version(maj: u32, min: u32, pat: u32) -> u32 {
    ((maj & 0xFF) << 24) | ((min & 0xFF) << 16) | (pat & 0xFFFF)
}
#[inline]
pub const fn daux_version_major(v: u32) -> u32 { (v >> 24) & 0xFF }
#[inline]
pub const fn daux_version_minor(v: u32) -> u32 { (v >> 16) & 0xFF }
#[inline]
pub const fn daux_version_patch(v: u32) -> u32 { v & 0xFFFF }

pub const DAUX_ABI_VERSION: u32 =
    daux_make_version(DAUX_ABI_VERSION_MAJOR, DAUX_ABI_VERSION_MINOR, DAUX_ABI_VERSION_PATCH);

// Fixed inline-string sizes.
pub const DAUX_ID_SIZE: usize = 64;
pub const DAUX_NAME_SIZE: usize = 128;
pub const DAUX_SHORT_SIZE: usize = 32;
pub const DAUX_VENDOR_SIZE: usize = 128;

// ===========================================================================
// Result codes (DAUx/Abi/Result.h)
// ===========================================================================
pub type daux_result = i32;
pub const DAUX_OK: daux_result = 0;
pub const DAUX_ERR_UNKNOWN: daux_result = -1;
pub const DAUX_ERR_INVALID_ARG: daux_result = -2;
pub const DAUX_ERR_NOT_SUPPORTED: daux_result = -3;
pub const DAUX_ERR_NOT_INITIALIZED: daux_result = -4;
pub const DAUX_ERR_OUT_OF_MEMORY: daux_result = -5;
pub const DAUX_ERR_INVALID_STATE: daux_result = -6;
pub const DAUX_ERR_BUFFER_TOO_SMALL: daux_result = -7;
pub const DAUX_ERR_ABI_MISMATCH: daux_result = -8;
pub const DAUX_ERR_NOT_FOUND: daux_result = -9;
pub const DAUX_ERR_IO: daux_result = -10;
pub const DAUX_ERROR_PLUGIN_FAILED: daux_result = -11;
pub const DAUX_ERROR_HOST_FAILED: daux_result = -12;

pub type daux_bool = i32;
pub const DAUX_TRUE: daux_bool = 1;
pub const DAUX_FALSE: daux_bool = 0;

// Opaque handles (DAUx/Abi/Types.h)
pub type daux_plugin_handle = *mut c_void;
pub type daux_editor_handle = *mut c_void;
pub type daux_host_context = *mut c_void;
pub type daux_native_window = *mut c_void;
pub type daux_param_id = u32;

// Category (DAUx/Plugin/Category.h)
pub type daux_plugin_category = i32;
pub const DAUX_CATEGORY_UNKNOWN: daux_plugin_category = 0;
pub const DAUX_CATEGORY_EFFECT: daux_plugin_category = 1;
pub const DAUX_CATEGORY_INSTRUMENT: daux_plugin_category = 2;
pub const DAUX_CATEGORY_MIDI_FX: daux_plugin_category = 3;
pub const DAUX_CATEGORY_ANALYZER: daux_plugin_category = 4;

// Sample format (DAUx/Audio/SampleFormat.h)
pub type daux_sample_format = i32;
pub const DAUX_SAMPLE_F32: daux_sample_format = 0;
pub const DAUX_SAMPLE_F64: daux_sample_format = 1;

// Parameter flags (DAUx/Parameter/ParameterFlags.h)
pub type daux_param_flags = i32;
pub const DAUX_PARAM_NONE: daux_param_flags = 0;
pub const DAUX_PARAM_AUTOMATABLE: daux_param_flags = 1 << 0;
pub const DAUX_PARAM_STEPPED: daux_param_flags = 1 << 1;
pub const DAUX_PARAM_READ_ONLY: daux_param_flags = 1 << 2;
pub const DAUX_PARAM_IS_BYPASS: daux_param_flags = 1 << 3;

// Capabilities (DAUx/Plugin/Capabilities.h)
pub type daux_plugin_caps = i32;
pub const DAUX_CAP_NONE: daux_plugin_caps = 0;
pub const DAUX_CAP_EDITOR: daux_plugin_caps = 1 << 0;
pub const DAUX_CAP_MIDI_INPUT: daux_plugin_caps = 1 << 1;
pub const DAUX_CAP_MIDI_OUTPUT: daux_plugin_caps = 1 << 2;
pub const DAUX_CAP_STATE: daux_plugin_caps = 1 << 3;
pub const DAUX_CAP_PRESETS: daux_plugin_caps = 1 << 4;

// ===========================================================================
// POD structs (DAUx/Audio, Parameter, Plugin, Editor)
// ===========================================================================
#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_process_context {
    pub sample_rate: f64,
    pub block_size: u32,
    pub sample_format: u32,
    pub timeline_samples: i64,
    pub tempo_bpm: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    pub is_playing: daux_bool,
    pub is_recording: daux_bool,
    pub is_looping: daux_bool,
    pub reserved: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_audio_bus_buffer {
    pub channel_count: u32,
    pub channels: *mut *mut f32,
    pub channels64: *mut *mut f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_process_data {
    pub context: *const daux_process_context,
    pub input_bus_count: u32,
    pub inputs: *mut daux_audio_bus_buffer,
    pub output_bus_count: u32,
    pub outputs: *mut daux_audio_bus_buffer,
    pub num_frames: u32,
    pub midi_in: *mut c_void,
    pub midi_out: *mut c_void,
    pub reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_param_info {
    pub id: daux_param_id,
    pub name: [c_char; DAUX_NAME_SIZE],
    pub short_name: [c_char; DAUX_SHORT_SIZE],
    pub unit: [c_char; DAUX_SHORT_SIZE],
    pub default_value: f64,
    pub min_value: f64,
    pub max_value: f64,
    pub step_count: i32,
    pub flags: daux_param_flags,
    pub reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_plugin_descriptor {
    pub id: [c_char; DAUX_ID_SIZE],
    pub name: [c_char; DAUX_NAME_SIZE],
    pub vendor: [c_char; DAUX_VENDOR_SIZE],
    pub version: u32,
    pub category: daux_plugin_category,
    pub capabilities: daux_plugin_caps,
    pub audio_input_buses: u32,
    pub audio_output_buses: u32,
    pub default_channels_per_bus: u32,
    pub reserved: [u32; 8],
}

// Editor geometry (DAUx/Editor/EditorBounds.h)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_size {
    pub width: u32,
    pub height: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

// Host callbacks (DAUx/Host/HostCallbacks.h)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_host_callbacks {
    pub struct_size: u32,
    pub abi_version: u32,
    pub context: daux_host_context,
    pub param_changed:
        Option<unsafe extern "C" fn(ctx: daux_host_context, id: daux_param_id, normalized: f64)>,
    pub param_gesture_begin: Option<unsafe extern "C" fn(ctx: daux_host_context, id: daux_param_id)>,
    pub param_gesture_end: Option<unsafe extern "C" fn(ctx: daux_host_context, id: daux_param_id)>,
    pub resize_editor:
        Option<unsafe extern "C" fn(ctx: daux_host_context, width: u32, height: u32)>,
    pub log: Option<unsafe extern "C" fn(ctx: daux_host_context, level: i32, msg: *const c_char)>,
    pub reserved: [*mut c_void; 8],
}

// Editor vtable (DAUx/Editor/Editor.h)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_editor_vtable {
    pub struct_size: u32,
    pub attach: Option<unsafe extern "C" fn(ed: daux_editor_handle, parent: daux_native_window) -> daux_result>,
    pub detach: Option<unsafe extern "C" fn(ed: daux_editor_handle) -> daux_result>,
    pub get_preferred_size: Option<unsafe extern "C" fn(ed: daux_editor_handle, out: *mut daux_size) -> daux_result>,
    pub set_bounds: Option<unsafe extern "C" fn(ed: daux_editor_handle, bounds: *const daux_rect) -> daux_result>,
    pub show: Option<unsafe extern "C" fn(ed: daux_editor_handle) -> daux_result>,
    pub hide: Option<unsafe extern "C" fn(ed: daux_editor_handle) -> daux_result>,
    pub idle: Option<unsafe extern "C" fn(ed: daux_editor_handle)>,
    pub reserved: [*mut c_void; 6],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_editor {
    pub handle: daux_editor_handle,
    pub vtable: *const daux_editor_vtable,
}

// Plugin vtable (DAUx/Plugin/Plugin.h)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_plugin_vtable {
    pub struct_size: u32,

    pub prepare: Option<unsafe extern "C" fn(self_: daux_plugin_handle, sample_rate: f64, max_block_size: u32) -> daux_result>,
    pub activate: Option<unsafe extern "C" fn(self_: daux_plugin_handle) -> daux_result>,
    pub deactivate: Option<unsafe extern "C" fn(self_: daux_plugin_handle) -> daux_result>,
    pub reset: Option<unsafe extern "C" fn(self_: daux_plugin_handle) -> daux_result>,

    pub process: Option<unsafe extern "C" fn(self_: daux_plugin_handle, data: *const daux_process_data) -> daux_result>,

    pub get_parameter_count: Option<unsafe extern "C" fn(self_: daux_plugin_handle) -> u32>,
    pub get_parameter_info: Option<unsafe extern "C" fn(self_: daux_plugin_handle, index: u32, out: *mut daux_param_info) -> daux_result>,
    pub get_parameter_value: Option<unsafe extern "C" fn(self_: daux_plugin_handle, id: daux_param_id, out: *mut f64) -> daux_result>,
    pub set_parameter_value: Option<unsafe extern "C" fn(self_: daux_plugin_handle, id: daux_param_id, value: f64) -> daux_result>,

    pub normalized_to_plain: Option<unsafe extern "C" fn(self_: daux_plugin_handle, id: daux_param_id, normalized: f64) -> f64>,
    pub plain_to_normalized: Option<unsafe extern "C" fn(self_: daux_plugin_handle, id: daux_param_id, plain: f64) -> f64>,

    pub format_parameter: Option<unsafe extern "C" fn(self_: daux_plugin_handle, id: daux_param_id, plain: f64, out_buf: *mut c_char, buf_size: u32) -> daux_result>,
    pub parse_parameter: Option<unsafe extern "C" fn(self_: daux_plugin_handle, id: daux_param_id, text: *const c_char, out_plain: *mut f64) -> daux_result>,

    pub get_state: Option<unsafe extern "C" fn(self_: daux_plugin_handle, out_buf: *mut c_void, buf_size: u32, out_size: *mut u32) -> daux_result>,
    pub set_state: Option<unsafe extern "C" fn(self_: daux_plugin_handle, data: *const c_void, size: u32) -> daux_result>,

    pub create_editor: Option<unsafe extern "C" fn(self_: daux_plugin_handle, host: *const daux_host_callbacks, out_editor: *mut daux_editor) -> daux_result>,
    pub destroy_editor: Option<unsafe extern "C" fn(self_: daux_plugin_handle, editor: daux_editor_handle) -> daux_result>,

    pub reserved: [*mut c_void; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_plugin_instance {
    pub handle: daux_plugin_handle,
    pub vtable: *const daux_plugin_vtable,
}

// Factory + entry point (DAUx/Plugin/EntryPoint.h)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct daux_plugin_factory {
    pub struct_size: u32,
    pub abi_version: u32,
    pub get_descriptor: Option<unsafe extern "C" fn() -> *const daux_plugin_descriptor>,
    pub create: Option<unsafe extern "C" fn(host: *const daux_host_callbacks, out_instance: *mut daux_plugin_instance) -> daux_result>,
    pub destroy: Option<unsafe extern "C" fn(self_: daux_plugin_handle) -> daux_result>,
    pub reserved: [*mut c_void; 6],
}

/// The exported entry point symbol name.
pub const DAUX_PLUGIN_ENTRY_SYMBOL: &str = "daux_plugin_entry";

pub type daux_plugin_entry_fn =
    unsafe extern "C" fn(host_abi_version: u32) -> *const daux_plugin_factory;
