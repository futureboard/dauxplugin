//! Fake plug-ins, a fake host and a fake process block, for driving the exported C entry
//! points without a DAW.
//!
//! Testing a format adapter through its Rust internals proves very little: what a host
//! actually touches is a table of `extern "C"` function pointers and a pile of raw
//! pointers. Everything here exists so the tests can be written from that side — build a
//! `clap_process`, call `plugin->process(plugin, &process)`, and look at what came back.

use core::cell::{Cell, RefCell};
use core::ffi::{CStr, c_char, c_void};
use core::ptr;

use daux_plugin_api::{
    AudioBuses, BusLayout, Capabilities, Category, DauxController, DauxError, DauxFactory,
    DauxPlugin, DauxProcessor, DauxResult, ErrorKind, EventPortLayout, FloatParam, Param, ParamId,
    ParamRange, Params, PluginDescriptor, ProcessConfig, ProcessContext, ProcessEvents,
    ProcessStatus, StateReader, StateWriter,
};

use crate::abi::{
    ClapAudioBuffer, ClapHost, ClapIStream, ClapInputEvents, ClapOStream, ClapOutputEvents,
    ClapProcess, ClapVersion,
};

/// Channels on every bus the fake host builds.
const CHANNELS: usize = 2;

/// Reads a C string the way a host would.
///
/// # Safety
///
/// `p` must be a live NUL-terminated string.
pub unsafe fn read_c(p: *const c_char) -> String {
    // SAFETY: the caller guarantees `p` is a live NUL-terminated string.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------------------
// a plug-in that works
// ---------------------------------------------------------------------------------------

/// One automatable parameter, so the params extension has something to serve.
pub struct TestParams {
    /// Gain in dB, `-60 ..= 12`.
    pub gain: FloatParam,
}

impl Default for TestParams {
    fn default() -> Self {
        Self {
            gain: FloatParam::new(
                ParamId(1),
                "Gain",
                0.0,
                ParamRange::Linear {
                    min: -60.0,
                    max: 12.0,
                },
            )
            .with_unit("dB"),
        }
    }
}

impl Params for TestParams {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        vec![(ParamId(1), &self.gain as &dyn Param)]
    }

    fn param(&self, id: ParamId) -> Option<&dyn Param> {
        // Overridden so the audio-thread `flush` path never allocates, as the `Params`
        // contract requires of anything reachable from `process`.
        (id == ParamId(1)).then_some(&self.gain as &dyn Param)
    }
}

/// A stereo effect that copies its input through, with one parameter and no editor.
#[derive(Default)]
pub struct TestPlugin {
    /// The parameter set, shared between the processor and controller halves.
    pub params: TestParams,
    /// Set by `prepare`, so a test can tell activation happened.
    pub prepared: Option<ProcessConfig>,
    /// Counts `reset` calls.
    pub resets: usize,
    /// Extra state the controller round-trips through `save_state`/`load_state`.
    pub note: String,
}

impl DauxProcessor for TestPlugin {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;
        self.prepared = Some(*config);
        Ok(())
    }

    fn reset(&mut self) {
        self.resets += 1;
    }

    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        _events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        let Some(input) = audio.main_input() else {
            audio.silence_outputs();
            return ProcessStatus::Continue;
        };
        if let Some(mut output) = audio.main_output() {
            let _ = output.copy_from(&input);
        }
        ProcessStatus::Continue
    }
}

impl DauxController for TestPlugin {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        w.put_str("note", &self.note);
        Ok(())
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        self.note = r.opt_str("note").unwrap_or_default().to_owned();
        Ok(())
    }
}

impl DauxPlugin for TestPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder("com.example.clap-test", "CLAP Test")
            .vendor("Example Audio")
            .category(Category::Effect)
            .capabilities(
                Capabilities::AUDIO_EFFECT
                    .with_midi_input()
                    .with_midi_output(),
            )
            .build()
            .expect("the test descriptor is valid")
    }

    fn bus_layout(&self) -> BusLayout {
        BusLayout::stereo_effect()
    }

    fn event_ports(&self) -> EventPortLayout {
        EventPortLayout::midi_effect()
    }

    fn processor(&mut self) -> &mut dyn DauxProcessor {
        self
    }

    fn controller(&mut self) -> &mut dyn DauxController {
        self
    }
}

/// A factory exporting exactly [`TestPlugin`].
#[derive(Default)]
pub struct TestFactory;

impl DauxFactory for TestFactory {
    fn plugin_count(&self) -> usize {
        1
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        (index == 0).then(TestPlugin::descriptor)
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        if id == "com.example.clap-test" {
            Ok(Box::new(TestPlugin::default()))
        } else {
            Err(DauxError::new(ErrorKind::NotFound, "no such plug-in"))
        }
    }
}

