//! The host side of the ABI: Rust host services seen through a `DauxHostV1`.
//!
//! A plug-in module never sees [`HostServices`]. It sees a `#[repr(C)]` function table and
//! an opaque handle, and every call it makes arrives here, in a function that must not
//! unwind and must not assume the plug-in passed anything sensible.

use core::ffi::c_void;
use core::panic::AssertUnwindSafe;
use std::sync::Arc;

use daux_abi::{
    DAUX_ABI_VERSION_MAJOR, DAUX_ABI_VERSION_MINOR, DAUX_FALSE, DAUX_TRUE, DauxBool, DauxHostApiV1,
    DauxHostGuiApiV1, DauxHostHandle, DauxHostLogApiV1, DauxHostParamsApiV1, DauxHostV1,
    DauxHostWorkerApiV1, DauxName, DauxStrView, DauxVersion, ext,
};
use daux_host_services::{HostServices, LogLevel, ParamId, RescanFlags, TaskId};

/// Everything a plug-in module may reach in the host, in the shape ABI v1 requires.
/// [main-thread] to build; the tables it publishes are callable from the threads their
/// `abi-v1` §11.6 documentation names.
///
/// # Lifetime
///
/// `abi-v1` §4 says the `DauxHostV1` handed to `create_factory` must stay valid until
/// `destroy_factory` returns, so a bridge is moved into the
/// [`LoadedFactory`](crate::LoadedFactory) it is used to build and dropped with it. The
/// state the plug-in's handle points at lives in an `Arc`, and the interface value itself
/// in a `Box`, so neither moves when the bridge does.
///
/// # What is published
///
/// `daux.host.log/1` is always available — a plug-in should never have to branch on whether
/// it can report a problem, and a host without a logger gets the inert one from
/// [`HostServices::null`]. Every other extension is published only when the host actually
/// implements the corresponding service, because `abi-v1` §11 requires an unknown or
/// unsupported id to return null rather than a table whose calls do nothing.
///
/// ```
/// use daux_host_services::HostServices;
/// use daux_runtime::HostBridge;
///
/// let bridge = HostBridge::new(HostServices::null());
/// // A host with no parameter service does not advertise `daux.host.params/1`.
/// assert!(bridge.extension(daux_abi::ext::HOST_PARAMS).is_null());
/// // Logging is always there.
/// assert!(!bridge.extension(daux_abi::ext::HOST_LOG).is_null());
/// ```
#[derive(Debug)]
pub struct HostBridge {
    /// The plug-in's handle points into this allocation, so it must not move.
    inner: Arc<HostBridgeInner>,
    /// Boxed because `create_factory` takes `*const DauxHostV1` and keeps it.
    interface: Box<DauxHostV1>,
}

/// The state a `DauxHostHandle` names.
#[derive(Debug)]
struct HostBridgeInner {
    services: HostServices,
    /// Inside the `Arc` so that its address is stable for as long as the handle is.
    api: DauxHostApiV1,
}

// SAFETY: the two raw pointers in `interface` address `inner`'s allocation and a `static`
// respectively, so they stay valid wherever the `HostBridge` is moved to. `HostBridgeInner`
// is `Send + Sync` because `HostServices` is and `DauxHostApiV1` is a table of function
// pointers and plain data that is never mutated after construction. Moving the bridge
// between threads therefore moves no thread-affine state.
unsafe impl Send for HostBridge {}
// SAFETY: as above; `&HostBridge` only ever hands out the same immutable pointers, and
// every callback reachable through them goes to a `Send + Sync` service.
unsafe impl Sync for HostBridge {}

