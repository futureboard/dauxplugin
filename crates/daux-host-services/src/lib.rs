//! Explicit host service interfaces for DAUxPlug: logging, automation gestures,
//! latency, tail, worker scheduling, GUI negotiation, timers, resources and
//! thread identification.
//!
//! A plug-in does not "call the host". It calls a service it was handed, and
//! only if it was handed one. This crate is where that idea is made concrete:
//! each `daux.host.*` extension of `docs/specifications/abi-v1.md` §11.6 becomes
//! one small `Send + Sync` trait, a host implements the ones it has, and
//! [`HostServices`] carries them as `Option`s that a plug-in must handle.
//!
//! # The split that matters
//!
//! There are two aggregates, and the difference between them is a load-bearing
//! part of the architecture:
//!
//! | | [`HostServices`] | [`RtHostServices`] |
//! | --- | --- | --- |
//! | Handed to | the controller, via `set_host` | `process`, via `ProcessContext::host` |
//! | Thread | main / UI | audio |
//! | May block | yes | **never** |
//! | May allocate | yes | **never** |
//! | Reaches the other | yes, via [`HostServices::rt`] | no — by construction |
//!
//! The audio thread cannot reach a blocking service because [`RtHostServices`]
//! has no path to one: it holds a [`RtHost`], whose five methods are the entire
//! real-time-safe surface of a host (`docs/architecture/realtime.md` §5). This
//! is a type-level guarantee, not a convention — you cannot accidentally load a
//! file from `process` through an object that has no file API on it.
//!
//! # Nothing is mandatory
//!
//! Every accessor except [`HostServices::log`] returns `Option`, and `None` is
//! the normal state in a host that does not implement that extension. Logging is
//! the exception because a plug-in should never have to branch on whether it can
//! report a problem; when the host provides no logger, [`NullHostLog`] quietly
//! absorbs the records.
//!
//! [`HostServices::null`] and [`RtHostServices::null`] give a complete, working,
//! entirely inert host, so unit tests, offline rendering and unhosted previews
//! never need a real one — and running against them is the cheapest way to prove
//! a plug-in degrades gracefully.
//!
//! # Example
//!
//! ```
//! use daux_host_services::{
//!     HostInfo, HostLog, HostServices, HostWorker, RtHost, RtHostServices, TaskId,
//! };
//! use daux_rt::{LogLevel, RtLogQueue, RtLogRecord};
//! use std::sync::Arc;
//!
//! /// A minimal host: logging goes into a bounded queue that the main thread
//! /// drains, and worker requests are counted.
//! struct MiniHost {
//!     records: RtLogQueue,
//! }
//!
//! impl HostLog for MiniHost {
//!     fn log(&self, level: LogLevel, msg: &str) {
//!         self.records.try_log(level, msg);          // bounded, never blocks
//!     }
//! }
//! impl RtHost for MiniHost {
//!     fn log(&self, record: &RtLogRecord) {
//!         self.records.try_push(*record);
//!     }
//!     fn schedule_worker(&self, _task: TaskId) -> bool {
//!         true
//!     }
//! }
//!
//! let mini = Arc::new(MiniHost { records: RtLogQueue::with_capacity(16) });
//! let host = HostServices::builder()
//!     .info(HostInfo::new("MiniHost", "Futureboard", "1.0"))
//!     .log(mini.clone())
//!     .rt_host(mini.clone())
//!     .build();
//!
//! // [main-thread]
//! host.log().log(LogLevel::Info, "instantiated");
//! assert!(host.timer().is_none(), "this host has no timer service");
//!
//! // [audio-thread] — the same host, through a much smaller door.
//! let rt = host.rt();
//! rt.log(LogLevel::Warn, "voice steal");
//! assert!(rt.schedule_worker(TaskId(1)));
//!
//! assert_eq!(mini.records.pop().unwrap().message(), "instantiated");
//! assert_eq!(mini.records.pop().unwrap().message(), "voice steal");
//! ```

#![forbid(unsafe_code)]

mod ids;
mod info;
mod null;
mod rescan;
mod rt;
mod services;
mod traits;

pub use crate::ids::{TaskId, TimerId};
pub use crate::info::HostInfo;
pub use crate::null::{NullHostLog, RtThreadCheck};
pub use crate::rescan::RescanFlags;
pub use crate::rt::{RtHost, RtHostServices};
pub use crate::services::{HostServices, HostServicesBuilder};
pub use crate::traits::{
    HostGui, HostLatency, HostLog, HostParams, HostResources, HostTail, HostTimer, HostWorker,
    ThreadCheck,
};

/// Severity of a log record.
///
/// Re-exported from `daux-rt` rather than defined again: the discriminants are
/// part of the binary contract (`DAUX_LOG_*`, abi-v1 §11.6) and a second
/// definition would be a second thing to keep in sync.
pub use daux_rt::LogLevel;

