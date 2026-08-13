//! The plug-in object: one DAUx instance wearing four VST3 interfaces.
//!
//! # Why one object rather than two
//!
//! VST3's canonical model splits a plug-in into an `IComponent` (the DSP) and an
//! `IEditController` (the parameters and the editor), which may live in different processes
//! and talk over `IConnectionPoint`. DAUx has one object with two halves —
//! [`DauxProcessor`](daux_plugin_api::DauxProcessor) and
//! [`DauxController`](daux_plugin_api::DauxController) — that share an `Arc` of parameters
//! and, for a meter, a lock-free queue.
//!
//! Splitting them across processes would mean the editor's parameter changes reach the DSP
//! only after a round trip through the host, and a meter's values would have to be
//! serialised into `IMessage`s. So this adapter exports a **single-component effect**: one
//! COM object implementing `IComponent`, `IAudioProcessor`, `IEditController` and
//! `IConnectionPoint`, which is legal VST3 (Steinberg ships `SingleComponentEffect` for
//! exactly this) and is what every host supports.
//!
//! `IComponent::getControllerClassId` therefore answers `kNotImplemented`, which is the
//! documented signal for "query `IEditController` from me instead".
//!
//! ## The mapping, in full
//!
//! | VST3 | DAUx |
//! |---|---|
//! | `IPluginBase::initialize` | [`PluginInstance::init`], then the parameter mirror and the editor are built |
//! | `IPluginBase::terminate` | the instance is dropped when the last reference goes |
//! | `IComponent::setActive(true)` | [`PluginInstance::activate`] → `DauxProcessor::prepare` |
//! | `IComponent::setActive(false)` | [`PluginInstance::deactivate`] |
//! | `IAudioProcessor::setupProcessing` | the [`ProcessConfig`] the next `prepare` is sized from |
//! | `IAudioProcessor::setProcessing` | [`PluginInstance::start_processing`] → `DauxProcessor::activate` |
//! | `IAudioProcessor::process` | [`PluginInstance::process`] |
//! | `IEditController::*` | the parameter mirror in [`crate::params`], never the plug-in |
//! | `IConnectionPoint::*` | accepted and ignored: there is no peer to talk to |
//!
//! # Threading
//!
//! VST3 calls `process` on the audio thread and everything else on the UI thread, and for a
//! single-component plug-in it calls them concurrently. The object is therefore split into
//! three disjoint parts, and nothing reaches across:
//!
//! * `dsp` — the [`PluginInstance`] and its preallocated buffers. Only `IPluginBase`,
//!   `IComponent` and `IAudioProcessor` touch it, and VST3 serialises those against `process`
//!   for one instance.
//! * `ui` — the editor. Only `IEditController::createView` and the view touch it.
//! * everything else — the parameter mirror, the latency and tail caches, the component
//!   handler — is atomics, readable from either thread.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::mem::offset_of;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use daux_plugin_api::{
    AudioBufferMut, AudioBufferRef, AudioBuses, BusLayout, DauxEvent, DauxGraphic, EventBuffer,
    EventHeader, EventPortLayout, HostGui, HostLatency, HostParams, HostServices, Latency,
    ParamEvent, ParamId, PluginInstance, ProcessConfig, ProcessContext, ProcessEvents,
    ProcessStatus, RescanFlags, RtHostServices, Sample, Tail,
};

use crate::api::{
    self, AudioBusBuffers, BusInfo, IAudioProcessorVtbl, IComponentHandlerVtbl, IComponentVtbl,
    IConnectionPointVtbl, IEditControllerVtbl, IEventListVtbl, IParamValueQueueVtbl,
    IParameterChangesVtbl, ParameterInfo, ProcessData, ProcessSetup, RoutingInfo, bus_direction,
    bus_flags, bus_type, media_type, restart_flags, sample_size,
};
use crate::com::{HostPtr, TBool, TResult, TUid, iid_eq, result};
use crate::factory::ClassEntry;
use crate::guard::Poison;
use crate::mapping;
use crate::params::ParamTable;
use crate::stream;
use crate::strings;
use crate::view::Vst3View;

/// Events one block may carry before the rest are dropped.
///
/// Preallocated in `setupProcessing`; `process` never grows it, because growing it would
/// allocate on the audio thread. Dropping the tail of an absurd event burst is the correct
/// real-time behaviour (abi-v1 §9).
const EVENT_CAPACITY: usize = 2048;

/// Bytes of SysEx payload one block may carry.
const EVENT_BYTE_CAPACITY: usize = 16 * 1024;

/// The configuration used when a host starts a plug-in without calling `setupProcessing`.
///
/// VST3 requires the call, and hosts still skip it. Guessing is better than refusing,
/// because refusing means silence.
fn fallback_config() -> ProcessConfig {
    ProcessConfig::new(48_000.0, 512)
}

// ---------------------------------------------------------------------------------------
// The object
// ---------------------------------------------------------------------------------------

/// The audio-thread half's state. Only ever borrowed from `IComponent`,
/// `IAudioProcessor` and `IPluginBase`, which VST3 serialises against each other.
struct Dsp {
    instance: PluginInstance,
    setup: ProcessConfig,
    bus_layout: BusLayout,
    event_ports: EventPortLayout,
    initialised: bool,
    active: bool,
    processing: bool,
    input_views: Vec<AudioBufferRef<'static, f32>>,
    output_views: Vec<AudioBufferMut<'static, f32>>,
    input_views_f64: Vec<AudioBufferRef<'static, f64>>,
    output_views_f64: Vec<AudioBufferMut<'static, f64>>,
    input_events: EventBuffer,
    output_events: EventBuffer,
}

/// The UI-thread half's state. Only ever borrowed from `IEditController::createView` and
/// from the view it hands out.
struct Ui {
    /// Created once during `initialize`, so that opening a window never needs a `&mut` on
    /// the plug-in while the audio thread is inside it. `None` for a headless plug-in.
    editor: Option<Box<dyn DauxGraphic>>,
    /// The view currently holding the editor, or null. Borrowed, not owned: the view holds
    /// a reference to *this* object, never the other way round, so there is no cycle.
    view: *mut Vst3View,
}

/// One plug-in instance, as VST3 sees it.
///
/// The four vtable pointers are the interface *heads*: a `*mut` to any of them is a valid
/// pointer to that interface, and every method recovers the object by subtracting its head's
/// offset. That is what C++ multiple inheritance emits, transcribed.
#[repr(C)]
pub struct Vst3Component {
    component_vtbl: *const IComponentVtbl,
    audio_processor_vtbl: *const IAudioProcessorVtbl,
    edit_controller_vtbl: *const IEditControllerVtbl,
    connection_point_vtbl: *const IConnectionPointVtbl,
    /// One count for the whole object, shared by all four heads — COM identity requires it.
    ref_count: AtomicU32,
    poison: Poison,
    class: Arc<ClassEntry>,
    /// The host's automation sink. An **owned** reference: released in `Drop` and whenever
    /// `setComponentHandler` replaces it.
    handler: HostPtr,
    /// Built during `initialize`, immutable afterwards apart from its atomics.
    params: OnceLock<ParamTable>,
    /// Set by `setParamNormalized`; drained by the next `process`.
    pending_params: AtomicBool,
    latency: AtomicU32,
    tail: AtomicU32,
    dsp: UnsafeCell<Dsp>,
    ui: UnsafeCell<Ui>,
}

static COMPONENT_VTBL: IComponentVtbl = IComponentVtbl {
    query_interface: Vst3Component::component_query_interface,
    add_ref: Vst3Component::component_add_ref,
    release: Vst3Component::component_release,
    initialize: Vst3Component::component_initialize,
    terminate: Vst3Component::component_terminate,
    get_controller_class_id: Vst3Component::get_controller_class_id,
    set_io_mode: Vst3Component::set_io_mode,
    get_bus_count: Vst3Component::get_bus_count,
    get_bus_info: Vst3Component::get_bus_info,
    get_routing_info: Vst3Component::get_routing_info,
    activate_bus: Vst3Component::activate_bus,
    set_active: Vst3Component::set_active,
    set_state: Vst3Component::component_set_state,
    get_state: Vst3Component::component_get_state,
};

static AUDIO_PROCESSOR_VTBL: IAudioProcessorVtbl = IAudioProcessorVtbl {
    query_interface: Vst3Component::audio_query_interface,
    add_ref: Vst3Component::audio_add_ref,
    release: Vst3Component::audio_release,
    set_bus_arrangements: Vst3Component::set_bus_arrangements,
    get_bus_arrangement: Vst3Component::get_bus_arrangement,
    can_process_sample_size: Vst3Component::can_process_sample_size,
    get_latency_samples: Vst3Component::get_latency_samples,
    setup_processing: Vst3Component::setup_processing,
    set_processing: Vst3Component::set_processing,
    process: Vst3Component::process,
    get_tail_samples: Vst3Component::get_tail_samples,
};

