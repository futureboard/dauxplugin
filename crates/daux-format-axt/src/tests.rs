//! Driving the generated C entry points the way a host does: through raw pointers.
//!
//! Nothing in here calls a Rust method on a plug-in. Every test resolves the exported
//! `daux_plugin_entry_v1`, validates the header, calls `create_factory`, and works through the
//! function tables — because that is the only way to test what a host actually sees, and the
//! only way a mistake in a `#[repr(C)]` table or a `catch_unwind` wrapper shows up.
//!
//! The two tests that matter most are
//! [`a_panicking_process_poisons_the_instance_instead_of_unwinding`] and
//! [`a_panicking_factory_refuses_everything_afterwards`]: a panic crossing `extern "C"` is
//! undefined behaviour, and a plug-in that has already broken its own invariants must not be
//! entered again (abi-v1 §17).

use std::sync::atomic::{AtomicUsize, Ordering};

use daux_abi::{
    DAUX_ERR_ABI_MISMATCH, DAUX_ERR_INVALID_ARG, DAUX_ERR_INVALID_STATE, DAUX_ERR_NOT_FOUND,
    DAUX_ERR_PANIC, DAUX_ERR_UNSUPPORTED, DAUX_ERR_VERSION, DAUX_FALSE, DAUX_OK,
    DAUX_PROCESS_CONTINUE, DAUX_PROCESS_ERROR, DAUX_SAMPLE_FORMAT_F32, DAUX_TRUE,
    DauxAudioBufferV1, DauxAudioPortInfoV1, DauxAudioPortsApiV1, DauxEventHeaderV1,
    DauxEventNoteV1, DauxFactoryV1, DauxGuiApiV1, DauxHostApiV1, DauxHostHandle, DauxHostLogApiV1,
    DauxHostV1, DauxLatencyApiV1, DauxName, DauxParamInfoV1, DauxParamsApiV1,
    DauxPluginDescriptorV1, DauxPluginEntryV1, DauxPluginV1, DauxProcessConfigV1, DauxProcessV1,
    DauxRenderApiV1, DauxStateApiV1, DauxStatus, DauxStrView, DauxTailApiV1, DauxText,
    DauxWindowV1, ext,
};
use daux_plugin_api::{
    AudioBuses, BusLayout, Capabilities, Category, DauxController, DauxError, DauxFactory,
    DauxGraphic, DauxGraphicResult, DauxPlugin, DauxProcessor, DauxResult, ErrorKind, FloatParam,
    GraphicCapabilities, GraphicContext, GraphicDescriptor, GraphicDescriptor as GDesc,
    GraphicFramework, GraphicProfile, GraphicRenderer, Latency, LogicalSize, Param, ParamId,
    ParamRange, Params, PhysicalSize, PluginDescriptor, PresentationMode, ProcessConfig,
    ProcessContext, ProcessEvents, ProcessStatus, StateReader, StateWriter, Tail,
};

use crate::events::testing::FakeList;
use crate::stream::testing::FakeStream;

// ---------------------------------------------------------------------------- plug-ins ----

/// Ids the test factory exports.
const GAIN_ID: &str = "com.example.gain";
const PANIC_ID: &str = "com.example.panic";
const EXPLODE_ID: &str = "com.example.explode-on-create";

/// How many editors have been opened and closed, process-wide.
///
/// Test-only state — nothing in the library has any — and read as a *delta* across one test, so
/// the number the harness's parallelism produces cannot matter.
static EDITOR_OPENS: AtomicUsize = AtomicUsize::new(0);
static EDITOR_CLOSES: AtomicUsize = AtomicUsize::new(0);

/// A minimal but complete plug-in: two parameters, real DSP, events in and out, an editor.
struct Gain {
    gain: FloatParam,
    /// A read-only parameter `prepare` writes the process mode into, so a test can observe
    /// through the ABI what the plug-in was actually activated with.
    mode: FloatParam,
    label: String,
}

impl Default for Gain {
    fn default() -> Self {
        Self {
            gain: FloatParam::new(
                ParamId::new(1),
                "Gain",
                2.0,
                ParamRange::Linear { min: 0.0, max: 4.0 },
            )
            .with_unit("x"),
            mode: FloatParam::new(
                ParamId::new(2),
                "Mode",
                0.0,
                ParamRange::Linear { min: 0.0, max: 3.0 },
            ),
            label: "initial".to_owned(),
        }
    }
}

impl Params for Gain {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        vec![
            (ParamId::new(1), &self.gain as &dyn Param),
            (ParamId::new(2), &self.mode as &dyn Param),
        ]
    }

    fn param(&self, id: ParamId) -> Option<&dyn Param> {
        match id.get() {
            1 => Some(&self.gain as &dyn Param),
            2 => Some(&self.mode as &dyn Param),
            _ => None,
        }
    }

    fn state_schema_version(&self) -> u32 {
        3
    }

    /// This plug-in used to call its gain "5" and had a switch it has since dropped.
    fn migrations(&self) -> &[daux_plugin_api::ParamMigration] {
        const MIGRATIONS: &[daux_plugin_api::ParamMigration] = &[
            daux_plugin_api::ParamMigration::rename(ParamId::new(5), ParamId::new(1)),
            daux_plugin_api::ParamMigration::removed(ParamId::new(6)),
        ];
        MIGRATIONS
    }
}

impl DauxProcessor for Gain {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;
        self.mode.set_plain(f64::from(config.process_mode.code()));
        Ok(())
    }

    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        let gain = self.gain.plain() as f32;
        if let Some(input) = audio.main_input() {
            if let Some(mut output) = audio.main_output() {
                let _ = output.copy_from(&input);
                for channel in output.split_channels_mut() {
                    for sample in channel {
                        *sample *= gain;
                    }
                }
            }
        }
        // Echo every note-on back as a note-end, so the output list is exercised too.
        let (input, output) = events.split();
        for index in 0..input.len() {
            if let Some(daux_plugin_api::DauxEvent::NoteOn(note)) = input.get(index) {
                let _ = output.try_push(&daux_plugin_api::DauxEvent::NoteEnd(note));
            }
        }
        // The transport is only read through its accessors, which is the point of them.
        if ctx.transport().is_some_and(|t| t.is_playing()) {
            return ProcessStatus::Continue;
        }
        ProcessStatus::ContinueIfNotQuiet
    }

    fn latency(&self) -> Latency {
        Latency::Samples(64)
    }

    fn tail(&self) -> Tail {
        Tail::Samples(128)
    }
}

impl DauxController for Gain {
    fn params(&self) -> &dyn Params {
        self
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        w.put_str("label", &self.label);
        Ok(())
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        // A blob a controller refuses *after* the framework has already written the parameter
        // values: the case abi-v1 §12's atomicity requirement exists for.
        if r.opt_str("label") == Some("refuse") {
            return Err(DauxError::new(ErrorKind::Plugin, "refusing on purpose"));
        }
        if let Some(label) = r.opt_str("label") {
            self.label = label.to_owned();
        }
        Ok(())
    }
}

impl DauxPlugin for Gain {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder(GAIN_ID, "Gain")
            .vendor("Futureboard")
            .category(Category::Utility)
            .capabilities(
                Capabilities::NONE
                    .with_audio_effect()
                    .with_has_gui()
                    .with_hard_realtime(),
            )
            .state_schema_version(3)
            .build()
            .expect("valid descriptor")
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::stereo_effect()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        self
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        self
    }

    fn create_editor(&mut self) -> Option<Box<dyn std::any::Any>> {
        Some(Box::new(Box::new(Editor) as Box<dyn DauxGraphic>))
    }
}

