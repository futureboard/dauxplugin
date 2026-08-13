//! Lock-free atomic floats built on the integer atomics.
//!
//! `AtomicF32`/`AtomicF64` are bit-casts over `AtomicU32`/`AtomicU64`: the value
//! is stored as its IEEE-754 bit pattern and converted back on read. That makes
//! every operation exactly as cheap and exactly as lock-free as the underlying
//! integer atomic, with no `Mutex` fallback anywhere.
//!
//! Two consequences follow from the bit-cast and are load-bearing for
//! [`AtomicF32::compare_exchange`] and friends: `-0.0` and `0.0` are *different*
//! values even though `-0.0 == 0.0`, and a `NaN` only matches a `NaN` with the
//! same payload even though `NaN != NaN`.

use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

macro_rules! atomic_float {
    (
        $name:ident, $float:ty, $atomic:ty, $doc:literal
    ) => {
        #[doc = $doc]
        ///
        /// The plain [`get`](Self::get)/[`set`](Self::set) pair uses
        /// [`Ordering::Relaxed`], which is what a parameter value wants: the read
        /// must be cheap and no other memory is being published alongside it. Use
        /// [`load`](Self::load)/[`store`](Self::store) with
        /// [`Ordering::Acquire`]/[`Ordering::Release`] when the float guards
        /// other data.
        ///
        /// [any-thread]
        pub struct $name($atomic);

        impl $name {
            /// Creates a new atomic float. Usable in `const` context. [any-thread]
            #[inline]
            #[must_use]
            pub const fn new(value: $float) -> Self {
                Self(<$atomic>::new(value.to_bits()))
            }

            /// Reads the current value with [`Ordering::Relaxed`]. [audio-thread]
            #[inline]
            #[must_use]
            pub fn get(&self) -> $float {
                self.load(Ordering::Relaxed)
            }

            /// Writes `value` with [`Ordering::Relaxed`]. [audio-thread]
            #[inline]
            pub fn set(&self, value: $float) {
                self.store(value, Ordering::Relaxed);
            }

            /// Reads the current value with an explicit ordering. [audio-thread]
            ///
            /// # Panics
            ///
            /// Panics if `order` is [`Ordering::Release`] or
            /// [`Ordering::AcqRel`], matching the integer atomics.
            #[inline]
            #[must_use]
            pub fn load(&self, order: Ordering) -> $float {
                <$float>::from_bits(self.0.load(order))
            }

            /// Writes `value` with an explicit ordering. [audio-thread]
            ///
            /// # Panics
            ///
            /// Panics if `order` is [`Ordering::Acquire`] or
            /// [`Ordering::AcqRel`], matching the integer atomics.
            #[inline]
            pub fn store(&self, value: $float, order: Ordering) {
                self.0.store(value.to_bits(), order);
            }

            /// Writes `value` and returns the previous one. [audio-thread]
            #[inline]
            pub fn swap(&self, value: $float, order: Ordering) -> $float {
                <$float>::from_bits(self.0.swap(value.to_bits(), order))
            }

            /// Stores `new` if the current value has exactly the bit pattern of
            /// `current`, returning the previous value either way.
            ///
            /// The comparison is bitwise, not numeric: `-0.0` never matches
            /// `0.0`, and a `NaN` matches only an identical `NaN`. [audio-thread]
            ///
            /// # Errors
            ///
            /// Returns the current value unchanged if it did not match.
            #[inline]
            pub fn compare_exchange(
                &self,
                current: $float,
                new: $float,
                success: Ordering,
                failure: Ordering,
            ) -> Result<$float, $float> {
                self.0
                    .compare_exchange(current.to_bits(), new.to_bits(), success, failure)
                    .map(<$float>::from_bits)
                    .map_err(<$float>::from_bits)
            }

            /// Applies `f` to the value until it lands, returning the previous
            /// value; `f` returning `None` aborts the update.
            ///
            /// Lock-free but not wait-free: `f` may be called several times when
            /// another thread writes concurrently, so it must be cheap and free
            /// of side effects. [audio-thread]
            ///
            /// # Errors
            ///
            /// Returns the current value if `f` returned `None`.
            #[inline]
            pub fn fetch_update<F>(
                &self,
                set_order: Ordering,
                fetch_order: Ordering,
                mut f: F,
            ) -> Result<$float, $float>
            where
                F: FnMut($float) -> Option<$float>,
            {
                self.0
                    .fetch_update(set_order, fetch_order, |bits| {
                        f(<$float>::from_bits(bits)).map(<$float>::to_bits)
                    })
                    .map(<$float>::from_bits)
                    .map_err(<$float>::from_bits)
            }

            /// Adds `value` and returns the previous value.
            ///
            /// Floating-point addition is not a native atomic operation, so this
            /// is a [`fetch_update`](Self::fetch_update) loop with the same
            /// lock-free (not wait-free) guarantee. [audio-thread]
            #[inline]
            pub fn fetch_add(&self, value: $float, order: Ordering) -> $float {
                let fetch_order = match order {
                    Ordering::Release => Ordering::Relaxed,
                    Ordering::AcqRel => Ordering::Acquire,
                    other => other,
                };
                self.fetch_update(order, fetch_order, |current| Some(current + value))
                    .unwrap_or_else(|current| current)
            }

            /// Replaces the value with `value` if `value` is larger, and returns
            /// the previous value.
            ///
            /// The comparison is numeric, so a stored `NaN` is replaced by any
            /// ordinary value. This is the meter idiom: many `fetch_max` calls on
            /// the audio thread, one [`swap`](Self::swap) to zero on the UI
            /// thread. [audio-thread]
            #[inline]
            pub fn fetch_max(&self, value: $float, order: Ordering) -> $float {
                let fetch_order = match order {
                    Ordering::Release => Ordering::Relaxed,
                    Ordering::AcqRel => Ordering::Acquire,
                    other => other,
                };
                self.fetch_update(order, fetch_order, |current| {
                    if current >= value { None } else { Some(value) }
                })
                .unwrap_or_else(|current| current)
            }

            /// Consumes the atomic and returns the contained value. [any-thread]
            #[inline]
            #[must_use]
            pub fn into_inner(self) -> $float {
                <$float>::from_bits(self.0.into_inner())
            }
        }

        impl Default for $name {
            /// Zero, not `Default::default()` of the float type — same thing, but
            /// stated explicitly because the bit pattern matters here.
            #[inline]
            fn default() -> Self {
                Self::new(0.0)
            }
        }

        impl From<$float> for $name {
            #[inline]
            fn from(value: $float) -> Self {
                Self::new(value)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(&self.get(), f)
            }
        }
    };
}

