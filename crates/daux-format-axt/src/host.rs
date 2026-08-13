//! Turning the host's `DauxHostV1` into the [`HostServices`] a plug-in understands
//! (abi-v1 §11.6).
//!
//! The translation is one object, [`RawHost`], implementing every host trait `daux-core` knows
//! about, shared behind a single `Arc`. A service the host did not advertise is simply absent
//! from the built [`HostServices`], which is exactly the `None` a well-written plug-in already
//! has to handle.

use std::sync::Arc;

use daux_abi::{
    DauxHostApiV1, DauxHostGuiApiV1, DauxHostHandle, DauxHostLogApiV1, DauxHostParamsApiV1,
    DauxHostV1, DauxHostWorkerApiV1, DauxStrView, daux_bool, daux_bool_is_true, ext,
};
use daux_plugin_api::daux_rt::RtLogRecord;
use daux_plugin_api::{
    HostGui, HostInfo, HostLog, HostParams, HostServices, HostWorker, LogLevel, ParamId,
    RescanFlags, RtHost, RtHostServices, TaskId, ThreadCheck,
};

/// The host's interface pair plus the extension tables it advertised, all resolved once.
///
/// Resolving `get_extension` once at construction rather than per call matters: abi-v1 §11.6
/// only promises the lookup is "cheap and lock-free", and the real-time path must not gamble on
/// a host's definition of cheap.
#[derive(Clone, Copy)]
struct RawHost {
    handle: DauxHostHandle,
    api: *const DauxHostApiV1,
    log: *const DauxHostLogApiV1,
    params: *const DauxHostParamsApiV1,
    worker: *const DauxHostWorkerApiV1,
    gui: *const DauxHostGuiApiV1,
}

// SAFETY: every pointer here belongs to the host module, not to this one, and is only ever
// read. abi-v1 §16.1 requires the host interface and its extension tables to stay valid until
// `destroy_factory` returns, and §11.6 marks the entries this type calls from the audio thread
// (`log`, `request_*`, `schedule`) `[any-thread]`, i.e. safe to invoke concurrently. Function
// tables are immutable for their whole lifetime, so sharing this value across threads hands out
// nothing but immutable reads of host-owned memory.
unsafe impl Send for RawHost {}
// SAFETY: as `Send` above.
unsafe impl Sync for RawHost {}

impl RawHost {
    /// Resolves the host's extension tables. `[main-thread]`
    ///
    /// # Safety
    ///
    /// `host` is null or a valid [`DauxHostV1`] whose table and handle outlive the factory.
    unsafe fn from_abi(host: *const DauxHostV1) -> Option<Self> {
        if host.is_null() {
            return None;
        }
        // SAFETY: the caller guarantees `host` points at a live, aligned `DauxHostV1` for the
        // duration of this call and beyond; it is only read.
        let host = unsafe { &*host };
        // SAFETY: same guarantee, extended to the table the pair names. `api()` returns `None`
        // for a null table, which is the one thing a malformed host can produce here.
        let api = unsafe { host.api() }?;
        if !api.is_v1_0_compatible() {
            // A table too short to contain the v1.0 fields cannot be called at all (abi-v1 §3).
            return None;
        }
        let mut raw = Self {
            handle: host.handle,
            api: host.api,
            log: core::ptr::null(),
            params: core::ptr::null(),
            worker: core::ptr::null(),
            gui: core::ptr::null(),
        };
        // SAFETY: `get_extension` is a non-optional entry of a table already validated as
        // v1.0-sized, and the ids are `'static` UTF-8 borrowed only for the call.
        unsafe {
            raw.log = (api.get_extension)(raw.handle, DauxStrView::from_str(ext::HOST_LOG)).cast();
            raw.params =
                (api.get_extension)(raw.handle, DauxStrView::from_str(ext::HOST_PARAMS)).cast();
            raw.worker =
                (api.get_extension)(raw.handle, DauxStrView::from_str(ext::HOST_WORKER)).cast();
            raw.gui = (api.get_extension)(raw.handle, DauxStrView::from_str(ext::HOST_GUI)).cast();
        }
        Some(raw)
    }

