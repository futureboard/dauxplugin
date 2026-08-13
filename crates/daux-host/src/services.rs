//! The host half of the harness: every `daux.host.*` service, implemented for real.
//!
//! A harness that stubbed these out would test the plug-in against a host that never says
//! anything back, which is the one host that does not exist. Every trait here is
//! implemented the way a DAW implements it — bounded, non-blocking where the contract
//! demands it, and recording what happened so a test can assert on it:
//!
//! | Service | Implementation | Why |
//! | --- | --- | --- |
//! | [`HostLog`] | a bounded lock-free queue | reachable from `process`, so it must not block or allocate |
//! | [`HostParams`] | a recorded list of gestures and changes | a test asserts the begin/change/end bracket |
//! | [`HostLatency`] | the last reported value, and a count | latency reporting is a two-step dance worth checking |
//! | [`HostTail`] | a count | "ask me again" is all the ABI says |
//! | [`HostWorker`] | a bounded lock-free queue | `[audio-thread]`, must refuse rather than wait |
//! | [`HostGui`] | recorded requests with a configurable answer | a plug-in must survive being refused |
//! | [`HostTimer`] | issued ids, with a configurable answer | same |
//! | [`HostResources`] | the bundle's own confined resource directory | the real thing, escapes and all |
//! | [`ThreadCheck`] | the thread the harness was built on | the answer a real host gives |
//!
//! The two queues are what make this usable from an audio thread at all. Formatting a log
//! record allocates, so the plug-in hands over a fixed-size record and
//! [`HarnessHost::drain_log`] does the formatting later, on the main thread.

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread::ThreadId;

use daux_runtime::daux_bundle::ResourceDir;
use daux_runtime::daux_core::daux_rt::{
    LogLevel, MpscQueue, RtLogQueue, RtLogRecord, ThreadClass, current_thread_class,
};
use daux_runtime::daux_host_services::{
    HostGui, HostLatency, HostLog, HostParams, HostResources, HostTail, HostTimer, HostWorker,
    ParamId, RescanFlags, RtHost, TaskId, ThreadCheck, TimerId,
};

/// How many log records the harness holds before it starts dropping them.
///
/// Dropping rather than growing is the point: a plug-in that logs from `process` must not be
/// able to make the host allocate, and a test that produced ten thousand records was going
/// to assert on the first ten anyway.
const LOG_CAPACITY: usize = 512;

/// How many worker tasks may be queued before [`HostWorker::schedule`] starts refusing.
///
/// A full queue is a normal condition the plug-in has to cope with (`abi-v1` §11.6), and a
/// harness that never filled up would never exercise that path.
const WORKER_CAPACITY: usize = 64;

/// One thing the plug-in did to the host's parameter model. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamActivity {
    /// The user grabbed a control.
    GestureBegin(ParamId),
    /// The user let go.
    GestureEnd(ParamId),
    /// The plug-in set a parameter itself, to a **plain** value (`abi-v1` §11.2).
    Changed(ParamId, f64),
    /// The parameter model itself changed and the host's cache is stale.
    Rescan(RescanFlags),
}

/// One thing the plug-in asked the host's window manager for. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuiRequest {
    /// Resize the editor window, in physical pixels.
    Resize {
        /// Requested width in physical pixels.
        width: u32,
        /// Requested height in physical pixels.
        height: u32,
    },
    /// Show the editor window.
    Show,
    /// Hide the editor window.
    Hide,
    /// The plug-in closed its own editor. `destroyed` is `false` when it is merely hidden.
    Closed {
        /// Whether the editor object itself is gone.
        destroyed: bool,
    },
}

/// One log record, formatted on the main thread. [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord {
    /// How severe the plug-in said it was.
    pub level: LogLevel,
    /// The message, truncated to the fixed record size if it was longer.
    pub message: String,
}

/// Every host service the harness offers, and a record of what was asked of it.
/// [any-thread]
///
/// Shared behind an `Arc` by every instance the harness owns, so a test can assert on the
/// whole session at once. Clearing is explicit ([`HarnessHost::clear`]) rather than
/// automatic: a test that ran three blocks usually wants all three.
#[derive(Debug)]
pub struct HarnessHost {
    log: RtLogQueue,
    params: Mutex<Vec<ParamActivity>>,
    latency: AtomicU32,
    latency_reports: AtomicUsize,
    tail_changes: AtomicUsize,
    worker: MpscQueue<TaskId>,
    worker_refusals: AtomicUsize,
    gui: Mutex<Vec<GuiRequest>>,
    gui_grants: AtomicBool,
    timers: Mutex<Vec<(TimerId, u32)>>,
    next_timer: AtomicU64,
    timer_grants: AtomicBool,
    callbacks: AtomicUsize,
    processes: AtomicUsize,
    restarts: AtomicUsize,
    main_thread: ThreadId,
}

