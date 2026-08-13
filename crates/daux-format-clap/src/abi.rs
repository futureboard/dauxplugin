//! A pure-Rust transcription of the parts of the CLAP C ABI this adapter needs.
//!
//! There is no `clap-sys` dependency and no vendored C header anywhere in DAUxPlug. Every
//! struct below is hand-written from the CLAP 1.2 headers the same way `daux-abi` is
//! hand-written from `docs/specifications/abi-v1.md`, and every one of them is
//! `#[repr(C)]`. Nothing here is a Rust `enum`, a `Vec`, a `String`, a reference or a
//! generic: this module is the wire format and nothing else.
//!
//! # Naming
//!
//! C names like `clap_plugin_descriptor_t` become `ClapPluginDescriptor`. The C spelling is
//! given in each doc comment so the two can be diffed by eye against a header.
//!
//! # Nullability
//!
//! Tables **we** fill in use bare `unsafe extern "C"` function pointers, because we always
//! provide them. Tables the **host** fills in use `Option<unsafe extern "C" fn…>`, because a
//! host that leaves a slot null must produce a refusal rather than a jump to address zero.
//!
//! `[any-thread]` — these are plain data definitions.

use core::ffi::{CStr, c_char, c_ulong, c_void};

// ---------------------------------------------------------------------------------------
// version
// ---------------------------------------------------------------------------------------

/// `clap_version_t`: the ABI version triple carried by the entry, the host and every
/// descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct ClapVersion {
    /// Major version. Only `1` exists, and a mismatch means "do not load".
    pub major: u32,
    /// Minor version. A host with a lower minor than the plug-in must still load it.
    pub minor: u32,
    /// Revision. Purely informational.
    pub revision: u32,
}

impl ClapVersion {
    /// The version this adapter is written against.
    pub const CURRENT: Self = Self {
        major: 1,
        minor: 2,
        revision: 2,
    };

    /// `clap_version_is_compatible`: major must match and the whole triple must be at least
    /// 1.0.0.
    #[must_use]
    pub const fn is_compatible(self) -> bool {
        self.major == 1
    }
}

// ---------------------------------------------------------------------------------------
// primitive aliases and sizes
// ---------------------------------------------------------------------------------------

/// `clap_id`: an opaque stable identifier for a port, a parameter or a preset.
pub type ClapId = u32;

/// `CLAP_INVALID_ID`: the sentinel `clap_id` meaning "none".
pub const CLAP_INVALID_ID: ClapId = u32::MAX;

/// `CLAP_NAME_SIZE`: capacity of every fixed name buffer in the CLAP ABI, NUL included.
pub const CLAP_NAME_SIZE: usize = 256;

/// `CLAP_PATH_SIZE`: capacity of every fixed path buffer in the CLAP ABI, NUL included.
pub const CLAP_PATH_SIZE: usize = 1024;

/// `CLAP_BEATTIME_FACTOR`: fixed-point scale of `clap_beattime`.
pub const CLAP_BEATTIME_FACTOR: i64 = 1 << 31;

/// `CLAP_SECTIME_FACTOR`: fixed-point scale of `clap_sectime`.
pub const CLAP_SECTIME_FACTOR: i64 = 1 << 31;

/// `CLAP_PLUGIN_FACTORY_ID`: the only factory id this adapter answers to.
pub const CLAP_PLUGIN_FACTORY_ID: &CStr = c"clap.plugin-factory";

// ---------------------------------------------------------------------------------------
// extension ids
// ---------------------------------------------------------------------------------------

/// `CLAP_EXT_AUDIO_PORTS`.
pub const CLAP_EXT_AUDIO_PORTS: &CStr = c"clap.audio-ports";
/// `CLAP_EXT_NOTE_PORTS`.
pub const CLAP_EXT_NOTE_PORTS: &CStr = c"clap.note-ports";
/// `CLAP_EXT_PARAMS`.
pub const CLAP_EXT_PARAMS: &CStr = c"clap.params";
/// `CLAP_EXT_STATE`.
pub const CLAP_EXT_STATE: &CStr = c"clap.state";
/// `CLAP_EXT_GUI`.
pub const CLAP_EXT_GUI: &CStr = c"clap.gui";
/// `CLAP_EXT_LATENCY`.
pub const CLAP_EXT_LATENCY: &CStr = c"clap.latency";
/// `CLAP_EXT_TAIL`.
pub const CLAP_EXT_TAIL: &CStr = c"clap.tail";
/// `CLAP_EXT_RENDER`.
pub const CLAP_EXT_RENDER: &CStr = c"clap.render";
/// `CLAP_EXT_LOG`, a host extension.
pub const CLAP_EXT_LOG: &CStr = c"clap.log";
/// `CLAP_EXT_THREAD_CHECK`, a host extension.
pub const CLAP_EXT_THREAD_CHECK: &CStr = c"clap.thread-check";

/// `CLAP_PORT_MONO`.
pub const CLAP_PORT_MONO: &CStr = c"mono";
/// `CLAP_PORT_STEREO`.
pub const CLAP_PORT_STEREO: &CStr = c"stereo";
/// `CLAP_PORT_SURROUND`.
pub const CLAP_PORT_SURROUND: &CStr = c"surround";
/// `CLAP_PORT_AMBISONIC`.
pub const CLAP_PORT_AMBISONIC: &CStr = c"ambisonic";

/// `CLAP_WINDOW_API_WIN32`.
pub const CLAP_WINDOW_API_WIN32: &CStr = c"win32";
/// `CLAP_WINDOW_API_COCOA`.
pub const CLAP_WINDOW_API_COCOA: &CStr = c"cocoa";
/// `CLAP_WINDOW_API_X11`.
pub const CLAP_WINDOW_API_X11: &CStr = c"x11";
/// `CLAP_WINDOW_API_WAYLAND`.
pub const CLAP_WINDOW_API_WAYLAND: &CStr = c"wayland";

// ---------------------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------------------

/// `CLAP_CORE_EVENT_SPACE_ID`: the only event space this adapter understands.
pub const CLAP_CORE_EVENT_SPACE_ID: u16 = 0;

