//! Bounded lock-free multi-producer queue (Vyukov sequence-numbered slot array).

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::cache::CachePadded;
use crate::error::Full;

/// One slot of the queue.
///
/// `sequence` is the whole synchronisation protocol. A slot is writable when its
/// sequence equals the producer ticket that claimed it, and readable when it
/// equals that ticket plus one. Producers bump it by one on publish, consumers by
/// the capacity on drain, so a slot walks forward one lap at a time and stale
/// tickets are recognised by comparing sequences rather than by an ABA-prone
/// pointer swing.
struct Slot<T> {
    sequence: AtomicUsize,
    value: UnsafeCell<MaybeUninit<T>>,
}

/// A bounded, lock-free queue with many producers and one consumer.
///
/// Both `try_push` and `pop` take `&self`, so the queue is normally wrapped in an
/// `Arc` and shared. Neither ever blocks, locks or allocates. They are *lock-free*
/// rather than wait-free: a caller retries only when another caller has just
/// committed a slot, so the system always makes progress but an individual push
/// may loop a few times under contention.
///
/// The contract designates a single consumer, but the dequeue path is written to
/// be correct with several consumers as well, so a `pop` from an unexpected
/// thread is a design smell rather than undefined behaviour.
///
/// ```
/// use daux_rt::MpscQueue;
///
/// let q = MpscQueue::<u32>::with_capacity(2);
/// q.try_push(1).unwrap();
/// q.try_push(2).unwrap();
/// assert!(q.try_push(3).is_err());
/// assert_eq!(q.pop(), Some(1));
/// assert_eq!(q.pop(), Some(2));
/// assert_eq!(q.pop(), None);
/// ```
pub struct MpscQueue<T> {
    slots: Box<[Slot<T>]>,
    mask: usize,
    /// Ticket dispenser for producers.
    tail: CachePadded<AtomicUsize>,
    /// Ticket dispenser for the consumer.
    head: CachePadded<AtomicUsize>,
}

// SAFETY: the queue owns its values and moves each one from a producer thread to
// the consumer thread exactly once, which is what `T: Send` permits. A slot is
// only written after its sequence proves the previous value has been drained, and
// only read after its sequence proves the value has been published, so no two
// threads ever touch the same slot's payload concurrently. No `&T` escapes, so
// `T: Sync` is not required.
unsafe impl<T: Send> Send for MpscQueue<T> {}
// SAFETY: see the `Send` impl; `&MpscQueue<T>` is the intended sharing mode and
// every mutation goes through the sequence protocol described on `Slot`.
unsafe impl<T: Send> Sync for MpscQueue<T> {}

impl<T: Send> MpscQueue<T> {
    /// Allocates a queue holding at least `capacity` items.
    ///
    /// The real capacity is `capacity` rounded up to a power of two, and at
    /// least two; [`MpscQueue::capacity`] reports it. Two is a hard floor of the
    /// algorithm, not a nicety: with a single slot the sequence a producer
    /// writes on publish is indistinguishable from the one the next producer
    /// expects to find on an empty slot, so "full" could not be detected.
    ///
    /// This is the only allocating operation on the queue.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is so large that rounding it up to a power of two
    /// overflows `usize`, or if the allocation fails.
    ///
    /// [main-thread]
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity
            .max(2)
            .checked_next_power_of_two()
            .expect("daux-rt: MPSC capacity overflows usize when rounded to a power of two");
        let slots: Box<[Slot<T>]> = (0..capacity)
            .map(|i| Slot {
                sequence: AtomicUsize::new(i),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            })
            .collect();
        Self {
            slots,
            mask: capacity - 1,
            tail: CachePadded::new(AtomicUsize::new(0)),
            head: CachePadded::new(AtomicUsize::new(0)),
        }
    }

    /// Appends `value`, or returns it in [`Full`] when the queue is full.
    ///
    /// Callable concurrently from any number of threads. Lock-free and
    /// allocation-free. [audio-thread]
    pub fn try_push(&self, value: T) -> Result<(), Full<T>> {
        let mut ticket = self.tail.0.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[ticket & self.mask];
            // Acquire: pairs with the consumer's `Release` store of the sequence,
            // so seeing "writable" also means the previous value is fully drained.
            let sequence = slot.sequence.load(Ordering::Acquire);
            let lag = sequence.wrapping_sub(ticket) as isize;
            if lag == 0 {
                match self.tail.0.compare_exchange_weak(
                    ticket,
                    ticket.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: winning the CAS gives this thread sole ownership
                        // of the slot until it republishes the sequence. The slot
                        // holds no live value (its sequence proved the previous one
                        // was drained), so writing does not leak, and the pointer is
                        // in range and aligned because it comes from the slot array.
                        unsafe { (*slot.value.get()).write(value) };
                        // Release: publishes the payload to the consumer's Acquire.
                        slot.sequence
                            .store(ticket.wrapping_add(1), Ordering::Release);
                        return Ok(());
                    }
                    Err(observed) => ticket = observed,
                }
            } else if lag < 0 {
                // The slot still holds an undrained value one lap behind: full.
                return Err(Full(value));
            } else {
                // Another producer claimed this ticket; re-read and try again.
                ticket = self.tail.0.load(Ordering::Relaxed);
            }
        }
    }

    /// Removes and returns the oldest item, or `None` when the queue is empty.
    ///
    /// Lock-free and allocation-free. [audio-thread]
    pub fn pop(&self) -> Option<T> {
        let mut ticket = self.head.0.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[ticket & self.mask];
            // Acquire: pairs with the producer's `Release` store of the sequence.
            let sequence = slot.sequence.load(Ordering::Acquire);
            let lag = sequence.wrapping_sub(ticket.wrapping_add(1)) as isize;
            if lag == 0 {
                match self.head.0.compare_exchange_weak(
                    ticket,
                    ticket.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: winning the CAS gives this thread sole ownership of
                        // the slot. Its sequence proved a producer had published an
                        // initialised value there, so moving it out is valid and
                        // happens exactly once; the slot is marked empty immediately
                        // afterwards so nobody reads the moved-from payload.
                        let value = unsafe { (*slot.value.get()).assume_init_read() };
                        // Release: hands the slot back to producers one lap ahead.
                        slot.sequence
                            .store(ticket.wrapping_add(self.mask + 1), Ordering::Release);
                        return Some(value);
                    }
                    Err(observed) => ticket = observed,
                }
            } else if lag < 0 {
                return None;
            } else {
                ticket = self.head.0.load(Ordering::Relaxed);
            }
        }
    }
}

