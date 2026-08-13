//! One CLAP plug-in instance, and every extension table it publishes.
//!
//! # Shape
//!
//! A [`ClapInstance`] owns the `clap_plugin` struct the host holds, a [`PluginInstance`]
//! driving the abi-v1 §7 lifecycle, an [`InstanceLock`] serialising the two threads CLAP
//! lets in at once, and the editor. `plugin_data` points back at the `ClapInstance`, which
//! is how every exported function finds its way home.
//!
//! # The three rules every exported function obeys
//!
//! 1. **`catch_unwind` around the whole body.** A panic never crosses the boundary
//!    (abi-v1 §17).
//! 2. **A panic poisons the instance.** Afterwards every call refuses rather than
//!    re-entering plug-in code whose invariants have already broken once. `destroy` still
//!    works, so a host can clean up.
//! 3. **Exclusive access is taken, or the call refuses.** `process` uses a non-blocking
//!    try-lock; main-thread calls wait briefly. See [`crate::lock`].
//!
//! # In-place processing
//!
//! Every audio port advertises `in_place_pair = CLAP_INVALID_ID`, so a host must give
//! distinct input and output buffers. That is not a limitation of the DSP model but of
//! soundness: [`AudioBufferMut::from_raw`](daux_plugin_api::AudioBufferMut::from_raw)
//! requires that no other reference to the samples exists, and an in-place pair is exactly
//! a live `&` and `&mut` to one buffer.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_void};
use core::panic::AssertUnwindSafe;
use core::ptr;
use std::panic::catch_unwind;
use std::sync::Arc;

use daux_plugin_api::{
    AudioBufferMut, AudioBufferRef, AudioBuses, BusLayout, DauxEvent, DauxGraphic, EventPortLayout,
    GraphicContext, GraphicFramework, GraphicProfile, GraphicRenderer, HostGraphicCaps,
    HostServices, InputEvents, LogicalSize, ParamId, PhysicalSize, PluginInstance,
    PresentationMode, ProcessConfig, ProcessContext, ProcessEvents, ProcessMode, ProcessStatus,
    RtHostServices, Sample, SampleFormat, ScaleFactor, StateReader, StateVersion, StateWriter,
    Tail, WindowTarget,
};

use crate::abi::{
    CLAP_EXT_AUDIO_PORTS, CLAP_EXT_GUI, CLAP_EXT_LATENCY, CLAP_EXT_NOTE_PORTS, CLAP_EXT_PARAMS,
    CLAP_EXT_RENDER, CLAP_EXT_STATE, CLAP_EXT_TAIL, CLAP_INVALID_ID, CLAP_NAME_SIZE,
    CLAP_NOTE_DIALECT_CLAP, CLAP_NOTE_DIALECT_MIDI, CLAP_NOTE_DIALECT_MIDI2, CLAP_PORT_AMBISONIC,
    CLAP_PORT_MONO, CLAP_PORT_STEREO, CLAP_PORT_SURROUND, CLAP_PROCESS_ERROR, CLAP_RENDER_OFFLINE,
    CLAP_RENDER_REALTIME, CLAP_WINDOW_API_COCOA, CLAP_WINDOW_API_WIN32, CLAP_WINDOW_API_X11,
    ClapAudioBuffer, ClapAudioPortInfo, ClapGuiResizeHints, ClapHost, ClapIStream, ClapInputEvents,
    ClapNotePortInfo, ClapOStream, ClapOutputEvents, ClapParamInfo, ClapPlugin,
    ClapPluginAudioPorts, ClapPluginGui, ClapPluginLatency, ClapPluginNotePorts, ClapPluginParams,
    ClapPluginRender, ClapPluginState, ClapPluginTail, ClapProcess, ClapWindow,
};
use crate::descriptor::OwnedDescriptor;
use crate::events::{ClapInputList, ClapOutputList};
use crate::host::ClapHostBridge;
use crate::lock::InstanceLock;
use crate::params::fill_param_info;
use crate::text::{borrow_str, write_capped, write_fixed};
use crate::transport::transport_from_clap;

/// How many audio buses in each direction one block may carry.
///
/// A stack array rather than a heap one, because `process` may not allocate. Thirty-two is
/// far past anything a real plug-in declares (a big surround mixer strip has under ten);
/// buses beyond it are ignored, and the plug-in sees them as absent rather than as garbage.
const MAX_BUSES: usize = 32;

/// The window API this build can embed an editor into.
const PLATFORM_WINDOW_API: &CStr = if cfg!(target_os = "windows") {
    CLAP_WINDOW_API_WIN32
} else if cfg!(target_os = "macos") {
    CLAP_WINDOW_API_COCOA
} else {
    CLAP_WINDOW_API_X11
};

/// Everything one instance owns, behind the lock.
struct Inner {
    /// The lifecycle state machine and the plug-in itself.
    instance: PluginInstance,
    /// The configuration the current activation was prepared with.
    config: ProcessConfig,
    /// The real-time-safe host handle handed to every `process` call.
    rt: RtHostServices,
    /// The full host services, kept for the editor.
    services: HostServices,
    /// Audio bus topology, read once at `init` so `process` never asks again.
    buses: BusLayout,
    /// Event port topology, read once at `init`.
    event_ports: EventPortLayout,
    /// Parameter ids in `Params::param_refs` order, which is the index order CLAP uses.
    param_ids: Vec<ParamId>,
    /// The mode the host asked for through `clap.render`, applied at the next activation.
    render_mode: ProcessMode,
    /// The editor, between `gui.create` and `gui.destroy`.
    editor: Option<Box<dyn DauxGraphic>>,
    /// `true` between a successful `set_parent` and `gui.destroy`.
    editor_open: bool,
    /// The editor's size in physical pixels.
    editor_size: PhysicalSize,
    /// The display scale the host last reported.
    editor_scale: ScaleFactor,
}

/// One CLAP plug-in instance.
///
/// # Threads
///
/// The struct is deliberately neither `Send` nor `Sync`, and nothing in this crate requires
/// it to be: the host holds a raw `clap_plugin *`, not a Rust reference, so the markers
/// never come up. What makes concurrent access sound is [`InstanceLock`], which gives the
/// audio thread and the main thread genuine mutual exclusion with acquire/release ordering.
///
/// The one piece of state that is *thread-affine* rather than merely shared is the editor:
/// a `Box<dyn DauxGraphic>` is typically `Rc`-based, so its reference counts must never be
/// touched from two threads. It is reached only from `clap_plugin_gui` methods and from
/// `destroy`, all of which CLAP marks `[main-thread]`.
pub struct ClapInstance {
    /// The table the host holds. Its `plugin_data` points back at this struct.
    plugin: ClapPlugin,
    /// Serialises the audio thread against the main thread.
    lock: InstanceLock,
    /// Everything the lock guards.
    inner: UnsafeCell<Inner>,
    /// Keeps the descriptor the host is still reading alive for this instance's lifetime.
    descriptor: &'static OwnedDescriptor,
    /// The host bridge, kept alive for the services and the editor.
    #[allow(dead_code)]
    host: Arc<ClapHostBridge>,
}