// ---------------------------------------------------------------------------------------
// a factory that refuses to build
// ---------------------------------------------------------------------------------------

/// A factory that advertises a plug-in it will never build — the shape of a licence check
/// that failed, and the case where returning null matters.
#[derive(Default)]
pub struct FailingFactory;

impl DauxFactory for FailingFactory {
    fn plugin_count(&self) -> usize {
        1
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        (index == 0).then(|| {
            PluginDescriptor::builder("com.example.clap-fail", "CLAP Fail")
                .build()
                .expect("the failing descriptor is valid")
        })
    }

    fn create(&self, _id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        Err(DauxError::new(ErrorKind::Plugin, "refusing to instantiate"))
    }
}

// ---------------------------------------------------------------------------------------
// a plug-in that panics on demand
// ---------------------------------------------------------------------------------------

/// Where the next call into [`PanickingPlugin`] should panic.
///
/// A thread-local switch rather than a field, because the panic has to be triggered from a
/// test that only holds a raw `clap_plugin *`.
pub struct PanicPoint;

thread_local! {
    /// `0` = nowhere, `1` = in `process`, `2` = in the parameter lookup.
    static ARMED: Cell<u8> = const { Cell::new(0) };
}

impl PanicPoint {
    /// The next `process` call panics.
    pub fn arm_process() {
        ARMED.set(1);
    }

    /// The next parameter lookup panics.
    pub fn arm_params() {
        ARMED.set(2);
    }

    /// Nothing panics.
    pub fn disarm() {
        ARMED.set(0);
    }

    /// Whether the given site is armed.
    fn armed(site: u8) -> bool {
        ARMED.get() == site
    }
}

/// Parameters that panic when [`PanicPoint::arm_params`] is in effect.
#[derive(Default)]
pub struct PanickingParams {
    /// A real parameter, so the extension has something to serve when disarmed.
    gain: Option<Box<FloatParam>>,
}

impl PanickingParams {
    /// The parameter, created on first use so `Default` stays trivial.
    fn gain(&self) -> &FloatParam {
        self.gain
            .as_deref()
            .expect("the parameter is built in `new`")
    }

    /// A parameter set with its one parameter built.
    fn new() -> Self {
        Self {
            gain: Some(Box::new(FloatParam::new(
                ParamId(1),
                "Gain",
                0.0,
                ParamRange::Linear { min: 0.0, max: 1.0 },
            ))),
        }
    }
}

impl Params for PanickingParams {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        assert!(
            !PanicPoint::armed(2),
            "PanickingParams::param_refs was armed to panic"
        );
        vec![(ParamId(1), self.gain() as &dyn Param)]
    }

    fn param(&self, id: ParamId) -> Option<&dyn Param> {
        assert!(
            !PanicPoint::armed(2),
            "PanickingParams::param was armed to panic"
        );
        (id == ParamId(1)).then(|| self.gain() as &dyn Param)
    }
}

/// A plug-in that panics wherever [`PanicPoint`] says it should.
pub struct PanickingPlugin {
    /// The parameter set.
    params: PanickingParams,
}

impl Default for PanickingPlugin {
    fn default() -> Self {
        Self {
            params: PanickingParams::new(),
        }
    }
}

impl DauxProcessor for PanickingPlugin {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()
    }

    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        _events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        assert!(
            !PanicPoint::armed(1),
            "PanickingPlugin::process was armed to panic"
        );
        audio.silence_outputs();
        ProcessStatus::Continue
    }
}

impl DauxController for PanickingPlugin {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    fn save_state(&self, _w: &mut StateWriter) -> DauxResult<()> {
        Ok(())
    }

    fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> {
        Ok(())
    }
}

impl DauxPlugin for PanickingPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder("com.example.clap-panic", "CLAP Panic")
            .capabilities(Capabilities::AUDIO_EFFECT)
            .build()
            .expect("the panicking descriptor is valid")
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

/// A factory exporting exactly [`PanickingPlugin`].
#[derive(Default)]
pub struct PanickingFactory;

impl DauxFactory for PanickingFactory {
    fn plugin_count(&self) -> usize {
        1
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        (index == 0).then(PanickingPlugin::descriptor)
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        if id == "com.example.clap-panic" {
            Ok(Box::new(PanickingPlugin::default()))
        } else {
            Err(DauxError::new(ErrorKind::NotFound, "no such plug-in"))
        }
    }
}

// ---------------------------------------------------------------------------------------
// the fake host
// ---------------------------------------------------------------------------------------