    /// The root table, which construction proved non-null and v1.0-sized.
    #[inline]
    fn api(&self) -> &DauxHostApiV1 {
        // SAFETY: `from_abi` is the only constructor and it returns `None` unless `api` names a
        // live, v1.0-sized table; the host keeps it alive and immutable for as long as the
        // factory exists (abi-v1 §16.1).
        unsafe { &*self.api }
    }

    /// The host's identity, copied out of the fixed buffers it published. `[main-thread]`
    fn info(&self) -> HostInfo {
        let api = self.api();
        HostInfo::new(
            api.name.as_str(),
            api.vendor.as_str(),
            format!(
                "{}.{}.{}",
                api.version.major, api.version.minor, api.version.patch
            ),
        )
    }
}

impl HostLog for RawHost {
    fn log(&self, level: LogLevel, msg: &str) {
        if self.log.is_null() {
            return;
        }
        // SAFETY: `self.log` is non-null and was obtained from the host's own `get_extension`
        // for `daux.host.log/1`, so it names a `DauxHostLogApiV1` the host owns and keeps alive.
        // `msg` is borrowed only for the duration of the call, which is all a `DauxStrView`
        // promises (abi-v1 §2).
        unsafe {
            let table = &*self.log;
            (table.log)(self.handle, level.as_u32(), DauxStrView::from_str(msg));
        }
    }
}

impl HostParams for RawHost {
    fn gesture_begin(&self, id: ParamId) {
        if let Some(table) = self.params_table() {
            // SAFETY: see `params_table`; `gesture_begin` is a non-optional v1.0 entry.
            unsafe { (table.gesture_begin)(self.handle, id.get()) }
        }
    }

    fn gesture_end(&self, id: ParamId) {
        if let Some(table) = self.params_table() {
            // SAFETY: see `params_table`.
            unsafe { (table.gesture_end)(self.handle, id.get()) }
        }
    }

    fn changed(&self, id: ParamId, plain: f64) {
        if let Some(table) = self.params_table() {
            // SAFETY: see `params_table`. The value crosses the ABI plain, never normalised
            // (abi-v1 §11.2).
            unsafe { (table.changed)(self.handle, id.get(), plain) }
        }
    }

    fn rescan(&self, flags: RescanFlags) {
        if let Some(table) = self.params_table() {
            // SAFETY: see `params_table`.
            unsafe { (table.rescan)(self.handle, flags.bits()) }
        }
    }
}

impl HostWorker for RawHost {
    fn schedule(&self, task: TaskId) -> bool {
        if self.worker.is_null() {
            return false;
        }
        // SAFETY: `self.worker` came from the host's `get_extension` for
        // `daux.host.worker/1`, so it names a live `DauxHostWorkerApiV1`; `schedule` is
        // documented `[any-thread]` and non-blocking (abi-v1 §11.6).
        unsafe {
            let table = &*self.worker;
            daux_bool_is_true((table.schedule)(self.handle, task.get()))
        }
    }
}

impl HostGui for RawHost {
    fn request_resize(&self, w: u32, h: u32) -> bool {
        match self.gui_table() {
            // SAFETY: see `gui_table`.
            Some(table) => unsafe { daux_bool_is_true((table.request_resize)(self.handle, w, h)) },
            None => false,
        }
    }

    fn request_show(&self) -> bool {
        match self.gui_table() {
            // SAFETY: see `gui_table`.
            Some(table) => unsafe { daux_bool_is_true((table.request_show)(self.handle)) },
            None => false,
        }
    }

    fn request_hide(&self) -> bool {
        match self.gui_table() {
            // SAFETY: see `gui_table`.
            Some(table) => unsafe { daux_bool_is_true((table.request_hide)(self.handle)) },
            None => false,
        }
    }

    fn closed(&self, destroyed: bool) {
        if let Some(table) = self.gui_table() {
            // SAFETY: see `gui_table`.
            unsafe { (table.closed)(self.handle, daux_bool(destroyed)) }
        }
    }
}

impl ThreadCheck for RawHost {
    fn is_main_thread(&self) -> bool {
        match self.api().is_main_thread {
            // SAFETY: an `Option<unsafe extern "C" fn(..)>` that is `Some` is a callable entry
            // of the host's own table (abi-v1 §2.3); the handle is the one it gave us.
            Some(f) => unsafe { daux_bool_is_true(f(self.handle)) },
            None => false,
        }
    }

