//! The VST3 interfaces, structures and constants this adapter implements or consumes.
//!
//! Everything here is a transcription of Steinberg's public C++ headers into `#[repr(C)]`
//! Rust: vtable layouts, plain-old-data structures and the enumerations that travel in their
//! `int32` fields. There is no C++ here, no bindgen, and no dependency on the Steinberg SDK —
//! the ABI is what it is regardless of which language emits it.
//!
//! Interfaces are split by who implements them:
//!
//! | Implemented by this crate | Implemented by the host |
//! |---|---|
//! | `IPluginFactory`, `IPluginFactory2`, `IPluginFactory3` | `IHostApplication` |
//! | `IComponent`, `IAudioProcessor`, `IEditController` | `IComponentHandler` |
//! | `IConnectionPoint`, `IPlugView` | `IPlugFrame`, `IBStream` |
//! | | `IParameterChanges`, `IParamValueQueue`, `IEventList` |
//!
//! Every vtable repeats `FUnknown`'s three slots first and, where the C++ interface derives
//! from `IPluginBase`, that base's two slots second. That is what C++ single inheritance
//! produces and therefore what the host expects to find.

#![allow(clippy::doc_markdown)]

use core::ffi::c_void;

use crate::com::{Char16, FidString, TBool, TResult, TUid, uid};

// ---------------------------------------------------------------------------------------
// Interface identifiers
// ---------------------------------------------------------------------------------------

/// `FUnknown`, the root of every VST3 interface.
pub const IFUNKNOWN_IID: TUid = uid(0x0000_0000, 0x0000_0000, 0xC000_0000, 0x0000_0046);
/// `IPluginBase`, the `initialize`/`terminate` pair shared by component and controller.
pub const IPLUGIN_BASE_IID: TUid = uid(0x5BC1_1507, 0xD8F9_4748, 0x8B3A_AF9D, 0x04E2_B3B6);
/// `IPluginFactory`.
pub const IPLUGIN_FACTORY_IID: TUid = uid(0x7A4D_811C, 0x5211_4A1F, 0xAED9_D2EE, 0x0B43_BF9F);
/// `IPluginFactory2`.
pub const IPLUGIN_FACTORY2_IID: TUid = uid(0x0007_B650, 0xF24B_4C0B, 0xA464_EDB9, 0xF00B_2ABB);
/// `IPluginFactory3`.
pub const IPLUGIN_FACTORY3_IID: TUid = uid(0x4555_A2AB, 0xC123_4E57, 0x9B12_2910, 0x3687_8931);
/// `Vst::IComponent`.
pub const ICOMPONENT_IID: TUid = uid(0xE831_FF31, 0xF2D5_4301, 0x928E_BBEE, 0x2569_7802);
/// `Vst::IAudioProcessor`.
pub const IAUDIO_PROCESSOR_IID: TUid = uid(0x4204_3F99, 0xB7DA_453C, 0xA569_E79D, 0x9AAE_C33D);
/// `Vst::IEditController`.
pub const IEDIT_CONTROLLER_IID: TUid = uid(0xDCD7_BBE3, 0x7742_448D, 0xA874_AACC, 0x979C_759E);
/// `Vst::IConnectionPoint`.
pub const ICONNECTION_POINT_IID: TUid = uid(0x70A4_156F, 0x6E6E_4026, 0x9891_48BF, 0xAA60_D8D1);
/// `IPlugView`.
pub const IPLUG_VIEW_IID: TUid = uid(0x5BC3_2507, 0xD060_49EA, 0xA615_1B52, 0x2B75_5B29);
/// `IPlugFrame`, the host side of a view.
pub const IPLUG_FRAME_IID: TUid = uid(0x367F_AF01, 0xAFA9_4693, 0x8D4D_A2A0, 0xED08_82A3);
/// `IBStream`, the host's byte stream for state.
pub const IBSTREAM_IID: TUid = uid(0xC3BF_6EA2, 0x3099_4752, 0x9B6B_F990, 0x1EE3_3E9B);
/// `Vst::IComponentHandler`, the host's automation sink.
pub const ICOMPONENT_HANDLER_IID: TUid = uid(0x93A0_BEA3, 0x0BD0_45DB, 0x8E89_0B0C, 0xC1E4_6AC6);
/// `Vst::IParameterChanges`.
pub const IPARAMETER_CHANGES_IID: TUid = uid(0xA477_9663, 0x0BB6_4A56, 0xB443_84A8, 0x466F_EB9D);
/// `Vst::IParamValueQueue`.
pub const IPARAM_VALUE_QUEUE_IID: TUid = uid(0x0126_3A18, 0xED07_4F6F, 0x98C9_D356, 0x4686_F9BA);
/// `Vst::IEventList`.
pub const IEVENT_LIST_IID: TUid = uid(0x3A2C_4214, 0x3463_49D9, 0xB2B5_FB6C, 0x725F_5CC0);
/// `Vst::IHostApplication`.
pub const IHOST_APPLICATION_IID: TUid = uid(0x58E5_95CC, 0x9B10_49E8, 0x8B5D_8C8B, 0x5F63_E31C);
/// `Vst::IMessage`, the payload of `IConnectionPoint::notify`.
pub const IMESSAGE_IID: TUid = uid(0x936F_033B, 0xC6C0_47DB, 0xBB08_2F81, 0x3985_2929);

// ---------------------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------------------

/// Cardinality value meaning "as many instances as the host likes".
pub const K_MANY_INSTANCES: i32 = 0x7FFF_FFFF;