impl HostBridge {
    /// Builds the ABI view of `services`. [main-thread]
    #[must_use]
    pub fn new(services: HostServices) -> Self {
        let info = services.info();
        let api = DauxHostApiV1 {
            size: DauxHostApiV1::SIZE,
            abi_version_major: DAUX_ABI_VERSION_MAJOR,
            abi_version_minor: DAUX_ABI_VERSION_MINOR,
            _pad0: 0,
            name: DauxName::new(&info.name),
            vendor: DauxName::new(&info.vendor),
            version: DauxVersion::ZERO,
            get_extension: host_get_extension,
            request_restart: host_request_restart,
            request_process: host_request_process,
            request_callback: host_request_callback,
            // `abi-v1` §11.6 marks both optional. Publishing a function that always
            // answers "no" would be worse than publishing nothing: a plug-in cannot tell
            // "not on the main thread" from "this host cannot say".
            is_main_thread: services
                .threads()
                .map(|_| host_is_main_thread as unsafe extern "C" fn(DauxHostHandle) -> DauxBool),
            is_audio_thread: services
                .threads()
                .map(|_| host_is_audio_thread as unsafe extern "C" fn(DauxHostHandle) -> DauxBool),
            reserved: [0; 8],
        };

        let inner = Arc::new(HostBridgeInner { services, api });
        let interface = Box::new(DauxHostV1::new(
            DauxHostHandle::from_ptr(Arc::as_ptr(&inner).cast_mut().cast::<c_void>()),
            &inner.api,
        ));
        Self { inner, interface }
    }

    /// The Rust services behind the bridge. [main-thread]
    #[inline]
    #[must_use]
    pub fn services(&self) -> &HostServices {
        &self.inner.services
    }

    /// The interface a module's `create_factory` is handed. [main-thread]
    ///
    /// Valid for as long as this `HostBridge` is alive, which is why
    /// [`LoadedFactory`](crate::LoadedFactory) owns the bridge rather than borrowing it.
    #[inline]
    #[must_use]
    pub fn as_raw(&self) -> *const DauxHostV1 {
        &raw const *self.interface
    }

    /// The table published for `id`, or null when this host does not implement it.
    /// [any-thread]
    ///
    /// The same answer `get_extension` gives a plug-in; exposed so a host can assert what
    /// it advertises without going through the module.
    #[must_use]
    pub fn extension(&self, id: &str) -> *const c_void {
        lookup_extension(&self.inner, id)
    }
}

/// The extension tables. They hold only function pointers, so one `static` serves every
/// bridge in the process; the per-host state travels in the handle.
static HOST_LOG_API: DauxHostLogApiV1 = DauxHostLogApiV1 {
    size: DauxHostLogApiV1::SIZE,
    _pad0: 0,
    log: host_log,
    reserved: [0; 2],
};

static HOST_PARAMS_API: DauxHostParamsApiV1 = DauxHostParamsApiV1 {
    size: DauxHostParamsApiV1::SIZE,
    _pad0: 0,
    changed: host_param_changed,
    gesture_begin: host_gesture_begin,
    gesture_end: host_gesture_end,
    rescan: host_rescan,
    reserved: [0; 4],
};

static HOST_WORKER_API: DauxHostWorkerApiV1 = DauxHostWorkerApiV1 {
    size: DauxHostWorkerApiV1::SIZE,
    _pad0: 0,
    schedule: host_schedule,
    reserved: [0; 2],
};

static HOST_GUI_API: DauxHostGuiApiV1 = DauxHostGuiApiV1 {
    size: DauxHostGuiApiV1::SIZE,
    _pad0: 0,
    request_resize: host_request_resize,
    request_show: host_request_show,
    request_hide: host_request_hide,
    closed: host_editor_closed,
    reserved: [0; 4],
};

