//! The one-instance mutual exclusion the CLAP threading model forces on us.
//!
//! CLAP annotates each of its entry points `[main-thread]` or `[audio-thread]` and allows
//! the two to run **at the same time** for one instance: a host may call
//! `clap_plugin_params::get_value` from its UI thread while `clap_plugin::process` runs on
//! its audio thread. The DAUx object model does not permit that — every path to a plug-in
//! goes through `&mut PluginInstance`, because `DauxPlugin::processor` and
//! `DauxPlugin::controller` both take `&mut self`. Two live `&mut` to one plug-in is
//! undefined behaviour no matter how well the host behaves, so the adapter has to serialise.
//!
//! [`InstanceLock`] is that serialisation, with the priorities an audio plug-in needs:
//!
//! * **The audio thread never waits.** [`InstanceLock::try_lock`] is one
//!   compare-exchange. If it fails, `process` silences its outputs and returns
//!   `CLAP_PROCESS_ERROR` — a glitch, but a defined one, and it allocates nothing and
//!   blocks on nothing (CLAUDE.md rule 1).
//! * **The main thread waits, briefly.** [`InstanceLock::lock_main`] spins and yields;
//!   CLAP explicitly tolerates blocking on `[main-thread]`. The audio thread holds the lock
//!   for one block, so the wait is bounded by the block period in practice.
//! * **Nesting is refused, not deadlocked.** A host that calls back into the plug-in from
//!   inside a plug-in→host call (`request_resize` → `set_size` is the classic) would
//!   otherwise spin against itself forever. A thread-local flag saying "this thread is
//!   already inside an instance" turns that into an immediate refusal.
//!
//! The residual cost is real and worth stating: a main-thread parameter read that lands
//! inside a `process` call waits for it. The way to remove that cost is a shared,
//! interior-mutable handle to the parameter set that does not go through
//! `&mut PluginInstance` — see the crate docs.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, Ordering};

/// How many times [`InstanceLock::lock_main`] spins before yielding the rest of its
/// timeslice. Small enough that a lost race costs a few hundred nanoseconds, large enough
/// that the common uncontended case never reaches the scheduler.
const SPINS_BEFORE_YIELD: u32 = 64;

/// How many yields [`InstanceLock::lock_main`] tolerates before giving up.
///
/// Reaching this means something is wrong — an audio thread wedged inside `process`, or a
/// host holding the lock from a thread that will never release it. Refusing the call is
/// strictly better than hanging the host's UI forever.
const MAX_YIELDS: u32 = 10_000;

thread_local! {
    /// `true` while this thread is inside *some* instance lock.
    ///
    /// A flag rather than a set of lock addresses: an adapter never has a legitimate reason
    /// to hold two instance locks at once, so refusing the second is both sound and
    /// deadlock-free, and one `Cell<bool>` with a `const` initialiser and no destructor
    /// compiles to a bare thread-local read — which is what makes this usable from the
    /// audio thread.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

/// Mutual exclusion over one plug-in instance. `[any-thread]`
#[derive(Debug, Default)]
pub struct InstanceLock {
    /// `true` while some thread holds the lock.
    locked: AtomicBool,
}

impl InstanceLock {
    /// `[any-thread]` A new, unlocked lock.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    /// `[audio-thread]` Takes the lock, or gives up immediately.
    ///
    /// One `compare_exchange` and one thread-local read: no allocation, no blocking, no
    /// syscall. Returns `None` when another thread holds the lock, or when this thread is
    /// already inside an instance.
    #[must_use]
    pub fn try_lock(&self) -> Option<LockGuard<'_>> {
        if INSIDE.get() {
            return None;
        }
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| {
                INSIDE.set(true);
                LockGuard { lock: self }
            })
    }

    /// `[main-thread]` Takes the lock, waiting for the audio thread if it has to.
    ///
    /// Returns `None` for a nested call, and `None` rather than hanging forever if the
    /// holder never releases.
    #[must_use]
    pub fn lock_main(&self) -> Option<LockGuard<'_>> {
        if INSIDE.get() {
            return None;
        }
        let mut yields = 0u32;
        loop {
            for _ in 0..SPINS_BEFORE_YIELD {
                if self
                    .locked
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    INSIDE.set(true);
                    return Some(LockGuard { lock: self });
                }
                core::hint::spin_loop();
            }
            yields += 1;
            if yields >= MAX_YIELDS {
                return None;
            }
            std::thread::yield_now();
        }
    }
}

