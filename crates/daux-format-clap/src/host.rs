//! The CLAP host, seen through the DAUx host-service traits.
//!
//! A `clap_host` is a table of function pointers plus a handful of optional extension
//! tables. [`ClapHostBridge`] resolves the extensions once, at instance creation, and then
//! implements every DAUx service the host actually offers. Services the host does not
//! provide stay `None` on the resulting [`HostServices`], which is what
//! `daux-host-services` is designed for: a plug-in must degrade rather than fail.
//!
//! # Lifetime and threads
//!
//! CLAP guarantees the `clap_host` pointer stays valid from `create_plugin` until the
//! plug-in is destroyed, and marks `request_restart`/`request_process`/`request_callback`
//! and `clap_host_log::log` thread-safe. Everything else is `[main-thread]`, and the bridge
//! only reaches those from main-thread entry points. That is the whole justification for
//! the `Send`/`Sync` impls below, and it is why the bridge is dropped with the instance and
//! never outlives it.
//!
//! Logging from the audio thread is allocation-free: [`RtLogRecord`] is a fixed-size value
//! and the NUL-terminated copy handed to the host is built on the stack.

use std::sync::Arc;

use daux_plugin_api::{
    HostGui, HostInfo, HostLatency, HostLog, HostParams, HostServices, HostTail, LogLevel, ParamId,
    RescanFlags, RtHost, RtHostServices, RtLogRecord, TaskId, ThreadCheck,
};

use crate::abi::{
    CLAP_EXT_GUI, CLAP_EXT_LATENCY, CLAP_EXT_LOG, CLAP_EXT_PARAMS, CLAP_EXT_TAIL,
    CLAP_EXT_THREAD_CHECK, CLAP_LOG_DEBUG, CLAP_LOG_ERROR, CLAP_LOG_FATAL, CLAP_LOG_INFO,
    CLAP_LOG_WARNING, CLAP_PARAM_RESCAN_ALL, CLAP_PARAM_RESCAN_INFO, CLAP_PARAM_RESCAN_TEXT,
    CLAP_PARAM_RESCAN_VALUES, ClapHost, ClapHostGui, ClapHostLatency, ClapHostLog, ClapHostParams,
    ClapHostTail, ClapHostThreadCheck,
};
use crate::text::{borrow_str, truncation_point};

/// The largest log message the bridge forwards, NUL excluded.
///
/// Matches [`daux_plugin_api::RT_LOG_MESSAGE_BYTES`] so an audio-thread record always fits
/// in the stack buffer without a length check that could silently drop text.
const LOG_BUFFER: usize = daux_plugin_api::RT_LOG_MESSAGE_BYTES + 1;

/// `[any-thread]` The CLAP severity for a DAUx log level.
const fn severity(level: LogLevel) -> i32 {
    match level {
        // CLAP has no `trace`; `debug` is the quietest level it offers, and mapping trace
        // to it is better than dropping the record.
        LogLevel::Trace | LogLevel::Debug => CLAP_LOG_DEBUG,
        LogLevel::Info => CLAP_LOG_INFO,
        LogLevel::Warn => CLAP_LOG_WARNING,
        LogLevel::Error => CLAP_LOG_ERROR,
        LogLevel::Fatal => CLAP_LOG_FATAL,
    }
}

/// `[main-thread]` The CLAP rescan bits for a DAUx rescan request.
const fn rescan_bits(flags: RescanFlags) -> u32 {
    let mut bits = 0;
    if flags.contains(RescanFlags::VALUES) {
        bits |= CLAP_PARAM_RESCAN_VALUES;
    }
    if flags.contains(RescanFlags::TEXT) {
        bits |= CLAP_PARAM_RESCAN_TEXT;
    }
    if flags.contains(RescanFlags::INFO) {
        bits |= CLAP_PARAM_RESCAN_INFO;
    }
    if flags.contains(RescanFlags::LIST) {
        // CLAP's `ALL` is the one that says the parameter list itself changed, and it is
        // the only bit a host is required to honour by re-reading everything.
        bits |= CLAP_PARAM_RESCAN_ALL;
    }
    bits
}