impl Default for HarnessHost {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessHost {
    /// Builds the host services, remembering the calling thread as the main thread.
    /// [main-thread]
    #[must_use]
    pub fn new() -> Self {
        Self {
            log: RtLogQueue::with_capacity(LOG_CAPACITY),
            params: Mutex::new(Vec::new()),
            latency: AtomicU32::new(0),
            latency_reports: AtomicUsize::new(0),
            tail_changes: AtomicUsize::new(0),
            worker: MpscQueue::with_capacity(WORKER_CAPACITY),
            worker_refusals: AtomicUsize::new(0),
            gui: Mutex::new(Vec::new()),
            gui_grants: AtomicBool::new(true),
            timers: Mutex::new(Vec::new()),
            next_timer: AtomicU64::new(1),
            timer_grants: AtomicBool::new(true),
            callbacks: AtomicUsize::new(0),
            processes: AtomicUsize::new(0),
            restarts: AtomicUsize::new(0),
            main_thread: std::thread::current().id(),
        }
    }

    /// Takes every log record the plug-in has produced. [main-thread] — allocates.
    ///
    /// Formatting happens here rather than at the call site, which is what lets
    /// [`HostLog::log`] be safe to call from `process`.
    #[must_use]
    pub fn drain_log(&self) -> Vec<LogRecord> {
        let mut records = Vec::new();
        while let Some(record) = self.log.pop() {
            records.push(LogRecord {
                level: record.level,
                message: record.message().to_owned(),
            });
        }
        records
    }

    /// How many log records were dropped because the queue was full. [any-thread]
    #[must_use]
    pub fn dropped_log_records(&self) -> usize {
        self.log.dropped()
    }

    /// Everything the plug-in did to the parameter model, in order. [main-thread]
    #[must_use]
    pub fn param_activity(&self) -> Vec<ParamActivity> {
        self.params
            .lock()
            .map_or_else(|e| e.into_inner().clone(), |g| g.clone())
    }

    /// The latency the plug-in last reported, in samples. [any-thread]
    #[must_use]
    pub fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    /// How many times the plug-in reported a latency change. [any-thread]
    #[must_use]
    pub fn latency_reports(&self) -> usize {
        self.latency_reports.load(Ordering::Relaxed)
    }

    /// How many times the plug-in said its tail changed. [any-thread]
    #[must_use]
    pub fn tail_changes(&self) -> usize {
        self.tail_changes.load(Ordering::Relaxed)
    }

    /// Takes the queued worker tasks. [main-thread]
    ///
    /// A real host runs these on a worker thread and then calls `on_main_thread`; the
    /// harness hands them to the caller so a test can decide when that happens.
    #[must_use]
    pub fn take_worker_tasks(&self) -> Vec<TaskId> {
        let mut tasks = Vec::new();
        while let Some(task) = self.worker.pop() {
            tasks.push(task);
        }
        tasks
    }

    /// How many worker requests were refused because the queue was full. [any-thread]
    #[must_use]
    pub fn refused_worker_tasks(&self) -> usize {
        self.worker_refusals.load(Ordering::Relaxed)
    }

    /// Everything the plug-in asked the window manager for, in order. [main-thread]
    #[must_use]
    pub fn gui_requests(&self) -> Vec<GuiRequest> {
        self.gui
            .lock()
            .map_or_else(|e| e.into_inner().clone(), |g| g.clone())
    }

    /// Whether the harness grants editor requests. [main-thread]
    ///
    /// Set it to `false` to check that a plug-in stays usable when the host says no, which
    /// several real hosts do.
    pub fn set_gui_grants(&self, grants: bool) {
        self.gui_grants.store(grants, Ordering::Relaxed);
    }

    /// Whether the harness grants timer registrations. [main-thread]
    pub fn set_timer_grants(&self, grants: bool) {
        self.timer_grants.store(grants, Ordering::Relaxed);
    }

    /// The timers the plug-in currently holds, as `(id, period_ms)`. [main-thread]
    #[must_use]
    pub fn timers(&self) -> Vec<(TimerId, u32)> {
        self.timers
            .lock()
            .map_or_else(|e| e.into_inner().clone(), |g| g.clone())
    }

    /// How many times the plug-in asked for an `on_main_thread` callback. [any-thread]
    #[must_use]
    pub fn callback_requests(&self) -> usize {
        self.callbacks.load(Ordering::Relaxed)
    }