/// `PClassInfo::category` for an audio plug-in.
pub const K_VST_AUDIO_EFFECT_CLASS: &str = "Audio Module Class";

/// The SDK version string a host reads out of `PClassInfo2`.
///
/// This adapter targets the VST 3.7.x interface set; the string is what hosts show in their
/// plug-in manager and what some of them parse to decide which optional interfaces to try.
pub const K_VST_SDK_VERSION: &str = "VST 3.7.0";

/// `PFactoryInfo::flags`.
pub mod factory_flags {
    /// No flags.
    pub const NO_FLAGS: i32 = 0;
    /// The host may unload the module when no instance is alive.
    pub const CLASSES_DISCARDABLE: i32 = 1 << 0;
    /// The plug-in performs a licence check.
    pub const LICENSE_CHECK: i32 = 1 << 1;
    /// The module must not be unloaded once loaded.
    pub const COMPONENT_NON_DISCARDABLE: i32 = 1 << 3;
    /// Strings in `PFactoryInfo` are UTF-8.
    pub const UNICODE: i32 = 1 << 4;
}

/// `PClassInfo2::classFlags` for an audio plug-in.
pub mod component_flags {
    /// `Vst::kDistributable` — the component and the edit controller may run in different
    /// processes. This adapter never sets it: see the crate documentation for why the two
    /// halves stay in one object.
    pub const DISTRIBUTABLE: u32 = 1 << 0;
    /// `Vst::kSimpleModeSupported`.
    pub const SIMPLE_MODE_SUPPORTED: u32 = 1 << 1;
}

/// `PFactoryInfo`, the vendor block a host shows in its plug-in manager.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PFactoryInfo {
    /// Vendor name, ASCII, null-terminated.
    pub vendor: [u8; 64],
    /// Vendor URL, ASCII, null-terminated.
    pub url: [u8; 256],
    /// Support e-mail address, ASCII, null-terminated.
    pub email: [u8; 128],
    /// See [`factory_flags`].
    pub flags: i32,
}

/// `PClassInfo`, the minimum description of one exported class.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PClassInfo {
    /// The class id: sixteen raw bytes.
    pub cid: TUid,
    /// How many instances may exist; normally [`K_MANY_INSTANCES`].
    pub cardinality: i32,
    /// Class category, e.g. [`K_VST_AUDIO_EFFECT_CLASS`].
    pub category: [u8; 32],
    /// Display name, ASCII.
    pub name: [u8; 64],
}

/// `PClassInfo2`, adding vendor, version and subcategories.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PClassInfo2 {
    /// The class id.
    pub cid: TUid,
    /// How many instances may exist.
    pub cardinality: i32,
    /// Class category.
    pub category: [u8; 32],
    /// Display name, ASCII.
    pub name: [u8; 64],
    /// See [`component_flags`].
    pub class_flags: u32,
    /// `|`-separated subcategory list, e.g. `"Fx|Filter"`.
    pub subcategories: [u8; 128],
    /// Vendor name.
    pub vendor: [u8; 64],
    /// Product version string.
    pub version: [u8; 64],
    /// SDK version string.
    pub sdk_version: [u8; 64],
}

/// `PClassInfoW`, `PClassInfo2` with UTF-16 strings.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PClassInfoW {
    /// The class id.
    pub cid: TUid,
    /// How many instances may exist.
    pub cardinality: i32,
    /// Class category, still ASCII.
    pub category: [u8; 32],
    /// Display name, UTF-16.
    pub name: [Char16; 64],
    /// See [`component_flags`].
    pub class_flags: u32,
    /// `|`-separated subcategory list, ASCII.
    pub subcategories: [u8; 128],
    /// Vendor name, UTF-16.
    pub vendor: [Char16; 64],
    /// Product version string, UTF-16.
    pub version: [Char16; 64],
    /// SDK version string, UTF-16.
    pub sdk_version: [Char16; 64],
}

/// `IPluginFactory` + `IPluginFactory2` + `IPluginFactory3`, laid out as C++ would.
///
/// The three are separate interfaces in the SDK but form a single inheritance chain, so one
/// vtable serves all three: a host that queried only `IPluginFactory` will never look past
/// [`Self::create_instance`], and one that queried `IPluginFactory3` finds the later slots
/// where C++ would have put them.
#[repr(C)]
pub struct IPluginFactoryVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// Fills the vendor block.
    pub get_factory_info: unsafe extern "system" fn(*mut c_void, *mut PFactoryInfo) -> TResult,
    /// How many classes this module exports.
    pub count_classes: unsafe extern "system" fn(*mut c_void) -> i32,
    /// Fills a [`PClassInfo`] for one class.
    pub get_class_info: unsafe extern "system" fn(*mut c_void, i32, *mut PClassInfo) -> TResult,
    /// Instantiates a class and hands back the requested interface.
    pub create_instance:
        unsafe extern "system" fn(*mut c_void, FidString, FidString, *mut *mut c_void) -> TResult,
    /// `IPluginFactory2::getClassInfo2`.
    pub get_class_info2: unsafe extern "system" fn(*mut c_void, i32, *mut PClassInfo2) -> TResult,
    /// `IPluginFactory3::getClassInfoUnicode`.
    pub get_class_info_unicode:
        unsafe extern "system" fn(*mut c_void, i32, *mut PClassInfoW) -> TResult,
    /// `IPluginFactory3::setHostContext`.
    pub set_host_context: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
}

