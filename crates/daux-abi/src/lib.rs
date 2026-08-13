//! Stable C-compatible ABI definitions for the DAUx Audio Extension (`.axt`) format.
//!
//! This crate is the Rust transcription of `docs/specifications/abi-v1.md`. That document
//! is normative: **where the two disagree, the document wins and this crate is a bug.**
//!
//! # What is in here
//!
//! Constants, `#[repr(C)]` structures and `unsafe extern "C"` function-pointer types, plus
//! the small `const fn` helpers needed to fill the fixed text buffers, validate `size`
//! fields and convert status codes. There is no logic, no state and no allocation
//! anywhere in the crate; every item is usable from any thread, and the ones marked
//! `[audio-thread]` are usable under real-time constraints.
//!
//! # Design rules the layout obeys
//!
//! * Every structure is `#[repr(C)]` and **append-only**: a future minor revision may add
//!   fields at the tail, but never reorders, resizes, repurposes or removes an existing
//!   one (`abi-v1` §1).
//! * Every structure that can grow carries `size: u32` as its first field. A reader
//!   validates `size` before touching any field beyond the ones it knows — see
//!   [`compat`], [`has_field`] and the generated `field_present`/`is_v1_0_compatible`
//!   methods.
//! * No Rust `enum`, `String`, `Vec`, `Box`, `&T`, trait object or generic crosses the
//!   boundary; booleans are [`DauxBool`], optional entries are
//!   `Option<unsafe extern "C" fn(..)>` where null means "not supported".
//! * No allocation crosses the boundary in either direction (`abi-v1` §16.2).
//!
//! # Constructing structures
//!
//! Data structures — descriptors, configurations, events, transport, parameter and port
//! info — have a `pub const fn new()` (aliased as `empty()`, plus `Default`) that zeroes
//! every field, including reserved arrays and implicit padding, and sets `size`.
//!
//! Function tables do **not**: their entries are non-nullable `unsafe extern "C" fn`
//! pointers, for which the all-zero bit pattern is not a valid value, and a positional
//! constructor over a dozen identically-typed function pointers would be a correctness
//! hazard in a binary contract. Build them as `static` struct literals instead, using the
//! generated `SIZE` constant:
//!
//! ```
//! # use daux_abi::*;
//! # unsafe extern "C" fn get(_: DauxPluginHandle) -> u32 { 0 }
//! static LATENCY: DauxLatencyApiV1 = DauxLatencyApiV1 {
//!     size: DauxLatencyApiV1::SIZE,
//!     _pad0: 0,
//!     get,
//!     reserved: [0; 2],
//! };
//! assert!(LATENCY.is_v1_0_compatible());
//! ```
//!
//! # Threading
//!
//! Doc comments carry `[main-thread]`, `[audio-thread]` or `[any-thread]` exactly as
//! `abi-v1` §15 defines them. `[audio-thread]` calls for one instance are never concurrent
//! with each other; calls for different instances may be, and may move between threads
//! between blocks, so no thread-local state may be assumed.

#![cfg_attr(not(test), no_std)]

pub mod compat;
pub mod descriptor;
pub mod entry;
pub mod events;
pub mod ext;
pub mod factory;
pub mod handle;
pub mod host;
pub mod plugin;
pub mod process;
pub mod shared_texture;
pub mod status;
pub mod string;
pub mod transport;
pub mod version;

#[cfg(test)]
mod tests;