/// Resolves an extension id against what this host actually implements.
fn lookup_extension(inner: &HostBridgeInner, id: &str) -> *const c_void {
    match id {
        // Always present: `HostServices::log` falls back to a no-op logger.
        ext::HOST_LOG => (&raw const HOST_LOG_API).cast::<c_void>(),
        ext::HOST_PARAMS if inner.services.params().is_some() => {
            (&raw const HOST_PARAMS_API).cast::<c_void>()
        }
        ext::HOST_WORKER if inner.services.worker().is_some() => {
            (&raw const HOST_WORKER_API).cast::<c_void>()
        }
        ext::HOST_GUI if inner.services.gui().is_some() => {
            (&raw const HOST_GUI_API).cast::<c_void>()
        }
        // `daux.host.latency/1`, `daux.host.tail/1` and `daux.host.timer/1` are named by
        // §11 but define no table in v1.0, and anything else is unknown. Both answer null,
        // which §11 requires: "Unknown ids MUST return null rather than fail."
        _ => core::ptr::null(),
    }
}

/// Recovers the bridge state a handle names.
///
/// # Safety
///
/// `handle` must be null or a handle this crate produced from a live `HostBridge` whose
/// `Arc` allocation is still alive — which `abi-v1` §16.1 guarantees, because a plug-in
/// must not retain the host pointer after `destroy_factory` returns and the bridge outlives
/// that call. The returned reference is shared and `HostBridgeInner` is never mutated after
/// construction, so concurrent calls from several plug-in threads cannot race.
#[inline]
unsafe fn bridge<'a>(handle: DauxHostHandle) -> Option<&'a HostBridgeInner> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: forwarded verbatim from this function's own contract.
    Some(unsafe { &*handle.as_ptr().cast_const().cast::<HostBridgeInner>() })
}

/// Runs a host callback so that a panic becomes `fallback` instead of unwinding into the
/// plug-in module, which is undefined behaviour (`abi-v1` §17).
#[inline]
fn guarded<R>(fallback: R, body: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(_) => fallback,
    }
}

unsafe extern "C" fn host_get_extension(handle: DauxHostHandle, id: DauxStrView) -> *const c_void {
    guarded(core::ptr::null(), || {
        // SAFETY: `handle` is the one this crate put in the interface the module was given.
        let Some(inner) = (unsafe { bridge(handle) }) else {
            return core::ptr::null();
        };
        // SAFETY: `abi-v1` §2 requires an argument `DauxStrView` to be valid for the
        // duration of the call; the borrow ends before this function returns. A malformed
        // view (null pointer with a non-zero length) or non-UTF-8 bytes yield `None`, which
        // simply matches no extension.
        let Some(id) = (unsafe { id.as_str() }) else {
            return core::ptr::null();
        };
        lookup_extension(inner, id)
    })
}

unsafe extern "C" fn host_request_restart(handle: DauxHostHandle) {
    guarded((), || {
        // SAFETY: as in `host_get_extension`.
        if let Some(inner) = unsafe { bridge(handle) } {
            inner.services.rt().request_restart();
        }
    });
}

unsafe extern "C" fn host_request_process(handle: DauxHostHandle) {
    guarded((), || {
        // SAFETY: as in `host_get_extension`.
        if let Some(inner) = unsafe { bridge(handle) } {
            inner.services.rt().request_process();
        }
    });
}

unsafe extern "C" fn host_request_callback(handle: DauxHostHandle) {
    guarded((), || {
        // SAFETY: as in `host_get_extension`.
        if let Some(inner) = unsafe { bridge(handle) } {
            inner.services.rt().request_callback();
        }
    });
}

unsafe extern "C" fn host_is_main_thread(handle: DauxHostHandle) -> DauxBool {
    guarded(DAUX_FALSE, || {
        // SAFETY: as in `host_get_extension`.
        let answer = unsafe { bridge(handle) }
            .and_then(|inner| inner.services.threads())
            .is_some_and(daux_host_services::ThreadCheck::is_main_thread);
        if answer { DAUX_TRUE } else { DAUX_FALSE }
    })
}