/// The fixed-size log record the audio thread produces, re-exported so a host
/// can implement [`RtHost::log`] without naming `daux-rt`.
pub use daux_rt::{RT_LOG_MESSAGE_BYTES, RtLogRecord};

/// Permanent parameter identity, re-exported so a host can implement
/// [`HostParams`] without naming `daux-parameter`.
pub use daux_parameter::ParamId;

/// The crate's own tests run under the counting allocator, so "the audio-thread
/// API does not allocate" is checked rather than asserted. Production builds are
/// untouched.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_rt::CountingAllocator = daux_rt::CountingAllocator;

#[cfg(test)]
mod tests {
    use super::*;
    use daux_rt::{AllocGuard, RtLogQueue, counting_allocator_installed};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Bridge {
        records: RtLogQueue,
        workers: AtomicUsize,
    }

    impl RtHost for Bridge {
        fn log(&self, record: &RtLogRecord) {
            self.records.try_push(*record);
        }
        fn schedule_worker(&self, _task: TaskId) -> bool {
            self.workers.fetch_add(1, Ordering::Relaxed);
            true
        }
    }

    impl HostLog for Bridge {
        fn log(&self, level: LogLevel, msg: &str) {
            self.records.try_log(level, msg);
        }
    }

    /// The whole point of the crate, as one test: the object `process` receives
    /// is usable, reaches the host, and allocates nothing.
    #[test]
    fn the_audio_thread_path_is_allocation_free() {
        assert!(
            counting_allocator_installed(),
            "the allocation tripwire is not installed, so this test would pass vacuously"
        );

        let bridge = Arc::new(Bridge {
            records: RtLogQueue::with_capacity(64),
            workers: AtomicUsize::new(0),
        });
        let host = HostServices::builder()
            .info(HostInfo::new("Harness", "Futureboard", "0.1"))
            .log(bridge.clone())
            .rt_host(bridge.clone())
            .build();
        let rt = host.rt();

        let ((), allocations) = AllocGuard::scope(|| {
            for _ in 0..512 {
                rt.log(LogLevel::Trace, "block");
                rt.request_callback();
                rt.request_process();
                rt.request_restart();
                let _ = rt.schedule_worker(TaskId(1));
                while bridge.records.pop().is_some() {}
            }
        });

        assert_eq!(allocations, 0, "process()'s view of the host allocated");
        assert_eq!(bridge.workers.load(Ordering::Relaxed), 512);
    }

    /// The type-level guarantee spelled out: `RtHostServices` exposes exactly
    /// five operations, and none of them is a way back to a blocking service.
    #[test]
    fn the_real_time_subset_has_no_door_back_to_the_blocking_services() {
        let host = HostServices::null();
        let rt: RtHostServices = host.rt().clone();
        // Everything below compiles; anything resembling `rt.resources()`,
        // `rt.params()` or `rt.gui()` does not, because those methods only exist
        // on `HostServices`.
        rt.log(LogLevel::Info, "");
        rt.request_callback();
        rt.request_process();
        rt.request_restart();
        assert!(!rt.schedule_worker(TaskId(0)));
    }

    #[test]
    fn a_plug_in_written_against_the_null_host_never_unwraps() {
        // The shape a well-behaved plug-in has: ask, and cope with `None`.
        let host = HostServices::null();

        let latency_reported = match host.latency() {
            Some(service) => {
                service.set_samples(64);
                true
            }
            None => false,
        };
        assert!(!latency_reported);

        let timer = host.timer().and_then(|t| t.register(16));
        assert_eq!(timer, None);

        let theme = host
            .resources()
            .and_then(|r| r.read_to_string("ui/theme.json").ok())
            .unwrap_or_else(|| "built-in".to_owned());
        assert_eq!(theme, "built-in");

        // And logging always works.
        host.log().log(LogLevel::Warn, "running unhosted");
    }

    #[test]
    fn the_public_surface_has_the_thread_bounds_the_contract_promises() {
        const fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<HostServices>();
        assert_send_sync::<HostServicesBuilder>();
        assert_send_sync::<RtHostServices>();
        assert_send_sync::<HostInfo>();
        assert_send_sync::<TaskId>();
        assert_send_sync::<TimerId>();
        assert_send_sync::<RescanFlags>();
        assert_send_sync::<NullHostLog>();
        assert_send_sync::<RtThreadCheck>();
        assert_send_sync::<dyn RtHost>();
        assert_send_sync::<dyn HostLog>();
        assert_send_sync::<dyn HostParams>();
        assert_send_sync::<dyn HostLatency>();
        assert_send_sync::<dyn HostTail>();
        assert_send_sync::<dyn HostWorker>();
        assert_send_sync::<dyn HostGui>();
        assert_send_sync::<dyn HostTimer>();
        assert_send_sync::<dyn HostResources>();
        assert_send_sync::<dyn ThreadCheck>();
    }
}
