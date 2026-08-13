//! Bounded, allocation-free logging for the audio thread.
//!
//! Formatting allocates, so it cannot happen on the audio thread. The split here
//! is deliberate: the producer copies at most [`RT_LOG_MESSAGE_BYTES`] bytes of
//! an already-formed `&str` into a fixed-size record and moves on; the consumer
//! drains the queue on a normal thread and formats, writes or forwards at
//! leisure.

use core::fmt;

use crate::mpsc::MpscQueue;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Severity of a log record. Mirrors `DAUX_LOG_*` in `abi-v1.md` §11.6, so the
/// discriminants are part of the binary contract and must not be renumbered.
///
/// [any-thread]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u32)]
pub enum LogLevel {
    /// Very fine-grained tracing; off in production hosts.
    Trace = 0,
    /// Diagnostics useful while developing a plug-in.
    Debug = 1,
    /// Normal, expected lifecycle information.
    #[default]
    Info = 2,
    /// Something unexpected that the plug-in recovered from.
    Warn = 3,
    /// An operation failed.
    Error = 4,
    /// The plug-in is in an unusable state.
    Fatal = 5,
}

impl LogLevel {
    /// Every level, ordered by increasing severity. [any-thread]
    pub const ALL: [LogLevel; 6] = [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
        LogLevel::Fatal,
    ];

    /// The ABI value of this level. [any-thread]
    #[inline]
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// The level for an ABI value, or `None` when the host sent something this
    /// version does not know. [any-thread]
    #[inline]
    #[must_use]
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(LogLevel::Trace),
            1 => Some(LogLevel::Debug),
            2 => Some(LogLevel::Info),
            3 => Some(LogLevel::Warn),
            4 => Some(LogLevel::Error),
            5 => Some(LogLevel::Fatal),
            _ => None,
        }
    }

    /// Lower-case name, for consumers that render the record. [any-thread]
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
            LogLevel::Fatal => "fatal",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Maximum number of message bytes a single [`RtLogRecord`] can carry.
///
/// Chosen so the whole record stays small enough to be copied cheaply and to
/// keep a queue of them predictable in size.
pub const RT_LOG_MESSAGE_BYTES: usize = 120;

/// One fixed-size log record. No pointers, no allocation, trivially copyable.
///
/// [any-thread]
#[derive(Clone, Copy)]
pub struct RtLogRecord {
    /// Severity of the record.
    pub level: LogLevel,
    /// Number of valid bytes in [`bytes`](RtLogRecord::bytes).
    pub len: u8,
    /// UTF-8 message bytes; only the first `len` are meaningful.
    pub bytes: [u8; RT_LOG_MESSAGE_BYTES],
}

impl RtLogRecord {
    /// Builds a record from `message`, truncating on a `char` boundary when it
    /// does not fit.
    ///
    /// Copies at most [`RT_LOG_MESSAGE_BYTES`] bytes and never allocates.
    /// [audio-thread]
    #[must_use]
    pub fn new(level: LogLevel, message: &str) -> Self {
        let mut end = message.len().min(RT_LOG_MESSAGE_BYTES);
        // Never split a multi-byte character: back up to the nearest boundary.
        while end > 0 && !message.is_char_boundary(end) {
            end -= 1;
        }
        let mut bytes = [0u8; RT_LOG_MESSAGE_BYTES];
        bytes[..end].copy_from_slice(&message.as_bytes()[..end]);
        Self {
            level,
            // `end <= RT_LOG_MESSAGE_BYTES`, which is 120, so this always fits.
            len: end as u8,
            bytes,
        }
    }

    /// The message.
    ///
    /// Records built by [`RtLogRecord::new`] always hold valid UTF-8. The fields
    /// are public, so a hand-built record might not; this returns an empty string
    /// in that case rather than panicking or allocating a lossy copy.
    /// [any-thread]
    #[must_use]
    pub fn message(&self) -> &str {
        core::str::from_utf8(self.message_bytes()).unwrap_or("")
    }

    /// The raw message bytes, without the UTF-8 check. [any-thread]
    #[inline]
    #[must_use]
    pub fn message_bytes(&self) -> &[u8] {
        let len = (self.len as usize).min(RT_LOG_MESSAGE_BYTES);
        &self.bytes[..len]
    }

    /// Whether `message` would have been truncated by [`RtLogRecord::new`].
    /// [any-thread]
    #[inline]
    #[must_use]
    pub fn would_truncate(message: &str) -> bool {
        message.len() > RT_LOG_MESSAGE_BYTES
    }
}

impl Default for RtLogRecord {
    fn default() -> Self {
        Self {
            level: LogLevel::default(),
            len: 0,
            bytes: [0; RT_LOG_MESSAGE_BYTES],
        }
    }
}

impl fmt::Debug for RtLogRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtLogRecord")
            .field("level", &self.level)
            .field("message", &self.message())
            .finish()
    }
}