unsafe extern "C" fn host_is_audio_thread(handle: DauxHostHandle) -> DauxBool {
    guarded(DAUX_FALSE, || {
        // SAFETY: as in `host_get_extension`.
        let answer = unsafe { bridge(handle) }
            .and_then(|inner| inner.services.threads())
            .is_some_and(daux_host_services::ThreadCheck::is_audio_thread);
        if answer { DAUX_TRUE } else { DAUX_FALSE }
    })
}

unsafe extern "C" fn host_log(handle: DauxHostHandle, level: u32, msg: DauxStrView) {
    guarded((), || {
        // SAFETY: as in `host_get_extension`.
        let Some(inner) = (unsafe { bridge(handle) }) else {
            return;
        };
        // SAFETY: an argument `DauxStrView` is valid for the duration of the call
        // (`abi-v1` §2). Invalid UTF-8 from a buggy module must not be a reason to drop the
        // record, so it is reported as an empty message rather than discarded.
        let text = unsafe { msg.as_str() }.unwrap_or("");
        // A level this build does not know is more likely to be important than not.
        let level = LogLevel::from_u32(level).unwrap_or(LogLevel::Error);
        inner.services.log().log(level, text);
    });
}

unsafe extern "C" fn host_param_changed(handle: DauxHostHandle, id: u32, value: f64) {
    guarded((), || {
        // SAFETY: as in `host_get_extension`.
        if let Some(params) = unsafe { bridge(handle) }.and_then(|i| i.services.params()) {
            params.changed(ParamId(id), value);
        }
    });
}

unsafe extern "C" fn host_gesture_begin(handle: DauxHostHandle, id: u32) {
    guarded((), || {
        // SAFETY: as in `host_get_extension`.
        if let Some(params) = unsafe { bridge(handle) }.and_then(|i| i.services.params()) {
            params.gesture_begin(ParamId(id));
        }
    });
}

unsafe extern "C" fn host_gesture_end(handle: DauxHostHandle, id: u32) {
    guarded((), || {
        // SAFETY: as in `host_get_extension`.
        if let Some(params) = unsafe { bridge(handle) }.and_then(|i| i.services.params()) {
            params.gesture_end(ParamId(id));
        }
    });
}

unsafe extern "C" fn host_rescan(handle: DauxHostHandle, flags: u32) {
    guarded((), || {
        // SAFETY: as in `host_get_extension`.
        if let Some(params) = unsafe { bridge(handle) }.and_then(|i| i.services.params()) {
            // ABI v1.0 defines no `DAUX_PARAM_RESCAN_*` constants, so a plug-in passes 0
            // and a host treats any non-zero value as "rescan everything".
            let flags = if flags == 0 {
                RescanFlags::ALL
            } else {
                RescanFlags::from_bits(flags)
            };
            params.rescan(flags);
        }
    });
}

unsafe extern "C" fn host_schedule(handle: DauxHostHandle, task_id: u64) -> DauxBool {
    guarded(DAUX_FALSE, || {
        // SAFETY: as in `host_get_extension`.
        let scheduled = unsafe { bridge(handle) }
            .and_then(|i| i.services.worker())
            .is_some_and(|worker| worker.schedule(TaskId(task_id)));
        if scheduled { DAUX_TRUE } else { DAUX_FALSE }
    })
}

unsafe extern "C" fn host_request_resize(handle: DauxHostHandle, w: u32, h_px: u32) -> DauxBool {
    guarded(DAUX_FALSE, || {
        // SAFETY: as in `host_get_extension`.
        let granted = unsafe { bridge(handle) }
            .and_then(|i| i.services.gui())
            .is_some_and(|gui| gui.request_resize(w, h_px));
        if granted { DAUX_TRUE } else { DAUX_FALSE }
    })
}

unsafe extern "C" fn host_request_show(handle: DauxHostHandle) -> DauxBool {
    guarded(DAUX_FALSE, || {
        // SAFETY: as in `host_get_extension`.
        let granted = unsafe { bridge(handle) }
            .and_then(|i| i.services.gui())
            .is_some_and(daux_host_services::HostGui::request_show);
        if granted { DAUX_TRUE } else { DAUX_FALSE }
    })
}

