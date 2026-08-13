//! A vector that allocates once and never grows.

use core::fmt;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};

use crate::error::{CapacityError, Full};

/// A bounded vector: exactly one allocation, made by
/// [`with_capacity`](FixedVec::with_capacity), and never a reallocation.
///
/// This is `Vec` with the one operation the audio thread cannot afford removed.
/// Everything that would grow the buffer returns an error instead, so a
/// capacity that turns out to be too small shows up as a handled failure rather
/// than as a dropout.
///
/// ```
/// use daux_rt::FixedVec;
///
/// let mut v = FixedVec::with_capacity(2);
/// v.push(1).unwrap();
/// v.push(2).unwrap();
/// assert_eq!(v.push(3).unwrap_err().into_inner(), 3);   // the value comes back
/// assert_eq!(&v[..], &[1, 2]);
/// ```
///
/// [any-thread]
pub struct FixedVec<T> {
    /// The first `len` slots are initialised; the rest are not.
    buf: Box<[MaybeUninit<T>]>,
    len: usize,
}

impl<T> FixedVec<T> {
    /// Allocates storage for exactly `capacity` items.
    ///
    /// This is the only allocating operation on the type. Call it from
    /// `prepare`/`activate`, never from `process`.
    ///
    /// # Panics
    ///
    /// Panics if the allocation fails.
    ///
    /// [main-thread]
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let buf: Box<[MaybeUninit<T>]> = (0..capacity).map(|_| MaybeUninit::uninit()).collect();
        Self { buf, len: 0 }
    }

    /// Number of items the vector can ever hold. [audio-thread]
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Number of items currently stored. [audio-thread]
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the vector holds no items. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the next [`push`](FixedVec::push) would fail. [audio-thread]
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.len == self.capacity()
    }

    /// Number of items that still fit. [audio-thread]
    #[inline]
    #[must_use]
    pub fn remaining_capacity(&self) -> usize {
        self.capacity() - self.len
    }

    /// Appends `value`, or returns it in [`Full`] when the vector is full.
    ///
    /// Allocation-free and wait-free. [audio-thread]
    ///
    /// # Errors
    ///
    /// Returns the value unchanged when the vector is already at capacity.
    #[inline]
    pub fn push(&mut self, value: T) -> Result<(), Full<T>> {
        if self.len == self.buf.len() {
            return Err(Full(value));
        }
        self.buf[self.len].write(value);
        self.len += 1;
        Ok(())
    }

    /// Removes and returns the last item. [audio-thread]
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: index `self.len` was within the initialised prefix before the
        // decrement, so it holds a live value; lowering `len` first means the slot
        // is no longer considered initialised and the value is moved out once.
        Some(unsafe { self.buf[self.len].assume_init_read() })
    }

    /// Removes the item at `index`, shifting the tail left. [audio-thread]
    ///
    /// # Panics
    ///
    /// Panics if `index >= len`, exactly like `Vec::remove`.
    #[inline]
    pub fn remove(&mut self, index: usize) -> T {
        assert!(
            index < self.len,
            "daux-rt: FixedVec::remove index out of bounds"
        );
        let base = self.buf.as_mut_ptr().cast::<T>();
        // SAFETY: `index < len <= capacity`, so `hole` is inside the allocation and
        // points at a live element. That element is moved out, then the
        // `len - index - 1` live elements after it are shifted down over the hole
        // with `copy` (a memmove, so the overlap is fine), leaving one stale slot
        // at the end which `len -= 1` excludes from the initialised prefix.
        unsafe {
            let hole = base.add(index);
            let removed = hole.read();
            core::ptr::copy(hole.add(1), hole, self.len - index - 1);
            self.len -= 1;
            removed
        }
    }

    /// Removes the item at `index` by swapping the last item into its place.
    /// [audio-thread]
    ///
    /// # Panics
    ///
    /// Panics if `index >= len`, exactly like `Vec::swap_remove`.
    #[inline]
    pub fn swap_remove(&mut self, index: usize) -> T {
        assert!(
            index < self.len,
            "daux-rt: FixedVec::swap_remove index out of bounds"
        );
        let last = self.len - 1;
        self.as_mut_slice().swap(index, last);
        self.pop()
            .expect("len > index >= 0, so the vector is non-empty")
    }

    /// Inserts `value` at `index`, shifting the tail right.
    ///
    /// # Errors
    ///
    /// Returns the value in [`Full`] when the vector is already at capacity.
    ///
    /// # Panics
    ///
    /// Panics if `index > len`, exactly like `Vec::insert`.
    ///
    /// [audio-thread]
    pub fn insert(&mut self, index: usize, value: T) -> Result<(), Full<T>> {
        assert!(
            index <= self.len,
            "daux-rt: FixedVec::insert index out of bounds"
        );
        if self.len == self.buf.len() {
            return Err(Full(value));
        }
        let base = self.buf.as_mut_ptr().cast::<T>();
        // SAFETY: `index <= len < capacity`, so `hole` and the slot one past the
        // last live element are both inside the allocation. The `len - index` live
        // elements from `index` on are shifted one slot up (a memmove, so the
        // overlap is fine) into space that is uninitialised, then `value` is
        // written into the hole the shift opened. `len += 1` below then covers
        // exactly the elements that are now live.
        unsafe {
            let hole = base.add(index);
            core::ptr::copy(hole, hole.add(1), self.len - index);
            hole.write(value);
        }
        self.len += 1;
        Ok(())
    }

    /// Shortens the vector to `len` items, dropping the rest. Longer values of
    /// `len` are a no-op. [audio-thread]
    pub fn truncate(&mut self, len: usize) {
        while self.len > len {
            self.len -= 1;
            // SAFETY: `self.len` now indexes a slot that was inside the
            // initialised prefix and has been excluded from it, so the value it
            // holds is dropped exactly once and never observed again.
            unsafe { self.buf[self.len].assume_init_drop() };
        }
    }

    /// Drops every item, keeping the allocation. [audio-thread]
    #[inline]
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Borrows the initialised prefix. [audio-thread]
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: the first `len` slots are initialised by construction and
        // `MaybeUninit<T>` is layout-compatible with `T`, so the prefix is a valid
        // `[T]` for as long as the borrow of `self` lasts.
        unsafe { core::slice::from_raw_parts(self.buf.as_ptr().cast::<T>(), self.len) }
    }

    /// Mutably borrows the initialised prefix. [audio-thread]
    #[inline]
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: as in `as_slice`; the mutable borrow of `self` guarantees
        // exclusive access to the prefix.
        unsafe { core::slice::from_raw_parts_mut(self.buf.as_mut_ptr().cast::<T>(), self.len) }
    }
}