/// `CLAP_EVENT_NOTE_ON`.
pub const CLAP_EVENT_NOTE_ON: u16 = 0;
/// `CLAP_EVENT_NOTE_OFF`.
pub const CLAP_EVENT_NOTE_OFF: u16 = 1;
/// `CLAP_EVENT_NOTE_CHOKE`.
pub const CLAP_EVENT_NOTE_CHOKE: u16 = 2;
/// `CLAP_EVENT_NOTE_END`.
pub const CLAP_EVENT_NOTE_END: u16 = 3;
/// `CLAP_EVENT_NOTE_EXPRESSION`.
pub const CLAP_EVENT_NOTE_EXPRESSION: u16 = 4;
/// `CLAP_EVENT_PARAM_VALUE`.
pub const CLAP_EVENT_PARAM_VALUE: u16 = 5;
/// `CLAP_EVENT_PARAM_MOD`.
pub const CLAP_EVENT_PARAM_MOD: u16 = 6;
/// `CLAP_EVENT_PARAM_GESTURE_BEGIN`.
pub const CLAP_EVENT_PARAM_GESTURE_BEGIN: u16 = 7;
/// `CLAP_EVENT_PARAM_GESTURE_END`.
pub const CLAP_EVENT_PARAM_GESTURE_END: u16 = 8;
/// `CLAP_EVENT_TRANSPORT`.
pub const CLAP_EVENT_TRANSPORT: u16 = 9;
/// `CLAP_EVENT_MIDI`.
pub const CLAP_EVENT_MIDI: u16 = 10;
/// `CLAP_EVENT_MIDI_SYSEX`.
pub const CLAP_EVENT_MIDI_SYSEX: u16 = 11;
/// `CLAP_EVENT_MIDI2`.
pub const CLAP_EVENT_MIDI2: u16 = 12;

/// `CLAP_EVENT_IS_LIVE`: performed live rather than played back from an automation lane.
pub const CLAP_EVENT_IS_LIVE: u32 = 1 << 0;
/// `CLAP_EVENT_DONT_RECORD`: the host should not write this event into its lanes.
pub const CLAP_EVENT_DONT_RECORD: u32 = 1 << 1;

/// `CLAP_NOTE_EXPRESSION_VOLUME`.
pub const CLAP_NOTE_EXPRESSION_VOLUME: i32 = 0;
/// `CLAP_NOTE_EXPRESSION_PAN`.
pub const CLAP_NOTE_EXPRESSION_PAN: i32 = 1;
/// `CLAP_NOTE_EXPRESSION_TUNING`.
pub const CLAP_NOTE_EXPRESSION_TUNING: i32 = 2;
/// `CLAP_NOTE_EXPRESSION_VIBRATO`.
pub const CLAP_NOTE_EXPRESSION_VIBRATO: i32 = 3;
/// `CLAP_NOTE_EXPRESSION_EXPRESSION`.
pub const CLAP_NOTE_EXPRESSION_EXPRESSION: i32 = 4;
/// `CLAP_NOTE_EXPRESSION_BRIGHTNESS`.
pub const CLAP_NOTE_EXPRESSION_BRIGHTNESS: i32 = 5;
/// `CLAP_NOTE_EXPRESSION_PRESSURE`.
pub const CLAP_NOTE_EXPRESSION_PRESSURE: i32 = 6;

/// `clap_event_header_t`: the prefix every CLAP event begins with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ClapEventHeader {
    /// Total size of the concrete event struct, header included.
    pub size: u32,
    /// Sample offset inside the current block.
    pub time: u32,
    /// Event namespace; only [`CLAP_CORE_EVENT_SPACE_ID`] is understood here.
    pub space_id: u16,
    /// One of the `CLAP_EVENT_*` codes.
    pub type_: u16,
    /// `CLAP_EVENT_IS_LIVE` / `CLAP_EVENT_DONT_RECORD`.
    pub flags: u32,
}

/// `clap_event_note_t`: note on / off / choke / end.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct ClapEventNote {
    /// Common prefix.
    pub header: ClapEventHeader,
    /// Host-assigned voice id, or `-1` when the host does not track voices.
    pub note_id: i32,
    /// Note port index, or `-1` as a wildcard.
    pub port_index: i16,
    /// MIDI channel `0..=15`, or `-1` as a wildcard.
    pub channel: i16,
    /// Key number `0..=127`, or `-1` as a wildcard.
    pub key: i16,
    /// Velocity, `0.0 ..= 1.0`.
    pub velocity: f64,
}

/// `clap_event_note_expression_t`: one per-note expression dimension.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct ClapEventNoteExpression {
    /// Common prefix.
    pub header: ClapEventHeader,
    /// One of the `CLAP_NOTE_EXPRESSION_*` codes.
    pub expression_id: i32,
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
    /// Note port index, or `-1`.
    pub port_index: i16,
    /// MIDI channel, or `-1`.
    pub channel: i16,
    /// Key number, or `-1`.
    pub key: i16,
    /// The new value; the meaningful range depends on `expression_id`.
    pub value: f64,
}

/// `clap_event_param_value_t`: an absolute parameter value, in plain units.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ClapEventParamValue {
    /// Common prefix.
    pub header: ClapEventHeader,
    /// The parameter's stable id.
    pub param_id: ClapId,
    /// The plug-in's own cookie from `clap_param_info`. Always null here.
    pub cookie: *mut c_void,
    /// Host-assigned voice id, or `-1` for a channel-wide change.
    pub note_id: i32,
    /// Note port index, or `-1`.
    pub port_index: i16,
    /// MIDI channel, or `-1`.
    pub channel: i16,
    /// Key number, or `-1`.
    pub key: i16,
    /// The new plain value.
    pub value: f64,
}

/// `clap_event_param_mod_t`: a signed parameter modulation offset, in plain units.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ClapEventParamMod {
    /// Common prefix.
    pub header: ClapEventHeader,
    /// The parameter's stable id.
    pub param_id: ClapId,
    /// The plug-in's own cookie from `clap_param_info`. Always null here.
    pub cookie: *mut c_void,
    /// Host-assigned voice id, or `-1` for a channel-wide modulation.
    pub note_id: i32,
    /// Note port index, or `-1`.
    pub port_index: i16,
    /// MIDI channel, or `-1`.
    pub channel: i16,
    /// Key number, or `-1`.
    pub key: i16,
    /// The signed offset added on top of the parameter's value.
    pub amount: f64,
}

/// `clap_event_param_gesture_t`: a knob grab or release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ClapEventParamGesture {
    /// Common prefix.
    pub header: ClapEventHeader,
    /// The parameter's stable id.
    pub param_id: ClapId,
}

