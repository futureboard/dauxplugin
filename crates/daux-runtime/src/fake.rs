//! A complete DAUx module implemented inside the test binary.
//!
//! There is no example plug-in to load yet, and even once there is, the cases worth testing
//! hardest are the ones a conforming plug-in never produces: a factory that reports success
//! and publishes nothing, an `init` that fails, an instance that returns `DAUX_ERR_PANIC`,
//! a descriptor that is never written. A real `.axt` cannot be made to do any of that.
//!
//! So this module is a real ABI implementation — the same `#[repr(C)]` tables, the same
//! opaque handles, the same `catch`-free `extern "C"` entries a compiled plug-in exports —
//! whose behaviour is chosen per test through [`Behaviour`]. Everything downstream of
//! `AxtModule` is the production code path unchanged.

use core::cell::RefCell;
use core::ffi::c_void;
use std::sync::{Arc, Mutex};

use daux_abi::{
    DAUX_CAP_AUDIO_EFFECT, DAUX_CAP_HAS_GUI, DAUX_CATEGORY_EFFECT, DAUX_CATEGORY_INSTRUMENT,
    DAUX_ERR_INVALID_ARG, DAUX_ERR_NOT_FOUND, DAUX_EVENT_NOTE_END, DAUX_FALSE, DAUX_OK,
    DAUX_PARAM_FLAG_AUTOMATABLE, DAUX_PROCESS_CONTINUE, DAUX_SAMPLE_FORMAT_F32, DAUX_TRUE,
    DauxBool, DauxEventListV1, DauxEventNoteV1, DauxFactoryApiV1, DauxFactoryHandle, DauxFactoryV1,
    DauxGuiApiV1, DauxHostV1, DauxId, DauxLatencyApiV1, DauxName, DauxParamInfoV1, DauxParamsApiV1,
    DauxPluginApiV1, DauxPluginDescriptorV1, DauxPluginEntryV1, DauxPluginHandle, DauxPluginV1,
    DauxProcessConfigV1, DauxProcessV1, DauxStateApiV1, DauxStatus, DauxStrView, DauxStreamV1,
    DauxTailApiV1, DauxText, DauxVersion, DauxWindowV1, ext,
};

use crate::module::AxtModule;

/// The state blob the fake plug-in saves and expects to load back.
pub(crate) const STATE_BLOB: &[u8] = b"DAUXST\0\0fake-state";

/// The id of the first plug-in the fake factory publishes.
pub(crate) const GAIN_ID: &str = "com.example.gain";
/// The id of the second plug-in the fake factory publishes.
pub(crate) const SYNTH_ID: &str = "com.example.synth";

/// One thing the fake module was asked to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Call {
    CreateFactory,
    DestroyFactory,
    Descriptor(u32),
    CreatePlugin(String),
    Init,
    Destroy,
    Activate {
        sample_rate: f64,
        max_block: u32,
        mode: u32,
        format: u32,
    },
    Deactivate,
    StartProcessing,
    StopProcessing,
    Reset,
    Process {
        frames: u32,
        input_channels: u32,
        first_input: f32,
        events_in: u32,
        tempo: f64,
    },
    OnMainThread,
    GuiCreate,
    GuiDestroy,
    ParamsFlush(u32),
    StateSave,
    StateLoad(usize),
}

/// The ordered record of everything the fake module was asked to do.
#[derive(Debug)]
pub(crate) struct Journal {
    calls: Mutex<Vec<Call>>,
}

impl Default for Journal {
    fn default() -> Self {
        // Preallocated on purpose. The allocation test measures whether *the runtime*
        // allocates on the audio thread; a journal that grew its `Vec` mid-run would show
        // up as the runtime's fault.
        Self {
            calls: Mutex::new(Vec::with_capacity(8_192)),
        }
    }
}

impl Journal {
    fn record(&self, call: Call) {
        let mut calls = self.calls.lock().expect("journal");
        assert!(
            calls.len() < calls.capacity(),
            "the fake module's journal is full; growing it here would be counted as an \
             allocation on the audio thread"
        );
        calls.push(call);
    }