impl<T: Clone> FixedVec<T> {
    /// Appends a clone of every item in `items`.
    ///
    /// All-or-nothing: when the slice does not fit, the vector is left exactly as
    /// it was and [`CapacityError`] is returned. [audio-thread]
    ///
    /// # Errors
    ///
    /// Returns [`CapacityError`] if `items.len() > remaining_capacity()`.
    pub fn extend_from_slice(&mut self, items: &[T]) -> Result<(), CapacityError> {
        if items.len() > self.remaining_capacity() {
            return Err(CapacityError);
        }
        for item in items {
            self.buf[self.len].write(item.clone());
            // Incrementing inside the loop keeps the vector consistent even if a
            // later `clone` panics: everything written so far is owned and will be
            // dropped, and nothing beyond `len` is ever read.
            self.len += 1;
        }
        Ok(())
    }

    /// Appends `count` clones of `value`.
    ///
    /// All-or-nothing, like [`extend_from_slice`](FixedVec::extend_from_slice).
    /// [audio-thread]
    ///
    /// # Errors
    ///
    /// Returns [`CapacityError`] if `count > remaining_capacity()`.
    pub fn resize_with_clone(&mut self, count: usize, value: &T) -> Result<(), CapacityError> {
        if count > self.remaining_capacity() {
            return Err(CapacityError);
        }
        for _ in 0..count {
            self.buf[self.len].write(value.clone());
            self.len += 1;
        }
        Ok(())
    }
}

impl<T> Drop for FixedVec<T> {
    fn drop(&mut self) {
        self.clear();
    }
}

impl<T> Deref for FixedVec<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> DerefMut for FixedVec<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<'a, T> IntoIterator for &'a FixedVec<T> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<'a, T> IntoIterator for &'a mut FixedVec<T> {
    type Item = &'a mut T;
    type IntoIter = core::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_mut_slice().iter_mut()
    }
}

impl<T: fmt::Debug> fmt::Debug for FixedVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

impl<T: PartialEq> PartialEq for FixedVec<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq> Eq for FixedVec<T> {}

