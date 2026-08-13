//! The plug-in instance function table (abi-v1 §7 and §8).
//!
//! One [`AxtInstance`] lives behind the [`DauxPluginHandle`] the host was given. It owns the
//! [`PluginInstance`] that enforces the lifecycle, the preallocated per-block bus views, the
//! host bridge and the editor — everything whose lifetime is the instance's.
//!
//! # Poisoning
//!
//! `PluginInstance` already refuses every call once poisoned, so §17.3 costs nothing here
//! beyond calling `poison()` from the unwind handler in [`with_instance`]. What that buys is
//! specific: after a panic the plug-in's own invariants are unknown, so the *next* call must
//! not reach its code at all — not `deactivate`, not `process`, not even `destroy`'s drop of
//! its DSP state, which is why `PluginInstance` keeps its own drop path allocation-free and
//! side-effect-free once poisoned.

use daux_abi::{
    DAUX_PROCESS_CONTINUE, DAUX_PROCESS_ERROR, DauxPluginApiV1, DauxPluginHandle, DauxPluginV1,
    DauxProcessConfigV1, DauxProcessV1, DauxStatus, DauxStrView, ext,
};
use daux_plugin_api::{
    DauxGraphic, DauxPlugin, PhysicalSize, PluginDescriptor, PluginInstance, ProcessConfig,
    ProcessContext, ProcessEvents, ProcessMode, SampleFormat, ScaleFactor, WindowApi,
};

use crate::audio::AudioScratch;
use crate::events::{AbiInputEvents, AbiOutputEvents};
use crate::host::HostBridge;
use crate::panic::{Refusal, catch_reporting, status_of};
use crate::transport;

/// Everything the editor half of an instance remembers between calls (abi-v1 §11.4).
///
/// It is deliberately a separate struct from the DSP state: rule 9 of `CLAUDE.md` says an
/// editor's lifetime is independent of the processor's, and keeping the two in one flat struct
/// is how that stops being true.
#[derive(Default)]
pub(crate) struct EditorState {
    /// The editor, from `create` until `destroy`. `None` means no editor exists.
    pub(crate) editor: Option<Box<dyn DauxGraphic>>,
    /// The windowing API the host asked for.
    pub(crate) api: Option<WindowApi>,
    /// The last scale the host reported, or 1.0.
    pub(crate) scale: Option<ScaleFactor>,
    /// The last size agreed with the host, in physical pixels.
    pub(crate) size: Option<PhysicalSize>,
    /// `true` between a successful `set_parent` and `destroy`.
    pub(crate) open: bool,
}

/// Everything behind a `DauxPluginHandle`.
pub(crate) struct AxtInstance {
    /// The lifecycle-enforcing wrapper around the plug-in.
    pub(crate) instance: PluginInstance,
    /// The descriptor the factory published for this id, when it could be found.
    pub(crate) descriptor: Option<PluginDescriptor>,
    /// The host services this instance was created with.
    pub(crate) host: HostBridge,
    /// The configuration of the current activation. Meaningless while inactive.
    pub(crate) config: ProcessConfig,
    /// A mode set through `daux.render/1`, applied at the next `activate`.
    pub(crate) render_mode: Option<ProcessMode>,
    /// The per-block bus views, sized in `activate`.
    pub(crate) audio: AudioScratch,
    /// The editor half.
    pub(crate) editor: EditorState,
}