// ---------------------------------------------------------------------------------------
// Component / audio processor
// ---------------------------------------------------------------------------------------

/// `Vst::MediaTypes`.
pub mod media_type {
    /// Audio buses.
    pub const AUDIO: i32 = 0;
    /// Event (MIDI) buses.
    pub const EVENT: i32 = 1;
}

/// `Vst::BusDirections`.
pub mod bus_direction {
    /// Input bus.
    pub const INPUT: i32 = 0;
    /// Output bus.
    pub const OUTPUT: i32 = 1;
}

/// `Vst::BusTypes`.
pub mod bus_type {
    /// The bus a host connects by default.
    pub const MAIN: i32 = 0;
    /// Any other bus, including sidechains.
    pub const AUX: i32 = 1;
}

/// `Vst::BusInfo::BusFlags`.
pub mod bus_flags {
    /// The host should connect this bus when it instantiates the plug-in.
    pub const DEFAULT_ACTIVE: u32 = 1 << 0;
    /// The bus carries control voltage rather than audio.
    pub const IS_CONTROL_VOLTAGE: u32 = 1 << 1;
}

/// `Vst::BusInfo`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BusInfo {
    /// See [`media_type`].
    pub media_type: i32,
    /// See [`bus_direction`].
    pub direction: i32,
    /// Number of channels; always `0` for event buses in VST3 3.7 terms it is the number of
    /// supported MIDI channels.
    pub channel_count: i32,
    /// Display name, UTF-16, null-padded.
    pub name: [Char16; 128],
    /// See [`bus_type`].
    pub bus_type: i32,
    /// See [`bus_flags`].
    pub flags: u32,
}

/// `Vst::RoutingInfo`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RoutingInfo {
    /// See [`media_type`].
    pub media_type: i32,
    /// Index of the bus this routing refers to.
    pub bus_index: i32,
    /// Channel within the bus, or `-1` for the whole bus.
    pub channel: i32,
}

/// `Vst::ProcessSetup::symbolicSampleSize`.
pub mod sample_size {
    /// 32-bit float samples.
    pub const SAMPLE32: i32 = 0;
    /// 64-bit float samples.
    pub const SAMPLE64: i32 = 1;
}

/// `Vst::ProcessModes`.
pub mod process_mode {
    /// Live playback.
    pub const REALTIME: i32 = 0;
    /// The host is filling a cache ahead of playback.
    pub const PREFETCH: i32 = 1;
    /// Faster-than-real-time bounce.
    pub const OFFLINE: i32 = 2;
}

/// `Vst::ProcessSetup`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessSetup {
    /// See [`process_mode`].
    pub process_mode: i32,
    /// See [`sample_size`].
    pub symbolic_sample_size: i32,
    /// Largest block the host will ask for.
    pub max_samples_per_block: i32,
    /// Sample rate in Hz.
    pub sample_rate: f64,
}

/// `Vst::AudioBusBuffers`.
///
/// The C++ original ends in an anonymous union of `Sample32**` and `Sample64**`; both arms
/// are a pointer to an array of pointers, so one field with a cast at the point of use is
/// the same layout and one fewer `unsafe`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBusBuffers {
    /// Channels in this bus.
    pub num_channels: i32,
    /// Bit `c` set means channel `c` is known to be silent for the whole block.
    pub silence_flags: u64,
    /// `Sample32**` or `Sample64**` depending on `ProcessData::symbolicSampleSize`.
    pub channel_buffers: *mut *mut c_void,
}

/// `Vst::FrameRate`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FrameRate {
    /// Frames per second, e.g. 24, 25, 30.
    pub frames_per_second: u32,
    /// Pull-down and drop-frame flags.
    pub flags: u32,
}

/// `Vst::Chord`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Chord {
    /// Key note in the chord.
    pub key_note: u8,
    /// Lowest note in the chord.
    pub root_note: u8,
    /// Bitmask of the chord's intervals.
    pub chord_mask: i16,
}

/// `Vst::ProcessContext::StatesAndFlags`.
pub mod context_state {
    /// The transport is rolling.
    pub const PLAYING: u32 = 1 << 1;
    /// A loop is armed.
    pub const CYCLE_ACTIVE: u32 = 1 << 2;
    /// The host is recording.
    pub const RECORDING: u32 = 1 << 3;
    /// `systemTime` is meaningful.
    pub const SYSTEM_TIME_VALID: u32 = 1 << 8;
    /// `projectTimeMusic` is meaningful.
    pub const PROJECT_TIME_MUSIC_VALID: u32 = 1 << 9;
    /// `tempo` is meaningful.
    pub const TEMPO_VALID: u32 = 1 << 10;
    /// `barPositionMusic` is meaningful.
    pub const BAR_POSITION_VALID: u32 = 1 << 11;
    /// `cycleStartMusic` and `cycleEndMusic` are meaningful.
    pub const CYCLE_VALID: u32 = 1 << 12;
    /// `timeSigNumerator` and `timeSigDenominator` are meaningful.
    pub const TIME_SIG_VALID: u32 = 1 << 13;
    /// SMPTE offset and frame rate are meaningful.
    pub const SMPTE_VALID: u32 = 1 << 14;
    /// `samplesToNextClock` is meaningful.
    pub const CLOCK_VALID: u32 = 1 << 15;
    /// `continousTimeSamples` is meaningful.
    pub const CONT_TIME_VALID: u32 = 1 << 17;
    /// `chord` is meaningful.
    pub const CHORD_VALID: u32 = 1 << 18;
}