    /// Where `call` first appears, if it does.
    pub(crate) fn index_of(&self, call: &Call) -> Option<usize> {
        self.calls().iter().position(|c| c == call)
    }

    /// Everything recorded so far, in order.
    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("journal").clone()
    }

    /// How many times `call` was recorded.
    pub(crate) fn count(&self, call: &Call) -> usize {
        self.calls().iter().filter(|c| *c == call).count()
    }

    /// `true` when the recorded calls contain `call`.
    pub(crate) fn contains(&self, call: &Call) -> bool {
        self.count(call) > 0
    }
}

/// How the fake module misbehaves, if at all.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Behaviour {
    pub create_factory_status: i32,
    pub factory_table: TableShape,
    pub init_status: i32,
    pub activate_status: i32,
    pub start_status: i32,
    pub plugin_table: TableShape,
    pub descriptor_writes_nothing: bool,
    pub descriptor_id: Option<&'static str>,
    pub process_result: i32,
    pub latency: u32,
    pub tail: u32,
    pub with_params: bool,
    pub with_state: bool,
    pub with_gui: bool,
    pub with_latency: bool,
    pub with_tail: bool,
    pub params_table: TableShape,
    pub save_status: i32,
    pub load_status: i32,
    pub gain: f64,
}

/// What shape a published function table has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableShape {
    /// A conforming table.
    Good,
    /// A `size` below the v1.0 minimum — the case `abi-v1` §3 rejection rule 4 exists for.
    Undersized,
    /// No table at all, published alongside a success status.
    Null,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            create_factory_status: DAUX_OK.0,
            factory_table: TableShape::Good,
            init_status: DAUX_OK.0,
            activate_status: DAUX_OK.0,
            start_status: DAUX_OK.0,
            plugin_table: TableShape::Good,
            descriptor_writes_nothing: false,
            descriptor_id: None,
            process_result: DAUX_PROCESS_CONTINUE,
            latency: 64,
            tail: 1_024,
            with_params: true,
            with_state: true,
            with_gui: true,
            with_latency: true,
            with_tail: true,
            params_table: TableShape::Good,
            save_status: DAUX_OK.0,
            load_status: DAUX_OK.0,
            gain: 0.5,
        }
    }
}

thread_local! {
    /// What the next `create_factory` on this thread produces. Thread-local rather than
    /// global so that tests running in parallel cannot see each other's settings.
    static PENDING: RefCell<(Behaviour, Arc<Journal>)> =
        RefCell::new((Behaviour::default(), Arc::new(Journal::default())));
}

/// Arms the next [`module`] on this thread and returns the journal it will write to.
pub(crate) fn install(behaviour: Behaviour) -> Arc<Journal> {
    let journal = Arc::new(Journal::default());
    let handed = Arc::clone(&journal);
    PENDING.with(|pending| *pending.borrow_mut() = (behaviour, handed));
    journal
}

/// A module whose entry header points at the fake implementation.
pub(crate) fn module() -> AxtModule {
    AxtModule::from_static_entry(DauxPluginEntryV1 {
        size: DauxPluginEntryV1::SIZE,
        abi_version_major: daux_abi::DAUX_ABI_VERSION_MAJOR,
        abi_version_minor: daux_abi::DAUX_ABI_VERSION_MINOR,
        _pad0: 0,
        magic: daux_abi::DAUX_ABI_MAGIC,
        sdk_name: DauxName::new("daux-runtime-fake"),
        sdk_version: DauxVersion::new(0, 1, 0, 0),
        create_factory,
        destroy_factory,
        reserved: [0; 8],
    })
}

// ---------------------------------------------------------------------- factory ----

struct FakeFactory {
    journal: Arc<Journal>,
    behaviour: Behaviour,
}

/// # Safety
///
/// `handle` must be a factory handle this module produced and not yet destroyed.
unsafe fn factory_of<'a>(handle: DauxFactoryHandle) -> Option<&'a FakeFactory> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: forwarded from this function's contract; the state is a leaked `Box` that
    // lives until `destroy_factory`, and nothing mutates it.
    Some(unsafe { &*handle.as_ptr().cast::<FakeFactory>() })
}