impl ClapInstance {
    /// `[main-thread]` Builds an instance and hands the host its `clap_plugin`.
    ///
    /// Returns null when the host pointer is unusable or the plug-in refuses to accept the
    /// host services, which are the two failures a host can cause here.
    ///
    /// # Safety
    ///
    /// `host` must be null, or a `clap_host` that outlives the returned instance. The
    /// returned pointer is owned by the caller and must be released through
    /// `clap_plugin::destroy`, never freed directly.
    #[must_use]
    pub unsafe fn create(
        plugin: Box<dyn daux_plugin_api::DauxPlugin>,
        descriptor: &'static OwnedDescriptor,
        host: *const ClapHost,
    ) -> *const ClapPlugin {
        // SAFETY: the caller guarantees `host` is null or a live `clap_host` outliving the
        // instance, which is exactly `ClapHostBridge::new`'s contract.
        let Some(bridge) = (unsafe { ClapHostBridge::new(host) }) else {
            return ptr::null();
        };
        let bridge = Arc::new(bridge);
        let services = bridge.services();
        let rt = services.rt().clone();

        let mut instance = PluginInstance::with_descriptor(plugin, descriptor.daux().clone());
        if instance.set_host(services.clone()).is_err() {
            return ptr::null();
        }

        let inner = Inner {
            instance,
            config: ProcessConfig::default(),
            rt,
            services,
            buses: BusLayout::new(),
            event_ports: EventPortLayout::none(),
            param_ids: Vec::new(),
            render_mode: ProcessMode::Realtime,
            editor: None,
            editor_open: false,
            editor_size: PhysicalSize::new(0, 0),
            editor_scale: ScaleFactor::ONE,
        };

        let mut boxed = Box::new(Self {
            plugin: ClapPlugin {
                desc: descriptor.view(),
                plugin_data: ptr::null_mut(),
                init: plugin_init,
                destroy: plugin_destroy,
                activate: plugin_activate,
                deactivate: plugin_deactivate,
                start_processing: plugin_start_processing,
                stop_processing: plugin_stop_processing,
                reset: plugin_reset,
                process: plugin_process,
                get_extension: plugin_get_extension,
                on_main_thread: plugin_on_main_thread,
            },
            lock: InstanceLock::new(),
            inner: UnsafeCell::new(inner),
            descriptor,
            host: bridge,
        });
        boxed.plugin.plugin_data = ptr::from_mut(boxed.as_mut()).cast();
        let raw = Box::into_raw(boxed);
        // SAFETY: `raw` came from `Box::into_raw` a line ago, so it is a live, aligned,
        // uniquely-owned `ClapInstance`. Handing out a pointer to its `plugin` field is
        // exactly the ownership transfer `create_plugin` documents; `destroy` takes it back.
        unsafe { ptr::from_ref(&(*raw).plugin) }
    }

    /// `[main-thread]` Runs `f` with exclusive access, waiting briefly for the audio thread.
    ///
    /// Returns `fallback` when the instance is poisoned, when the lock could not be taken,
    /// or when `f` panicked — and a panic poisons the instance on the way out.
    fn with_main<R>(&self, fallback: impl FnOnce() -> R, f: impl FnOnce(&mut Inner) -> R) -> R {
        let Some(_guard) = self.lock.lock_main() else {
            return fallback();
        };
        self.run_locked(fallback, f)
    }

    /// `[audio-thread]` Runs `f` with exclusive access, or gives up at once.
    ///
    /// Never blocks and never allocates on the path that succeeds, which is what makes it
    /// legal from `process`.
    fn with_audio<R>(&self, fallback: impl FnOnce() -> R, f: impl FnOnce(&mut Inner) -> R) -> R {
        let Some(_guard) = self.lock.try_lock() else {
            return fallback();
        };
        self.run_locked(fallback, f)
    }

    /// Body shared by [`with_main`](Self::with_main) and [`with_audio`](Self::with_audio);
    /// the caller has already taken the lock.
    fn run_locked<R>(&self, fallback: impl FnOnce() -> R, f: impl FnOnce(&mut Inner) -> R) -> R {
        // SAFETY: the caller holds the instance lock, which is the only way to reach this
        // function, so no other thread holds a reference into `inner` and this `&mut` is
        // unique for as long as the guard lives.
        let inner = unsafe { &mut *self.inner.get() };
        if inner.instance.is_poisoned() {
            return fallback();
        }
        // `AssertUnwindSafe` is the right claim rather than a shortcut: a panic here does
        // leave `inner` in an unknown state, and the answer is to poison the instance so
        // nothing observes it again (abi-v1 §17.3).
        match catch_unwind(AssertUnwindSafe(|| f(inner))) {
            Ok(value) => value,
            Err(_) => {
                // SAFETY: the guard is still held, so re-deriving the reference is still
                // exclusive. `poison` only writes one state field and cannot itself panic.
                let inner = unsafe { &mut *self.inner.get() };
                inner.instance.poison();
                fallback()
            }
        }
    }

    /// `[any-thread]` `true` once a panic has poisoned this instance.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        let Some(_guard) = self.lock.try_lock() else {
            // A live call is in progress, so nothing has poisoned it *and* returned yet.
            return false;
        };
        // SAFETY: the guard proves exclusive access for the rest of this function.
        unsafe { &*self.inner.get() }.instance.is_poisoned()
    }
}

/// Recovers the instance behind a `clap_plugin`.
///
/// # Safety
///
/// `plugin` must be null, or a pointer the adapter handed out from
/// [`ClapInstance::create`] and that has not yet been destroyed.
unsafe fn instance_of<'a>(plugin: *const ClapPlugin) -> Option<&'a ClapInstance> {
    // SAFETY: the caller guarantees `plugin` is null or one of our own live tables, whose
    // `plugin_data` was set to the owning `ClapInstance` in `create` and is never changed.
    unsafe {
        let table = plugin.as_ref()?;
        table.plugin_data.cast::<ClapInstance>().as_ref()
    }
}

// ---------------------------------------------------------------------------------------
// clap_plugin
// ---------------------------------------------------------------------------------------

unsafe extern "C" fn plugin_init(plugin: *const ClapPlugin) -> bool {
    // SAFETY: CLAP passes back the table it was given, which `instance_of` tolerates being
    // null.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    this.with_main(
        || false,
        |inner| {
            if inner.instance.init().is_err() {
                return false;
            }
            // Topology and the parameter index order are fixed for the instance's life, so they
            // are read once here rather than on every host query — which also means `process`
            // never needs the main-thread half of the plug-in.
            inner.buses = inner.instance.bus_layout().unwrap_or_default();
            inner.event_ports = inner.instance.event_ports().unwrap_or_default();
            inner.param_ids = match inner.instance.params() {
                Ok(params) => params.param_refs().into_iter().map(|(id, _)| id).collect(),
                Err(_) => Vec::new(),
            };
            true
        },
    )
}