static EDIT_CONTROLLER_VTBL: IEditControllerVtbl = IEditControllerVtbl {
    query_interface: Vst3Component::controller_query_interface,
    add_ref: Vst3Component::controller_add_ref,
    release: Vst3Component::controller_release,
    initialize: Vst3Component::controller_initialize,
    terminate: Vst3Component::controller_terminate,
    set_component_state: Vst3Component::set_component_state,
    set_state: Vst3Component::controller_set_state,
    get_state: Vst3Component::controller_get_state,
    get_parameter_count: Vst3Component::get_parameter_count,
    get_parameter_info: Vst3Component::get_parameter_info,
    get_param_string_by_value: Vst3Component::get_param_string_by_value,
    get_param_value_by_string: Vst3Component::get_param_value_by_string,
    normalized_param_to_plain: Vst3Component::normalized_param_to_plain,
    plain_param_to_normalized: Vst3Component::plain_param_to_normalized,
    get_param_normalized: Vst3Component::get_param_normalized,
    set_param_normalized: Vst3Component::set_param_normalized,
    set_component_handler: Vst3Component::set_component_handler,
    create_view: Vst3Component::create_view,
};

static CONNECTION_POINT_VTBL: IConnectionPointVtbl = IConnectionPointVtbl {
    query_interface: Vst3Component::connection_query_interface,
    add_ref: Vst3Component::connection_add_ref,
    release: Vst3Component::connection_release,
    connect: Vst3Component::connect,
    disconnect: Vst3Component::disconnect,
    notify: Vst3Component::notify,
};

impl Vst3Component {
    /// `[main-thread]` Builds an instance and hands back its `IComponent` head.
    ///
    /// The returned pointer carries **one** reference, which the caller owns and must
    /// eventually `release`.
    #[must_use]
    pub fn create(instance: PluginInstance, class: Arc<ClassEntry>) -> *mut Vst3Component {
        let component = Box::new(Self {
            component_vtbl: &raw const COMPONENT_VTBL,
            audio_processor_vtbl: &raw const AUDIO_PROCESSOR_VTBL,
            edit_controller_vtbl: &raw const EDIT_CONTROLLER_VTBL,
            connection_point_vtbl: &raw const CONNECTION_POINT_VTBL,
            ref_count: AtomicU32::new(1),
            poison: Poison::new(),
            class,
            handler: HostPtr::null(),
            params: OnceLock::new(),
            pending_params: AtomicBool::new(false),
            latency: AtomicU32::new(0),
            tail: AtomicU32::new(api::K_NO_TAIL),
            dsp: UnsafeCell::new(Dsp {
                instance,
                setup: fallback_config(),
                bus_layout: BusLayout::new(),
                event_ports: EventPortLayout::none(),
                initialised: false,
                active: false,
                processing: false,
                input_views: Vec::new(),
                output_views: Vec::new(),
                input_views_f64: Vec::new(),
                output_views_f64: Vec::new(),
                input_events: EventBuffer::with_capacity(0, 0),
                output_events: EventBuffer::with_capacity(0, 0),
            }),
            ui: UnsafeCell::new(Ui {
                editor: None,
                view: core::ptr::null_mut(),
            }),
        });
        Box::into_raw(component)
    }

    /// `[any-thread]` The `IComponent` head, which is also this object's COM identity.
    #[must_use]
    pub fn as_com(component: *mut Vst3Component) -> *mut c_void {
        component.cast::<c_void>()
    }

    // ---- head <-> object -------------------------------------------------------------

    /// # Safety
    ///
    /// `this` must be a live `IComponent` head produced by [`Vst3Component::create`].
    unsafe fn from_component<'a>(this: *mut c_void) -> &'a Self {
        // SAFETY: the component head is the first field, so its address is the object's.
        // The caller promises the object is alive; the shared borrow is sound because every
        // mutable part lives behind `UnsafeCell` or an atomic.
        unsafe { &*this.cast::<Self>() }
    }

    /// # Safety
    ///
    /// As [`Vst3Component::from_component`], for the `IAudioProcessor` head.
    unsafe fn from_audio<'a>(this: *mut c_void) -> &'a Self {
        // SAFETY: the head sits at a known offset inside the object, so subtracting it
        // recovers the base — the transcription of C++'s `this` adjustment.
        unsafe {
            &*this
                .cast::<u8>()
                .sub(offset_of!(Self, audio_processor_vtbl))
                .cast::<Self>()
        }
    }

    /// # Safety
    ///
    /// As [`Vst3Component::from_component`], for the `IEditController` head.
    unsafe fn from_controller<'a>(this: *mut c_void) -> &'a Self {
        // SAFETY: as `from_audio`.
        unsafe {
            &*this
                .cast::<u8>()
                .sub(offset_of!(Self, edit_controller_vtbl))
                .cast::<Self>()
        }
    }

    /// # Safety
    ///
    /// As [`Vst3Component::from_component`], for the `IConnectionPoint` head.
    unsafe fn from_connection<'a>(this: *mut c_void) -> &'a Self {
        // SAFETY: as `from_audio`.
        unsafe {
            &*this
                .cast::<u8>()
                .sub(offset_of!(Self, connection_point_vtbl))
                .cast::<Self>()
        }
    }

    /// `[audio-thread]` The DSP half.
    ///
    /// # Safety
    ///
    /// The caller must be inside an `IPluginBase`, `IComponent` or `IAudioProcessor` method.
    /// VST3 does not call two of those concurrently for one instance, and nothing else in
    /// this object reaches `dsp`, so the exclusive borrow does not alias.
    #[allow(clippy::mut_from_ref)]
    unsafe fn dsp(&self) -> &mut Dsp {
        // SAFETY: see the method's own contract — the exclusivity is the host's threading
        // guarantee, made explicit by keeping `dsp` unreachable from the controller half.
        unsafe { &mut *self.dsp.get() }
    }

    /// `[main-thread]` The UI half.
    ///
    /// # Safety
    ///
    /// The caller must be inside `IEditController::createView` or a method of the view it
    /// produced, both of which VST3 makes UI-thread-only.
    #[allow(clippy::mut_from_ref)]
    unsafe fn ui(&self) -> &mut Ui {
        // SAFETY: as `dsp`, for the UI thread's disjoint half.
        unsafe { &mut *self.ui.get() }
    }

    /// `[any-thread]` The parameter mirror, empty until `initialize` has run.
    fn table(&self) -> &ParamTable {
        static EMPTY: OnceLock<ParamTable> = OnceLock::new();
        self.params
            .get()
            .unwrap_or_else(|| EMPTY.get_or_init(ParamTable::default))
    }

    // ---- reference counting ----------------------------------------------------------

    fn retain(&self) -> u32 {
        self.ref_count.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// # Safety
    ///
    /// The caller must own the reference being dropped and must not use the object again.
    unsafe fn dismiss(&self) -> u32 {
        let remaining = self.ref_count.fetch_sub(1, Ordering::AcqRel) - 1;
        if remaining == 0 {
            // SAFETY: the count reached zero, so this is the last reference and no other
            // thread can be inside the object; the pointer came from `Box::into_raw` in
            // `create`, so reconstituting the `Box` frees exactly the same allocation.
            drop(unsafe { Box::from_raw(core::ptr::from_ref(self).cast_mut()) });
        }
        remaining
    }

    /// `[any-thread]` Answers `queryInterface` for any of the four heads.
    ///
    /// # Safety
    ///
    /// `iid` must be null or point to sixteen readable bytes; `obj` must be null or a
    /// writable `*mut c_void` the caller owns.
    unsafe fn query(&self, iid: *const TUid, obj: *mut *mut c_void) -> TResult {
        if obj.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: `obj` was just checked; a conforming host passes a live out-parameter.
        unsafe { *obj = core::ptr::null_mut() };

        let base = core::ptr::from_ref(self).cast::<u8>().cast_mut();
        // SAFETY: `iid` is the host's; `iid_eq` tolerates null and reads sixteen bytes.
        let found = unsafe {
            if iid_eq(iid, &api::IFUNKNOWN_IID)
                || iid_eq(iid, &api::IPLUGIN_BASE_IID)
                || iid_eq(iid, &api::ICOMPONENT_IID)
            {
                // The component head is this object's canonical identity: `queryInterface`
                // for `FUnknown` must return the same pointer from every head, or a host
                // cannot tell two interfaces of one object from two objects.
                Some(base.add(offset_of!(Self, component_vtbl)))
            } else if iid_eq(iid, &api::IAUDIO_PROCESSOR_IID) {
                Some(base.add(offset_of!(Self, audio_processor_vtbl)))
            } else if iid_eq(iid, &api::IEDIT_CONTROLLER_IID) {
                Some(base.add(offset_of!(Self, edit_controller_vtbl)))
            } else if iid_eq(iid, &api::ICONNECTION_POINT_IID) {
                Some(base.add(offset_of!(Self, connection_point_vtbl)))
            } else {
                None
            }
        };

        match found {
            Some(head) => {
                self.retain();
                // SAFETY: `obj` was checked non-null above.
                unsafe { *obj = head.cast::<c_void>() };
                result::OK
            }
            None => result::NO_INTERFACE,
        }
    }

    // ---- host services ---------------------------------------------------------------

    /// `[main-thread]` The host services handed to the plug-in's controller and its editor.
    ///
    /// Every service reaches back into this object rather than capturing the handler
    /// pointer, so a handler that arrives (or is replaced) after `initialize` is still used.
    fn host_services(&self) -> HostServices {
        let back = ComponentRef(core::ptr::from_ref(self).cast_mut());
        HostServices::builder()
            .params(Arc::new(back))
            .latency(Arc::new(ComponentRef(back.0)))
            .gui(Arc::new(ComponentRef(back.0)))
            .build()
    }

    /// `[main-thread]` Calls one method on the host's `IComponentHandler`, if there is one.
    fn with_handler(&self, f: impl FnOnce(*mut c_void, &IComponentHandlerVtbl)) {
        let handler = self.handler.get();
        if handler.is_null() {
            return;
        }
        // SAFETY: `handler` is an owned reference this object holds, so it is alive until
        // `setComponentHandler` replaces it or `Drop` releases it — neither of which can run
        // concurrently with a UI-thread call, which is the only place this is used.
        let vtbl = unsafe { *handler.cast::<*const IComponentHandlerVtbl>() };
        if vtbl.is_null() {
            return;
        }
        // SAFETY: a conforming host's vtable pointer is valid for the object's lifetime.
        f(handler, unsafe { &*vtbl });
    }
}

impl Drop for Vst3Component {
    fn drop(&mut self) {
        let handler = self.handler.swap(core::ptr::null_mut());
        // SAFETY: the object owns one reference to `handler`, taken in
        // `set_component_handler`; dropping the object is exactly when it must be given
        // back. `release` tolerates null.
        unsafe { crate::com::release(handler) };
    }
}

/// A back-reference from a host service to the component that owns it.
///
/// The plug-in's controller and its editor hold these inside the `HostServices` they were
/// given, and both are dropped with the component — the `Arc` cannot outlive the object it
/// points at, because the object owns the `PluginInstance` that owns the `Arc`.
#[derive(Clone, Copy)]
struct ComponentRef(*mut Vst3Component);

// SAFETY: the pointer is only dereferenced from the main thread, which is where every
// `HostParams`, `HostLatency` and `HostGui` method is documented to be called from
// (abi-v1 §15). Moving the handle itself between threads moves a pointer and nothing else.
unsafe impl Send for ComponentRef {}
// SAFETY: as above — the services take `&self` and only read atomics or call the host's
// handler, which VST3 also restricts to the UI thread.
unsafe impl Sync for ComponentRef {}

impl ComponentRef {
    /// `[main-thread]` The component, or `None` if the pointer was never set.
    fn get(self) -> Option<&'static Vst3Component> {
        if self.0.is_null() {
            return None;
        }
        // SAFETY: the component outlives every service holding this handle, because it owns
        // the `PluginInstance` that owns them.
        Some(unsafe { &*self.0 })
    }
}

