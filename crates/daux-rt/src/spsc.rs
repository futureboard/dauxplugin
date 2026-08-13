//! Bounded wait-free single-producer / single-consumer ring buffer.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::cache::CachePadded;
use crate::error::Full;

/// Shared storage. `head` is written only by the consumer, `tail` only by the
/// producer; both are free-running counters that are masked when they index the
/// slot array, so `tail - head` is the exact number of queued items and the
/// "empty" and "full" states are never ambiguous.
struct Ring<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    mask: usize,
    /// Index of the next slot the producer will fill.
    tail: CachePadded<AtomicUsize>,
    /// Index of the next slot the consumer will drain.
    head: CachePadded<AtomicUsize>,
}

// SAFETY: `Ring` owns its values and hands each one from the producer thread to
// the consumer thread exactly once, which is precisely what `T: Send` allows.
// The head/tail protocol guarantees that the producer and the consumer never
// touch the same slot at the same time: the producer only writes to a slot after
// observing (via an `Acquire` load of `head`) that the consumer has moved the
// previous value out, and the consumer only reads a slot after observing (via an
// `Acquire` load of `tail`) the producer's `Release` publication of it. No `&T`
// is ever handed to two threads at once, so `T: Sync` is not required.
unsafe impl<T: Send> Send for Ring<T> {}
// SAFETY: see the `Send` impl above; sharing `&Ring<T>` between exactly one
// producer and one consumer is the intended and only supported use, and the two
// halves are handed out as separate owned objects that cannot be duplicated.
unsafe impl<T: Send> Sync for Ring<T> {}

impl<T> Ring<T> {
    #[inline]
    fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Raw pointer to the slot addressed by the free-running counter `index`.
    ///
    /// The mask makes the index unconditionally in range, so the bounds check
    /// never fires and this cannot panic.
    #[inline]
    fn slot(&self, index: usize) -> *mut T {
        self.slots[index & self.mask].get().cast::<T>()
    }

    /// Number of queued items, clamped so a racy read can never report nonsense.
    #[inline]
    fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        let queued = tail.wrapping_sub(head);
        // `head` may have advanced past the `tail` we loaded; that underflows and
        // wraps, which we report as "empty" rather than as a huge number.
        if queued > self.capacity() { 0 } else { queued }
    }
}

impl<T> Drop for Ring<T> {
    fn drop(&mut self) {
        let head = *self.head.0.get_mut();
        let tail = *self.tail.0.get_mut();
        let mut index = head;
        while index != tail {
            // SAFETY: both halves have been dropped, so no other thread can touch
            // the ring. Every slot in `head..tail` was initialised by the producer
            // and not yet moved out by the consumer, so it owns a live value that
            // must be dropped exactly once. The index is masked inside `slot`.
            unsafe { core::ptr::drop_in_place(self.slot(index)) };
            index = index.wrapping_add(1);
        }
    }
}

/// Factory for the bounded single-producer / single-consumer ring buffer.
///
/// This is a namespace, not a container: the buffer itself only exists as the
/// [`Producer`]/[`Consumer`] pair returned by [`SpscRingBuffer::with_capacity`],
/// which is what makes the "single producer, single consumer" rule impossible to
/// break by accident.
///
/// ```
/// use daux_rt::SpscRingBuffer;
///
/// let (mut tx, mut rx) = SpscRingBuffer::with_capacity::<u32>(4);
/// tx.push(1).unwrap();
/// tx.push(2).unwrap();
/// assert_eq!(rx.pop(), Some(1));
/// assert_eq!(rx.pop(), Some(2));
/// assert_eq!(rx.pop(), None);
/// ```
///
/// [any-thread]
pub struct SpscRingBuffer;