unsafe extern "C" fn plugin_destroy(plugin: *const ClapPlugin) {
    if plugin.is_null() {
        return;
    }
    // The owning pointer is taken straight out of `plugin_data` rather than rebuilt from a
    // `&ClapInstance`: it has to carry the unique provenance `Box::into_raw` gave it in
    // `create`, and a pointer laundered through a shared reference would not.
    // SAFETY: CLAP passes back a table this adapter handed out, whose `plugin_data` is the
    // `Box::into_raw` pointer from `create` and is never changed afterwards.
    let raw = unsafe { (*plugin).plugin_data.cast::<ClapInstance>() };
    if raw.is_null() {
        return;
    }
    {
        // SAFETY: `raw` is a live `ClapInstance`; this shared borrow ends before the box is
        // reconstituted below.
        let this = unsafe { &*raw };
        // Close the editor first. CLAP allows a host to destroy the plug-in without calling
        // `gui.destroy`, and an editor dropped with its window still attached is a classic
        // way to take a DAW down. This runs even for a poisoned instance, because the editor
        // is the adapter's object and not the plug-in's DSP state.
        // Bound to a name on purpose: a guard left as a temporary in an `if` condition is
        // dropped at the end of that condition, so the lock would be released before the
        // teardown below ever ran.
        let guard = this.lock.lock_main();
        if guard.is_some() {
            // SAFETY: `guard` proves exclusive access for the rest of this block.
            let inner = unsafe { &mut *this.inner.get() };
            let _ = catch_unwind(AssertUnwindSafe(|| {
                if let Some(editor) = inner.editor.as_mut() {
                    editor.close();
                }
                inner.editor = None;
                inner.editor_open = false;
            }));
        }
        drop(guard);
    }
    // SAFETY: `raw` is the pointer `Box::into_raw` produced in `create`, still uniquely
    // owned; CLAP guarantees `destroy` is called once and that no other call is in flight.
    // Reconstituting the box and dropping it releases the instance exactly once.
    drop(unsafe { Box::from_raw(raw) });
}

unsafe extern "C" fn plugin_activate(
    plugin: *const ClapPlugin,
    sample_rate: f64,
    min_frames_count: u32,
    max_frames_count: u32,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    this.with_main(
        || false,
        |inner| {
            // A host that offers no upper bound would leave every scratch buffer unsized, so it
            // is refused rather than guessed at.
            if max_frames_count == 0 {
                return false;
            }
            // The sample format stays `F32`, the one every DAUx plug-in must support: CLAP
            // chooses 32- or 64-bit per port per block, so there is no single answer to put
            // here, and `process` dispatches on what the host actually sent.
            let config = ProcessConfig::new(sample_rate, max_frames_count)
                .with_min_block_size(min_frames_count.min(max_frames_count))
                .with_sample_format(SampleFormat::F32)
                .with_process_mode(inner.render_mode);
            if inner.instance.activate(&config).is_err() {
                return false;
            }
            inner.config = config;
            true
        },
    )
}

unsafe extern "C" fn plugin_deactivate(plugin: *const ClapPlugin) {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return;
    };
    this.with_main(
        || (),
        |inner| {
            // A host that forgot `stop_processing` would otherwise leave the instance stuck in
            // `Processing` and every later `activate` refused.
            let _ = inner.instance.stop_processing();
            let _ = inner.instance.deactivate();
        },
    );
}

unsafe extern "C" fn plugin_start_processing(plugin: *const ClapPlugin) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    this.with_audio(|| false, |inner| inner.instance.start_processing().is_ok())
}

unsafe extern "C" fn plugin_stop_processing(plugin: *const ClapPlugin) {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return;
    };
    this.with_audio(
        || (),
        |inner| {
            let _ = inner.instance.stop_processing();
        },
    );
}

unsafe extern "C" fn plugin_reset(plugin: *const ClapPlugin) {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return;
    };
    this.with_audio(
        || (),
        |inner| {
            // CLAP allows `reset` while the plug-in is processing; abi-v1 §7 does not. The
            // faithful bridge is the sandwich below: `stop_processing` and `start_processing`
            // are both audio-thread-safe and allocation-free, so a locate mid-playback still
            // clears delay lines instead of being silently refused.
            if inner.instance.state().is_processing() {
                let _ = inner.instance.stop_processing();
                let _ = inner.instance.reset();
                let _ = inner.instance.start_processing();
            } else {
                let _ = inner.instance.reset();
            }
        },
    );
}

unsafe extern "C" fn plugin_on_main_thread(plugin: *const ClapPlugin) {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return;
    };
    this.with_main(
        || (),
        |inner| {
            let _ = inner.instance.on_main_thread();
        },
    );
}

unsafe extern "C" fn plugin_get_extension(
    plugin: *const ClapPlugin,
    id: *const c_char,
) -> *const c_void {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return ptr::null();
    };
    if id.is_null() {
        return ptr::null();
    }
    // SAFETY: CLAP passes a NUL-terminated extension id that is valid for the call.
    let id = unsafe { CStr::from_ptr(id) };

    if id == CLAP_EXT_AUDIO_PORTS {
        ptr::from_ref(&AUDIO_PORTS).cast()
    } else if id == CLAP_EXT_NOTE_PORTS {
        ptr::from_ref(&NOTE_PORTS).cast()
    } else if id == CLAP_EXT_PARAMS {
        ptr::from_ref(&PARAMS).cast()
    } else if id == CLAP_EXT_STATE {
        ptr::from_ref(&STATE).cast()
    } else if id == CLAP_EXT_LATENCY {
        ptr::from_ref(&LATENCY).cast()
    } else if id == CLAP_EXT_TAIL {
        ptr::from_ref(&TAIL).cast()
    } else if id == CLAP_EXT_RENDER {
        ptr::from_ref(&RENDER).cast()
    } else if id == CLAP_EXT_GUI && this.descriptor.daux().capabilities.is_has_gui() {
        // A plug-in that does not advertise an editor must not publish `clap.gui`: a host
        // that sees the extension shows an "open editor" button and then an empty window.
        ptr::from_ref(&GUI).cast()
    } else {
        ptr::null()
    }
}

// ---------------------------------------------------------------------------------------
// process
// ---------------------------------------------------------------------------------------

/// The two sample types CLAP can hand over, and how to reach each one's buffers.
///
/// A private trait rather than two copies of `process_block`: the buffer assembly, the
/// clamping and the event wiring are identical, and only the pointer field and the
/// `PluginInstance` entry point differ.
trait ClapSample: Sample + Sized {
    /// The channel-pointer array for this sample type, or null when the bus is not in it.
    fn channel_ptrs(buffer: &ClapAudioBuffer) -> *mut *mut Self;

    /// Calls the `PluginInstance` entry point for this sample type.
    fn run<'a>(
        instance: &mut PluginInstance,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, Self>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus;
}

impl ClapSample for f32 {
    fn channel_ptrs(buffer: &ClapAudioBuffer) -> *mut *mut f32 {
        buffer.data32
    }

    fn run<'a>(
        instance: &mut PluginInstance,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        instance.process(ctx, audio, events)
    }
}

impl ClapSample for f64 {
    fn channel_ptrs(buffer: &ClapAudioBuffer) -> *mut *mut f64 {
        buffer.data64
    }

    fn run<'a>(
        instance: &mut PluginInstance,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f64>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        instance.process_f64(ctx, audio, events)
    }
}

/// `[audio-thread]` Whether the host filled the 64-bit pointers for this block.
///
/// CLAP lets a host choose per port; this adapter answers per block, from the bus the
/// plug-in is most likely to care about, and only when the descriptor advertised 64-bit
/// support in the first place.
///
/// # Safety
///
/// `process` must be a live `clap_process` whose bus arrays hold the counts it declares.
unsafe fn wants_f64(process: &ClapProcess, supports_f64: bool) -> bool {
    if !supports_f64 {
        return false;
    }
    // SAFETY: the caller guarantees the arrays are as long as the counts say, so reading
    // the first element of a non-empty one is in bounds.
    let probe = unsafe {
        if process.audio_outputs_count > 0 && !process.audio_outputs.is_null() {
            Some(&*process.audio_outputs)
        } else if process.audio_inputs_count > 0 && !process.audio_inputs.is_null() {
            Some(&*process.audio_inputs)
        } else {
            None
        }
    };
    probe.is_some_and(|b| b.data32.is_null() && !b.data64.is_null())
}

