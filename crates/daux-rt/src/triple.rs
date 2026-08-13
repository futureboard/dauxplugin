//! Triple buffer: the audio → UI snapshot channel.

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Bits 0..=1 of the shared word hold the index of the middle slot.
const INDEX_MASK: usize = 0b11;
/// Bit 2 marks the middle slot as holding a value the reader has not taken yet.
const DIRTY: usize = 0b100;

/// Three slots plus one atomic word. At any instant one slot belongs to the
/// writer, one belongs to the reader, and the third — identified by the shared
/// word — is the hand-off point. Ownership changes only by swapping the shared
/// word, so neither side ever waits for the other.
struct TripleShared<T> {
    slots: [UnsafeCell<T>; 3],
    shared: AtomicUsize,
}

// SAFETY: exactly one slot index is owned by each side at any time and ownership
// moves between threads only through the `AcqRel` swap of `shared`, which is a
// transfer of the value, i.e. `T: Send`. The writer never touches the reader's
// index and vice versa, and no `&T` is ever handed to two threads at once (the
// writer keeps its own staging copy instead of reading a published slot), so
// `T: Sync` is not required.
unsafe impl<T: Send> Send for TripleShared<T> {}
// SAFETY: see the `Send` impl; sharing `&TripleShared<T>` between exactly one
// writer and one reader is the only supported use, and the two halves are handed
// out as separate owned objects that cannot be duplicated.
unsafe impl<T: Send> Sync for TripleShared<T> {}

/// Factory for the audio → UI snapshot channel.
///
/// A triple buffer is the right tool when the producer must never wait and the
/// consumer only cares about the newest value: meters, spectra, playhead
/// positions. Values that must not be missed belong in an
/// [`SpscRingBuffer`](crate::SpscRingBuffer) instead.
///
/// ```
/// use daux_rt::TripleBuffer;
///
/// let (mut writer, mut reader) = TripleBuffer::new(0.0f32);
/// assert!(!reader.has_update());
/// writer.write(-6.0);
/// writer.write(-3.0);           // the writer never waits for the reader
/// assert!(reader.has_update());
/// assert_eq!(*reader.read(), -3.0);   // and the reader sees only the newest value
/// ```
pub struct TripleBuffer<T> {
    marker: PhantomData<fn() -> T>,
}

impl<T: Clone + Send> TripleBuffer<T> {
    /// Allocates the three slots, fills them with `initial`, and splits the
    /// buffer into its two halves.
    ///
    /// This is the only allocating operation on the buffer.
    ///
    /// [main-thread]
    // The contract fixes this constructor as `new(initial) -> (writer, reader)`;
    // there is no single `Self` to return because the halves are the API.
    #[allow(clippy::new_ret_no_self)]
    #[must_use]
    pub fn new(initial: T) -> (TripleWriter<T>, TripleReader<T>) {
        let shared = Arc::new(TripleShared {
            slots: [
                UnsafeCell::new(initial.clone()),
                UnsafeCell::new(initial.clone()),
                UnsafeCell::new(initial.clone()),
            ],
            // Slot 1 is the hand-off point, and it is clean: the reader has
            // nothing new to take until the writer publishes.
            shared: AtomicUsize::new(1),
        });
        (
            TripleWriter {
                shared: Arc::clone(&shared),
                staging: initial,
                write_index: 0,
            },
            TripleReader {
                shared,
                read_index: 2,
            },
        )
    }
}

/// The writing half of a [`TripleBuffer`]. Lives on the audio thread.
///
/// The writer keeps its own copy of the current value (`staging`) so that
/// [`TripleWriter::with`] can read-modify-write without ever touching a slot the
/// reader might be reading. Publishing therefore copies `staging` into the back
/// slot with [`Clone::clone_from`], which is allocation-free for the plain data
/// snapshots this channel is meant for.
pub struct TripleWriter<T> {
    shared: Arc<TripleShared<T>>,
    /// Authoritative current value, private to the writer thread.
    staging: T,
    /// Index of the slot the writer owns; the reader never touches it.
    write_index: usize,
}

impl<T: Clone> TripleWriter<T> {
    /// Publishes `value` as the newest snapshot.
    ///
    /// Never blocks, never waits for the reader and never fails; an unread
    /// snapshot is simply overwritten. Allocation-free as long as
    /// `T::clone_from` is, which is guaranteed for `Copy` types and for any
    /// fixed-size value. [audio-thread]
    #[inline]
    pub fn write(&mut self, value: T) {
        self.staging = value;
        self.publish();
    }

