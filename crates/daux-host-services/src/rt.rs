//! The real-time-safe subset of the host, and nothing else.

use core::fmt;
use std::sync::Arc;

use daux_rt::{LogLevel, RtLogRecord};

use crate::TaskId;

/// The host callbacks that are safe to invoke from the audio thread.
///
/// Implemented by hosts and by the format adapters; plug-ins consume it through
/// [`RtHostServices`] rather than directly. Every method carries the same
/// obligation, and it is absolute: **no allocation, no lock, no syscall, no
/// blocking, no panic**. A host that cannot honour that for a given callback
/// must leave it at its default rather than provide a slow one, because the
/// caller has a hard deadline measured in microseconds
/// (`docs/architecture/realtime.md`).
///
/// Every method has a default that does nothing, so a host implements only what
/// it actually supports:
///
/// ```
/// use daux_host_services::{RtHost, RtHostServices, TaskId};
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicUsize, Ordering};
///
/// #[derive(Default)]
/// struct Requests(AtomicUsize);
///
/// impl RtHost for Requests {
///     fn request_callback(&self) {
///         self.0.fetch_add(1, Ordering::Relaxed);   // wait-free
///     }
/// }
///
/// let requests = Arc::new(Requests::default());
/// let host = RtHostServices::new(requests.clone());
/// host.request_callback();
/// assert_eq!(requests.0.load(Ordering::Relaxed), 1);
/// // Anything the host did not implement is simply inert.
/// assert!(!host.schedule_worker(TaskId(1)));
/// ```
pub trait RtHost: Send + Sync {
    /// Takes one already-bounded log record. `[audio-thread]`
    ///
    /// The record is a fixed-size value with no pointers, so the implementation
    /// can copy it into a lock-free queue and return. Formatting it is the
    /// consumer's job, on some other thread.
    fn log(&self, record: &RtLogRecord) {
        let _ = record;
    }

    /// Asks the host to call the plug-in's `on_main_thread` soon.
    /// `[audio-thread]`
    fn request_callback(&self) {}

    /// Asks the host to resume calling `process` after the plug-in went to
    /// sleep. `[audio-thread]`
    fn request_process(&self) {}

    /// Asks the host to deactivate and reactivate the plug-in — the only way to
    /// change something that is fixed for the lifetime of an activation, such as
    /// the bus layout. `[audio-thread]`
    ///
    /// The restart happens later, on the main thread; `process` keeps being
    /// called until it does.
    fn request_restart(&self) {}

    /// Queues `task` to run off the audio thread, returning `false` when the
    /// host's queue is full. `[audio-thread]`
    ///
    /// A full queue is a normal condition: retry next block or drop the work.
    /// Never spin, never wait.
    fn schedule_worker(&self, task: TaskId) -> bool {
        let _ = task;
        false
    }
}

/// Everything a plug-in may touch from `process`, and deliberately nothing else.
///
/// This is the object `ProcessContext::host()` hands out. It is a different type
/// from [`HostServices`](crate::HostServices) on purpose: the blocking services —
/// state, resources, GUI, timers, parameter gestures — are not merely
/// discouraged here, they are **unreachable**, because this struct holds no path
/// to them. The compiler stops the mistake before the allocation counter has to.
///
/// Cloning is cheap (one atomic increment) and is a `[main-thread]` operation.
/// Nothing in the audio-thread API allocates, locks or blocks:
/// [`log`](RtHostServices::log) copies at most
/// [`daux_rt::RT_LOG_MESSAGE_BYTES`] bytes into a stack record, truncating on a
/// `char` boundary, and every other method is a single virtual call.
///
/// [`RtHostServices::null`] is a fully functional instance that does nothing, so
/// offline rendering, unit tests and unhosted previews never need a host at all.
#[derive(Clone, Default)]
pub struct RtHostServices {
    /// `None` is the null host: every call is inert, and no branch beyond this
    /// `Option` is taken.
    host: Option<Arc<dyn RtHost>>,
}