impl SpscRingBuffer {
    /// Allocates a ring holding at least `capacity` items and splits it into its
    /// two halves.
    ///
    /// The real capacity is `capacity` rounded up to a power of two, and at least
    /// one; [`Producer::capacity`] reports it. Rounding is what lets the hot path
    /// index with a mask instead of a division.
    ///
    /// This is the only allocating operation on the ring. Call it from
    /// `prepare`/`activate`, never from `process`.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is so large that rounding it up to a power of two
    /// overflows `usize`, or if the allocation fails.
    ///
    /// [main-thread]
    #[must_use]
    pub fn with_capacity<T: Send>(capacity: usize) -> (Producer<T>, Consumer<T>) {
        let capacity = capacity
            .max(1)
            .checked_next_power_of_two()
            .expect("daux-rt: SPSC capacity overflows usize when rounded to a power of two");
        let slots: Box<[UnsafeCell<MaybeUninit<T>>]> = (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect();
        let ring = Arc::new(Ring {
            slots,
            mask: capacity - 1,
            tail: CachePadded::new(AtomicUsize::new(0)),
            head: CachePadded::new(AtomicUsize::new(0)),
        });
        (
            Producer {
                ring: Arc::clone(&ring),
                cached_head: 0,
            },
            Consumer {
                ring,
                cached_tail: 0,
            },
        )
    }
}

/// The writing half of an [`SpscRingBuffer`]. Owned by exactly one thread.
///
/// `push` is wait-free: it performs a bounded number of instructions with no
/// loop, no lock and no allocation, so it is safe to call from `process`.
pub struct Producer<T> {
    ring: Arc<Ring<T>>,
    /// Last observed value of `head`. Re-reading the consumer's cache line is the
    /// expensive part of a push, so it is only refreshed when the ring looks full.
    cached_head: usize,
}

impl<T> Producer<T> {
    /// Appends `value`, or returns it in [`Full`] when the ring is full.
    ///
    /// Wait-free and allocation-free. [audio-thread]
    #[inline]
    pub fn push(&mut self, value: T) -> Result<(), Full<T>> {
        let tail = self.ring.tail.0.load(Ordering::Relaxed);
        if tail.wrapping_sub(self.cached_head) == self.ring.capacity() {
            // Acquire: the consumer's `Release` store of `head` happens after it
            // has moved the value out, so seeing the new `head` means the slot we
            // are about to overwrite is genuinely free.
            self.cached_head = self.ring.head.0.load(Ordering::Acquire);
            if tail.wrapping_sub(self.cached_head) == self.ring.capacity() {
                return Err(Full(value));
            }
        }
        // SAFETY: `tail` addresses a slot the consumer has already drained (checked
        // above), so this producer has exclusive access to it and the slot holds no
        // live value to overwrite. The pointer is in range and correctly aligned
        // because it comes from the slot array itself.
        unsafe { self.ring.slot(tail).write(value) };
        // Release: publishes the slot contents to the consumer's `Acquire` load.
        self.ring
            .tail
            .0
            .store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Number of items the ring can hold. [audio-thread]
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.ring.capacity()
    }

    /// Number of items currently queued. Racy by nature: the consumer may drain
    /// items concurrently, so treat this as a lower bound. [audio-thread]
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether the ring is empty right now. Racy; see [`Producer::len`]. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the next [`Producer::push`] would fail. Never a false positive:
    /// only the consumer can free space. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }

    /// Whether the [`Consumer`] has been dropped. Queued items will never be
    /// drained again once this is `true`. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        Arc::strong_count(&self.ring) == 1
    }
}

/// The reading half of an [`SpscRingBuffer`]. Owned by exactly one thread.
///
/// `pop` is wait-free: no loop, no lock, no allocation.
pub struct Consumer<T> {
    ring: Arc<Ring<T>>,
    /// Last observed value of `tail`; refreshed only when the ring looks empty,
    /// so the producer's cache line is left alone on the common path.
    cached_tail: usize,
}

impl<T> Consumer<T> {
    /// Removes and returns the oldest item, or `None` when the ring is empty.
    ///
    /// Wait-free and allocation-free. [audio-thread]
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        let head = self.ring.head.0.load(Ordering::Relaxed);
        if head == self.cached_tail {
            // Acquire: pairs with the producer's `Release` store of `tail` and
            // makes the value it wrote visible to us.
            self.cached_tail = self.ring.tail.0.load(Ordering::Acquire);
            if head == self.cached_tail {
                return None;
            }
        }
        // SAFETY: the producer published this slot before storing the `tail` we
        // observed, so it holds an initialised value that nobody else will touch;
        // moving it out transfers ownership to us exactly once, because `head` is
        // advanced immediately afterwards and only by this thread.
        let value = unsafe { self.ring.slot(head).read() };
        // Release: tells the producer the slot is free to overwrite.
        self.ring
            .head
            .0
            .store(head.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    /// Borrows the oldest item without removing it. [audio-thread]
    #[inline]
    #[must_use]
    pub fn peek(&self) -> Option<&T> {
        let head = self.ring.head.0.load(Ordering::Relaxed);
        let tail = self.ring.tail.0.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: as in `pop`, the slot at `head` is initialised and published.
        // The producer cannot overwrite it because `head` has not advanced, and
        // the borrow of `self` keeps `pop` from running while the reference lives.
        Some(unsafe { &*self.ring.slot(head) })
    }

    /// Number of items the ring can hold. [audio-thread]
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.ring.capacity()
    }

    /// Number of items currently queued. Racy: the producer may append
    /// concurrently, so treat this as a lower bound. [audio-thread]
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether the next [`Consumer::pop`] would return `None`. Never a false
    /// positive: only the producer can add items. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the [`Producer`] has been dropped. Once this is `true` and the
    /// ring is empty, no further items can ever arrive. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        Arc::strong_count(&self.ring) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::SpscRingBuffer;
    use crate::alloc_probe::AllocGuard;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn capacity_is_rounded_up_to_a_power_of_two() {
        let (p, c) = SpscRingBuffer::with_capacity::<u8>(3);
        assert_eq!(p.capacity(), 4);
        assert_eq!(c.capacity(), 4);
        let (p, _c) = SpscRingBuffer::with_capacity::<u8>(64);
        assert_eq!(p.capacity(), 64);
    }