/// Proof that the holder has exclusive access to the instance the lock guards.
///
/// Releasing happens in [`Drop`], including while a panic unwinds through the guard, which
/// is what keeps a panicking `process` from wedging every later call.
#[derive(Debug)]
pub struct LockGuard<'a> {
    /// The lock to release.
    lock: &'a InstanceLock,
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        INSIDE.set(false);
        self.lock.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn an_uncontended_lock_is_taken_and_released() {
        let lock = InstanceLock::new();
        {
            let guard = lock.try_lock();
            assert!(guard.is_some());
        }
        assert!(lock.try_lock().is_some(), "the guard must have released");
        assert!(lock.lock_main().is_some());
    }

    #[test]
    fn re_entrancy_is_refused_rather_than_deadlocked() {
        let lock = InstanceLock::new();
        let _outer = lock.try_lock().expect("first acquisition succeeds");
        assert!(
            lock.try_lock().is_none(),
            "a second try_lock on the same thread must refuse"
        );
        // The important half: `lock_main` must not spin against itself for ten thousand
        // yields. If nesting detection regressed, this line would hang the test.
        assert!(lock.lock_main().is_none());
    }

    #[test]
    fn a_second_instance_is_refused_while_the_first_is_held() {
        let a = InstanceLock::new();
        let b = InstanceLock::new();
        let outer = a.try_lock().expect("a is free");
        assert!(
            b.try_lock().is_none(),
            "nesting two instance locks must be refused, not deadlocked later"
        );
        drop(outer);
        assert!(b.try_lock().is_some(), "b is available once a is released");
    }

    #[test]
    fn the_audio_thread_never_waits_for_a_holder() {
        let lock = Arc::new(InstanceLock::new());
        let held = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));

        let holder = {
            let lock = Arc::clone(&lock);
            let held = Arc::clone(&held);
            let release = Arc::clone(&release);
            std::thread::spawn(move || {
                let _guard = lock.try_lock().expect("uncontended");
                held.store(true, Ordering::Release);
                while !release.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
            })
        };

        while !held.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        assert!(
            lock.try_lock().is_none(),
            "try_lock must fail while another thread holds the lock"
        );
        release.store(true, Ordering::Release);
        holder.join().expect("holder thread");
        assert!(lock.try_lock().is_some());
    }

    #[test]
    fn the_lock_actually_excludes_under_contention() {
        let lock = Arc::new(InstanceLock::new());
        let inside = Arc::new(AtomicU32::new(0));
        let overlaps = Arc::new(AtomicU32::new(0));
        let acquisitions = Arc::new(AtomicU32::new(0));

        let threads: Vec<_> = (0..4)
            .map(|_| {
                let lock = Arc::clone(&lock);
                let inside = Arc::clone(&inside);
                let overlaps = Arc::clone(&overlaps);
                let acquisitions = Arc::clone(&acquisitions);
                std::thread::spawn(move || {
                    for _ in 0..2_000 {
                        let Some(_guard) = lock.lock_main() else {
                            continue;
                        };
                        acquisitions.fetch_add(1, Ordering::Relaxed);
                        if inside.fetch_add(1, Ordering::AcqRel) != 0 {
                            overlaps.fetch_add(1, Ordering::Relaxed);
                        }
                        inside.fetch_sub(1, Ordering::AcqRel);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("worker thread");
        }

        assert_eq!(overlaps.load(Ordering::Relaxed), 0, "the lock let two in");
        assert_eq!(acquisitions.load(Ordering::Relaxed), 8_000);
    }

    #[test]
    fn a_panic_through_the_guard_still_releases() {
        let lock = InstanceLock::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = lock.try_lock().expect("uncontended");
            panic!("the plug-in went wrong");
        }));
        assert!(result.is_err());
        assert!(
            lock.try_lock().is_some(),
            "unwinding through the guard must release the lock"
        );
    }
}