    /// Mutates the current value in place and publishes the result.
    ///
    /// `f` receives the writer's private copy of the value it last published (or
    /// of the initial value), so read-modify-write updates such as "add this
    /// block's peak" behave the way they read. Same real-time properties as
    /// [`TripleWriter::write`]. [audio-thread]
    #[inline]
    pub fn with(&mut self, f: impl FnOnce(&mut T)) {
        f(&mut self.staging);
        self.publish();
    }

    /// Copies `staging` into the writer's slot and swaps that slot into the
    /// hand-off position, taking whatever slot was there in exchange.
    #[inline]
    fn publish(&mut self) {
        // SAFETY: `write_index` is owned exclusively by this writer — the reader
        // only ever reads its own index, and ownership of a slot changes only via
        // the swap below — so creating a `&mut T` to it cannot alias. The slot
        // holds an initialised value (all three start initialised and are only
        // ever overwritten), so `clone_from` is valid.
        unsafe { (*self.shared.slots[self.write_index].get()).clone_from(&self.staging) };
        // Release: the payload copy above must be visible to the reader before it
        // can observe the new index. Acquire: the slot we take back was owned by
        // the reader, and we must see its accesses as completed before we reuse it.
        let previous = self
            .shared
            .shared
            .swap(self.write_index | DIRTY, Ordering::AcqRel);
        self.write_index = previous & INDEX_MASK;
    }

    /// Borrows the value the writer last published. [audio-thread]
    #[inline]
    #[must_use]
    pub fn current(&self) -> &T {
        &self.staging
    }

    /// Whether the [`TripleReader`] has been dropped. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }
}

/// The reading half of a [`TripleBuffer`]. Lives on the UI or main thread.
pub struct TripleReader<T> {
    shared: Arc<TripleShared<T>>,
    /// Index of the slot the reader owns; the writer never touches it.
    read_index: usize,
}

impl<T> TripleReader<T> {
    /// Returns the most recent complete value.
    ///
    /// Takes the writer's latest publication if there is one, otherwise returns
    /// the value from the previous call. Never blocks, never tears and never
    /// fails. The borrow lasts until the next call, which is what keeps the
    /// reader from swapping a slot out from under a live reference. [any-thread]
    #[inline]
    pub fn read(&mut self) -> &T {
        // Relaxed is enough for the fast-path test: the swap below carries the
        // Acquire that actually orders the payload.
        if self.shared.shared.load(Ordering::Relaxed) & DIRTY != 0 {
            // Acquire: pairs with the writer's Release swap and makes the payload
            // it copied visible. Release: hands our old slot back to the writer.
            let previous = self.shared.shared.swap(self.read_index, Ordering::AcqRel);
            self.read_index = previous & INDEX_MASK;
        }
        // SAFETY: `read_index` is owned exclusively by this reader; the writer
        // never writes to it (it only writes to its own index) and ownership moves
        // only through the swap above. The slot has been initialised since
        // construction, and the returned borrow is tied to `&mut self`, so no swap
        // can happen while it is alive.
        unsafe { &*self.shared.slots[self.read_index].get() }
    }

    /// Returns the newest value only if the writer published since the last read.
    /// [any-thread]
    #[inline]
    pub fn read_if_updated(&mut self) -> Option<&T> {
        if self.has_update() {
            Some(self.read())
        } else {
            None
        }
    }

    /// Whether the writer has published a value the reader has not taken yet.
    /// [any-thread]
    #[inline]
    #[must_use]
    pub fn has_update(&self) -> bool {
        self.shared.shared.load(Ordering::Acquire) & DIRTY != 0
    }

    /// Whether the [`TripleWriter`] has been dropped. No further updates can
    /// arrive once this is `true`. [any-thread]
    #[inline]
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::TripleBuffer;
    use crate::alloc_probe::AllocGuard;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn reader_starts_with_the_initial_value_and_no_update() {
        let (_w, mut r) = TripleBuffer::new(42u32);
        assert!(!r.has_update());
        assert_eq!(*r.read(), 42);
        assert_eq!(r.read_if_updated(), None);
    }