    /// How many times the plug-in asked to be woken from sleep. [any-thread]
    #[must_use]
    pub fn process_requests(&self) -> usize {
        self.processes.load(Ordering::Relaxed)
    }

    /// How many times the plug-in asked to be deactivated and reactivated. [any-thread]
    #[must_use]
    pub fn restart_requests(&self) -> usize {
        self.restarts.load(Ordering::Relaxed)
    }

    /// Forgets everything recorded so far. [main-thread]
    pub fn clear(&self) {
        while self.log.pop().is_some() {}
        let _ = self.log.take_dropped();
        while self.worker.pop().is_some() {}
        if let Ok(mut params) = self.params.lock() {
            params.clear();
        }
        if let Ok(mut gui) = self.gui.lock() {
            gui.clear();
        }
        if let Ok(mut timers) = self.timers.lock() {
            timers.clear();
        }
        self.latency.store(0, Ordering::Relaxed);
        self.latency_reports.store(0, Ordering::Relaxed);
        self.tail_changes.store(0, Ordering::Relaxed);
        self.worker_refusals.store(0, Ordering::Relaxed);
        self.callbacks.store(0, Ordering::Relaxed);
        self.processes.store(0, Ordering::Relaxed);
        self.restarts.store(0, Ordering::Relaxed);
    }

    /// Records one parameter event, tolerating a poisoned lock.
    ///
    /// A panic in one test must not make every later assertion in the same process fail on
    /// a lock that will never recover.
    fn record_param(&self, activity: ParamActivity) {
        match self.params.lock() {
            Ok(mut params) => params.push(activity),
            Err(poisoned) => poisoned.into_inner().push(activity),
        }
    }

    fn record_gui(&self, request: GuiRequest) {
        match self.gui.lock() {
            Ok(mut gui) => gui.push(request),
            Err(poisoned) => poisoned.into_inner().push(request),
        }
    }
}

impl HostLog for HarnessHost {
    /// [any-thread] — bounded and non-blocking, so `process` may call it.
    fn log(&self, level: LogLevel, msg: &str) {
        self.log.try_log(level, msg);
    }
}

impl HostParams for HarnessHost {
    fn gesture_begin(&self, id: ParamId) {
        self.record_param(ParamActivity::GestureBegin(id));
    }

    fn gesture_end(&self, id: ParamId) {
        self.record_param(ParamActivity::GestureEnd(id));
    }

    fn changed(&self, id: ParamId, plain: f64) {
        self.record_param(ParamActivity::Changed(id, plain));
    }