pub use crate::compat::{AbiStruct, has_field, is_v1_0_compatible, size_of_v1_0};
pub use crate::descriptor::{
    DAUX_CAP_ANALYZER, DAUX_CAP_AUDIO_EFFECT, DAUX_CAP_DYNAMIC_BUSES, DAUX_CAP_HARD_REALTIME,
    DAUX_CAP_HAS_GUI, DAUX_CAP_INSTRUMENT, DAUX_CAP_LATENCY_DYNAMIC, DAUX_CAP_MIDI_EFFECT,
    DAUX_CAP_MIDI_INPUT, DAUX_CAP_MIDI_OUTPUT, DAUX_CAP_MIDI2, DAUX_CAP_NOTE_EXPRESSION,
    DAUX_CAP_OFFLINE_RENDER, DAUX_CAP_REQUIRES_GUI, DAUX_CAP_SAMPLE_ACCURATE_AUTO,
    DAUX_CAP_SANDBOX_SAFE, DAUX_CAP_SHARED_TEXTURE_GUI, DAUX_CAP_SIDECHAIN, DAUX_CAP_STEREO_ONLY,
    DAUX_CAP_TAIL_INFINITE, DAUX_CATEGORY_ANALYZER, DAUX_CATEGORY_EFFECT, DAUX_CATEGORY_GENERATOR,
    DAUX_CATEGORY_INSTRUMENT, DAUX_CATEGORY_MIDI_EFFECT, DAUX_CATEGORY_UNKNOWN,
    DAUX_CATEGORY_UTILITY, DAUX_SAMPLE_FORMAT_F32, DAUX_SAMPLE_FORMAT_F64, DauxPluginDescriptorV1,
};
pub use crate::entry::{
    DAUX_ENTRY_SYMBOL, DAUX_ENTRY_SYMBOL_CSTR, DauxPluginEntryFn, DauxPluginEntryV1,
};
pub use crate::events::{
    DAUX_EVENT_CUSTOM, DAUX_EVENT_FLAG_DONT_RECORD, DAUX_EVENT_FLAG_IS_LIVE, DAUX_EVENT_MIDI1,
    DAUX_EVENT_MIDI2, DAUX_EVENT_NOTE_CHOKE, DAUX_EVENT_NOTE_END, DAUX_EVENT_NOTE_EXPRESSION,
    DAUX_EVENT_NOTE_OFF, DAUX_EVENT_NOTE_ON, DAUX_EVENT_PARAM_GESTURE_BEGIN,
    DAUX_EVENT_PARAM_GESTURE_END, DAUX_EVENT_PARAM_MOD, DAUX_EVENT_PARAM_VALUE, DAUX_EVENT_SYSEX,
    DAUX_EVENT_TRANSPORT, DAUX_NOTE_EXPR_BRIGHTNESS, DAUX_NOTE_EXPR_EXPRESSION, DAUX_NOTE_EXPR_PAN,
    DAUX_NOTE_EXPR_PRESSURE, DAUX_NOTE_EXPR_TUNING, DAUX_NOTE_EXPR_VIBRATO, DAUX_NOTE_EXPR_VOLUME,
    DauxEventHeaderV1, DauxEventListV1, DauxEventMidi1V1, DauxEventMidi2V1,
    DauxEventNoteExpressionV1, DauxEventNoteV1, DauxEventParamV1, DauxEventSysExV1,
    DauxEventTransportV1,
};
pub use crate::ext::audio_ports::{
    DAUX_LAYOUT_AMBISONIC_1ST, DAUX_LAYOUT_AMBISONIC_2ND, DAUX_LAYOUT_AMBISONIC_3RD,
    DAUX_LAYOUT_ATMOS_7_1_4, DAUX_LAYOUT_CUSTOM, DAUX_LAYOUT_DISCRETE, DAUX_LAYOUT_L_R_C,
    DAUX_LAYOUT_MONO, DAUX_LAYOUT_QUAD, DAUX_LAYOUT_STEREO, DAUX_LAYOUT_SURROUND_2_1,
    DAUX_LAYOUT_SURROUND_5_1, DAUX_LAYOUT_SURROUND_7_1, DAUX_LAYOUT_UNKNOWN, DAUX_PORT_FLAG_CV,
    DAUX_PORT_FLAG_IS_MAIN, DAUX_PORT_FLAG_OPTIONAL, DAUX_PORT_FLAG_SUPPORTS_64,
    DAUX_PORT_PURPOSE_ANALYSIS, DAUX_PORT_PURPOSE_AUX, DAUX_PORT_PURPOSE_CONTROL,
    DAUX_PORT_PURPOSE_CV, DAUX_PORT_PURPOSE_MAIN, DAUX_PORT_PURPOSE_MONITOR,
    DAUX_PORT_PURPOSE_REFERENCE, DAUX_PORT_PURPOSE_SIDECHAIN, DauxAudioPortInfoV1,
    DauxAudioPortsApiV1,
};
pub use crate::ext::gui::{
    DAUX_WINDOW_API_COCOA, DAUX_WINDOW_API_WAYLAND, DAUX_WINDOW_API_WIN32, DAUX_WINDOW_API_X11,
    DauxGuiApiV1, DauxWindowV1,
};
pub use crate::ext::params::{
    DAUX_PARAM_FLAG_AUTOMATABLE, DAUX_PARAM_FLAG_BYPASS, DAUX_PARAM_FLAG_HIDDEN,
    DAUX_PARAM_FLAG_IS_METER, DAUX_PARAM_FLAG_MODULATABLE, DAUX_PARAM_FLAG_PER_NOTE,
    DAUX_PARAM_FLAG_READ_ONLY, DAUX_PARAM_FLAG_REQUIRES_PROCESS, DAUX_PARAM_FLAG_STEPPED,
    DauxParamInfoV1, DauxParamsApiV1,
};
pub use crate::ext::render::{
    DAUX_TAIL_INFINITE, DAUX_TAIL_UNKNOWN, DauxLatencyApiV1, DauxRenderApiV1, DauxTailApiV1,
};
pub use crate::ext::state::{DauxStateApiV1, DauxStreamV1};
pub use crate::factory::DauxFactoryApiV1;
pub use crate::handle::{
    DauxFactoryHandle, DauxFactoryV1, DauxHostHandle, DauxHostV1, DauxPluginHandle, DauxPluginV1,
};
pub use crate::host::{
    DAUX_LOG_DEBUG, DAUX_LOG_ERROR, DAUX_LOG_FATAL, DAUX_LOG_INFO, DAUX_LOG_TRACE, DAUX_LOG_WARN,
    DauxHostApiV1, DauxHostGuiApiV1, DauxHostLogApiV1, DauxHostParamsApiV1, DauxHostWorkerApiV1,
};
pub use crate::plugin::DauxPluginApiV1;
pub use crate::process::{
    DAUX_PROCESS_CONTINUE, DAUX_PROCESS_CONTINUE_IF_LOUD, DAUX_PROCESS_ERROR,
    DAUX_PROCESS_MODE_ANALYSIS, DAUX_PROCESS_MODE_OFFLINE, DAUX_PROCESS_MODE_PREFETCH,
    DAUX_PROCESS_MODE_REALTIME, DAUX_PROCESS_SLEEP, DAUX_PROCESS_TAIL, DauxAudioBufferV1,
    DauxProcessConfigV1, DauxProcessV1,
};
pub use crate::shared_texture::{
    DAUX_TEXTURE_HANDLE_D3D11_SHARED, DAUX_TEXTURE_HANDLE_D3D12_HEAP, DAUX_TEXTURE_HANDLE_DMABUF,
    DAUX_TEXTURE_HANDLE_IOSURFACE, DAUX_TEXTURE_HANDLE_VULKAN_FD, DAUX_TEXTURE_HANDLE_VULKAN_WIN32,
    DauxSharedTextureV1,
};
pub use crate::status::{
    DAUX_ERR_ABI_MISMATCH, DAUX_ERR_GRAPHICS, DAUX_ERR_HOST, DAUX_ERR_INTERNAL,
    DAUX_ERR_INVALID_ARG, DAUX_ERR_INVALID_STATE, DAUX_ERR_IO, DAUX_ERR_NOT_FOUND,
    DAUX_ERR_NOT_REALTIME, DAUX_ERR_OUT_OF_MEMORY, DAUX_ERR_PANIC, DAUX_ERR_PLUGIN,
    DAUX_ERR_UNKNOWN, DAUX_ERR_UNSUPPORTED, DAUX_ERR_VERSION, DAUX_ERR_WRONG_THREAD, DAUX_FALSE,
    DAUX_OK, DAUX_TRUE, DauxBool, DauxStatus, daux_bool, daux_bool_is_true,
};
pub use crate::string::{
    DAUX_ID_SIZE, DAUX_NAME_SIZE, DAUX_PATH_SIZE, DAUX_TEXT_SIZE, DauxId, DauxName, DauxPath,
    DauxStrView, DauxText, extension_table,
};
pub use crate::transport::{
    DAUX_TRANSPORT_HAS_BAR, DAUX_TRANSPORT_HAS_BEATS, DAUX_TRANSPORT_HAS_LOOP,
    DAUX_TRANSPORT_HAS_SECONDS, DAUX_TRANSPORT_HAS_TEMPO, DAUX_TRANSPORT_HAS_TIME_SIG,
    DAUX_TRANSPORT_IS_LOOPING, DAUX_TRANSPORT_IS_PLAYING, DAUX_TRANSPORT_IS_PREROLL,
    DAUX_TRANSPORT_IS_RECORDING, DauxTransportV1,
};
pub use crate::version::{
    DAUX_ABI_MAGIC, DAUX_ABI_VERSION_MAJOR, DAUX_ABI_VERSION_MINOR, DauxVersion, check_entry_header,
};