unsafe extern "C" fn host_get_extension(
    _host: *const ClapHost,
    _id: *const c_char,
) -> *const c_void {
    // A host with no extensions at all: the case every plug-in must survive.
    ptr::null()
}

unsafe extern "C" fn host_nothing(_host: *const ClapHost) {}

/// A minimal `clap_host`: identity, the request trio, and no extensions.
pub struct TestHost {
    /// The table handed to the adapter. Boxed so its address survives moves.
    host: Box<ClapHost>,
}

impl TestHost {
    /// Builds the fake host.
    #[must_use]
    pub fn new() -> Self {
        Self {
            host: Box::new(ClapHost {
                clap_version: ClapVersion::CURRENT,
                host_data: ptr::null_mut(),
                name: c"Test Host".as_ptr(),
                vendor: c"DAUxPlug".as_ptr(),
                url: c"https://example.com".as_ptr(),
                version: c"0.1.0".as_ptr(),
                get_extension: Some(host_get_extension),
                request_restart: Some(host_nothing),
                request_process: Some(host_nothing),
                request_callback: Some(host_nothing),
            }),
        }
    }

    /// The pointer to hand to `create_plugin`.
    #[must_use]
    pub fn as_ptr(&self) -> *const ClapHost {
        ptr::from_ref(self.host.as_ref())
    }

    /// Builds a process block with `inputs`/`outputs` stereo buses of `frames` frames.
    ///
    /// Every input sample starts at `1.0` and every output sample at `0.0`, so "the plug-in
    /// wrote something" and "the adapter silenced the outputs" are distinguishable.
    #[must_use]
    pub fn block(&self, inputs: usize, outputs: usize, frames: usize) -> Block {
        Block::new(inputs, outputs, frames)
    }
}

/// One `clap_process` and every buffer it points at.
///
/// The pointers are recomputed by [`Block::as_ptr`] rather than fixed at construction, so
/// moving the block around a test cannot leave a dangling `clap_audio_buffer`.
pub struct Block {
    /// Input samples, `bus * CHANNELS + channel`.
    in_data: Vec<Vec<f32>>,
    /// Output samples, `bus * CHANNELS + channel`.
    out_data: Vec<Vec<f32>>,
    /// Channel pointers into `in_data`.
    in_ptrs: Vec<*mut f32>,
    /// Channel pointers into `out_data`.
    out_ptrs: Vec<*mut f32>,
    /// One `clap_audio_buffer` per input bus.
    in_bufs: Vec<ClapAudioBuffer>,
    /// One `clap_audio_buffer` per output bus.
    out_bufs: Vec<ClapAudioBuffer>,
    /// An empty input event list.
    in_events: ClapInputEvents,
    /// An output event sink that accepts and discards.
    out_events: ClapOutputEvents,
    /// The struct handed to `process`.
    process: ClapProcess,
    /// Frames per channel.
    frames: usize,
}

unsafe extern "C" fn no_events(_list: *const ClapInputEvents) -> u32 {
    0
}

unsafe extern "C" fn no_event(
    _list: *const ClapInputEvents,
    _index: u32,
) -> *const crate::abi::ClapEventHeader {
    ptr::null()
}

unsafe extern "C" fn accept_event(
    _list: *const ClapOutputEvents,
    _event: *const crate::abi::ClapEventHeader,
) -> bool {
    true
}

impl Block {
    /// Allocates the buffers for one block.
    fn new(inputs: usize, outputs: usize, frames: usize) -> Self {
        let in_data: Vec<Vec<f32>> = (0..inputs * CHANNELS).map(|_| vec![1.0; frames]).collect();
        let out_data: Vec<Vec<f32>> = (0..outputs * CHANNELS).map(|_| vec![0.0; frames]).collect();
        Self {
            in_ptrs: vec![ptr::null_mut(); in_data.len()],
            out_ptrs: vec![ptr::null_mut(); out_data.len()],
            in_bufs: (0..inputs)
                .map(|_| ClapAudioBuffer {
                    data32: ptr::null_mut(),
                    data64: ptr::null_mut(),
                    channel_count: CHANNELS as u32,
                    latency: 0,
                    constant_mask: 0,
                })
                .collect(),
            out_bufs: (0..outputs)
                .map(|_| ClapAudioBuffer {
                    data32: ptr::null_mut(),
                    data64: ptr::null_mut(),
                    channel_count: CHANNELS as u32,
                    latency: 0,
                    constant_mask: 0,
                })
                .collect(),
            in_events: ClapInputEvents {
                ctx: ptr::null_mut(),
                size: Some(no_events),
                get: Some(no_event),
            },
            out_events: ClapOutputEvents {
                ctx: ptr::null_mut(),
                try_push: Some(accept_event),
            },
            process: ClapProcess {
                steady_time: 0,
                frames_count: frames as u32,
                transport: ptr::null(),
                audio_inputs: ptr::null(),
                audio_outputs: ptr::null_mut(),
                audio_inputs_count: inputs as u32,
                audio_outputs_count: outputs as u32,
                in_events: ptr::null(),
                out_events: ptr::null(),
            },
            in_data,
            out_data,
            frames,
        }
    }

