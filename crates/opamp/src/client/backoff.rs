//! The reconnect backoff both drivers use.

use std::time::Duration;

/// Retry backoff: exponential from one second, capped at a minute, **with jitter** — the drivers'
/// reconnects, and any other retry an application wants spread the same way.
///
/// The Baseline asks for the jitter by name — *"use exponential backoff strategy with jitter to
/// avoid overwhelming the Server"* — and the reason is a fleet, not one Client: after a Server
/// restart every Agent reconnects at once, and a deterministic 1 s, 2 s, 4 s ladder has all of them
/// arrive on the same instants — the reconnect storm this breaks up.
///
/// The shape is *equal jitter*: half the ceiling plus a random share of the other half. Full jitter
/// — a uniform draw over the whole interval — spreads marginally better but lets a retry land at
/// nearly zero, which for a fleet reconnecting together is the very burst being avoided. This keeps
/// a floor and still decorrelates.
pub struct Backoff {
    /// The ceiling of the next delay, before jitter. Doubles per failure, capped.
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    const START: Duration = Duration::from_secs(1);
    const CAP: Duration = Duration::from_secs(60);
    /// The window the first reconnect after a lost connection is drawn over: the default heartbeat
    /// interval.
    pub const AFTER_LOSS: Duration = Duration::from_secs(30);

    #[must_use]
    pub fn new() -> Self {
        Backoff { next: Self::START }
    }

    pub fn reset(&mut self) {
        self.next = Self::START;
    }

    /// The delay before the first reconnect after an established connection was lost, drawn
    /// uniformly over `[0, AFTER_LOSS)`; the failures after it start the exponential steps again
    /// (ADR-draft-tls-1-3-two-planes-and-bounded-input clause 4).
    ///
    /// The draw starts at zero, unlike [`advance`](Self::advance): the floor of equal jitter is
    /// what puts a whole fleet into the same half second once its Server goes away, and a fleet
    /// lost together is what this spreads.
    pub fn after_loss(&mut self) -> Duration {
        self.reset();
        let window = u64::try_from(Self::AFTER_LOSS.as_nanos()).unwrap_or(u64::MAX);
        Duration::from_nanos(crate::random::below(window))
    }

    /// The delay to wait now — somewhere in `[ceiling/2, ceiling)`; subsequent failures wait longer.
    pub fn advance(&mut self) -> Duration {
        let ceiling = self.next;
        self.next = (self.next * 2).min(Self::CAP);
        let half = ceiling / 2;
        half + Duration::from_nanos(crate::random::below(half.as_nanos() as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ceiling still doubles and still caps; each delay lands in the lower-bounded half of its
    /// ceiling. Bounds rather than exact values, because the jitter is the point.
    /// Verifies: ADR-0012
    #[test]
    fn backoff_doubles_and_caps_within_its_jittered_bounds() {
        // `half + rand(0, half)` never reaches `half + half`, so the ceiling is exclusive.
        let within = |delay: Duration, ceiling: Duration| delay >= ceiling / 2 && delay < ceiling;

        let mut backoff = Backoff::new();
        let first = backoff.advance();
        assert!(within(first, Duration::from_secs(1)), "{first:?}");
        let second = backoff.advance();
        assert!(within(second, Duration::from_secs(2)), "{second:?}");
        for _ in 0..10 {
            backoff.advance();
        }
        // Capped: the ceiling stops at a minute, so every later delay is in [30 s, 60 s).
        for _ in 0..3 {
            let capped = backoff.advance();
            assert!(within(capped, Duration::from_secs(60)), "{capped:?}");
        }
        backoff.reset();
        let after_reset = backoff.advance();
        assert!(
            within(after_reset, Duration::from_secs(1)),
            "{after_reset:?}"
        );
    }

    /// The first reconnect after a lost connection is drawn over the whole of `[0, 30 s)`, not its
    /// upper half, and the failures after it start again from the first exponential step.
    /// Verifies: ADR-draft-tls-1-3-two-planes-and-bounded-input
    #[test]
    fn the_first_reconnect_after_a_lost_connection_is_drawn_over_thirty_seconds() {
        let mut backoff = Backoff::new();
        for _ in 0..5 {
            backoff.advance();
        }
        let draws: Vec<Duration> = (0..200).map(|_| backoff.after_loss()).collect();
        assert!(draws.iter().all(|d| *d < Backoff::AFTER_LOSS), "{draws:?}");
        // Two hundred uniform draws: some fall in the first and some in the last tenth, which
        // equal jitter's floor of 15 s would never allow.
        assert!(
            draws.iter().any(|d| *d < Duration::from_secs(3)),
            "{draws:?}"
        );
        assert!(
            draws.iter().any(|d| *d >= Duration::from_secs(27)),
            "{draws:?}"
        );
        let next = backoff.advance();
        assert!(
            next >= Duration::from_millis(500) && next < Duration::from_secs(1),
            "{next:?}"
        );
    }

    /// The whole point of the jitter: two Clients failing at the same instant do not come back on
    /// the same instant. Drawn over enough attempts that an accidental tie is not a flake — with a
    /// nanosecond-resolution draw over half a second, ten matching pairs is not something that
    /// happens.
    /// Verifies: ADR-0012
    #[test]
    fn two_backoffs_do_not_produce_the_same_ladder() {
        let mut one = Backoff::new();
        let mut other = Backoff::new();
        let differs = (0..10).any(|_| one.advance() != other.advance());
        assert!(
            differs,
            "a deterministic ladder is the reconnect storm this jitter exists to break"
        );
    }
}
