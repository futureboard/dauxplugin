//! The individual services a host may offer a plug-in.
//!
//! Each trait is one `daux.host.*` extension of `docs/specifications/abi-v1.md`
//! §11.6, expressed in safe Rust. A host implements the ones it has; the rest
//! simply stay `None` on [`HostServices`](crate::HostServices) and the plug-in
//! degrades. Every trait is `Send + Sync` because one plug-in instance is
//! reachable from the main thread, the UI thread and (for the real-time subset)
//! the audio thread at the same time.
//!
//! Everything here except [`HostLog`] and [`HostWorker`] is **main-thread only
//! and may block**. That is why none of these traits is reachable from
//! [`RtHostServices`](crate::RtHostServices): the audio thread gets a different,
//! deliberately smaller object.

use std::io;

use daux_parameter::ParamId;
use daux_rt::LogLevel;

use crate::{RescanFlags, TaskId, TimerId};

/// Structured logging. `daux.host.log/1`. `[any-thread]`
///
/// The one service that must never block: a host implementation is required to
/// be non-blocking and allocation-free so that audio-thread code can reach it
/// through [`RtHostServices::log`](crate::RtHostServices::log). Implementations
/// that need to write to a file or a UI console must hand the record to a
/// bounded queue such as [`daux_rt::RtLogQueue`] and drain it elsewhere.
///
/// `msg` is already formatted and borrowed only for the duration of the call.
pub trait HostLog: Send + Sync {
    /// Records one message. `[any-thread]` — must not block or allocate.
    fn log(&self, level: LogLevel, msg: &str);
}

/// Automation gestures and parameter-model invalidation. `daux.host.params/1`.
/// `[main-thread]`
///
/// A plug-in calls these when *it* changes a parameter — from its editor, from a
/// preset load, or because its parameter model itself changed. Automation that
/// the host writes never comes back through here.
///
/// A user drag is always three calls in order: [`gesture_begin`], one or more
/// [`changed`], then [`gesture_end`]. Hosts rely on that bracket to record a
/// single undo step, so an unbalanced sequence produces unusable automation.
///
/// [`gesture_begin`]: HostParams::gesture_begin
/// [`changed`]: HostParams::changed
/// [`gesture_end`]: HostParams::gesture_end
pub trait HostParams: Send + Sync {
    /// The user grabbed a control. `[main-thread]`
    fn gesture_begin(&self, id: ParamId);

    /// The user let go of a control. `[main-thread]`
    fn gesture_end(&self, id: ParamId);

    /// The plug-in set `id` to `plain` itself. The value is a **plain**
    /// (real-world) value, never normalised — abi-v1 §11.2. `[main-thread]`
    fn changed(&self, id: ParamId, plain: f64);

    /// Part of the parameter model changed and the host's cache is stale.
    /// `[main-thread]`
    fn rescan(&self, flags: RescanFlags);
}

/// Latency change notification. `daux.host.latency/1`. `[main-thread]`
///
/// Reporting latency is a two-step dance: the plug-in updates the value its own
/// `daux.latency/1` extension reports, then calls this so the host re-reads it
/// and re-aligns delay compensation. Most hosts only tolerate this while the
/// plug-in is inactive; calling it from `process` is forbidden.
pub trait HostLatency: Send + Sync {
    /// The plug-in's latency is now `samples`. `[main-thread]`
    fn set_samples(&self, samples: u32);
}

/// Tail length change notification. `daux.host.tail/1`. `[main-thread]`
///
/// The new length is read back from the plug-in's `daux.tail/1` extension, so
/// this is a bare "ask me again".
pub trait HostTail: Send + Sync {
    /// The plug-in's tail length changed. `[main-thread]`
    fn changed(&self);
}