/// An editor that records that it was opened and closed, and nothing else.
struct Editor;

impl DauxGraphic for Editor {
    fn descriptor(&self) -> GraphicDescriptor {
        GDesc::resizable(
            GraphicCapabilities::new().with(GraphicProfile::new(
                GraphicFramework::Custom,
                GraphicRenderer::Software,
                PresentationMode::EmbeddedSurface,
            )),
            LogicalSize::new(400.0, 300.0),
            LogicalSize::new(200.0, 150.0),
        )
    }

    fn open(&mut self, _ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> {
        EDITOR_OPENS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn resize(&mut self, _size: PhysicalSize) -> DauxGraphicResult<()> {
        Ok(())
    }

    fn close(&mut self) {
        EDITOR_CLOSES.fetch_add(1, Ordering::Relaxed);
    }
}

/// A plug-in that panics inside `process`, which is the case abi-v1 §17 exists for.
#[derive(Default)]
struct Panicky;

impl Params for Panicky {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        Vec::new()
    }
}

impl DauxProcessor for Panicky {
    fn prepare(&mut self, _config: &ProcessConfig) -> DauxResult<()> {
        Ok(())
    }

    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        _audio: &mut AudioBuses<'a, f32>,
        _events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        panic!("this plug-in is deliberately broken");
    }
}

impl DauxController for Panicky {
    fn params(&self) -> &dyn Params {
        self
    }

    fn save_state(&self, _w: &mut StateWriter) -> DauxResult<()> {
        Ok(())
    }

    fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> {
        Ok(())
    }
}

impl DauxPlugin for Panicky {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder(PANIC_ID, "Panicky")
            .build()
            .expect("valid descriptor")
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::stereo_effect()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        self
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        self
    }
}

/// The module's factory: two working plug-ins and one id whose construction panics.
#[derive(Default)]
struct TestFactory;

impl DauxFactory for TestFactory {
    fn plugin_count(&self) -> usize {
        3
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        match index {
            0 => Some(Gain::descriptor()),
            1 => Some(Panicky::descriptor()),
            2 => Some(
                PluginDescriptor::builder(EXPLODE_ID, "Explode")
                    .build()
                    .expect("valid descriptor"),
            ),
            _ => None,
        }
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        match id {
            GAIN_ID => Ok(Box::new(Gain::default())),
            PANIC_ID => Ok(Box::new(Panicky)),
            EXPLODE_ID => panic!("this factory is deliberately broken"),
            other => Err(ErrorKind::NotFound.error(format!("no plug-in `{other}`"))),
        }
    }
}

crate::export_entry!(TestFactory);

// SAFETY: the symbol is defined by the `export_entry!` invocation above, in this same crate,
// with exactly this signature. Declaring it here is how a test reaches an exported symbol that
// has no Rust path — which is also the only way a host reaches it.
unsafe extern "C" {
    fn daux_plugin_entry_v1() -> *const DauxPluginEntryV1;
}

// ------------------------------------------------------------------------------- a host ----

/// A host that records what the plug-in asked of it.
#[derive(Default)]
struct FakeHost {
    logs: AtomicUsize,
    callbacks: AtomicUsize,
}

struct HostSide {
    state: Box<FakeHost>,
    api: DauxHostApiV1,
    interface: DauxHostV1,
}

impl HostSide {
    fn new() -> Box<Self> {
        let state = Box::new(FakeHost::default());
        let handle =
            DauxHostHandle::from_ptr((&raw const *state).cast::<core::ffi::c_void>().cast_mut());
        let mut this = Box::new(Self {
            state,
            api: DauxHostApiV1 {
                size: DauxHostApiV1::SIZE,
                abi_version_major: 1,
                abi_version_minor: 0,
                _pad0: 0,
                name: DauxName::new("TestHost"),
                vendor: DauxName::new("Futureboard"),
                version: daux_abi::DauxVersion {
                    major: 1,
                    minor: 2,
                    patch: 3,
                    build: 0,
                },
                get_extension: host_get_extension,
                request_restart: host_noop,
                request_process: host_noop,
                request_callback: host_callback,
                is_main_thread: None,
                is_audio_thread: None,
                reserved: [0; 8],
            },
            interface: DauxHostV1::null(),
        });
        this.interface = DauxHostV1::new(handle, &raw const this.api);
        // The log table is reached through `get_extension`, which needs to find it from the
        // handle alone; the handle is the `FakeHost`, so the table has to be a `static`.
        this
    }

    fn interface(&self) -> *const DauxHostV1 {
        &raw const self.interface
    }
}

/// The one log table every fake host shares. `get_extension` only has the handle to work with,
/// and a `static` table is exactly what abi-v1 §2.3 expects a module to hand out.
static HOST_LOG_TABLE: DauxHostLogApiV1 = DauxHostLogApiV1 {
    size: DauxHostLogApiV1::SIZE,
    _pad0: 0,
    log: host_log,
    reserved: [0; 2],
};