/// `CLAP_TRANSPORT_HAS_TEMPO`.
pub const CLAP_TRANSPORT_HAS_TEMPO: u32 = 1 << 0;
/// `CLAP_TRANSPORT_HAS_BEATS_TIMELINE`.
pub const CLAP_TRANSPORT_HAS_BEATS_TIMELINE: u32 = 1 << 1;
/// `CLAP_TRANSPORT_HAS_SECONDS_TIMELINE`.
pub const CLAP_TRANSPORT_HAS_SECONDS_TIMELINE: u32 = 1 << 2;
/// `CLAP_TRANSPORT_HAS_TIME_SIGNATURE`.
pub const CLAP_TRANSPORT_HAS_TIME_SIGNATURE: u32 = 1 << 3;
/// `CLAP_TRANSPORT_IS_PLAYING`.
pub const CLAP_TRANSPORT_IS_PLAYING: u32 = 1 << 4;
/// `CLAP_TRANSPORT_IS_RECORDING`.
pub const CLAP_TRANSPORT_IS_RECORDING: u32 = 1 << 5;
/// `CLAP_TRANSPORT_IS_LOOP_ACTIVE`.
pub const CLAP_TRANSPORT_IS_LOOP_ACTIVE: u32 = 1 << 6;
/// `CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL`.
pub const CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL: u32 = 1 << 7;

/// `clap_event_transport_t`: the host's timeline for this block, or a discontinuity in it.
///
/// Beat and second positions are 64-bit fixed point; see [`CLAP_BEATTIME_FACTOR`] and
/// [`CLAP_SECTIME_FACTOR`].
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct ClapEventTransport {
    /// Common prefix.
    pub header: ClapEventHeader,
    /// `CLAP_TRANSPORT_*` bits saying which fields below are meaningful.
    pub flags: u32,
    /// Musical position in fixed-point quarter-note beats.
    pub song_pos_beats: i64,
    /// Timeline position in fixed-point seconds.
    pub song_pos_seconds: i64,
    /// Tempo in BPM.
    pub tempo: f64,
    /// Tempo change per sample, in BPM.
    pub tempo_inc: f64,
    /// Loop start, fixed-point beats.
    pub loop_start_beats: i64,
    /// Loop end, fixed-point beats.
    pub loop_end_beats: i64,
    /// Loop start, fixed-point seconds.
    pub loop_start_seconds: i64,
    /// Loop end, fixed-point seconds.
    pub loop_end_seconds: i64,
    /// Start of the current bar, fixed-point beats.
    pub bar_start: i64,
    /// Index of the current bar as the host displays it.
    pub bar_number: i32,
    /// Time-signature numerator.
    pub tsig_num: u16,
    /// Time-signature denominator.
    pub tsig_denom: u16,
}

/// `clap_event_midi_t`: a MIDI 1.0 message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ClapEventMidi {
    /// Common prefix.
    pub header: ClapEventHeader,
    /// Note port index the message belongs to.
    pub port_index: u16,
    /// The three status/data bytes; unused trailing bytes are zero.
    pub data: [u8; 3],
}

/// `clap_event_midi_sysex_t`: a System Exclusive message borrowed from the host.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ClapEventMidiSysex {
    /// Common prefix.
    pub header: ClapEventHeader,
    /// Note port index the message belongs to.
    pub port_index: u16,
    /// The payload, owned by the sender and valid only for the duration of the call.
    pub buffer: *const u8,
    /// Number of bytes behind `buffer`.
    pub size: u32,
}

/// `clap_event_midi2_t`: one MIDI 2.0 Universal MIDI Packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ClapEventMidi2 {
    /// Common prefix.
    pub header: ClapEventHeader,
    /// Note port index the packet belongs to.
    pub port_index: u16,
    /// The four packet words; unused trailing words are zero.
    pub data: [u32; 4],
}

/// `clap_input_events_t`: the host's read-only, time-sorted event list for one block.
#[repr(C)]
pub struct ClapInputEvents {
    /// Opaque context owned by whoever built this list.
    pub ctx: *mut c_void,
    /// Number of events in the list.
    pub size: Option<unsafe extern "C" fn(list: *const ClapInputEvents) -> u32>,
    /// The event at `index`, or null.
    pub get: Option<
        unsafe extern "C" fn(list: *const ClapInputEvents, index: u32) -> *const ClapEventHeader,
    >,
}

/// `clap_output_events_t`: the bounded sink a plug-in writes events into.
#[repr(C)]
pub struct ClapOutputEvents {
    /// Opaque context owned by whoever built this sink.
    pub ctx: *mut c_void,
    /// Appends a copy of the event; `false` means the sink is full.
    pub try_push: Option<
        unsafe extern "C" fn(list: *const ClapOutputEvents, event: *const ClapEventHeader) -> bool,
    >,
}

// ---------------------------------------------------------------------------------------
// processing
// ---------------------------------------------------------------------------------------

/// `CLAP_PROCESS_ERROR`.
pub const CLAP_PROCESS_ERROR: i32 = 0;
/// `CLAP_PROCESS_CONTINUE`.
pub const CLAP_PROCESS_CONTINUE: i32 = 1;
/// `CLAP_PROCESS_CONTINUE_IF_NOT_QUIET`.
pub const CLAP_PROCESS_CONTINUE_IF_NOT_QUIET: i32 = 2;
/// `CLAP_PROCESS_TAIL`.
pub const CLAP_PROCESS_TAIL: i32 = 3;
/// `CLAP_PROCESS_SLEEP`.
pub const CLAP_PROCESS_SLEEP: i32 = 4;

/// `clap_audio_buffer_t`: one audio bus for one block, planar and 32- or 64-bit.
///
/// Exactly one of `data32` and `data64` is non-null.
#[repr(C)]
pub struct ClapAudioBuffer {
    /// `channel_count` pointers to `frames_count` `f32`s, or null.
    pub data32: *mut *mut f32,
    /// `channel_count` pointers to `frames_count` `f64`s, or null.
    pub data64: *mut *mut f64,
    /// Number of channels on this bus.
    pub channel_count: u32,
    /// Per-bus latency, host to plug-in. Unused by this adapter.
    pub latency: u32,
    /// Bit `c` set means channel `c` holds one repeated value for the whole block.
    pub constant_mask: u64,
}