impl HostParams for ComponentRef {
    fn gesture_begin(&self, id: ParamId) {
        let Some(component) = self.get() else { return };
        component.with_handler(|handler, vtbl| {
            // SAFETY: `handler` is the live, owned `IComponentHandler`; `beginEdit` takes a
            // parameter id by value and returns a status we have nothing to do with.
            unsafe {
                let _ = (vtbl.begin_edit)(handler, id.get());
            }
        });
    }

    fn gesture_end(&self, id: ParamId) {
        let Some(component) = self.get() else { return };
        component.with_handler(|handler, vtbl| {
            // SAFETY: as `gesture_begin`.
            unsafe {
                let _ = (vtbl.end_edit)(handler, id.get());
            }
        });
    }

    fn changed(&self, id: ParamId, plain: f64) {
        let Some(component) = self.get() else { return };
        // The plug-in speaks plain values; VST3 automation is normalised. This is the
        // conversion that makes an editor's knob land in the right place in the host's lane.
        let Some(entry) = component.table().find(id.get()) else {
            return;
        };
        let normalized = entry.curve.to_normalized(plain);
        entry.set_normalized(normalized);
        component.with_handler(|handler, vtbl| {
            // SAFETY: as `gesture_begin`.
            unsafe {
                let _ = (vtbl.perform_edit)(handler, id.get(), normalized);
            }
        });
    }

    fn rescan(&self, flags: RescanFlags) {
        let Some(component) = self.get() else { return };
        let mut vst3 = 0;
        if flags.contains(RescanFlags::VALUES) {
            vst3 |= restart_flags::PARAM_VALUES_CHANGED;
        }
        if flags.intersects(RescanFlags::TEXT.union(RescanFlags::INFO)) {
            vst3 |= restart_flags::PARAM_TITLES_CHANGED;
        }
        if flags.contains(RescanFlags::LIST) {
            // A parameter appearing or disappearing is not something VST3 can express
            // incrementally; the host has to re-read the whole plug-in.
            vst3 |= restart_flags::RELOAD_COMPONENT;
        }
        if vst3 == 0 {
            return;
        }
        component.with_handler(|handler, vtbl| {
            // SAFETY: as `gesture_begin`.
            unsafe {
                let _ = (vtbl.restart_component)(handler, vst3);
            }
        });
    }
}

impl HostLatency for ComponentRef {
    fn set_samples(&self, samples: u32) {
        let Some(component) = self.get() else { return };
        component.latency.store(samples, Ordering::Release);
        component.with_handler(|handler, vtbl| {
            // SAFETY: as `HostParams::gesture_begin`.
            unsafe {
                let _ = (vtbl.restart_component)(handler, restart_flags::LATENCY_CHANGED);
            }
        });
    }
}

impl HostGui for ComponentRef {
    fn request_resize(&self, w: u32, h: u32) -> bool {
        let Some(component) = self.get() else {
            return false;
        };
        Vst3View::request_resize(component, w, h)
    }

    fn request_show(&self) -> bool {
        // VST3 has no "show my editor" request: the host owns the window and opens it when
        // the user asks. Refusing is the honest answer.
        false
    }

    fn closed(&self, _destroyed: bool) {
        // A VST3 editor never closes itself; the host calls `removed`.
    }
}

// ---------------------------------------------------------------------------------------
// FUnknown, three times over
// ---------------------------------------------------------------------------------------

macro_rules! funknown_head {
    ($qi:ident, $add:ident, $rel:ident, $from:ident) => {
        unsafe extern "system" fn $qi(
            this: *mut c_void,
            iid: *const TUid,
            obj: *mut *mut c_void,
        ) -> TResult {
            if this.is_null() {
                return result::INVALID_ARGUMENT;
            }
            // SAFETY: a non-null `this` on this head came from `queryInterface` or
            // `createInstance`, so it points into a live object.
            let me = unsafe { Self::$from(this) };
            // Deliberately *not* gated on the poison flag: a host still has to be able to
            // find and release the interfaces of an object that has failed.
            // SAFETY: `iid`/`obj` are the host's; `query` checks both.
            me.poison.call_always(|| unsafe { me.query(iid, obj) })
        }

        unsafe extern "system" fn $add(this: *mut c_void) -> u32 {
            if this.is_null() {
                return 0;
            }
            // SAFETY: as above.
            unsafe { Self::$from(this) }.retain()
        }

        unsafe extern "system" fn $rel(this: *mut c_void) -> u32 {
            if this.is_null() {
                return 0;
            }
            // SAFETY: as above; the caller owns the reference it is dropping.
            unsafe {
                let me = Self::$from(this);
                me.dismiss()
            }
        }
    };
}

impl Vst3Component {
    funknown_head!(
        component_query_interface,
        component_add_ref,
        component_release,
        from_component
    );
    funknown_head!(
        audio_query_interface,
        audio_add_ref,
        audio_release,
        from_audio
    );
    funknown_head!(
        controller_query_interface,
        controller_add_ref,
        controller_release,
        from_controller
    );
    funknown_head!(
        connection_query_interface,
        connection_add_ref,
        connection_release,
        from_connection
    );
}

// ---------------------------------------------------------------------------------------
// IPluginBase + IComponent
// ---------------------------------------------------------------------------------------

impl Vst3Component {
    unsafe extern "system" fn component_initialize(
        this: *mut c_void,
        _context: *mut c_void,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live component head.
        let me = unsafe { Self::from_component(this) };
        me.poison.call(|| {
            // SAFETY: `initialize` is an `IPluginBase` call, so no `process` is in flight.
            let dsp = unsafe { me.dsp() };
            if dsp.initialised {
                return result::NOT_INITIALIZED;
            }
            if dsp.instance.init().is_err() {
                return result::INTERNAL_ERROR;
            }

            let Ok(params) = dsp.instance.params() else {
                return result::INTERNAL_ERROR;
            };
            let table = ParamTable::build(params);
            let _ = me.params.set(table);

            dsp.bus_layout = dsp
                .instance
                .bus_layout()
                .unwrap_or_else(|_| BusLayout::new());
            dsp.event_ports = dsp
                .instance
                .event_ports()
                .unwrap_or_else(|_| EventPortLayout::none());

            // The plug-in gets its host services before anything else, as abi-v1 §7 requires.
            let _ = dsp.instance.set_host(me.host_services());

            // The editor is built here, while nothing is processing, so that `createView`
            // never needs an exclusive borrow of the plug-in the audio thread is inside.
            // Its lifetime is still independent of the DSP's: it is opened and closed as
            // many times as the user likes, and closing it touches nothing in `dsp`.
            if let Ok(editor) = dsp.instance.create_editor() {
                // SAFETY: `initialize` is a UI-thread call and no view exists yet.
                unsafe { me.ui() }.editor = editor;
            }

            me.latency.store(
                dsp.instance.latency().unwrap_or(Latency::Zero).samples(),
                Ordering::Release,
            );
            me.tail.store(
                tail_samples(dsp.instance.tail().unwrap_or(Tail::None)),
                Ordering::Release,
            );

            dsp.initialised = true;
            result::OK
        })
    }