    #[test]
    fn zero_capacity_becomes_one() {
        let (mut p, mut c) = SpscRingBuffer::with_capacity::<u8>(0);
        assert_eq!(p.capacity(), 1);
        assert!(p.push(1).is_ok());
        assert!(p.push(2).is_err());
        assert_eq!(c.pop(), Some(1));
        assert_eq!(c.pop(), None);
    }

    #[test]
    fn capacity_one_alternates() {
        let (mut p, mut c) = SpscRingBuffer::with_capacity::<u32>(1);
        for i in 0..1000 {
            p.push(i).unwrap();
            assert!(p.is_full());
            assert_eq!(c.pop(), Some(i));
            assert!(c.is_empty());
        }
    }

    #[test]
    fn full_hands_the_value_back() {
        let (mut p, _c) = SpscRingBuffer::with_capacity::<String>(1);
        p.push("first".to_owned()).unwrap();
        let rejected = p.push("second".to_owned()).unwrap_err().into_inner();
        assert_eq!(rejected, "second");
    }

    #[test]
    fn empty_ring_pops_none() {
        let (_p, mut c) = SpscRingBuffer::with_capacity::<u8>(8);
        assert_eq!(c.pop(), None);
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        assert_eq!(c.peek(), None);
    }

    #[test]
    fn peek_does_not_consume() {
        let (mut p, mut c) = SpscRingBuffer::with_capacity::<u32>(4);
        p.push(9).unwrap();
        assert_eq!(c.peek(), Some(&9));
        assert_eq!(c.peek(), Some(&9));
        assert_eq!(c.len(), 1);
        assert_eq!(c.pop(), Some(9));
    }

    #[test]
    fn indices_wrap_far_past_the_capacity() {
        let (mut p, mut c) = SpscRingBuffer::with_capacity::<usize>(2);
        for i in 0..10_000 {
            p.push(i).unwrap();
            p.push(i + 1).unwrap();
            assert!(p.push(i + 2).is_err(), "ring should be full at capacity 2");
            assert_eq!(c.pop(), Some(i));
            assert_eq!(c.pop(), Some(i + 1));
        }
        assert!(c.is_empty());
    }

    #[test]
    fn len_tracks_fill_level() {
        let (mut p, mut c) = SpscRingBuffer::with_capacity::<u8>(4);
        assert_eq!(p.len(), 0);
        for i in 0..4u8 {
            p.push(i).unwrap();
            assert_eq!(p.len(), usize::from(i) + 1);
        }
        assert!(p.is_full());
        for i in 0..4u8 {
            assert_eq!(c.pop(), Some(i));
            assert_eq!(c.len(), 3 - usize::from(i));
        }
    }

    #[test]
    fn abandonment_is_visible_to_both_halves() {
        let (p, c) = SpscRingBuffer::with_capacity::<u8>(2);
        assert!(!p.is_abandoned());
        assert!(!c.is_abandoned());
        drop(c);
        assert!(p.is_abandoned());
    }

    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn queued_values_are_dropped_exactly_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let (mut p, mut c) = SpscRingBuffer::with_capacity::<DropCounter>(4);
            for _ in 0..4 {
                p.push(DropCounter(Arc::clone(&drops))).unwrap();
            }
            drop(c.pop());
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            // Re-fill so the live range wraps around the end of the slot array.
            p.push(DropCounter(Arc::clone(&drops))).unwrap();
            drop(p);
            drop(c);
        }
        assert_eq!(drops.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn push_and_pop_do_not_allocate() {
        let (mut p, mut c) = SpscRingBuffer::with_capacity::<[u64; 8]>(16);
        let (popped, allocations) = AllocGuard::scope(|| {
            let mut popped = 0usize;
            for i in 0..10_000u64 {
                if p.push([i; 8]).is_err() {
                    while c.pop().is_some() {
                        popped += 1;
                    }
                }
            }
            while c.pop().is_some() {
                popped += 1;
            }
            popped
        });
        assert_eq!(allocations, 0, "SPSC hot path allocated");
        assert!(popped > 0);
    }

    #[test]
    fn hammer_one_hundred_thousand_items_across_threads() {
        const COUNT: usize = 100_000;
        let (mut p, mut c) = SpscRingBuffer::with_capacity::<usize>(64);

        let producer = std::thread::spawn(move || {
            for i in 0..COUNT {
                let mut value = i;
                loop {
                    match p.push(value) {
                        Ok(()) => break,
                        Err(full) => {
                            value = full.into_inner();
                            std::thread::yield_now();
                        }
                    }
                }
            }
        });

        let consumer = std::thread::spawn(move || {
            let mut next = 0usize;
            while next < COUNT {
                match c.pop() {
                    Some(value) => {
                        assert_eq!(value, next, "SPSC lost, duplicated or reordered an item");
                        next += 1;
                    }
                    None => std::thread::yield_now(),
                }
            }
            assert_eq!(c.pop(), None, "extra items appeared after the last one");
            next
        });

        producer.join().unwrap();
        assert_eq!(consumer.join().unwrap(), COUNT);
    }
}