unsafe extern "C" fn host_log(h: DauxHostHandle, _level: u32, _msg: DauxStrView) {
    // SAFETY: the handle is the `FakeHost` the interface was built with.
    let host = unsafe { &*h.as_ptr().cast::<FakeHost>() };
    host.logs.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "C" fn host_callback(h: DauxHostHandle) {
    // SAFETY: as `host_log`.
    let host = unsafe { &*h.as_ptr().cast::<FakeHost>() };
    host.callbacks.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "C" fn host_noop(_h: DauxHostHandle) {}

unsafe extern "C" fn host_get_extension(
    _h: DauxHostHandle,
    id: DauxStrView,
) -> *const core::ffi::c_void {
    // SAFETY: the view is readable for the duration of the call, as the ABI requires.
    let Some(id) = (unsafe { id.as_str() }) else {
        return core::ptr::null();
    };
    if id == ext::HOST_LOG {
        (&raw const HOST_LOG_TABLE).cast()
    } else {
        core::ptr::null()
    }
}

// ------------------------------------------------------------------------- test fixtures ----

/// The module header, validated the way a host validates it.
fn entry() -> &'static DauxPluginEntryV1 {
    // SAFETY: the exported function returns a pointer to a `static` with `'static` lifetime and
    // is callable before anything else in the module (abi-v1 §4).
    let ptr = unsafe { daux_plugin_entry_v1() };
    assert!(!ptr.is_null(), "rejection rule 1");
    // SAFETY: non-null was just checked, and the pointee is a `static`.
    unsafe { &*ptr }
}

/// A live factory plus the host it was created with, destroyed on drop.
struct Fixture {
    host: Box<HostSide>,
    factory: DauxFactoryV1,
}

impl Fixture {
    fn new() -> Self {
        let host = HostSide::new();
        let mut factory = DauxFactoryV1::null();
        // SAFETY: `host.interface()` is live for longer than the factory, and `factory` is a
        // writable local — exactly what `create_factory` documents.
        let status = unsafe { (entry().create_factory)(host.interface(), &raw mut factory) };
        assert_eq!(status, DAUX_OK);
        assert!(!factory.is_null());
        Self { host, factory }
    }

    fn api(&self) -> &daux_abi::DauxFactoryApiV1 {
        // SAFETY: the table pointer came from `create_factory` and is a `static` in this crate.
        unsafe { &*self.factory.api }
    }

    fn create(&self, id: &str) -> Option<Instance> {
        let mut plugin = DauxPluginV1::null();
        // SAFETY: the handle is live, the id is borrowed for the call, and `plugin` is a
        // writable local.
        let status = unsafe {
            (self.api().create_plugin)(
                self.factory.handle,
                DauxStrView::from_str(id),
                &raw mut plugin,
            )
        };
        if status.is_ok() {
            Some(Instance { plugin })
        } else {
            None
        }
    }

    fn create_ok(&self, id: &str) -> Instance {
        self.create(id).expect("the factory exports this id")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // SAFETY: every instance created from this factory is destroyed by `Instance::drop`
        // before the fixture goes away, because the tests keep them in narrower scopes.
        unsafe { (entry().destroy_factory)(self.factory) };
    }
}

/// A live instance, destroyed on drop.
struct Instance {
    plugin: DauxPluginV1,
}

impl Instance {
    fn api(&self) -> &daux_abi::DauxPluginApiV1 {
        // SAFETY: the table came from `create_plugin` and is a `static` in this crate.
        unsafe { &*self.plugin.api }
    }

    fn init(&self) -> DauxStatus {
        // SAFETY: the handle is live and this is the only call in progress on it.
        unsafe { (self.api().init)(self.plugin.handle) }
    }

    fn activate(&self, config: &DauxProcessConfigV1) -> DauxStatus {
        // SAFETY: as `init`; `config` is a live local for the duration of the call.
        unsafe { (self.api().activate)(self.plugin.handle, config) }
    }

    fn deactivate(&self) {
        // SAFETY: as `init`.
        unsafe { (self.api().deactivate)(self.plugin.handle) }
    }

    fn start(&self) -> DauxStatus {
        // SAFETY: as `init`.
        unsafe { (self.api().start_processing)(self.plugin.handle) }
    }

    fn stop(&self) {
        // SAFETY: as `init`.
        unsafe { (self.api().stop_processing)(self.plugin.handle) }
    }

    fn reset(&self) {
        // SAFETY: as `init`.
        unsafe { (self.api().reset)(self.plugin.handle) }
    }

    fn process(&self, block: &DauxProcessV1) -> i32 {
        // SAFETY: as `init`; `block` and everything it points at outlive the call.
        unsafe { (self.api().process)(self.plugin.handle, block) }
    }

    fn extension<T>(&self, id: &str) -> Option<&T> {
        // SAFETY: as `init`; the id is borrowed for the call.
        let ptr =
            unsafe { (self.api().get_extension)(self.plugin.handle, DauxStrView::from_str(id)) };
        // SAFETY: a non-null answer is a `static` table of the type the id names.
        unsafe { daux_abi::extension_table::<T>(ptr) }
    }

    fn params(&self) -> &DauxParamsApiV1 {
        self.extension(ext::PARAMS).expect("params is standard")
    }

    fn state(&self) -> &DauxStateApiV1 {
        self.extension(ext::STATE).expect("state is standard")
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the handle is live and is destroyed exactly once, here.
        unsafe { (self.api().destroy)(self.plugin.handle) }
    }
}

fn config(frames: u32) -> DauxProcessConfigV1 {
    let mut config = DauxProcessConfigV1::new();
    config.sample_rate = 48_000.0;
    config.min_block_size = 1;
    config.max_block_size = frames;
    config.sample_format = DAUX_SAMPLE_FORMAT_F32;
    config
}

/// One block of audio, host-side.
struct Block {
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    input_ptrs: Vec<*mut f32>,
    output_ptrs: Vec<*mut f32>,
    buses: Vec<DauxAudioBufferV1>,
    in_events: FakeList,
    out_events: FakeList,
}

impl Block {
    fn new(frames: usize, value: f32) -> Box<Self> {
        let mut this = Box::new(Self {
            input: vec![vec![value; frames]; 2],
            output: vec![vec![0.0; frames]; 2],
            input_ptrs: Vec::new(),
            output_ptrs: Vec::new(),
            buses: Vec::new(),
            in_events: FakeList::with_capacity(16),
            out_events: FakeList::with_capacity(16),
        });
        this.input_ptrs = this.input.iter_mut().map(|c| c.as_mut_ptr()).collect();
        this.output_ptrs = this.output.iter_mut().map(|c| c.as_mut_ptr()).collect();
        let mut input_bus = DauxAudioBufferV1::new();
        input_bus.channel_count = 2;
        input_bus.data32 = this.input_ptrs.as_ptr();
        let mut output_bus = DauxAudioBufferV1::new();
        output_bus.channel_count = 2;
        output_bus.data32 = this.output_ptrs.as_ptr();
        this.buses = vec![input_bus, output_bus];
        this
    }

    fn abi(&mut self, frames: u32) -> DauxProcessV1 {
        let mut block = DauxProcessV1::new();
        block.frame_count = frames;
        block.audio_input_count = 1;
        block.audio_inputs = self.buses.as_ptr();
        block.audio_output_count = 1;
        // The output bus is the second entry; the ABI wants a `*mut` to it.
        block.audio_outputs = self.buses[1..].as_mut_ptr();
        block.in_events = self.in_events.table();
        block.out_events = self.out_events.table();
        block
    }
}

// --------------------------------------------------------------------------------- tests ----

#[test]
fn the_module_header_passes_every_rejection_rule() {
    let first = entry();
    let second = entry();
    assert!(
        core::ptr::eq(first, second),
        "the entry pointer must be identical across calls (abi-v1 §4)"
    );
    assert_eq!(first.magic, 0x4441_5558_4142_4931, "rejection rule 2");
    assert_eq!(first.abi_version_major, 1, "rejection rule 3");
    assert!(first.is_v1_0_compatible(), "rejection rule 4");
    assert!(first.check().is_ok());
    assert_eq!(first.reserved, [0; 8], "a writer zeroes reserved fields");
    assert_eq!(first.sdk_name.as_str(), crate::SDK_NAME);
}

#[test]
fn a_factory_enumerates_its_plugins() {
    let fixture = Fixture::new();
    // SAFETY: the handle is live.
    let count = unsafe { (fixture.api().plugin_count)(fixture.factory.handle) };
    assert_eq!(count, 3);

    let mut descriptor = DauxPluginDescriptorV1::new();
    // SAFETY: the handle is live and `descriptor` is a writable local.
    let status =
        unsafe { (fixture.api().descriptor)(fixture.factory.handle, 0, &raw mut descriptor) };
    assert_eq!(status, DAUX_OK);
    assert_eq!(descriptor.id.as_str(), GAIN_ID);
    assert_eq!(descriptor.name.as_str(), "Gain");
    assert_eq!(descriptor.vendor.as_str(), "Futureboard");
    assert_eq!(descriptor.category, daux_abi::DAUX_CATEGORY_UTILITY);
    assert_eq!(descriptor.state_schema_version, 3);
    assert_ne!(descriptor.capabilities & daux_abi::DAUX_CAP_HAS_GUI, 0);
    assert_eq!(descriptor.reserved, [0; 8]);

    // SAFETY: as above.
    let status =
        unsafe { (fixture.api().descriptor)(fixture.factory.handle, 99, &raw mut descriptor) };
    assert_eq!(status, DAUX_ERR_NOT_FOUND, "an index past the end");

    // A null out pointer is refused rather than written through.
    // SAFETY: passing null is explicitly part of the entry's contract.
    let status =
        unsafe { (fixture.api().descriptor)(fixture.factory.handle, 0, core::ptr::null_mut()) };
    assert_eq!(status, DAUX_ERR_INVALID_ARG);

    // A caller whose structure is smaller than v1.0 is refused, not overrun.
    let mut small = DauxPluginDescriptorV1::new();
    small.size = 8;
    // SAFETY: `small` is a full-size local; only its declared `size` is small, which is the
    // case under test.
    let status = unsafe { (fixture.api().descriptor)(fixture.factory.handle, 0, &raw mut small) };
    assert_eq!(status, DAUX_ERR_ABI_MISMATCH);
}

#[test]
fn creating_an_unknown_plugin_reports_not_found() {
    let fixture = Fixture::new();
    let mut plugin = DauxPluginV1::null();
    // SAFETY: the handle is live and `plugin` is a writable local.
    let status = unsafe {
        (fixture.api().create_plugin)(
            fixture.factory.handle,
            DauxStrView::from_str("com.example.nope"),
            &raw mut plugin,
        )
    };
    assert_eq!(status, DauxStatus::from_raw(-10), "DAUX_ERR_NOT_FOUND");
    assert!(plugin.is_null(), "nothing may be written on failure");
}

#[test]
fn a_null_handle_is_refused_rather_than_dereferenced() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    let null = daux_abi::DauxPluginHandle::null();

    // SAFETY: a null handle is explicitly part of every entry's contract.
    unsafe {
        assert_eq!((instance.api().init)(null), DAUX_ERR_INVALID_ARG);
        assert_eq!(
            (instance.api().start_processing)(null),
            DAUX_ERR_INVALID_ARG
        );
        assert_eq!(
            (instance.api().process)(null, core::ptr::null()),
            DAUX_PROCESS_ERROR
        );
        assert!((instance.api().get_extension)(null, DauxStrView::from_str(ext::PARAMS)).is_null());
        // The `void` entries must simply not crash.
        (instance.api().deactivate)(null);
        (instance.api().stop_processing)(null);
        (instance.api().reset)(null);
        (instance.api().on_main_thread)(null);
        (instance.api().destroy)(null);
    }
}