/// `Vst::ProcessContext`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessContext {
    /// See [`context_state`].
    pub state: u32,
    /// Sample rate in Hz.
    pub sample_rate: f64,
    /// Playhead position in samples.
    pub project_time_samples: i64,
    /// Host system time in nanoseconds.
    pub system_time: i64,
    /// A sample counter that never runs backwards.
    pub continous_time_samples: i64,
    /// Playhead position in quarter notes.
    pub project_time_music: f64,
    /// Position of the current bar, in quarter notes.
    pub bar_position_music: f64,
    /// Loop start, in quarter notes.
    pub cycle_start_music: f64,
    /// Loop end, in quarter notes.
    pub cycle_end_music: f64,
    /// Tempo in BPM.
    pub tempo: f64,
    /// Time signature numerator.
    pub time_sig_numerator: i32,
    /// Time signature denominator.
    pub time_sig_denominator: i32,
    /// Current chord, when `CHORD_VALID` is set.
    pub chord: Chord,
    /// SMPTE offset in subframes.
    pub smpte_offset_subframes: i32,
    /// SMPTE frame rate.
    pub frame_rate: FrameRate,
    /// Samples until the next MIDI clock.
    pub samples_to_next_clock: i32,
}

/// `Vst::ProcessData`.
#[repr(C)]
pub struct ProcessData {
    /// See [`process_mode`].
    pub process_mode: i32,
    /// See [`sample_size`].
    pub symbolic_sample_size: i32,
    /// Frames in this block.
    pub num_samples: i32,
    /// Number of input buses in `inputs`.
    pub num_inputs: i32,
    /// Number of output buses in `outputs`.
    pub num_outputs: i32,
    /// Input buses, or null when there are none.
    pub inputs: *mut AudioBusBuffers,
    /// Output buses, or null when there are none.
    pub outputs: *mut AudioBusBuffers,
    /// Automation for this block, or null.
    pub input_parameter_changes: *mut c_void,
    /// Where to write parameter changes the plug-in makes, or null.
    pub output_parameter_changes: *mut c_void,
    /// Events for this block, or null.
    pub input_events: *mut c_void,
    /// Where to write events the plug-in makes, or null.
    pub output_events: *mut c_void,
    /// The host's transport, or null.
    pub process_context: *mut ProcessContext,
}

/// `Vst::IComponent`, which derives from `IPluginBase`.
#[repr(C)]
pub struct IComponentVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `IPluginBase::initialize`.
    pub initialize: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// `IPluginBase::terminate`.
    pub terminate: unsafe extern "system" fn(*mut c_void) -> TResult,
    /// The class id of the separate edit controller, if there is one.
    pub get_controller_class_id: unsafe extern "system" fn(*mut c_void, *mut TUid) -> TResult,
    /// Simple/advanced I/O mode; unused by every current host.
    pub set_io_mode: unsafe extern "system" fn(*mut c_void, i32) -> TResult,
    /// Number of buses of a given media type and direction.
    pub get_bus_count: unsafe extern "system" fn(*mut c_void, i32, i32) -> i32,
    /// Description of one bus.
    pub get_bus_info:
        unsafe extern "system" fn(*mut c_void, i32, i32, i32, *mut BusInfo) -> TResult,
    /// Which output a given input feeds, for hosts that ask.
    pub get_routing_info:
        unsafe extern "system" fn(*mut c_void, *mut RoutingInfo, *mut RoutingInfo) -> TResult,
    /// Turns one bus on or off.
    pub activate_bus: unsafe extern "system" fn(*mut c_void, i32, i32, i32, TBool) -> TResult,
    /// Allocates or releases DSP resources.
    pub set_active: unsafe extern "system" fn(*mut c_void, TBool) -> TResult,
    /// Restores component state.
    pub set_state: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// Saves component state.
    pub get_state: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
}

/// `Vst::IAudioProcessor`.
#[repr(C)]
pub struct IAudioProcessorVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// Proposes speaker arrangements for every bus at once.
    pub set_bus_arrangements:
        unsafe extern "system" fn(*mut c_void, *mut u64, i32, *mut u64, i32) -> TResult,
    /// Reads back the arrangement of one bus.
    pub get_bus_arrangement: unsafe extern "system" fn(*mut c_void, i32, i32, *mut u64) -> TResult,
    /// Whether 32- or 64-bit samples are supported.
    pub can_process_sample_size: unsafe extern "system" fn(*mut c_void, i32) -> TResult,
    /// Latency in samples.
    pub get_latency_samples: unsafe extern "system" fn(*mut c_void) -> u32,
    /// Announces the sample rate, block size and sample format.
    pub setup_processing: unsafe extern "system" fn(*mut c_void, *mut ProcessSetup) -> TResult,
    /// Arms or disarms the audio thread.
    pub set_processing: unsafe extern "system" fn(*mut c_void, TBool) -> TResult,
    /// Processes one block.
    pub process: unsafe extern "system" fn(*mut c_void, *mut ProcessData) -> TResult,
    /// Tail length in samples.
    pub get_tail_samples: unsafe extern "system" fn(*mut c_void) -> u32,
}

/// `Vst::kNoTail`: output is silent as soon as input is.
pub const K_NO_TAIL: u32 = 0;
/// `Vst::kInfiniteTail`: the plug-in never stops producing.
pub const K_INFINITE_TAIL: u32 = u32::MAX;

// ---------------------------------------------------------------------------------------
// Edit controller
// ---------------------------------------------------------------------------------------