atomic_float!(
    AtomicF32,
    f32,
    AtomicU32,
    "An `f32` that can be read and written from any thread without a lock."
);
atomic_float!(
    AtomicF64,
    f64,
    AtomicU64,
    "An `f64` that can be read and written from any thread without a lock."
);

#[cfg(test)]
mod tests {
    use super::{AtomicF32, AtomicF64};
    use crate::alloc_probe::AllocGuard;
    use core::sync::atomic::Ordering;
    use std::sync::Arc;

    #[test]
    fn round_trips_ordinary_values() {
        let a = AtomicF32::new(1.5);
        assert_eq!(a.get(), 1.5);
        a.set(-2.25);
        assert_eq!(a.get(), -2.25);
        assert_eq!(a.swap(0.0, Ordering::AcqRel), -2.25);
        assert_eq!(a.into_inner(), 0.0);

        let b = AtomicF64::new(f64::MIN);
        assert_eq!(b.get(), f64::MIN);
        b.store(f64::MAX, Ordering::Release);
        assert_eq!(b.load(Ordering::Acquire), f64::MAX);
    }

    #[test]
    fn preserves_boundary_bit_patterns() {
        for value in [
            0.0f32,
            -0.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            f32::EPSILON,
            f32::MAX,
            f32::MIN,
        ] {
            let a = AtomicF32::new(value);
            assert_eq!(a.get().to_bits(), value.to_bits(), "lost bits for {value}");
        }
        let nan = AtomicF32::new(f32::NAN);
        assert!(nan.get().is_nan());
    }