/// One CLAP host, resolved into the extensions it actually implements.
///
/// Held in an `Arc` and handed to `daux-host-services` as several trait objects at once,
/// which is why every method takes `&self`.
pub struct ClapHostBridge {
    /// The host table. Never null for a bridge built by [`ClapHostBridge::new`].
    host: *const ClapHost,
    /// `clap.log`, or null.
    log: *const ClapHostLog,
    /// `clap.params`, or null.
    params: *const ClapHostParams,
    /// `clap.gui`, or null.
    gui: *const ClapHostGui,
    /// `clap.latency`, or null.
    latency: *const ClapHostLatency,
    /// `clap.tail`, or null.
    tail: *const ClapHostTail,
    /// `clap.thread-check`, or null.
    thread_check: *const ClapHostThreadCheck,
}

impl core::fmt::Debug for ClapHostBridge {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClapHostBridge")
            .field("log", &!self.log.is_null())
            .field("params", &!self.params.is_null())
            .field("gui", &!self.gui.is_null())
            .field("latency", &!self.latency.is_null())
            .field("tail", &!self.tail.is_null())
            .field("thread_check", &!self.thread_check.is_null())
            .finish_non_exhaustive()
    }
}

// SAFETY: the only data in this struct are raw pointers into memory the host owns and keeps
// alive for the whole life of the plug-in instance (CLAP §clap_plugin_factory::create_plugin
// and §clap_plugin::destroy). The bridge never writes through them; it only calls functions
// the host published. The calls the bridge makes from a non-main thread are exactly the ones
// CLAP marks thread-safe — `request_restart`, `request_process`, `request_callback` and
// `clap_host_log::log` — and every other method is reached only from a `[main-thread]` entry
// point. Nothing here has interior mutability, so there is no state to race on.
unsafe impl Send for ClapHostBridge {}
// SAFETY: as above — shared access only ever reads immutable pointers and calls host
// functions whose thread rules the bridge respects.
unsafe impl Sync for ClapHostBridge {}

impl ClapHostBridge {
    /// `[main-thread]` Resolves every extension this bridge can use.
    ///
    /// Returns `None` for a null host, which is the one thing a conforming CLAP host never
    /// passes and a fuzzer always does.
    ///
    /// # Safety
    ///
    /// `host` must be null, or point to a `clap_host` that stays valid, with valid
    /// extensions, until the plug-in instance holding this bridge is destroyed.
    #[must_use]
    pub unsafe fn new(host: *const ClapHost) -> Option<Self> {
        // SAFETY: the caller guarantees `host` is null or a live `clap_host`; `as_ref`
        // handles the null case and produces a reference valid for this call.
        let table = unsafe { host.as_ref() }?;
        let get = table.get_extension;
        let resolve = |id: &core::ffi::CStr| -> *const core::ffi::c_void {
            match get {
                // SAFETY: `get_extension` is a host function the caller vouched for, called
                // with the same `host` pointer it belongs to and a `'static` C string. CLAP
                // marks it `[main-thread]`, and `new` runs only from `create_plugin`, which
                // is `[main-thread]`. A host that left the slot null gets no extensions
                // rather than a jump to address zero.
                Some(get) => unsafe { get(host, id.as_ptr()) },
                None => core::ptr::null(),
            }
        };
        Some(Self {
            host,
            log: resolve(CLAP_EXT_LOG).cast(),
            params: resolve(CLAP_EXT_PARAMS).cast(),
            gui: resolve(CLAP_EXT_GUI).cast(),
            latency: resolve(CLAP_EXT_LATENCY).cast(),
            tail: resolve(CLAP_EXT_TAIL).cast(),
            thread_check: resolve(CLAP_EXT_THREAD_CHECK).cast(),
        })
    }

    /// `[main-thread]` Who the host says it is.
    #[must_use]
    pub fn info(&self) -> HostInfo {
        // SAFETY: `new` guarantees `self.host` is a live `clap_host` for the instance's
        // lifetime, and its name/vendor/version are NUL-terminated strings the host owns.
        unsafe {
            let Some(table) = self.host.as_ref() else {
                return HostInfo::unknown();
            };
            let name = borrow_str(table.name).unwrap_or(HostInfo::UNKNOWN_NAME);
            let vendor = borrow_str(table.vendor).unwrap_or_default();
            let version = borrow_str(table.version).unwrap_or_default();
            HostInfo::new(name, vendor, version)
        }
    }