    unsafe extern "system" fn component_terminate(this: *mut c_void) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live component head.
        let me = unsafe { Self::from_component(this) };
        me.poison.call(|| {
            // SAFETY: `terminate` is an `IPluginBase` call.
            let dsp = unsafe { me.dsp() };
            if dsp.processing {
                let _ = dsp.instance.stop_processing();
                dsp.processing = false;
            }
            if dsp.active {
                let _ = dsp.instance.deactivate();
                dsp.active = false;
            }
            // The editor is dropped here rather than at `Drop`, because `terminate` is the
            // last UI-thread call a host makes and a GUI object must not be freed from
            // whatever thread happens to release the final reference.
            // SAFETY: `terminate` is a UI-thread call; a host that has not closed its view
            // first is broken, and leaving the editor alone in that case is still sound
            // because the view holds its own reference to this object.
            let ui = unsafe { me.ui() };
            if ui.view.is_null() {
                ui.editor = None;
            }
            result::OK
        })
    }

    unsafe extern "system" fn get_controller_class_id(
        _this: *mut c_void,
        _class_id: *mut TUid,
    ) -> TResult {
        // "Query `IEditController` from me instead." See the module documentation.
        result::NOT_IMPLEMENTED
    }

    unsafe extern "system" fn set_io_mode(_this: *mut c_void, _mode: i32) -> TResult {
        result::NOT_IMPLEMENTED
    }

    unsafe extern "system" fn get_bus_count(this: *mut c_void, media: i32, direction: i32) -> i32 {
        if this.is_null() {
            return 0;
        }
        // SAFETY: a live component head.
        let me = unsafe { Self::from_component(this) };
        me.poison.call_value(0, || {
            // SAFETY: an `IComponent` call.
            let dsp = unsafe { me.dsp() };
            let count = match (media, direction) {
                (media_type::AUDIO, bus_direction::INPUT) => dsp.bus_layout.inputs.len(),
                (media_type::AUDIO, bus_direction::OUTPUT) => dsp.bus_layout.outputs.len(),
                (media_type::EVENT, bus_direction::INPUT) => dsp.event_ports.inputs.len(),
                (media_type::EVENT, bus_direction::OUTPUT) => dsp.event_ports.outputs.len(),
                _ => 0,
            };
            i32::try_from(count).unwrap_or(i32::MAX)
        })
    }

    unsafe extern "system" fn get_bus_info(
        this: *mut c_void,
        media: i32,
        direction: i32,
        index: i32,
        info: *mut BusInfo,
    ) -> TResult {
        if this.is_null() || info.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live component head.
        let me = unsafe { Self::from_component(this) };
        me.poison.call(|| {
            let Ok(index) = usize::try_from(index) else {
                return result::INVALID_ARGUMENT;
            };
            // SAFETY: an `IComponent` call.
            let dsp = unsafe { me.dsp() };
            let mut out = BusInfo {
                media_type: media,
                direction,
                channel_count: 0,
                name: [0; 128],
                bus_type: bus_type::MAIN,
                flags: bus_flags::DEFAULT_ACTIVE,
            };

            match media {
                media_type::AUDIO => {
                    let buses = if direction == bus_direction::INPUT {
                        &dsp.bus_layout.inputs
                    } else {
                        &dsp.bus_layout.outputs
                    };
                    let Some(bus) = buses.get(index) else {
                        return result::INVALID_ARGUMENT;
                    };
                    out.channel_count = i32::from(bus.channel_count());
                    out.bus_type = if bus.is_main() {
                        bus_type::MAIN
                    } else {
                        bus_type::AUX
                    };
                    if bus.is_optional() {
                        out.flags = 0;
                    }
                    if bus.flags.contains(daux_plugin_api::BusFlags::CV) {
                        out.flags |= bus_flags::IS_CONTROL_VOLTAGE;
                    }
                    strings::write_utf16(&mut out.name, &bus.name);
                }
                media_type::EVENT => {
                    let ports = if direction == bus_direction::INPUT {
                        &dsp.event_ports.inputs
                    } else {
                        &dsp.event_ports.outputs
                    };
                    let Some(port) = ports.get(index) else {
                        return result::INVALID_ARGUMENT;
                    };
                    // VST3 counts MIDI channels here, not audio channels.
                    out.channel_count = 16;
                    out.bus_type = if port.is_main {
                        bus_type::MAIN
                    } else {
                        bus_type::AUX
                    };
                    strings::write_utf16(&mut out.name, &port.name);
                }
                _ => return result::INVALID_ARGUMENT,
            }

            // SAFETY: `info` was checked non-null and is a caller-owned `BusInfo`.
            unsafe { *info = out };
            result::OK
        })
    }

    unsafe extern "system" fn get_routing_info(
        _this: *mut c_void,
        _input: *mut RoutingInfo,
        _output: *mut RoutingInfo,
    ) -> TResult {
        // DAUx has no per-channel routing model; the host's default is correct.
        result::NOT_IMPLEMENTED
    }

    unsafe extern "system" fn activate_bus(
        this: *mut c_void,
        media: i32,
        _direction: i32,
        _index: i32,
        _state: TBool,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // A DAUx plug-in's bus topology is fixed, so there is nothing to switch on or off.
        // Answering `kResultOk` rather than `kNotImplemented` matters: several hosts refuse
        // to instantiate a plug-in whose `activateBus` fails.
        if media == media_type::AUDIO || media == media_type::EVENT {
            result::OK
        } else {
            result::INVALID_ARGUMENT
        }
    }

    unsafe extern "system" fn set_active(this: *mut c_void, state: TBool) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live component head.
        let me = unsafe { Self::from_component(this) };
        me.poison.call(|| {
            // SAFETY: an `IComponent` call, never concurrent with `process`.
            let dsp = unsafe { me.dsp() };
            if !dsp.initialised {
                return result::NOT_INITIALIZED;
            }
            if state != 0 {
                if dsp.active {
                    return result::OK;
                }
                let config = dsp.setup;
                if dsp.instance.activate(&config).is_err() {
                    return result::INTERNAL_ERROR;
                }
                dsp.active = true;
                me.allocate_block_storage(dsp);

                // Anything the host set through `setParamNormalized` while inactive has only
                // reached the mirror; push it into the plug-in now, while nothing is
                // processing. During playback the same job is done by the events `process`
                // synthesises.
                if let Ok(params) = dsp.instance.params() {
                    me.table().apply_to(params);
                }
                me.pending_params.store(false, Ordering::Release);

                me.latency.store(
                    dsp.instance.latency().unwrap_or(Latency::Zero).samples(),
                    Ordering::Release,
                );
                me.tail.store(
                    tail_samples(dsp.instance.tail().unwrap_or(Tail::None)),
                    Ordering::Release,
                );
                result::OK
            } else {
                if dsp.processing {
                    let _ = dsp.instance.stop_processing();
                    dsp.processing = false;
                }
                if dsp.active {
                    let _ = dsp.instance.deactivate();
                    dsp.active = false;
                }
                result::OK
            }
        })
    }

    /// Preallocates everything `process` will need. `[main-thread]`
    fn allocate_block_storage(&self, dsp: &mut Dsp) {
        let inputs = dsp.bus_layout.inputs.len();
        let outputs = dsp.bus_layout.outputs.len();
        dsp.input_views.clear();
        dsp.input_views.resize(inputs, AudioBufferRef::empty());
        dsp.output_views.clear();
        dsp.output_views.resize_with(outputs, AudioBufferMut::empty);
        dsp.input_views_f64.clear();
        dsp.input_views_f64.resize(inputs, AudioBufferRef::empty());
        dsp.output_views_f64.clear();
        dsp.output_views_f64
            .resize_with(outputs, AudioBufferMut::empty);
        dsp.input_events = EventBuffer::with_capacity(EVENT_CAPACITY, EVENT_BYTE_CAPACITY);
        dsp.output_events = EventBuffer::with_capacity(EVENT_CAPACITY, EVENT_BYTE_CAPACITY);
    }

    unsafe extern "system" fn component_set_state(
        this: *mut c_void,
        state: *mut c_void,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live component head.
        let me = unsafe { Self::from_component(this) };
        me.poison.call(|| {
            // SAFETY: the host owns `state` for the duration of the call.
            unsafe { stream::rewind(state) };
            // SAFETY: as above.
            let bytes = match unsafe { stream::read_all(state) } {
                Ok(bytes) => bytes,
                Err(status) => return status,
            };
            if bytes.is_empty() {
                return result::INVALID_ARGUMENT;
            }
            // SAFETY: an `IComponent` call.
            let dsp = unsafe { me.dsp() };
            if !dsp.initialised {
                return result::NOT_INITIALIZED;
            }
            match stream::load(&mut dsp.instance, me.table(), &bytes) {
                Ok(()) => result::OK,
                Err(_) => result::INTERNAL_ERROR,
            }
        })
    }

    unsafe extern "system" fn component_get_state(
        this: *mut c_void,
        state: *mut c_void,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live component head.
        let me = unsafe { Self::from_component(this) };
        me.poison.call(|| {
            // SAFETY: an `IComponent` call.
            let dsp = unsafe { me.dsp() };
            if !dsp.initialised {
                return result::NOT_INITIALIZED;
            }
            let Ok(bytes) = stream::save(&mut dsp.instance, me.table()) else {
                return result::INTERNAL_ERROR;
            };
            // SAFETY: the host owns `state` for the duration of the call.
            unsafe { stream::write_all(state, &bytes) }
        })
    }
}