impl AxtInstance {
    /// [audio-thread] One block (abi-v1 §8).
    ///
    /// # Safety
    ///
    /// `block` is null or points at a [`DauxProcessV1`] whose buffers, event lists and
    /// transport stay valid for the duration of this call (abi-v1 §16.3).
    unsafe fn process(&mut self, block: *const DauxProcessV1) -> i32 {
        if block.is_null() {
            return DAUX_PROCESS_ERROR;
        }
        // SAFETY: non-null was checked, and the caller guarantees the block is live for the
        // call. It is copied out rather than borrowed so that nothing here holds a reference
        // into host memory longer than one field read.
        let block = unsafe { *block };
        if !block.is_v1_0_compatible() {
            // A host that published a structure smaller than v1.0 has not filled the fields
            // this adapter is about to read (abi-v1 §3).
            return DAUX_PROCESS_ERROR;
        }
        let frames = block.frame_count as usize;
        if frames == 0 {
            // abi-v1 §8 promises `1 ..= max_block_size`. Nothing to do, and nothing wrong.
            return DAUX_PROCESS_CONTINUE;
        }
        if frames > self.config.max_block_size as usize {
            // The host is asking for a longer block than the activation promised. Every buffer
            // in this block was sized from that promise — by the plug-in, and possibly by the
            // host itself — so the frame count is refused *here*, before a single view is
            // built from it. Trusting it and letting a later layer notice would mean writing
            // past the end of the host's own buffers on the way to reporting the error.
            return DAUX_PROCESS_ERROR;
        }

        let Self {
            instance,
            host,
            config,
            audio,
            ..
        } = self;

        // The host's transport, if it published one this block.
        let host_transport = if block.transport.is_null() {
            None
        } else {
            // SAFETY: non-null was checked; abi-v1 §16.3 makes the record valid for the call.
            let abi = unsafe { &*block.transport };
            transport::is_usable(abi).then(|| transport::from_abi(abi))
        };

        // SAFETY: `in_events`/`out_events` are host-owned lists valid for this call; the
        // adapters tolerate null and never retain anything past the borrow.
        let input_events = unsafe { AbiInputEvents::new(block.in_events) };
        // SAFETY: as above.
        let mut output_events = unsafe { AbiOutputEvents::new(block.out_events) };
        let mut events = ProcessEvents::new(&input_events, &mut output_events);

        let mut ctx = ProcessContext::new(frames, config, host.rt());
        if let Some(t) = host_transport.as_ref() {
            ctx = ctx.with_transport(t);
        }
        if block.steady_time != -1 {
            ctx = ctx.with_steady_time(block.steady_time);
        }

        let status = match config.sample_format {
            SampleFormat::F64 => {
                // SAFETY: the block was validated above and its buffers are valid for the call.
                if unsafe { audio.fill_f64(&block) }.is_err() {
                    audio.clear();
                    return DAUX_PROCESS_ERROR;
                }
                let mut buses = audio.buses_f64(frames);
                instance.process_f64(&ctx, &mut buses, &mut events)
            }
            _ => {
                // SAFETY: as above.
                if unsafe { audio.fill_f32(&block) }.is_err() {
                    audio.clear();
                    return DAUX_PROCESS_ERROR;
                }
                let mut buses = audio.buses_f32(frames);
                instance.process(&ctx, &mut buses, &mut events)
            }
        };

        // No view may survive the call that built it; see `AudioScratch::buses_f32`.
        audio.clear();
        status.code()
    }

    /// [main-thread] Applies a host configuration and sizes everything the block path needs.
    fn activate(&mut self, config: *const DauxProcessConfigV1) -> DauxStatus {
        if config.is_null() {
            return DauxStatus::INVALID_ARG;
        }
        // SAFETY: non-null was checked; the caller owns the structure and it stays valid for
        // the call. It is copied out immediately.
        let abi = unsafe { *config };
        if !abi.is_v1_0_compatible() {
            return daux_abi::DAUX_ERR_ABI_MISMATCH;
        }
        let process_config = ProcessConfig::new(abi.sample_rate, abi.max_block_size)
            .with_min_block_size(abi.min_block_size)
            .with_sample_format(sample_format(abi.sample_format))
            // A mode set through `daux.render/1` while inactive wins over the one in the
            // configuration: it is the later, more specific statement of intent (abi-v1 §11.5).
            .with_process_mode(
                self.render_mode
                    .unwrap_or_else(|| ProcessMode::from_code(abi.process_mode)),
            );

        // Sized from what the plug-in declares, which is what the host is required to send.
        let layout = match self.instance.bus_layout() {
            Ok(layout) => layout,
            Err(err) => return crate::panic::status_of_error(&err),
        };
        let scratch = AudioScratch::new(layout.inputs.len(), layout.outputs.len());

        let status = status_of(self.instance.activate(&process_config));
        if status.is_ok() {
            self.config = process_config;
            self.audio = scratch;
        }
        status
    }
}