    fn rescan(&self, flags: RescanFlags) {
        self.record_param(ParamActivity::Rescan(flags));
    }
}

impl HostLatency for HarnessHost {
    fn set_samples(&self, samples: u32) {
        self.latency.store(samples, Ordering::Relaxed);
        self.latency_reports.fetch_add(1, Ordering::Relaxed);
    }
}

impl HostTail for HarnessHost {
    fn changed(&self) {
        self.tail_changes.fetch_add(1, Ordering::Relaxed);
    }
}

impl HostWorker for HarnessHost {
    /// [audio-thread] — lock-free, and refuses rather than waits when full.
    fn schedule(&self, task: TaskId) -> bool {
        if self.worker.try_push(task).is_ok() {
            true
        } else {
            self.worker_refusals.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

impl HostGui for HarnessHost {
    fn request_resize(&self, w: u32, h: u32) -> bool {
        self.record_gui(GuiRequest::Resize {
            width: w,
            height: h,
        });
        // A zero-sized window is not a window, whatever the host's policy is.
        w > 0 && h > 0 && self.gui_grants.load(Ordering::Relaxed)
    }

    fn request_show(&self) -> bool {
        self.record_gui(GuiRequest::Show);
        self.gui_grants.load(Ordering::Relaxed)
    }

    fn request_hide(&self) -> bool {
        self.record_gui(GuiRequest::Hide);
        self.gui_grants.load(Ordering::Relaxed)
    }

    fn closed(&self, destroyed: bool) {
        self.record_gui(GuiRequest::Closed { destroyed });
    }
}

impl HostTimer for HarnessHost {
    fn register(&self, period_ms: u32) -> Option<TimerId> {
        if !self.timer_grants.load(Ordering::Relaxed) {
            return None;
        }
        // A period of zero is a busy loop, not a timer; a real host clamps or refuses, and
        // refusing is the behaviour a plug-in is least likely to have tested against.
        if period_ms == 0 {
            return None;
        }
        let id = TimerId(self.next_timer.fetch_add(1, Ordering::Relaxed));
        match self.timers.lock() {
            Ok(mut timers) => timers.push((id, period_ms)),
            Err(poisoned) => poisoned.into_inner().push((id, period_ms)),
        }
        Some(id)
    }

    fn unregister(&self, id: TimerId) {
        // Unregistering an unknown or already-cancelled id is a no-op, never a panic.
        match self.timers.lock() {
            Ok(mut timers) => timers.retain(|(timer, _)| *timer != id),
            Err(poisoned) => poisoned.into_inner().retain(|(timer, _)| *timer != id),
        }
    }
}

impl ThreadCheck for HarnessHost {
    fn is_main_thread(&self) -> bool {
        std::thread::current().id() == self.main_thread
    }

    /// [any-thread] — non-blocking and allocation-free, because debug assertions in a
    /// plug-in call it from `process`.
    fn is_audio_thread(&self) -> bool {
        current_thread_class() == ThreadClass::Audio
    }
}

impl RtHost for HarnessHost {
    /// [audio-thread] — copies a fixed-size record into a bounded queue and returns.
    fn log(&self, record: &RtLogRecord) {
        self.log.try_push(*record);
    }

    fn request_callback(&self) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
    }

    fn request_process(&self) {
        self.processes.fetch_add(1, Ordering::Relaxed);
    }

    fn request_restart(&self) {
        self.restarts.fetch_add(1, Ordering::Relaxed);
    }

    fn schedule_worker(&self, task: TaskId) -> bool {
        HostWorker::schedule(self, task)
    }
}

/// Bundle-relative resource access, backed by the bundle the instance was loaded from.
/// [main-thread]
///
/// The real thing, not a stub: every lookup goes through `daux-bundle`'s confinement rules,
/// so a plug-in that asks for `../../../etc/passwd` gets an error here exactly as it would
/// in a DAW.
#[derive(Debug)]
pub(crate) struct BundleResources {
    resources: ResourceDir,
}

impl BundleResources {
    pub(crate) const fn new(resources: ResourceDir) -> Self {
        Self { resources }
    }
}

impl HostResources for BundleResources {
    fn read(&self, logical_path: &str) -> io::Result<Vec<u8>> {
        self.resources
            .read(logical_path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))
    }

    fn read_to_string(&self, logical_path: &str) -> io::Result<String> {
        self.resources
            .read_to_string(logical_path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))
    }

