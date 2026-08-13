//! Opaque identifiers that travel between a plug-in and its host.

use core::fmt;

/// Identifies one unit of work a plug-in asked the host to run off the audio
/// thread. `[any-thread]`
///
/// The value is chosen by the plug-in and echoed back verbatim to the
/// controller's `on_worker`, so it is normally a small constant that names the
/// job ("reload impulse response", "rebuild wavetable"). The host treats it as
/// an opaque token and never interprets it.
///
/// The representation is `u64` because that is what `DauxHostWorkerApiV1::schedule`
/// carries across the ABI (`docs/specifications/abi-v1.md` §11.6).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct TaskId(pub u64);

impl TaskId {
    /// Wraps a raw task number. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw task number, as it crosses the ABI. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for TaskId {
    #[inline]
    fn from(raw: u64) -> Self {
        Self(raw)
    }
}

impl From<TaskId> for u64 {
    #[inline]
    fn from(id: TaskId) -> Self {
        id.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task#{}", self.0)
    }
}

/// Identifies a periodic main-thread callback registered with
/// [`HostTimer::register`](crate::HostTimer::register). `[any-thread]`
///
/// Unlike [`TaskId`] the value is chosen by the **host**: a plug-in must store
/// whatever it is given and hand exactly that back to
/// [`HostTimer::unregister`](crate::HostTimer::unregister). Inventing one, or
/// reusing one after unregistering it, is a bug.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct TimerId(pub u64);

impl TimerId {
    /// Wraps a raw timer handle. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw timer handle. `[any-thread]`
    #[inline]
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for TimerId {
    #[inline]
    fn from(raw: u64) -> Self {
        Self(raw)
    }
}

impl From<TimerId> for u64 {
    #[inline]
    fn from(id: TimerId) -> Self {
        id.0
    }
}

impl fmt::Display for TimerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "timer#{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids_round_trip_through_the_raw_wire_value() {
        for raw in [0u64, 1, 42, u64::MAX] {
            let id = TaskId::from(raw);
            assert_eq!(id.get(), raw);
            assert_eq!(u64::from(id), raw);
            assert_eq!(TaskId::new(raw), id);
        }
        assert_eq!(TaskId::default(), TaskId(0));
        assert_eq!(TaskId(7).to_string(), "task#7");
    }

    #[test]
    fn timer_ids_round_trip_through_the_raw_wire_value() {
        for raw in [0u64, 3, u64::MAX] {
            let id = TimerId::from(raw);
            assert_eq!(id.get(), raw);
            assert_eq!(u64::from(id), raw);
            assert_eq!(TimerId::new(raw), id);
        }
        assert_eq!(TimerId::default(), TimerId(0));
        assert_eq!(TimerId(9).to_string(), "timer#9");
    }

    #[test]
    fn ids_order_and_hash_like_their_numbers() {
        let mut ids = [TaskId(9), TaskId(1), TaskId(5)];
        ids.sort_unstable();
        assert_eq!(ids, [TaskId(1), TaskId(5), TaskId(9)]);

        let mut set = std::collections::HashSet::new();
        assert!(set.insert(TimerId(1)));
        assert!(!set.insert(TimerId(1)));
        assert!(set.insert(TimerId(2)));
    }
}
