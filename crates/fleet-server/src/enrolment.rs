//! Enrolment: how a host gets its first client certificate (ADR-0039 clauses 19 to 22).
//!
//! A host enrols with a bootstrap certificate from a CA of its own. Such a connection is admitted
//! only while an operator holds an **enrolment window** open, and the certificate signing request
//! it sends waits in a **pending queue** until an operator approves or rejects it. The window and
//! the queue live in memory only, so a Server restart closes the window and forgets every request.
//! When the window closes, every pending request expires.
//!
//! A request is keyed by the fingerprint of its public key: a Client re-sends the same request
//! until it is answered, and a re-sent request joins its own entry rather than queueing again.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::fleet::{CertificateSigner, Clock};

/// The longest an enrolment window may stay open.
pub const MAX_WINDOW_SECS: u64 = 86_400;

/// The most requests the queue holds at once.
pub const MAX_PENDING: usize = 1024;

/// What a request says about itself, read from the CSR by the adapter that parses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The PEM the Agent sent, signed as it is on approval.
    pub csr_pem: String,
    /// The CSR's subject, for the operator to read.
    pub subject: String,
    /// The SHA-256 fingerprint of the requested public key, hex — the key of the queue, and what the
    /// Client logs so an operator can match a request to a host.
    pub key_fingerprint: String,
}

/// Who asked: the bootstrap certificate the connection carried, and from where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requester {
    pub bootstrap_subject: String,
    pub bootstrap_fingerprint: String,
    pub peer: Option<IpAddr>,
}

/// One pending request as the REST API lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub id: String,
    pub arrived_ms: u64,
    pub subject: String,
    pub key_fingerprint: String,
    pub requester: Requester,
}

/// What a submitted request comes to.
#[derive(Debug, PartialEq, Eq)]
pub enum Submitted {
    /// Waiting for an operator; the Client asks again.
    Waiting,
    /// Approved: the issued certificate, PEM.
    Issued(String),
    /// Rejected by an operator.
    Rejected,
    /// The queue is full; the Client is told to come back later.
    Full,
    /// No window is open.
    Closed,
}

/// Why an operator's decision could not be applied.
#[derive(Debug, PartialEq, Eq)]
pub enum DecisionError {
    /// No pending request has that id, or it has expired — a `404`.
    NotFound,
    /// The CA could not sign the request.
    Sign(String),
}

enum Decision {
    Approved(String),
    Rejected,
}

struct Entry {
    pending: Pending,
    csr_pem: String,
    decision: Option<Decision>,
}

#[derive(Default)]
struct State {
    window_until_ms: Option<u64>,
    /// Keyed by the id, which is the key fingerprint.
    entries: BTreeMap<String, Entry>,
}

/// The enrolment window and its queue.
pub struct Enrolment {
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    changes: watch::Sender<u64>,
}