    /// Rebuilds every pointer and hands back the `clap_process` a host would pass.
    pub fn as_ptr(&mut self) -> *const ClapProcess {
        for (i, channel) in self.in_data.iter_mut().enumerate() {
            self.in_ptrs[i] = channel.as_mut_ptr();
        }
        for (i, channel) in self.out_data.iter_mut().enumerate() {
            self.out_ptrs[i] = channel.as_mut_ptr();
        }
        for bus in 0..self.in_bufs.len() {
            let base = self.in_ptrs.as_mut_ptr().wrapping_add(bus * CHANNELS);
            self.in_bufs[bus].data32 = base;
        }
        for bus in 0..self.out_bufs.len() {
            let base = self.out_ptrs.as_mut_ptr().wrapping_add(bus * CHANNELS);
            self.out_bufs[bus].data32 = base;
        }
        self.process.audio_inputs = self.in_bufs.as_ptr();
        self.process.audio_outputs = self.out_bufs.as_mut_ptr();
        self.process.in_events = ptr::from_ref(&self.in_events);
        self.process.out_events = ptr::from_ref(&self.out_events);
        ptr::from_ref(&self.process)
    }

    /// The samples of one output channel.
    #[must_use]
    pub fn output(&self, bus: usize, channel: usize) -> &[f32] {
        &self.out_data[bus * CHANNELS + channel]
    }

    /// Overwrites one input channel.
    pub fn fill_input(&mut self, bus: usize, channel: usize, value: f32) {
        self.in_data[bus * CHANNELS + channel].fill(value);
    }

    /// How many frames this block covers.
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }
}

// ---------------------------------------------------------------------------------------
// a fake input event list
// ---------------------------------------------------------------------------------------

/// A `clap_input_events` carrying parameter-value events, for driving `params.flush`.
pub struct EventList {
    /// The encoded events.
    ///
    /// Boxed on purpose: `ctx` is a raw pointer to this vector, so its address has to
    /// survive the `EventList` being moved — which is what the extra indirection buys and
    /// why `clippy::box_collection` is wrong here.
    #[allow(clippy::box_collection)]
    blobs: Box<Vec<Vec<u8>>>,
    /// The view handed to the adapter.
    view: ClapInputEvents,
}

unsafe extern "C" fn list_size(list: *const ClapInputEvents) -> u32 {
    // SAFETY: the adapter passes back the pointer it was given, whose `ctx` an `EventList`
    // set to its own live blob vector.
    let blobs = unsafe { &*(*list).ctx.cast::<Vec<Vec<u8>>>() };
    blobs.len() as u32
}

unsafe extern "C" fn list_get(
    list: *const ClapInputEvents,
    index: u32,
) -> *const crate::abi::ClapEventHeader {
    // SAFETY: as in `list_size`.
    let blobs = unsafe { &*(*list).ctx.cast::<Vec<Vec<u8>>>() };
    blobs
        .get(index as usize)
        .map_or(ptr::null(), |b| b.as_ptr().cast())
}

impl EventList {
    /// One `CLAP_EVENT_PARAM_VALUE` per `(param_id, plain value)` pair, at time zero.
    #[must_use]
    pub fn with_param_values(values: &[(u32, f64)]) -> Self {
        let blobs: Vec<Vec<u8>> = values
            .iter()
            .map(|(id, value)| {
                let event = crate::abi::ClapEventParamValue {
                    header: crate::abi::ClapEventHeader {
                        size: size_of::<crate::abi::ClapEventParamValue>() as u32,
                        time: 0,
                        space_id: crate::abi::CLAP_CORE_EVENT_SPACE_ID,
                        type_: crate::abi::CLAP_EVENT_PARAM_VALUE,
                        flags: 0,
                    },
                    param_id: *id,
                    cookie: ptr::null_mut(),
                    note_id: -1,
                    port_index: -1,
                    channel: -1,
                    key: -1,
                    value: *value,
                };
                let bytes = ptr::from_ref(&event).cast::<u8>();
                // SAFETY: `event` is a live, fully initialised `#[repr(C)]` struct, so the
                // bytes behind it are readable for its whole size. The copy is byte-for-byte,
                // which is what a host's arena holds.
                unsafe { core::slice::from_raw_parts(bytes, size_of_val(&event)) }.to_vec()
            })
            .collect();
        let mut blobs = Box::new(blobs);
        let ctx = ptr::from_mut(blobs.as_mut()).cast();
        Self {
            blobs,
            view: ClapInputEvents {
                ctx,
                size: Some(list_size),
                get: Some(list_get),
            },
        }
    }