/// The VST3 tail length for a DAUx tail.
fn tail_samples(tail: Tail) -> u32 {
    match tail {
        Tail::None => api::K_NO_TAIL,
        Tail::Samples(n) => n,
        // VST3 has no "I do not know"; the safe reading of both is "never stop calling me".
        Tail::Infinite | Tail::Unknown => api::K_INFINITE_TAIL,
    }
}

// ---------------------------------------------------------------------------------------
// IAudioProcessor
// ---------------------------------------------------------------------------------------

impl Vst3Component {
    unsafe extern "system" fn set_bus_arrangements(
        this: *mut c_void,
        inputs: *mut u64,
        num_ins: i32,
        outputs: *mut u64,
        num_outs: i32,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live audio-processor head.
        let me = unsafe { Self::from_audio(this) };
        me.poison.call(|| {
            // SAFETY: an `IAudioProcessor` call made while inactive.
            let dsp = unsafe { me.dsp() };
            let (want_ins, want_outs) = (dsp.bus_layout.inputs.len(), dsp.bus_layout.outputs.len());
            if usize::try_from(num_ins.max(0)).unwrap_or(0) != want_ins
                || usize::try_from(num_outs.max(0)).unwrap_or(0) != want_outs
            {
                return result::FALSE;
            }

            let matches = |ptr: *mut u64, count: usize, buses: &[daux_plugin_api::BusInfo]| {
                if count == 0 {
                    return true;
                }
                if ptr.is_null() {
                    return false;
                }
                (0..count).all(|i| {
                    // SAFETY: the host promises `count` readable arrangements at `ptr`, and
                    // `count` was checked against our own bus count above.
                    let proposed = unsafe { *ptr.add(i) };
                    mapping::arrangement_channel_count(proposed) == buses[i].channel_count()
                })
            };

            if matches(inputs, want_ins, &dsp.bus_layout.inputs)
                && matches(outputs, want_outs, &dsp.bus_layout.outputs)
            {
                result::TRUE
            } else {
                // Refusing makes the host re-propose or fall back to what `getBusArrangement`
                // reports. Accepting a width the plug-in cannot handle would be worse.
                result::FALSE
            }
        })
    }

    unsafe extern "system" fn get_bus_arrangement(
        this: *mut c_void,
        direction: i32,
        index: i32,
        out: *mut u64,
    ) -> TResult {
        if this.is_null() || out.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live audio-processor head.
        let me = unsafe { Self::from_audio(this) };
        me.poison.call(|| {
            let Ok(index) = usize::try_from(index) else {
                return result::INVALID_ARGUMENT;
            };
            // SAFETY: an `IAudioProcessor` call.
            let dsp = unsafe { me.dsp() };
            let buses = if direction == bus_direction::INPUT {
                &dsp.bus_layout.inputs
            } else {
                &dsp.bus_layout.outputs
            };
            let Some(bus) = buses.get(index) else {
                return result::INVALID_ARGUMENT;
            };
            // SAFETY: `out` was checked non-null and is caller-owned.
            unsafe { *out = mapping::speaker_arrangement(bus.layout) };
            result::OK
        })
    }