    #[test]
    fn reader_sees_only_the_newest_value() {
        let (mut w, mut r) = TripleBuffer::new(0u32);
        for i in 1..=100 {
            w.write(i);
        }
        assert!(r.has_update());
        assert_eq!(*r.read(), 100);
        assert!(!r.has_update());
        assert_eq!(*r.read(), 100, "a second read repeats the last value");
    }

    #[test]
    fn every_value_is_observable_when_the_reader_keeps_up() {
        let (mut w, mut r) = TripleBuffer::new(0u32);
        for i in 1..=100 {
            w.write(i);
            assert_eq!(r.read_if_updated().copied(), Some(i));
            assert_eq!(r.read_if_updated(), None);
        }
    }

    #[test]
    fn with_performs_a_read_modify_write() {
        let (mut w, mut r) = TripleBuffer::new(0u32);
        for _ in 0..10 {
            w.with(|v| *v += 3);
        }
        assert_eq!(*r.read(), 30);
        assert_eq!(*w.current(), 30);
    }

    #[test]
    fn write_and_with_can_be_interleaved() {
        let (mut w, mut r) = TripleBuffer::new(0i32);
        w.with(|v| *v -= 5);
        assert_eq!(*r.read(), -5);
        w.write(100);
        w.with(|v| *v += 1);
        assert_eq!(*r.read(), 101);
    }

    #[test]
    fn slots_rotate_through_all_three_indices() {
        // 200 publications with reads interleaved at an awkward cadence exercise
        // every ownership permutation of the three slots.
        let (mut w, mut r) = TripleBuffer::new(0usize);
        let mut last = 0usize;
        for i in 1..=200 {
            w.write(i);
            if i % 3 == 0 {
                let seen = *r.read();
                assert!(
                    seen >= last,
                    "the reader went backwards: {seen} after {last}"
                );
                assert_eq!(seen, i, "the reader missed the newest value");
                last = seen;
            }
        }
        assert_eq!(*r.read(), 200);
    }

    #[test]
    fn abandonment_is_visible_to_both_halves() {
        let (w, r) = TripleBuffer::new(0u8);
        assert!(!w.is_abandoned());
        assert!(!r.is_abandoned());
        drop(r);
        assert!(w.is_abandoned());
    }

    #[test]
    fn write_and_read_do_not_allocate() {
        #[derive(Clone, Copy, Default)]
        struct Snapshot {
            peak: [f32; 8],
            frames: u64,
        }

        let (mut w, mut r) = TripleBuffer::new(Snapshot::default());
        let ((), allocations) = AllocGuard::scope(|| {
            for i in 0..10_000u64 {
                w.with(|s| {
                    s.frames = i;
                    s.peak[0] = i as f32;
                });
                let _ = r.read();
            }
        });
        assert_eq!(allocations, 0, "triple buffer hot path allocated");
        assert_eq!(r.read().frames, 9_999);
    }

    /// Payload whose two halves must always agree: a torn read is detectable.
    #[derive(Clone, Copy, Default)]
    struct Checked {
        counter: u64,
        doubled: u64,
    }

    #[test]
    fn contended_reader_never_sees_a_torn_or_stale_value() {
        const WRITES: u64 = 100_000;
        let (mut w, mut r) = TripleBuffer::new(Checked::default());
        let done = Arc::new(AtomicBool::new(false));

        std::thread::scope(|scope| {
            let done_writer = Arc::clone(&done);
            scope.spawn(move || {
                for i in 1..=WRITES {
                    w.write(Checked {
                        counter: i,
                        doubled: i * 2,
                    });
                }
                done_writer.store(true, Ordering::Release);
            });

            scope.spawn(move || {
                let mut last = 0u64;
                let mut updates = 0u64;
                loop {
                    let finished = done.load(Ordering::Acquire);
                    let snapshot = *r.read();
                    assert_eq!(
                        snapshot.doubled,
                        snapshot.counter * 2,
                        "triple buffer produced a torn value"
                    );
                    assert!(
                        snapshot.counter >= last,
                        "triple buffer went backwards: {} after {last}",
                        snapshot.counter
                    );
                    if snapshot.counter > last {
                        updates += 1;
                        last = snapshot.counter;
                    }
                    // `finished` was loaded with Acquire *before* the read, so if it
                    // was set the read above is guaranteed to observe the writer's
                    // final publication and this loop terminates.
                    if finished {
                        assert_eq!(last, WRITES, "the final snapshot was not observed");
                        break;
                    }
                }
                assert!(updates > 0);
            });
        });
    }
}