#[test]
fn the_lifecycle_state_machine_is_enforced() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    let config = config(128);

    // Nothing is legal before `init`.
    assert_eq!(instance.activate(&config), DAUX_ERR_INVALID_STATE);
    assert_eq!(instance.start(), DAUX_ERR_INVALID_STATE);

    assert_eq!(instance.init(), DAUX_OK);
    assert_eq!(instance.init(), DAUX_ERR_INVALID_STATE, "init twice");

    assert_eq!(instance.activate(&config), DAUX_OK);
    assert_eq!(instance.activate(&config), DAUX_ERR_INVALID_STATE);

    assert_eq!(instance.start(), DAUX_OK);
    assert_eq!(instance.start(), DAUX_ERR_INVALID_STATE);

    // `reset` is "audio-thread, only while not processing".
    instance.reset();
    instance.stop();
    instance.reset();
    instance.deactivate();

    // ...and after deactivating, processing is impossible again.
    assert_eq!(instance.start(), DAUX_ERR_INVALID_STATE);
}

#[test]
fn a_null_or_short_configuration_is_refused() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);

    // SAFETY: a null config is part of `activate`'s contract.
    let status = unsafe { (instance.api().activate)(instance.plugin.handle, core::ptr::null()) };
    assert_eq!(status, DAUX_ERR_INVALID_ARG);

    let mut short = config(64);
    short.size = 4;
    assert_eq!(instance.activate(&short), DAUX_ERR_ABI_MISMATCH);

    // A configuration no plug-in can be sized from is rejected by the model, not obeyed.
    let mut nonsense = config(64);
    nonsense.sample_rate = f64::NAN;
    assert_eq!(instance.activate(&nonsense), DAUX_ERR_INVALID_ARG);
}

#[test]
fn audio_and_events_travel_through_process() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    assert_eq!(instance.activate(&config(64)), DAUX_OK);
    assert_eq!(instance.start(), DAUX_OK);

    let mut block = Block::new(64, 0.5);
    let mut note = DauxEventNoteV1::new();
    note.header = DauxEventHeaderV1::with(daux_abi::DAUX_EVENT_NOTE_ON, DauxEventNoteV1::SIZE, 3);
    note.key = 64;
    // SAFETY: the record is a `#[repr(C)]` aggregate of plain data, fully initialised.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const note).cast::<u8>(),
            core::mem::size_of::<DauxEventNoteV1>(),
        )
    };
    block.in_events.push_bytes(bytes);

    let abi = block.abi(64);
    assert_eq!(
        instance.process(&abi),
        daux_abi::DAUX_PROCESS_CONTINUE_IF_LOUD,
        "the plug-in saw no playing transport"
    );

    // The default gain is 2.0, so 0.5 in is 1.0 out — through the ABI, into host memory.
    assert!(block.output.iter().all(|c| c.iter().all(|s| *s == 1.0)));
    assert_eq!(block.out_events.len(), 1, "the note-end was pushed back");

    instance.stop();
    instance.deactivate();
}

#[test]
fn a_block_the_host_got_wrong_is_refused_without_touching_the_plugin() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    assert_eq!(instance.activate(&config(64)), DAUX_OK);
    assert_eq!(instance.start(), DAUX_OK);

    // SAFETY: a null block is part of `process`'s contract.
    let status = unsafe { (instance.api().process)(instance.plugin.handle, core::ptr::null()) };
    assert_eq!(status, DAUX_PROCESS_ERROR);

    let mut block = Block::new(64, 0.5);

    let mut short = block.abi(64);
    short.size = 8;
    assert_eq!(instance.process(&short), DAUX_PROCESS_ERROR);

    let mut too_long = block.abi(64);
    too_long.frame_count = 4096; // beyond the activated max_block_size
    assert_eq!(instance.process(&too_long), DAUX_PROCESS_ERROR);

    let mut no_buses = block.abi(64);
    no_buses.audio_inputs = core::ptr::null();
    assert_eq!(instance.process(&no_buses), DAUX_PROCESS_ERROR);

    // A zero-frame block is not an error, just nothing to do.
    let mut empty = block.abi(0);
    empty.frame_count = 0;
    assert_eq!(instance.process(&empty), DAUX_PROCESS_CONTINUE);

    instance.stop();
    instance.deactivate();
}

#[test]
fn processing_out_of_order_is_refused_and_silences_the_output() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    assert_eq!(instance.activate(&config(64)), DAUX_OK);

    let mut block = Block::new(64, 0.5);
    for channel in &mut block.output {
        channel.fill(9.0);
    }
    let abi = block.abi(64);
    // `start_processing` has not been called.
    assert_eq!(instance.process(&abi), DAUX_PROCESS_ERROR);
    assert!(
        block.output.iter().all(|c| c.iter().all(|s| *s == 0.0)),
        "a refused block must leave silence, not stale samples"
    );
    instance.deactivate();
}

