//! A recording host: every `daux-host-services` trait implemented, nothing stubbed.
//!
//! A plug-in tested against a host that always says yes has not been tested. [`FakeHost`]
//! implements every service trait of `abi-v1` §11.6, records what it was asked to do, and
//! can be told to refuse — a full worker queue, an editor resize the host will not grant, a
//! timer it has no slot for. Those are the paths a plug-in gets wrong.
//!
//! # The two halves
//!
//! The main-thread half ([`HostLog`], [`HostParams`], [`HostGui`], …) records into
//! `Mutex`-protected `Vec`s: it may allocate and it may block, because a host's main thread
//! may.
//!
//! The audio-thread half ([`RtHost`]) may do neither. It records into a
//! [`daux_rt::RtLogQueue`] and a set of atomics, both allocated up front by
//! [`FakeHost::new`], so driving a whole `process` block through it allocates nothing —
//! which the real-time suite asserts.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use daux_rt::{LogLevel, RtLogQueue, RtLogRecord};

use crate::host_services::{
    HostGui, HostInfo, HostLatency, HostLog, HostParams, HostResources, HostServices, HostTail,
    HostTimer, HostWorker, ParamId, RescanFlags, RtHost, TaskId, ThreadCheck, TimerId,
};

/// One thing the plug-in did to a parameter, in the order it happened. [main-thread]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamActivity {
    /// The plug-in began a gesture — a knob was grabbed.
    GestureBegin(ParamId),
    /// The plug-in ended a gesture.
    GestureEnd(ParamId),
    /// The plug-in changed a value itself and told the host the plain value.
    Changed(ParamId, f64),
    /// The plug-in asked the host to re-read parameter metadata.
    Rescan(RescanFlags),
}

/// One editor request. [main-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuiRequest {
    /// The editor asked to become `w` × `h` physical pixels.
    Resize(u32, u32),
    /// The editor asked to be shown.
    Show,
    /// The editor asked to be hidden.
    Hide,
    /// The editor reported it was closed; `true` when it was also destroyed.
    Closed(bool),
}

/// How the host should answer requests it is allowed to refuse. [main-thread]
///
/// The defaults are the permissive ones; a test flips exactly the switch it wants to
/// exercise.
#[derive(Clone, Copy, Debug)]
pub struct HostPolicy {
    /// Number of worker tasks the queue accepts before it starts refusing.
    pub worker_capacity: usize,
    /// Whether the host grants editor resize requests.
    pub allow_resize: bool,
    /// Whether the host grants show/hide requests.
    pub allow_visibility: bool,
    /// Whether the host has a timer slot to give out.
    pub allow_timer: bool,
}

impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            worker_capacity: 64,
            allow_resize: true,
            allow_visibility: true,
            allow_timer: true,
        }
    }
}

/// What [`FakeHost`] observed, all of it readable from a test. [main-thread]
#[derive(Debug, Default)]
struct Journal {
    logs: Vec<(LogLevel, String)>,
    params: Vec<ParamActivity>,
    gui: Vec<GuiRequest>,
    latency: Vec<u32>,
    tail_changes: usize,
    worker_tasks: Vec<TaskId>,
    timers: Vec<TimerId>,
    resources: Vec<(String, Vec<u8>)>,
}

/// A host that implements every service and remembers everything.
///
/// Wrap it in an [`std::sync::Arc`] and hand the same `Arc` to as many
/// [`HostServicesBuilder`](crate::host_services::HostServicesBuilder) slots as the test
/// needs; one object serving several traits is exactly how a real host is written.
///
/// [main-thread] for construction and inspection, [audio-thread] for the [`RtHost`] half.
#[derive(Debug)]
pub struct FakeHost {
    policy: HostPolicy,
    journal: Mutex<Journal>,
    /// The audio thread's log sink: bounded, lock-free, allocated in [`FakeHost::new`].
    rt_log: RtLogQueue,
    rt_callbacks: AtomicUsize,
    rt_processes: AtomicUsize,
    rt_restarts: AtomicUsize,
    rt_worker_requests: AtomicUsize,
    rt_worker_refusals: AtomicUsize,
    next_timer: AtomicUsize,
    is_audio_thread: AtomicBool,
}

impl FakeHost {
    /// [main-thread] A permissive host with a 256-record real-time log queue.
    #[must_use]
    pub fn new() -> Self {
        Self::with_policy(HostPolicy::default())
    }

