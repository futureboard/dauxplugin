//! Thread markers and the debug-only audio-thread tripwire.
//!
//! `abi-v1.md` §15 is explicit that a plug-in may not *assume* anything about the
//! thread it runs on: audio callbacks for one instance are never concurrent, but
//! they may migrate between OS threads from block to block. Nothing in this
//! module may therefore drive behaviour — it exists so that debug builds can
//! catch "this ran on the wrong thread" during development, and so that a host or
//! test harness can label the threads it owns.

use core::cell::Cell;
use core::fmt;

/// What a thread is for.
///
/// Purely descriptive: the class is whatever the code that owns the thread said
/// it is, and defaults to [`ThreadClass::Unknown`] on a thread nobody labelled.
///
/// [any-thread]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ThreadClass {
    /// The host's main/UI thread, where blocking is tolerated.
    Main,
    /// A real-time audio callback thread; §8's obligations apply with no exceptions.
    Audio,
    /// An editor thread that is not the host's main thread.
    Ui,
    /// A background worker running scheduled off-thread tasks.
    Worker,
    /// A plug-in scanner thread.
    Scanner,
    /// An inter-process communication pump.
    Ipc,
    /// Unlabelled: the default for every thread the SDK did not create.
    #[default]
    Unknown,
}

impl ThreadClass {
    /// Lower-case name of the class. [any-thread]
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ThreadClass::Main => "main",
            ThreadClass::Audio => "audio",
            ThreadClass::Ui => "ui",
            ThreadClass::Worker => "worker",
            ThreadClass::Scanner => "scanner",
            ThreadClass::Ipc => "ipc",
            ThreadClass::Unknown => "unknown",
        }
    }

    /// Whether real-time rules apply to this class. [any-thread]
    #[inline]
    #[must_use]
    pub const fn is_realtime(self) -> bool {
        matches!(self, ThreadClass::Audio)
    }
}

impl fmt::Display for ThreadClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

thread_local! {
    /// `const`-initialised and `Drop`-free on purpose: that compiles to a plain
    /// `#[thread_local]` slot, so reading it neither allocates nor runs lazy
    /// initialisation and is safe on the audio thread.
    static THREAD_CLASS: Cell<ThreadClass> = const { Cell::new(ThreadClass::Unknown) };
}

/// The calling thread's class, or [`ThreadClass::Unknown`] when it was never
/// labelled.
///
/// Allocation-free and non-blocking. [any-thread]
#[inline]
#[must_use]
pub fn current_thread_class() -> ThreadClass {
    // `try_with` rather than `with`: during thread teardown the slot may already
    // be gone, and this must never panic.
    THREAD_CLASS
        .try_with(Cell::get)
        .unwrap_or(ThreadClass::Unknown)
}

/// Labels the calling thread.
///
/// Call this once at the top of an audio callback, a worker loop or a scanner
/// thread. Allocation-free and non-blocking. [any-thread]
#[inline]
pub fn set_current_thread_class(class: ThreadClass) {
    let _ = THREAD_CLASS.try_with(|slot| slot.set(class));
}

/// Labels the calling thread and returns its previous class. [any-thread]
#[inline]
pub fn replace_current_thread_class(class: ThreadClass) -> ThreadClass {
    THREAD_CLASS
        .try_with(|slot| slot.replace(class))
        .unwrap_or(ThreadClass::Unknown)
}

/// Restores the previous [`ThreadClass`] when dropped.
///
/// Useful in tests and in nested host callbacks, where a thread temporarily acts
/// as something other than what it was labelled.
///
/// [any-thread]
#[derive(Debug)]
pub struct ThreadClassGuard {
    previous: ThreadClass,
}

impl ThreadClassGuard {
    /// Labels the calling thread until the guard is dropped. [any-thread]
    #[must_use]
    pub fn enter(class: ThreadClass) -> Self {
        Self {
            previous: replace_current_thread_class(class),
        }
    }
}

impl Drop for ThreadClassGuard {
    fn drop(&mut self) {
        set_current_thread_class(self.previous);
    }
}

/// Panics if the calling thread is labelled as something other than
/// [`ThreadClass::Audio`].
///
/// An [`Unknown`](ThreadClass::Unknown) thread passes: most hosts never label
/// their threads, and a tripwire that fires on every unhosted run is a tripwire
/// people switch off. Label the threads you control with
/// [`set_current_thread_class`] to make this effective.
///
/// This is the implementation behind [`rt_assert_audio_thread!`](crate::rt_assert_audio_thread),
/// which is the form to use because it disappears entirely in release builds.
///
/// # Panics
///
/// Panics when the current class is known and is not `Audio`.
///
/// [any-thread]
#[inline]
#[track_caller]
pub fn assert_audio_thread(context: &str) {
    let class = current_thread_class();
    if !matches!(class, ThreadClass::Audio | ThreadClass::Unknown) {
        wrong_thread(class, context);
    }
}