    fn exists(&self, logical_path: &str) -> bool {
        self.resources.exists(logical_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn parameter_gestures_are_recorded_in_order() {
        let host = HarnessHost::new();
        host.gesture_begin(ParamId(3));
        HostParams::changed(&host, ParamId(3), -6.0);
        HostParams::changed(&host, ParamId(3), -3.0);
        host.gesture_end(ParamId(3));
        host.rescan(RescanFlags::VALUES);

        assert_eq!(
            host.param_activity(),
            [
                ParamActivity::GestureBegin(ParamId(3)),
                ParamActivity::Changed(ParamId(3), -6.0),
                ParamActivity::Changed(ParamId(3), -3.0),
                ParamActivity::GestureEnd(ParamId(3)),
                ParamActivity::Rescan(RescanFlags::VALUES),
            ],
            "hosts record one undo step per gesture bracket, so the order is the contract"
        );
    }

    /// The log is reachable from the audio thread, so it must be bounded: a plug-in that
    /// logs in a loop must lose records rather than make the host allocate.
    #[test]
    fn the_log_is_bounded_and_reports_what_it_dropped() {
        let host = HarnessHost::new();
        for index in 0..LOG_CAPACITY + 20 {
            HostLog::log(&host, LogLevel::Info, &format!("record {index}"));
        }
        let drained = host.drain_log();
        assert_eq!(drained.len(), LOG_CAPACITY);
        assert_eq!(host.dropped_log_records(), 20);
        assert_eq!(drained[0].message, "record 0");
        assert_eq!(drained[0].level, LogLevel::Info);

        // Drained means drained.
        assert!(host.drain_log().is_empty());
    }

    /// `abi-v1` §11.6: a full worker queue is a normal condition and must be *reported*,
    /// not hidden, because the plug-in decides whether to retry or drop the work.
    #[test]
    fn a_full_worker_queue_refuses_rather_than_growing() {
        let host = HarnessHost::new();
        for index in 0..WORKER_CAPACITY {
            assert!(
                HostWorker::schedule(&host, TaskId(index as u64)),
                "task {index} should still fit"
            );
        }
        assert!(
            !HostWorker::schedule(&host, TaskId(9_999)),
            "the queue is full and must say so"
        );
        assert_eq!(host.refused_worker_tasks(), 1);

        let tasks = host.take_worker_tasks();
        assert_eq!(tasks.len(), WORKER_CAPACITY);
        assert_eq!(tasks[0], TaskId(0));
        assert!(
            HostWorker::schedule(&host, TaskId(1)),
            "draining makes room again"
        );
    }

    #[test]
    fn editor_requests_are_recorded_and_can_be_refused() {
        let host = HarnessHost::new();
        assert!(host.request_resize(800, 600));
        assert!(
            !host.request_resize(0, 600),
            "a zero-width window is not one"
        );
        assert!(host.request_show());

        host.set_gui_grants(false);
        assert!(!host.request_resize(640, 480));
        assert!(!host.request_show());
        assert!(!host.request_hide());
        HostGui::closed(&host, true);

        assert_eq!(
            host.gui_requests(),
            [
                GuiRequest::Resize {
                    width: 800,
                    height: 600
                },
                GuiRequest::Resize {
                    width: 0,
                    height: 600
                },
                GuiRequest::Show,
                GuiRequest::Resize {
                    width: 640,
                    height: 480
                },
                GuiRequest::Show,
                GuiRequest::Hide,
                GuiRequest::Closed { destroyed: true },
            ]
        );
    }

    #[test]
    fn timers_are_issued_refusable_and_cancellable() {
        let host = HarnessHost::new();
        let first = host.register(16).expect("granted");
        let second = host.register(33).expect("granted");
        assert_ne!(first, second, "ids must be distinct");
        assert_eq!(host.timers().len(), 2);

        host.unregister(first);
        assert_eq!(host.timers().len(), 1);
        host.unregister(first);
        assert_eq!(host.timers().len(), 1, "cancelling twice is a no-op");

        assert_eq!(host.register(0), None, "a zero period is a busy loop");

        host.set_timer_grants(false);
        assert_eq!(
            host.register(16),
            None,
            "a plug-in must cope with a host that has no timers"
        );
    }

    #[test]
    fn latency_and_tail_notifications_are_counted() {
        let host = HarnessHost::new();
        assert_eq!(host.latency(), 0);
        host.set_samples(128);
        host.set_samples(256);
        HostTail::changed(&host);

        assert_eq!(host.latency(), 256);
        assert_eq!(host.latency_reports(), 2);
        assert_eq!(host.tail_changes(), 1);
    }

    #[test]
    fn the_real_time_callbacks_are_counted_separately() {
        let host = HarnessHost::new();
        RtHost::request_callback(&host);
        RtHost::request_process(&host);
        RtHost::request_process(&host);
        RtHost::request_restart(&host);
        RtHost::log(&host, &RtLogRecord::new(LogLevel::Warn, "voice steal"));

        assert_eq!(host.callback_requests(), 1);
        assert_eq!(host.process_requests(), 2);
        assert_eq!(host.restart_requests(), 1);
        assert_eq!(host.drain_log()[0].message, "voice steal");
    }

    /// The thread check is what a plug-in's debug assertions rely on, so it has to give
    /// the answer a real host gives rather than a constant.
    #[test]
    fn the_thread_check_answers_for_the_thread_that_asks() {
        let host = Arc::new(HarnessHost::new());
        assert!(host.is_main_thread());
        assert!(!host.is_audio_thread());

        let elsewhere = Arc::clone(&host);
        std::thread::spawn(move || {
            assert!(!elsewhere.is_main_thread());
            assert!(!elsewhere.is_audio_thread());
            daux_runtime::daux_core::daux_rt::set_current_thread_class(ThreadClass::Audio);
            assert!(elsewhere.is_audio_thread());
        })
        .join()
        .expect("the thread ran");
    }

    #[test]
    fn clearing_forgets_everything_including_the_drop_count() {
        let host = HarnessHost::new();
        for index in 0..LOG_CAPACITY + 5 {
            HostLog::log(&host, LogLevel::Trace, &format!("{index}"));
        }
        host.gesture_begin(ParamId(1));
        host.set_samples(64);
        HostWorker::schedule(&host, TaskId(1));
        host.request_show();

        host.clear();
        assert!(host.drain_log().is_empty());
        assert_eq!(host.dropped_log_records(), 0);
        assert!(host.param_activity().is_empty());
        assert!(host.gui_requests().is_empty());
        assert!(host.take_worker_tasks().is_empty());
        assert_eq!(host.latency(), 0);
    }

    #[test]
    fn the_services_cross_threads() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HarnessHost>();
        assert_send_sync::<BundleResources>();
    }
}