    /// [main-thread] A host that answers according to `policy`.
    #[must_use]
    pub fn with_policy(policy: HostPolicy) -> Self {
        Self {
            policy,
            journal: Mutex::new(Journal::default()),
            rt_log: RtLogQueue::with_capacity(256),
            rt_callbacks: AtomicUsize::new(0),
            rt_processes: AtomicUsize::new(0),
            rt_restarts: AtomicUsize::new(0),
            rt_worker_requests: AtomicUsize::new(0),
            rt_worker_refusals: AtomicUsize::new(0),
            next_timer: AtomicUsize::new(1),
            is_audio_thread: AtomicBool::new(false),
        }
    }

    /// [main-thread] Builds the [`HostServices`] a controller is handed.
    ///
    /// Every service is wired to `self`, which is what makes the recording complete.
    #[must_use]
    pub fn services(self: &std::sync::Arc<Self>) -> HostServices {
        HostServices::builder()
            .info(HostInfo::new("DAUx Test Harness", "Futureboard", "0.1.0"))
            .log(self.clone())
            .params(self.clone())
            .latency(self.clone())
            .tail(self.clone())
            .worker(self.clone())
            .gui(self.clone())
            .timer(self.clone())
            .resources(self.clone())
            .threads(self.clone())
            .rt_host(self.clone())
            .build()
    }

    /// [main-thread] Marks the calling context as the audio thread for [`ThreadCheck`].
    ///
    /// A plug-in that asserts its thread must be able to see both answers; without this the
    /// harness would only ever exercise the main-thread branch.
    pub fn set_audio_thread(&self, on: bool) {
        self.is_audio_thread.store(on, Ordering::Relaxed);
    }

    /// [main-thread] Every log record the plug-in wrote through [`HostLog`].
    #[must_use]
    pub fn logs(&self) -> Vec<(LogLevel, String)> {
        self.lock().logs.clone()
    }

    /// [main-thread] Every parameter interaction, in order.
    #[must_use]
    pub fn param_activity(&self) -> Vec<ParamActivity> {
        self.lock().params.clone()
    }

    /// [main-thread] Every editor request, in order.
    #[must_use]
    pub fn gui_requests(&self) -> Vec<GuiRequest> {
        self.lock().gui.clone()
    }

    /// [main-thread] Every latency the plug-in reported, in order.
    #[must_use]
    pub fn latency_reports(&self) -> Vec<u32> {
        self.lock().latency.clone()
    }

    /// [main-thread] How many times the plug-in said its tail changed.
    #[must_use]
    pub fn tail_changes(&self) -> usize {
        self.lock().tail_changes
    }

    /// [main-thread] Takes the queued worker tasks, leaving the queue empty.
    ///
    /// A real host drains its worker queue on some other thread; a test drains it when it
    /// is ready to run the tasks.
    #[must_use]
    pub fn take_worker_tasks(&self) -> Vec<TaskId> {
        core::mem::take(&mut self.lock().worker_tasks)
    }

    /// [main-thread] Every timer the plug-in registered.
    #[must_use]
    pub fn timers(&self) -> Vec<TimerId> {
        self.lock().timers.clone()
    }

    /// [main-thread] Adds a resource the plug-in will be able to read.
    pub fn add_resource(&self, logical_path: &str, bytes: &[u8]) {
        self.lock()
            .resources
            .push((logical_path.to_owned(), bytes.to_vec()));
    }

    /// [main-thread] Drains the audio thread's log queue into owned strings.
    #[must_use]
    pub fn drain_rt_log(&self) -> Vec<(LogLevel, String)> {
        let mut out = Vec::new();
        while let Some(record) = self.rt_log.pop() {
            out.push((record.level, record.message().to_owned()));
        }
        out
    }

    /// [main-thread] `request_callback` calls made from the audio thread.
    #[must_use]
    pub fn rt_callbacks(&self) -> usize {
        self.rt_callbacks.load(Ordering::Relaxed)
    }

    /// [main-thread] `request_process` calls made from the audio thread.
    #[must_use]
    pub fn rt_processes(&self) -> usize {
        self.rt_processes.load(Ordering::Relaxed)
    }

    /// [main-thread] `request_restart` calls made from the audio thread.
    #[must_use]
    pub fn rt_restarts(&self) -> usize {
        self.rt_restarts.load(Ordering::Relaxed)
    }