/// `Vst::ParameterInfo::ParameterFlags`.
pub mod param_flags {
    /// No flags.
    pub const NO_FLAGS: i32 = 0;
    /// The host may record and play back automation.
    pub const CAN_AUTOMATE: i32 = 1 << 0;
    /// The host must not write this parameter.
    pub const IS_READ_ONLY: i32 = 1 << 1;
    /// The control wraps around at its ends.
    pub const IS_WRAP_AROUND: i32 = 1 << 2;
    /// The parameter is a list of named values.
    pub const IS_LIST: i32 = 1 << 3;
    /// The parameter exists but generic UIs should not show it.
    pub const IS_HIDDEN: i32 = 1 << 4;
    /// The parameter is a program change.
    pub const IS_PROGRAM_CHANGE: i32 = 1 << 15;
    /// The parameter is the plug-in's bypass.
    pub const IS_BYPASS: i32 = 1 << 16;
}

/// `Vst::ParameterInfo`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ParameterInfo {
    /// The parameter's permanent id.
    pub id: u32,
    /// Display name, UTF-16.
    pub title: [Char16; 128],
    /// Abbreviated name, UTF-16.
    pub short_title: [Char16; 128],
    /// Unit suffix, UTF-16.
    pub units: [Char16; 128],
    /// Number of intervals between discrete values; `0` for continuous.
    pub step_count: i32,
    /// Default value, normalised to `0..=1`.
    pub default_normalized_value: f64,
    /// Which parameter unit (group) this belongs to; `0` is the root.
    pub unit_id: i32,
    /// See [`param_flags`].
    pub flags: i32,
}

/// `Vst::IEditController`, which derives from `IPluginBase`.
#[repr(C)]
pub struct IEditControllerVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `IPluginBase::initialize`.
    pub initialize: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// `IPluginBase::terminate`.
    pub terminate: unsafe extern "system" fn(*mut c_void) -> TResult,
    /// The component's state, so the controller can mirror it.
    pub set_component_state: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// Restores controller-only state.
    pub set_state: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// Saves controller-only state.
    pub get_state: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// Number of parameters.
    pub get_parameter_count: unsafe extern "system" fn(*mut c_void) -> i32,
    /// Description of the parameter at an index.
    pub get_parameter_info:
        unsafe extern "system" fn(*mut c_void, i32, *mut ParameterInfo) -> TResult,
    /// Formats a normalised value as text.
    pub get_param_string_by_value:
        unsafe extern "system" fn(*mut c_void, u32, f64, *mut Char16) -> TResult,
    /// Parses text into a normalised value.
    pub get_param_value_by_string:
        unsafe extern "system" fn(*mut c_void, u32, *mut Char16, *mut f64) -> TResult,
    /// Converts normalised to plain.
    pub normalized_param_to_plain: unsafe extern "system" fn(*mut c_void, u32, f64) -> f64,
    /// Converts plain to normalised.
    pub plain_param_to_normalized: unsafe extern "system" fn(*mut c_void, u32, f64) -> f64,
    /// The controller's current normalised value for a parameter.
    pub get_param_normalized: unsafe extern "system" fn(*mut c_void, u32) -> f64,
    /// Sets the controller's normalised value for a parameter.
    pub set_param_normalized: unsafe extern "system" fn(*mut c_void, u32, f64) -> TResult,
    /// Hands the plug-in the host's automation sink.
    pub set_component_handler: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// Creates an editor.
    pub create_view: unsafe extern "system" fn(*mut c_void, FidString) -> *mut c_void,
}

/// `Vst::IComponentHandler`, implemented by the host.
#[repr(C)]
pub struct IComponentHandlerVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// The user grabbed a control.
    pub begin_edit: unsafe extern "system" fn(*mut c_void, u32) -> TResult,
    /// The user moved a control.
    pub perform_edit: unsafe extern "system" fn(*mut c_void, u32, f64) -> TResult,
    /// The user released a control.
    pub end_edit: unsafe extern "system" fn(*mut c_void, u32) -> TResult,
    /// Something the host caches has changed; see [`restart_flags`].
    pub restart_component: unsafe extern "system" fn(*mut c_void, i32) -> TResult,
}

/// `Vst::RestartFlags`.
pub mod restart_flags {
    /// The whole component must be reloaded.
    pub const RELOAD_COMPONENT: i32 = 1 << 0;
    /// Bus topology changed.
    pub const IO_CHANGED: i32 = 1 << 1;
    /// Parameter values changed behind the host's back.
    pub const PARAM_VALUES_CHANGED: i32 = 1 << 2;
    /// Latency changed and the host must re-read it.
    pub const LATENCY_CHANGED: i32 = 1 << 3;
    /// Parameter names or ranges changed.
    pub const PARAM_TITLES_CHANGED: i32 = 1 << 4;
}

// ---------------------------------------------------------------------------------------
// Connection point
// ---------------------------------------------------------------------------------------

/// `Vst::IConnectionPoint`.
#[repr(C)]
pub struct IConnectionPointVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// The host pairs this object with another connection point.
    pub connect: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// The pairing is dissolved.
    pub disconnect: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// A message arrives from the peer.
    pub notify: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
}

// ---------------------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------------------

/// `ViewRect`, in physical pixels on Windows and Linux and in points on macOS.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ViewRect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge, exclusive.
    pub right: i32,
    /// Bottom edge, exclusive.
    pub bottom: i32,
}

