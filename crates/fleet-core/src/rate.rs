//! The token buckets a sender's messages are counted in, on the Server and on a Gateway alike
//! (ADR-0013 clauses 19 and 26, ADR-0061).
//!
//! A bucket holds `burst` tokens and gains `rate` of them a second, continuously, and a new one
//! starts full. A table of them is bounded, and a full one forgets the bucket used least recently,
//! which can only grant tokens, never withhold them.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::net::IpAddr;

/// One token, in the thousandths a bucket counts in, so that a refill of a few milliseconds is not
/// lost.
pub const TOKEN: u64 = 1000;

/// The address a peer is counted under: an IPv6 address by its /64, the block one host holds, so
/// a peer cannot step past its bucket or its back-off by changing the low bits; an IPv4-mapped one
/// as IPv4.
#[must_use]
pub fn peer_key(peer: IpAddr) -> IpAddr {
    match peer {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let mut segments = v6.segments();
                segments[4..].fill(0);
                IpAddr::V6(std::net::Ipv6Addr::from(segments))
            }
        },
        v4 => v4,
    }
}

/// A bucket's tokens, in thousandths.
#[derive(Clone, Copy)]
struct Level {
    milli: u64,
    at_ms: u64,
    used: u64,
}

/// One bounded table of buckets, forgetting the one used least recently when full.
pub struct Table<K> {
    capacity: usize,
    tick: u64,
    buckets: HashMap<K, Level>,
    /// The order of use: the tick each bucket was last used at.
    order: BTreeMap<u64, K>,
}

impl<K: Clone + Eq + Hash> Table<K> {
    /// A table of at most `capacity` buckets, and at least one.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Table {
            capacity: capacity.max(1),
            tick: 0,
            buckets: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    /// The tokens in `key`'s bucket at `now_ms`, in thousandths, refilled at `rate` a second up to
    /// `burst`; a new bucket starts full. Asking uses the bucket: it is the one used most recently.
    pub fn level(&mut self, key: &K, rate: u32, burst: u32, now_ms: u64) -> u64 {
        let full = u64::from(burst) * TOKEN;
        if !self.buckets.contains_key(key) && self.buckets.len() >= self.capacity {
            if let Some((_, oldest)) = self.order.pop_first() {
                self.buckets.remove(&oldest);
            }
        }
        self.tick += 1;
        let tick = self.tick;
        let level = self.buckets.entry(key.clone()).or_insert(Level {
            milli: full,
            at_ms: now_ms,
            used: tick,
        });
        // Thousandths a millisecond is tokens a second.
        let gained = now_ms
            .saturating_sub(level.at_ms)
            .saturating_mul(u64::from(rate));
        level.milli = level.milli.saturating_add(gained).min(full);
        level.at_ms = level.at_ms.max(now_ms);
        self.order.remove(&level.used);
        level.used = tick;
        self.order.insert(tick, key.clone());
        level.milli
    }

    /// Takes one token from `key`'s bucket, which [`level`](Self::level) found to hold one.
    pub fn spend(&mut self, key: &K) {
        if let Some(level) = self.buckets.get_mut(key) {
            level.milli = level.milli.saturating_sub(TOKEN);
        }
    }

    /// Takes one token from `key`'s bucket when it holds a whole one, and says whether it did.
    pub fn take(&mut self, key: &K, rate: u32, burst: u32, now_ms: u64) -> bool {
        if self.level(key, rate, burst, now_ms) < TOKEN {
            return false;
        }
        self.spend(key);
        true
    }

    /// How many buckets the table holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Whether the table holds no bucket.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies: ADR-0013, ADR-0061
    #[test]
    fn a_bucket_starts_full_refills_continuously_and_holds_no_more_than_its_burst() {
        let mut table = Table::new(4);
        let passing = |table: &mut Table<&str>, now| {
            (0..100).take_while(|_| table.take(&"a", 2, 3, now)).count()
        };
        assert_eq!(passing(&mut table, 1000), 3, "a new bucket holds the burst");
        assert!(!table.take(&"a", 2, 3, 1499), "not yet a whole token");
        assert!(
            table.take(&"a", 2, 3, 1500),
            "two a second is one per 500 ms"
        );
        assert_eq!(passing(&mut table, 61_500), 3, "never more than the burst");
    }

    /// Verifies: ADR-0013, ADR-0061
    #[test]
    fn a_full_table_forgets_the_bucket_used_least_recently() {
        let mut table = Table::new(2);
        assert!(table.take(&"a", 1, 1, 0));
        assert!(table.take(&"b", 1, 1, 0));
        // `a` is used again, so `b` is the one used least recently when `c` arrives.
        assert!(!table.take(&"a", 1, 1, 0));
        assert!(table.take(&"c", 1, 1, 0));
        assert_eq!(table.len(), 2);
        assert!(
            !table.take(&"a", 1, 1, 0),
            "the bucket used most recently was kept"
        );
        assert!(
            table.take(&"b", 1, 1, 0),
            "b was forgotten: a new, full bucket"
        );
    }

    /// Verifies: ADR-0013, ADR-0061
    #[test]
    fn an_ipv6_peer_is_counted_by_its_64_and_a_mapped_one_as_ipv4() {
        let key = |ip: &str| peer_key(ip.parse().expect("ip"));
        assert_eq!(key("2001:db8::1"), key("2001:db8::ffff:2"));
        assert_ne!(key("2001:db8::1"), key("2001:db8:0:1::1"));
        assert_eq!(key("::ffff:192.0.2.7"), key("192.0.2.7"));
        assert_ne!(key("192.0.2.7"), key("192.0.2.8"));
    }
}