/// `clap_process_t`: everything one `process` call is given.
#[repr(C)]
pub struct ClapProcess {
    /// A monotonic sample counter that never runs backwards, or `-1` when unavailable.
    pub steady_time: i64,
    /// Number of frames in this block.
    pub frames_count: u32,
    /// The host's timeline, or null when the host has no transport.
    pub transport: *const ClapEventTransport,
    /// `audio_inputs_count` input buses.
    pub audio_inputs: *const ClapAudioBuffer,
    /// `audio_outputs_count` output buses.
    pub audio_outputs: *mut ClapAudioBuffer,
    /// Number of input buses.
    pub audio_inputs_count: u32,
    /// Number of output buses.
    pub audio_outputs_count: u32,
    /// The host's events for this block, sorted by time.
    pub in_events: *const ClapInputEvents,
    /// The sink for events the plug-in produces.
    pub out_events: *const ClapOutputEvents,
}

// ---------------------------------------------------------------------------------------
// plug-in, host, factory, entry
// ---------------------------------------------------------------------------------------

/// `clap_plugin_descriptor_t`: what a host can know before instantiating.
///
/// Every pointer is a NUL-terminated UTF-8 C string owned by the plug-in and valid until
/// `clap_plugin_entry::deinit`.
#[repr(C)]
pub struct ClapPluginDescriptor {
    /// The ABI version the descriptor was built against.
    pub clap_version: ClapVersion,
    /// Permanent reverse-DNS identity.
    pub id: *const c_char,
    /// Product name.
    pub name: *const c_char,
    /// Vendor name.
    pub vendor: *const c_char,
    /// Product home page.
    pub url: *const c_char,
    /// Manual URL.
    pub manual_url: *const c_char,
    /// Support URL.
    pub support_url: *const c_char,
    /// Product version as text.
    pub version: *const c_char,
    /// One-line description.
    pub description: *const c_char,
    /// NULL-terminated array of feature strings.
    pub features: *const *const c_char,
}

/// `clap_plugin_t`: one plug-in instance's function table.
#[repr(C)]
pub struct ClapPlugin {
    /// The descriptor this instance was created from.
    pub desc: *const ClapPluginDescriptor,
    /// The plug-in's own per-instance state.
    pub plugin_data: *mut c_void,
    /// `[main-thread]` Completes construction.
    pub init: unsafe extern "C" fn(plugin: *const ClapPlugin) -> bool,
    /// `[main-thread]` Destroys the instance.
    pub destroy: unsafe extern "C" fn(plugin: *const ClapPlugin),
    /// `[main-thread]` Allocates DSP resources for a sample rate and block-size range.
    pub activate: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        sample_rate: f64,
        min_frames_count: u32,
        max_frames_count: u32,
    ) -> bool,
    /// `[main-thread]` Releases the activation.
    pub deactivate: unsafe extern "C" fn(plugin: *const ClapPlugin),
    /// `[audio-thread]` Arms the processor.
    pub start_processing: unsafe extern "C" fn(plugin: *const ClapPlugin) -> bool,
    /// `[audio-thread]` Disarms the processor.
    pub stop_processing: unsafe extern "C" fn(plugin: *const ClapPlugin),
    /// `[audio-thread]` Clears everything that depends on past audio.
    pub reset: unsafe extern "C" fn(plugin: *const ClapPlugin),
    /// `[audio-thread]` Processes one block.
    pub process:
        unsafe extern "C" fn(plugin: *const ClapPlugin, process: *const ClapProcess) -> i32,
    /// `[main-thread]` Returns an extension table, or null.
    pub get_extension:
        unsafe extern "C" fn(plugin: *const ClapPlugin, id: *const c_char) -> *const c_void,
    /// `[main-thread]` Runs work the audio thread asked for.
    pub on_main_thread: unsafe extern "C" fn(plugin: *const ClapPlugin),
}

/// `clap_host_t`: the host's function table, handed to the factory at creation.
#[repr(C)]
pub struct ClapHost {
    /// The ABI version the host was built against.
    pub clap_version: ClapVersion,
    /// The host's own state. Never touched by the plug-in.
    pub host_data: *mut c_void,
    /// Host name.
    pub name: *const c_char,
    /// Host vendor.
    pub vendor: *const c_char,
    /// Host home page.
    pub url: *const c_char,
    /// Host version as text.
    pub version: *const c_char,
    /// `[main-thread]` Returns a host extension table, or null.
    pub get_extension:
        Option<unsafe extern "C" fn(host: *const ClapHost, id: *const c_char) -> *const c_void>,
    /// `[thread-safe]` Asks the host to deactivate and reactivate the plug-in.
    pub request_restart: Option<unsafe extern "C" fn(host: *const ClapHost)>,
    /// `[thread-safe]` Asks the host to resume calling `process`.
    pub request_process: Option<unsafe extern "C" fn(host: *const ClapHost)>,
    /// `[thread-safe]` Asks the host to call `on_main_thread` soon.
    pub request_callback: Option<unsafe extern "C" fn(host: *const ClapHost)>,
}

/// `clap_plugin_factory_t`: enumeration and instantiation for one module.
#[repr(C)]
pub struct ClapPluginFactory {
    /// `[main-thread]` How many plug-ins this module exports.
    pub get_plugin_count: unsafe extern "C" fn(factory: *const ClapPluginFactory) -> u32,
    /// `[main-thread]` The descriptor at `index`, or null.
    pub get_plugin_descriptor: unsafe extern "C" fn(
        factory: *const ClapPluginFactory,
        index: u32,
    ) -> *const ClapPluginDescriptor,
    /// `[main-thread]` Instantiates the plug-in with that id, or returns null.
    pub create_plugin: unsafe extern "C" fn(
        factory: *const ClapPluginFactory,
        host: *const ClapHost,
        plugin_id: *const c_char,
    ) -> *const ClapPlugin,
}

/// `clap_plugin_entry_t`: the one exported symbol of a CLAP binary.
#[repr(C)]
pub struct ClapPluginEntry {
    /// The ABI version this binary was built against.
    pub clap_version: ClapVersion,
    /// `[main-thread]` Called once before anything else, with the module's own path.
    pub init: unsafe extern "C" fn(plugin_path: *const c_char) -> bool,
    /// `[main-thread]` Called once when the host is done with the module.
    pub deinit: unsafe extern "C" fn(),
    /// `[main-thread]` Returns the factory with that id, or null.
    pub get_factory: unsafe extern "C" fn(factory_id: *const c_char) -> *const c_void,
}