    fn is_audio_thread(&self) -> bool {
        match self.api().is_audio_thread {
            // SAFETY: as `is_main_thread` above.
            Some(f) => unsafe { daux_bool_is_true(f(self.handle)) },
            None => false,
        }
    }
}

impl RtHost for RawHost {
    fn log(&self, record: &RtLogRecord) {
        HostLog::log(self, record.level, record.message());
    }

    fn request_callback(&self) {
        // SAFETY: `request_callback` is a non-optional v1.0 entry of a table `from_abi`
        // validated, and abi-v1 §11.6 marks it `[any-thread]` and real-time safe.
        unsafe { (self.api().request_callback)(self.handle) }
    }

    fn request_process(&self) {
        // SAFETY: as `request_callback`.
        unsafe { (self.api().request_process)(self.handle) }
    }

    fn request_restart(&self) {
        // SAFETY: as `request_callback`.
        unsafe { (self.api().request_restart)(self.handle) }
    }

    fn schedule_worker(&self, task: TaskId) -> bool {
        HostWorker::schedule(self, task)
    }
}

impl RawHost {
    /// The `daux.host.params/1` table, or `None` when the host has none.
    ///
    /// # Safety of the callers
    ///
    /// A returned reference names a live, host-owned `DauxHostParamsApiV1`: the pointer came
    /// from the host's own `get_extension` and abi-v1 §2.3 makes function tables immutable and
    /// valid for as long as the producing module is loaded.
    #[inline]
    fn params_table(&self) -> Option<&DauxHostParamsApiV1> {
        if self.params.is_null() {
            return None;
        }
        // SAFETY: non-null was just checked, and the pointer is the host's own table.
        Some(unsafe { &*self.params })
    }

    /// The `daux.host.gui/1` table, or `None`. See [`RawHost::params_table`].
    #[inline]
    fn gui_table(&self) -> Option<&DauxHostGuiApiV1> {
        if self.gui.is_null() {
            return None;
        }
        // SAFETY: non-null was just checked, and the pointer is the host's own table.
        Some(unsafe { &*self.gui })
    }
}

/// Everything this module keeps about the host that created it.
///
/// One per factory, cloned into every instance. Cloning is one atomic increment per service.
#[derive(Clone)]
pub(crate) struct HostBridge {
    services: HostServices,
}

impl HostBridge {
    /// [main-thread] Builds the bridge from the host interface `create_factory` was given.
    ///
    /// A null or malformed `host` yields [`HostServices::null`], which is a complete, inert
    /// host — abi-v1 §18 requires a plug-in to survive one, and this is where that becomes
    /// true for every plug-in written against this SDK.
    ///
    /// # Safety
    ///
    /// `host` is null or points at a valid [`DauxHostV1`] whose table and handle stay valid
    /// until `destroy_factory` returns (abi-v1 §16.1).
    pub(crate) unsafe fn from_abi(host: *const DauxHostV1) -> Self {
        // SAFETY: forwarded verbatim to `RawHost::from_abi`, which has the same contract.
        let Some(raw) = (unsafe { RawHost::from_abi(host) }) else {
            return Self {
                services: HostServices::null(),
            };
        };
        let info = raw.info();
        let shared = Arc::new(raw);
        let mut builder = HostServices::builder()
            .info(info)
            .rt_host(shared.clone())
            .threads(shared.clone());
        if !raw.log.is_null() {
            builder = builder.log(shared.clone());
        }
        if !raw.params.is_null() {
            builder = builder.params(shared.clone());
        }
        if !raw.worker.is_null() {
            builder = builder.worker(shared.clone());
        }
        if !raw.gui.is_null() {
            builder = builder.gui(shared.clone());
        }
        Self {
            services: builder.build(),
        }
    }

    /// [main-thread] The services to hand a controller through `set_host`.
    pub(crate) fn services(&self) -> &HostServices {
        &self.services
    }

    /// [audio-thread] The real-time-safe subset to hand `process`.
    pub(crate) fn rt(&self) -> &RtHostServices {
        self.services.rt()
    }
}