    /// [main-thread] Worker requests made from the audio thread, and how many were refused.
    #[must_use]
    pub fn rt_worker_stats(&self) -> (usize, usize) {
        (
            self.rt_worker_requests.load(Ordering::Relaxed),
            self.rt_worker_refusals.load(Ordering::Relaxed),
        )
    }

    /// A poisoned journal means a test thread panicked while holding it; the recording is
    /// still perfectly readable, and turning that into a second panic would hide the first.
    fn lock(&self) -> std::sync::MutexGuard<'_, Journal> {
        self.journal.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for FakeHost {
    fn default() -> Self {
        Self::new()
    }
}

impl HostLog for FakeHost {
    fn log(&self, level: LogLevel, msg: &str) {
        self.lock().logs.push((level, msg.to_owned()));
    }
}

impl HostParams for FakeHost {
    fn gesture_begin(&self, id: ParamId) {
        self.lock().params.push(ParamActivity::GestureBegin(id));
    }

    fn gesture_end(&self, id: ParamId) {
        self.lock().params.push(ParamActivity::GestureEnd(id));
    }

    fn changed(&self, id: ParamId, plain: f64) {
        self.lock().params.push(ParamActivity::Changed(id, plain));
    }

    fn rescan(&self, flags: RescanFlags) {
        self.lock().params.push(ParamActivity::Rescan(flags));
    }
}

impl HostLatency for FakeHost {
    fn set_samples(&self, samples: u32) {
        self.lock().latency.push(samples);
    }
}

impl HostTail for FakeHost {
    fn changed(&self) {
        self.lock().tail_changes += 1;
    }
}

impl HostWorker for FakeHost {
    fn schedule(&self, task: TaskId) -> bool {
        let mut journal = self.lock();
        if journal.worker_tasks.len() >= self.policy.worker_capacity {
            return false;
        }
        journal.worker_tasks.push(task);
        true
    }
}

impl HostGui for FakeHost {
    fn request_resize(&self, w: u32, h: u32) -> bool {
        self.lock().gui.push(GuiRequest::Resize(w, h));
        self.policy.allow_resize
    }

    fn request_show(&self) -> bool {
        self.lock().gui.push(GuiRequest::Show);
        self.policy.allow_visibility
    }

    fn request_hide(&self) -> bool {
        self.lock().gui.push(GuiRequest::Hide);
        self.policy.allow_visibility
    }

    fn closed(&self, destroyed: bool) {
        self.lock().gui.push(GuiRequest::Closed(destroyed));
    }
}

impl HostTimer for FakeHost {
    fn register(&self, _period_ms: u32) -> Option<TimerId> {
        if !self.policy.allow_timer {
            return None;
        }
        let id = TimerId(self.next_timer.fetch_add(1, Ordering::Relaxed) as u64);
        self.lock().timers.push(id);
        Some(id)
    }

    fn unregister(&self, id: TimerId) {
        self.lock().timers.retain(|t| *t != id);
    }
}

impl HostResources for FakeHost {
    fn read(&self, logical_path: &str) -> std::io::Result<Vec<u8>> {
        self.lock()
            .resources
            .iter()
            .find(|(path, _)| path == logical_path)
            .map(|(_, bytes)| bytes.clone())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no resource `{logical_path}`"),
                )
            })
    }

    fn exists(&self, logical_path: &str) -> bool {
        self.lock()
            .resources
            .iter()
            .any(|(path, _)| path == logical_path)
    }
}

impl ThreadCheck for FakeHost {
    fn is_main_thread(&self) -> bool {
        !self.is_audio_thread.load(Ordering::Relaxed)
    }

    fn is_audio_thread(&self) -> bool {
        self.is_audio_thread.load(Ordering::Relaxed)
    }
}

/// The audio-thread half. Every method is wait-free and allocation-free.
impl RtHost for FakeHost {
    fn log(&self, record: &RtLogRecord) {
        // A full queue drops the record. That is the correct behaviour on a deadline: the
        // alternative is to grow it, which is an allocation.
        self.rt_log.try_push(*record);
    }

    fn request_callback(&self) {
        self.rt_callbacks.fetch_add(1, Ordering::Relaxed);
    }

    fn request_process(&self) {
        self.rt_processes.fetch_add(1, Ordering::Relaxed);
    }

    fn request_restart(&self) {
        self.rt_restarts.fetch_add(1, Ordering::Relaxed);
    }