    unsafe extern "system" fn can_process_sample_size(this: *mut c_void, size: i32) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live audio-processor head.
        let me = unsafe { Self::from_audio(this) };
        me.poison.call(|| match size {
            sample_size::SAMPLE32 => result::OK,
            sample_size::SAMPLE64 => {
                if me
                    .class
                    .descriptor
                    .supports(daux_plugin_api::SampleFormat::F64)
                {
                    result::OK
                } else {
                    result::FALSE
                }
            }
            _ => result::FALSE,
        })
    }

    unsafe extern "system" fn get_latency_samples(this: *mut c_void) -> u32 {
        if this.is_null() {
            return 0;
        }
        // SAFETY: a live audio-processor head. The value is a cached atomic precisely so
        // that a host asking during playback does not have to reach the plug-in.
        let me = unsafe { Self::from_audio(this) };
        me.poison
            .call_value(0, || me.latency.load(Ordering::Acquire))
    }

    unsafe extern "system" fn get_tail_samples(this: *mut c_void) -> u32 {
        if this.is_null() {
            return api::K_NO_TAIL;
        }
        // SAFETY: a live audio-processor head.
        let me = unsafe { Self::from_audio(this) };
        me.poison
            .call_value(api::K_NO_TAIL, || me.tail.load(Ordering::Acquire))
    }

    unsafe extern "system" fn setup_processing(
        this: *mut c_void,
        setup: *mut ProcessSetup,
    ) -> TResult {
        if this.is_null() || setup.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live audio-processor head.
        let me = unsafe { Self::from_audio(this) };
        me.poison.call(|| {
            // SAFETY: `setup` was checked non-null and the host owns it for the call.
            let setup = unsafe { *setup };
            if !setup.sample_rate.is_finite()
                || setup.sample_rate <= 0.0
                || setup.max_samples_per_block <= 0
            {
                return result::INVALID_ARGUMENT;
            }
            let format = if setup.symbolic_sample_size == sample_size::SAMPLE64 {
                daux_plugin_api::SampleFormat::F64
            } else {
                daux_plugin_api::SampleFormat::F32
            };
            if !me.class.descriptor.supports(format) {
                return result::FALSE;
            }

            // SAFETY: an `IAudioProcessor` call, made while inactive.
            let dsp = unsafe { me.dsp() };
            if dsp.processing {
                return result::NOT_INITIALIZED;
            }
            dsp.setup = ProcessConfig::new(
                setup.sample_rate,
                u32::try_from(setup.max_samples_per_block).unwrap_or(u32::MAX),
            )
            .with_sample_format(format)
            .with_process_mode(mapping::process_mode_from_vst3(setup.process_mode));
            result::OK
        })
    }

    unsafe extern "system" fn set_processing(this: *mut c_void, state: TBool) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live audio-processor head.
        let me = unsafe { Self::from_audio(this) };
        me.poison.call(|| {
            // SAFETY: an `IAudioProcessor` call.
            let dsp = unsafe { me.dsp() };
            if !dsp.active {
                return result::NOT_INITIALIZED;
            }
            if state != 0 {
                if dsp.processing {
                    return result::OK;
                }
                if dsp.instance.start_processing().is_err() {
                    return result::INTERNAL_ERROR;
                }
                dsp.processing = true;
            } else {
                if !dsp.processing {
                    return result::OK;
                }
                let _ = dsp.instance.stop_processing();
                dsp.processing = false;
            }
            result::OK
        })
    }

    unsafe extern "system" fn process(this: *mut c_void, data: *mut ProcessData) -> TResult {
        if this.is_null() || data.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live audio-processor head.
        let me = unsafe { Self::from_audio(this) };
        // A panic inside `process` becomes an internal error *and* poisons: from here on the
        // instance answers `kNotInitialized` to everything rather than being re-entered with
        // whatever state the panic left behind (abi-v1 §17.3).
        me.poison.call_or(result::INTERNAL_ERROR, || {
            // SAFETY: `data` was checked non-null; the host owns it for the call.
            let data = unsafe { &mut *data };
            // SAFETY: `process` is the audio thread's only entry point into this object, and
            // VST3 does not call any other `IComponent`/`IAudioProcessor` method while it is
            // running.
            let dsp = unsafe { me.dsp() };
            if !dsp.processing {
                return result::NOT_INITIALIZED;
            }
            let Ok(frames) = usize::try_from(data.num_samples) else {
                return result::INVALID_ARGUMENT;
            };
            if frames > dsp.setup.max_block_size as usize {
                // The plug-in sized its buffers from `max_block_size`; an over-long block is
                // the exact input that makes it overrun or allocate.
                return result::INVALID_ARGUMENT;
            }

            // SAFETY: the host owns every pointer in `data` for the call.
            unsafe { me.collect_input_events(dsp, data, frames) };

            // Destructured rather than passed as `&mut Dsp`, so that the instance, the
            // configuration, the views and the two event buffers are borrowed as the
            // disjoint fields they are.
            let status = if data.symbolic_sample_size == sample_size::SAMPLE64 {
                let Dsp {
                    instance,
                    setup,
                    input_views_f64,
                    output_views_f64,
                    input_events,
                    output_events,
                    ..
                } = &mut *dsp;
                // SAFETY: as above; the sample size selects which union arm the host's
                // channel pointers are.
                unsafe {
                    run_block::<f64>(
                        instance,
                        setup,
                        input_views_f64,
                        output_views_f64,
                        input_events,
                        output_events,
                        data,
                        frames,
                    )
                }
            } else {
                let Dsp {
                    instance,
                    setup,
                    input_views,
                    output_views,
                    input_events,
                    output_events,
                    ..
                } = &mut *dsp;
                // SAFETY: as above.
                unsafe {
                    run_block::<f32>(
                        instance,
                        setup,
                        input_views,
                        output_views,
                        input_events,
                        output_events,
                        data,
                        frames,
                    )
                }
            };

            // SAFETY: the host owns `data.output_events` and `data.output_parameter_changes`
            // for the call.
            unsafe {
                me.emit_output_events(dsp, data);
                me.emit_output_parameters(dsp, data, frames);
            }
            dsp.output_events.clear();

            if status == ProcessStatus::Error {
                result::INTERNAL_ERROR
            } else {
                result::OK
            }
        })
    }

    /// Turns the host's automation and events into DAUx events. `[audio-thread]`
    ///
    /// # Safety
    ///
    /// Every pointer inside `data` must be null or a live host object for this call.
    unsafe fn collect_input_events(&self, dsp: &mut Dsp, data: &ProcessData, frames: usize) {
        dsp.input_events.clear();
        let last_frame = u32::try_from(frames.saturating_sub(1)).unwrap_or(0);

        // Anything the host set through `setParamNormalized` while the transport was rolling
        // has only reached the mirror; it becomes an event at the top of the block.
        if self.pending_params.swap(false, Ordering::AcqRel) {
            for entry in self.table().entries() {
                if entry.is_read_only() {
                    continue;
                }
                let _ = dsp
                    .input_events
                    .try_push(&DauxEvent::ParamValue(ParamEvent {
                        header: EventHeader::at(0),
                        param_id: entry.vst3_id(),
                        value: entry.plain(),
                        ..ParamEvent::default()
                    }));
            }
        }

        if !data.input_parameter_changes.is_null() {
            let changes = data.input_parameter_changes;
            // SAFETY: the caller promises a live `IParameterChanges`, whose first word is
            // its vtable pointer.
            let vtbl = unsafe { *changes.cast::<*const IParameterChangesVtbl>() };
            if !vtbl.is_null() {
                // SAFETY: a conforming host's vtable is valid for the object's lifetime.
                let vtbl = unsafe { &*vtbl };
                // SAFETY: `changes` is live.
                let count = unsafe { (vtbl.get_parameter_count)(changes) };
                for index in 0..count.max(0) {
                    // SAFETY: `index` is within the count the host just reported.
                    let queue = unsafe { (vtbl.get_parameter_data)(changes, index) };
                    if queue.is_null() {
                        continue;
                    }
                    // SAFETY: the host owns the queue for this call.
                    unsafe { self.drain_queue(dsp, queue, last_frame) };
                }
            }
        }

        if !data.input_events.is_null() {
            let list = data.input_events;
            // SAFETY: the caller promises a live `IEventList`, whose first word is its
            // vtable pointer.
            let vtbl = unsafe { *list.cast::<*const IEventListVtbl>() };
            if !vtbl.is_null() {
                // SAFETY: as above.
                let vtbl = unsafe { &*vtbl };
                // SAFETY: `list` is live.
                let count = unsafe { (vtbl.get_event_count)(list) };
                let ports = u16::try_from(dsp.event_ports.inputs.len()).unwrap_or(0);
                for index in 0..count.max(0) {
                    let mut event = api::Event::default();
                    // SAFETY: `event` is a live local and `index` is within the count.
                    let status = unsafe { (vtbl.get_event)(list, index, &raw mut event) };
                    if !result::is_ok(status) {
                        continue;
                    }
                    let port = u16::try_from(event.bus_index.max(0)).unwrap_or(0);
                    if ports == 0 || port >= ports {
                        continue;
                    }
                    // SAFETY: a SysEx payload is borrowed from the host's own buffer, which
                    // it owns until `process` returns; the event is copied into
                    // `input_events` (payload included) before this borrow ends.
                    if let Some(daux) = unsafe { crate::events::to_daux(&event, port) } {
                        let _ = dsp.input_events.try_push(&daux);
                    }
                }
            }
        }

        // The DAUx event model promises a time-sorted list; automation and MIDI arrive as
        // two independently sorted streams, so the merge has to be re-sorted. The sort is
        // stable, which is what keeps a note-off before the note-on that replaces it.
        dsp.input_events.sort_by_time();
    }

    /// Turns one automation lane into `ParamValue` events with **plain** values.
    ///
    /// # Safety
    ///
    /// `queue` must be a live `IParamValueQueue` for this call.
    unsafe fn drain_queue(&self, dsp: &mut Dsp, queue: *mut c_void, last_frame: u32) {
        // SAFETY: the caller promises a live queue object.
        let vtbl = unsafe { *queue.cast::<*const IParamValueQueueVtbl>() };
        if vtbl.is_null() {
            return;
        }
        // SAFETY: a conforming host's vtable outlives the object.
        let vtbl = unsafe { &*vtbl };
        // SAFETY: `queue` is live.
        let id = unsafe { (vtbl.get_parameter_id)(queue) };
        let Some(entry) = self.table().find(id) else {
            return;
        };
        // SAFETY: `queue` is live.
        let points = unsafe { (vtbl.get_point_count)(queue) };
        let mut last = None;
        for index in 0..points.max(0) {
            let mut offset: i32 = 0;
            let mut normalized: f64 = 0.0;
            // SAFETY: both out-parameters are live locals and `index` is within the count.
            let status =
                unsafe { (vtbl.get_point)(queue, index, &raw mut offset, &raw mut normalized) };
            if !result::is_ok(status) {
                continue;
            }
            let time = u32::try_from(offset.max(0)).unwrap_or(0).min(last_frame);
            // The conversion this whole adapter exists for: VST3 speaks normalised, DAUx
            // speaks plain, and the curve in between is the parameter's own.
            let plain = entry.curve.to_plain(normalized);
            let _ = dsp
                .input_events
                .try_push(&DauxEvent::ParamValue(ParamEvent {
                    header: EventHeader::at(time),
                    param_id: id,
                    value: plain,
                    ..ParamEvent::default()
                }));
            last = Some(normalized);
        }
        // The controller's value is the block's last automation point, so a UI opened
        // mid-playback shows what the automation lane says.
        if let Some(normalized) = last {
            entry.set_normalized(normalized);
        }
    }

    /// Copies the plug-in's output parameter changes into the host's lanes. `[audio-thread]`
    ///
    /// This is how a meter reaches a host's automation view and how a plug-in that moves its
    /// own control from the DSP — a follower, a randomiser, a learned macro — is recorded.
    /// Values are **normalised** on the way out, through the parameter's own curve.
    ///
    /// Gestures are *not* forwarded: VST3 records a user's drag through
    /// `IComponentHandler::beginEdit`/`endEdit`, which are `[main-thread]` calls and must
    /// never be made from `process`. A plug-in's editor makes them instead, through
    /// [`daux_plugin_api::HostParams`].
    ///
    /// # Safety
    ///
    /// `data.output_parameter_changes` must be null or a live `IParameterChanges` for this
    /// call.
    unsafe fn emit_output_parameters(&self, dsp: &Dsp, data: &ProcessData, frames: usize) {
        let changes = data.output_parameter_changes;
        if changes.is_null() {
            return;
        }
        // SAFETY: the caller promises a live `IParameterChanges`, whose first word is its
        // vtable pointer.
        let vtbl = unsafe { *changes.cast::<*const IParameterChangesVtbl>() };
        if vtbl.is_null() {
            return;
        }
        // SAFETY: a conforming host's vtable outlives the object.
        let vtbl = unsafe { &*vtbl };
        let last_frame = i32::try_from(frames.saturating_sub(1)).unwrap_or(0);

        for index in 0..dsp.output_events.len() {
            let Some(DauxEvent::ParamValue(event)) = dsp.output_events.get(index) else {
                continue;
            };
            let Some(entry) = self.table().find(event.param_id) else {
                continue;
            };
            let normalized = entry.curve.to_normalized(event.value);
            entry.set_normalized(normalized);

            let mut queue_index: i32 = 0;
            // SAFETY: `event.param_id` and `queue_index` are live locals; the host either
            // returns a queue it owns for the rest of the call, or null.
            let queue = unsafe {
                (vtbl.add_parameter_data)(
                    changes,
                    core::ptr::from_ref(&event.param_id),
                    &raw mut queue_index,
                )
            };
            if queue.is_null() {
                continue;
            }
            // SAFETY: the host owns the queue for this call.
            let queue_vtbl = unsafe { *queue.cast::<*const IParamValueQueueVtbl>() };
            if queue_vtbl.is_null() {
                continue;
            }
            let offset = i32::try_from(event.header.time)
                .unwrap_or(0)
                .clamp(0, last_frame);
            let mut point_index: i32 = 0;
            // SAFETY: `point_index` is a live local; `addPoint` copies the value.
            unsafe {
                let _ = ((*queue_vtbl).add_point)(queue, offset, normalized, &raw mut point_index);
            }
        }
    }

    /// Copies the plug-in's output events into the host's list. `[audio-thread]`
    ///
    /// # Safety
    ///
    /// `data.output_events` must be null or a live `IEventList` for this call.
    unsafe fn emit_output_events(&self, dsp: &Dsp, data: &ProcessData) {
        if data.output_events.is_null() {
            return;
        }
        // SAFETY: the caller promises a live `IEventList`.
        let vtbl = unsafe { *data.output_events.cast::<*const IEventListVtbl>() };
        if vtbl.is_null() {
            return;
        }
        // SAFETY: a conforming host's vtable outlives the object.
        let vtbl = unsafe { &*vtbl };
        for index in 0..dsp.output_events.len() {
            let Some(event) = dsp.output_events.get(index) else {
                continue;
            };
            let bus = i32::from(event.port_index());
            let Some(mut out) = crate::events::from_daux(&event, bus) else {
                continue;
            };
            // SAFETY: `out` is a live local the host copies from; `add_event` does not
            // retain the pointer.
            unsafe {
                let _ = (vtbl.add_event)(data.output_events, &raw mut out);
            }
        }
    }
}

/// Selects the `process` overload for one sample type.
///
/// A trait rather than a `match`, because [`AudioBuses`] is generic over the sample type and
/// the two arms would otherwise be the same thirty lines written twice.
trait BlockSample: Sample {
    /// The `process` overload for this sample type.
    fn run<'a>(
        instance: &mut PluginInstance,
        ctx: &ProcessContext<'a>,
        buses: &mut AudioBuses<'a, Self>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus;
}