unsafe extern "C" fn create_factory(
    _host: *const DauxHostV1,
    out: *mut DauxFactoryV1,
) -> DauxStatus {
    let (behaviour, journal) = PENDING.with(|pending| pending.borrow().clone());
    journal.record(Call::CreateFactory);
    if behaviour.create_factory_status != DAUX_OK.0 {
        return DauxStatus(behaviour.create_factory_status);
    }
    let state = Box::into_raw(Box::new(FakeFactory { journal, behaviour }));
    let api: *const DauxFactoryApiV1 = match behaviour.factory_table {
        TableShape::Good => &raw const FACTORY_API,
        TableShape::Undersized => &raw const FACTORY_API_UNDERSIZED,
        TableShape::Null => core::ptr::null(),
    };
    // SAFETY: `out` is the caller-owned out-parameter `abi-v1` §4 requires, valid for the
    // duration of the call.
    unsafe {
        out.write(DauxFactoryV1 {
            handle: DauxFactoryHandle::from_ptr(state.cast::<c_void>()),
            api,
        });
    }
    DAUX_OK
}

unsafe extern "C" fn destroy_factory(factory: DauxFactoryV1) {
    if factory.handle.is_null() {
        return;
    }
    // SAFETY: the handle is the leaked `Box` `create_factory` produced, and `abi-v1` §4
    // guarantees `destroy_factory` runs exactly once per factory.
    let state = unsafe { Box::from_raw(factory.handle.as_ptr().cast::<FakeFactory>()) };
    state.journal.record(Call::DestroyFactory);
}

unsafe extern "C" fn plugin_count(handle: DauxFactoryHandle) -> u32 {
    // SAFETY: `handle` is the one this module published.
    unsafe { factory_of(handle) }.map_or(0, |_| 2)
}

unsafe extern "C" fn descriptor(
    handle: DauxFactoryHandle,
    index: u32,
    out: *mut DauxPluginDescriptorV1,
) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    let Some(factory) = (unsafe { factory_of(handle) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    factory.journal.record(Call::Descriptor(index));
    if index >= 2 {
        return DAUX_ERR_NOT_FOUND;
    }
    if factory.behaviour.descriptor_writes_nothing {
        // The pathological module: success, and the host's zeroed buffer untouched.
        return DAUX_OK;
    }

    let mut d = DauxPluginDescriptorV1::new();
    let (id, name, category, caps) = if index == 0 {
        (
            GAIN_ID,
            "Fake Gain",
            DAUX_CATEGORY_EFFECT,
            DAUX_CAP_AUDIO_EFFECT,
        )
    } else {
        (
            SYNTH_ID,
            "Fake Synth",
            DAUX_CATEGORY_INSTRUMENT,
            DAUX_CAP_HAS_GUI,
        )
    };
    d.id = DauxId::new(factory.behaviour.descriptor_id.unwrap_or(id));
    d.name = DauxName::new(name);
    d.vendor = DauxName::new("Example Audio");
    d.version = DauxVersion::new(1, 2, 3, 4);
    d.description = DauxText::new("A fake plug-in that exists only in the test binary.");
    d.category = category;
    d.capabilities = caps;
    d.sample_formats = DAUX_SAMPLE_FORMAT_F32;
    d.state_schema_version = 2;
    d.features = DauxText::new("test;fake");
    // SAFETY: `out` is the caller-owned buffer `abi-v1` §5 requires, valid for this call.
    unsafe { out.write(d) };
    DAUX_OK
}

unsafe extern "C" fn create_plugin(
    handle: DauxFactoryHandle,
    id: DauxStrView,
    out: *mut DauxPluginV1,
) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    let Some(factory) = (unsafe { factory_of(handle) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    // SAFETY: an argument `DauxStrView` is valid for the duration of the call (`abi-v1` §2).
    let requested = unsafe { id.as_str() }.unwrap_or("");
    factory
        .journal
        .record(Call::CreatePlugin(requested.to_owned()));
    if requested != GAIN_ID && requested != SYNTH_ID {
        return DAUX_ERR_NOT_FOUND;
    }

    let api: *const DauxPluginApiV1 = match factory.behaviour.plugin_table {
        TableShape::Good => &raw const PLUGIN_API,
        TableShape::Undersized => &raw const PLUGIN_API_UNDERSIZED,
        TableShape::Null => core::ptr::null(),
    };
    if api.is_null() {
        // SAFETY: `out` is the caller-owned out-parameter, valid for this call.
        unsafe { out.write(DauxPluginV1::null()) };
        return DAUX_OK;
    }

    let state = Box::into_raw(Box::new(FakePlugin {
        journal: Arc::clone(&factory.journal),
        behaviour: factory.behaviour,
        gain: factory.behaviour.gain,
        saved: Vec::new(),
    }));
    // SAFETY: as above.
    unsafe {
        out.write(DauxPluginV1 {
            handle: DauxPluginHandle::from_ptr(state.cast::<c_void>()),
            api,
        });
    }
    DAUX_OK
}

unsafe extern "C" fn factory_extension(
    _handle: DauxFactoryHandle,
    _id: DauxStrView,
) -> *const c_void {
    core::ptr::null()
}

static FACTORY_API: DauxFactoryApiV1 = DauxFactoryApiV1 {
    size: DauxFactoryApiV1::SIZE,
    _pad0: 0,
    plugin_count,
    descriptor,
    create_plugin,
    get_extension: Some(factory_extension),
    reserved: [0; 6],
};

/// The same entries, with a `size` a v1.0 host must refuse.
static FACTORY_API_UNDERSIZED: DauxFactoryApiV1 = DauxFactoryApiV1 {
    size: 8,
    _pad0: 0,
    plugin_count,
    descriptor,
    create_plugin,
    get_extension: None,
    reserved: [0; 6],
};

// --------------------------------------------------------------------- instance ----

struct FakePlugin {
    journal: Arc<Journal>,
    behaviour: Behaviour,
    gain: f64,
    saved: Vec<u8>,
}

/// # Safety
///
/// `handle` must be an instance handle this module produced and not yet destroyed, and the
/// caller must hold no other reference to it — which `abi-v1` §15 guarantees, since calls
/// for one instance are never concurrent.
unsafe fn plugin_of<'a>(handle: DauxPluginHandle) -> Option<&'a mut FakePlugin> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: forwarded from this function's contract.
    Some(unsafe { &mut *handle.as_ptr().cast::<FakePlugin>() })
}

unsafe extern "C" fn init(handle: DauxPluginHandle) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    plugin.journal.record(Call::Init);
    DauxStatus(plugin.behaviour.init_status)
}