impl ViewRect {
    /// `[main-thread]` A rect anchored at the origin.
    #[must_use]
    pub const fn sized(width: i32, height: i32) -> Self {
        Self {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }

    /// `[main-thread]` Width, never negative.
    #[must_use]
    pub const fn width(self) -> i32 {
        if self.right > self.left {
            self.right - self.left
        } else {
            0
        }
    }

    /// `[main-thread]` Height, never negative.
    #[must_use]
    pub const fn height(self) -> i32 {
        if self.bottom > self.top {
            self.bottom - self.top
        } else {
            0
        }
    }
}

/// `kPlatformTypeHWND`.
pub const PLATFORM_TYPE_HWND: &[u8] = b"HWND\0";
/// `kPlatformTypeNSView`.
pub const PLATFORM_TYPE_NSVIEW: &[u8] = b"NSView\0";
/// `kPlatformTypeX11EmbedWindowID`.
pub const PLATFORM_TYPE_X11: &[u8] = b"X11EmbedWindowID\0";

/// `IPlugView`.
#[repr(C)]
pub struct IPlugViewVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// Whether the editor can live in a window of this platform type.
    pub is_platform_type_supported: unsafe extern "system" fn(*mut c_void, FidString) -> TResult,
    /// The host has a window ready; create rendering resources in it.
    pub attached: unsafe extern "system" fn(*mut c_void, *mut c_void, FidString) -> TResult,
    /// The window is going away.
    pub removed: unsafe extern "system" fn(*mut c_void) -> TResult,
    /// A scroll wheel moved.
    pub on_wheel: unsafe extern "system" fn(*mut c_void, f32) -> TResult,
    /// A key went down.
    pub on_key_down: unsafe extern "system" fn(*mut c_void, Char16, i16, i16) -> TResult,
    /// A key came up.
    pub on_key_up: unsafe extern "system" fn(*mut c_void, Char16, i16, i16) -> TResult,
    /// The editor's preferred size.
    pub get_size: unsafe extern "system" fn(*mut c_void, *mut ViewRect) -> TResult,
    /// The host resized the window.
    pub on_size: unsafe extern "system" fn(*mut c_void, *mut ViewRect) -> TResult,
    /// The window gained or lost focus.
    pub on_focus: unsafe extern "system" fn(*mut c_void, TBool) -> TResult,
    /// The host's frame, used to request resizes.
    pub set_frame: unsafe extern "system" fn(*mut c_void, *mut c_void) -> TResult,
    /// Whether the user may resize the window.
    pub can_resize: unsafe extern "system" fn(*mut c_void) -> TResult,
    /// Adjusts a proposed size to one the editor accepts.
    pub check_size_constraint: unsafe extern "system" fn(*mut c_void, *mut ViewRect) -> TResult,
}

/// `IPlugFrame`, implemented by the host.
#[repr(C)]
pub struct IPlugFrameVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// Asks the host to resize the window the view is in.
    pub resize_view: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut ViewRect) -> TResult,
}

// ---------------------------------------------------------------------------------------
// Streams, parameter changes and events (host-implemented)
// ---------------------------------------------------------------------------------------

/// `IBStream::IStreamSeekMode`.
pub mod seek_mode {
    /// Relative to the start of the stream.
    pub const SET: i32 = 0;
    /// Relative to the current position.
    pub const CUR: i32 = 1;
    /// Relative to the end of the stream.
    pub const END: i32 = 2;
}

/// `IBStream`, implemented by the host.
#[repr(C)]
pub struct IBStreamVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// Reads up to `numBytes`, reporting how many arrived.
    pub read: unsafe extern "system" fn(*mut c_void, *mut c_void, i32, *mut i32) -> TResult,
    /// Writes up to `numBytes`, reporting how many were taken.
    pub write: unsafe extern "system" fn(*mut c_void, *mut c_void, i32, *mut i32) -> TResult,
    /// Moves the cursor.
    pub seek: unsafe extern "system" fn(*mut c_void, i64, i32, *mut i64) -> TResult,
    /// Reports the cursor.
    pub tell: unsafe extern "system" fn(*mut c_void, *mut i64) -> TResult,
}

/// `Vst::IParamValueQueue`, implemented by the host.
#[repr(C)]
pub struct IParamValueQueueVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// Which parameter this queue belongs to.
    pub get_parameter_id: unsafe extern "system" fn(*mut c_void) -> u32,
    /// How many automation points it holds.
    pub get_point_count: unsafe extern "system" fn(*mut c_void) -> i32,
    /// Reads one point: a sample offset and a normalised value.
    pub get_point: unsafe extern "system" fn(*mut c_void, i32, *mut i32, *mut f64) -> TResult,
    /// Appends one point.
    pub add_point: unsafe extern "system" fn(*mut c_void, i32, f64, *mut i32) -> TResult,
}

/// `Vst::IParameterChanges`, implemented by the host.
#[repr(C)]
pub struct IParameterChangesVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// How many parameters changed in this block.
    pub get_parameter_count: unsafe extern "system" fn(*mut c_void) -> i32,
    /// The queue at an index; borrowed, never released by the caller.
    pub get_parameter_data: unsafe extern "system" fn(*mut c_void, i32) -> *mut c_void,
    /// Creates or finds the queue for a parameter id.
    pub add_parameter_data:
        unsafe extern "system" fn(*mut c_void, *const u32, *mut i32) -> *mut c_void,
}

