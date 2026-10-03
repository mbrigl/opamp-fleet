//! Repeated admission failures from one peer address are throttled (ADR-0039 clause 24).
//!
//! Every `401` counts as a failure of the peer's IP address. A peer with `max_failures` failures
//! within `window_secs` is in back-off for `backoff_secs`, and is answered before its credential
//! is compared. A success clears nothing: behind a shared address a member's success would wipe a
//! guesser's count, so failures only age out of the window. The table is bounded; when it is full
//! the address heard from least recently that is not in back-off is dropped first, so a flood of
//! fresh addresses costs memory once and cannot push a blocked address out.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use crate::fleet::Clock;

/// The addresses one table remembers at most.
pub const DEFAULT_CAPACITY: usize = 10_000;

/// How a throttle counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_failures: u32,
    pub window_secs: u64,
    pub backoff_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_failures: 10,
            window_secs: 60,
            backoff_secs: 300,
        }
    }
}

#[derive(Clone, Copy)]
struct Entry {
    /// When the current window began.
    since_ms: u64,
    failures: u32,
    /// Set while the address is in back-off.
    blocked_until_ms: Option<u64>,
    last_ms: u64,
}

/// One table of peer addresses and their failures.
pub struct Throttle {
    limits: Limits,
    capacity: usize,
    clock: Arc<dyn Clock>,
    entries: Mutex<HashMap<IpAddr, Entry>>,
}

impl Throttle {
    #[must_use]
    pub fn new(limits: Limits, clock: Arc<dyn Clock>) -> Self {
        Throttle::with_capacity(limits, DEFAULT_CAPACITY, clock)
    }

    #[must_use]
    pub fn with_capacity(limits: Limits, capacity: usize, clock: Arc<dyn Clock>) -> Self {
        Throttle {
            limits,
            capacity: capacity.max(1),
            clock,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// The seconds `peer` still has to wait, or `None` when it may try.
    #[must_use]
    pub fn retry_after(&self, peer: IpAddr) -> Option<u64> {
        let now = self.clock.now_ms();
        let entries = self.entries.lock().expect("throttle lock");
        let until = entries.get(&peer)?.blocked_until_ms?;
        (until > now).then(|| (until - now).div_ceil(1000).max(1))
    }

    /// Counts one failure of `peer`, and puts it in back-off once it has failed too often.
    pub fn failed(&self, peer: IpAddr) {
        let now = self.clock.now_ms();
        let window_ms = self.limits.window_secs * 1000;
        let mut entries = self.entries.lock().expect("throttle lock");
        if !entries.contains_key(&peer) && entries.len() >= self.capacity {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| {
                    let blocked = entry.blocked_until_ms.is_some_and(|until| until > now);
                    (blocked, entry.last_ms)
                })
                .map(|(address, _)| *address)
            {
                entries.remove(&oldest);
            }
        }
        let entry = entries.entry(peer).or_insert(Entry {
            since_ms: now,
            failures: 0,
            blocked_until_ms: None,
            last_ms: now,
        });
        if now.saturating_sub(entry.since_ms) > window_ms {
            entry.since_ms = now;
            entry.failures = 0;
        }
        entry.failures = entry.failures.saturating_add(1);
        entry.last_ms = now;
        if entry.failures >= self.limits.max_failures {
            entry.blocked_until_ms = Some(now + self.limits.backoff_secs * 1000);
            entry.failures = 0;
            entry.since_ms = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Manual(AtomicU64);

    impl Clock for Manual {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn throttle(capacity: usize) -> (Throttle, Arc<Manual>) {
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let limits = Limits {
            max_failures: 3,
            window_secs: 60,
            backoff_secs: 300,
        };
        (
            Throttle::with_capacity(limits, capacity, clock.clone()),
            clock,
        )
    }

    const PEER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));

    /// Verifies: ADR-0039
    #[test]
    fn a_peer_that_fails_too_often_waits_out_the_back_off() {
        let (throttle, clock) = throttle(16);
        throttle.failed(PEER);
        throttle.failed(PEER);
        assert_eq!(
            throttle.retry_after(PEER),
            None,
            "two failures are not too many"
        );
        throttle.failed(PEER);
        assert_eq!(throttle.retry_after(PEER), Some(300));
        clock.0.fetch_add(299_500, Ordering::SeqCst);
        assert_eq!(throttle.retry_after(PEER), Some(1));
        clock.0.fetch_add(1_000, Ordering::SeqCst);
        assert_eq!(throttle.retry_after(PEER), None, "the back-off ends");
    }

    #[test]
    fn failures_outside_the_window_do_not_add_up() {
        let (throttle, clock) = throttle(16);
        throttle.failed(PEER);
        throttle.failed(PEER);
        clock.0.fetch_add(61_000, Ordering::SeqCst);
        throttle.failed(PEER);
        assert_eq!(throttle.retry_after(PEER), None);
    }

    /// Verifies: ADR-0039
    #[test]
    fn a_full_table_keeps_an_address_in_back_off() {
        let (throttle, clock) = throttle(2);
        let other = |n| IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, n));
        for _ in 0..3 {
            throttle.failed(PEER);
        }
        for n in 1..=5 {
            clock.0.fetch_add(1, Ordering::SeqCst);
            throttle.failed(other(n));
        }
        assert!(
            throttle.retry_after(PEER).is_some(),
            "fresh addresses pushed a blocked one out"
        );
    }

    /// Verifies: ADR-0039
    #[test]
    fn the_table_is_bounded_and_drops_the_oldest() {
        let (throttle, clock) = throttle(2);
        let other = |n| IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, n));
        for _ in 0..2 {
            throttle.failed(PEER);
        }
        clock.0.fetch_add(1, Ordering::SeqCst);
        throttle.failed(other(1));
        clock.0.fetch_add(1, Ordering::SeqCst);
        throttle.failed(other(2));
        assert_eq!(throttle.entries.lock().expect("lock").len(), 2);
        assert!(
            !throttle.entries.lock().expect("lock").contains_key(&PEER),
            "the oldest address was dropped"
        );
    }
}
