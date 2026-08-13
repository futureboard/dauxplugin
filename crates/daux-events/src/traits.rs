//! The audio-thread interfaces a plug-in sees: a read-only input list and a bounded output.

use core::fmt;

use crate::event::DauxEvent;

/// The bounded event output was full.
///
/// This is a **normal** condition, not a bug and not a fatal error: the output queue is
/// preallocated and the audio thread may not grow it. A plug-in that gets this back should
/// drop or defer the event, never allocate, never panic, and never abort the block. It
/// mirrors `DAUX_ERR_OUT_OF_MEMORY` from `DauxEventListV1::push`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EventOverflow;

impl fmt::Display for EventOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("event output is full")
    }
}

impl std::error::Error for EventOverflow {}

/// A read-only, borrowed, time-sorted list of the events for one block.
///
/// The host owns the storage; a plug-in must not retain any borrowed payload past the end of
/// `process`. Implementations must be allocation-free and must never panic: `get` returns
/// `None` for an out-of-range index rather than indexing out of bounds.
///
/// Events arrive sorted by [`DauxEvent::time`], and events with equal timestamps keep the
/// order the host queued them in (abi-v1 §9).
///
/// [audio-thread]
pub trait InputEvents {
    /// [audio-thread] Number of events in the block.
    fn len(&self) -> usize;

    /// [audio-thread] `true` when the block carries no events.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// [audio-thread] The event at `index`, or `None` when `index >= len()`.
    fn get(&self, index: usize) -> Option<DauxEvent<'_>>;

    /// [audio-thread] Iterates the block in order.
    ///
    /// `where Self: Sized` keeps the trait object safe; call
    /// [`InputEventIter::new`] to iterate a `&dyn InputEvents`.
    fn iter(&self) -> InputEventIter<'_>
    where
        Self: Sized,
    {
        InputEventIter::new(self)
    }
}

/// A bounded event output.
///
/// [audio-thread]
pub trait OutputEvents {
    /// [audio-thread] Appends a copy of `e`, borrowed payloads included.
    ///
    /// Never allocates. Returns `Err(EventOverflow)` when the preallocated output is full,
    /// which is a normal condition the caller must handle without allocating or panicking.
    fn try_push(&mut self, e: &DauxEvent<'_>) -> Result<(), EventOverflow>;
}

/// Iterator over an [`InputEvents`] list, produced by [`InputEvents::iter`].
///
/// Allocation-free: it holds a borrow of the list and an index.
#[derive(Clone)]
pub struct InputEventIter<'a> {
    events: &'a dyn InputEvents,
    index: usize,
    len: usize,
}

impl<'a> InputEventIter<'a> {
    /// [audio-thread] Iterates any event list, including a trait object.
    ///
    /// The length is read once, so an implementation that changes size mid-iteration cannot
    /// make this run away.
    pub fn new(events: &'a dyn InputEvents) -> Self {
        Self {
            events,
            index: 0,
            len: events.len(),
        }
    }
}

impl fmt::Debug for InputEventIter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputEventIter")
            .field("index", &self.index)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl<'a> Iterator for InputEventIter<'a> {
    type Item = DauxEvent<'a>;

    fn next(&mut self) -> Option<DauxEvent<'a>> {
        while self.index < self.len {
            let i = self.index;
            self.index += 1;
            if let Some(e) = self.events.get(i) {
                return Some(e);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // The upper bound is exact for a conforming implementation; the lower bound stays 0
        // because `get` is allowed to answer `None` for a slot it cannot decode.
        (0, Some(self.len - self.index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventHeader, NoteEvent};

    /// A minimal `InputEvents` that is not an `EventBuffer`, to prove the trait is usable on
    /// its own and that the default `iter` body works.
    struct Notes(Vec<NoteEvent>);

    impl InputEvents for Notes {
        fn len(&self) -> usize {
            self.0.len()
        }

        fn get(&self, index: usize) -> Option<DauxEvent<'_>> {
            self.0.get(index).map(|n| DauxEvent::NoteOn(*n))
        }
    }

    fn note(time: u32) -> NoteEvent {
        NoteEvent {
            header: EventHeader::at(time),
            ..NoteEvent::default()
        }
    }

    #[test]
    fn overflow_is_a_displayable_error() {
        let e = EventOverflow;
        assert_eq!(e.to_string(), "event output is full");
        let _: &dyn std::error::Error = &e;
        assert_eq!(e, EventOverflow);
    }

    #[test]
    fn default_iter_walks_the_list_in_order() {
        let n = Notes(vec![note(0), note(5), note(9)]);
        assert_eq!(n.len(), 3);
        assert!(!n.is_empty());
        let times: Vec<u32> = n.iter().map(|e| e.time()).collect();
        assert_eq!(times, [0, 5, 9]);
    }

    #[test]
    fn an_empty_list_iterates_zero_times() {
        let n = Notes(Vec::new());
        assert!(n.is_empty());
        assert_eq!(n.iter().count(), 0);
        assert_eq!(n.get(0), None);
    }

    #[test]
    fn a_trait_object_can_be_iterated_explicitly() {
        let n = Notes(vec![note(1), note(2)]);
        let dynamic: &dyn InputEvents = &n;
        assert_eq!(dynamic.len(), 2);
        let times: Vec<u32> = InputEventIter::new(dynamic).map(|e| e.time()).collect();
        assert_eq!(times, [1, 2]);
    }

    #[test]
    fn size_hint_never_over_promises() {
        let n = Notes(vec![note(1), note(2), note(3)]);
        let mut it = n.iter();
        assert_eq!(it.size_hint(), (0, Some(3)));
        it.next();
        assert_eq!(it.size_hint(), (0, Some(2)));
        it.by_ref().for_each(drop);
        assert_eq!(it.size_hint(), (0, Some(0)));
        assert!(it.next().is_none());
    }

    #[test]
    fn out_of_range_reads_return_none_instead_of_panicking() {
        let n = Notes(vec![note(1)]);
        assert!(n.get(1).is_none());
        assert!(n.get(usize::MAX).is_none());
    }
}