    /// `[main-thread]` Whether the host implements `clap.gui`.
    #[must_use]
    pub fn has_gui(&self) -> bool {
        !self.gui.is_null()
    }

    /// `[main-thread]` Assembles the full DAUx host services from this bridge.
    ///
    /// Services the host did not publish are left out entirely rather than stubbed, so a
    /// plug-in can tell "the host refused" from "the host cannot".
    #[must_use]
    pub fn services(self: &Arc<Self>) -> HostServices {
        let mut builder = HostServices::builder()
            .info(self.info())
            .rt(RtHostServices::new(Arc::clone(self) as Arc<dyn RtHost>));
        if !self.log.is_null() {
            builder = builder.log(Arc::clone(self) as Arc<dyn HostLog>);
        }
        if !self.params.is_null() {
            builder = builder.params(Arc::clone(self) as Arc<dyn HostParams>);
        }
        if !self.gui.is_null() {
            builder = builder.gui(Arc::clone(self) as Arc<dyn HostGui>);
        }
        if !self.latency.is_null() {
            builder = builder.latency(Arc::clone(self) as Arc<dyn HostLatency>);
        }
        if !self.tail.is_null() {
            builder = builder.tail(Arc::clone(self) as Arc<dyn HostTail>);
        }
        if !self.thread_check.is_null() {
            builder = builder.threads(Arc::clone(self) as Arc<dyn ThreadCheck>);
        }
        builder.build()
    }

    /// `[any-thread]` Sends an already-bounded message to `clap_host_log`.
    ///
    /// The NUL-terminated copy is built on the stack, so this is safe to call from the
    /// audio thread — which is why the buffer is sized from
    /// [`daux_plugin_api::RT_LOG_MESSAGE_BYTES`] rather than from the message.
    fn log_bounded(&self, level: LogLevel, bytes: &[u8]) {
        // SAFETY: `new` guarantees `self.log` is null or a live `clap_host_log` for the
        // instance's lifetime. CLAP marks `clap_host_log::log` thread-safe, so calling it
        // from any thread is within contract, and `buffer` outlives the call.
        unsafe {
            let Some(table) = self.log.as_ref() else {
                return;
            };
            let Some(log) = table.log else {
                return;
            };
            let mut buffer = [0u8; LOG_BUFFER];
            let len = bytes.len().min(LOG_BUFFER - 1);
            buffer[..len].copy_from_slice(&bytes[..len]);
            log(
                self.host,
                severity(level),
                buffer.as_ptr().cast::<core::ffi::c_char>(),
            );
        }
    }
}

impl HostLog for ClapHostBridge {
    fn log(&self, level: LogLevel, msg: &str) {
        // Cut on a character boundary rather than mid-sequence: a host log window that is
        // handed half a UTF-8 sequence shows a replacement glyph at best.
        let end = truncation_point(msg, LOG_BUFFER - 1);
        self.log_bounded(level, &msg.as_bytes()[..end]);
    }
}

impl RtHost for ClapHostBridge {
    fn log(&self, record: &RtLogRecord) {
        self.log_bounded(record.level, record.message_bytes());
    }

    fn request_callback(&self) {
        // SAFETY: `self.host` is a live `clap_host` for the instance's lifetime and CLAP
        // marks `request_callback` thread-safe. A host that left the slot null is ignored.
        unsafe {
            if let Some(table) = self.host.as_ref() {
                if let Some(f) = table.request_callback {
                    f(self.host);
                }
            }
        }
    }

    fn request_process(&self) {
        // SAFETY: as in `request_callback`; `request_process` is thread-safe in CLAP.
        unsafe {
            if let Some(table) = self.host.as_ref() {
                if let Some(f) = table.request_process {
                    f(self.host);
                }
            }
        }
    }

    fn request_restart(&self) {
        // SAFETY: as in `request_callback`; `request_restart` is thread-safe in CLAP.
        unsafe {
            if let Some(table) = self.host.as_ref() {
                if let Some(f) = table.request_restart {
                    f(self.host);
                }
            }
        }
    }

    fn schedule_worker(&self, _task: TaskId) -> bool {
        // CLAP has no worker-thread extension in 1.2. Refusing is the honest answer: the
        // plug-in keeps the work and retries on the main thread via `request_callback`.
        false
    }
}