impl fmt::Display for RtLogRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.level, self.message())
    }
}

/// A bounded queue of log records that any thread may write to and one thread
/// drains.
///
/// Overflow is counted, never blocked on and never allocated around: when the
/// queue is full the record is dropped and [`dropped`](RtLogQueue::dropped)
/// increments, so the consumer can report "N messages lost" instead of the audio
/// thread stalling.
///
/// ```
/// use daux_rt::{LogLevel, RtLogQueue};
///
/// let queue = RtLogQueue::with_capacity(16);
/// assert!(queue.try_log(LogLevel::Warn, "buffer underrun"));   // [audio-thread]
/// let record = queue.pop().unwrap();                           // [main-thread]
/// assert_eq!(record.message(), "buffer underrun");
/// assert_eq!(record.level, LogLevel::Warn);
/// ```
pub struct RtLogQueue {
    queue: MpscQueue<RtLogRecord>,
    dropped: AtomicUsize,
}

impl RtLogQueue {
    /// Allocates a queue holding at least `records` records.
    ///
    /// The real capacity is `records` rounded up to a power of two. This is the
    /// only allocating operation on the queue.
    ///
    /// # Panics
    ///
    /// Panics if `records` overflows `usize` when rounded up, or if the
    /// allocation fails.
    ///
    /// [main-thread]
    #[must_use]
    pub fn with_capacity(records: usize) -> Self {
        Self {
            queue: MpscQueue::with_capacity(records),
            dropped: AtomicUsize::new(0),
        }
    }

    /// Copies `message` into a record and queues it, returning `false` when the
    /// queue was full.
    ///
    /// The message is truncated to [`RT_LOG_MESSAGE_BYTES`] on a `char`
    /// boundary. No formatting, no allocation, no blocking: build the string
    /// somewhere else or log a constant. [audio-thread]
    #[inline]
    pub fn try_log(&self, level: LogLevel, message: &str) -> bool {
        self.try_push(RtLogRecord::new(level, message))
    }