#[test]
fn a_panicking_process_poisons_the_instance_instead_of_unwinding() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(PANIC_ID);
    assert_eq!(instance.init(), DAUX_OK);
    assert_eq!(instance.activate(&config(32)), DAUX_OK);
    assert_eq!(instance.start(), DAUX_OK);

    let mut block = Block::new(32, 1.0);
    let abi = block.abi(32);
    // The panic is caught at the boundary and becomes an error code (§17.1, §17.2).
    assert_eq!(instance.process(&abi), DAUX_PROCESS_ERROR);

    // ...and the instance is poisoned: every later call is refused rather than re-entering
    // plug-in code that has already broken its own invariants (§17.3).
    assert_eq!(instance.process(&abi), DAUX_PROCESS_ERROR);
    assert_eq!(instance.init(), DAUX_ERR_INVALID_STATE);
    assert_eq!(instance.start(), DAUX_ERR_INVALID_STATE);
    assert_eq!(instance.activate(&config(32)), DAUX_ERR_INVALID_STATE);
    instance.stop();
    instance.deactivate();
    instance.reset();

    // Extensions are refused too, in the way each of them reports refusal.
    assert_eq!(
        // SAFETY: the handle is live.
        unsafe { (instance.params().count)(instance.plugin.handle) },
        0
    );
    let latency: &DauxLatencyApiV1 = instance.extension(ext::LATENCY).expect("standard");
    // SAFETY: the handle is live.
    assert_eq!(unsafe { (latency.get)(instance.plugin.handle) }, 0);

    // Destroying a poisoned instance is still safe — it is the only thing a host may do.
    drop(instance);
}

#[test]
fn a_panicking_factory_refuses_everything_afterwards() {
    let fixture = Fixture::new();
    // The factory panics while constructing this id.
    assert!(fixture.create(EXPLODE_ID).is_none());

    let mut plugin = DauxPluginV1::null();
    // SAFETY: the handle is live and `plugin` is a writable local.
    let status = unsafe {
        (fixture.api().create_plugin)(
            fixture.factory.handle,
            DauxStrView::from_str(EXPLODE_ID),
            &raw mut plugin,
        )
    };
    assert_eq!(status, DAUX_ERR_INVALID_STATE, "the factory is poisoned");

    // Even the entries that cannot fail report the refusal.
    // SAFETY: the handle is live.
    let count = unsafe { (fixture.api().plugin_count)(fixture.factory.handle) };
    assert_eq!(count, 0);
    let mut descriptor = DauxPluginDescriptorV1::new();
    // SAFETY: as above.
    let status =
        unsafe { (fixture.api().descriptor)(fixture.factory.handle, 0, &raw mut descriptor) };
    assert_eq!(status, DAUX_ERR_INVALID_STATE);

    // And a good id is refused as well: the module is not to be trusted any more.
    assert!(fixture.create(GAIN_ID).is_none());
}

#[test]
fn a_panic_is_reported_as_a_panic_the_first_time() {
    let fixture = Fixture::new();
    let mut plugin = DauxPluginV1::null();
    // SAFETY: the handle is live and `plugin` is a writable local.
    let status = unsafe {
        (fixture.api().create_plugin)(
            fixture.factory.handle,
            DauxStrView::from_str(EXPLODE_ID),
            &raw mut plugin,
        )
    };
    assert_eq!(status, DAUX_ERR_PANIC, "§17.2, before §17.3 takes over");
}

#[test]
fn extensions_are_the_standard_ones_and_nothing_else() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);

    assert!(
        instance
            .extension::<DauxAudioPortsApiV1>(ext::AUDIO_PORTS)
            .is_some()
    );
    assert!(instance.extension::<DauxParamsApiV1>(ext::PARAMS).is_some());
    assert!(instance.extension::<DauxStateApiV1>(ext::STATE).is_some());
    assert!(
        instance
            .extension::<DauxLatencyApiV1>(ext::LATENCY)
            .is_some()
    );
    assert!(instance.extension::<DauxTailApiV1>(ext::TAIL).is_some());
    assert!(instance.extension::<DauxRenderApiV1>(ext::RENDER).is_some());
    assert!(instance.extension::<DauxGuiApiV1>(ext::GUI).is_some());

    // Not implemented in v1.0, and not invented here.
    assert!(instance.extension::<u8>(ext::NOTE_PORTS).is_none());
    // Unknown ids return null rather than failing (abi-v1 §11).
    assert!(instance.extension::<u8>("com.example.nonsense/1").is_none());
    assert!(instance.extension::<u8>("").is_none());

    // A headless plug-in offers no GUI table.
    let headless = fixture.create_ok(PANIC_ID);
    assert_eq!(headless.init(), DAUX_OK);
    assert!(headless.extension::<DauxGuiApiV1>(ext::GUI).is_none());
}

#[test]
fn the_audio_ports_extension_publishes_the_declared_layout() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    let ports: &DauxAudioPortsApiV1 = instance.extension(ext::AUDIO_PORTS).expect("standard");

    // SAFETY: the handle is live.
    unsafe {
        assert_eq!((ports.count)(instance.plugin.handle, DAUX_TRUE), 1);
        assert_eq!((ports.count)(instance.plugin.handle, DAUX_FALSE), 1);
    }

    let mut info = DauxAudioPortInfoV1::new();
    // SAFETY: the handle is live and `info` is a writable local.
    let status = unsafe { (ports.get)(instance.plugin.handle, 0, DAUX_TRUE, &raw mut info) };
    assert_eq!(status, DAUX_OK);
    assert_eq!(info.channel_count, 2);
    assert_eq!(info.layout, daux_abi::DAUX_LAYOUT_STEREO);
    assert_eq!(info.purpose, daux_abi::DAUX_PORT_PURPOSE_MAIN);
    assert_ne!(info.flags & daux_abi::DAUX_PORT_FLAG_IS_MAIN, 0);
    assert_eq!(info.reserved, [0; 4]);

    // SAFETY: as above.
    let status = unsafe { (ports.get)(instance.plugin.handle, 7, DAUX_TRUE, &raw mut info) };
    assert_eq!(status, DAUX_ERR_NOT_FOUND);
    // SAFETY: passing null is part of the entry's contract.
    let status =
        unsafe { (ports.get)(instance.plugin.handle, 0, DAUX_TRUE, core::ptr::null_mut()) };
    assert_eq!(status, DAUX_ERR_INVALID_ARG);
    assert!(ports.set_active.is_none(), "not implemented, so null");
}

#[test]
fn parameters_cross_the_abi_as_plain_values() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    let params = instance.params();
    let handle = instance.plugin.handle;

    // SAFETY: the handle is live throughout.
    unsafe {
        assert_eq!((params.count)(handle), 2);

        let mut info = DauxParamInfoV1::new();
        assert_eq!((params.get_info)(handle, 0, &raw mut info), DAUX_OK);
        assert_eq!(info.id, 1);
        assert_eq!(info.name.as_str(), "Gain");
        assert_eq!(info.unit.as_str(), "x");
        assert_eq!(info.min_value, 0.0);
        assert_eq!(info.max_value, 4.0);
        assert_eq!(info.default_value, 2.0);
        assert_eq!(info.cookie, core::ptr::null_mut());
        assert_eq!(
            (params.get_info)(handle, 9, &raw mut info),
            DAUX_ERR_NOT_FOUND
        );

        // The value is plain, not normalised: 2.0 of a 0..4 range would be 0.5 normalised.
        let mut value = 0.0f64;
        assert_eq!((params.get_value)(handle, 1, &raw mut value), DAUX_OK);
        assert_eq!(value, 2.0);
        assert_eq!(
            (params.get_value)(handle, 77, &raw mut value),
            DAUX_ERR_NOT_FOUND
        );
        assert_eq!(
            (params.get_value)(handle, 1, core::ptr::null_mut()),
            DAUX_ERR_INVALID_ARG
        );

        let mut text = DauxText::empty();
        assert_eq!(
            (params.value_to_text)(handle, 1, 3.5, &raw mut text),
            DAUX_OK
        );
        assert!(text.as_str().starts_with("3.5"), "got {:?}", text.as_str());

        let mut parsed = 0.0f64;
        assert_eq!(
            (params.text_to_value)(handle, 1, DauxStrView::from_str("1.25"), &raw mut parsed),
            DAUX_OK
        );
        assert_eq!(parsed, 1.25);
        assert_eq!(
            (params.text_to_value)(
                handle,
                1,
                DauxStrView::from_str("nonsense"),
                &raw mut parsed
            ),
            DAUX_ERR_INVALID_ARG
        );
    }
}