impl<T: Clone> Clone for FixedVec<T> {
    /// Allocates a fresh buffer with the same capacity. [main-thread]
    fn clone(&self) -> Self {
        let mut out = Self::with_capacity(self.capacity());
        out.extend_from_slice(self.as_slice())
            .expect("the clone has the same capacity as the original");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::FixedVec;
    use crate::alloc_probe::AllocGuard;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn zero_capacity_rejects_everything() {
        let mut v = FixedVec::<u8>::with_capacity(0);
        assert!(v.is_empty());
        assert!(v.is_full());
        assert_eq!(v.capacity(), 0);
        assert_eq!(v.push(1).unwrap_err().into_inner(), 1);
        assert_eq!(v.pop(), None);
        assert_eq!(&v[..], &[] as &[u8]);
    }

    #[test]
    fn fills_to_capacity_then_refuses() {
        let mut v = FixedVec::with_capacity(3);
        for i in 0..3u8 {
            v.push(i).unwrap();
            assert_eq!(v.len(), usize::from(i) + 1);
        }
        assert!(v.is_full());
        assert_eq!(v.remaining_capacity(), 0);
        assert_eq!(v.push(9).unwrap_err().into_inner(), 9);
        assert_eq!(v.as_slice(), &[0, 1, 2]);
    }

    #[test]
    fn pop_and_truncate_and_clear() {
        let mut v = FixedVec::with_capacity(4);
        v.extend_from_slice(&[1, 2, 3, 4]).unwrap();
        assert_eq!(v.pop(), Some(4));
        v.truncate(10);
        assert_eq!(v.len(), 3, "truncating to more than len is a no-op");
        v.truncate(1);
        assert_eq!(v.as_slice(), &[1]);
        v.clear();
        assert!(v.is_empty());
        assert_eq!(v.pop(), None);
    }

    #[test]
    fn extend_from_slice_is_all_or_nothing() {
        let mut v = FixedVec::with_capacity(4);
        v.push(0).unwrap();
        assert!(v.extend_from_slice(&[1, 2, 3, 4]).is_err());
        assert_eq!(
            v.as_slice(),
            &[0],
            "a failed extend must not touch the vector"
        );
        v.extend_from_slice(&[1, 2, 3]).unwrap();
        assert_eq!(v.as_slice(), &[0, 1, 2, 3]);
        v.extend_from_slice(&[]).unwrap();
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn insert_remove_and_swap_remove() {
        let mut v = FixedVec::with_capacity(4);
        v.extend_from_slice(&[1, 2, 3]).unwrap();
        v.insert(1, 9).unwrap();
        assert_eq!(v.as_slice(), &[1, 9, 2, 3]);
        assert_eq!(v.insert(0, 0).unwrap_err().into_inner(), 0);
        assert_eq!(v.remove(1), 9);
        assert_eq!(v.as_slice(), &[1, 2, 3]);
        assert_eq!(v.swap_remove(0), 1);
        assert_eq!(v.as_slice(), &[3, 2]);
        assert_eq!(v.remove(1), 2);
        assert_eq!(v.as_slice(), &[3]);
    }

    #[test]
    fn insert_at_the_end_is_a_push() {
        let mut v = FixedVec::with_capacity(2);
        v.insert(0, 1).unwrap();
        v.insert(1, 2).unwrap();
        assert_eq!(v.as_slice(), &[1, 2]);
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn remove_past_the_end_panics() {
        let mut v = FixedVec::<u8>::with_capacity(2);
        v.push(1).unwrap();
        let _ = v.remove(1);
    }

    #[test]
    fn deref_gives_the_whole_slice_api() {
        let mut v = FixedVec::with_capacity(4);
        v.extend_from_slice(&[3, 1, 2]).unwrap();
        v.sort_unstable();
        assert_eq!(v.iter().copied().sum::<i32>(), 6);
        assert_eq!(v.first(), Some(&1));
        assert_eq!(v.last(), Some(&3));
        assert_eq!(v[1], 2);
        v[1] = 5;
        let mut max = i32::MIN;
        for item in &v {
            max = max.max(*item);
        }
        assert_eq!(max, 5);
        for item in &mut v {
            *item *= 2;
        }
        assert_eq!(v.as_slice(), &[2, 10, 6]);
    }

    #[test]
    fn clone_and_eq_and_debug() {
        let mut v = FixedVec::with_capacity(3);
        v.extend_from_slice(&[1, 2]).unwrap();
        let c = v.clone();
        assert_eq!(v, c);
        assert_eq!(c.capacity(), 3);
        assert_eq!(format!("{v:?}"), "[1, 2]");
        v.push(3).unwrap();
        assert_ne!(v, c);
    }

    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn every_item_is_dropped_exactly_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let mut v = FixedVec::with_capacity(4);
            for _ in 0..4 {
                v.push(DropCounter(Arc::clone(&drops))).unwrap();
            }
            drop(v.pop());
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            v.truncate(2);
            assert_eq!(drops.load(Ordering::Relaxed), 2);
            drop(v.remove(0));
            assert_eq!(drops.load(Ordering::Relaxed), 3);
        }
        assert_eq!(drops.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn rejected_push_does_not_drop_the_value() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut v = FixedVec::with_capacity(1);
        v.push(DropCounter(Arc::clone(&drops))).unwrap();
        let returned = v.push(DropCounter(Arc::clone(&drops))).unwrap_err();
        assert_eq!(
            drops.load(Ordering::Relaxed),
            0,
            "the value must survive rejection"
        );
        drop(returned);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn mutation_does_not_allocate() {
        let mut v = FixedVec::<u64>::with_capacity(256);
        let ((), allocations) = AllocGuard::scope(|| {
            for round in 0..100u64 {
                for i in 0..256u64 {
                    v.push(i + round).unwrap();
                }
                assert!(v.push(0).is_err());
                v.truncate(128);
                let _ = v.remove(0);
                let _ = v.swap_remove(0);
                v.insert(0, 1).unwrap();
                v.extend_from_slice(&[1, 2, 3]).unwrap();
                v.clear();
            }
        });
        assert_eq!(allocations, 0, "FixedVec mutation allocated");
    }

    #[test]
    fn holds_zero_sized_types() {
        let mut v = FixedVec::<()>::with_capacity(2);
        v.push(()).unwrap();
        v.push(()).unwrap();
        assert!(v.push(()).is_err());
        assert_eq!(v.len(), 2);
        assert_eq!(v.pop(), Some(()));
    }
}