unsafe extern "C" fn destroy(handle: DauxPluginHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle is the leaked `Box` `create_plugin` produced, and `abi-v1` §7
    // guarantees `destroy` runs exactly once per instance.
    let plugin = unsafe { Box::from_raw(handle.as_ptr().cast::<FakePlugin>()) };
    plugin.journal.record(Call::Destroy);
}

unsafe extern "C" fn activate(
    handle: DauxPluginHandle,
    config: *const DauxProcessConfigV1,
) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    if config.is_null() {
        return DAUX_ERR_INVALID_ARG;
    }
    // SAFETY: `abi-v1` §7 requires `config` to address a readable configuration for the
    // duration of the call; the read is unaligned and by value.
    let config = unsafe { config.read_unaligned() };
    plugin.journal.record(Call::Activate {
        sample_rate: config.sample_rate,
        max_block: config.max_block_size,
        mode: config.process_mode,
        format: config.sample_format,
    });
    DauxStatus(plugin.behaviour.activate_status)
}

unsafe extern "C" fn deactivate(handle: DauxPluginHandle) {
    // SAFETY: `handle` is the one this module published.
    if let Some(plugin) = unsafe { plugin_of(handle) } {
        plugin.journal.record(Call::Deactivate);
    }
}

unsafe extern "C" fn start_processing(handle: DauxPluginHandle) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    plugin.journal.record(Call::StartProcessing);
    DauxStatus(plugin.behaviour.start_status)
}

unsafe extern "C" fn stop_processing(handle: DauxPluginHandle) {
    // SAFETY: `handle` is the one this module published.
    if let Some(plugin) = unsafe { plugin_of(handle) } {
        plugin.journal.record(Call::StopProcessing);
    }
}