impl Enrolment {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Enrolment {
            clock,
            state: Mutex::new(State::default()),
            changes: watch::channel(0).0,
        }
    }

    /// Opens the window, or moves its end, to `secs` from now; answers when it closes.
    ///
    /// # Errors
    /// Returns an error for a duration outside 1 to [`MAX_WINDOW_SECS`] seconds.
    pub fn open(&self, secs: u64) -> Result<u64, String> {
        if !(1..=MAX_WINDOW_SECS).contains(&secs) {
            return Err(format!(
                "open_for_secs must be between 1 and {MAX_WINDOW_SECS}, not {secs}"
            ));
        }
        let until = self.clock.now_ms() + secs * 1000;
        self.state.lock().expect("enrolment lock").window_until_ms = Some(until);
        self.changed();
        Ok(until)
    }

    /// Closes the window now; every pending request expires.
    pub fn close(&self) {
        let mut state = self.state.lock().expect("enrolment lock");
        state.window_until_ms = None;
        state.entries.clear();
        drop(state);
        self.changed();
    }

    /// When the open window closes, or `None` while it is closed.
    pub fn window(&self) -> Option<u64> {
        let mut state = self.state.lock().expect("enrolment lock");
        self.expire(&mut state)
    }

    /// Whether a bootstrap certificate may be admitted now.
    pub fn is_open(&self) -> bool {
        self.window().is_some()
    }

    /// Takes in a request, or answers it once it has been decided.
    pub fn submit(&self, request: Request, requester: Requester) -> Submitted {
        let mut state = self.state.lock().expect("enrolment lock");
        if self.expire(&mut state).is_none() {
            return Submitted::Closed;
        }
        if let Some(entry) = state.entries.get(&request.key_fingerprint) {
            return match &entry.decision {
                None => Submitted::Waiting,
                Some(Decision::Approved(cert)) => Submitted::Issued(cert.clone()),
                Some(Decision::Rejected) => Submitted::Rejected,
            };
        }
        if state.entries.len() >= MAX_PENDING {
            return Submitted::Full;
        }
        let id = request.key_fingerprint.clone();
        let pending = Pending {
            id: id.clone(),
            arrived_ms: self.clock.now_ms(),
            subject: request.subject,
            key_fingerprint: request.key_fingerprint,
            requester,
        };
        state.entries.insert(
            id,
            Entry {
                pending,
                csr_pem: request.csr_pem,
                decision: None,
            },
        );
        drop(state);
        self.changed();
        Submitted::Waiting
    }

    /// The requests waiting for an operator, oldest first.
    pub fn pending(&self) -> Vec<Pending> {
        let mut state = self.state.lock().expect("enrolment lock");
        self.expire(&mut state);
        let mut pending: Vec<Pending> = state
            .entries
            .values()
            .filter(|entry| entry.decision.is_none())
            .map(|entry| entry.pending.clone())
            .collect();
        pending.sort_by_key(|p| p.arrived_ms);
        pending
    }

    /// Approves one request: the CA signs it, and the certificate waits for the Agent to collect.
    ///
    /// # Errors
    /// Returns [`DecisionError::NotFound`] for an unknown, decided or expired id, and
    /// [`DecisionError::Sign`] when the CA refuses the request.
    pub fn approve(&self, id: &str, signer: &dyn CertificateSigner) -> Result<(), DecisionError> {
        let mut state = self.state.lock().expect("enrolment lock");
        self.expire(&mut state);
        let entry = state
            .entries
            .get_mut(id)
            .filter(|entry| entry.decision.is_none())
            .ok_or(DecisionError::NotFound)?;
        let cert = signer.sign(&entry.csr_pem).map_err(DecisionError::Sign)?;
        entry.decision = Some(Decision::Approved(cert));
        drop(state);
        self.changed();
        Ok(())
    }

    /// Rejects one request.
    ///
    /// # Errors
    /// Returns [`DecisionError::NotFound`] for an unknown, decided or expired id.
    pub fn reject(&self, id: &str) -> Result<(), DecisionError> {
        let mut state = self.state.lock().expect("enrolment lock");
        self.expire(&mut state);
        let entry = state
            .entries
            .get_mut(id)
            .filter(|entry| entry.decision.is_none())
            .ok_or(DecisionError::NotFound)?;
        entry.decision = Some(Decision::Rejected);
        drop(state);
        self.changed();
        Ok(())
    }

    /// Whether the request for `key` was rejected — its connection then ends.
    pub fn is_rejected(&self, key: &str) -> bool {
        let state = self.state.lock().expect("enrolment lock");
        matches!(
            state
                .entries
                .get(key)
                .and_then(|entry| entry.decision.as_ref()),
            Some(Decision::Rejected)
        )
    }

    /// Fires whenever the window or a request changes, for the connections waiting on them.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// The window's end if it is still open; a window that has run out is closed here, and its
    /// requests expire with it.
    fn expire(&self, state: &mut State) -> Option<u64> {
        let until = state.window_until_ms?;
        if self.clock.now_ms() < until {
            return Some(until);
        }
        state.window_until_ms = None;
        state.entries.clear();
        self.changes.send_modify(|rev| *rev += 1);
        None
    }

    fn changed(&self) {
        self.changes.send_modify(|rev| *rev += 1);
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

    struct Echo;

    impl CertificateSigner for Echo {
        fn sign(&self, csr_pem: &str) -> Result<String, String> {
            Ok(format!("issued for {csr_pem}"))
        }
    }

    fn enrolment() -> (Enrolment, Arc<Manual>) {
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        (Enrolment::new(clock.clone()), clock)
    }

    fn request(key: &str) -> Request {
        Request {
            csr_pem: format!("csr-{key}"),
            subject: "CN=edge-01".to_string(),
            key_fingerprint: key.to_string(),
        }
    }

    fn requester() -> Requester {
        Requester {
            bootstrap_subject: "CN=bootstrap".to_string(),
            bootstrap_fingerprint: "bf".to_string(),
            peer: None,
        }
    }

    /// Verifies: ADR-0039
    #[test]
    fn the_window_is_closed_by_default_and_bounded() {
        let (enrolment, clock) = enrolment();
        assert!(!enrolment.is_open());
        assert_eq!(
            enrolment.submit(request("a"), requester()),
            Submitted::Closed
        );
        assert!(enrolment.open(0).is_err());
        assert!(enrolment.open(MAX_WINDOW_SECS + 1).is_err());
        enrolment.open(60).expect("open");
        assert!(enrolment.is_open());
        clock.0.fetch_add(60_000, Ordering::SeqCst);
        assert!(!enrolment.is_open(), "the window closes on its own");
    }

    /// Verifies: ADR-0039
    #[test]
    fn a_request_waits_for_an_operator_and_is_answered_once_decided() {
        let (enrolment, _) = enrolment();
        enrolment.open(600).expect("open");
        assert_eq!(
            enrolment.submit(request("a"), requester()),
            Submitted::Waiting
        );
        assert_eq!(
            enrolment.submit(request("a"), requester()),
            Submitted::Waiting,
            "a re-sent request joins its own entry"
        );
        assert_eq!(enrolment.pending().len(), 1);
        enrolment.approve("a", &Echo).expect("approve");
        assert_eq!(
            enrolment.submit(request("a"), requester()),
            Submitted::Issued("issued for csr-a".to_string())
        );
        assert!(
            enrolment.pending().is_empty(),
            "a decided request is no longer pending"
        );
        assert_eq!(enrolment.approve("a", &Echo), Err(DecisionError::NotFound));

        enrolment.submit(request("b"), requester());
        enrolment.reject("b").expect("reject");
        assert_eq!(
            enrolment.submit(request("b"), requester()),
            Submitted::Rejected
        );
        assert_eq!(enrolment.reject("unknown"), Err(DecisionError::NotFound));
    }

    /// Verifies: ADR-0039
    #[test]
    fn closing_the_window_expires_every_request() {
        let (enrolment, clock) = enrolment();
        enrolment.open(60).expect("open");
        enrolment.submit(request("a"), requester());
        enrolment.close();
        assert!(enrolment.pending().is_empty());
        enrolment.open(60).expect("open");
        enrolment.submit(request("b"), requester());
        clock.0.fetch_add(61_000, Ordering::SeqCst);
        assert_eq!(enrolment.approve("b", &Echo), Err(DecisionError::NotFound));
    }

    /// Verifies: ADR-0039
    #[test]
    fn the_queue_is_bounded() {
        let (enrolment, _) = enrolment();
        enrolment.open(600).expect("open");
        for n in 0..MAX_PENDING {
            assert_eq!(
                enrolment.submit(request(&n.to_string()), requester()),
                Submitted::Waiting
            );
        }
        assert_eq!(
            enrolment.submit(request("one more"), requester()),
            Submitted::Full
        );
    }
}
