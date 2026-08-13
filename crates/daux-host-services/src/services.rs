//! The main-thread aggregate: every service the host offers, in one value.

use core::fmt;
use std::sync::Arc;

use crate::{
    HostGui, HostInfo, HostLatency, HostLog, HostParams, HostResources, HostTail, HostTimer,
    HostWorker, NullHostLog, RtHost, RtHostServices, ThreadCheck,
};

/// The no-op logger handed out when the host provides none. A `static` rather
/// than a fresh value so the accessor can return a borrow without storing one.
static NULL_LOG: NullHostLog = NullHostLog;

/// Everything a plug-in may reach on a non-real-time thread. `[main-thread]`
///
/// A `HostServices` is built once when the plug-in is created and handed to the
/// controller through `set_host`. It is cheap to clone (one atomic increment per
/// service present), `Send + Sync`, and `'static`, so an editor may keep its own
/// copy for as long as it lives.
///
/// # Optional means optional
///
/// Only [`log`](HostServices::log) is guaranteed; every other accessor returns
/// `Option`, and `None` is not an error condition — it is the ordinary state of
/// affairs in a host that does not implement that extension. A plug-in that
/// unwraps one of these will die in the wild. Degrade instead: no
/// [`HostParams`] means automation is one-way, no [`HostTimer`] means the editor
/// repaints on its own events, no [`HostResources`] means bundled assets are
/// unavailable and the built-in defaults have to do.
///
/// # Threads
///
/// Everything reachable from here may block. That is precisely why the audio
/// thread receives [`RtHostServices`] instead — see [`rt`](HostServices::rt).
///
/// ```
/// use daux_host_services::{HostInfo, HostLog, HostServices};
/// use daux_rt::LogLevel;
/// use std::sync::{Arc, Mutex};
///
/// #[derive(Default)]
/// struct Collect(Mutex<Vec<String>>);
/// impl HostLog for Collect {
///     fn log(&self, _level: LogLevel, msg: &str) {
///         self.0.lock().unwrap().push(msg.to_owned());
///     }
/// }
///
/// let sink = Arc::new(Collect::default());
/// let host = HostServices::builder()
///     .info(HostInfo::new("Reaper", "Cockos", "7.19"))
///     .log(sink.clone())
///     .build();
///
/// host.log().log(LogLevel::Info, "loaded");
/// assert_eq!(*sink.0.lock().unwrap(), ["loaded"]);
/// assert_eq!(host.info().name, "Reaper");
/// assert!(host.params().is_none(), "this host offers no automation service");
///
/// // Nothing at all, for tests and offline rendering.
/// let none = HostServices::null();
/// none.log().log(LogLevel::Info, "goes nowhere, and that is fine");
/// ```
#[derive(Clone, Default)]
pub struct HostServices {
    info: Arc<HostInfo>,
    log: Option<Arc<dyn HostLog>>,
    params: Option<Arc<dyn HostParams>>,
    latency: Option<Arc<dyn HostLatency>>,
    tail: Option<Arc<dyn HostTail>>,
    worker: Option<Arc<dyn HostWorker>>,
    gui: Option<Arc<dyn HostGui>>,
    timer: Option<Arc<dyn HostTimer>>,
    resources: Option<Arc<dyn HostResources>>,
    threads: Option<Arc<dyn ThreadCheck>>,
    rt: RtHostServices,
}

impl HostServices {
    /// Starts building an instance from individual service implementations.
    /// `[main-thread]`
    #[must_use]
    pub fn builder() -> HostServicesBuilder {
        HostServicesBuilder::new()
    }

    /// A host that provides nothing. `[main-thread]`
    ///
    /// Every optional accessor is `None`, [`log`](HostServices::log) discards,
    /// and [`rt`](HostServices::rt) is [`RtHostServices::null`]. This is what
    /// unit tests, offline rendering and unhosted previews use, and running
    /// against it is the cheapest way to prove a plug-in really does degrade
    /// gracefully.
    #[must_use]
    pub fn null() -> Self {
        Self::default()
    }

    /// Who the host is. `[main-thread]`
    #[inline]
    #[must_use]
    pub fn info(&self) -> &HostInfo {
        &self.info
    }

