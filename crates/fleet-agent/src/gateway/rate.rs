//! How often a downstream host may be heard through this Gateway (ADR-0061).
//!
//! Every downstream report, enrolment request and package download takes one token from the bucket
//! of the host that sent it before the Gateway does anything else with it. A bucket holds `burst`
//! tokens and gains `messages_per_sec` of them a second, and a new one starts full, as the Server
//! sizes a host's (ADR-0013 clause 19). The key is the host the downstream certificate speaks as —
//! the one it names, else the one derived from its issuer and serial (ADR-0058 clause 2) — or, for
//! an enrolling peer, its address, an IPv6 one by its /64. The buckets live in memory, in one table
//! bounded at the Gateway's total of bindings, and a full one forgets the bucket used least
//! recently, which can only grant tokens (ADR-0061 clause 5).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fleet_core::rate::{peer_key, Table};
use opamp::proto::{server_error_response, RetryInfo, ServerErrorResponse, ServerToAgent};
use tracing::warn;

use crate::config::GatewayRateLimitConfig;

/// The most buckets held at once: the Gateway's total of bindings (ADR-0061 clause 5).
pub const MAX_BUCKETS: usize = super::cache::MAX_BINDINGS;

/// What a report or an enrolment request past the limit is told, as the Server tells it
/// (ADR-0013 clause 23).
pub const THROTTLED: &str = "too many messages arrive from this sender — retry later";

/// How long a sender told [`THROTTLED`] waits before it asks again: the Server's `RETRY_AFTER`
/// (ADR-0013 clause 23).
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// The `Retry-After` of a download past the limit, in seconds (ADR-0013 clause 24).
pub const RETRY_AFTER_SECS: &str = "30";

/// How long one host's refusals are counted before the next line says how many there were.
const LOG_EVERY_MS: u64 = 60_000;

/// The most hosts whose refusals are counted at once; past it the count starts over.
const LOGGED_HOSTS: usize = 4096;

/// Whose bucket a downstream message or download takes its token from (ADR-0061 clause 1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// The host a member's certificate speaks as.
    Host(String),
    /// An enrolling peer, by its address — an IPv6 one by its /64 — since a bootstrap certificate
    /// may be one for the whole fleet.
    Peer(IpAddr),
}

impl Key {
    /// The bucket of a downstream peer: an enrolling one's address, else the host `speaker` its
    /// certificate speaks as. Every listener supplies the address the socket reports, so `None` —
    /// and with it one bucket shared by every such peer — occurs only where a test drives the
    /// handler without a listener, as on the Server (`agent_rate::Subject::peer`).
    #[must_use]
    pub fn of(enrolling: bool, speaker: &str, peer: Option<SocketAddr>) -> Key {
        if enrolling {
            return Key::Peer(peer_key(
                peer.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |peer| peer.ip()),
            ));
        }
        Key::Host(speaker.to_string())
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Host(host) => f.write_str(host),
            Key::Peer(peer) => write!(f, "{peer}"),
        }
    }
}

/// The refusals of one host since the line that last said so.
struct Refused {
    logged_ms: u64,
    count: u64,
}

struct State {
    buckets: Table<Key>,
    refused: HashMap<Key, Refused>,
}

impl State {
    /// Counts one refusal of `key`, and answers how many to log when its minute is up: once per
    /// host per minute, with how many were refused since the last line, this one included.
    fn refused(&mut self, key: &Key, now_ms: u64) -> Option<u64> {
        if !self.refused.contains_key(key) && self.refused.len() >= LOGGED_HOSTS {
            self.refused.clear();
        }
        match self.refused.get_mut(key) {
            Some(refused) if now_ms.saturating_sub(refused.logged_ms) < LOG_EVERY_MS => {
                refused.count += 1;
                None
            }
            Some(refused) => {
                let count = refused.count + 1;
                *refused = Refused {
                    logged_ms: now_ms,
                    count: 0,
                };
                Some(count)
            }
            None => {
                self.refused.insert(
                    key.clone(),
                    Refused {
                        logged_ms: now_ms,
                        count: 0,
                    },
                );
                Some(1)
            }
        }
    }
}