impl<T> MpscQueue<T> {
    /// Number of items the queue can hold. [audio-thread]
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Number of items currently queued.
    ///
    /// Racy by nature and therefore clamped to the capacity: producers and the
    /// consumer may commit while this reads the two counters. [audio-thread]
    #[must_use]
    pub fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        let queued = tail.wrapping_sub(head);
        if queued > self.capacity() { 0 } else { queued }
    }

    /// Whether the queue currently looks empty. Racy; see [`MpscQueue::len`].
    /// [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops every queued value. Only used from `Drop`, where `&mut self` proves
    /// no other thread can be inside the queue.
    fn drain_in_place(&mut self) {
        let head = *self.head.0.get_mut();
        let tail = *self.tail.0.get_mut();
        let mut ticket = head;
        while ticket != tail {
            let slot = &mut self.slots[ticket & self.mask];
            // A producer may have claimed a ticket without publishing yet; that
            // cannot happen here (`&mut self`), but the sequence check keeps the
            // drain honest about which slots actually hold a value.
            if *slot.sequence.get_mut() == ticket.wrapping_add(1) {
                // SAFETY: `&mut self` means no producer or consumer is running, and
                // the sequence proves this slot holds a published, undrained value
                // that we now drop exactly once.
                unsafe { (*slot.value.get()).assume_init_drop() };
            }
            ticket = ticket.wrapping_add(1);
        }
    }
}

impl<T> Drop for MpscQueue<T> {
    fn drop(&mut self) {
        self.drain_in_place();
    }
}