    /// Queues an already-built record, returning `false` when the queue was
    /// full. [audio-thread]
    pub fn try_push(&self, record: RtLogRecord) -> bool {
        if self.queue.try_push(record).is_ok() {
            true
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    /// Removes and returns the oldest record. [any-thread]
    #[inline]
    #[must_use]
    pub fn pop(&self) -> Option<RtLogRecord> {
        self.queue.pop()
    }

    /// Number of records rejected because the queue was full.
    ///
    /// This counts attempts, not distinct messages: a caller that retries a
    /// rejected message increments it once per rejection. [any-thread]
    #[inline]
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Resets the dropped-record counter and returns its previous value, so a
    /// consumer can report the loss once per drain. [any-thread]
    #[inline]
    pub fn take_dropped(&self) -> usize {
        self.dropped.swap(0, Ordering::Relaxed)
    }

    /// Number of records the queue can hold. [any-thread]
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.queue.capacity()
    }

    /// Number of records waiting to be drained. Racy under concurrent writers.
    /// [any-thread]
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the queue currently looks empty. Racy; see [`RtLogQueue::len`].
    /// [any-thread]
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl fmt::Debug for RtLogQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtLogQueue")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .field("dropped", &self.dropped())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{LogLevel, RT_LOG_MESSAGE_BYTES, RtLogQueue, RtLogRecord};
    use crate::alloc_probe::AllocGuard;
    use std::sync::Arc;

    #[test]
    fn levels_match_the_abi_numbering() {
        assert_eq!(LogLevel::Trace.as_u32(), 0);
        assert_eq!(LogLevel::Debug.as_u32(), 1);
        assert_eq!(LogLevel::Info.as_u32(), 2);
        assert_eq!(LogLevel::Warn.as_u32(), 3);
        assert_eq!(LogLevel::Error.as_u32(), 4);
        assert_eq!(LogLevel::Fatal.as_u32(), 5);
        for level in LogLevel::ALL {
            assert_eq!(LogLevel::from_u32(level.as_u32()), Some(level));
        }
        assert_eq!(LogLevel::from_u32(6), None);
        assert_eq!(LogLevel::from_u32(u32::MAX), None);
        assert_eq!(LogLevel::default(), LogLevel::Info);
        assert!(LogLevel::Error > LogLevel::Warn);
        assert_eq!(LogLevel::Warn.to_string(), "warn");
    }

    #[test]
    fn short_messages_round_trip() {
        let record = RtLogRecord::new(LogLevel::Error, "denormals detected");
        assert_eq!(record.message(), "denormals detected");
        assert_eq!(record.len as usize, "denormals detected".len());
        assert_eq!(record.level, LogLevel::Error);
        assert!(format!("{record}").starts_with("[error] "));
        assert!(format!("{record:?}").contains("denormals"));
    }

    #[test]
    fn empty_messages_are_fine() {
        let record = RtLogRecord::new(LogLevel::Trace, "");
        assert_eq!(record.message(), "");
        assert_eq!(record.len, 0);
        assert_eq!(RtLogRecord::default().message(), "");
    }

    #[test]
    fn long_messages_are_truncated_not_allocated() {
        let long = "x".repeat(500);
        let record = RtLogRecord::new(LogLevel::Info, &long);
        assert_eq!(record.message().len(), RT_LOG_MESSAGE_BYTES);
        assert!(record.message().chars().all(|c| c == 'x'));
        assert!(RtLogRecord::would_truncate(&long));
        assert!(!RtLogRecord::would_truncate("short"));
    }

    #[test]
    fn truncation_lands_on_a_char_boundary() {
        // 'é' is two bytes: a 121-character run crosses the limit mid-character.
        let text = "é".repeat(121);
        let record = RtLogRecord::new(LogLevel::Info, &text);
        assert_eq!(record.message().len(), RT_LOG_MESSAGE_BYTES);
        assert_eq!(record.message().chars().count(), 60);

        // A 4-byte character straddling the limit must be dropped whole.
        let mut text = "a".repeat(RT_LOG_MESSAGE_BYTES - 1);
        text.push('🎛');
        let record = RtLogRecord::new(LogLevel::Info, &text);
        assert_eq!(record.message().len(), RT_LOG_MESSAGE_BYTES - 1);
        assert!(record.message().chars().all(|c| c == 'a'));
    }

    #[test]
    fn a_malformed_hand_built_record_does_not_panic() {
        let record = RtLogRecord {
            level: LogLevel::Info,
            len: 200, // beyond the array, and the bytes are not valid UTF-8
            bytes: [0xff; RT_LOG_MESSAGE_BYTES],
        };
        assert_eq!(record.message_bytes().len(), RT_LOG_MESSAGE_BYTES);
        assert_eq!(record.message(), "");
    }

    #[test]
    fn queue_drains_in_order_and_counts_overflow() {
        let queue = RtLogQueue::with_capacity(2);
        assert_eq!(queue.capacity(), 2);
        assert!(queue.is_empty());
        assert!(queue.try_log(LogLevel::Info, "one"));
        assert!(queue.try_log(LogLevel::Warn, "two"));
        assert!(
            !queue.try_log(LogLevel::Error, "three"),
            "the queue is full"
        );
        assert_eq!(queue.dropped(), 1);
        assert_eq!(queue.len(), 2);

        assert_eq!(queue.pop().unwrap().message(), "one");
        assert_eq!(queue.pop().unwrap().message(), "two");
        assert!(queue.pop().is_none());
        assert_eq!(queue.take_dropped(), 1);
        assert_eq!(queue.dropped(), 0, "taking the count resets it");
        assert!(format!("{queue:?}").contains("dropped"));
    }

    #[test]
    fn logging_does_not_allocate() {
        let queue = RtLogQueue::with_capacity(64);
        let (logged, allocations) = AllocGuard::scope(|| {
            let mut logged = 0usize;
            for _ in 0..1_000 {
                if queue.try_log(LogLevel::Trace, "block processed with no work to do") {
                    logged += 1;
                }
                while queue.pop().is_some() {}
            }
            logged
        });
        assert_eq!(allocations, 0, "RtLogQueue::try_log allocated");
        assert_eq!(logged, 1_000);
    }

    #[test]
    fn many_threads_log_without_losing_records() {
        const THREADS: usize = 4;
        const PER_THREAD: usize = 2_000;
        let queue = Arc::new(RtLogQueue::with_capacity(64));

        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let queue = Arc::clone(&queue);
                scope.spawn(move || {
                    let message = match thread {
                        0 => "zero",
                        1 => "one",
                        2 => "two",
                        _ => "three",
                    };
                    for _ in 0..PER_THREAD {
                        while !queue.try_log(LogLevel::Debug, message) {
                            std::thread::yield_now();
                        }
                    }
                });
            }

            let queue = Arc::clone(&queue);
            scope.spawn(move || {
                let mut received = 0usize;
                while received < THREADS * PER_THREAD {
                    match queue.pop() {
                        Some(record) => {
                            assert!(
                                matches!(record.message(), "zero" | "one" | "two" | "three"),
                                "a record was corrupted: {record:?}"
                            );
                            received += 1;
                        }
                        None => std::thread::yield_now(),
                    }
                }
            });
        });

        assert!(queue.is_empty());
        // `dropped` counts rejected *attempts*, so a message the producer retried
        // shows up there even though it eventually arrived; the exact number is
        // timing-dependent. What the loop above already proved is the invariant
        // that matters: every produced record arrived, intact and exactly once.
        queue.take_dropped();
        assert_eq!(queue.dropped(), 0, "taking the count resets it");
    }
}
