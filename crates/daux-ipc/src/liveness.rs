//! Deciding when a peer has stopped answering.
//!
//! A sandbox process can die without closing anything: it can hang, spin, or be stopped by
//! a debugger. The control channel stays open and simply goes quiet, so "the transport
//! closed" is not enough to notice. The host therefore also watches the clock.
//!
//! [`LivenessPolicy`] is deliberately clock-free: it takes an elapsed [`Duration`] and
//! returns a verdict. That makes the policy — the part with the judgement calls in it —
//! testable without sleeping, and leaves the caller to decide whether "now" comes from
//! [`Instant::now`](std::time::Instant::now), from the audio clock, or from a test.

use core::time::Duration;

/// How a peer looks, given how long it has been silent. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PeerHealth {
    /// Heard from recently enough; nothing to do.
    Alive,
    /// Overdue but not yet written off. Worth a heartbeat and a log line, not a restart.
    Late,
    /// Past the deadline. Treat the instance as failed, keep its last known state, and
    /// offer to restart it.
    Dead,
}

impl PeerHealth {
    /// [any-thread] `true` for anything other than [`PeerHealth::Alive`].
    #[inline]
    #[must_use]
    pub const fn is_concerning(self) -> bool {
        !matches!(self, Self::Alive)
    }
}

/// When to send a heartbeat, and when to give up on a peer. [any-thread]
///
/// The two numbers are a trade-off, not a fact: too short a deadline kills a peer that was
/// merely swapped out, too long a one leaves the user's session stalled. The defaults —
/// a heartbeat four times a second and a two-second deadline — are chosen so that a peer
/// must miss roughly eight heartbeats in a row before it is written off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LivenessPolicy {
    /// How often to send a heartbeat while there is nothing else to say.
    heartbeat: Duration,
    /// How long a peer may be silent before it is presumed dead.
    deadline: Duration,
}

impl LivenessPolicy {
    /// Default heartbeat interval: 250 ms.
    pub const DEFAULT_HEARTBEAT: Duration = Duration::from_millis(250);
    /// Default silence deadline: 2 s.
    pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(2);

    /// The default policy, usable in `const` context. [any-thread]
    pub const DEFAULT: Self = Self {
        heartbeat: Self::DEFAULT_HEARTBEAT,
        deadline: Self::DEFAULT_DEADLINE,
    };

    /// [any-thread] The default policy.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self::DEFAULT
    }

    /// [any-thread] Returns the policy with a different heartbeat interval.
    ///
    /// A zero interval is raised to one millisecond — heartbeating on every poll would
    /// flood the channel — and the deadline is pushed out if it would otherwise be shorter
    /// than two heartbeats, because a deadline a peer cannot meet is a restart loop.
    #[must_use]
    pub const fn with_heartbeat(mut self, heartbeat: Duration) -> Self {
        self.heartbeat = if heartbeat.is_zero() {
            Duration::from_millis(1)
        } else {
            heartbeat
        };
        self.enforce_ordering()
    }

    /// [any-thread] Returns the policy with a different silence deadline.
    ///
    /// Clamped to at least two heartbeat intervals, for the reason above.
    #[must_use]
    pub const fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self.enforce_ordering()
    }

    /// [any-thread] How often a heartbeat is sent.
    #[inline]
    #[must_use]
    pub const fn heartbeat(&self) -> Duration {
        self.heartbeat
    }

    /// [any-thread] How long a peer may be silent before it is presumed dead.
    #[inline]
    #[must_use]
    pub const fn deadline(&self) -> Duration {
        self.deadline
    }

    /// [any-thread] What to make of a peer that was last heard from `silent_for` ago.
    ///
    /// One heartbeat of silence is normal; beyond that the peer is [`PeerHealth::Late`],
    /// and at or past the deadline it is [`PeerHealth::Dead`].
    #[must_use]
    pub const fn health(&self, silent_for: Duration) -> PeerHealth {
        if silent_for.as_nanos() >= self.deadline.as_nanos() {
            PeerHealth::Dead
        } else if silent_for.as_nanos() > self.heartbeat.as_nanos() {
            PeerHealth::Late
        } else {
            PeerHealth::Alive
        }
    }

    /// [any-thread] `true` when a heartbeat is due, given how long ago this side last sent
    /// anything.
    ///
    /// Any traffic counts: a heartbeat exists only to fill silence, so a busy connection
    /// never sends one.
    #[inline]
    #[must_use]
    pub const fn should_send_heartbeat(&self, quiet_for: Duration) -> bool {
        quiet_for.as_nanos() >= self.heartbeat.as_nanos()
    }

    /// Keeps the deadline at least two heartbeats away.
    const fn enforce_ordering(mut self) -> Self {
        let floor = self.heartbeat.as_nanos().saturating_mul(2);
        if self.deadline.as_nanos() < floor {
            // `floor` is at most twice a `Duration`, which cannot overflow `u64` seconds in
            // any value this crate accepts; the saturating cast keeps that true anyway.
            let secs = floor / 1_000_000_000;
            let nanos = floor % 1_000_000_000;
            self.deadline = Duration::new(secs as u64, nanos as u32);
        }
        self
    }
}