// SAFETY: `ClapPluginEntry` is a table of `extern "C"` function pointers and a `Copy`
// version triple. It contains no interior mutability and no thread-affine resource, so
// sharing a `&'static ClapPluginEntry` across threads — which is exactly what exporting it
// as a `static` symbol does — can race on nothing.
unsafe impl Sync for ClapPluginEntry {}

// ---------------------------------------------------------------------------------------
// audio-ports extension
// ---------------------------------------------------------------------------------------

/// `CLAP_AUDIO_PORT_IS_MAIN`.
pub const CLAP_AUDIO_PORT_IS_MAIN: u32 = 1 << 0;
/// `CLAP_AUDIO_PORT_SUPPORTS_64BITS`.
pub const CLAP_AUDIO_PORT_SUPPORTS_64BITS: u32 = 1 << 1;
/// `CLAP_AUDIO_PORT_PREFERS_64BITS`.
pub const CLAP_AUDIO_PORT_PREFERS_64BITS: u32 = 1 << 2;
/// `CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE`.
pub const CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE: u32 = 1 << 3;

/// `clap_audio_port_info_t`: one audio bus, filled in by the plug-in.
#[repr(C)]
pub struct ClapAudioPortInfo {
    /// Stable port id.
    pub id: ClapId,
    /// NUL-terminated display name.
    pub name: [c_char; CLAP_NAME_SIZE],
    /// `CLAP_AUDIO_PORT_*` bits.
    pub flags: u32,
    /// Number of channels on this bus.
    pub channel_count: u32,
    /// `CLAP_PORT_*` layout hint, or null.
    pub port_type: *const c_char,
    /// The opposite-direction port this one may be processed in place with, or
    /// [`CLAP_INVALID_ID`].
    pub in_place_pair: ClapId,
}

/// `clap_plugin_audio_ports_t`.
#[repr(C)]
pub struct ClapPluginAudioPorts {
    /// `[main-thread]` Number of buses in one direction.
    pub count: unsafe extern "C" fn(plugin: *const ClapPlugin, is_input: bool) -> u32,
    /// `[main-thread]` Fills `info` for one bus; `false` when the index is out of range.
    pub get: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        index: u32,
        is_input: bool,
        info: *mut ClapAudioPortInfo,
    ) -> bool,
}

// ---------------------------------------------------------------------------------------
// note-ports extension
// ---------------------------------------------------------------------------------------

/// `CLAP_NOTE_DIALECT_CLAP`.
pub const CLAP_NOTE_DIALECT_CLAP: u32 = 1 << 0;
/// `CLAP_NOTE_DIALECT_MIDI`.
pub const CLAP_NOTE_DIALECT_MIDI: u32 = 1 << 1;
/// `CLAP_NOTE_DIALECT_MIDI_MPE`.
pub const CLAP_NOTE_DIALECT_MIDI_MPE: u32 = 1 << 2;
/// `CLAP_NOTE_DIALECT_MIDI2`.
pub const CLAP_NOTE_DIALECT_MIDI2: u32 = 1 << 3;

/// `clap_note_port_info_t`: one event port, filled in by the plug-in.
#[repr(C)]
pub struct ClapNotePortInfo {
    /// Stable port id.
    pub id: ClapId,
    /// `CLAP_NOTE_DIALECT_*` bits this port understands.
    pub supported_dialects: u32,
    /// The single dialect the plug-in would rather receive.
    pub preferred_dialect: u32,
    /// NUL-terminated display name.
    pub name: [c_char; CLAP_NAME_SIZE],
}

/// `clap_plugin_note_ports_t`.
#[repr(C)]
pub struct ClapPluginNotePorts {
    /// `[main-thread]` Number of ports in one direction.
    pub count: unsafe extern "C" fn(plugin: *const ClapPlugin, is_input: bool) -> u32,
    /// `[main-thread]` Fills `info` for one port; `false` when the index is out of range.
    pub get: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        index: u32,
        is_input: bool,
        info: *mut ClapNotePortInfo,
    ) -> bool,
}

// ---------------------------------------------------------------------------------------
// params extension
// ---------------------------------------------------------------------------------------

/// `CLAP_PARAM_IS_STEPPED`.
pub const CLAP_PARAM_IS_STEPPED: u32 = 1 << 0;
/// `CLAP_PARAM_IS_PERIODIC`.
pub const CLAP_PARAM_IS_PERIODIC: u32 = 1 << 1;
/// `CLAP_PARAM_IS_HIDDEN`.
pub const CLAP_PARAM_IS_HIDDEN: u32 = 1 << 2;
/// `CLAP_PARAM_IS_READONLY`.
pub const CLAP_PARAM_IS_READONLY: u32 = 1 << 3;
/// `CLAP_PARAM_IS_BYPASS`.
pub const CLAP_PARAM_IS_BYPASS: u32 = 1 << 4;
/// `CLAP_PARAM_IS_AUTOMATABLE`.
pub const CLAP_PARAM_IS_AUTOMATABLE: u32 = 1 << 5;
/// `CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID`.
pub const CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID: u32 = 1 << 6;
/// `CLAP_PARAM_IS_AUTOMATABLE_PER_KEY`.
pub const CLAP_PARAM_IS_AUTOMATABLE_PER_KEY: u32 = 1 << 7;
/// `CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL`.
pub const CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL: u32 = 1 << 8;
/// `CLAP_PARAM_IS_AUTOMATABLE_PER_PORT`.
pub const CLAP_PARAM_IS_AUTOMATABLE_PER_PORT: u32 = 1 << 9;
/// `CLAP_PARAM_IS_MODULATABLE`.
pub const CLAP_PARAM_IS_MODULATABLE: u32 = 1 << 10;
/// `CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID`.
pub const CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID: u32 = 1 << 11;
/// `CLAP_PARAM_IS_MODULATABLE_PER_KEY`.
pub const CLAP_PARAM_IS_MODULATABLE_PER_KEY: u32 = 1 << 12;
/// `CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL`.
pub const CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL: u32 = 1 << 13;
/// `CLAP_PARAM_IS_MODULATABLE_PER_PORT`.
pub const CLAP_PARAM_IS_MODULATABLE_PER_PORT: u32 = 1 << 14;
/// `CLAP_PARAM_REQUIRES_PROCESS`.
pub const CLAP_PARAM_REQUIRES_PROCESS: u32 = 1 << 15;
/// `CLAP_PARAM_IS_ENUM`.
pub const CLAP_PARAM_IS_ENUM: u32 = 1 << 16;