/// `[audio-thread]` Writes silence into every output bus, whatever sample size it uses.
///
/// The answer to every refusal: a host that gets `CLAP_PROCESS_ERROR` is entitled to
/// whatever is in the buffers, and "whatever is in the buffers" is how a dropped block
/// turns into a full-scale click.
///
/// # Safety
///
/// `process` must be a live `clap_process` whose output array holds `audio_outputs_count`
/// buffers, each with `channel_count` writable channels of `frames_count` samples.
unsafe fn silence_outputs(process: &ClapProcess) {
    if process.audio_outputs.is_null() {
        return;
    }
    let frames = process.frames_count as usize;
    for bus in 0..process.audio_outputs_count as usize {
        // SAFETY: the caller guarantees the output array is `audio_outputs_count` long, so
        // every index below it is in bounds.
        let buffer = unsafe { &*process.audio_outputs.add(bus) };
        for channel in 0..buffer.channel_count as usize {
            // SAFETY: the caller guarantees each bus has `channel_count` channel pointers,
            // each writable for `frames` samples. A null array or a null channel is skipped
            // rather than written through.
            unsafe {
                if !buffer.data32.is_null() {
                    let p = *buffer.data32.add(channel);
                    if !p.is_null() {
                        ptr::write_bytes(p, 0, frames);
                    }
                }
                if !buffer.data64.is_null() {
                    let p = *buffer.data64.add(channel);
                    if !p.is_null() {
                        ptr::write_bytes(p, 0, frames);
                    }
                }
            }
        }
    }
}

/// `[audio-thread]` Runs one block for one sample type.
///
/// # Safety
///
/// `process` must be a live `clap_process` describing buffers that stay valid for the whole
/// call, with distinct input and output storage — which is what advertising
/// `in_place_pair = CLAP_INVALID_ID` obliges the host to provide.
unsafe fn process_block<T: ClapSample>(inner: &mut Inner, process: &ClapProcess) -> ProcessStatus {
    let frames = process.frames_count as usize;
    let input_count = (process.audio_inputs_count as usize).min(MAX_BUSES);
    let output_count = (process.audio_outputs_count as usize).min(MAX_BUSES);

    let mut inputs = [AudioBufferRef::<T>::empty(); MAX_BUSES];
    let mut outputs: [AudioBufferMut<'_, T>; MAX_BUSES] =
        core::array::from_fn(|_| AudioBufferMut::empty());

    for (bus, slot) in inputs.iter_mut().enumerate().take(input_count) {
        // SAFETY: the caller guarantees the input array is at least
        // `audio_inputs_count` long, and `input_count` is clamped to it.
        let buffer = unsafe { &*process.audio_inputs.add(bus) };
        let ptrs = T::channel_ptrs(buffer);
        if ptrs.is_null() {
            continue;
        }
        // SAFETY: CLAP guarantees `ptrs` addresses `channel_count` pointers, each to
        // `frames_count` readable samples that stay valid for this call (CLAP
        // `clap_audio_buffer`, abi-v1 §16.3). Nothing writes to them while this shared view
        // exists, because the outputs are separate storage — `in_place_pair` is
        // `CLAP_INVALID_ID` on every port this adapter publishes.
        *slot = unsafe {
            AudioBufferRef::from_raw_with_mask(
                ptrs.cast_const().cast::<*const T>(),
                buffer.channel_count as usize,
                frames,
                buffer.constant_mask,
            )
        };
    }

    for (bus, slot) in outputs.iter_mut().enumerate().take(output_count) {
        // SAFETY: as for the inputs, against `audio_outputs_count`.
        let buffer = unsafe { &*process.audio_outputs.add(bus) };
        let ptrs = T::channel_ptrs(buffer);
        if ptrs.is_null() {
            continue;
        }
        // SAFETY: CLAP guarantees `ptrs` addresses `channel_count` pointers to
        // `frames_count` writable samples valid for this call, and that the channels of one
        // bus do not overlap. They do not overlap the inputs either, because every port
        // advertises `in_place_pair = CLAP_INVALID_ID`, which forbids the host from
        // aliasing them.
        *slot = unsafe {
            AudioBufferMut::from_raw(ptrs.cast_const(), buffer.channel_count as usize, frames)
        };
    }

    // Everything the borrow of `buses` will outlive has to be declared before it, because
    // `AudioBuses<'a>`, `ProcessEvents<'a>` and `ProcessContext<'a>` share one lifetime.
    let config = inner.config;
    let transport = if process.transport.is_null() {
        None
    } else {
        // SAFETY: a non-null `transport` points at a live `clap_event_transport` for this
        // call.
        Some(transport_from_clap(
            unsafe { &*process.transport },
            config.sample_rate,
        ))
    };
    // SAFETY: `process.in_events` is null or a live list the host owns for the duration of
    // this call, which is the lifetime the view takes.
    let input_events = unsafe { ClapInputList::new(process.in_events, config.sample_rate) };
    // SAFETY: `process.out_events` is null or a live sink the host owns for the duration of
    // this call, which is the lifetime the view takes.
    let mut output_events = unsafe { ClapOutputList::new(process.out_events) };

    let mut buses = AudioBuses::new(&inputs[..input_count], &mut outputs[..output_count], frames);
    let mut events = ProcessEvents::new(&input_events, &mut output_events);

    let mut ctx = ProcessContext::new(frames, &config, &inner.rt);
    if let Some(t) = transport.as_ref() {
        ctx = ctx.with_transport(t);
    }
    // CLAP uses a negative counter for "the host has no steady clock", and DAUx says the
    // same thing with `None` — never with a fabricated zero.
    if process.steady_time >= 0 {
        ctx = ctx.with_steady_time(process.steady_time);
    }
    let status = T::run(&mut inner.instance, &ctx, &mut buses, &mut events);

    // Hand back whatever the plug-in decided about constancy, reading it through `buses` so
    // a mask set with `output_slot_mut` survives. The views start at "no information", so a
    // plug-in that never touches the mask cannot accidentally tell the host that a live
    // channel is constant.
    for (bus, slot) in buses.outputs().iter().enumerate() {
        // SAFETY: `bus` is below `output_count`, which is clamped to
        // `audio_outputs_count`, and CLAP hands `clap_audio_buffer *` non-const precisely so
        // the plug-in can update this field.
        unsafe {
            (*process.audio_outputs.add(bus)).constant_mask = slot.constant_mask();
        }
    }
    status
}

unsafe extern "C" fn plugin_process(plugin: *const ClapPlugin, process: *const ClapProcess) -> i32 {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return CLAP_PROCESS_ERROR;
    };
    if process.is_null() {
        return CLAP_PROCESS_ERROR;
    }
    // SAFETY: a non-null `process` points at a live `clap_process` for this call.
    let process = unsafe { &*process };

    let status = this.with_audio(
        // Reached when the lock is held by the other thread or the instance is poisoned.
        || CLAP_PROCESS_ERROR,
        |inner| {
            // A block longer than the activation promised would overrun buffers the plug-in
            // sized in `prepare`. `PluginInstance` refuses it too, but refusing here keeps
            // the buffer assembly below from ever seeing an out-of-range frame count.
            if process.frames_count as usize > inner.config.max_block_size as usize {
                return CLAP_PROCESS_ERROR;
            }
            let supports_f64 = inner
                .instance
                .descriptor()
                .is_some_and(|d| d.supports(SampleFormat::F64));
            // SAFETY: `process` is a live `clap_process` whose buffers stay valid for the
            // call; `wants_f64` only reads the first bus of a non-empty array.
            let status = if unsafe { wants_f64(process, supports_f64) } {
                // SAFETY: as documented on `process_block`.
                unsafe { process_block::<f64>(inner, process) }
            } else {
                // SAFETY: as documented on `process_block`.
                unsafe { process_block::<f32>(inner, process) }
            };
            status.code()
        },
    );
    if status == CLAP_PROCESS_ERROR {
        // Every failure path ends here: a lost lock, a poisoned instance, a caught panic, an
        // over-long block, or a plug-in that reported an error itself. CLAP says the outputs
        // are undefined after an error, and "undefined" in a DAW means whatever was in the
        // buffer last — which is how a dropped block turns into a full-scale click.
        // SAFETY: `process` describes the host's own output buffers, valid for this call.
        unsafe { silence_outputs(process) };
    }
    status
}