    /// Structured logging — always available. `[any-thread]`
    ///
    /// Falls back to [`NullHostLog`] when the host offers none, so a plug-in
    /// never has to branch on the presence of a logger.
    #[inline]
    #[must_use]
    pub fn log(&self) -> &dyn HostLog {
        match &self.log {
            Some(log) => log.as_ref(),
            None => &NULL_LOG as &dyn HostLog,
        }
    }

    /// `true` when the host actually provides a logger, as opposed to the
    /// no-op fallback. `[main-thread]`
    #[inline]
    #[must_use]
    pub fn has_log(&self) -> bool {
        self.log.is_some()
    }

    /// Automation gestures and parameter rescans. `[main-thread]`
    #[inline]
    #[must_use]
    pub fn params(&self) -> Option<&dyn HostParams> {
        self.params.as_deref()
    }

    /// Latency change notification. `[main-thread]`
    #[inline]
    #[must_use]
    pub fn latency(&self) -> Option<&dyn HostLatency> {
        self.latency.as_deref()
    }

    /// Tail change notification. `[main-thread]`
    #[inline]
    #[must_use]
    pub fn tail(&self) -> Option<&dyn HostTail> {
        self.tail.as_deref()
    }

    /// Off-thread work scheduling. `[main-thread]`
    ///
    /// The audio thread reaches the same facility through
    /// [`RtHostServices::schedule_worker`].
    #[inline]
    #[must_use]
    pub fn worker(&self) -> Option<&dyn HostWorker> {
        self.worker.as_deref()
    }

    /// Editor window negotiation. `[main-thread]`
    #[inline]
    #[must_use]
    pub fn gui(&self) -> Option<&dyn HostGui> {
        self.gui.as_deref()
    }

    /// Periodic main-thread callbacks. `[main-thread]`
    #[inline]
    #[must_use]
    pub fn timer(&self) -> Option<&dyn HostTimer> {
        self.timer.as_deref()
    }

    /// Bundle-relative resource access. `[main-thread]`
    #[inline]
    #[must_use]
    pub fn resources(&self) -> Option<&dyn HostResources> {
        self.resources.as_deref()
    }

    /// Thread identification. `[any-thread]`
    #[inline]
    #[must_use]
    pub fn threads(&self) -> Option<&dyn ThreadCheck> {
        self.threads.as_deref()
    }

    /// The real-time-safe subset to hand to `process`. `[any-thread]`
    ///
    /// Handing this out is a one-way door: nothing on [`RtHostServices`] leads
    /// back to the blocking services above.
    #[inline]
    #[must_use]
    pub fn rt(&self) -> &RtHostServices {
        &self.rt
    }

    /// `true` when the caller is on the host's main thread, or `None` when the
    /// host cannot tell. `[any-thread]`
    ///
    /// Deliberately tri-state: "the host says no" and "there is no way to ask"
    /// must not collapse into the same answer, because assertions built on the
    /// second one fire in every host that does not implement the check.
    #[inline]
    #[must_use]
    pub fn is_main_thread(&self) -> Option<bool> {
        self.threads().map(ThreadCheck::is_main_thread)
    }

    /// `true` when the caller is on an audio thread, or `None` when the host
    /// cannot tell. `[any-thread]`
    #[inline]
    #[must_use]
    pub fn is_audio_thread(&self) -> Option<bool> {
        self.threads().map(ThreadCheck::is_audio_thread)
    }
}

impl fmt::Debug for HostServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        /// Trait objects are not `Debug`; what matters here is which services
        /// exist at all.
        fn present<T: ?Sized>(slot: &Option<Arc<T>>) -> &'static str {
            if slot.is_some() { "yes" } else { "no" }
        }

        f.debug_struct("HostServices")
            .field("info", &*self.info)
            .field("log", &present(&self.log))
            .field("params", &present(&self.params))
            .field("latency", &present(&self.latency))
            .field("tail", &present(&self.tail))
            .field("worker", &present(&self.worker))
            .field("gui", &present(&self.gui))
            .field("timer", &present(&self.timer))
            .field("resources", &present(&self.resources))
            .field("threads", &present(&self.threads))
            .field("rt", &self.rt)
            .finish()
    }
}