impl HostParams for ClapHostBridge {
    fn gesture_begin(&self, _id: ParamId) {
        // CLAP has no host-side gesture bracket: a plug-in reports gestures as
        // `CLAP_EVENT_PARAM_GESTURE_BEGIN`/`_END` on its process output list, which the
        // adapter forwards verbatim. Nothing to do here, and inventing a call would put an
        // unmatched gesture in the host's undo history.
    }

    fn gesture_end(&self, _id: ParamId) {
        // See `gesture_begin`.
    }

    fn changed(&self, _id: ParamId, _plain: f64) {
        // A value the plug-in changed itself reaches a CLAP host the same way: as a
        // `CLAP_EVENT_PARAM_VALUE` on the output list. Asking for a rescan of every value
        // here instead would make an editor drag re-read the whole parameter list per
        // frame.
    }

    fn rescan(&self, flags: RescanFlags) {
        let bits = rescan_bits(flags);
        if bits == 0 {
            return;
        }
        // SAFETY: `new` guarantees `self.params` is null or a live `clap_host_params`.
        // `rescan` is `[main-thread]` in CLAP, and `HostParams::rescan` is `[main-thread]`
        // in DAUx, so the calling thread is right by construction.
        unsafe {
            if let Some(table) = self.params.as_ref() {
                if let Some(rescan) = table.rescan {
                    rescan(self.host, bits);
                }
            }
        }
    }
}

impl HostLatency for ClapHostBridge {
    fn set_samples(&self, _samples: u32) {
        // CLAP pulls rather than pushes: the host re-reads `clap_plugin_latency::get` after
        // being told the value moved, so the sample count is not passed here.
        // SAFETY: `new` guarantees `self.latency` is null or a live `clap_host_latency`,
        // and `changed` is `[main-thread]`, matching `HostLatency::set_samples`.
        unsafe {
            if let Some(table) = self.latency.as_ref() {
                if let Some(changed) = table.changed {
                    changed(self.host);
                }
            }
        }
    }
}

impl HostTail for ClapHostBridge {
    fn changed(&self) {
        // SAFETY: `new` guarantees `self.tail` is null or a live `clap_host_tail`.
        unsafe {
            if let Some(table) = self.tail.as_ref() {
                if let Some(changed) = table.changed {
                    changed(self.host);
                }
            }
        }
    }
}

impl HostGui for ClapHostBridge {
    fn request_resize(&self, w: u32, h: u32) -> bool {
        // SAFETY: `new` guarantees `self.gui` is null or a live `clap_host_gui`; every
        // method on it is `[main-thread]`, matching `HostGui`.
        unsafe {
            let Some(table) = self.gui.as_ref() else {
                return false;
            };
            table.request_resize.is_some_and(|f| f(self.host, w, h))
        }
    }

    fn request_show(&self) -> bool {
        // SAFETY: as in `request_resize`.
        unsafe {
            let Some(table) = self.gui.as_ref() else {
                return false;
            };
            table.request_show.is_some_and(|f| f(self.host))
        }
    }

    fn request_hide(&self) -> bool {
        // SAFETY: as in `request_resize`.
        unsafe {
            let Some(table) = self.gui.as_ref() else {
                return false;
            };
            table.request_hide.is_some_and(|f| f(self.host))
        }
    }

    fn closed(&self, destroyed: bool) {
        // SAFETY: as in `request_resize`.
        unsafe {
            if let Some(table) = self.gui.as_ref() {
                if let Some(closed) = table.closed {
                    closed(self.host, destroyed);
                }
            }
        }
    }
}

impl ThreadCheck for ClapHostBridge {
    fn is_main_thread(&self) -> bool {
        // SAFETY: `new` guarantees `self.thread_check` is null or a live
        // `clap_host_thread_check`, whose methods CLAP marks thread-safe.
        unsafe {
            let Some(table) = self.thread_check.as_ref() else {
                return false;
            };
            table.is_main_thread.is_some_and(|f| f(self.host))
        }
    }