/// Split out and marked `#[cold]` so the check itself stays a predictable,
/// branch-free-ish comparison on the hot path.
#[cold]
#[inline(never)]
#[track_caller]
fn wrong_thread(class: ThreadClass, context: &str) -> ! {
    if context.is_empty() {
        panic!("daux-rt: expected the audio thread, but this thread is labelled '{class}'");
    }
    panic!("daux-rt: expected the audio thread, but this thread is labelled '{class}': {context}");
}

/// Asserts, in debug builds only, that the calling thread is the audio thread.
///
/// Expands to nothing at all in release builds — the `#[cfg]` is evaluated in the
/// *calling* crate, so a release plug-in carries no check and no string.
///
/// Takes an optional `&str` note; formatting is deliberately not supported,
/// because `format!` allocates and this macro's whole point is to live on the
/// audio thread.
///
/// ```
/// use daux_rt::{ThreadClass, rt_assert_audio_thread, set_current_thread_class};
///
/// set_current_thread_class(ThreadClass::Audio);
/// rt_assert_audio_thread!();
/// rt_assert_audio_thread!("Processor::process");
/// ```
///
/// [audio-thread]
#[macro_export]
macro_rules! rt_assert_audio_thread {
    () => {{
        #[cfg(debug_assertions)]
        {
            $crate::assert_audio_thread("");
        }
    }};
    ($context:expr $(,)?) => {{
        #[cfg(debug_assertions)]
        {
            $crate::assert_audio_thread($context);
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::{
        ThreadClass, ThreadClassGuard, assert_audio_thread, current_thread_class,
        replace_current_thread_class, set_current_thread_class,
    };
    use crate::alloc_probe::AllocGuard;

    #[test]
    fn unlabelled_threads_are_unknown() {
        // A fresh thread, so no other test can have labelled it.
        let class = std::thread::spawn(current_thread_class).join().unwrap();
        assert_eq!(class, ThreadClass::Unknown);
        assert_eq!(ThreadClass::default(), ThreadClass::Unknown);
    }

    #[test]
    fn the_class_is_per_thread() {
        std::thread::spawn(|| {
            set_current_thread_class(ThreadClass::Audio);
            assert_eq!(current_thread_class(), ThreadClass::Audio);
            // A thread spawned from here must not inherit the label.
            let inner = std::thread::spawn(current_thread_class).join().unwrap();
            assert_eq!(inner, ThreadClass::Unknown);
            assert_eq!(current_thread_class(), ThreadClass::Audio);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn replace_returns_the_previous_class() {
        std::thread::spawn(|| {
            assert_eq!(
                replace_current_thread_class(ThreadClass::Ui),
                ThreadClass::Unknown
            );
            assert_eq!(
                replace_current_thread_class(ThreadClass::Worker),
                ThreadClass::Ui
            );
            assert_eq!(current_thread_class(), ThreadClass::Worker);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn the_guard_restores_the_previous_class() {
        std::thread::spawn(|| {
            set_current_thread_class(ThreadClass::Main);
            {
                let _guard = ThreadClassGuard::enter(ThreadClass::Audio);
                assert_eq!(current_thread_class(), ThreadClass::Audio);
                let _nested = ThreadClassGuard::enter(ThreadClass::Worker);
                assert_eq!(current_thread_class(), ThreadClass::Worker);
            }
            assert_eq!(current_thread_class(), ThreadClass::Main);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn names_and_realtime_flags() {
        assert_eq!(ThreadClass::Audio.as_str(), "audio");
        assert_eq!(ThreadClass::Ipc.to_string(), "ipc");
        assert!(ThreadClass::Audio.is_realtime());
        assert!(!ThreadClass::Main.is_realtime());
        assert!(!ThreadClass::Unknown.is_realtime());
    }

    #[test]
    fn the_assertion_passes_on_audio_and_unknown_threads() {
        std::thread::spawn(|| {
            assert_audio_thread("unlabelled threads are tolerated");
            rt_assert_audio_thread!();
            set_current_thread_class(ThreadClass::Audio);
            assert_audio_thread("");
            rt_assert_audio_thread!();
            rt_assert_audio_thread!("with a note");
            // Also valid in expression position, in both profiles: the macro
            // evaluates to `()` in debug and to an empty block in release.
            let as_expression = || rt_assert_audio_thread!();
            as_expression();
        })
        .join()
        .unwrap();
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "expected the audio thread")]
    fn the_assertion_fires_on_a_known_non_audio_thread() {
        set_current_thread_class(ThreadClass::Main);
        rt_assert_audio_thread!("Processor::process");
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn the_macro_compiles_away_in_release() {
        set_current_thread_class(ThreadClass::Main);
        rt_assert_audio_thread!("this must not panic in release");
    }

    #[test]
    fn class_access_does_not_allocate() {
        let (class, allocations) = AllocGuard::scope(|| {
            set_current_thread_class(ThreadClass::Audio);
            let mut class = ThreadClass::Unknown;
            for _ in 0..10_000 {
                class = current_thread_class();
                assert_audio_thread("");
            }
            class
        });
        set_current_thread_class(ThreadClass::Unknown);
        assert_eq!(allocations, 0, "thread-class access allocated");
        assert_eq!(class, ThreadClass::Audio);
    }
}