/// Assembles a [`HostServices`] from the pieces a host actually implements.
/// `[main-thread]`
///
/// Every setter takes an `Arc`, so one object that implements several traits is
/// registered once per role and shared:
///
/// ```
/// # use daux_host_services::*;
/// # use daux_rt::LogLevel;
/// # use daux_parameter::ParamId;
/// # use std::sync::Arc;
/// struct Bridge;
/// impl HostLog for Bridge { fn log(&self, _: LogLevel, _: &str) {} }
/// impl HostLatency for Bridge { fn set_samples(&self, _: u32) {} }
///
/// let bridge = Arc::new(Bridge);
/// let host = HostServices::builder()
///     .log(bridge.clone())
///     .latency(bridge.clone())
///     .build();
/// assert!(host.has_log() && host.latency().is_some());
/// ```
#[derive(Default)]
pub struct HostServicesBuilder {
    services: HostServices,
}

impl HostServicesBuilder {
    /// A builder with no services and an unknown host identity.
    /// `[main-thread]`
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the host's identity. `[main-thread]`
    #[must_use]
    pub fn info(mut self, info: HostInfo) -> Self {
        self.services.info = Arc::new(info);
        self
    }

    /// Installs the logging service. `[main-thread]`
    #[must_use]
    pub fn log(mut self, log: Arc<dyn HostLog>) -> Self {
        self.services.log = Some(log);
        self
    }

    /// Installs the automation service. `[main-thread]`
    #[must_use]
    pub fn params(mut self, params: Arc<dyn HostParams>) -> Self {
        self.services.params = Some(params);
        self
    }

    /// Installs latency change notification. `[main-thread]`
    #[must_use]
    pub fn latency(mut self, latency: Arc<dyn HostLatency>) -> Self {
        self.services.latency = Some(latency);
        self
    }

    /// Installs tail change notification. `[main-thread]`
    #[must_use]
    pub fn tail(mut self, tail: Arc<dyn HostTail>) -> Self {
        self.services.tail = Some(tail);
        self
    }

    /// Installs off-thread work scheduling. `[main-thread]`
    #[must_use]
    pub fn worker(mut self, worker: Arc<dyn HostWorker>) -> Self {
        self.services.worker = Some(worker);
        self
    }

    /// Installs editor window negotiation. `[main-thread]`
    #[must_use]
    pub fn gui(mut self, gui: Arc<dyn HostGui>) -> Self {
        self.services.gui = Some(gui);
        self
    }

    /// Installs periodic main-thread callbacks. `[main-thread]`
    #[must_use]
    pub fn timer(mut self, timer: Arc<dyn HostTimer>) -> Self {
        self.services.timer = Some(timer);
        self
    }

    /// Installs bundle-relative resource access. `[main-thread]`
    #[must_use]
    pub fn resources(mut self, resources: Arc<dyn HostResources>) -> Self {
        self.services.resources = Some(resources);
        self
    }

    /// Installs thread identification. `[main-thread]`
    ///
    /// [`RtThreadCheck`](crate::RtThreadCheck) is a reasonable stand-in for a
    /// host that has no check of its own but does label its threads.
    #[must_use]
    pub fn threads(mut self, threads: Arc<dyn ThreadCheck>) -> Self {
        self.services.threads = Some(threads);
        self
    }

    /// Installs the audio-thread callbacks. `[main-thread]`
    ///
    /// Note the deliberate asymmetry: the real-time host is a separate object
    /// with its own, much stricter contract. Registering the same type for both
    /// roles is fine as long as its [`RtHost`] half honours that contract.
    #[must_use]
    pub fn rt_host(mut self, rt: Arc<dyn RtHost>) -> Self {
        self.services.rt = RtHostServices::new(rt);
        self
    }

    /// Installs an already-built real-time subset. `[main-thread]`
    #[must_use]
    pub fn rt(mut self, rt: RtHostServices) -> Self {
        self.services.rt = rt;
        self
    }

    /// Finishes the aggregate. `[main-thread]`
    #[must_use]
    pub fn build(self) -> HostServices {
        self.services
    }
}