// ---------------------------------------------------------------------------------------
// clap.audio-ports
// ---------------------------------------------------------------------------------------

/// The `clap_audio_port_info::port_type` hint for a channel count.
///
/// CLAP's hints are coarse — mono, stereo, surround, ambisonic — so anything that is not
/// one or two channels is described as surround, which is what hosts expect for a bus they
/// should not fold.
fn port_type(channels: u32, ambisonic: bool) -> *const c_char {
    if ambisonic {
        CLAP_PORT_AMBISONIC.as_ptr()
    } else {
        match channels {
            1 => CLAP_PORT_MONO.as_ptr(),
            2 => CLAP_PORT_STEREO.as_ptr(),
            _ => CLAP_PORT_SURROUND.as_ptr(),
        }
    }
}

unsafe extern "C" fn audio_ports_count(plugin: *const ClapPlugin, is_input: bool) -> u32 {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return 0;
    };
    this.with_main(
        || 0,
        |inner| {
            let buses = if is_input {
                &inner.buses.inputs
            } else {
                &inner.buses.outputs
            };
            u32::try_from(buses.len()).unwrap_or(u32::MAX)
        },
    )
}

unsafe extern "C" fn audio_ports_get(
    plugin: *const ClapPlugin,
    index: u32,
    is_input: bool,
    info: *mut ClapAudioPortInfo,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if info.is_null() {
        return false;
    }
    // SAFETY: CLAP guarantees `info` points at a writable `clap_audio_port_info` for the
    // call; the struct is plain data with no invariant to preserve, so overwriting it whole
    // is sound.
    let info = unsafe { &mut *info };

    this.with_main(
        || false,
        |inner| {
            let buses = if is_input {
                &inner.buses.inputs
            } else {
                &inner.buses.outputs
            };
            let Some(bus) = buses.get(index as usize) else {
                return false;
            };
            info.id = bus.id;
            write_fixed(&mut info.name, &bus.name);
            info.flags = if bus.is_main() {
                crate::abi::CLAP_AUDIO_PORT_IS_MAIN
            } else {
                0
            };
            if inner
                .instance
                .descriptor()
                .is_some_and(|d| d.supports(SampleFormat::F64))
            {
                info.flags |= crate::abi::CLAP_AUDIO_PORT_SUPPORTS_64BITS;
            }
            info.channel_count = u32::from(bus.channel_count());
            info.port_type = port_type(info.channel_count, bus.layout.is_ambisonic());
            // Never advertise an in-place pair: see the module docs. A host that processed in
            // place would put a live `&` and `&mut` on one buffer.
            info.in_place_pair = CLAP_INVALID_ID;
            true
        },
    )
}

/// The `clap.audio-ports` table.
static AUDIO_PORTS: ClapPluginAudioPorts = ClapPluginAudioPorts {
    count: audio_ports_count,
    get: audio_ports_get,
};

// ---------------------------------------------------------------------------------------
// clap.note-ports
// ---------------------------------------------------------------------------------------

unsafe extern "C" fn note_ports_count(plugin: *const ClapPlugin, is_input: bool) -> u32 {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return 0;
    };
    this.with_main(
        || 0,
        |inner| {
            let ports = if is_input {
                &inner.event_ports.inputs
            } else {
                &inner.event_ports.outputs
            };
            u32::try_from(ports.len()).unwrap_or(u32::MAX)
        },
    )
}

unsafe extern "C" fn note_ports_get(
    plugin: *const ClapPlugin,
    index: u32,
    is_input: bool,
    info: *mut ClapNotePortInfo,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if info.is_null() {
        return false;
    }
    // SAFETY: CLAP guarantees `info` points at a writable `clap_note_port_info` for the
    // call.
    let info = unsafe { &mut *info };

    this.with_main(
        || false,
        |inner| {
            let ports = if is_input {
                &inner.event_ports.inputs
            } else {
                &inner.event_ports.outputs
            };
            let Some(port) = ports.get(index as usize) else {
                return false;
            };
            info.id = index;
            // Every DAUx event port speaks the CLAP dialect (that is what `DauxEvent` *is*) and
            // MIDI 1.0; MIDI 2.0 only when the port advertised it (abi-v1 §9).
            info.supported_dialects = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;
            if port.supports_midi2 {
                info.supported_dialects |= CLAP_NOTE_DIALECT_MIDI2;
            }
            // The CLAP dialect is preferred because it is the only one that carries note ids,
            // per-note expression and sample-accurate parameter changes without loss.
            info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
            write_fixed(&mut info.name, &port.name);
            true
        },
    )
}

/// The `clap.note-ports` table.
static NOTE_PORTS: ClapPluginNotePorts = ClapPluginNotePorts {
    count: note_ports_count,
    get: note_ports_get,
};

// ---------------------------------------------------------------------------------------
// clap.params
// ---------------------------------------------------------------------------------------

unsafe extern "C" fn params_count(plugin: *const ClapPlugin) -> u32 {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return 0;
    };
    this.with_main(
        || 0,
        |inner| u32::try_from(inner.param_ids.len()).unwrap_or(u32::MAX),
    )
}

unsafe extern "C" fn params_get_info(
    plugin: *const ClapPlugin,
    index: u32,
    info: *mut ClapParamInfo,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if info.is_null() {
        return false;
    }
    // SAFETY: CLAP guarantees `info` points at a writable `clap_param_info` for the call.
    let info = unsafe { &mut *info };

    this.with_main(
        || false,
        |inner| {
            let Ok(params) = inner.instance.params() else {
                return false;
            };
            let refs = params.param_refs();
            let Some((_, param)) = refs.get(index as usize) else {
                return false;
            };
            fill_param_info(&param.info(), info);
            true
        },
    )
}

unsafe extern "C" fn params_get_value(
    plugin: *const ClapPlugin,
    param_id: u32,
    out_value: *mut f64,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if out_value.is_null() {
        return false;
    }
    this.with_main(
        || false,
        |inner| {
            let Ok(params) = inner.instance.params() else {
                return false;
            };
            let Some(param) = params.param(ParamId(param_id)) else {
                return false;
            };
            // SAFETY: CLAP guarantees `out_value` points at one writable `double` for the call.
            unsafe { out_value.write(param.plain()) };
            true
        },
    )
}