/// `CLAP_PARAM_RESCAN_VALUES`.
pub const CLAP_PARAM_RESCAN_VALUES: u32 = 1 << 0;
/// `CLAP_PARAM_RESCAN_TEXT`.
pub const CLAP_PARAM_RESCAN_TEXT: u32 = 1 << 1;
/// `CLAP_PARAM_RESCAN_INFO`.
pub const CLAP_PARAM_RESCAN_INFO: u32 = 1 << 2;
/// `CLAP_PARAM_RESCAN_ALL`.
pub const CLAP_PARAM_RESCAN_ALL: u32 = 1 << 3;

/// `clap_param_info_t`: one parameter, filled in by the plug-in.
#[repr(C)]
pub struct ClapParamInfo {
    /// Permanent parameter id.
    pub id: ClapId,
    /// `CLAP_PARAM_*` bits.
    pub flags: u32,
    /// The plug-in's own cookie, echoed back in parameter events. Always null here.
    pub cookie: *mut c_void,
    /// NUL-terminated display name.
    pub name: [c_char; CLAP_NAME_SIZE],
    /// NUL-terminated `/`-separated group path.
    pub module: [c_char; CLAP_PATH_SIZE],
    /// Smallest plain value.
    pub min_value: f64,
    /// Largest plain value.
    pub max_value: f64,
    /// Plain value a fresh instance starts at.
    pub default_value: f64,
}

/// `clap_plugin_params_t`.
#[repr(C)]
pub struct ClapPluginParams {
    /// `[main-thread]` Number of parameters.
    pub count: unsafe extern "C" fn(plugin: *const ClapPlugin) -> u32,
    /// `[main-thread]` Fills `info` for the parameter at `index`.
    pub get_info: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        index: u32,
        info: *mut ClapParamInfo,
    ) -> bool,
    /// `[main-thread]` Reads the current plain value.
    pub get_value: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        param_id: ClapId,
        out_value: *mut f64,
    ) -> bool,
    /// `[main-thread]` Formats a plain value into a caller-owned buffer.
    pub value_to_text: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        param_id: ClapId,
        value: f64,
        out_buffer: *mut c_char,
        out_buffer_capacity: u32,
    ) -> bool,
    /// `[main-thread]` Parses user text into a plain value.
    pub text_to_value: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        param_id: ClapId,
        param_value_text: *const c_char,
        out_value: *mut f64,
    ) -> bool,
    /// `[active ? audio-thread : main-thread]` Delivers parameter changes outside `process`.
    pub flush: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        input: *const ClapInputEvents,
        output: *const ClapOutputEvents,
    ),
}

/// `clap_host_params_t`: the host half of the parameters extension.
#[repr(C)]
pub struct ClapHostParams {
    /// `[main-thread]` The plug-in's parameter set changed in the given ways.
    pub rescan: Option<unsafe extern "C" fn(host: *const ClapHost, flags: u32)>,
    /// `[main-thread]` Clear automation or modulation for one parameter.
    pub clear: Option<unsafe extern "C" fn(host: *const ClapHost, param_id: ClapId, flags: u32)>,
    /// `[thread-safe]` Ask the host to call `flush` soon.
    pub request_flush: Option<unsafe extern "C" fn(host: *const ClapHost)>,
}

// ---------------------------------------------------------------------------------------
// state extension
// ---------------------------------------------------------------------------------------

/// `clap_istream_t`: a byte source owned by the host.
#[repr(C)]
pub struct ClapIStream {
    /// Opaque host context.
    pub ctx: *mut c_void,
    /// Reads up to `size` bytes; `0` is end of stream and a negative value is an error.
    pub read: Option<
        unsafe extern "C" fn(stream: *const ClapIStream, buffer: *mut c_void, size: u64) -> i64,
    >,
}

/// `clap_ostream_t`: a byte sink owned by the host.
#[repr(C)]
pub struct ClapOStream {
    /// Opaque host context.
    pub ctx: *mut c_void,
    /// Writes up to `size` bytes; a negative value is an error.
    pub write: Option<
        unsafe extern "C" fn(stream: *const ClapOStream, buffer: *const c_void, size: u64) -> i64,
    >,
}

/// `clap_plugin_state_t`.
#[repr(C)]
pub struct ClapPluginState {
    /// `[main-thread]` Writes everything needed to reproduce this instance.
    pub save: unsafe extern "C" fn(plugin: *const ClapPlugin, stream: *const ClapOStream) -> bool,
    /// `[main-thread]` Restores what `save` wrote.
    pub load: unsafe extern "C" fn(plugin: *const ClapPlugin, stream: *const ClapIStream) -> bool,
}

// ---------------------------------------------------------------------------------------
// gui extension
// ---------------------------------------------------------------------------------------

/// `clap_window_t`'s payload: the platform handle the host is lending the editor.
#[repr(C)]
pub union ClapWindowHandle {
    /// An `NSView *` on macOS.
    pub cocoa: *mut c_void,
    /// An X11 `Window` id.
    pub x11: c_ulong,
    /// An `HWND` on Windows.
    pub win32: *mut c_void,
    /// The same bits as an untyped pointer.
    pub ptr: *mut c_void,
}

/// `clap_window_t`: a window API name plus the handle it describes.
#[repr(C)]
pub struct ClapWindow {
    /// One of the `CLAP_WINDOW_API_*` strings.
    pub api: *const c_char,
    /// The handle itself, interpreted according to `api`.
    pub handle: ClapWindowHandle,
}

/// `clap_gui_resize_hints_t`: how the host may resize the editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ClapGuiResizeHints {
    /// Whether width may change.
    pub can_resize_horizontally: bool,
    /// Whether height may change.
    pub can_resize_vertically: bool,
    /// Whether the aspect ratio below must be preserved.
    pub preserve_aspect_ratio: bool,
    /// Numerator of the preserved aspect ratio.
    pub aspect_ratio_width: u32,
    /// Denominator of the preserved aspect ratio.
    pub aspect_ratio_height: u32,
}