    /// The pointer to hand to `params.flush`.
    #[must_use]
    pub fn as_ptr(&self) -> *const ClapInputEvents {
        assert!(!self.blobs.is_empty() || self.blobs.is_empty());
        ptr::from_ref(&self.view)
    }
}

// ---------------------------------------------------------------------------------------
// in-memory CLAP streams
// ---------------------------------------------------------------------------------------

/// A `clap_ostream`/`clap_istream` pair over one `Vec<u8>`, plus the knobs a hostile host
/// would turn: short writes, short reads, and hard failures.
pub struct TestStream {
    /// The bytes written so far, or the bytes to be read.
    bytes: Box<RefCell<StreamState>>,
    /// The write half.
    ostream: ClapOStream,
    /// The read half.
    istream: ClapIStream,
}

/// What a [`TestStream`]'s callbacks operate on.
pub struct StreamState {
    /// The buffer itself.
    pub data: Vec<u8>,
    /// Read cursor.
    pub read_at: usize,
    /// Largest number of bytes one call will move; `0` means "no limit".
    pub chunk: usize,
    /// When true, every call reports an error.
    pub fail: bool,
}

unsafe extern "C" fn stream_write(
    stream: *const ClapOStream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    // SAFETY: the adapter passes back the pointer it was given, whose `ctx` a `TestStream`
    // set to its own live state.
    let state = unsafe { &*(*stream).ctx.cast::<RefCell<StreamState>>() };
    let mut state = state.borrow_mut();
    if state.fail {
        return -1;
    }
    let mut n = size as usize;
    if state.chunk > 0 {
        n = n.min(state.chunk);
    }
    // SAFETY: CLAP guarantees `buffer` is readable for `size` bytes, and `n <= size`.
    let src = unsafe { core::slice::from_raw_parts(buffer.cast::<u8>(), n) };
    state.data.extend_from_slice(src);
    n as i64
}

unsafe extern "C" fn stream_read(
    stream: *const ClapIStream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    // SAFETY: as in `stream_write`.
    let state = unsafe { &*(*stream).ctx.cast::<RefCell<StreamState>>() };
    let mut state = state.borrow_mut();
    if state.fail {
        return -1;
    }
    let mut n = (size as usize).min(state.data.len() - state.read_at);
    if state.chunk > 0 {
        n = n.min(state.chunk);
    }
    // SAFETY: CLAP guarantees `buffer` is writable for `size` bytes, and `n <= size`.
    unsafe {
        ptr::copy_nonoverlapping(
            state.data.as_ptr().add(state.read_at),
            buffer.cast::<u8>(),
            n,
        );
    }
    state.read_at += n;
    n as i64
}

impl TestStream {
    /// An empty stream that moves as much as it is asked to.
    #[must_use]
    pub fn new() -> Self {
        Self::with(Vec::new(), 0, false)
    }

    /// A stream primed with `data`, moving at most `chunk` bytes per call.
    #[must_use]
    pub fn with(data: Vec<u8>, chunk: usize, fail: bool) -> Self {
        let mut bytes = Box::new(RefCell::new(StreamState {
            data,
            read_at: 0,
            chunk,
            fail,
        }));
        let ctx: *mut c_void = ptr::from_mut(bytes.as_mut()).cast();
        Self {
            bytes,
            ostream: ClapOStream {
                ctx,
                write: Some(stream_write),
            },
            istream: ClapIStream {
                ctx,
                read: Some(stream_read),
            },
        }
    }

    /// The write half.
    #[must_use]
    pub fn ostream(&self) -> *const ClapOStream {
        ptr::from_ref(&self.ostream)
    }

    /// The read half.
    #[must_use]
    pub fn istream(&self) -> *const ClapIStream {
        ptr::from_ref(&self.istream)
    }

    /// The bytes written so far.
    #[must_use]
    pub fn data(&self) -> Vec<u8> {
        self.bytes.borrow().data.clone()
    }

    /// Rewinds the read cursor.
    pub fn rewind(&self) {
        self.bytes.borrow_mut().read_at = 0;
    }
}