impl RtHostServices {
    /// Wraps a host implementation. `[main-thread]` — the only allocation-aware
    /// step, and the caller supplies the `Arc`.
    #[must_use]
    pub fn new(host: Arc<dyn RtHost>) -> Self {
        Self { host: Some(host) }
    }

    /// A host that accepts everything and does nothing. `[any-thread]`
    ///
    /// Allocation-free, so it is safe to build even in a context that must not
    /// allocate — including, if it ever came to that, the audio thread.
    #[inline]
    #[must_use]
    pub const fn null() -> Self {
        Self { host: None }
    }

    /// `true` when this is the null host and every call is inert.
    /// `[audio-thread]`
    #[inline]
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.host.is_none()
    }

    /// Sends one bounded log record to the host. `[audio-thread]`
    ///
    /// `msg` is copied into a fixed-size [`RtLogRecord`] and truncated on a
    /// `char` boundary if it does not fit. Nothing is formatted and nothing is
    /// allocated: build the string elsewhere, or log a constant.
    #[inline]
    pub fn log(&self, level: LogLevel, msg: &str) {
        if let Some(host) = &self.host {
            host.log(&RtLogRecord::new(level, msg));
        }
    }

    /// Sends a record the caller already built. `[audio-thread]`
    ///
    /// Useful when the same message is logged every block: build the record once
    /// in `prepare` and hand it over here.
    #[inline]
    pub fn log_record(&self, record: &RtLogRecord) {
        if let Some(host) = &self.host {
            host.log(record);
        }
    }

    /// Asks the host to call `on_main_thread` soon. `[audio-thread]`
    #[inline]
    pub fn request_callback(&self) {
        if let Some(host) = &self.host {
            host.request_callback();
        }
    }

    /// Asks the host to resume calling `process`. `[audio-thread]`
    #[inline]
    pub fn request_process(&self) {
        if let Some(host) = &self.host {
            host.request_process();
        }
    }

    /// Asks the host to deactivate and reactivate the plug-in. `[audio-thread]`
    #[inline]
    pub fn request_restart(&self) {
        if let Some(host) = &self.host {
            host.request_restart();
        }
    }

    /// Queues off-thread work; `false` when the host refused or there is no
    /// host. `[audio-thread]`
    #[inline]
    pub fn schedule_worker(&self, task: TaskId) -> bool {
        match &self.host {
            Some(host) => host.schedule_worker(task),
            None => false,
        }
    }
}