/// `clap_plugin_gui_t`. Every method is `[main-thread]`.
#[repr(C)]
pub struct ClapPluginGui {
    /// Whether the editor can live under that window API.
    pub is_api_supported: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        api: *const c_char,
        is_floating: bool,
    ) -> bool,
    /// The API and floating-ness the editor would rather have.
    pub get_preferred_api: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        api: *mut *const c_char,
        is_floating: *mut bool,
    ) -> bool,
    /// Creates the editor object; the window arrives later via `set_parent`.
    pub create: unsafe extern "C" fn(
        plugin: *const ClapPlugin,
        api: *const c_char,
        is_floating: bool,
    ) -> bool,
    /// Destroys the editor.
    pub destroy: unsafe extern "C" fn(plugin: *const ClapPlugin),
    /// Sets the display scale factor.
    pub set_scale: unsafe extern "C" fn(plugin: *const ClapPlugin, scale: f64) -> bool,
    /// Reads the editor's current size in physical pixels.
    pub get_size:
        unsafe extern "C" fn(plugin: *const ClapPlugin, width: *mut u32, height: *mut u32) -> bool,
    /// Whether the user may resize the editor.
    pub can_resize: unsafe extern "C" fn(plugin: *const ClapPlugin) -> bool,
    /// Fills the resize hints.
    pub get_resize_hints:
        unsafe extern "C" fn(plugin: *const ClapPlugin, hints: *mut ClapGuiResizeHints) -> bool,
    /// Rounds a proposed size to one the editor accepts.
    pub adjust_size:
        unsafe extern "C" fn(plugin: *const ClapPlugin, width: *mut u32, height: *mut u32) -> bool,
    /// Applies a new size.
    pub set_size: unsafe extern "C" fn(plugin: *const ClapPlugin, width: u32, height: u32) -> bool,
    /// Embeds the editor in the host's window.
    pub set_parent:
        unsafe extern "C" fn(plugin: *const ClapPlugin, window: *const ClapWindow) -> bool,
    /// Marks a floating editor transient for the host's window.
    pub set_transient:
        unsafe extern "C" fn(plugin: *const ClapPlugin, window: *const ClapWindow) -> bool,
    /// Suggests a window title for a floating editor.
    pub suggest_title: unsafe extern "C" fn(plugin: *const ClapPlugin, title: *const c_char),
    /// Shows the editor.
    pub show: unsafe extern "C" fn(plugin: *const ClapPlugin) -> bool,
    /// Hides the editor.
    pub hide: unsafe extern "C" fn(plugin: *const ClapPlugin) -> bool,
}

/// `clap_host_gui_t`: the host half of the GUI extension.
#[repr(C)]
pub struct ClapHostGui {
    /// `[main-thread]` The editor's resize constraints changed.
    pub resize_hints_changed: Option<unsafe extern "C" fn(host: *const ClapHost)>,
    /// `[main-thread]` Ask the host to resize its window.
    pub request_resize:
        Option<unsafe extern "C" fn(host: *const ClapHost, width: u32, height: u32) -> bool>,
    /// `[main-thread]` Ask the host to show the editor.
    pub request_show: Option<unsafe extern "C" fn(host: *const ClapHost) -> bool>,
    /// `[main-thread]` Ask the host to hide the editor.
    pub request_hide: Option<unsafe extern "C" fn(host: *const ClapHost) -> bool>,
    /// `[main-thread]` Tell the host the editor closed itself.
    pub closed: Option<unsafe extern "C" fn(host: *const ClapHost, was_destroyed: bool)>,
}

// ---------------------------------------------------------------------------------------
// latency, tail, render
// ---------------------------------------------------------------------------------------

/// `clap_plugin_latency_t`.
#[repr(C)]
pub struct ClapPluginLatency {
    /// `[main-thread]` Latency in samples at the activated sample rate.
    pub get: unsafe extern "C" fn(plugin: *const ClapPlugin) -> u32,
}

/// `clap_host_latency_t`.
#[repr(C)]
pub struct ClapHostLatency {
    /// `[main-thread]` The plug-in's latency changed and must be re-read.
    pub changed: Option<unsafe extern "C" fn(host: *const ClapHost)>,
}

/// `clap_plugin_tail_t`.
#[repr(C)]
pub struct ClapPluginTail {
    /// `[main-thread or audio-thread]` Tail length in samples; `UINT32_MAX` is infinite.
    pub get: unsafe extern "C" fn(plugin: *const ClapPlugin) -> u32,
}

/// `clap_host_tail_t`.
#[repr(C)]
pub struct ClapHostTail {
    /// `[audio-thread]` The plug-in's tail changed.
    pub changed: Option<unsafe extern "C" fn(host: *const ClapHost)>,
}

/// `CLAP_RENDER_REALTIME`.
pub const CLAP_RENDER_REALTIME: i32 = 0;
/// `CLAP_RENDER_OFFLINE`.
pub const CLAP_RENDER_OFFLINE: i32 = 1;

/// `clap_plugin_render_t`.
#[repr(C)]
pub struct ClapPluginRender {
    /// `[main-thread]` Whether the plug-in must run in real time to be correct.
    pub has_hard_realtime_requirement: unsafe extern "C" fn(plugin: *const ClapPlugin) -> bool,
    /// `[main-thread]` Selects real-time or offline rendering.
    pub set: unsafe extern "C" fn(plugin: *const ClapPlugin, mode: i32) -> bool,
}

// ---------------------------------------------------------------------------------------
// host log / thread check
// ---------------------------------------------------------------------------------------

/// `CLAP_LOG_DEBUG`.
pub const CLAP_LOG_DEBUG: i32 = 0;
/// `CLAP_LOG_INFO`.
pub const CLAP_LOG_INFO: i32 = 1;
/// `CLAP_LOG_WARNING`.
pub const CLAP_LOG_WARNING: i32 = 2;
/// `CLAP_LOG_ERROR`.
pub const CLAP_LOG_ERROR: i32 = 3;
/// `CLAP_LOG_FATAL`.
pub const CLAP_LOG_FATAL: i32 = 4;

/// `clap_host_log_t`.
#[repr(C)]
pub struct ClapHostLog {
    /// `[thread-safe]` Writes one message to the host's log.
    pub log: Option<unsafe extern "C" fn(host: *const ClapHost, severity: i32, msg: *const c_char)>,
}