unsafe extern "C" fn reset(handle: DauxPluginHandle) {
    // SAFETY: `handle` is the one this module published.
    if let Some(plugin) = unsafe { plugin_of(handle) } {
        plugin.journal.record(Call::Reset);
    }
}

unsafe extern "C" fn process(handle: DauxPluginHandle, block: *const DauxProcessV1) -> i32 {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return daux_abi::DAUX_PROCESS_ERROR;
    };
    if block.is_null() {
        return daux_abi::DAUX_PROCESS_ERROR;
    }
    // SAFETY: `abi-v1` §8 requires `block` to address a readable `DauxProcessV1` for the
    // duration of the call; the read is unaligned and by value.
    let block = unsafe { block.read_unaligned() };
    let frames = block.frame_count;

    let mut input_channels = 0;
    let mut first_input = 0.0f32;
    if block.audio_input_count > 0 && !block.audio_inputs.is_null() {
        // SAFETY: `audio_input_count` is non-zero, so `audio_inputs` addresses at least one
        // `DauxAudioBufferV1` for the call.
        let bus = unsafe { *block.audio_inputs };
        input_channels = bus.channel_count;
        if bus.channel_count > 0 && !bus.data32.is_null() && frames > 0 {
            // SAFETY: `channel_count` is non-zero, so `data32` addresses at least one
            // channel pointer, and that channel holds `frame_count` samples.
            first_input = unsafe {
                let channel = *bus.data32;
                if channel.is_null() { 0.0 } else { *channel }
            };
        }
    }

    if block.audio_output_count > 0 && !block.audio_outputs.is_null() {
        // SAFETY: `audio_output_count` is non-zero, so `audio_outputs` addresses at least
        // one bus; each of its `channel_count` pointers addresses `frame_count` writable
        // samples for the duration of the call (`abi-v1` §8).
        unsafe {
            let bus = *block.audio_outputs;
            for channel in 0..bus.channel_count {
                let samples = *bus.data32.add(channel as usize);
                if samples.is_null() {
                    continue;
                }
                for frame in 0..frames {
                    *samples.add(frame as usize) = plugin.gain as f32;
                }
            }
        }
    }

    let tempo = if block.transport.is_null() {
        f64::NAN
    } else {
        // SAFETY: a non-null `transport` addresses a readable `DauxTransportV1` for the call.
        unsafe { (*block.transport).tempo }
    };

    let events_in = read_count(block.in_events);
    if !block.out_events.is_null() {
        let mut end = DauxEventNoteV1::new();
        end.header.kind = DAUX_EVENT_NOTE_END;
        end.header.time = frames.saturating_sub(1);
        end.key = 71;
        // SAFETY: `out_events` addresses a host-owned list valid for the call, and `end` is
        // a complete record whose declared size matches its layout (`abi-v1` §9).
        unsafe {
            let list = &*block.out_events;
            let _ = (list.push)(list.ctx, (&raw const end).cast());
        }
    }

    plugin.journal.record(Call::Process {
        frames,
        input_channels,
        first_input,
        events_in,
        tempo,
    });
    plugin.behaviour.process_result
}

/// Reads `count` off an event list the host published, tolerating a null list.
fn read_count(list: *const DauxEventListV1) -> u32 {
    if list.is_null() {
        return 0;
    }
    // SAFETY: a non-null list addresses a host-owned `DauxEventListV1` valid for the call,
    // and `count` is a non-optional entry of it.
    unsafe {
        let list = &*list;
        (list.count)(list.ctx)
    }
}

unsafe extern "C" fn get_extension(handle: DauxPluginHandle, id: DauxStrView) -> *const c_void {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return core::ptr::null();
    };
    // SAFETY: an argument `DauxStrView` is valid for the duration of the call.
    let Some(id) = (unsafe { id.as_str() }) else {
        return core::ptr::null();
    };
    let behaviour = plugin.behaviour;
    match id {
        ext::PARAMS if behaviour.with_params => match behaviour.params_table {
            TableShape::Good => (&raw const PARAMS_API).cast::<c_void>(),
            TableShape::Undersized => (&raw const PARAMS_API_UNDERSIZED).cast::<c_void>(),
            TableShape::Null => core::ptr::null(),
        },
        ext::STATE if behaviour.with_state => (&raw const STATE_API).cast::<c_void>(),
        ext::GUI if behaviour.with_gui => (&raw const GUI_API).cast::<c_void>(),
        ext::LATENCY if behaviour.with_latency => (&raw const LATENCY_API).cast::<c_void>(),
        ext::TAIL if behaviour.with_tail => (&raw const TAIL_API).cast::<c_void>(),
        _ => core::ptr::null(),
    }
}