impl Default for LivenessPolicy {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::{LivenessPolicy, PeerHealth};
    use core::time::Duration;

    #[test]
    fn the_defaults_are_the_documented_numbers() {
        let p = LivenessPolicy::default();
        assert_eq!(p.heartbeat(), Duration::from_millis(250));
        assert_eq!(p.deadline(), Duration::from_secs(2));
        assert_eq!(LivenessPolicy::new(), p);
    }

    #[test]
    fn health_moves_from_alive_through_late_to_dead_at_the_boundaries() {
        let p = LivenessPolicy::new();
        assert_eq!(p.health(Duration::ZERO), PeerHealth::Alive);
        assert_eq!(p.health(Duration::from_millis(250)), PeerHealth::Alive);
        assert_eq!(p.health(Duration::from_millis(251)), PeerHealth::Late);
        assert_eq!(p.health(Duration::from_millis(1_999)), PeerHealth::Late);
        assert_eq!(p.health(Duration::from_secs(2)), PeerHealth::Dead);
        assert_eq!(p.health(Duration::from_secs(600)), PeerHealth::Dead);
        assert!(!PeerHealth::Alive.is_concerning());
        assert!(PeerHealth::Late.is_concerning());
        assert!(PeerHealth::Dead.is_concerning());
    }

    #[test]
    fn a_heartbeat_is_due_only_once_the_line_has_actually_gone_quiet() {
        let p = LivenessPolicy::new();
        assert!(!p.should_send_heartbeat(Duration::ZERO));
        assert!(!p.should_send_heartbeat(Duration::from_millis(249)));
        assert!(p.should_send_heartbeat(Duration::from_millis(250)));
        assert!(p.should_send_heartbeat(Duration::from_secs(5)));
    }

    /// A deadline shorter than the heartbeat would declare every peer dead between
    /// heartbeats — a restart loop, not a health check.
    #[test]
    fn a_deadline_that_no_peer_could_meet_is_pushed_out() {
        let p = LivenessPolicy::new().with_deadline(Duration::from_millis(10));
        assert_eq!(p.heartbeat(), Duration::from_millis(250));
        assert_eq!(p.deadline(), Duration::from_millis(500));
        assert_eq!(p.health(Duration::from_millis(400)), PeerHealth::Late);

        // Raising the heartbeat past the deadline pushes the deadline out too.
        let p = LivenessPolicy::new().with_heartbeat(Duration::from_secs(4));
        assert_eq!(p.deadline(), Duration::from_secs(8));
    }

    #[test]
    fn a_zero_heartbeat_is_raised_so_the_channel_is_not_flooded() {
        let p = LivenessPolicy::new().with_heartbeat(Duration::ZERO);
        assert_eq!(p.heartbeat(), Duration::from_millis(1));
        assert_eq!(p.deadline(), Duration::from_secs(2), "untouched");
        assert!(p.should_send_heartbeat(Duration::from_millis(1)));
    }

    #[test]
    fn a_long_but_sane_configuration_is_left_alone() {
        let p = LivenessPolicy::new()
            .with_heartbeat(Duration::from_millis(500))
            .with_deadline(Duration::from_secs(30));
        assert_eq!(p.heartbeat(), Duration::from_millis(500));
        assert_eq!(p.deadline(), Duration::from_secs(30));
        assert_eq!(p.health(Duration::from_secs(29)), PeerHealth::Late);
        assert_eq!(p.health(Duration::from_secs(30)), PeerHealth::Dead);
    }
}