unsafe extern "C" fn params_value_to_text(
    plugin: *const ClapPlugin,
    param_id: u32,
    value: f64,
    out_buffer: *mut c_char,
    out_buffer_capacity: u32,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    this.with_main(
        || false,
        |inner| {
            let Ok(params) = inner.instance.params() else {
                return false;
            };
            let Some(param) = params.param(ParamId(param_id)) else {
                return false;
            };
            let mut text = String::new();
            param.to_text(value, &mut text);
            // SAFETY: CLAP guarantees `out_buffer` is null or points at `out_buffer_capacity`
            // writable bytes for the call, which is exactly `write_capped`'s contract.
            unsafe { write_capped(out_buffer, out_buffer_capacity, &text) }
        },
    )
}

unsafe extern "C" fn params_text_to_value(
    plugin: *const ClapPlugin,
    param_id: u32,
    param_value_text: *const c_char,
    out_value: *mut f64,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if out_value.is_null() {
        return false;
    }
    // SAFETY: CLAP passes a NUL-terminated string valid for the call, or null.
    let Some(text) = (unsafe { borrow_str(param_value_text) }) else {
        return false;
    };
    this.with_main(
        || false,
        |inner| {
            let Ok(params) = inner.instance.params() else {
                return false;
            };
            let Some(param) = params.param(ParamId(param_id)) else {
                return false;
            };
            let Some(value) = param.from_text(text) else {
                return false;
            };
            // SAFETY: CLAP guarantees `out_value` points at one writable `double`.
            unsafe { out_value.write(value) };
            true
        },
    )
}

unsafe extern "C" fn params_flush(
    plugin: *const ClapPlugin,
    input: *const ClapInputEvents,
    _output: *const ClapOutputEvents,
) {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return;
    };
    // `flush` is `[audio-thread]` while the plug-in is active and `[main-thread]` while it
    // is not, so the non-blocking acquisition is the one that is correct in both cases —
    // and while inactive there is no audio thread to contend with anyway.
    this.with_audio(
        || (),
        |inner| {
            // SAFETY: `input` is null or a live `clap_input_events` for this call.
            let events = unsafe { ClapInputList::new(input, inner.config.sample_rate) };
            let Ok(params) = inner.instance.params() else {
                return;
            };
            // Parameter changes that arrive *outside* `process` have to be applied here: there
            // is no `process` call to hand them to. Changes that arrive *during* `process` are
            // deliberately left alone and passed through to the plug-in, because collapsing
            // them here would throw away the sample offsets that make automation accurate.
            for index in 0..events.len() {
                let Some(DauxEvent::ParamValue(e)) = events.get(index) else {
                    continue;
                };
                if let Some(param) = params.param(ParamId(e.param_id)) {
                    param.set_plain(e.value);
                }
            }
        },
    );
}

/// The `clap.params` table.
static PARAMS: ClapPluginParams = ClapPluginParams {
    count: params_count,
    get_info: params_get_info,
    get_value: params_get_value,
    value_to_text: params_value_to_text,
    text_to_value: params_text_to_value,
    flush: params_flush,
};

// ---------------------------------------------------------------------------------------
// clap.state
// ---------------------------------------------------------------------------------------

/// Group every parameter value is written under, so a plug-in's own keys can never collide
/// with the framework's.
const PARAM_GROUP: &str = "daux.params";

/// Largest state blob this adapter will read from a host, matching
/// [`daux_plugin_api::StateLimits`]'s default.
///
/// A hostile or corrupt project file is the expected input here, not the exception.
const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;

unsafe extern "C" fn state_save(plugin: *const ClapPlugin, stream: *const ClapOStream) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if stream.is_null() {
        return false;
    }
    let blob = this.with_main(
        || None,
        |inner| {
            let schema = inner
                .instance
                .descriptor()
                .map_or(1, |d| d.state_schema_version);
            let mut writer = StateWriter::new(StateVersion(schema));

            // Parameter values are the framework's job (abi-v1 §12); the controller's
            // `save_state` is only for what the framework cannot know about.
            writer.begin_group(PARAM_GROUP);
            if let Ok(params) = inner.instance.params() {
                for (id, param) in params.param_refs() {
                    writer.put_f64(&id.0.to_string(), param.plain());
                }
            }
            writer.end_group();

            if inner.instance.save_state(&mut writer).is_err() {
                return None;
            }
            writer.try_finish().ok()
        },
    );
    let Some(blob) = blob else {
        return false;
    };

    // SAFETY: `stream` is a live `clap_ostream` for the duration of the call; a null
    // `write` slot is treated as a failed save rather than jumped to.
    unsafe {
        let Some(table) = stream.as_ref() else {
            return false;
        };
        let Some(write) = table.write else {
            return false;
        };
        let mut written = 0usize;
        while written < blob.len() {
            let n = write(
                stream,
                blob.as_ptr().add(written).cast::<c_void>(),
                (blob.len() - written) as u64,
            );
            // CLAP: a negative result is an error, and zero means the host will accept no
            // more — either way the blob is incomplete and the save has failed.
            if n <= 0 {
                return false;
            }
            written += n as usize;
        }
    }
    true
}

unsafe extern "C" fn state_load(plugin: *const ClapPlugin, stream: *const ClapIStream) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if stream.is_null() {
        return false;
    }

    let mut blob: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    // SAFETY: `stream` is a live `clap_istream` for the duration of the call; a null `read`
    // slot is treated as an empty stream rather than jumped to.
    unsafe {
        let Some(table) = stream.as_ref() else {
            return false;
        };
        let Some(read) = table.read else {
            return false;
        };
        loop {
            let n = read(
                stream,
                chunk.as_mut_ptr().cast::<c_void>(),
                chunk.len() as u64,
            );
            if n < 0 {
                return false;
            }
            if n == 0 {
                break;
            }
            let n = (n as usize).min(chunk.len());
            // A host that never ends its stream must not be able to exhaust memory.
            if blob.len() + n > MAX_STATE_BYTES {
                return false;
            }
            blob.extend_from_slice(&chunk[..n]);
        }
    }

    this.with_main(
        || false,
        |inner| {
            let Ok(reader) = StateReader::from_bytes(&blob) else {
                return false;
            };
            // Restore parameters first, so a controller's `load_state` sees the values it was
            // saved alongside and can derive anything it caches from them.
            if let Ok(params) = inner.instance.params() {
                for (id, param) in params.param_refs() {
                    if let Some(value) = reader.opt_f64(&format!("{PARAM_GROUP}/{}", id.0)) {
                        param.set_plain(value);
                    }
                }
            }
            inner.instance.load_state(&reader).is_ok()
        },
    )
}

/// The `clap.state` table.
static STATE: ClapPluginState = ClapPluginState {
    save: state_save,
    load: state_load,
};

// ---------------------------------------------------------------------------------------
// clap.latency / clap.tail / clap.render
// ---------------------------------------------------------------------------------------

unsafe extern "C" fn latency_get(plugin: *const ClapPlugin) -> u32 {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return 0;
    };
    this.with_main(
        || 0,
        |inner| inner.instance.latency().map_or(0, |l| l.samples()),
    )
}

/// The `clap.latency` table.
static LATENCY: ClapPluginLatency = ClapPluginLatency { get: latency_get };