unsafe extern "C" fn on_main_thread(handle: DauxPluginHandle) {
    // SAFETY: `handle` is the one this module published.
    if let Some(plugin) = unsafe { plugin_of(handle) } {
        plugin.journal.record(Call::OnMainThread);
    }
}

static PLUGIN_API: DauxPluginApiV1 = DauxPluginApiV1 {
    size: DauxPluginApiV1::SIZE,
    _pad0: 0,
    init,
    destroy,
    activate,
    deactivate,
    start_processing,
    stop_processing,
    reset,
    process,
    get_extension,
    on_main_thread,
    reserved: [0; 6],
};

static PLUGIN_API_UNDERSIZED: DauxPluginApiV1 = DauxPluginApiV1 {
    size: 16,
    _pad0: 0,
    init,
    destroy,
    activate,
    deactivate,
    start_processing,
    stop_processing,
    reset,
    process,
    get_extension,
    on_main_thread,
    reserved: [0; 6],
};

// ------------------------------------------------------------------- daux.params ----

unsafe extern "C" fn param_count(_handle: DauxPluginHandle) -> u32 {
    2
}

unsafe extern "C" fn param_info(
    handle: DauxPluginHandle,
    index: u32,
    out: *mut DauxParamInfoV1,
) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    if unsafe { plugin_of(handle) }.is_none() {
        return DAUX_ERR_INVALID_ARG;
    }
    if index >= 2 {
        return DAUX_ERR_NOT_FOUND;
    }
    let mut info = DauxParamInfoV1::new();
    info.id = if index == 0 { 1 } else { 7 };
    info.flags = DAUX_PARAM_FLAG_AUTOMATABLE;
    info.name = DauxName::new(if index == 0 { "Gain" } else { "Mix" });
    info.group = DauxName::new("Main");
    info.unit = DauxName::new(if index == 0 { "dB" } else { "%" });
    info.min_value = if index == 0 { -60.0 } else { 0.0 };
    info.max_value = if index == 0 { 12.0 } else { 100.0 };
    info.default_value = if index == 0 { 0.0 } else { 50.0 };
    // SAFETY: `out` is the caller-owned buffer `abi-v1` §11.2 requires.
    unsafe { out.write(info) };
    DAUX_OK
}

unsafe extern "C" fn param_value(handle: DauxPluginHandle, id: u32, out: *mut f64) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    let value = match id {
        1 => plugin.gain,
        7 => 50.0,
        _ => return DAUX_ERR_NOT_FOUND,
    };
    // SAFETY: `out` is the caller-owned `f64` the host provided for this call.
    unsafe { out.write(value) };
    DAUX_OK
}

unsafe extern "C" fn param_to_text(
    _handle: DauxPluginHandle,
    id: u32,
    value: f64,
    out: *mut DauxText,
) -> DauxStatus {
    if id != 1 && id != 7 {
        return DAUX_ERR_NOT_FOUND;
    }
    let text = DauxText::new(&format!("{value:.2}"));
    // SAFETY: `out` is the caller-owned `DAUX_TEXT_SIZE` buffer of `abi-v1` §11.2.
    unsafe { out.write(text) };
    DAUX_OK
}