    #[test]
    fn signed_zeros_are_distinct_bit_patterns() {
        let a = AtomicF32::new(0.0);
        assert!(
            a.compare_exchange(-0.0, 1.0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        );
        assert_eq!(a.get(), 0.0);
        assert!(
            a.compare_exchange(0.0, 1.0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        );
        assert_eq!(a.get(), 1.0);
    }

    #[test]
    fn nan_compares_by_payload() {
        let a = AtomicF64::new(f64::NAN);
        // A NaN never equals itself numerically, but its bit pattern does.
        let stored = a.get();
        assert!(stored.is_nan());
        assert!(
            a.compare_exchange(stored, 1.0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        );
        assert_eq!(a.get(), 1.0);
    }

    #[test]
    fn fetch_update_can_abort() {
        let a = AtomicF32::new(3.0);
        let err = a
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |_| None)
            .unwrap_err();
        assert_eq!(err, 3.0);
        assert_eq!(a.get(), 3.0);

        let previous = a
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| Some(v * 2.0))
            .unwrap();
        assert_eq!(previous, 3.0);
        assert_eq!(a.get(), 6.0);
    }

    #[test]
    fn fetch_add_and_fetch_max_behave() {
        let a = AtomicF64::new(1.0);
        assert_eq!(a.fetch_add(0.5, Ordering::AcqRel), 1.0);
        assert_eq!(a.get(), 1.5);

        let m = AtomicF32::new(0.25);
        assert_eq!(m.fetch_max(0.1, Ordering::AcqRel), 0.25);
        assert_eq!(m.get(), 0.25);
        assert_eq!(m.fetch_max(0.9, Ordering::AcqRel), 0.25);
        assert_eq!(m.get(), 0.9);

        // A stored NaN loses to any ordinary value, so a meter recovers.
        let n = AtomicF32::new(f32::NAN);
        n.fetch_max(0.5, Ordering::AcqRel);
        assert_eq!(n.get(), 0.5);
    }

    #[test]
    fn default_and_conversions() {
        assert_eq!(AtomicF32::default().get(), 0.0);
        assert_eq!(AtomicF64::default().get(), 0.0);
        assert_eq!(AtomicF32::from(2.0).get(), 2.0);
        assert_eq!(format!("{:?}", AtomicF64::new(1.0)), "1.0");
    }

    #[test]
    fn is_usable_in_a_const_static() {
        static GAIN: AtomicF32 = AtomicF32::new(0.5);
        assert_eq!(GAIN.get(), 0.5);
    }

    #[test]
    fn accessors_do_not_allocate() {
        let a = AtomicF32::new(0.0);
        let (sum, allocations) = AllocGuard::scope(|| {
            let mut sum = 0.0f32;
            for i in 0..10_000 {
                a.set(i as f32);
                sum += a.get();
                a.fetch_max(1.0, Ordering::Relaxed);
            }
            sum
        });
        assert_eq!(allocations, 0, "atomic float accessors allocated");
        assert!(sum > 0.0);
    }

    #[test]
    fn concurrent_updates_never_lose_a_write() {
        const THREADS: usize = 4;
        const PER_THREAD: usize = 10_000;
        let value = Arc::new(AtomicF64::new(0.0));

        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let value = Arc::clone(&value);
                scope.spawn(move || {
                    for _ in 0..PER_THREAD {
                        value.fetch_add(1.0, Ordering::AcqRel);
                    }
                });
            }
        });

        // Every increment is exactly representable, so the total is exact.
        assert_eq!(value.get(), (THREADS * PER_THREAD) as f64);
    }
}