impl fmt::Debug for HostServicesBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostServicesBuilder")
            .field("services", &self.services)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RescanFlags, RtThreadCheck, TaskId, TimerId};
    use daux_parameter::ParamId;
    use daux_rt::{LogLevel, RtLogRecord};
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One object playing every host role, as a real adapter does.
    #[derive(Default)]
    struct Everything {
        lines: Mutex<Vec<String>>,
        latency: AtomicUsize,
        tails: AtomicUsize,
        tasks: AtomicUsize,
        closed: AtomicUsize,
        timers: AtomicUsize,
        rt_callbacks: AtomicUsize,
    }

    impl HostLog for Everything {
        fn log(&self, level: LogLevel, msg: &str) {
            self.lines.lock().unwrap().push(format!("{level}:{msg}"));
        }
    }
    impl HostParams for Everything {
        fn gesture_begin(&self, id: ParamId) {
            self.lines.lock().unwrap().push(format!("begin {}", id.get()));
        }
        fn gesture_end(&self, id: ParamId) {
            self.lines.lock().unwrap().push(format!("end {}", id.get()));
        }
        fn changed(&self, id: ParamId, plain: f64) {
            self.lines
                .lock()
                .unwrap()
                .push(format!("set {} {plain}", id.get()));
        }
        fn rescan(&self, flags: RescanFlags) {
            self.lines
                .lock()
                .unwrap()
                .push(format!("rescan {}", flags.bits()));
        }
    }
    impl HostLatency for Everything {
        fn set_samples(&self, samples: u32) {
            self.latency.store(samples as usize, Ordering::Relaxed);
        }
    }
    impl HostTail for Everything {
        fn changed(&self) {
            self.tails.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl HostWorker for Everything {
        fn schedule(&self, task: TaskId) -> bool {
            self.tasks.fetch_add(task.get() as usize, Ordering::Relaxed);
            true
        }
    }
    impl HostGui for Everything {
        fn request_resize(&self, w: u32, h: u32) -> bool {
            w > 0 && h > 0
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
    impl HostTimer for Everything {
        fn register(&self, period_ms: u32) -> Option<TimerId> {
            (period_ms >= 10).then(|| {
                let n = self.timers.fetch_add(1, Ordering::Relaxed);
                TimerId(n as u64 + 1)
            })
        }
        fn unregister(&self, _id: TimerId) {
            self.timers.fetch_sub(1, Ordering::Relaxed);
        }
    }
    impl HostResources for Everything {
        fn read(&self, logical_path: &str) -> io::Result<Vec<u8>> {
            if logical_path == "ui/theme.json" {
                Ok(b"{}".to_vec())
            } else {
                Err(io::Error::from(io::ErrorKind::NotFound))
            }
        }
        fn exists(&self, logical_path: &str) -> bool {
            logical_path == "ui/theme.json"
        }
    }
    impl RtHost for Everything {
        fn log(&self, record: &RtLogRecord) {
            self.lines
                .lock()
                .unwrap()
                .push(format!("rt:{}", record.message()));
        }
        fn request_callback(&self) {
            self.rt_callbacks.fetch_add(1, Ordering::Relaxed);
        }
        fn schedule_worker(&self, task: TaskId) -> bool {
            self.tasks.fetch_add(task.get() as usize, Ordering::Relaxed);
            true
        }
    }

    fn full() -> (Arc<Everything>, HostServices) {
        let host = Arc::new(Everything::default());
        let services = HostServices::builder()
            .info(HostInfo::new("Bitwig Studio", "Bitwig GmbH", "5.2"))
            .log(host.clone())
            .params(host.clone())
            .latency(host.clone())
            .tail(host.clone())
            .worker(host.clone())
            .gui(host.clone())
            .timer(host.clone())
            .resources(host.clone())
            .threads(Arc::new(RtThreadCheck))
            .rt_host(host.clone())
            .build();
        (host, services)
    }

    #[test]
    fn a_null_host_offers_nothing_but_still_logs() {
        let host = HostServices::null();
        assert!(!host.has_log());
        assert!(host.params().is_none());
        assert!(host.latency().is_none());
        assert!(host.tail().is_none());
        assert!(host.worker().is_none());
        assert!(host.gui().is_none());
        assert!(host.timer().is_none());
        assert!(host.resources().is_none());
        assert!(host.threads().is_none());
        assert_eq!(host.is_main_thread(), None);
        assert_eq!(host.is_audio_thread(), None);
        assert!(host.rt().is_null());
        assert!(!host.info().is_known());

        // The one guaranteed service works and goes nowhere.
        host.log().log(LogLevel::Fatal, "into the void");

        let debug = format!("{host:?}");
        assert!(debug.contains("log: \"no\""), "{debug}");
        assert!(debug.contains("unknown"), "{debug}");
    }

    #[test]
    fn every_service_is_reachable_once_installed() {
        let (spy, host) = full();

        assert_eq!(host.info().name, "Bitwig Studio");
        assert!(host.has_log());
        host.log().log(LogLevel::Info, "hello");

        let params = host.params().expect("installed");
        params.gesture_begin(ParamId(1));
        params.changed(ParamId(1), 0.5);
        params.gesture_end(ParamId(1));
        params.rescan(RescanFlags::VALUES);

        host.latency().expect("installed").set_samples(128);
        host.tail().expect("installed").changed();
        assert!(host.worker().expect("installed").schedule(TaskId(3)));

        let gui = host.gui().expect("installed");
        assert!(gui.request_resize(640, 480));
        assert!(!gui.request_resize(0, 480));
        assert!(gui.request_show());
        assert!(gui.request_hide());
        gui.closed(true);

        let timer = host.timer().expect("installed");
        let id = timer.register(16).expect("granted");
        assert_eq!(id, TimerId(1));
        assert_eq!(timer.register(1), None, "an impossible period is refused");
        timer.unregister(id);

        let resources = host.resources().expect("installed");
        assert!(resources.exists("ui/theme.json"));
        assert_eq!(resources.read_to_string("ui/theme.json").unwrap(), "{}");
        assert!(resources.read("ui/missing.json").is_err());

        assert_eq!(host.is_main_thread(), Some(false));
        assert_eq!(host.is_audio_thread(), Some(false));

        assert_eq!(spy.latency.load(Ordering::Relaxed), 128);
        assert_eq!(spy.tails.load(Ordering::Relaxed), 1);
        assert_eq!(spy.tasks.load(Ordering::Relaxed), 3);
        assert_eq!(spy.closed.load(Ordering::Relaxed), 2);
        assert_eq!(spy.timers.load(Ordering::Relaxed), 0);
        assert_eq!(
            *spy.lines.lock().unwrap(),
            [
                "info:hello",
                "begin 1",
                "set 1 0.5",
                "end 1",
                "rescan 1",
            ]
        );
    }

    #[test]
    fn the_real_time_subset_comes_along_and_reaches_the_same_host() {
        let (spy, host) = full();
        let rt = host.rt().clone();
        assert!(!rt.is_null());

        rt.log(LogLevel::Warn, "voice steal");
        rt.request_callback();
        assert!(rt.schedule_worker(TaskId(5)));

        assert_eq!(spy.rt_callbacks.load(Ordering::Relaxed), 1);
        assert_eq!(spy.tasks.load(Ordering::Relaxed), 5);
        assert_eq!(spy.lines.lock().unwrap().last().unwrap(), "rt:voice steal");
    }

    #[test]
    fn an_explicit_real_time_subset_can_be_installed_on_its_own() {
        let host = HostServices::builder().rt(RtHostServices::null()).build();
        assert!(host.rt().is_null());
        assert!(!host.has_log());
    }

    #[test]
    fn cloning_shares_the_services_rather_than_copying_them() {
        let (spy, host) = full();
        let clone = host.clone();
        host.log().log(LogLevel::Info, "one");
        clone.log().log(LogLevel::Info, "two");
        assert_eq!(spy.lines.lock().unwrap().len(), 2);
        assert_eq!(clone.info().name, "Bitwig Studio");
    }

    #[test]
    fn services_cross_threads() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HostServices>();
        assert_send_sync::<HostServicesBuilder>();

        let (spy, host) = full();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let host = host.clone();
                scope.spawn(move || {
                    for _ in 0..50 {
                        host.log().log(LogLevel::Trace, "x");
                    }
                });
            }
        });
        assert_eq!(spy.lines.lock().unwrap().len(), 200);
    }

    #[test]
    fn the_builder_is_debuggable_and_the_last_value_wins() {
        let builder = HostServices::builder().info(HostInfo::new("A", "", ""));
        assert!(format!("{builder:?}").contains('A'));

        let host = builder.info(HostInfo::new("B", "", "")).build();
        assert_eq!(host.info().name, "B");
    }
}