unsafe extern "C" fn param_from_text(
    _handle: DauxPluginHandle,
    id: u32,
    text: DauxStrView,
    out: *mut f64,
) -> DauxStatus {
    if id != 1 && id != 7 {
        return DAUX_ERR_NOT_FOUND;
    }
    // SAFETY: an argument `DauxStrView` is valid for the duration of the call.
    let Some(text) = (unsafe { text.as_str() }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    let Ok(value) = text.trim().parse::<f64>() else {
        return DAUX_ERR_INVALID_ARG;
    };
    // SAFETY: `out` is the caller-owned `f64` the host provided for this call.
    unsafe { out.write(value) };
    DAUX_OK
}

unsafe extern "C" fn param_flush(
    handle: DauxPluginHandle,
    in_events: *const DauxEventListV1,
    out_events: *const DauxEventListV1,
) {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return;
    };
    let count = read_count(in_events);
    plugin.journal.record(Call::ParamsFlush(count));
    if !out_events.is_null() {
        let mut end = DauxEventNoteV1::new();
        end.header.kind = DAUX_EVENT_NOTE_END;
        end.key = 12;
        // SAFETY: a non-null output list is host-owned and valid for the call, and `end` is
        // a complete record whose declared size matches its layout.
        unsafe {
            let list = &*out_events;
            let _ = (list.push)(list.ctx, (&raw const end).cast());
        }
    }
}

static PARAMS_API: DauxParamsApiV1 = DauxParamsApiV1 {
    size: DauxParamsApiV1::SIZE,
    _pad0: 0,
    count: param_count,
    get_info: param_info,
    get_value: param_value,
    value_to_text: param_to_text,
    text_to_value: param_from_text,
    flush: param_flush,
    reserved: [0; 4],
};

static PARAMS_API_UNDERSIZED: DauxParamsApiV1 = DauxParamsApiV1 {
    size: 12,
    _pad0: 0,
    count: param_count,
    get_info: param_info,
    get_value: param_value,
    value_to_text: param_to_text,
    text_to_value: param_from_text,
    flush: param_flush,
    reserved: [0; 4],
};

// -------------------------------------------------------------------- daux.state ----

unsafe extern "C" fn state_save(
    handle: DauxPluginHandle,
    stream: *const DauxStreamV1,
) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    plugin.journal.record(Call::StateSave);
    if plugin.behaviour.save_status != DAUX_OK.0 {
        return DauxStatus(plugin.behaviour.save_status);
    }
    if stream.is_null() {
        return DAUX_ERR_INVALID_ARG;
    }
    // SAFETY: `abi-v1` §11.3 requires `stream` to address a host-owned stream valid for the
    // call.
    let stream = unsafe { &*stream };
    let Some(write) = stream.write else {
        return DAUX_ERR_INVALID_ARG;
    };
    // Written in two chunks on purpose: a plug-in is entitled to any chunking it likes.
    let (head, tail) = STATE_BLOB.split_at(6);
    // SAFETY: both slices are live and the stream is valid for this call.
    let written = unsafe {
        write(stream.ctx, head.as_ptr(), head.len()) + write(stream.ctx, tail.as_ptr(), tail.len())
    };
    if written != STATE_BLOB.len() as isize {
        return daux_abi::DAUX_ERR_IO;
    }
    DAUX_OK
}

unsafe extern "C" fn state_load(
    handle: DauxPluginHandle,
    stream: *const DauxStreamV1,
) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    if plugin.behaviour.load_status != DAUX_OK.0 {
        plugin.journal.record(Call::StateLoad(0));
        return DauxStatus(plugin.behaviour.load_status);
    }
    if stream.is_null() {
        return DAUX_ERR_INVALID_ARG;
    }
    // SAFETY: as in `state_save`.
    let stream = unsafe { &*stream };
    let Some(read) = stream.read else {
        return DAUX_ERR_INVALID_ARG;
    };
    let mut blob = Vec::new();
    let mut chunk = [0u8; 7];
    loop {
        // SAFETY: `chunk` is seven writable bytes and the stream is valid for this call.
        let got = unsafe { read(stream.ctx, chunk.as_mut_ptr(), chunk.len()) };
        if got < 0 {
            return daux_abi::DAUX_ERR_IO;
        }
        if got == 0 {
            break;
        }
        blob.extend_from_slice(&chunk[..got as usize]);
    }
    plugin.journal.record(Call::StateLoad(blob.len()));
    plugin.saved = blob;
    DAUX_OK
}

static STATE_API: DauxStateApiV1 = DauxStateApiV1 {
    size: DauxStateApiV1::SIZE,
    _pad0: 0,
    save: state_save,
    load: state_load,
    reserved: [0; 4],
};