/// Off-thread work scheduling. `daux.host.worker/1`. `[any-thread]`
///
/// The escape hatch for work the audio thread must not do: loading a file,
/// building a table, allocating a bigger buffer. [`schedule`](HostWorker::schedule)
/// is required to be real-time safe — non-blocking, allocation-free, and
/// returning `false` rather than waiting when the host's queue is full.
///
/// The plug-in never waits for the result. The worker eventually runs
/// `on_worker(task)` on a host thread, and the result travels back through a
/// lock-free queue the plug-in owns.
pub trait HostWorker: Send + Sync {
    /// Queues `task` to run off the audio thread; `false` when the host's queue
    /// is full, which is a normal condition. `[audio-thread]`
    fn schedule(&self, task: TaskId) -> bool;
}

/// Editor window negotiation. `daux.host.gui/1`. `[main-thread]`
///
/// Sizes are **physical pixels**, matching abi-v1 §11.4. Every method may be
/// refused by the host; a plug-in must stay usable when it is.
pub trait HostGui: Send + Sync {
    /// Asks the host to resize the editor window to `w` × `h` physical pixels.
    /// `[main-thread]`
    fn request_resize(&self, w: u32, h: u32) -> bool;

    /// Asks the host to show the editor window. `[main-thread]`
    fn request_show(&self) -> bool;

    /// Asks the host to hide the editor window. `[main-thread]`
    ///
    /// Defaults to "refused" because ABI v1 marks it optional and several hosts
    /// do not implement it.
    fn request_hide(&self) -> bool {
        false
    }

    /// Tells the host the plug-in closed its own editor. `destroyed` is `true`
    /// when the editor object is gone, `false` when it is merely hidden.
    /// `[main-thread]`
    fn closed(&self, destroyed: bool);
}

/// Periodic main-thread callbacks. `daux.host.timer/1`. `[main-thread]`
///
/// The only sanctioned way for an editor to get a repaint tick: a plug-in must
/// never spawn its own UI thread. `period_ms` is a request, not a guarantee —
/// hosts clamp it, and a starved host may skip ticks entirely.
pub trait HostTimer: Send + Sync {
    /// Registers a timer firing roughly every `period_ms` milliseconds, or
    /// `None` when the host refuses. `[main-thread]`
    fn register(&self, period_ms: u32) -> Option<TimerId>;

    /// Cancels a timer previously returned by
    /// [`register`](HostTimer::register). Unregistering an unknown or
    /// already-cancelled id must be a no-op, never a panic. `[main-thread]`
    fn unregister(&self, id: TimerId);
}

/// Bundle-relative resource access. `[main-thread]`
///
/// Plug-ins do not know where they were installed and must never build absolute
/// paths. A `logical_path` is always relative to the bundle's resource root and
/// uses `/` separators, e.g. `"skins/dark.png"`. The host — in practice
/// `daux-bundle` — resolves it and rejects anything that would escape the
/// bundle: `..`, absolute paths, drive letters, Windows device names and
/// symlink escapes all fail rather than reaching outside.
///
/// Every method does file I/O. None of them may be called from `process`.
pub trait HostResources: Send + Sync {
    /// Reads a resource whole. `[main-thread]` — blocks and allocates.
    fn read(&self, logical_path: &str) -> io::Result<Vec<u8>>;

    /// Reads a UTF-8 resource whole. `[main-thread]` — blocks and allocates.
    ///
    /// The default implementation reads the bytes and validates them, reporting
    /// [`io::ErrorKind::InvalidData`] for anything that is not UTF-8.
    fn read_to_string(&self, logical_path: &str) -> io::Result<String> {
        let bytes = self.read(logical_path)?;
        String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// `true` when the resource exists and is readable. `[main-thread]`
    fn exists(&self, logical_path: &str) -> bool;
}

/// Thread identification. `[any-thread]`
///
/// Backs `DauxHostApiV1::is_main_thread` / `is_audio_thread`, both optional in
/// ABI v1. Use it for debug assertions and for deciding whether work has to be
/// deferred — never as a lock. Both answers may be `false` at once: a plug-in
/// can legitimately be called on a worker or UI thread that is neither.
pub trait ThreadCheck: Send + Sync {
    /// `true` when the caller is on the host's main/UI thread. `[any-thread]`
    fn is_main_thread(&self) -> bool;