    fn is_audio_thread(&self) -> bool {
        // SAFETY: as in `is_main_thread`.
        unsafe {
            let Some(table) = self.thread_check.as_ref() else {
                return false;
            };
            table.is_audio_thread.is_some_and(|f| f(self.host))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::ClapVersion;
    use core::cell::RefCell;
    use core::ffi::{CStr, c_char, c_void};
    use core::ptr;

    /// What the fake host was asked to do.
    #[derive(Default)]
    struct Calls {
        /// `(severity, message)` per log call.
        logs: Vec<(i32, String)>,
        /// Rescan bit sets, in order.
        rescans: Vec<u32>,
        /// How many restarts, processes and callbacks were requested.
        restarts: u32,
        /// Requested process resumptions.
        processes: u32,
        /// Requested main-thread callbacks.
        callbacks: u32,
        /// Latency-changed notifications.
        latency_changed: u32,
        /// Tail-changed notifications.
        tail_changed: u32,
        /// Requested window sizes.
        resizes: Vec<(u32, u32)>,
        /// `closed(destroyed)` arguments.
        closed: Vec<bool>,
    }

    /// A `clap_host` wired to a [`Calls`] recorder, with every extension present.
    struct FakeHost {
        /// The recorder the callbacks write into.
        calls: Box<RefCell<Calls>>,
        /// The host table itself.
        host: Box<ClapHost>,
        /// Extension tables, kept alive alongside the host.
        log: Box<ClapHostLog>,
        /// `clap.params`.
        params: Box<ClapHostParams>,
        /// `clap.gui`.
        gui: Box<ClapHostGui>,
        /// `clap.latency`.
        latency: Box<ClapHostLatency>,
        /// `clap.tail`.
        tail: Box<ClapHostTail>,
        /// `clap.thread-check`.
        thread_check: Box<ClapHostThreadCheck>,
        /// Which extension ids `get_extension` answers to.
        offer: &'static [&'static CStr],
    }

    thread_local! {
        /// The host the extern callbacks below reach. One per thread, which is all the
        /// tests need and keeps `host_data` free for the recorder pointer.
        static ACTIVE: RefCell<Option<*const FakeHost>> = const { RefCell::new(None) };
    }

    /// Reads the recorder out of a host pointer.
    ///
    /// # Safety
    ///
    /// `host` must be a live `clap_host` whose `host_data` is a `RefCell<Calls>`.
    unsafe fn calls<'a>(host: *const ClapHost) -> &'a RefCell<Calls> {
        // SAFETY: every callback is only ever reached through a `FakeHost` that set
        // `host_data` to its own live recorder.
        unsafe { &*(*host).host_data.cast::<RefCell<Calls>>() }
    }

    unsafe extern "C" fn get_extension(host: *const ClapHost, id: *const c_char) -> *const c_void {
        // SAFETY: `id` is a NUL-terminated string CLAP passes in, and the active host lives
        // for the whole test.
        let id = unsafe { CStr::from_ptr(id) };
        let fake = ACTIVE.with_borrow(|a| (*a).expect("a fake host is active"));
        // SAFETY: `ACTIVE` holds a pointer to a `FakeHost` that outlives every call.
        let fake = unsafe { &*fake };
        let _ = host;
        if !fake.offer.contains(&id) {
            return ptr::null();
        }
        if id == CLAP_EXT_LOG {
            ptr::from_ref(fake.log.as_ref()).cast()
        } else if id == CLAP_EXT_PARAMS {
            ptr::from_ref(fake.params.as_ref()).cast()
        } else if id == CLAP_EXT_GUI {
            ptr::from_ref(fake.gui.as_ref()).cast()
        } else if id == CLAP_EXT_LATENCY {
            ptr::from_ref(fake.latency.as_ref()).cast()
        } else if id == CLAP_EXT_TAIL {
            ptr::from_ref(fake.tail.as_ref()).cast()
        } else if id == CLAP_EXT_THREAD_CHECK {
            ptr::from_ref(fake.thread_check.as_ref()).cast()
        } else {
            ptr::null()
        }
    }

    unsafe extern "C" fn do_log(host: *const ClapHost, sev: i32, msg: *const c_char) {
        // SAFETY: the bridge always passes a live NUL-terminated buffer.
        let text = unsafe { CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: `host` is the fake host, whose `host_data` is its recorder.
        unsafe { calls(host) }.borrow_mut().logs.push((sev, text));
    }

    unsafe extern "C" fn do_rescan(host: *const ClapHost, flags: u32) {
        // SAFETY: as in `do_log`.
        unsafe { calls(host) }.borrow_mut().rescans.push(flags);
    }

    unsafe extern "C" fn do_restart(host: *const ClapHost) {
        // SAFETY: as in `do_log`.
        unsafe { calls(host) }.borrow_mut().restarts += 1;
    }

    unsafe extern "C" fn do_process(host: *const ClapHost) {
        // SAFETY: as in `do_log`.
        unsafe { calls(host) }.borrow_mut().processes += 1;
    }

    unsafe extern "C" fn do_callback(host: *const ClapHost) {
        // SAFETY: as in `do_log`.
        unsafe { calls(host) }.borrow_mut().callbacks += 1;
    }

    unsafe extern "C" fn do_latency_changed(host: *const ClapHost) {
        // SAFETY: as in `do_log`.
        unsafe { calls(host) }.borrow_mut().latency_changed += 1;
    }

    unsafe extern "C" fn do_tail_changed(host: *const ClapHost) {
        // SAFETY: as in `do_log`.
        unsafe { calls(host) }.borrow_mut().tail_changed += 1;
    }

    unsafe extern "C" fn do_resize(host: *const ClapHost, w: u32, h: u32) -> bool {
        // SAFETY: as in `do_log`.
        unsafe { calls(host) }.borrow_mut().resizes.push((w, h));
        true
    }

    unsafe extern "C" fn do_show(_host: *const ClapHost) -> bool {
        true
    }

    unsafe extern "C" fn do_closed(host: *const ClapHost, destroyed: bool) {
        // SAFETY: as in `do_log`.
        unsafe { calls(host) }.borrow_mut().closed.push(destroyed);
    }

    unsafe extern "C" fn yes(_host: *const ClapHost) -> bool {
        true
    }

    unsafe extern "C" fn no(_host: *const ClapHost) -> bool {
        false
    }

    const ALL_EXTENSIONS: &[&CStr] = &[
        CLAP_EXT_LOG,
        CLAP_EXT_PARAMS,
        CLAP_EXT_GUI,
        CLAP_EXT_LATENCY,
        CLAP_EXT_TAIL,
        CLAP_EXT_THREAD_CHECK,
    ];

    impl FakeHost {
        fn new(offer: &'static [&'static CStr]) -> Box<Self> {
            let mut calls = Box::new(RefCell::new(Calls::default()));
            let host_data = ptr::from_mut(calls.as_mut()).cast();
            let fake = Box::new(Self {
                calls,
                host: Box::new(ClapHost {
                    clap_version: ClapVersion::CURRENT,
                    host_data,
                    name: c"Fake DAW".as_ptr(),
                    vendor: c"Example".as_ptr(),
                    url: c"https://example.com".as_ptr(),
                    version: c"1.0".as_ptr(),
                    get_extension: Some(get_extension),
                    request_restart: Some(do_restart),
                    request_process: Some(do_process),
                    request_callback: Some(do_callback),
                }),
                log: Box::new(ClapHostLog { log: Some(do_log) }),
                params: Box::new(ClapHostParams {
                    rescan: Some(do_rescan),
                    clear: None,
                    request_flush: None,
                }),
                gui: Box::new(ClapHostGui {
                    resize_hints_changed: None,
                    request_resize: Some(do_resize),
                    request_show: Some(do_show),
                    request_hide: Some(no),
                    closed: Some(do_closed),
                }),
                latency: Box::new(ClapHostLatency {
                    changed: Some(do_latency_changed),
                }),
                tail: Box::new(ClapHostTail {
                    changed: Some(do_tail_changed),
                }),
                thread_check: Box::new(ClapHostThreadCheck {
                    is_main_thread: Some(yes),
                    is_audio_thread: Some(no),
                }),
                offer,
            });
            ACTIVE.with_borrow_mut(|a| *a = Some(ptr::from_ref(fake.as_ref())));
            fake
        }

        fn bridge(&self) -> Arc<ClapHostBridge> {
            // SAFETY: `self.host` lives as long as `self`, which outlives the bridge in
            // every test below.
            Arc::new(
                unsafe { ClapHostBridge::new(ptr::from_ref(self.host.as_ref())) }
                    .expect("a non-null host produces a bridge"),
            )
        }
    }

    impl Drop for FakeHost {
        fn drop(&mut self) {
            ACTIVE.with_borrow_mut(|a| *a = None);
        }
    }

    #[test]
    fn a_null_host_produces_no_bridge_at_all() {
        // SAFETY: null is explicitly allowed by `new`.
        assert!(unsafe { ClapHostBridge::new(ptr::null()) }.is_none());
    }

    #[test]
    fn the_hosts_identity_reaches_the_plugin() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        let info = bridge.info();
        assert_eq!(info.name, "Fake DAW");
        assert_eq!(info.vendor, "Example");
        assert_eq!(info.version, "1.0");
    }