impl BlockSample for f32 {
    fn run<'a>(
        instance: &mut PluginInstance,
        ctx: &ProcessContext<'a>,
        buses: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        instance.process(ctx, buses, events)
    }
}

impl BlockSample for f64 {
    fn run<'a>(
        instance: &mut PluginInstance,
        ctx: &ProcessContext<'a>,
        buses: &mut AudioBuses<'a, f64>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        instance.process_f64(ctx, buses, events)
    }
}

/// Assembles the audio views and runs one block. `[audio-thread]`
///
/// Takes the [`Dsp`] fields it needs one by one rather than the whole struct, because the
/// instance, the configuration, the views and the two event buffers are all borrowed at once
/// and only disjoint field borrows make that legal.
///
/// # Safety
///
/// `data.inputs`/`data.outputs` must describe live buffers of `frames` samples in the sample
/// format `T`, owned by the host for exactly this call.
#[allow(clippy::too_many_arguments)]
unsafe fn run_block<T: BlockSample>(
    instance: &mut PluginInstance,
    setup: &ProcessConfig,
    input_views: &mut [AudioBufferRef<'static, T>],
    output_views: &mut [AudioBufferMut<'static, T>],
    input_events: &EventBuffer,
    output_events: &mut EventBuffer,
    data: &mut ProcessData,
    frames: usize,
) -> ProcessStatus {
    let context = if data.process_context.is_null() {
        None
    } else {
        // SAFETY: the host owns the context for this call.
        Some(unsafe { *data.process_context })
    };
    let transport = context.as_ref().map(mapping::transport_from_context);
    let steady = context.and_then(|ctx| {
        (ctx.state & api::context_state::CONT_TIME_VALID != 0).then_some(ctx.continous_time_samples)
    });

    // SAFETY: the caller promises the host's buffers are live for this call.
    let used_in = unsafe { fill_inputs(input_views, data.inputs, data.num_inputs, frames) };
    // SAFETY: as above, for writable output buffers.
    let used_out = unsafe { fill_outputs(output_views, data.outputs, data.num_outputs, frames) };

    let host = RtHostServices::null();
    let status = {
        let mut buses = AudioBuses::new(
            &input_views[..used_in],
            // SAFETY: shortens the placeholder `'static` in the preallocated view array to
            // this block. The pointers inside were written from `data` a line ago and are
            // valid for exactly this call; nothing escapes the `AudioBuses`, which is dropped
            // before `process` returns.
            unsafe { shorten_mut(&mut output_views[..used_out]) },
            frames,
        );

        let mut ctx = ProcessContext::new(frames, setup, &host);
        if let Some(transport) = transport.as_ref() {
            ctx = ctx.with_transport(transport);
        }
        if let Some(steady) = steady {
            ctx = ctx.with_steady_time(steady);
        }
        let mut events = ProcessEvents::new(input_events, output_events);
        T::run(instance, &ctx, &mut buses, &mut events)
    };

    // Never leave the host's pointers in our arrays between blocks.
    input_views.fill(AudioBufferRef::empty());
    for view in output_views.iter_mut() {
        *view = AudioBufferMut::empty();
    }

    // The plug-in wrote real samples, so the host's "this bus is silent" hints are stale.
    if !data.outputs.is_null() {
        for index in 0..data.num_outputs.max(0) {
            // SAFETY: the host promises `num_outputs` bus descriptions at `outputs`.
            unsafe { (*data.outputs.add(index as usize)).silence_flags = 0 };
        }
    }
    status
}

/// Writes the host's input buses into the preallocated views, returning how many were used.
///
/// # Safety
///
/// `buses` must be null or point to `count` live [`AudioBusBuffers`] whose channel pointers
/// each cover `frames` readable samples of type `T`.
unsafe fn fill_inputs<T: Sample>(
    views: &mut [AudioBufferRef<'static, T>],
    buses: *mut AudioBusBuffers,
    count: i32,
    frames: usize,
) -> usize {
    let available = usize::try_from(count.max(0)).unwrap_or(0).min(views.len());
    // A bus the host has switched off arrives with no channels, and it still belongs to a
    // block of `frames` frames — an `AudioBufferRef::empty()` would claim zero and trip
    // `AudioBuses`' own consistency check.
    // SAFETY: with `channels == 0` the pointer array is never read, so null is allowed.
    let silent = unsafe { AudioBufferRef::<T>::from_raw(core::ptr::null(), 0, frames) };
    if buses.is_null() {
        for view in views.iter_mut().take(available) {
            *view = silent;
        }
        return available;
    }
    for (index, view) in views.iter_mut().take(available).enumerate() {
        // SAFETY: the caller promises `count` bus descriptions and `index < count`.
        let bus = unsafe { *buses.add(index) };
        let channels = usize::try_from(bus.num_channels.max(0)).unwrap_or(0);
        *view = if channels == 0 || bus.channel_buffers.is_null() {
            silent
        } else {
            // SAFETY: the caller promises `channels` non-null pointers, each covering
            // `frames` readable samples; the `'static` lifetime is a placeholder that
            // `run_block` immediately shortens and never lets escape the block.
            unsafe {
                AudioBufferRef::from_raw(
                    bus.channel_buffers.cast::<*const T>().cast_const(),
                    channels,
                    frames,
                )
            }
        };
    }
    available
}

/// Writes the host's output buses into the preallocated views, returning how many were used.
///
/// # Safety
///
/// As [`fill_inputs`], with reads upgraded to writes, and the channel pointers of one bus
/// pairwise distinct — which VST3 requires of a host.
unsafe fn fill_outputs<T: Sample>(
    views: &mut [AudioBufferMut<'static, T>],
    buses: *mut AudioBusBuffers,
    count: i32,
    frames: usize,
) -> usize {
    let available = usize::try_from(count.max(0)).unwrap_or(0).min(views.len());
    if buses.is_null() {
        for view in views.iter_mut().take(available) {
            // SAFETY: as `fill_inputs` — a channel-less view reads no pointers, and it must
            // still report the block's frame count.
            *view = unsafe { AudioBufferMut::<T>::from_raw(core::ptr::null(), 0, frames) };
        }
        return available;
    }
    for (index, view) in views.iter_mut().take(available).enumerate() {
        // SAFETY: the caller promises `count` bus descriptions and `index < count`.
        let bus = unsafe { *buses.add(index) };
        let channels = usize::try_from(bus.num_channels.max(0)).unwrap_or(0);
        *view = if channels == 0 || bus.channel_buffers.is_null() {
            // SAFETY: as above.
            unsafe { AudioBufferMut::<T>::from_raw(core::ptr::null(), 0, frames) }
        } else {
            // SAFETY: as `fill_inputs`, for writable, pairwise-distinct channels.
            unsafe {
                AudioBufferMut::from_raw(bus.channel_buffers.cast::<*mut T>(), channels, frames)
            }
        };
    }
    available
}

/// Shortens the placeholder `'static` inside a preallocated output view array.
///
/// # Safety
///
/// The pointers inside `views` must be valid for the whole of `'a`, and nothing derived from
/// them may outlive it.
unsafe fn shorten_mut<'a, T: Sample>(
    views: &'a mut [AudioBufferMut<'static, T>],
) -> &'a mut [AudioBufferMut<'a, T>] {
    // SAFETY: the two types differ only in a lifetime parameter, so they have identical
    // layout; `&mut [_]` is invariant in its element type, which is the only reason a
    // transmute is needed rather than a coercion. The caller guarantees the pointers outlive
    // `'a`.
    unsafe { core::mem::transmute(views) }
}

// ---------------------------------------------------------------------------------------
// IEditController
// ---------------------------------------------------------------------------------------

impl Vst3Component {
    unsafe extern "system" fn controller_initialize(
        this: *mut c_void,
        context: *mut c_void,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        // In single-component mode the object is initialised once, through whichever head the
        // host reached first. A host that initialises both halves — some do, out of habit —
        // must get a success, not an error, or it refuses to load the plug-in at all. This is
        // therefore idempotent, unlike the component head, where a second `initialize` really
        // is a sequencing bug.
        let already = me.poison.call_value(false, || {
            // SAFETY: `initialize` is an `IPluginBase` call, so no `process` is in flight.
            unsafe { me.dsp() }.initialised
        });
        if already {
            return result::OK;
        }
        let component = Self::as_com(core::ptr::from_ref(me).cast_mut());
        // SAFETY: `component` is this object's `IComponent` head.
        unsafe { Self::component_initialize(component, context) }
    }

    unsafe extern "system" fn controller_terminate(this: *mut c_void) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        let component = Self::as_com(core::ptr::from_ref(me).cast_mut());
        // SAFETY: `component` is this object's `IComponent` head.
        unsafe { Self::component_terminate(component) }
    }

    unsafe extern "system" fn set_component_state(
        this: *mut c_void,
        state: *mut c_void,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison.call(|| {
            // SAFETY: the host owns `state` for the call.
            unsafe { stream::rewind(state) };
            // SAFETY: as above.
            let bytes = match unsafe { stream::read_all(state) } {
                Ok(bytes) => bytes,
                Err(status) => return status,
            };
            let Ok(reader) = daux_plugin_api::StateReader::from_bytes(&bytes) else {
                return result::INVALID_ARGUMENT;
            };
            // Only the mirror is touched: the component half has already loaded the same
            // blob into the plug-in itself, and reaching the plug-in from here would alias
            // the audio thread's borrow.
            stream::mirror_params(&reader, me.table());
            result::OK
        })
    }

    unsafe extern "system" fn controller_set_state(
        this: *mut c_void,
        _state: *mut c_void,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // DAUx keeps no controller-only state: everything a plug-in saves belongs to the
        // component, which is what makes a preset portable between the VST3, CLAP and AXT
        // exports of the same plug-in.
        result::OK
    }

    unsafe extern "system" fn controller_get_state(
        this: *mut c_void,
        _state: *mut c_void,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        result::OK
    }

    unsafe extern "system" fn get_parameter_count(this: *mut c_void) -> i32 {
        if this.is_null() {
            return 0;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison
            .call_value(0, || i32::try_from(me.table().len()).unwrap_or(i32::MAX))
    }

    unsafe extern "system" fn get_parameter_info(
        this: *mut c_void,
        index: i32,
        info: *mut ParameterInfo,
    ) -> TResult {
        if this.is_null() || info.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison.call(|| {
            let Ok(index) = usize::try_from(index) else {
                return result::INVALID_ARGUMENT;
            };
            let Some(entry) = me.table().at(index) else {
                return result::INVALID_ARGUMENT;
            };

            let mut out = ParameterInfo {
                id: entry.vst3_id(),
                title: [0; 128],
                short_title: [0; 128],
                units: [0; 128],
                step_count: i32::try_from(entry.info.step_count).unwrap_or(i32::MAX),
                default_normalized_value: entry.default_normalized,
                unit_id: 0,
                flags: mapping::parameter_flags(entry.info.flags, entry.info.step_count > 0),
            };
            strings::write_utf16(&mut out.title, &entry.info.name);
            strings::write_utf16(&mut out.short_title, &entry.info.name);
            strings::write_utf16(&mut out.units, &entry.info.unit);

            // SAFETY: `info` was checked non-null and is caller-owned.
            unsafe { *info = out };
            result::OK
        })
    }

    unsafe extern "system" fn get_param_string_by_value(
        this: *mut c_void,
        id: u32,
        normalized: f64,
        string: *mut crate::com::Char16,
    ) -> TResult {
        if this.is_null() || string.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison.call(|| {
            let Some(entry) = me.table().find(id) else {
                return result::INVALID_ARGUMENT;
            };
            let mut text = String::with_capacity(32);
            entry.format(entry.curve.to_plain(normalized), &mut text);
            // SAFETY: VST3's `String128` is 128 code units the host owns, and `string` was
            // checked non-null.
            let out = unsafe { core::slice::from_raw_parts_mut(string, 128) };
            strings::write_utf16(out, &text);
            result::OK
        })
    }

    unsafe extern "system" fn get_param_value_by_string(
        this: *mut c_void,
        id: u32,
        string: *mut crate::com::Char16,
        normalized: *mut f64,
    ) -> TResult {
        if this.is_null() || string.is_null() || normalized.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison.call(|| {
            let Some(entry) = me.table().find(id) else {
                return result::INVALID_ARGUMENT;
            };
            // SAFETY: `string` is a host-owned `String128`; `read_utf16` stops at the first
            // null or at 128 code units, whichever comes first.
            let text = unsafe { strings::read_utf16(string, 128) };
            let Some(plain) = entry.parse(&text) else {
                return result::FALSE;
            };
            // SAFETY: `normalized` was checked non-null and is caller-owned.
            unsafe { *normalized = entry.curve.to_normalized(plain) };
            result::OK
        })
    }

    unsafe extern "system" fn normalized_param_to_plain(
        this: *mut c_void,
        id: u32,
        normalized: f64,
    ) -> f64 {
        if this.is_null() {
            return 0.0;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison.call_value(0.0, || {
            me.table()
                .find(id)
                .map_or(0.0, |entry| entry.curve.to_plain(normalized))
        })
    }

    unsafe extern "system" fn plain_param_to_normalized(
        this: *mut c_void,
        id: u32,
        plain: f64,
    ) -> f64 {
        if this.is_null() {
            return 0.0;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison.call_value(0.0, || {
            me.table()
                .find(id)
                .map_or(0.0, |entry| entry.curve.to_normalized(plain))
        })
    }

    unsafe extern "system" fn get_param_normalized(this: *mut c_void, id: u32) -> f64 {
        if this.is_null() {
            return 0.0;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison
            .call_value(0.0, || me.table().find(id).map_or(0.0, |e| e.normalized()))
    }

    unsafe extern "system" fn set_param_normalized(
        this: *mut c_void,
        id: u32,
        value: f64,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison.call(|| {
            let Some(entry) = me.table().find(id) else {
                return result::INVALID_ARGUMENT;
            };
            if entry.is_read_only() {
                return result::FALSE;
            }
            entry.set_normalized(value);
            // The DSP hears about it through an event at the top of the next block, which is
            // both real-time safe and the only way to reach the plug-in without aliasing the
            // audio thread's borrow of it.
            me.pending_params.store(true, Ordering::Release);
            result::OK
        })
    }

    unsafe extern "system" fn set_component_handler(
        this: *mut c_void,
        handler: *mut c_void,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison.call(|| {
            // The incoming pointer is borrowed until we retain it, and the outgoing one is
            // ours to give back. Getting this pair the wrong way round is how an adapter
            // either leaks the host's handler or frees it under the host.
            // SAFETY: `handler` is null or a live COM object the host is handing over.
            unsafe { crate::com::add_ref(handler) };
            let previous = me.handler.swap(handler);
            // SAFETY: `previous` was retained by an earlier call to this method, so this
            // gives back exactly that reference.
            unsafe { crate::com::release(previous) };
            result::OK
        })
    }

    unsafe extern "system" fn create_view(
        this: *mut c_void,
        name: crate::com::FidString,
    ) -> *mut c_void {
        if this.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: a live controller head.
        let me = unsafe { Self::from_controller(this) };
        me.poison.call_value(core::ptr::null_mut(), || {
            // VST3 defines exactly one view name; anything else is a host asking for a
            // window this plug-in does not have.
            // SAFETY: `name` is null or a null-terminated host string.
            if !name.is_null() && !unsafe { strings::c_str_eq(name, b"editor\0") } {
                return core::ptr::null_mut();
            }
            // SAFETY: `createView` is a UI-thread call.
            let ui = unsafe { me.ui() };
            if ui.editor.is_none() || !ui.view.is_null() {
                // Headless, or a host that forgot to release the previous view. Returning
                // null is the documented "no editor" answer and is safer than handing out
                // two views onto one editor.
                return core::ptr::null_mut();
            }
            // SAFETY: `me` is live, its editor is present, and no other view holds it — all
            // three checked immediately above.
            let view = unsafe { Vst3View::create(core::ptr::from_ref(me).cast_mut()) };
            ui.view = view.cast::<Vst3View>();
            view
        })
    }

    /// `[main-thread]` The editor, while a view holds it open.
    ///
    /// # Safety
    ///
    /// The caller must be the view this component handed out, on the UI thread.
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn editor(&self) -> Option<&mut Box<dyn DauxGraphic>> {
        // SAFETY: the caller is the view, which VST3 drives from the UI thread only, and the
        // view is the only thing that reaches the editor while it is open.
        let ui = unsafe { self.ui() };
        ui.editor.as_mut()
    }

    /// `[main-thread]` The editor's descriptor, or `None` for a headless plug-in.
    pub(crate) fn with_editor_descriptor(&self) -> Option<daux_plugin_api::GraphicDescriptor> {
        // SAFETY: only called from `createView` and from the view, both UI-thread-only.
        let ui = unsafe { self.ui() };
        ui.editor.as_ref().map(|editor| editor.descriptor())
    }

    /// `[main-thread]` The view currently holding the editor, or null.
    pub(crate) fn current_view(&self) -> *mut Vst3View {
        // SAFETY: only called from the UI thread.
        let ui = unsafe { self.ui() };
        ui.view
    }

    /// `[main-thread]` Gives the editor back when a view is released.
    ///
    /// # Safety
    ///
    /// The caller must be the view being released, on the UI thread.
    pub(crate) unsafe fn release_view(&self) {
        // SAFETY: see the method's contract.
        let ui = unsafe { self.ui() };
        ui.view = core::ptr::null_mut();
    }

    /// `[main-thread]` This object's host services, for the editor's context.
    pub(crate) fn services(&self) -> HostServices {
        self.host_services()
    }
}

// ---------------------------------------------------------------------------------------
// IConnectionPoint
// ---------------------------------------------------------------------------------------

impl Vst3Component {
    unsafe extern "system" fn connect(this: *mut c_void, other: *mut c_void) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // A single-component plug-in has no peer to talk to, but hosts still connect the
        // object to itself or to a proxy and refuse to load a plug-in that says no.
        if other.is_null() {
            result::INVALID_ARGUMENT
        } else {
            result::OK
        }
    }

    unsafe extern "system" fn disconnect(this: *mut c_void, _other: *mut c_void) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        result::OK
    }

    unsafe extern "system" fn notify(this: *mut c_void, message: *mut c_void) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        if message.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // Nothing sends us messages, so there is nothing to decode. Reporting "not
        // implemented" rather than success tells a host its message went nowhere.
        result::NOT_IMPLEMENTED
    }
}