// ---------------------------------------------------------------------- daux.gui ----

unsafe extern "C" fn gui_is_supported(
    _handle: DauxPluginHandle,
    api: u32,
    _floating: DauxBool,
) -> DauxBool {
    if api == daux_abi::DAUX_WINDOW_API_WIN32 {
        DAUX_TRUE
    } else {
        DAUX_FALSE
    }
}

unsafe extern "C" fn gui_create(
    handle: DauxPluginHandle,
    api: u32,
    _floating: DauxBool,
) -> DauxStatus {
    // SAFETY: `handle` is the one this module published.
    let Some(plugin) = (unsafe { plugin_of(handle) }) else {
        return DAUX_ERR_INVALID_ARG;
    };
    if api != daux_abi::DAUX_WINDOW_API_WIN32 {
        return daux_abi::DAUX_ERR_UNSUPPORTED;
    }
    plugin.journal.record(Call::GuiCreate);
    DAUX_OK
}

unsafe extern "C" fn gui_destroy(handle: DauxPluginHandle) {
    // SAFETY: `handle` is the one this module published.
    if let Some(plugin) = unsafe { plugin_of(handle) } {
        plugin.journal.record(Call::GuiDestroy);
    }
}

unsafe extern "C" fn gui_size(
    _handle: DauxPluginHandle,
    width: *mut u32,
    height: *mut u32,
) -> DauxStatus {
    // SAFETY: both are caller-owned `u32`s the host provided for this call.
    unsafe {
        width.write(640);
        height.write(480);
    }
    DAUX_OK
}

unsafe extern "C" fn gui_can_resize(_handle: DauxPluginHandle) -> DauxBool {
    DAUX_TRUE
}

unsafe extern "C" fn gui_set_size(
    _handle: DauxPluginHandle,
    width: u32,
    _height: u32,
) -> DauxStatus {
    if width == 0 {
        return DAUX_ERR_INVALID_ARG;
    }
    DAUX_OK
}

unsafe extern "C" fn gui_set_parent(
    _handle: DauxPluginHandle,
    window: *const DauxWindowV1,
) -> DauxStatus {
    if window.is_null() {
        return DAUX_ERR_INVALID_ARG;
    }
    DAUX_OK
}

unsafe extern "C" fn gui_show(_handle: DauxPluginHandle) -> DauxStatus {
    DAUX_OK
}

unsafe extern "C" fn gui_hide(_handle: DauxPluginHandle) -> DauxStatus {
    DAUX_OK
}

static GUI_API: DauxGuiApiV1 = DauxGuiApiV1 {
    size: DauxGuiApiV1::SIZE,
    _pad0: 0,
    is_api_supported: gui_is_supported,
    create: gui_create,
    destroy: gui_destroy,
    // Left null on purpose: `abi-v1` §11.4 marks both optional, and the host must cope.
    set_scale: None,
    get_size: gui_size,
    can_resize: gui_can_resize,
    adjust_size: None,
    set_size: gui_set_size,
    set_parent: gui_set_parent,
    show: gui_show,
    hide: gui_hide,
    reserved: [0; 6],
};

// --------------------------------------------------------------- latency and tail ----

unsafe extern "C" fn latency_get(handle: DauxPluginHandle) -> u32 {
    // SAFETY: `handle` is the one this module published.
    unsafe { plugin_of(handle) }.map_or(0, |p| p.behaviour.latency)
}

unsafe extern "C" fn tail_get(handle: DauxPluginHandle) -> u32 {
    // SAFETY: `handle` is the one this module published.
    unsafe { plugin_of(handle) }.map_or(0, |p| p.behaviour.tail)
}

static LATENCY_API: DauxLatencyApiV1 = DauxLatencyApiV1 {
    size: DauxLatencyApiV1::SIZE,
    _pad0: 0,
    get: latency_get,
    reserved: [0; 2],
};

static TAIL_API: DauxTailApiV1 = DauxTailApiV1 {
    size: DauxTailApiV1::SIZE,
    _pad0: 0,
    get: tail_get,
    reserved: [0; 2],
};