#[test]
fn flush_applies_parameter_values_outside_process() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    let params = instance.params();
    let handle = instance.plugin.handle;

    let list = FakeList::with_capacity(4);
    let mut event = daux_abi::DauxEventParamV1::new();
    event.header = DauxEventHeaderV1::with(
        daux_abi::DAUX_EVENT_PARAM_VALUE,
        daux_abi::DauxEventParamV1::SIZE,
        0,
    );
    event.param_id = 1;
    event.value = 3.0;
    // SAFETY: the record is a `#[repr(C)]` aggregate of plain data, fully initialised.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const event).cast::<u8>(),
            core::mem::size_of::<daux_abi::DauxEventParamV1>(),
        )
    };
    list.push_bytes(bytes);

    // SAFETY: the handle and the list are live for the call.
    unsafe { (params.flush)(handle, list.table(), core::ptr::null()) };

    let mut value = 0.0f64;
    // SAFETY: the handle is live.
    unsafe { (params.get_value)(handle, 1, &raw mut value) };
    assert_eq!(value, 3.0);

    // A null list is tolerated rather than dereferenced.
    // SAFETY: null is part of the entry's contract.
    unsafe { (params.flush)(handle, core::ptr::null(), core::ptr::null()) };
}

#[test]
fn state_round_trips_parameters_and_controller_data() {
    let fixture = Fixture::new();
    let saved = {
        let instance = fixture.create_ok(GAIN_ID);
        assert_eq!(instance.init(), DAUX_OK);
        let params = instance.params();
        // Set the value the way a host would: through an event flush.
        let list = FakeList::with_capacity(2);
        let mut event = daux_abi::DauxEventParamV1::new();
        event.header = DauxEventHeaderV1::with(
            daux_abi::DAUX_EVENT_PARAM_VALUE,
            daux_abi::DauxEventParamV1::SIZE,
            0,
        );
        event.param_id = 1;
        event.value = 3.25;
        // SAFETY: plain `#[repr(C)]` data, fully initialised.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (&raw const event).cast::<u8>(),
                core::mem::size_of::<daux_abi::DauxEventParamV1>(),
            )
        };
        list.push_bytes(bytes);
        // SAFETY: the handle and list are live for the call.
        unsafe { (params.flush)(instance.plugin.handle, list.table(), core::ptr::null()) };

        let stream = FakeStream::new(Vec::new());
        // SAFETY: the handle and the stream are live for the call.
        let status = unsafe { (instance.state().save)(instance.plugin.handle, stream.table()) };
        assert_eq!(status, DAUX_OK);
        stream.written()
    };
    assert!(!saved.is_empty());

    // A fresh instance starts at the default and ends up where the first one was.
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    let stream = FakeStream::new(saved);
    // SAFETY: the handle and the stream are live for the call.
    let status = unsafe { (instance.state().load)(instance.plugin.handle, stream.table()) };
    assert_eq!(status, DAUX_OK);

    let mut value = 0.0f64;
    // SAFETY: the handle is live.
    unsafe { (instance.params().get_value)(instance.plugin.handle, 1, &raw mut value) };
    assert_eq!(value, 3.25, "the parameter came back through the ABI");
}

#[test]
fn a_hostile_or_future_state_blob_is_refused_without_side_effects() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);

    let read_gain = || {
        let mut value = 0.0f64;
        // SAFETY: the handle is live.
        unsafe { (instance.params().get_value)(instance.plugin.handle, 1, &raw mut value) };
        value
    };
    assert_eq!(read_gain(), 2.0);

    // Garbage.
    let stream = FakeStream::new(vec![0xFF; 64]);
    // SAFETY: the handle and stream are live for the call.
    let status = unsafe { (instance.state().load)(instance.plugin.handle, stream.table()) };
    assert_eq!(status, DAUX_ERR_INVALID_ARG);
    assert_eq!(read_gain(), 2.0, "nothing may have been applied");

    // Truncated.
    let mut writer = StateWriter::new(daux_plugin_api::StateVersion(3));
    writer.begin_group("params");
    writer.put_f64("1", 1.0);
    writer.end_group();
    let good = writer.try_finish().expect("writable");
    let stream = FakeStream::new(good[..good.len() / 2].to_vec());
    // SAFETY: as above.
    let status = unsafe { (instance.state().load)(instance.plugin.handle, stream.table()) };
    assert!(status.is_err());
    assert_eq!(read_gain(), 2.0);

    // Written by a newer version of this plug-in than the one loading it (abi-v1 §12).
    let mut writer = StateWriter::new(daux_plugin_api::StateVersion(99));
    writer.begin_group("params");
    writer.put_f64("1", 1.0);
    writer.end_group();
    let future = writer.try_finish().expect("writable");
    let stream = FakeStream::new(future);
    // SAFETY: as above.
    let status = unsafe { (instance.state().load)(instance.plugin.handle, stream.table()) };
    assert_eq!(status, DAUX_ERR_VERSION);
    assert_eq!(read_gain(), 2.0);

    // A null stream is refused rather than dereferenced.
    // SAFETY: null is part of the entry's contract.
    let status = unsafe { (instance.state().load)(instance.plugin.handle, core::ptr::null()) };
    assert_eq!(status, DAUX_ERR_INVALID_ARG);
    // SAFETY: as above.
    let status = unsafe { (instance.state().save)(instance.plugin.handle, core::ptr::null()) };
    assert_eq!(status, DAUX_ERR_INVALID_ARG);
}

/// abi-v1 §12: "`load` is atomic from the host's point of view."
#[test]
fn a_load_the_controller_refuses_leaves_every_parameter_where_it_was() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);

    let read_gain = || {
        let mut value = 0.0f64;
        // SAFETY: the handle is live.
        unsafe { (instance.params().get_value)(instance.plugin.handle, 1, &raw mut value) };
        value
    };
    assert_eq!(read_gain(), 2.0);

    // A blob whose parameter values are perfectly loadable, and whose controller half this
    // plug-in rejects. The parameter write happens first, so without the snapshot the gain
    // would be left at 0.5 by a load the host was told had failed.
    let mut writer = StateWriter::new(daux_plugin_api::StateVersion(3));
    writer.begin_group("params");
    writer.put_f64("1", 0.5);
    writer.end_group();
    writer.put_str("label", "refuse");
    let blob = writer.try_finish().expect("writable");

    let stream = FakeStream::new(blob);
    // SAFETY: the handle and the stream are live for the call.
    let status = unsafe { (instance.state().load)(instance.plugin.handle, stream.table()) };
    assert!(status.is_err(), "the controller refused the blob");
    assert_eq!(read_gain(), 2.0, "a refused load must change nothing");
}