/// `Vst::Event::EventTypes`.
pub mod event_type {
    /// `NoteOnEvent`.
    pub const NOTE_ON: u16 = 0;
    /// `NoteOffEvent`.
    pub const NOTE_OFF: u16 = 1;
    /// `DataEvent`, which carries SysEx.
    pub const DATA: u16 = 2;
    /// `PolyPressureEvent`.
    pub const POLY_PRESSURE: u16 = 3;
    /// `NoteExpressionValueEvent`.
    pub const NOTE_EXPRESSION_VALUE: u16 = 4;
    /// `NoteExpressionTextEvent`.
    pub const NOTE_EXPRESSION_TEXT: u16 = 5;
    /// `ChordEvent`.
    pub const CHORD: u16 = 6;
    /// `ScaleEvent`.
    pub const SCALE: u16 = 7;
    /// `LegacyMIDICCOutEvent`.
    pub const LEGACY_MIDI_CC_OUT: u16 = 65535;
}

/// `Vst::Event::EventFlags`.
pub mod event_flags {
    /// The event was played live rather than read from the timeline.
    pub const IS_LIVE: u16 = 1 << 0;
}

/// `Vst::DataEvent::DataTypes::kMidiSysEx`.
pub const K_MIDI_SYSEX: u32 = 0;

/// `Vst::NoteExpressionTypeIDs`.
pub mod note_expression_type {
    /// Per-note volume, `0..=1`.
    pub const VOLUME: u32 = 0;
    /// Per-note pan, `0..=1` with `0.5` centred.
    pub const PAN: u32 = 1;
    /// Per-note tuning in semitones, `0..=1` mapping `-120..=+120`.
    pub const TUNING: u32 = 2;
    /// Per-note vibrato depth.
    pub const VIBRATO: u32 = 3;
    /// Per-note expression.
    pub const EXPRESSION: u32 = 4;
    /// Per-note brightness.
    pub const BRIGHTNESS: u32 = 5;
    /// Per-note pressure.
    pub const PRESSURE: u32 = 6;
}

/// `Vst::NoteOnEvent`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct NoteOnEvent {
    /// MIDI channel `0..=15`.
    pub channel: i16,
    /// Key number `0..=127`.
    pub pitch: i16,
    /// Detune in cents.
    pub tuning: f32,
    /// Velocity `0..=1`.
    pub velocity: f32,
    /// Length in samples, or `0` when unknown.
    pub length: i32,
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
}

/// `Vst::NoteOffEvent`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct NoteOffEvent {
    /// MIDI channel `0..=15`.
    pub channel: i16,
    /// Key number `0..=127`.
    pub pitch: i16,
    /// Release velocity `0..=1`.
    pub velocity: f32,
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
    /// Detune in cents.
    pub tuning: f32,
}

/// `Vst::DataEvent`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DataEvent {
    /// Payload length in bytes.
    pub size: u32,
    /// Payload type; [`K_MIDI_SYSEX`] is the only standard one.
    pub data_type: u32,
    /// Payload, borrowed for the duration of the block.
    pub bytes: *const u8,
}

/// `Vst::PolyPressureEvent`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PolyPressureEvent {
    /// MIDI channel `0..=15`.
    pub channel: i16,
    /// Key number `0..=127`.
    pub pitch: i16,
    /// Pressure `0..=1`.
    pub pressure: f32,
    /// Host-assigned voice id, or `-1`.
    pub note_id: i32,
}

/// `Vst::NoteExpressionValueEvent`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct NoteExpressionValueEvent {
    /// See [`note_expression_type`].
    pub type_id: u32,
    /// Host-assigned voice id.
    pub note_id: i32,
    /// The value, normalised to `0..=1`.
    pub value: f64,
}

/// The union arm of [`Event`].
///
/// The C++ original is an anonymous union of nine event structs. Only the four this adapter
/// translates are named; the rest are covered by `raw`, which is what keeps the size and
/// alignment right regardless.
#[repr(C)]
#[derive(Clone, Copy)]
pub union EventPayload {
    /// Valid when `type` is [`event_type::NOTE_ON`].
    pub note_on: NoteOnEvent,
    /// Valid when `type` is [`event_type::NOTE_OFF`].
    pub note_off: NoteOffEvent,
    /// Valid when `type` is [`event_type::DATA`].
    pub data: DataEvent,
    /// Valid when `type` is [`event_type::POLY_PRESSURE`].
    pub poly_pressure: PolyPressureEvent,
    /// Valid when `type` is [`event_type::NOTE_EXPRESSION_VALUE`].
    pub note_expression_value: NoteExpressionValueEvent,
    /// The largest arm, so the union's size and alignment match the C++ original whatever
    /// the host puts in it.
    pub raw: [u64; 3],
}

impl Default for EventPayload {
    fn default() -> Self {
        Self { raw: [0; 3] }
    }
}

/// `Vst::Event`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Event {
    /// Which event bus the event belongs to.
    pub bus_index: i32,
    /// Offset from the start of the block, in samples.
    pub sample_offset: i32,
    /// Position in quarter notes, or `0` when the host does not know.
    pub ppq_position: f64,
    /// See [`event_flags`].
    pub flags: u16,
    /// See [`event_type`].
    pub event_type: u16,
    /// The event's payload, selected by `event_type`.
    pub payload: EventPayload,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            bus_index: 0,
            sample_offset: 0,
            ppq_position: 0.0,
            flags: 0,
            event_type: event_type::NOTE_ON,
            payload: EventPayload::default(),
        }
    }
}