/// `clap_host_thread_check_t`.
#[repr(C)]
pub struct ClapHostThreadCheck {
    /// `[thread-safe]` Whether the caller is on the host's main thread.
    pub is_main_thread: Option<unsafe extern "C" fn(host: *const ClapHost) -> bool>,
    /// `[thread-safe]` Whether the caller is on an audio thread.
    pub is_audio_thread: Option<unsafe extern "C" fn(host: *const ClapHost) -> bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    /// The event structs are read straight out of a host's memory, so their layout is the
    /// contract. These numbers are what a C compiler produces for the CLAP 1.2 headers on
    /// every 64-bit target DAUxPlug supports.
    #[test]
    fn event_layout_matches_the_c_headers() {
        assert_eq!(size_of::<ClapEventHeader>(), 16);
        assert_eq!(align_of::<ClapEventHeader>(), 4);
        assert_eq!(size_of::<ClapEventNote>(), 40);
        assert_eq!(size_of::<ClapEventNoteExpression>(), 40);
        assert_eq!(size_of::<ClapEventParamValue>(), 56);
        assert_eq!(size_of::<ClapEventParamMod>(), 56);
        assert_eq!(size_of::<ClapEventParamGesture>(), 20);
        assert_eq!(size_of::<ClapEventMidi>(), 24);
        assert_eq!(size_of::<ClapEventMidi2>(), 36);
        assert_eq!(size_of::<ClapEventTransport>(), 104);
    }

    /// A host reads `header.size` and steps that far to the next event, so the header must
    /// sit at offset zero of every concrete event and nothing may precede it.
    #[test]
    fn every_event_begins_with_its_header() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(ClapEventNote, header), 0);
        assert_eq!(offset_of!(ClapEventNoteExpression, header), 0);
        assert_eq!(offset_of!(ClapEventParamValue, header), 0);
        assert_eq!(offset_of!(ClapEventParamMod, header), 0);
        assert_eq!(offset_of!(ClapEventParamGesture, header), 0);
        assert_eq!(offset_of!(ClapEventTransport, header), 0);
        assert_eq!(offset_of!(ClapEventMidi, header), 0);
        assert_eq!(offset_of!(ClapEventMidiSysex, header), 0);
        assert_eq!(offset_of!(ClapEventMidi2, header), 0);
    }

    /// The fields a decoder reads by offset rather than by name, because a host wrote them
    /// with a C compiler.
    #[test]
    fn the_field_offsets_a_decoder_depends_on_are_the_c_ones() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(ClapEventHeader, size), 0);
        assert_eq!(offset_of!(ClapEventHeader, time), 4);
        assert_eq!(offset_of!(ClapEventHeader, space_id), 8);
        assert_eq!(offset_of!(ClapEventHeader, type_), 10);
        assert_eq!(offset_of!(ClapEventHeader, flags), 12);

        assert_eq!(offset_of!(ClapEventNote, note_id), 16);
        assert_eq!(offset_of!(ClapEventNote, port_index), 20);
        assert_eq!(offset_of!(ClapEventNote, channel), 22);
        assert_eq!(offset_of!(ClapEventNote, key), 24);
        assert_eq!(offset_of!(ClapEventNote, velocity), 32);

        assert_eq!(offset_of!(ClapEventTransport, flags), 16);
        assert_eq!(offset_of!(ClapEventTransport, song_pos_beats), 24);
        assert_eq!(offset_of!(ClapEventTransport, bar_number), 96);
        assert_eq!(offset_of!(ClapEventTransport, tsig_num), 100);
        assert_eq!(offset_of!(ClapEventTransport, tsig_denom), 102);
    }

    /// A header must be readable from an under-aligned position inside a host's event
    /// arena, so nothing in the event structs may demand more than 8-byte alignment.
    #[test]
    fn no_event_struct_over_aligns() {
        assert!(align_of::<ClapEventNote>() <= 8);
        assert!(align_of::<ClapEventNoteExpression>() <= 8);
        assert!(align_of::<ClapEventParamValue>() <= 8);
        assert!(align_of::<ClapEventParamMod>() <= 8);
        assert!(align_of::<ClapEventTransport>() <= 8);
        assert!(align_of::<ClapEventMidiSysex>() <= 8);
    }

    #[test]
    fn the_version_we_publish_is_a_compatible_one() {
        assert!(ClapVersion::CURRENT.is_compatible());
        assert_eq!(ClapVersion::CURRENT.major, 1);
        assert!(
            !ClapVersion {
                major: 2,
                minor: 0,
                revision: 0
            }
            .is_compatible()
        );
    }

    /// Every id is compared against a host-supplied C string, so a stray NUL or a typo'd
    /// prefix would silently disable an extension.
    #[test]
    fn extension_ids_are_the_published_strings() {
        assert_eq!(CLAP_PLUGIN_FACTORY_ID.to_bytes(), b"clap.plugin-factory");
        assert_eq!(CLAP_EXT_AUDIO_PORTS.to_bytes(), b"clap.audio-ports");
        assert_eq!(CLAP_EXT_NOTE_PORTS.to_bytes(), b"clap.note-ports");
        assert_eq!(CLAP_EXT_PARAMS.to_bytes(), b"clap.params");
        assert_eq!(CLAP_EXT_STATE.to_bytes(), b"clap.state");
        assert_eq!(CLAP_EXT_GUI.to_bytes(), b"clap.gui");
        assert_eq!(CLAP_EXT_LATENCY.to_bytes(), b"clap.latency");
        assert_eq!(CLAP_EXT_TAIL.to_bytes(), b"clap.tail");
        assert_eq!(CLAP_EXT_RENDER.to_bytes(), b"clap.render");
        assert_eq!(CLAP_EXT_LOG.to_bytes(), b"clap.log");
        assert_eq!(CLAP_EXT_THREAD_CHECK.to_bytes(), b"clap.thread-check");
    }

    /// CLAP's process-status codes and DAUx's `DAUX_PROCESS_*` happen to agree. That is
    /// load-bearing — the mapping in `convert` relies on it — so it is asserted rather than
    /// assumed.
    #[test]
    fn process_status_codes_line_up_with_the_daux_ones() {
        use daux_plugin_api::ProcessStatus;
        assert_eq!(ProcessStatus::Error.code(), CLAP_PROCESS_ERROR);
        assert_eq!(ProcessStatus::Continue.code(), CLAP_PROCESS_CONTINUE);
        assert_eq!(
            ProcessStatus::ContinueIfNotQuiet.code(),
            CLAP_PROCESS_CONTINUE_IF_NOT_QUIET
        );
        assert_eq!(ProcessStatus::Tail.code(), CLAP_PROCESS_TAIL);
        assert_eq!(ProcessStatus::Sleep.code(), CLAP_PROCESS_SLEEP);
    }
}