/// abi-v1 §14: a parameter id is permanent, so a renamed one has to be replayed on load.
#[test]
fn a_saved_value_follows_a_renamed_parameter_and_a_removed_one_is_dropped() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);

    // State written by a version of this plug-in whose gain was parameter 5, which also had a
    // parameter 6 that no longer exists, alongside an id this build has never heard of.
    let mut writer = StateWriter::new(daux_plugin_api::StateVersion(3));
    writer.begin_group("params");
    writer.put_f64("5", 1.5);
    writer.put_f64("6", 9.9);
    writer.put_f64("404", 9.9);
    writer.put_str("not-an-id", "ignored");
    writer.end_group();
    let blob = writer.try_finish().expect("writable");

    let stream = FakeStream::new(blob);
    // SAFETY: the handle and the stream are live for the call.
    let status = unsafe { (instance.state().load)(instance.plugin.handle, stream.table()) };
    assert_eq!(status, DAUX_OK);

    let mut value = 0.0f64;
    // SAFETY: the handle is live.
    unsafe { (instance.params().get_value)(instance.plugin.handle, 1, &raw mut value) };
    assert_eq!(value, 1.5, "the value followed the rename from 5 to 1");
}

#[test]
fn latency_tail_and_render_report_what_the_plugin_says() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    let handle = instance.plugin.handle;

    let latency: &DauxLatencyApiV1 = instance.extension(ext::LATENCY).expect("standard");
    let tail: &DauxTailApiV1 = instance.extension(ext::TAIL).expect("standard");
    let render: &DauxRenderApiV1 = instance.extension(ext::RENDER).expect("standard");

    // SAFETY: the handle is live throughout.
    unsafe {
        assert_eq!((latency.get)(handle), 64);
        assert_eq!((tail.get)(handle), 128);
        assert_eq!(
            (render.has_hard_realtime_requirement)(handle),
            DAUX_TRUE,
            "the descriptor declares DAUX_CAP_HARD_REALTIME"
        );

        // The mode may only change while inactive (abi-v1 §11.5).
        assert_eq!(
            (render.set_mode)(handle, daux_abi::DAUX_PROCESS_MODE_OFFLINE),
            DAUX_OK
        );
        assert_eq!(instance.activate(&config(64)), DAUX_OK);
        assert_eq!(
            (render.set_mode)(handle, daux_abi::DAUX_PROCESS_MODE_REALTIME),
            DAUX_ERR_INVALID_STATE
        );

        // ...and it really reached `prepare`, which is the only thing that makes setting it
        // worth anything: the plug-in echoed the mode it was activated with into parameter 2.
        let mut applied = 0.0f64;
        assert_eq!(
            (instance.params().get_value)(handle, 2, &raw mut applied),
            DAUX_OK
        );
        assert_eq!(applied, f64::from(daux_abi::DAUX_PROCESS_MODE_OFFLINE));

        instance.deactivate();
        assert_eq!(
            (render.set_mode)(handle, daux_abi::DAUX_PROCESS_MODE_REALTIME),
            DAUX_OK
        );
        assert_eq!(instance.activate(&config(64)), DAUX_OK);
        assert_eq!(
            (instance.params().get_value)(handle, 2, &raw mut applied),
            DAUX_OK
        );
        assert_eq!(applied, f64::from(daux_abi::DAUX_PROCESS_MODE_REALTIME));
        instance.deactivate();
    }
}

#[test]
fn an_editor_opens_and_closes_without_touching_the_processor() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    assert_eq!(instance.activate(&config(64)), DAUX_OK);
    assert_eq!(instance.start(), DAUX_OK);

    let gui: &DauxGuiApiV1 = instance
        .extension(ext::GUI)
        .expect("the descriptor claims a GUI");
    let handle = instance.plugin.handle;
    let native = daux_plugin_api::WindowApi::PLATFORM.as_bits();
    let opens_before = EDITOR_OPENS.load(Ordering::Relaxed);
    let closes_before = EDITOR_CLOSES.load(Ordering::Relaxed);

    // SAFETY: the handle is live throughout, and every pointer below is a live local.
    unsafe {
        assert_eq!(
            (gui.is_api_supported)(handle, native, DAUX_FALSE),
            DAUX_TRUE
        );
        assert_eq!(
            (gui.is_api_supported)(handle, native, DAUX_TRUE),
            DAUX_FALSE,
            "floating editors are refused, not faked"
        );
        assert_eq!((gui.is_api_supported)(handle, 99, DAUX_FALSE), DAUX_FALSE);

        assert_eq!(
            (gui.create)(handle, native, DAUX_TRUE),
            DAUX_ERR_UNSUPPORTED
        );
        assert_eq!((gui.create)(handle, 99, DAUX_FALSE), DAUX_ERR_INVALID_ARG);
        assert_eq!((gui.create)(handle, native, DAUX_FALSE), DAUX_OK);
        assert_eq!(
            (gui.create)(handle, native, DAUX_FALSE),
            DAUX_ERR_INVALID_STATE,
            "the host must destroy before creating again"
        );

        let mut width = 0u32;
        let mut height = 0u32;
        assert_eq!(
            (gui.get_size)(handle, &raw mut width, &raw mut height),
            DAUX_OK
        );
        assert_eq!((width, height), (400, 300));
        assert_eq!((gui.can_resize)(handle), DAUX_TRUE);

        // A proposed size below the minimum is clamped, not accepted.
        let mut w = 10u32;
        let mut h = 10u32;
        let adjust = gui.adjust_size.expect("this adapter implements it");
        assert_eq!(adjust(handle, &raw mut w, &raw mut h), DAUX_OK);
        assert_eq!((w, h), (200, 150));

        assert_eq!((gui.set_size)(handle, 640, 480), DAUX_OK);
        assert_eq!(
            (gui.get_size)(handle, &raw mut width, &raw mut height),
            DAUX_OK
        );
        assert_eq!((width, height), (640, 480));

        let set_scale = gui.set_scale.expect("this adapter implements it");
        assert_eq!(set_scale(handle, 2.0), DAUX_OK);
        assert_eq!(set_scale(handle, 0.0), DAUX_ERR_INVALID_ARG);

        let mut window = DauxWindowV1::new();
        window.api = native;
        window.handle = 0x1234 as *mut core::ffi::c_void;
        assert_eq!((gui.set_parent)(handle, &raw const window), DAUX_OK);
        assert_eq!(
            (gui.set_parent)(handle, &raw const window),
            DAUX_ERR_INVALID_STATE,
            "open is called at most once without a close"
        );
        assert_eq!((gui.show)(handle), DAUX_OK);
        assert_eq!((gui.hide)(handle), DAUX_OK);
        assert_eq!(
            (gui.set_parent)(handle, core::ptr::null()),
            DAUX_ERR_INVALID_ARG
        );

        (gui.destroy)(handle);
        // Everything is refused again once the editor is gone...
        assert_eq!((gui.show)(handle), DAUX_ERR_INVALID_STATE);
        assert_eq!((gui.can_resize)(handle), DAUX_FALSE);
        // ...and destroying twice is harmless.
        (gui.destroy)(handle);

        // The editor really was opened and closed, exactly once each.
        assert_eq!(EDITOR_OPENS.load(Ordering::Relaxed) - opens_before, 1);
        assert_eq!(EDITOR_CLOSES.load(Ordering::Relaxed) - closes_before, 1);

        // A user may open and close an editor a hundred times while audio never stops, which
        // is rule 9: the second cycle must work exactly like the first.
        assert_eq!((gui.create)(handle, native, DAUX_FALSE), DAUX_OK);
        assert_eq!((gui.set_parent)(handle, &raw const window), DAUX_OK);
        (gui.destroy)(handle);
        assert_eq!(EDITOR_OPENS.load(Ordering::Relaxed) - opens_before, 2);
        assert_eq!(EDITOR_CLOSES.load(Ordering::Relaxed) - closes_before, 2);

        // The processor never noticed any of it.
        assert_eq!(
            instance.process(&Block::new(64, 1.0).abi(64)),
            daux_abi::DAUX_PROCESS_CONTINUE_IF_LOUD
        );
    }

    instance.stop();
    instance.deactivate();
}