unsafe extern "C" fn tail_get(plugin: *const ClapPlugin) -> u32 {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return 0;
    };
    // CLAP marks `tail.get` `[main-thread or audio-thread]`, so the acquisition has to be
    // the non-blocking one. The fallback is "infinite" rather than "none": when the adapter
    // cannot ask the plug-in, the honest answer is that it does not know, and CLAP's only
    // way to say that errs towards keeping the plug-in alive rather than cutting a reverb
    // tail off mid-decay.
    this.with_audio(
        || u32::MAX,
        |inner| {
            match inner.instance.tail().unwrap_or(Tail::None) {
                Tail::None => 0,
                Tail::Samples(n) => n,
                // CLAP has one sentinel where DAUx has two. `Unknown` must map to "infinite"
                // rather than to zero: telling a host it may stop calling a plug-in that could
                // not say how long it rings out cuts the tail off.
                Tail::Infinite | Tail::Unknown => u32::MAX,
            }
        },
    )
}

/// The `clap.tail` table.
static TAIL: ClapPluginTail = ClapPluginTail { get: tail_get };

unsafe extern "C" fn render_has_hard_realtime_requirement(plugin: *const ClapPlugin) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    this.descriptor.daux().capabilities.is_hard_realtime()
}

unsafe extern "C" fn render_set(plugin: *const ClapPlugin, mode: i32) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    let caps = this.descriptor.daux().capabilities;
    let requested = match mode {
        CLAP_RENDER_REALTIME => ProcessMode::Realtime,
        CLAP_RENDER_OFFLINE => ProcessMode::Offline,
        _ => return false,
    };
    if requested == ProcessMode::Offline && caps.is_hard_realtime() {
        return false;
    }
    this.with_main(
        || false,
        |inner| {
            inner.render_mode = requested;
            // `ProcessConfig::process_mode` is fixed for an activation (abi-v1 §8), so the new
            // mode takes effect at the next `activate`. CLAP hosts deactivate before switching
            // to offline rendering, which is exactly that.
            true
        },
    )
}

/// The `clap.render` table.
static RENDER: ClapPluginRender = ClapPluginRender {
    has_hard_realtime_requirement: render_has_hard_realtime_requirement,
    set: render_set,
};

// ---------------------------------------------------------------------------------------
// clap.gui
// ---------------------------------------------------------------------------------------

/// `[main-thread]` The editor's preferred size in physical pixels.
fn preferred_physical(inner: &Inner) -> PhysicalSize {
    inner.editor.as_ref().map_or_else(
        || PhysicalSize::new(0, 0),
        |e| {
            e.descriptor()
                .preferred_size
                .to_physical(inner.editor_scale)
        },
    )
}

/// `[main-thread]` Builds a [`WindowTarget`] from a `clap_window`.
///
/// # Safety
///
/// `window` must point at a live `clap_window` whose `api` names the union member the host
/// initialised.
unsafe fn window_target(window: *const ClapWindow) -> Option<WindowTarget> {
    // SAFETY: the caller guarantees `window` is a live `clap_window` for the call.
    let window = unsafe { window.as_ref() }?;
    // SAFETY: `api` is a NUL-terminated string the host owns for the call.
    let api = unsafe { borrow_str(window.api) }?;
    // SAFETY: CLAP's contract is that `api` names which union member is valid, which is the
    // only thing that makes reading one of them sound. Each constructor rejects a null or
    // zero handle, so a host that lied about the member still cannot produce a usable
    // target out of a null pointer.
    unsafe {
        match api {
            "win32" => WindowTarget::win32(window.handle.win32.addr() as isize),
            "cocoa" => WindowTarget::cocoa(window.handle.cocoa),
            // CLAP's X11 handle is the window id alone; the display is the process default,
            // which `WindowTarget::x11` expresses as a null `Display *`.
            "x11" => WindowTarget::x11(window.handle.x11 as u64, ptr::null_mut()),
            // Wayland needs a surface *and* a display and CLAP 1.2 carries only one
            // pointer, so there is nothing honest to build.
            _ => None,
        }
    }
}

unsafe extern "C" fn gui_is_api_supported(
    plugin: *const ClapPlugin,
    api: *const c_char,
    is_floating: bool,
) -> bool {
    // SAFETY: as in `plugin_init`.
    if unsafe { instance_of(plugin) }.is_none() {
        return false;
    }
    if is_floating {
        // A floating editor means the plug-in owns a top-level window, which `DauxGraphic`
        // deliberately does not do: the host hands over a view (abi-v1 §11.4).
        return false;
    }
    // SAFETY: CLAP passes a NUL-terminated API name valid for the call, or null.
    let Some(api) = (unsafe { borrow_str(api) }) else {
        return false;
    };
    PLATFORM_WINDOW_API.to_str().is_ok_and(|p| p == api)
}

unsafe extern "C" fn gui_get_preferred_api(
    plugin: *const ClapPlugin,
    api: *mut *const c_char,
    is_floating: *mut bool,
) -> bool {
    // SAFETY: as in `plugin_init`.
    if unsafe { instance_of(plugin) }.is_none() {
        return false;
    }
    if api.is_null() || is_floating.is_null() {
        return false;
    }
    // SAFETY: CLAP guarantees both out-pointers are writable for the call. The string
    // written is a `'static` C literal, so it outlives anything the host does with it.
    unsafe {
        api.write(PLATFORM_WINDOW_API.as_ptr());
        is_floating.write(false);
    }
    true
}

unsafe extern "C" fn gui_create(
    plugin: *const ClapPlugin,
    api: *const c_char,
    is_floating: bool,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    // SAFETY: `gui_is_api_supported` only reads `api` and the instance, both valid here.
    if !unsafe { gui_is_api_supported(plugin, api, is_floating) } {
        return false;
    }
    this.with_main(
        || false,
        |inner| {
            if inner.editor.is_some() {
                // CLAP forbids creating twice without destroying; refusing is safer than
                // silently dropping the first editor while its window is still attached.
                return false;
            }
            match inner.instance.create_editor() {
                Ok(Some(editor)) => {
                    inner.editor_size = editor
                        .descriptor()
                        .preferred_size
                        .to_physical(inner.editor_scale);
                    inner.editor = Some(editor);
                    true
                }
                Ok(None) | Err(_) => false,
            }
        },
    )
}

unsafe extern "C" fn gui_destroy(plugin: *const ClapPlugin) {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return;
    };
    this.with_main(
        || (),
        |inner| {
            if let Some(editor) = inner.editor.as_mut() {
                // `close` is idempotent by contract, so calling it for an editor that never got
                // a parent window is fine — and skipping it for one that did would leak the
                // rendering resources it created.
                editor.close();
            }
            inner.editor = None;
            inner.editor_open = false;
        },
    );
}

unsafe extern "C" fn gui_set_scale(plugin: *const ClapPlugin, scale: f64) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    let Some(scale) = ScaleFactor::new(scale) else {
        return false;
    };
    this.with_main(
        || false,
        |inner| {
            inner.editor_scale = scale;
            if let Some(editor) = inner.editor.as_mut() {
                editor.scale_factor_changed(scale);
                true
            } else {
                false
            }
        },
    )
}

unsafe extern "C" fn gui_get_size(
    plugin: *const ClapPlugin,
    width: *mut u32,
    height: *mut u32,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if width.is_null() || height.is_null() {
        return false;
    }
    this.with_main(
        || false,
        |inner| {
            if inner.editor.is_none() {
                return false;
            }
            let size = if inner.editor_size.is_empty() {
                preferred_physical(inner)
            } else {
                inner.editor_size
            };
            // SAFETY: CLAP guarantees both out-pointers are writable for the call.
            unsafe {
                width.write(size.width);
                height.write(size.height);
            }
            true
        },
    )
}