/// The downstream hosts' buckets.
pub struct HostRate {
    limits: GatewayRateLimitConfig,
    started: Instant,
    /// A time that does not move, where a test holds the clock still.
    frozen: Option<u64>,
    state: Mutex<State>,
}

impl HostRate {
    /// The buckets as a Gateway keeps them, at most [`MAX_BUCKETS`] of them.
    #[must_use]
    pub fn new(limits: GatewayRateLimitConfig) -> Self {
        HostRate::with_capacity(limits, MAX_BUCKETS)
    }

    fn with_capacity(limits: GatewayRateLimitConfig, capacity: usize) -> Self {
        HostRate {
            limits,
            started: Instant::now(),
            frozen: None,
            state: Mutex::new(State {
                buckets: Table::new(capacity),
                refused: HashMap::new(),
            }),
        }
    }

    /// Buckets that gain no token while a test runs, however long it takes.
    #[cfg(test)]
    #[must_use]
    pub fn frozen(limits: GatewayRateLimitConfig) -> Self {
        HostRate {
            frozen: Some(0),
            ..HostRate::new(limits)
        }
    }

    /// Takes one token from `key`'s bucket, and says whether there was one. A refusal is logged
    /// once per host per minute, with the host and how many were refused (ADR-0061 clause 4).
    pub fn admit(&self, key: &Key) -> bool {
        let now = self.frozen.unwrap_or_else(|| {
            u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        self.admit_at(key, now)
    }

    fn admit_at(&self, key: &Key, now_ms: u64) -> bool {
        let mut state = self.state.lock().expect("host rate lock");
        let limits = self.limits;
        if state
            .buckets
            .take(key, limits.messages_per_sec, limits.burst, now_ms)
        {
            return true;
        }
        if let Some(refused) = state.refused(key, now_ms) {
            warn!(
                host = %key, refused,
                "refusing what a downstream host sends past its rate limit; it is answered to \
                 retry later and nothing of it is forwarded"
            );
        }
        false
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state.lock().expect("host rate lock").buckets.len()
    }
}

/// The answer to a report or an enrolment request past the limit, as the Server gives it
/// (ADR-0013 clauses 23 and 25): its own `instance_uid`, so it reaches the Agent that sent it,
/// `Unavailable` and when to retry. It claims none of the Server's capabilities, which the
/// Gateway does not speak for.
#[must_use]
pub fn unavailable(instance_uid: &[u8]) -> ServerToAgent {
    ServerToAgent {
        instance_uid: instance_uid.to_vec(),
        error_response: Some(ServerErrorResponse {
            r#type: opamp::proto::ServerErrorResponseType::Unavailable as i32,
            error_message: THROTTLED.to_string(),
            details: Some(server_error_response::Details::RetryInfo(RetryInfo {
                retry_after_nanoseconds: u64::try_from(RETRY_AFTER.as_nanos()).unwrap_or(u64::MAX),
            })),
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: GatewayRateLimitConfig = GatewayRateLimitConfig {
        messages_per_sec: 2,
        burst: 3,
    };

    fn host(name: &str) -> Key {
        Key::Host(name.to_string())
    }

    fn passing(rate: &HostRate, key: &Key, now_ms: u64) -> usize {
        (0..100).take_while(|_| rate.admit_at(key, now_ms)).count()
    }

    /// A host's bucket starts full at its burst and refills at its rate; another host's bucket is
    /// its own.
    /// Verifies: ADR-0061
    #[test]
    fn a_hosts_bucket_holds_its_burst_and_refills_at_its_rate() {
        let rate = HostRate::with_capacity(LIMITS, 16);
        assert_eq!(passing(&rate, &host("a"), 1000), 3);
        assert_eq!(passing(&rate, &host("b"), 1000), 3, "another host");
        assert!(!rate.admit_at(&host("a"), 1499), "not yet a whole token");
        assert!(rate.admit_at(&host("a"), 1500), "two a second");
        assert_eq!(
            passing(&rate, &host("a"), 600_000),
            3,
            "never past the burst"
        );
    }

    /// A member is keyed by the host its certificate speaks as; an enrolling peer by its address,
    /// an IPv6 one by its /64, whatever host its bootstrap certificate names.
    /// Verifies: ADR-0061
    #[test]
    fn an_enrolling_peer_is_keyed_by_its_address_an_ipv6_one_by_its_64() {
        let at = |ip: &str| Some(SocketAddr::new(ip.parse().expect("ip"), 4000));
        assert_eq!(Key::of(false, "h1", at("192.0.2.1")), host("h1"));
        assert_eq!(
            Key::of(true, "h1", at("2001:db8::1")),
            Key::of(true, "h2", at("2001:db8::ffff:2")),
            "one /64 is one bucket"
        );
        assert_ne!(
            Key::of(true, "h1", at("2001:db8::1")),
            Key::of(true, "h1", at("2001:db8:0:1::1"))
        );
        assert_ne!(
            Key::of(true, "", at("192.0.2.1")),
            Key::of(true, "", at("192.0.2.2"))
        );

        let rate = HostRate::with_capacity(LIMITS, 16);
        let one = Key::of(true, "", at("2001:db8::1"));
        let same_block = Key::of(true, "", at("2001:db8::ffff:2"));
        assert_eq!(passing(&rate, &one, 0), 3);
        assert!(!rate.admit_at(&same_block, 0), "the /64 shares the bucket");
        assert_eq!(
            passing(&rate, &Key::of(true, "", at("192.0.2.7")), 0),
            3,
            "another address"
        );
    }

    /// The table holds at most its capacity, and a new key evicts the bucket used least recently.
    /// Verifies: ADR-0061
    #[test]
    fn a_full_table_evicts_the_bucket_used_least_recently() {
        let rate = HostRate::with_capacity(LIMITS, 2);
        let (a, b, c) = (host("a"), host("b"), host("c"));
        assert_eq!(passing(&rate, &a, 0), 3);
        assert!(rate.admit_at(&b, 0));
        // `a` is used again, so `b` is the one used least recently when `c` arrives.
        assert!(!rate.admit_at(&a, 0));
        assert!(rate.admit_at(&c, 0));
        assert_eq!(rate.len(), 2);
        assert!(
            !rate.admit_at(&a, 0),
            "the bucket used most recently was kept"
        );
        assert_eq!(passing(&rate, &b, 0), 3, "b was forgotten: a new bucket");
        for n in 0..50 {
            assert!(rate.admit_at(&host(&format!("h{n}")), 0));
        }
        assert_eq!(rate.len(), 2);
        assert_eq!(MAX_BUCKETS, 1_000_000);
    }

    /// What a host has refused is logged once a minute, with how many were refused since the last
    /// line.
    /// Verifies: ADR-0061
    #[test]
    fn refusals_are_logged_once_per_host_per_minute_with_their_count() {
        let mut state = State {
            buckets: Table::new(4),
            refused: HashMap::new(),
        };
        assert_eq!(state.refused(&host("a"), 0), Some(1));
        for _ in 0..5 {
            assert_eq!(state.refused(&host("a"), 30_000), None);
        }
        assert_eq!(state.refused(&host("b"), 30_000), Some(1), "another host");
        assert_eq!(state.refused(&host("a"), 60_000), Some(6));
        assert_eq!(state.refused(&host("a"), 60_001), None);
    }

    /// Verifies: ADR-0061
    #[test]
    fn a_refusal_names_its_own_agent_says_unavailable_and_when_to_retry() {
        let reply = unavailable(&[7; 16]);
        assert_eq!(reply.instance_uid, vec![7; 16]);
        let error = reply.error_response.expect("an error response");
        assert_eq!(
            error.r#type,
            opamp::proto::ServerErrorResponseType::Unavailable as i32
        );
        assert!(error.error_message.contains("too many messages"));
        match error.details {
            Some(server_error_response::Details::RetryInfo(info)) => {
                assert_eq!(info.retry_after_nanoseconds, 30_000_000_000);
            }
            other => panic!("no retry_info: {other:?}"),
        }
    }
}