unsafe extern "C" fn host_request_hide(handle: DauxHostHandle) -> DauxBool {
    guarded(DAUX_FALSE, || {
        // SAFETY: as in `host_get_extension`.
        let granted = unsafe { bridge(handle) }
            .and_then(|i| i.services.gui())
            .is_some_and(daux_host_services::HostGui::request_hide);
        if granted { DAUX_TRUE } else { DAUX_FALSE }
    })
}

unsafe extern "C" fn host_editor_closed(handle: DauxHostHandle, was_destroyed: DauxBool) {
    guarded((), || {
        // SAFETY: as in `host_get_extension`.
        if let Some(gui) = unsafe { bridge(handle) }.and_then(|i| i.services.gui()) {
            // `abi-v1` §2: a consumer treats any non-zero value as true.
            gui.closed(was_destroyed != DAUX_FALSE);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_host_services::{HostGui, HostInfo, HostLog, HostParams, HostWorker, ThreadCheck};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Recorder {
        logs: Mutex<Vec<(LogLevel, String)>>,
        gestures: Mutex<Vec<(u32, bool)>>,
        changes: Mutex<Vec<(u32, f64)>>,
        rescans: Mutex<Vec<RescanFlags>>,
        resizes: Mutex<Vec<(u32, u32)>>,
        closed: AtomicUsize,
        scheduled: Mutex<Vec<u64>>,
        worker_accepts: AtomicBool,
        main_thread: AtomicBool,
    }

    impl HostLog for Recorder {
        fn log(&self, level: LogLevel, msg: &str) {
            self.logs.lock().unwrap().push((level, msg.to_owned()));
        }
    }

    impl HostParams for Recorder {
        fn gesture_begin(&self, id: ParamId) {
            self.gestures.lock().unwrap().push((id.0, true));
        }
        fn gesture_end(&self, id: ParamId) {
            self.gestures.lock().unwrap().push((id.0, false));
        }
        fn changed(&self, id: ParamId, plain: f64) {
            self.changes.lock().unwrap().push((id.0, plain));
        }
        fn rescan(&self, flags: RescanFlags) {
            self.rescans.lock().unwrap().push(flags);
        }
    }

    impl HostWorker for Recorder {
        fn schedule(&self, task: TaskId) -> bool {
            self.scheduled.lock().unwrap().push(task.0);
            self.worker_accepts.load(Ordering::Relaxed)
        }
    }

    impl HostGui for Recorder {
        fn request_resize(&self, w: u32, h: u32) -> bool {
            self.resizes.lock().unwrap().push((w, h));
            true
        }
        fn request_show(&self) -> bool {
            true
        }
        fn request_hide(&self) -> bool {
            true
        }
        fn closed(&self, destroyed: bool) {
            self.closed
                .fetch_add(if destroyed { 2 } else { 1 }, Ordering::Relaxed);
        }
    }

    impl ThreadCheck for Recorder {
        fn is_main_thread(&self) -> bool {
            self.main_thread.load(Ordering::Relaxed)
        }
        fn is_audio_thread(&self) -> bool {
            !self.main_thread.load(Ordering::Relaxed)
        }
    }

    fn full_host() -> (Arc<Recorder>, HostBridge) {
        let recorder = Arc::new(Recorder::default());
        recorder.worker_accepts.store(true, Ordering::Relaxed);
        let services = HostServices::builder()
            .info(HostInfo::new("Harness", "Futureboard", "0.1"))
            .log(recorder.clone())
            .params(recorder.clone())
            .worker(recorder.clone())
            .gui(recorder.clone())
            .threads(recorder.clone())
            .build();
        (recorder, HostBridge::new(services))
    }

    /// The table a module reads out of `DauxHostV1` must be the one the bridge owns, and
    /// the handle must address the bridge's own state.
    #[test]
    fn the_interface_points_at_the_bridges_own_state() {
        let (_, bridge) = full_host();
        let raw = bridge.as_raw();
        // SAFETY: `raw` is the boxed interface owned by `bridge`, which is alive here.
        let interface = unsafe { &*raw };
        assert!(!interface.handle.is_null());
        // SAFETY: the table pointer addresses the `DauxHostApiV1` inside the bridge's `Arc`.
        let api = unsafe { interface.api() }.expect("a bridge always publishes its table");
        assert_eq!(api.abi_version_major, DAUX_ABI_VERSION_MAJOR);
        assert_eq!(api.size, DauxHostApiV1::SIZE);
        assert_eq!(api.name.as_str(), "Harness");
        assert_eq!(api.vendor.as_str(), "Futureboard");
        assert!(api.is_main_thread.is_some());
    }

    /// Moving the bridge must not move the state its handle names — that is the whole
    /// reason the state lives in an `Arc` and the interface in a `Box`.
    #[test]
    fn the_handle_survives_the_bridge_being_moved() {
        let (recorder, bridge) = full_host();
        let before = {
            // SAFETY: the interface is owned by `bridge`, alive in this scope.
            unsafe { *bridge.as_raw() }.handle.as_ptr()
        };
        let moved = Box::new(bridge);
        let after = {
            // SAFETY: as above, through the moved bridge.
            unsafe { *moved.as_raw() }.handle.as_ptr()
        };
        assert_eq!(before, after, "the handle must be move-stable");

        // And it still reaches the services.
        let handle = DauxHostHandle::from_ptr(after);
        // SAFETY: `handle` came from a bridge that is still alive.
        unsafe { host_log(handle, 3, DauxStrView::from_str("after the move")) };
        assert_eq!(
            recorder.logs.lock().unwrap().as_slice(),
            [(LogLevel::Warn, "after the move".to_owned())]
        );
    }

    #[test]
    fn extensions_are_published_only_when_the_service_exists() {
        let (_, full) = full_host();
        assert!(!full.extension(ext::HOST_LOG).is_null());
        assert!(!full.extension(ext::HOST_PARAMS).is_null());
        assert!(!full.extension(ext::HOST_WORKER).is_null());
        assert!(!full.extension(ext::HOST_GUI).is_null());

        let bare = HostBridge::new(HostServices::null());
        assert!(
            !bare.extension(ext::HOST_LOG).is_null(),
            "logging is always available"
        );
        assert!(bare.extension(ext::HOST_PARAMS).is_null());
        assert!(bare.extension(ext::HOST_WORKER).is_null());
        assert!(bare.extension(ext::HOST_GUI).is_null());
    }

    /// `abi-v1` §11: unknown ids return null rather than failing, and the ids that are
    /// named but have no v1.0 table are among them.
    #[test]
    fn unknown_and_tableless_extension_ids_return_null() {
        let (_, bridge) = full_host();
        for id in [
            ext::HOST_LATENCY,
            ext::HOST_TAIL,
            ext::HOST_TIMER,
            ext::AUDIO_PORTS,
            "daux.host.log/2",
            "daux.host.log",
            "",
            "com.example.made-up/1",
        ] {
            assert!(
                bridge.extension(id).is_null(),
                "`{id}` must not resolve to a table"
            );
        }
    }

    /// The lookup a plug-in actually performs, including the hostile shapes a
    /// `DauxStrView` can take.
    #[test]
    fn get_extension_tolerates_malformed_string_views() {
        let (_, bridge) = full_host();
        // SAFETY: `bridge` is alive, so its interface and handle are valid.
        let handle = unsafe { *bridge.as_raw() }.handle;

        // SAFETY: a well-formed view over a `&'static str`.
        let good = unsafe { host_get_extension(handle, DauxStrView::from_str(ext::HOST_LOG)) };
        assert!(!good.is_null());

        // A null pointer with a non-zero length is malformed and must match nothing.
        let malformed = DauxStrView {
            ptr: core::ptr::null(),
            len: 7,
        };
        // SAFETY: `as_str` is documented to return `None` for exactly this shape without
        // dereferencing the pointer.
        assert!(unsafe { host_get_extension(handle, malformed) }.is_null());

        // Invalid UTF-8 must match nothing rather than panic.
        let bytes = [0xffu8, 0xfe, 0x00];
        let invalid = DauxStrView {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        };
        // SAFETY: the view describes three initialised bytes that outlive the call.
        assert!(unsafe { host_get_extension(handle, invalid) }.is_null());

        // A null handle is not a handle this bridge produced; the callback must survive it.
        // SAFETY: `bridge` explicitly documents null as a value the callbacks handle without
        // dereferencing it.
        let answer = unsafe {
            host_get_extension(DauxHostHandle::null(), DauxStrView::from_str(ext::HOST_LOG))
        };
        assert!(answer.is_null());
    }

    /// Every callback must be a no-op — never a crash — when the module passes a handle
    /// the bridge did not produce.
    #[test]
    fn a_null_handle_makes_every_callback_inert() {
        let null = DauxHostHandle::null();
        // SAFETY: every one of these callbacks documents null as a handled value; none of
        // them dereferences it.
        unsafe {
            host_request_restart(null);
            host_request_process(null);
            host_request_callback(null);
            host_log(null, 2, DauxStrView::from_str("nobody"));
            host_param_changed(null, 1, 0.5);
            host_gesture_begin(null, 1);
            host_gesture_end(null, 1);
            host_rescan(null, 0);
            assert_eq!(host_schedule(null, 4), DAUX_FALSE);
            assert_eq!(host_request_resize(null, 1, 1), DAUX_FALSE);
            assert_eq!(host_request_show(null), DAUX_FALSE);
            assert_eq!(host_request_hide(null), DAUX_FALSE);
            host_editor_closed(null, DAUX_TRUE);
            assert_eq!(host_is_main_thread(null), DAUX_FALSE);
            assert_eq!(host_is_audio_thread(null), DAUX_FALSE);
        }
    }

    #[test]
    fn parameter_callbacks_reach_the_service() {
        let (recorder, bridge) = full_host();
        // SAFETY: `bridge` is alive for the whole test.
        let handle = unsafe { *bridge.as_raw() }.handle;
        // SAFETY: `handle` is this bridge's own handle.
        unsafe {
            host_gesture_begin(handle, 7);
            host_param_changed(handle, 7, -6.0);
            host_gesture_end(handle, 7);
            host_rescan(handle, 0);
            host_rescan(handle, RescanFlags::VALUES.bits());
        }
        assert_eq!(*recorder.gestures.lock().unwrap(), [(7, true), (7, false)]);
        assert_eq!(*recorder.changes.lock().unwrap(), [(7, -6.0)]);
        assert_eq!(
            *recorder.rescans.lock().unwrap(),
            [RescanFlags::ALL, RescanFlags::VALUES],
            "flags 0 means `rescan everything` in ABI v1.0"
        );
    }

    #[test]
    fn gui_and_worker_callbacks_carry_their_answers_back() {
        let (recorder, bridge) = full_host();
        // SAFETY: `bridge` is alive for the whole test.
        let handle = unsafe { *bridge.as_raw() }.handle;
        // SAFETY: `handle` is this bridge's own handle.
        unsafe {
            assert_eq!(host_request_resize(handle, 800, 600), DAUX_TRUE);
            assert_eq!(host_request_show(handle), DAUX_TRUE);
            assert_eq!(host_request_hide(handle), DAUX_TRUE);
            host_editor_closed(handle, DAUX_TRUE);
            // §2: any non-zero value is true, not just 1.
            host_editor_closed(handle, 0);
            assert_eq!(host_schedule(handle, 42), DAUX_TRUE);
            recorder.worker_accepts.store(false, Ordering::Relaxed);
            assert_eq!(
                host_schedule(handle, 43),
                DAUX_FALSE,
                "a full worker queue must be reported, not hidden"
            );
        }
        assert_eq!(*recorder.resizes.lock().unwrap(), [(800, 600)]);
        assert_eq!(recorder.closed.load(Ordering::Relaxed), 3);
        assert_eq!(*recorder.scheduled.lock().unwrap(), [42, 43]);
    }

    #[test]
    fn an_unknown_log_level_is_not_silently_downgraded() {
        let (recorder, bridge) = full_host();
        // SAFETY: `bridge` is alive for the whole test.
        let handle = unsafe { *bridge.as_raw() }.handle;
        // SAFETY: `handle` is this bridge's own handle; the views borrow live literals.
        unsafe {
            host_log(handle, 0, DauxStrView::from_str("trace"));
            host_log(handle, 5, DauxStrView::from_str("fatal"));
            host_log(handle, 99, DauxStrView::from_str("from the future"));
        }
        let logs = recorder.logs.lock().unwrap();
        assert_eq!(logs[0].0, LogLevel::Trace);
        assert_eq!(logs[1].0, LogLevel::Fatal);
        assert_eq!(
            logs[2].0,
            LogLevel::Error,
            "a level this build does not know is more likely important than not"
        );
    }

    /// A host that cannot answer thread questions must leave the entries null, so the
    /// plug-in can tell "no" from "cannot say".
    #[test]
    fn thread_checks_are_absent_when_the_host_has_no_answer() {
        let bridge = HostBridge::new(HostServices::null());
        // SAFETY: `bridge` is alive; the table is the one it owns.
        let api = unsafe { (*bridge.as_raw()).api() }.expect("table");
        assert!(api.is_main_thread.is_none());
        assert!(api.is_audio_thread.is_none());

        let (recorder, full) = full_host();
        recorder.main_thread.store(true, Ordering::Relaxed);
        // SAFETY: `full` is alive for the whole test.
        let handle = unsafe { *full.as_raw() }.handle;
        // SAFETY: `handle` is that bridge's own handle.
        unsafe {
            assert_eq!(host_is_main_thread(handle), DAUX_TRUE);
            assert_eq!(host_is_audio_thread(handle), DAUX_FALSE);
        }
    }

    /// A panic in a host service must be converted at the boundary, because unwinding
    /// into the plug-in module is undefined behaviour (`abi-v1` §17).
    #[test]
    fn a_panicking_service_does_not_unwind_into_the_module() {
        struct Exploding;
        impl HostLog for Exploding {
            fn log(&self, _: LogLevel, _: &str) {
                panic!("the host's logger is broken");
            }
        }
        impl HostGui for Exploding {
            fn request_resize(&self, _: u32, _: u32) -> bool {
                panic!("the host's window manager is broken");
            }
            fn request_show(&self) -> bool {
                true
            }
            fn closed(&self, _: bool) {}
        }

        let exploding = Arc::new(Exploding);
        let services = HostServices::builder()
            .log(exploding.clone())
            .gui(exploding)
            .build();
        let bridge = HostBridge::new(services);
        // SAFETY: `bridge` is alive for the whole test.
        let handle = unsafe { *bridge.as_raw() }.handle;

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        // SAFETY: `handle` is this bridge's own handle; both calls hit a service that
        // panics, and the callbacks must absorb it.
        let resized = unsafe {
            host_log(handle, 2, DauxStrView::from_str("boom"));
            host_request_resize(handle, 10, 10)
        };
        std::panic::set_hook(previous);
        assert_eq!(
            resized, DAUX_FALSE,
            "a panicking service must report failure, not unwind"
        );
    }

    #[test]
    fn the_bridge_crosses_threads() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HostBridge>();
    }
}