unsafe extern "C" fn gui_can_resize(plugin: *const ClapPlugin) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    this.with_main(
        || false,
        |inner| {
            inner
                .editor
                .as_ref()
                .is_some_and(|e| e.descriptor().resizable)
        },
    )
}

unsafe extern "C" fn gui_get_resize_hints(
    plugin: *const ClapPlugin,
    hints: *mut ClapGuiResizeHints,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if hints.is_null() {
        return false;
    }
    // SAFETY: CLAP guarantees `hints` points at a writable struct for the call.
    let hints = unsafe { &mut *hints };
    this.with_main(
        || false,
        |inner| {
            let Some(editor) = inner.editor.as_ref() else {
                return false;
            };
            let descriptor = editor.descriptor();
            hints.can_resize_horizontally = descriptor.resizable;
            hints.can_resize_vertically = descriptor.resizable;
            match descriptor.keeps_aspect {
                // CLAP wants an integer ratio; scaling by 1000 keeps three decimal places,
                // which is finer than any window manager honours.
                Some(ratio) if ratio.is_finite() && ratio > 0.0 => {
                    hints.preserve_aspect_ratio = true;
                    hints.aspect_ratio_width = (ratio * 1000.0).round().clamp(1.0, 1e9) as u32;
                    hints.aspect_ratio_height = 1000;
                }
                _ => {
                    hints.preserve_aspect_ratio = false;
                    hints.aspect_ratio_width = 0;
                    hints.aspect_ratio_height = 0;
                }
            }
            true
        },
    )
}

unsafe extern "C" fn gui_adjust_size(
    plugin: *const ClapPlugin,
    width: *mut u32,
    height: *mut u32,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    if width.is_null() || height.is_null() {
        return false;
    }
    // SAFETY: CLAP guarantees both pointers are readable and writable for the call.
    let (w, h) = unsafe { (width.read(), height.read()) };
    this.with_main(
        || false,
        |inner| {
            let Some(editor) = inner.editor.as_ref() else {
                return false;
            };
            let descriptor = editor.descriptor();
            let logical = PhysicalSize::new(w, h).to_logical(inner.editor_scale);
            let clamped = descriptor.clamp(logical).to_physical(inner.editor_scale);
            // SAFETY: as above.
            unsafe {
                width.write(clamped.width.max(1));
                height.write(clamped.height.max(1));
            }
            true
        },
    )
}

unsafe extern "C" fn gui_set_size(plugin: *const ClapPlugin, width: u32, height: u32) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    this.with_main(
        || false,
        |inner| {
            let size = PhysicalSize::new(width, height);
            let Some(editor) = inner.editor.as_mut() else {
                return false;
            };
            if editor.resize(size).is_err() {
                return false;
            }
            inner.editor_size = size;
            true
        },
    )
}

unsafe extern "C" fn gui_set_parent(plugin: *const ClapPlugin, window: *const ClapWindow) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    // SAFETY: CLAP guarantees `window` is a live `clap_window` for the call whose `api`
    // names the initialised union member.
    let Some(target) = (unsafe { window_target(window) }) else {
        return false;
    };
    this.with_main(
        || false,
        |inner| {
            if inner.editor_open {
                // CLAP does not re-parent an open editor; a host that tries would leave the
                // editor rendering into a window it no longer owns.
                return false;
            }
            let size = if inner.editor_size.is_empty() {
                preferred_physical(inner)
            } else {
                inner.editor_size
            };
            let host_caps = HostGraphicCaps::in_process();
            let Some(editor) = inner.editor.as_mut() else {
                return false;
            };
            let descriptor = editor.descriptor();
            // Prefer what the host can actually drive; fall back to the editor's own first
            // choice, and only then to a software-rendered embedded surface, which is the one
            // combination every platform can present.
            let profile = descriptor
                .capabilities
                .negotiate_with_fallback(&host_caps)
                .or_else(|| descriptor.capabilities.profiles().first().copied())
                .unwrap_or_else(|| {
                    GraphicProfile::new(
                        GraphicFramework::Custom,
                        GraphicRenderer::Software,
                        PresentationMode::EmbeddedSurface,
                    )
                });
            let mut ctx =
                GraphicContext::new(target, size, inner.editor_scale, profile, &inner.services);
            if editor.open(&mut ctx).is_err() {
                return false;
            }
            inner.editor_size = size;
            inner.editor_open = true;
            true
        },
    )
}

unsafe extern "C" fn gui_set_transient(
    plugin: *const ClapPlugin,
    _window: *const ClapWindow,
) -> bool {
    // SAFETY: as in `plugin_init`.
    let _ = unsafe { instance_of(plugin) };
    // Only meaningful for floating editors, which this adapter does not offer.
    false
}

unsafe extern "C" fn gui_suggest_title(plugin: *const ClapPlugin, _title: *const c_char) {
    // SAFETY: as in `plugin_init`.
    let _ = unsafe { instance_of(plugin) };
    // Only meaningful for floating editors, which this adapter does not offer.
}

unsafe extern "C" fn gui_show(plugin: *const ClapPlugin) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    // An embedded editor's visibility belongs to the host's window; there is nothing for the
    // plug-in to do, and reporting success is what tells the host the editor is ready.
    this.with_main(|| false, |inner| inner.editor_open)
}

unsafe extern "C" fn gui_hide(plugin: *const ClapPlugin) -> bool {
    // SAFETY: as in `plugin_init`.
    let Some(this) = (unsafe { instance_of(plugin) }) else {
        return false;
    };
    this.with_main(|| false, |inner| inner.editor_open)
}

/// The `clap.gui` table.
static GUI: ClapPluginGui = ClapPluginGui {
    is_api_supported: gui_is_api_supported,
    get_preferred_api: gui_get_preferred_api,
    create: gui_create,
    destroy: gui_destroy,
    set_scale: gui_set_scale,
    get_size: gui_get_size,
    can_resize: gui_can_resize,
    get_resize_hints: gui_get_resize_hints,
    adjust_size: gui_adjust_size,
    set_size: gui_set_size,
    set_parent: gui_set_parent,
    set_transient: gui_set_transient,
    suggest_title: gui_suggest_title,
    show: gui_show,
    hide: gui_hide,
};

/// `[main-thread]` The default editor size when a plug-in offers no descriptor at all.
///
/// Exposed for tests and for hosts that ask before an editor exists.
#[must_use]
pub const fn fallback_editor_size() -> LogicalSize {
    LogicalSize::new(640.0, 480.0)
}

/// `[main-thread]` The extension-id constants this adapter answers to, in the order
/// `clap_plugin::get_extension` tests them.
///
/// Useful to a host-side conformance test and to `daux validate`.
#[must_use]
pub fn published_extensions() -> [&'static CStr; 8] {
    [
        CLAP_EXT_AUDIO_PORTS,
        CLAP_EXT_NOTE_PORTS,
        CLAP_EXT_PARAMS,
        CLAP_EXT_STATE,
        CLAP_EXT_LATENCY,
        CLAP_EXT_TAIL,
        CLAP_EXT_RENDER,
        CLAP_EXT_GUI,
    ]
}

/// `[main-thread]` The largest CLAP name buffer, exposed so tests can assert truncation.
pub const NAME_CAPACITY: usize = CLAP_NAME_SIZE;