impl<T> core::fmt::Debug for MpscQueue<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MpscQueue")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::MpscQueue;
    use crate::alloc_probe::AllocGuard;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn capacity_is_rounded_up_to_a_power_of_two_of_at_least_two() {
        assert_eq!(MpscQueue::<u8>::with_capacity(5).capacity(), 8);
        assert_eq!(MpscQueue::<u8>::with_capacity(64).capacity(), 64);
        // One slot cannot express "full"; the floor is two.
        assert_eq!(MpscQueue::<u8>::with_capacity(0).capacity(), 2);
        assert_eq!(MpscQueue::<u8>::with_capacity(1).capacity(), 2);
        assert_eq!(MpscQueue::<u8>::with_capacity(2).capacity(), 2);
    }

    #[test]
    fn the_smallest_queue_round_trips() {
        let q = MpscQueue::<u32>::with_capacity(2);
        for i in 0..1000 {
            q.try_push(i).unwrap();
            q.try_push(i + 1).unwrap();
            assert!(q.try_push(i + 2).is_err(), "two slots means two items");
            assert_eq!(q.pop(), Some(i));
            assert_eq!(q.pop(), Some(i + 1));
            assert_eq!(q.pop(), None);
        }
    }

    #[test]
    fn fifo_order_is_preserved_for_one_producer() {
        let q = MpscQueue::<usize>::with_capacity(8);
        for i in 0..8 {
            q.try_push(i).unwrap();
        }
        assert!(q.try_push(99).is_err());
        for i in 0..8 {
            assert_eq!(q.pop(), Some(i));
        }
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn full_hands_the_value_back() {
        let q = MpscQueue::<String>::with_capacity(2);
        q.try_push("kept".to_owned()).unwrap();
        q.try_push("also kept".to_owned()).unwrap();
        let rejected = q.try_push("rejected".to_owned()).unwrap_err().into_inner();
        assert_eq!(rejected, "rejected");
        assert_eq!(q.pop().as_deref(), Some("kept"));
    }

    #[test]
    fn tickets_wrap_far_past_the_capacity() {
        let q = MpscQueue::<usize>::with_capacity(4);
        for i in 0..10_000 {
            q.try_push(i).unwrap();
            assert_eq!(q.pop(), Some(i));
        }
        assert!(q.is_empty());
    }

    #[test]
    fn len_is_clamped_and_tracks_the_fill_level() {
        let q = MpscQueue::<u8>::with_capacity(4);
        assert_eq!(q.len(), 0);
        assert!(q.is_empty());
        q.try_push(1).unwrap();
        q.try_push(2).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.len(), 1);
        assert!(format!("{q:?}").contains("capacity"));
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
            let q = MpscQueue::<DropCounter>::with_capacity(4);
            for _ in 0..4 {
                q.try_push(DropCounter(Arc::clone(&drops))).unwrap();
            }
            drop(q.pop());
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            // Wrap the live range across the end of the slot array.
            q.try_push(DropCounter(Arc::clone(&drops))).unwrap();
        }
        assert_eq!(drops.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn push_and_pop_do_not_allocate() {
        let q = MpscQueue::<[u64; 4]>::with_capacity(16);
        let (drained, allocations) = AllocGuard::scope(|| {
            let mut drained = 0usize;
            for i in 0..10_000u64 {
                if q.try_push([i; 4]).is_err() {
                    while q.pop().is_some() {
                        drained += 1;
                    }
                }
            }
            while q.pop().is_some() {
                drained += 1;
            }
            drained
        });
        assert_eq!(allocations, 0, "MPSC hot path allocated");
        assert!(drained > 0);
    }

    #[test]
    fn four_producers_lose_nothing_and_duplicate_nothing() {
        const PRODUCERS: usize = 4;
        const PER_PRODUCER: usize = 25_000;
        const TOTAL: usize = PRODUCERS * PER_PRODUCER;

        let queue = Arc::new(MpscQueue::<usize>::with_capacity(128));
        let mut seen = vec![false; TOTAL];

        std::thread::scope(|scope| {
            for producer in 0..PRODUCERS {
                let queue = Arc::clone(&queue);
                scope.spawn(move || {
                    for i in 0..PER_PRODUCER {
                        // Encode the producer in the value so duplicates across
                        // producers are detectable, not just within one.
                        let mut value = producer * PER_PRODUCER + i;
                        loop {
                            match queue.try_push(value) {
                                Ok(()) => break,
                                Err(full) => {
                                    value = full.into_inner();
                                    std::thread::yield_now();
                                }
                            }
                        }
                    }
                });
            }

            let mut received = 0usize;
            while received < TOTAL {
                match queue.pop() {
                    Some(value) => {
                        assert!(value < TOTAL, "MPSC produced a value out of range");
                        assert!(!seen[value], "MPSC duplicated item {value}");
                        seen[value] = true;
                        received += 1;
                    }
                    None => std::thread::yield_now(),
                }
            }
        });

        assert!(
            queue.pop().is_none(),
            "extra items appeared after the last one"
        );
        assert!(seen.iter().all(|&s| s), "MPSC lost at least one item");
    }

    #[test]
    fn producers_and_consumer_agree_under_a_tiny_capacity() {
        // The smallest legal queue maximises contention on its two slots.
        let queue = Arc::new(MpscQueue::<u32>::with_capacity(2));
        let total = Arc::new(AtomicUsize::new(0));

        std::thread::scope(|scope| {
            for _ in 0..4 {
                let queue = Arc::clone(&queue);
                scope.spawn(move || {
                    for _ in 0..5_000u32 {
                        while queue.try_push(1).is_err() {
                            std::thread::yield_now();
                        }
                    }
                });
            }
            let queue = Arc::clone(&queue);
            let total = Arc::clone(&total);
            scope.spawn(move || {
                let mut received = 0usize;
                while received < 20_000 {
                    if queue.pop().is_some() {
                        received += 1;
                    } else {
                        std::thread::yield_now();
                    }
                }
                total.store(received, Ordering::Relaxed);
            });
        });

        assert_eq!(total.load(Ordering::Relaxed), 20_000);
        assert!(queue.is_empty());
    }
}