    /// `true` when the caller is on an audio thread. `[any-thread]` — must be
    /// non-blocking and allocation-free, because debug assertions call it from
    /// `process`.
    fn is_audio_thread(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A `HostResources` that only implements the two required methods, to prove
    /// the provided `read_to_string` works and reports bad UTF-8 cleanly.
    struct Fake(&'static [u8]);

    impl HostResources for Fake {
        fn read(&self, logical_path: &str) -> io::Result<Vec<u8>> {
            if logical_path == "there" {
                Ok(self.0.to_vec())
            } else {
                Err(io::Error::new(io::ErrorKind::NotFound, "no such resource"))
            }
        }

        fn exists(&self, logical_path: &str) -> bool {
            logical_path == "there"
        }
    }

    #[test]
    fn read_to_string_validates_utf8() {
        let good = Fake(b"skin { }");
        assert_eq!(good.read_to_string("there").unwrap(), "skin { }");
        assert_eq!(
            good.read_to_string("elsewhere").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        let bad = Fake(&[0xff, 0xfe, 0x00]);
        assert_eq!(
            bad.read_to_string("there").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(bad.read("there").is_ok(), "the bytes themselves are fine");
        assert!(bad.exists("there"));
        assert!(!bad.exists("elsewhere"));
    }

    /// A host that implements every trait at once — the shape a real adapter
    /// has — recording what it was asked to do.
    #[derive(Default)]
    struct Recorder {
        log: Mutex<Vec<String>>,
        gestures: Mutex<Vec<(ParamId, bool)>>,
    }

    impl HostLog for Recorder {
        fn log(&self, level: LogLevel, msg: &str) {
            self.log.lock().unwrap().push(format!("{level}:{msg}"));
        }
    }

    impl HostParams for Recorder {
        fn gesture_begin(&self, id: ParamId) {
            self.gestures.lock().unwrap().push((id, true));
        }
        fn gesture_end(&self, id: ParamId) {
            self.gestures.lock().unwrap().push((id, false));
        }
        fn changed(&self, id: ParamId, plain: f64) {
            self.log.lock().unwrap().push(format!("{id:?}={plain}"));
        }
        fn rescan(&self, flags: RescanFlags) {
            self.log.lock().unwrap().push(format!("rescan {flags:?}"));
        }
    }

    impl HostGui for Recorder {
        fn request_resize(&self, _w: u32, _h: u32) -> bool {
            true
        }
        fn request_show(&self) -> bool {
            true
        }
        fn closed(&self, _destroyed: bool) {}
    }

    #[test]
    fn one_object_can_serve_several_traits() {
        let host = Recorder::default();
        let log: &dyn HostLog = &host;
        let params: &dyn HostParams = &host;
        let gui: &dyn HostGui = &host;

        log.log(LogLevel::Warn, "hello");
        params.gesture_begin(ParamId(3));
        params.changed(ParamId(3), -6.0);
        params.gesture_end(ParamId(3));
        params.rescan(RescanFlags::VALUES);
        assert!(gui.request_resize(800, 600));
        assert!(gui.request_show());
        assert!(!gui.request_hide(), "the default is to refuse");
        gui.closed(true);

        assert_eq!(
            *host.log.lock().unwrap(),
            [
                "warn:hello".to_owned(),
                "ParamId(3)=-6".to_owned(),
                "rescan RescanFlags(VALUES)".to_owned(),
            ]
        );
        assert_eq!(
            *host.gestures.lock().unwrap(),
            [(ParamId(3), true), (ParamId(3), false)]
        );
    }

    #[test]
    fn every_service_trait_is_object_safe_and_shareable() {
        const fn assert_send_sync<T: Send + Sync + ?Sized>() {}
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
