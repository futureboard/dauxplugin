//! Allocation counting: the tripwire that turns "must not allocate" into a test.
//!
//! [`CountingAllocator`] is a thin wrapper around the system allocator that adds
//! two counters and changes nothing else. It is **never** installed
//! automatically: a `#[global_allocator]` is a whole-program decision, so only a
//! test harness (or a developer build that wants the check) may opt in:
//!
//! ```
//! use daux_rt::CountingAllocator;
//!
//! #[global_allocator]
//! static ALLOCATOR: CountingAllocator = CountingAllocator;
//! # fn main() {}
//! ```
//!
//! With it installed, [`AllocGuard::scope`] reports how many allocations a piece
//! of code made on the calling thread:
//!
//! ```
//! use daux_rt::{AllocGuard, FixedVec};
//!
//! let mut buffer = FixedVec::with_capacity(8);       // allocates, on purpose
//! let ((), allocations) = AllocGuard::scope(|| {
//!     buffer.push(1.0f32).unwrap();                  // must not
//! });
//! # if daux_rt::counting_allocator_installed() {
//! assert_eq!(allocations, 0);
//! # }
//! ```
//!
//! Without it installed every counter stays at zero, so an assertion on the
//! count would pass vacuously. Tests that care should gate on
//! [`counting_allocator_installed`].

use core::alloc::{GlobalAlloc, Layout};
use core::cell::Cell;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::alloc::System;

/// Process-wide number of allocations.
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
/// Process-wide number of deallocations.
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Per-thread allocation count. `const`-initialised and `Drop`-free, so
    /// touching it from inside the allocator cannot recurse into the allocator.
    static THREAD_ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

#[inline]
fn record_allocation() {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    // `try_with`: during thread teardown the slot may be gone, and an allocator
    // that panics is far worse than a slightly low count.
    let _ = THREAD_ALLOCATIONS.try_with(|count| count.set(count.get().wrapping_add(1)));
}

/// A `#[global_allocator]`-compatible wrapper around the system allocator that
/// counts allocations and deallocations.
///
/// Every call is forwarded to [`std::alloc::System`] unchanged; the only added
/// work is a relaxed atomic increment and a thread-local increment, so an
/// instrumented build behaves like an uninstrumented one apart from the counters.
/// `realloc` counts as one allocation, because it may move memory and is exactly
/// as forbidden on the audio thread as a fresh allocation.
///
/// Installing this is the harness's choice. Nothing in `daux-rt` installs it, so
/// production builds are untouched.
///
/// [any-thread]
pub struct CountingAllocator;

// SAFETY: every method forwards to `std::alloc::System`, which is a correct
// `GlobalAlloc`, with the same layout and pointer arguments it was given, and
// returns exactly what `System` returned. The counters are plain integers that
// never touch the allocator themselves (the thread-local is `const`-initialised
// and has no destructor), so no reentrancy is introduced. Memory is therefore
// allocated, resized and freed under precisely `System`'s contract.
unsafe impl GlobalAlloc for CountingAllocator {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: `layout` is forwarded unchanged from our caller, who owes
        // `GlobalAlloc::alloc` a non-zero-sized, valid layout.
        unsafe { System.alloc(layout) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: as in `alloc`; `layout` comes from our caller unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `ptr` was returned by one of our allocation methods — which all
        // delegate to `System` — and `layout` is the one it was allocated with, as
        // `GlobalAlloc::dealloc` requires of the caller.
        unsafe { System.dealloc(ptr, layout) };
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation();
        // SAFETY: `ptr`/`layout` identify a live block allocated through `System`
        // and `new_size` satisfies `GlobalAlloc::realloc`'s requirements, both
        // guaranteed by our caller; all three are forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Total number of allocations made by the whole process since start, or `0`
/// when [`CountingAllocator`] is not installed. [any-thread]
#[inline]
#[must_use]
pub fn alloc_count() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}

/// Total number of deallocations made by the whole process since start, or `0`
/// when [`CountingAllocator`] is not installed. [any-thread]
#[inline]
#[must_use]
pub fn dealloc_count() -> usize {
    DEALLOCATIONS.load(Ordering::Relaxed)
}

/// Number of allocations made by the calling thread since it started, or `0`
/// when [`CountingAllocator`] is not installed.
///
/// This is what [`AllocGuard`] measures: a process-wide counter would be useless
/// while other threads are running. [any-thread]
#[inline]
#[must_use]
pub fn thread_alloc_count() -> usize {
    THREAD_ALLOCATIONS.try_with(Cell::get).unwrap_or(0)
}

/// Whether [`CountingAllocator`] is the active global allocator.
///
/// Determined by allocating once and watching the counter, so it is honest
/// rather than declarative. Allocates; call it from a test, not from `process`.
/// [main-thread]
#[must_use]
pub fn counting_allocator_installed() -> bool {
    let before = thread_alloc_count();
    let probe: Vec<u8> = Vec::with_capacity(64);
    let probe = core::hint::black_box(probe);
    drop(probe);
    thread_alloc_count() != before
}

/// Counts the allocations made on the calling thread while it is alive.
///
/// ```
/// use daux_rt::AllocGuard;
///
/// let guard = AllocGuard::new();
/// let data = vec![0u8; 16];
/// let allocations = guard.count();
/// drop(data);
/// # if daux_rt::counting_allocator_installed() {
/// assert_eq!(allocations, 1);
/// # }
/// ```
///
/// [any-thread]
#[derive(Debug)]
pub struct AllocGuard {
    start: usize,
}

impl AllocGuard {
    /// Starts counting from the calling thread's current allocation count.
    /// [any-thread]
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: thread_alloc_count(),
        }
    }

    /// Allocations made on this thread since the guard was created.
    /// [any-thread]
    #[inline]
    #[must_use]
    pub fn count(&self) -> usize {
        thread_alloc_count().wrapping_sub(self.start)
    }

    /// Restarts the count from now. [any-thread]
    #[inline]
    pub fn reset(&mut self) {
        self.start = thread_alloc_count();
    }

    /// Runs `f` and returns its result together with the number of allocations
    /// it made on this thread.
    ///
    /// The count is `0` when [`CountingAllocator`] is not installed, so a test
    /// that must not pass vacuously should also assert
    /// [`counting_allocator_installed`]. [any-thread]
    #[inline]
    pub fn scope<R>(f: impl FnOnce() -> R) -> (R, usize) {
        let guard = Self::new();
        let result = f();
        let allocations = guard.count();
        (result, allocations)
    }

    /// Runs `f` and panics if it allocated on this thread.
    ///
    /// # Panics
    ///
    /// Panics if `f` made at least one allocation. [any-thread]
    #[inline]
    #[track_caller]
    pub fn assert_no_alloc<R>(f: impl FnOnce() -> R) -> R {
        let (result, allocations) = Self::scope(f);
        assert_eq!(allocations, 0, "daux-rt: real-time code allocated");
        result
    }
}