/// The first architectural rule, as a measurement: the audio thread never allocates.
///
/// Everything the block path needs — the bus views, the event adapters, the context — is
/// either preallocated in `activate` or a stack value, so a hundred blocks must cost zero
/// allocations. A regression here is invisible until a DAW glitches under load, which is why
/// it is asserted rather than reasoned about.
#[test]
fn a_block_allocates_nothing() {
    assert!(
        daux_plugin_api::daux_rt::counting_allocator_installed(),
        "the tripwire is not installed, so this test would pass vacuously"
    );

    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    assert_eq!(instance.activate(&config(64)), DAUX_OK);
    assert_eq!(instance.start(), DAUX_OK);

    let mut block = Block::new(64, 0.25);
    let mut note = DauxEventNoteV1::new();
    note.header = DauxEventHeaderV1::with(daux_abi::DAUX_EVENT_NOTE_ON, DauxEventNoteV1::SIZE, 0);
    // SAFETY: the record is a `#[repr(C)]` aggregate of plain data, fully initialised.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const note).cast::<u8>(),
            core::mem::size_of::<DauxEventNoteV1>(),
        )
    };
    block.in_events.push_bytes(bytes);
    let mut transport = daux_abi::DauxTransportV1::new();
    transport.flags = daux_abi::DAUX_TRANSPORT_IS_PLAYING | daux_abi::DAUX_TRANSPORT_HAS_TEMPO;
    transport.tempo = 120.0;
    let mut abi = block.abi(64);
    abi.transport = &raw const transport;
    abi.steady_time = 4_096;

    // One warm-up block, so nothing lazily initialised is counted.
    assert_eq!(instance.process(&abi), DAUX_PROCESS_CONTINUE);

    let ((), allocations) = daux_plugin_api::daux_rt::AllocGuard::scope(|| {
        for _ in 0..100 {
            assert_eq!(instance.process(&abi), DAUX_PROCESS_CONTINUE);
        }
    });
    assert_eq!(allocations, 0, "the audio path allocated");

    instance.stop();
    instance.deactivate();
}

#[test]
fn hundreds_of_instances_coexist_in_one_process() {
    let fixture = Fixture::new();
    let instances: Vec<Instance> = (0..256).map(|_| fixture.create_ok(GAIN_ID)).collect();
    for instance in &instances {
        assert_eq!(instance.init(), DAUX_OK);
        assert_eq!(instance.activate(&config(16)), DAUX_OK);
        assert_eq!(instance.start(), DAUX_OK);
    }

    // Each one has its own parameter state; changing one must not move another.
    let list = FakeList::with_capacity(2);
    let mut event = daux_abi::DauxEventParamV1::new();
    event.header = DauxEventHeaderV1::with(
        daux_abi::DAUX_EVENT_PARAM_VALUE,
        daux_abi::DauxEventParamV1::SIZE,
        0,
    );
    event.param_id = 1;
    event.value = 4.0;
    // SAFETY: plain `#[repr(C)]` data, fully initialised.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const event).cast::<u8>(),
            core::mem::size_of::<daux_abi::DauxEventParamV1>(),
        )
    };
    list.push_bytes(bytes);
    // SAFETY: the handle and list are live for the call.
    unsafe {
        (instances[0].params().flush)(instances[0].plugin.handle, list.table(), core::ptr::null());
    }

    let read = |instance: &Instance| {
        let mut value = 0.0f64;
        // SAFETY: the handle is live.
        unsafe { (instance.params().get_value)(instance.plugin.handle, 1, &raw mut value) };
        value
    };
    assert_eq!(read(&instances[0]), 4.0);
    assert_eq!(read(&instances[1]), 2.0);
    assert_eq!(read(&instances[255]), 2.0);

    for instance in &instances {
        instance.stop();
        instance.deactivate();
    }
}

#[test]
fn a_module_created_without_a_host_still_works() {
    // abi-v1 §18: a plug-in survives a host that provides nothing.
    let mut factory = DauxFactoryV1::null();
    // SAFETY: a null host is explicitly allowed by `create_factory`'s contract, and `factory`
    // is a writable local.
    let status = unsafe { (entry().create_factory)(core::ptr::null(), &raw mut factory) };
    assert_eq!(status, DAUX_OK);

    // SAFETY: the table came from `create_factory`.
    let api = unsafe { &*factory.api };
    let mut plugin = DauxPluginV1::null();
    // SAFETY: the handle is live and `plugin` is a writable local.
    let status = unsafe {
        (api.create_plugin)(
            factory.handle,
            DauxStrView::from_str(GAIN_ID),
            &raw mut plugin,
        )
    };
    assert_eq!(status, DAUX_OK);
    let instance = Instance { plugin };
    assert_eq!(instance.init(), DAUX_OK);
    assert_eq!(instance.activate(&config(8)), DAUX_OK);
    instance.deactivate();
    drop(instance);

    // SAFETY: every instance is destroyed, and the pair is the one `create_factory` produced.
    unsafe { (entry().destroy_factory)(factory) };

    // A null out pointer is refused rather than written through.
    // SAFETY: null is part of the contract.
    let status = unsafe { (entry().create_factory)(core::ptr::null(), core::ptr::null_mut()) };
    assert_eq!(status, DAUX_ERR_INVALID_ARG);
}

#[test]
fn destroying_a_foreign_or_null_factory_is_ignored_rather_than_freed() {
    // A handle this module never produced must not be passed to `Box::from_raw`.
    let mut foreign = DauxFactoryV1::null();
    foreign.handle =
        daux_abi::DauxFactoryHandle::from_ptr(std::ptr::dangling_mut::<core::ffi::c_void>());
    // SAFETY: the pair carries a null `api`, which the entry uses to recognise that the handle
    // is not one of its own — the case under test.
    unsafe { (entry().destroy_factory)(foreign) };
    // SAFETY: a null pair is the other half of the same case.
    unsafe { (entry().destroy_factory)(DauxFactoryV1::null()) };
}

#[test]
fn the_host_bridge_reaches_the_host_it_was_given() {
    let fixture = Fixture::new();
    let instance = fixture.create_ok(GAIN_ID);
    assert_eq!(instance.init(), DAUX_OK);
    // The plug-in has not logged anything, but the bridge resolved the host's log extension,
    // which is what `create_factory` is responsible for.
    assert_eq!(fixture.host.state.logs.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.host.state.callbacks.load(Ordering::Relaxed), 0);
    drop(instance);
}

#[test]
fn the_compatibility_report_is_empty_for_a_plugin_this_format_can_express() {
    let report = crate::compatibility_report(&Gain::descriptor());
    assert!(report.is_empty(), "{report:?}");
}