/// The sample format an activation asks for.
///
/// `DAUX_SAMPLE_FORMAT_F64` selects `f64`; anything else — including a host that sets both bits
/// or none — selects `f32`, the format abi-v1 §8 requires every plug-in to support.
fn sample_format(bits: u32) -> SampleFormat {
    if bits == daux_abi::DAUX_SAMPLE_FORMAT_F64 {
        SampleFormat::F64
    } else {
        SampleFormat::F32
    }
}

/// [main-thread] Builds the instance interface the factory hands back.
///
/// The controller is given its host services here, before `init`, which is the one point
/// abi-v1 §7 leaves for it: `create_plugin` is the only call that knows both the plug-in and
/// the host.
pub(crate) fn create(
    plugin: Box<dyn DauxPlugin>,
    descriptor: Option<PluginDescriptor>,
    host: HostBridge,
) -> DauxPluginV1 {
    let mut instance = match descriptor.clone() {
        Some(descriptor) => PluginInstance::with_descriptor(plugin, descriptor),
        None => PluginInstance::new(plugin),
    };
    // In `Created`, so this cannot fail for a state reason; a plug-in that refuses its host
    // services is not a reason to fail creation.
    let _ = instance.set_host(host.services().clone());
    let state = Box::new(AxtInstance {
        instance,
        descriptor,
        host,
        config: ProcessConfig::default(),
        render_mode: None,
        audio: AudioScratch::new(0, 0),
        editor: EditorState::default(),
    });
    DauxPluginV1::new(
        DauxPluginHandle::from_ptr(Box::into_raw(state).cast()),
        &raw const PLUGIN_API,
    )
}

/// [any-thread] Runs `body` with the instance behind `p`, honouring abi-v1 §17.
///
/// Shared by this module and every extension table, which is why it is `pub(crate)`.
///
/// # Safety
///
/// `p` is null or a [`DauxPluginHandle`] this module produced in [`create`] and has not yet
/// destroyed, and no other call on the same instance is in progress (abi-v1 §15).
pub(crate) unsafe fn with_instance<R: Refusal>(
    p: DauxPluginHandle,
    body: impl FnOnce(&mut AxtInstance) -> R,
) -> R {
    if p.is_null() {
        return R::INVALID_ARG;
    }
    let state = p.as_ptr().cast::<AxtInstance>();
    // SAFETY: the caller guarantees the handle is one of ours, still live, and not concurrently
    // in use, so producing one `&mut` from it is exclusive. The reference is confined to
    // `body` — the poison path below re-derives its own from the raw pointer afterwards.
    if unsafe { (*state).instance.is_poisoned() } {
        return R::POISONED;
    }
    // SAFETY: as above.
    match catch_reporting(|| body(unsafe { &mut *state })) {
        Ok(value) => value,
        Err(()) => {
            // SAFETY: the previous `&mut` died with the closure, so this one is again unique.
            // Poisoning touches nothing but a flag, which is what makes it safe to do on a
            // stack that has just unwound out of plug-in code (§17.3).
            unsafe { (*state).instance.poison() };
            R::PANICKED
        }
    }
}

/// [main-thread] Late initialisation (abi-v1 §7).
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn init(p: DauxPluginHandle) -> DauxStatus {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, |state| status_of(state.instance.init())) }
}

/// [main-thread] Destroys the instance (abi-v1 §7).
///
/// # Safety
///
/// `p` is null or a handle [`create`] produced, passed back exactly once, with no other call on
/// the same instance in progress.
unsafe extern "C" fn destroy(p: DauxPluginHandle) {
    if p.is_null() {
        return;
    }
    // A `Drop` in the plug-in that panics must not escape either (§17.1).
    let _ = catch_reporting(|| {
        // SAFETY: the handle was produced by `Box::into_raw` in `create`, ownership passed to
        // the host, and the host is returning it exactly once.
        drop(unsafe { Box::from_raw(p.as_ptr().cast::<AxtInstance>()) });
    });
}