impl fmt::Debug for RtHostServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtHostServices")
            .field("host", &if self.is_null() { "null" } else { "present" })
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_rt::{AllocGuard, RT_LOG_MESSAGE_BYTES, RtLogQueue};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A host of the shape an adapter actually writes: everything lands in
    /// lock-free structures allocated up front.
    struct Spy {
        log: RtLogQueue,
        callbacks: AtomicUsize,
        processes: AtomicUsize,
        restarts: AtomicUsize,
        tasks: AtomicUsize,
        worker_capacity: usize,
    }

    impl Spy {
        fn new(worker_capacity: usize) -> Arc<Self> {
            Arc::new(Self {
                log: RtLogQueue::with_capacity(8),
                callbacks: AtomicUsize::new(0),
                processes: AtomicUsize::new(0),
                restarts: AtomicUsize::new(0),
                tasks: AtomicUsize::new(0),
                worker_capacity,
            })
        }
    }

    impl RtHost for Spy {
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
        fn schedule_worker(&self, _task: TaskId) -> bool {
            let n = self.tasks.fetch_add(1, Ordering::Relaxed);
            n < self.worker_capacity
        }
    }

    #[test]
    fn the_null_host_swallows_everything_and_refuses_work() {
        let host = RtHostServices::null();
        assert!(host.is_null());
        assert_eq!(host.is_null(), RtHostServices::default().is_null());
        host.log(LogLevel::Error, "nobody is listening");
        host.log_record(&RtLogRecord::new(LogLevel::Info, "still nobody"));
        host.request_callback();
        host.request_process();
        host.request_restart();
        assert!(!host.schedule_worker(TaskId(1)));
        assert!(format!("{host:?}").contains("null"));
    }

    #[test]
    fn calls_reach_the_host() {
        let spy = Spy::new(2);
        let host = RtHostServices::new(spy.clone());
        assert!(!host.is_null());
        assert!(format!("{host:?}").contains("present"));

        host.log(LogLevel::Warn, "voice steal");
        host.request_callback();
        host.request_callback();
        host.request_process();
        host.request_restart();
        assert!(host.schedule_worker(TaskId(7)));
        assert!(host.schedule_worker(TaskId(7)));
        assert!(
            !host.schedule_worker(TaskId(7)),
            "a full worker queue must refuse, not block"
        );

        let record = spy.log.pop().expect("one record");
        assert_eq!(record.message(), "voice steal");
        assert_eq!(record.level, LogLevel::Warn);
        assert_eq!(spy.callbacks.load(Ordering::Relaxed), 2);
        assert_eq!(spy.processes.load(Ordering::Relaxed), 1);
        assert_eq!(spy.restarts.load(Ordering::Relaxed), 1);
        assert_eq!(spy.tasks.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn long_messages_are_truncated_rather_than_allocated() {
        let spy = Spy::new(0);
        let host = RtHostServices::new(spy.clone());
        // A multi-byte character straddling the limit must be dropped whole.
        let mut message = "a".repeat(RT_LOG_MESSAGE_BYTES - 1);
        message.push('é');
        host.log(LogLevel::Info, &message);

        let record = spy.log.pop().expect("one record");
        assert_eq!(record.message().len(), RT_LOG_MESSAGE_BYTES - 1);
        assert!(record.message().chars().all(|c| c == 'a'));
    }

    #[test]
    fn a_whole_block_of_host_traffic_allocates_nothing() {
        let spy = Spy::new(usize::MAX);
        let host = RtHostServices::new(spy.clone());
        let null = RtHostServices::null();
        let prebuilt = RtLogRecord::new(LogLevel::Trace, "prebuilt");

        let ((), allocations) = AllocGuard::scope(|| {
            for _ in 0..1_000 {
                host.log(LogLevel::Trace, "block processed");
                host.log_record(&prebuilt);
                host.request_callback();
                host.request_process();
                host.request_restart();
                let _ = host.schedule_worker(TaskId(1));
                null.log(LogLevel::Trace, "into the void");
                null.request_callback();
                let _ = null.schedule_worker(TaskId(1));
                while spy.log.pop().is_some() {}
            }
        });
        assert_eq!(allocations, 0, "the audio-thread host API allocated");
    }

    #[test]
    fn cloning_shares_one_host() {
        let spy = Spy::new(0);
        let host = RtHostServices::new(spy.clone());
        let clone = host.clone();
        host.request_callback();
        clone.request_callback();
        assert_eq!(spy.callbacks.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_host_that_implements_nothing_still_works() {
        struct Silent;
        impl RtHost for Silent {}

        let host = RtHostServices::new(Arc::new(Silent));
        host.log(LogLevel::Fatal, "nothing happens");
        host.request_callback();
        host.request_process();
        host.request_restart();
        assert!(!host.schedule_worker(TaskId(0)));
        assert!(!host.is_null(), "a silent host is still a host");
    }

    #[test]
    fn the_type_crosses_threads() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RtHostServices>();
        assert_send_sync::<TaskId>();

        let spy = Spy::new(0);
        let host = RtHostServices::new(spy.clone());
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let host = host.clone();
                scope.spawn(move || {
                    for _ in 0..100 {
                        host.request_process();
                    }
                });
            }
        });
        assert_eq!(spy.processes.load(Ordering::Relaxed), 400);
    }
}