    #[test]
    fn only_the_extensions_the_host_offers_become_services() {
        let fake = FakeHost::new(&[CLAP_EXT_LOG]);
        let bridge = fake.bridge();
        let services = bridge.services();
        assert!(services.has_log());
        assert!(services.params().is_none());
        assert!(services.gui().is_none());
        assert!(services.latency().is_none());
        assert!(services.tail().is_none());
        assert!(services.threads().is_none());

        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        let services = bridge.services();
        assert!(services.params().is_some());
        assert!(services.gui().is_some());
        assert!(services.latency().is_some());
        assert!(services.tail().is_some());
        assert!(services.threads().is_some());
    }

    #[test]
    fn a_host_with_no_extensions_at_all_still_yields_working_services() {
        let fake = FakeHost::new(&[]);
        let bridge = fake.bridge();
        let services = bridge.services();
        // The null log fallback must still be callable rather than a panic.
        services.log().log(LogLevel::Warn, "nobody is listening");
        assert!(!services.has_log());
        // …and the RT half is always present, because CLAP always has the request_* trio.
        services.rt().request_callback();
        assert_eq!(fake.calls.borrow().callbacks, 1);
    }

    #[test]
    fn log_levels_map_onto_clap_severities() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        for (level, expected) in [
            (LogLevel::Trace, CLAP_LOG_DEBUG),
            (LogLevel::Debug, CLAP_LOG_DEBUG),
            (LogLevel::Info, CLAP_LOG_INFO),
            (LogLevel::Warn, CLAP_LOG_WARNING),
            (LogLevel::Error, CLAP_LOG_ERROR),
            (LogLevel::Fatal, CLAP_LOG_FATAL),
        ] {
            HostLog::log(bridge.as_ref(), level, "message");
            let calls = fake.calls.borrow();
            let (severity, text) = calls.logs.last().expect("a log call");
            assert_eq!(*severity, expected, "{level:?}");
            assert_eq!(text, "message");
        }
    }

    #[test]
    fn an_audio_thread_log_record_is_forwarded_whole_and_nul_terminated() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        let record = RtLogRecord::new(LogLevel::Error, "voice pool exhausted");
        RtHost::log(bridge.as_ref(), &record);
        let calls = fake.calls.borrow();
        assert_eq!(
            calls.logs.last().expect("a log call"),
            &(CLAP_LOG_ERROR, "voice pool exhausted".to_owned())
        );
    }

    #[test]
    fn a_maximum_length_record_is_not_truncated_and_does_not_run_off_the_buffer() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        let long = "x".repeat(daux_plugin_api::RT_LOG_MESSAGE_BYTES);
        let record = RtLogRecord::new(LogLevel::Info, &long);
        RtHost::log(bridge.as_ref(), &record);
        let calls = fake.calls.borrow();
        assert_eq!(calls.logs.last().expect("a log call").1.len(), long.len());
    }

    #[test]
    fn a_message_longer_than_the_stack_buffer_is_cut_rather_than_overflowing_it() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        let long = "y".repeat(4096);
        HostLog::log(bridge.as_ref(), LogLevel::Info, &long);
        let calls = fake.calls.borrow();
        assert_eq!(
            calls.logs.last().expect("a log call").1.len(),
            daux_plugin_api::RT_LOG_MESSAGE_BYTES
        );
    }

    #[test]
    fn the_request_trio_reaches_the_host() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        RtHost::request_restart(bridge.as_ref());
        RtHost::request_process(bridge.as_ref());
        RtHost::request_callback(bridge.as_ref());
        RtHost::request_callback(bridge.as_ref());
        let calls = fake.calls.borrow();
        assert_eq!(calls.restarts, 1);
        assert_eq!(calls.processes, 1);
        assert_eq!(calls.callbacks, 2);
        assert!(
            !RtHost::schedule_worker(bridge.as_ref(), TaskId(1)),
            "CLAP 1.2 has no worker extension, so scheduling must refuse rather than lie"
        );
    }

    #[test]
    fn rescan_flags_map_onto_clap_bits_and_an_empty_set_is_not_forwarded() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        HostParams::rescan(bridge.as_ref(), RescanFlags::NONE);
        assert!(
            fake.calls.borrow().rescans.is_empty(),
            "an empty rescan must not wake the host"
        );

        HostParams::rescan(bridge.as_ref(), RescanFlags::VALUES);
        HostParams::rescan(bridge.as_ref(), RescanFlags::TEXT);
        HostParams::rescan(bridge.as_ref(), RescanFlags::INFO);
        HostParams::rescan(bridge.as_ref(), RescanFlags::LIST);
        HostParams::rescan(bridge.as_ref(), RescanFlags::ALL);
        assert_eq!(
            fake.calls.borrow().rescans,
            [
                CLAP_PARAM_RESCAN_VALUES,
                CLAP_PARAM_RESCAN_TEXT,
                CLAP_PARAM_RESCAN_INFO,
                CLAP_PARAM_RESCAN_ALL,
                CLAP_PARAM_RESCAN_VALUES
                    | CLAP_PARAM_RESCAN_TEXT
                    | CLAP_PARAM_RESCAN_INFO
                    | CLAP_PARAM_RESCAN_ALL,
            ]
        );
    }

    #[test]
    fn latency_and_tail_notifications_are_pulls_not_pushes() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        HostLatency::set_samples(bridge.as_ref(), 512);
        HostTail::changed(bridge.as_ref());
        let calls = fake.calls.borrow();
        assert_eq!(calls.latency_changed, 1);
        assert_eq!(calls.tail_changed, 1);
    }

    #[test]
    fn gui_requests_are_forwarded_and_a_refusal_is_reported_honestly() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        assert!(HostGui::request_resize(bridge.as_ref(), 800, 600));
        assert!(HostGui::request_show(bridge.as_ref()));
        assert!(
            !HostGui::request_hide(bridge.as_ref()),
            "the fake host refuses hide, and a refusal must not be reported as success"
        );
        HostGui::closed(bridge.as_ref(), true);
        let calls = fake.calls.borrow();
        assert_eq!(calls.resizes, [(800, 600)]);
        assert_eq!(calls.closed, [true]);
    }

    #[test]
    fn a_host_that_leaves_slots_null_is_tolerated_rather_than_jumped_to() {
        let mut host = ClapHost {
            clap_version: ClapVersion::CURRENT,
            host_data: ptr::null_mut(),
            name: ptr::null(),
            vendor: ptr::null(),
            url: ptr::null(),
            version: ptr::null(),
            get_extension: None,
            request_restart: None,
            request_process: None,
            request_callback: None,
        };
        // SAFETY: `host` lives for the rest of the test and every slot is null, which the
        // bridge explicitly tolerates.
        let bridge = unsafe { ClapHostBridge::new(ptr::from_mut(&mut host)) }
            .expect("a non-null host with null slots still produces a bridge");
        // None of these may jump through a null pointer.
        HostLog::log(&bridge, LogLevel::Error, "nowhere to go");
        RtHost::request_restart(&bridge);
        RtHost::request_process(&bridge);
        RtHost::request_callback(&bridge);
        HostParams::rescan(&bridge, RescanFlags::ALL);
        HostLatency::set_samples(&bridge, 1);
        HostTail::changed(&bridge);
        assert!(!HostGui::request_resize(&bridge, 1, 1));
        assert!(!HostGui::request_show(&bridge));
        HostGui::closed(&bridge, false);
        assert!(!ThreadCheck::is_main_thread(&bridge));
        assert!(!ThreadCheck::is_audio_thread(&bridge));
        assert_eq!(bridge.info().name, HostInfo::UNKNOWN_NAME);
        assert!(!bridge.has_gui());
    }

    #[test]
    fn thread_check_answers_come_from_the_host() {
        let fake = FakeHost::new(ALL_EXTENSIONS);
        let bridge = fake.bridge();
        assert!(ThreadCheck::is_main_thread(bridge.as_ref()));
        assert!(!ThreadCheck::is_audio_thread(bridge.as_ref()));
    }
}
