//! Fallback implementations: the no-op logger and a thread check backed by
//! `daux-rt`'s thread markers.

use daux_rt::{LogLevel, ThreadClass, current_thread_class};

use crate::{HostLog, ThreadCheck};

/// A [`HostLog`] that discards everything. `[any-thread]`
///
/// This is what [`HostServices::log`](crate::HostServices::log) returns when the
/// host provides no logging, which is why that accessor can promise a value
/// instead of an `Option`: logging is the one service a plug-in should never
/// have to branch on. Discarding a record costs one virtual call and touches no
/// memory, so it is safe from the audio thread like any other `HostLog`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NullHostLog;

impl HostLog for NullHostLog {
    #[inline]
    fn log(&self, level: LogLevel, msg: &str) {
        let _ = (level, msg);
    }
}

/// A [`ThreadCheck`] answering from `daux-rt`'s per-thread markers.
/// `[any-thread]`
///
/// Useful when the host offers no `is_main_thread` / `is_audio_thread` of its
/// own, which ABI v1 permits — both entries of `DauxHostApiV1` are optional.
/// It is only as good as the bookkeeping around it: it reports the truth for
/// threads that were labelled with
/// [`daux_rt::set_current_thread_class`] or entered through
/// [`daux_rt::ThreadClassGuard`], and answers `false` to both questions for a
/// thread nobody labelled. That is the correct failure mode — "I do not know"
/// reads as "not this one" — but it means a plug-in must not treat a `false`
/// from [`is_main_thread`](ThreadCheck::is_main_thread) as proof that it is on
/// the audio thread.
///
/// ```
/// use daux_host_services::{RtThreadCheck, ThreadCheck};
/// use daux_rt::{ThreadClass, ThreadClassGuard};
///
/// let check = RtThreadCheck;
/// let _guard = ThreadClassGuard::enter(ThreadClass::Audio);
/// assert!(check.is_audio_thread());
/// assert!(!check.is_main_thread());
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RtThreadCheck;

impl ThreadCheck for RtThreadCheck {
    #[inline]
    fn is_main_thread(&self) -> bool {
        current_thread_class() == ThreadClass::Main
    }

    #[inline]
    fn is_audio_thread(&self) -> bool {
        current_thread_class() == ThreadClass::Audio
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_rt::{AllocGuard, ThreadClassGuard};

    #[test]
    fn the_null_logger_discards_without_allocating() {
        let log = NullHostLog;
        let dynamic: &dyn HostLog = &log;
        let ((), allocations) = AllocGuard::scope(|| {
            for level in LogLevel::ALL {
                dynamic.log(level, "dropped on the floor");
            }
        });
        assert_eq!(allocations, 0, "the null logger allocated");
        assert_eq!(log, NullHostLog);
    }

    #[test]
    fn the_thread_check_follows_the_current_class() {
        // Each assertion runs on its own thread so that the class this test
        // installs cannot leak into another test on the same worker.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let check = RtThreadCheck;
                let _guard = ThreadClassGuard::enter(ThreadClass::Main);
                assert!(check.is_main_thread());
                assert!(!check.is_audio_thread());
            });
            scope.spawn(|| {
                let check = RtThreadCheck;
                let _guard = ThreadClassGuard::enter(ThreadClass::Audio);
                assert!(check.is_audio_thread());
                assert!(!check.is_main_thread());
            });
            scope.spawn(|| {
                // An unlabelled thread is neither, which is the honest answer.
                let check = RtThreadCheck;
                assert!(!check.is_main_thread());
                assert!(!check.is_audio_thread());
            });
            scope.spawn(|| {
                // A labelled-but-different thread is also neither.
                let check = RtThreadCheck;
                let _guard = ThreadClassGuard::enter(ThreadClass::Worker);
                assert!(!check.is_main_thread());
                assert!(!check.is_audio_thread());
            });
        });
    }
}