impl Default for AllocGuard {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AllocGuard, alloc_count, counting_allocator_installed, dealloc_count, thread_alloc_count,
    };

    #[test]
    fn the_test_harness_installed_the_counting_allocator() {
        // Every other allocation assertion in this crate is only meaningful
        // because of this; if it ever regresses, this test says so directly.
        assert!(
            counting_allocator_installed(),
            "daux-rt's own tests must run under CountingAllocator"
        );
        assert!(alloc_count() > 0);
        assert!(dealloc_count() > 0);
    }

    /// One heap allocation, written so clippy cannot suggest an array instead —
    /// allocating is the entire point of these tests.
    fn allocate(bytes: usize) -> Vec<u8> {
        Vec::with_capacity(bytes)
    }

    #[test]
    fn a_scope_counts_the_allocations_it_contains() {
        let (value, allocations) = AllocGuard::scope(|| allocate(3).capacity());
        assert_eq!(value, 3);
        assert_eq!(allocations, 1);
    }

    #[test]
    fn a_scope_that_allocates_nothing_counts_zero() {
        let mut buffer = [0u8; 64];
        let ((), allocations) = AllocGuard::scope(|| {
            for (i, slot) in buffer.iter_mut().enumerate() {
                *slot = i as u8;
            }
        });
        assert_eq!(allocations, 0);
        assert_eq!(buffer[63], 63);
    }

    #[test]
    fn nested_scopes_count_independently() {
        let ((), outer) = AllocGuard::scope(|| {
            let _a = allocate(8);
            let ((), inner) = AllocGuard::scope(|| {
                let _b = allocate(8);
                let _c = allocate(8);
            });
            assert_eq!(inner, 2);
        });
        assert_eq!(outer, 3);
    }

    #[test]
    fn a_guard_can_be_reset_and_read_repeatedly() {
        let mut guard = AllocGuard::new();
        let _a = allocate(8);
        assert_eq!(guard.count(), 1);
        let _b = allocate(8);
        assert_eq!(guard.count(), 2);
        guard.reset();
        assert_eq!(guard.count(), 0);
        let _c = allocate(8);
        assert_eq!(guard.count(), 1);
        assert_eq!(AllocGuard::default().count(), 0);
    }

    #[test]
    fn reallocation_counts_as_an_allocation() {
        let (_, allocations) = AllocGuard::scope(|| {
            let mut v = allocate(1);
            v.extend_from_slice(&[1, 2, 3]); // outgrows the capacity: one realloc
            v
        });
        assert!(
            allocations >= 2,
            "a realloc must be counted, got {allocations}"
        );
    }

    #[test]
    fn assert_no_alloc_returns_the_value() {
        let value = AllocGuard::assert_no_alloc(|| 21 * 2);
        assert_eq!(value, 42);
    }

    #[test]
    #[should_panic(expected = "real-time code allocated")]
    fn assert_no_alloc_catches_an_allocation() {
        AllocGuard::assert_no_alloc(|| vec![0u8; 4]);
    }

    #[test]
    fn counts_are_per_thread() {
        const CHILD_ALLOCATIONS: usize = 1_000;

        let before = thread_alloc_count();
        let global_before = alloc_count();
        let child_delta = std::thread::spawn(|| {
            let base = thread_alloc_count();
            for _ in 0..CHILD_ALLOCATIONS {
                drop(core::hint::black_box(allocate(32)));
            }
            thread_alloc_count() - base
        })
        .join()
        .unwrap();
        let parent_delta = thread_alloc_count() - before;

        assert_eq!(
            child_delta, CHILD_ALLOCATIONS,
            "the child counted its own work"
        );
        assert!(
            parent_delta < CHILD_ALLOCATIONS,
            "another thread's allocations were attributed to this one: {parent_delta}"
        );
        assert!(
            alloc_count() - global_before >= CHILD_ALLOCATIONS,
            "the process-wide counter must see every thread"
        );
    }
}