    fn schedule_worker(&self, _task: TaskId) -> bool {
        let queued = self.rt_worker_requests.fetch_add(1, Ordering::Relaxed);
        if queued < self.policy.worker_capacity {
            true
        } else {
            self.rt_worker_refusals.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn one_object_serves_every_service_the_builder_asks_for() {
        let host = Arc::new(FakeHost::new());
        let services = host.services();

        assert_eq!(services.info().name, "DAUx Test Harness");
        assert!(services.params().is_some());
        assert!(services.latency().is_some());
        assert!(services.tail().is_some());
        assert!(services.worker().is_some());
        assert!(services.gui().is_some());
        assert!(services.timer().is_some());
        assert!(services.resources().is_some());
        assert!(services.threads().is_some());
        assert!(!services.rt().is_null());
    }

    #[test]
    fn the_journal_keeps_the_order_the_plug_in_used() {
        let host = Arc::new(FakeHost::new());
        let services = host.services();
        let params = services.params().expect("params service");

        params.gesture_begin(ParamId(1));
        params.changed(ParamId(1), -6.0);
        params.gesture_end(ParamId(1));
        services.log().log(LogLevel::Warn, "clipping");

        assert_eq!(
            host.param_activity(),
            vec![
                ParamActivity::GestureBegin(ParamId(1)),
                ParamActivity::Changed(ParamId(1), -6.0),
                ParamActivity::GestureEnd(ParamId(1)),
            ]
        );
        assert_eq!(host.logs(), vec![(LogLevel::Warn, "clipping".to_owned())]);
    }

    #[test]
    fn a_host_that_refuses_is_representable() {
        let host = Arc::new(FakeHost::with_policy(HostPolicy {
            worker_capacity: 1,
            allow_resize: false,
            allow_visibility: false,
            allow_timer: false,
        }));
        let services = host.services();
        let gui = services.gui().expect("gui service");
        let worker = services.worker().expect("worker service");

        assert!(!gui.request_resize(800, 600), "this host refuses resizes");
        assert!(!gui.request_show());
        assert!(services.timer().expect("timer").register(16).is_none());
        assert!(worker.schedule(TaskId(1)), "the first task fits");
        assert!(!worker.schedule(TaskId(2)), "the second must be refused");

        // The refusals were still recorded: the plug-in asked, the host said no.
        assert_eq!(
            host.gui_requests(),
            vec![GuiRequest::Resize(800, 600), GuiRequest::Show]
        );
        assert_eq!(host.take_worker_tasks(), vec![TaskId(1)]);
        assert!(host.take_worker_tasks().is_empty(), "taking drains");
    }

    #[test]
    fn the_audio_thread_half_allocates_nothing() {
        let host = Arc::new(FakeHost::new());
        let services = host.services();
        let rt = services.rt().clone();

        let ((), allocations) = daux_rt::AllocGuard::scope(|| {
            for _ in 0..512 {
                rt.log(LogLevel::Trace, "block");
                rt.request_callback();
                rt.request_process();
                rt.request_restart();
                let _ = rt.schedule_worker(TaskId(1));
            }
        });

        assert_eq!(allocations, 0, "the audio-thread host path allocated");
        assert_eq!(host.rt_callbacks(), 512);
        assert_eq!(host.rt_processes(), 512);
        assert_eq!(host.rt_restarts(), 512);
        let (requests, refusals) = host.rt_worker_stats();
        assert_eq!(requests, 512);
        assert_eq!(refusals, 512 - 64, "the queue holds 64 by default");
        // The log queue is bounded, so it dropped rather than grew.
        assert!(host.drain_rt_log().len() <= 256);
    }

    #[test]
    fn resources_are_served_from_the_journal_and_missing_ones_are_errors() {
        let host = Arc::new(FakeHost::new());
        host.add_resource("fonts/Inter.txt", b"hello");
        let services = host.services();
        let resources = services.resources().expect("resource service");

        assert!(resources.exists("fonts/Inter.txt"));
        assert_eq!(resources.read("fonts/Inter.txt").unwrap(), b"hello");
        assert!(!resources.exists("fonts/missing.txt"));
        assert_eq!(
            resources.read("fonts/missing.txt").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn the_thread_check_can_answer_both_ways() {
        let host = Arc::new(FakeHost::new());
        let services = host.services();
        assert_eq!(services.is_main_thread(), Some(true));
        assert_eq!(services.is_audio_thread(), Some(false));

        host.set_audio_thread(true);
        assert_eq!(services.is_main_thread(), Some(false));
        assert_eq!(services.is_audio_thread(), Some(true));
    }
}