/// [main-thread] Allocates DSP resources (abi-v1 §7).
///
/// # Safety
///
/// `config` is null or points at a readable [`DauxProcessConfigV1`]. See [`with_instance`].
unsafe extern "C" fn activate(
    p: DauxPluginHandle,
    config: *const DauxProcessConfigV1,
) -> DauxStatus {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, |state| state.activate(config)) }
}

/// [main-thread] Releases the activation (abi-v1 §7).
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn deactivate(p: DauxPluginHandle) {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            // A host that deactivates while processing is refused by `PluginInstance`; there is
            // no status to report it through, so the refusal is simply that nothing happens.
            let _ = state.instance.deactivate();
        });
    }
}

/// [audio-thread] Arms the processor (abi-v1 §7).
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn start_processing(p: DauxPluginHandle) -> DauxStatus {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, |state| status_of(state.instance.start_processing())) }
}

/// [audio-thread] Disarms the processor (abi-v1 §7).
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn stop_processing(p: DauxPluginHandle) {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            let _ = state.instance.stop_processing();
        });
    }
}

/// [audio-thread, only while not processing] Clears state that depends on past audio.
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn reset(p: DauxPluginHandle) {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            let _ = state.instance.reset();
        });
    }
}

/// [audio-thread] One block (abi-v1 §8).
///
/// # Safety
///
/// `block` is null or a [`DauxProcessV1`] valid for this call. See [`with_instance`].
unsafe extern "C" fn process(p: DauxPluginHandle, block: *const DauxProcessV1) -> i32 {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe { with_instance(p, |state| state.process(block)) }
}

/// [any-thread] Extension lookup (abi-v1 §11).
///
/// Unknown ids return null, as the specification requires. `daux.gui/1` is offered only when
/// the descriptor advertises `DAUX_CAP_HAS_GUI`, so a null there means "headless" rather than
/// "unsupported by this SDK".
///
/// # Safety
///
/// `id` points at `id.len` readable bytes for the duration of the call. See [`with_instance`].
unsafe extern "C" fn get_extension(
    p: DauxPluginHandle,
    id: DauxStrView,
) -> *const core::ffi::c_void {
    let refuse = core::ptr::null();
    if p.is_null() {
        return refuse;
    }
    // SAFETY: the caller guarantees the view is readable for this call.
    let Some(id) = (unsafe { id.as_str() }) else {
        return refuse;
    };
    let table: *const core::ffi::c_void = match id {
        ext::AUDIO_PORTS => (&raw const crate::ext::audio_ports::TABLE).cast(),
        ext::PARAMS => (&raw const crate::ext::params::TABLE).cast(),
        ext::STATE => (&raw const crate::ext::state::TABLE).cast(),
        ext::LATENCY => (&raw const crate::ext::render::LATENCY_TABLE).cast(),
        ext::TAIL => (&raw const crate::ext::render::TAIL_TABLE).cast(),
        ext::RENDER => (&raw const crate::ext::render::RENDER_TABLE).cast(),
        ext::GUI => {
            // SAFETY: the caller guarantees the handle is live and not concurrently in use;
            // only the cached descriptor is read.
            let has_gui = unsafe {
                (*p.as_ptr().cast::<AxtInstance>())
                    .descriptor
                    .as_ref()
                    .is_some_and(|d| d.capabilities.is_has_gui())
            };
            if has_gui {
                (&raw const crate::ext::gui::TABLE).cast()
            } else {
                refuse
            }
        }
        _ => refuse,
    };
    table
}

/// [main-thread] Drains work the audio thread asked for (abi-v1 §7).
///
/// # Safety
///
/// See [`with_instance`].
unsafe extern "C" fn on_main_thread(p: DauxPluginHandle) {
    // SAFETY: forwarded verbatim from this function's own contract.
    unsafe {
        with_instance(p, |state| {
            let _ = state.instance.on_main_thread();
        });
    }
}

/// The instance table every `.axt` this crate builds hands out.
///
/// A `static`: one table shared by every instance, which is what keeps a thousand instances
/// costing a thousand handles and no more (abi-v1 §2.3).
pub(crate) static PLUGIN_API: DauxPluginApiV1 = DauxPluginApiV1 {
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