/// `Vst::IEventList`, implemented by the host.
#[repr(C)]
pub struct IEventListVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// How many events are in the list.
    pub get_event_count: unsafe extern "system" fn(*mut c_void) -> i32,
    /// Copies one event out.
    pub get_event: unsafe extern "system" fn(*mut c_void, i32, *mut Event) -> TResult,
    /// Appends one event.
    pub add_event: unsafe extern "system" fn(*mut c_void, *mut Event) -> TResult,
}

/// `Vst::IHostApplication`, implemented by the host.
#[repr(C)]
pub struct IHostApplicationVtbl {
    /// `FUnknown::queryInterface`.
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const TUid, *mut *mut c_void) -> TResult,
    /// `FUnknown::addRef`.
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    /// `FUnknown::release`.
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// The host's product name, UTF-16.
    pub get_name: unsafe extern "system" fn(*mut c_void, *mut Char16) -> TResult,
    /// Asks the host to create one of its own objects.
    pub create_instance:
        unsafe extern "system" fn(*mut c_void, *mut TUid, *mut TUid, *mut *mut c_void) -> TResult,
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    /// The layouts a host writes into. If any of these drifts, every field after it is read
    /// from the wrong offset and the failure looks like corrupt audio rather than a bug.
    #[test]
    fn the_shared_structures_have_the_layout_the_sdk_defines() {
        assert_eq!(size_of::<ViewRect>(), 16);
        assert_eq!(size_of::<ProcessSetup>(), 24);
        assert_eq!(size_of::<FrameRate>(), 8);
        assert_eq!(size_of::<Chord>(), 4);
        assert_eq!(align_of::<AudioBusBuffers>(), 8);
        assert_eq!(size_of::<AudioBusBuffers>(), 24);
        // `Event`: 4 + 4 + 8 + 2 + 2 + (4 pad) + 24 = 48 on a 64-bit target.
        assert_eq!(align_of::<Event>(), 8);
        assert_eq!(size_of::<Event>(), 48);
        assert_eq!(size_of::<EventPayload>(), 24);
        assert_eq!(size_of::<NoteOnEvent>(), 20);
        assert_eq!(size_of::<NoteOffEvent>(), 16);
        assert_eq!(size_of::<NoteExpressionValueEvent>(), 16);
        // `ProcessData` is eleven `int32`s worth of header plus seven pointers.
        assert_eq!(align_of::<ProcessData>(), align_of::<*mut c_void>());
    }

    #[test]
    fn the_class_info_blocks_are_fixed_size_records() {
        assert_eq!(size_of::<PFactoryInfo>(), 64 + 256 + 128 + 4);
        assert_eq!(size_of::<PClassInfo>(), 16 + 4 + 32 + 64);
        assert_eq!(
            size_of::<PClassInfo2>(),
            16 + 4 + 32 + 64 + 4 + 128 + 64 + 64 + 64
        );
        assert_eq!(
            size_of::<PClassInfoW>(),
            16 + 4 + 32 + 128 + 4 + 128 + 128 + 128 + 128
        );
    }

    #[test]
    fn every_vtable_is_a_flat_array_of_function_pointers() {
        let word = size_of::<*const c_void>();
        assert_eq!(size_of::<IPluginFactoryVtbl>(), 10 * word);
        assert_eq!(size_of::<IComponentVtbl>(), 14 * word);
        assert_eq!(size_of::<IAudioProcessorVtbl>(), 11 * word);
        assert_eq!(size_of::<IEditControllerVtbl>(), 18 * word);
        assert_eq!(size_of::<IConnectionPointVtbl>(), 6 * word);
        assert_eq!(size_of::<IPlugViewVtbl>(), 15 * word);
        assert_eq!(size_of::<IBStreamVtbl>(), 7 * word);
        assert_eq!(size_of::<IParameterChangesVtbl>(), 6 * word);
        assert_eq!(size_of::<IParamValueQueueVtbl>(), 7 * word);
        assert_eq!(size_of::<IEventListVtbl>(), 6 * word);
        assert_eq!(size_of::<IComponentHandlerVtbl>(), 7 * word);
        assert_eq!(size_of::<IPlugFrameVtbl>(), 4 * word);
        assert_eq!(size_of::<IHostApplicationVtbl>(), 5 * word);
    }

    #[test]
    fn interface_ids_are_distinct() {
        let all = [
            IFUNKNOWN_IID,
            IPLUGIN_BASE_IID,
            IPLUGIN_FACTORY_IID,
            IPLUGIN_FACTORY2_IID,
            IPLUGIN_FACTORY3_IID,
            ICOMPONENT_IID,
            IAUDIO_PROCESSOR_IID,
            IEDIT_CONTROLLER_IID,
            ICONNECTION_POINT_IID,
            IPLUG_VIEW_IID,
            IPLUG_FRAME_IID,
            IBSTREAM_IID,
            ICOMPONENT_HANDLER_IID,
            IPARAMETER_CHANGES_IID,
            IPARAM_VALUE_QUEUE_IID,
            IEVENT_LIST_IID,
            IHOST_APPLICATION_IID,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "two interfaces share an id");
            }
        }
    }

    #[test]
    fn a_view_rect_never_reports_a_negative_extent() {
        assert_eq!(ViewRect::sized(640, 480).width(), 640);
        assert_eq!(ViewRect::sized(640, 480).height(), 480);
        let inverted = ViewRect {
            left: 10,
            top: 10,
            right: 0,
            bottom: 0,
        };
        assert_eq!(inverted.width(), 0);
        assert_eq!(inverted.height(), 0);
    }
}
