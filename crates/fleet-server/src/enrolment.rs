//! Enrolment: how a host gets its first client certificate (ADR-0038 clause 17, ADR-0044 clause 8
//! and ADR-0022 clauses 9 and 10).
//!
//! A host enrols with a bootstrap certificate from a CA of its own. Such a connection is admitted
//! only while an operator holds an **enrolment window** open, and the certificate signing request
//! it sends waits in a **pending queue** until an operator approves or rejects it. The window's end
//! and the queue — an approved request with its certificate included — are persisted through the
//! [`EnrolmentStore`] port before the act that changes them is answered (ADR-0044 clause 8), so a
//! restart keeps an open window open until its end and keeps its requests; a window whose end has
//! passed is closed when it is loaded. When the window closes, every pending request expires.
//!
//! A request is keyed by the fingerprint of its public key: a Client re-sends the same request
//! until it is answered, and a re-sent request joins its own entry rather than queueing again.
//!
//! A rejected key outlives the window
//! (ADR-0052
//! clause 16): the store keeps it, a request for it is answered as rejected and not listed, and
//! no approval signs it. An operator may reject a key before its request arrives.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::fleet::{CertificateSigner, Clock};
use crate::revocation::{Approved, CertId, Signed, SubjectValues};

/// The longest an enrolment window may stay open.
pub const MAX_WINDOW_SECS: u64 = 86_400;

/// The most requests the queue holds at once.
pub const MAX_PENDING: usize = 1024;

/// The most requests of one bootstrap certificate the queue holds at once, unless
/// `[enrolment] max_pending_per_bootstrap` says otherwise (ADR-0022): a quarter of the queue.
pub const DEFAULT_MAX_PENDING_PER_BOOTSTRAP: usize = MAX_PENDING / 4;

/// What a request says about itself, read from the CSR by the adapter that parses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The PEM the Agent sent, signed as it is on approval.
    pub csr_pem: String,
    /// The CSR's subject, for the audit record only: the host chose it, so it is neither listed
    /// nor carried (ADR-0022 clause 10).
    pub subject: String,
    /// The SHA-256 fingerprint of the requested public key, hex — the key of the queue, and what the
    /// Client logs so an operator can match a request to a host.
    pub key_fingerprint: String,
    /// The `instance_uid` of the message that carried it, for the register (ADR-0044 clause 9).
    pub instance_uid: Vec<u8>,
}

/// Who asked: the bootstrap certificate the request proves (ADR-0015 clause 22), and from where — the
/// peer address of an enrolment connection, or the Gateway's host it came through (ADR-0015
/// clause 11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requester {
    /// The bootstrap certificate's subject as text, for the audit record.
    pub bootstrap_subject: String,
    /// The values an approval carries into the host certificate, or why the subject breaks the
    /// rules of ADR-0022 clause 1 — read from the certificate verified in the handshake. The queue
    /// lists this as it stands (ADR-0022 clause 10), so an operator sees a refusal before pressing
    /// Approve.
    pub carried: Result<SubjectValues, String>,
    pub bootstrap_fingerprint: String,
    /// The bootstrap certificate's issuer and serial; the serial as
    /// `GET /api/v1/bootstrap-certificates` lists it.
    pub bootstrap: CertId,
    pub peer: Option<IpAddr>,
    /// The host of the Gateway the request came through, in place of a peer address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
}

/// One pending request as the REST API lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub id: String,
    pub arrived_ms: u64,
    pub key_fingerprint: String,
    pub requester: Requester,
}

/// Writes the register entry and the Host an approval prepared, and takes them up, for the
/// enrolment store that writes them with the decision on its request (ADR-0044 clause 9).
pub trait Registrar {
    /// Writes `approved` through the register's own store and takes it up: what a store calls
    /// once it has written the decision, when it cannot write the register in the same write. A
    /// revoked bootstrap certificate is answered [`DecisionError::Revoked`].
    ///
    /// # Errors
    /// Returns the refusal, or why it could not be written; nothing is then taken up.
    fn register(&self, approved: &Approved) -> Result<(), DecisionError>;

    /// Takes up `approved` once the store wrote it in the transaction that decided its request,
    /// and that transaction committed, with the revocation list's version the transaction left,
    /// `written`: `None` where it is not known, as for a commit read back (ADR-0044 clause 10).
    fn committed(&self, approved: &Approved, written: Option<u64>);
}

/// What an approval keeps of what it signed (ADR-0044 clause 9): it records the signature in the
/// audit record and prepares the register entry, before the decision is written, and records a
/// signature whose decision was not written.
pub trait Recorder: Registrar {
    /// Records the signature of `signed`, asked under `instance_uid` for the request `request`,
    /// and prepares its register entry and Host: `None` when the Server keeps no register. A
    /// revoked bootstrap certificate is answered [`DecisionError::Revoked`]. Nothing is recorded
    /// when this fails.
    ///
    /// # Errors
    /// Returns the refusal: a bound, a revoked bootstrap certificate, or an audit record that
    /// cannot take the entry.
    fn signed(
        &self,
        signed: &Signed,
        request: &str,
        instance_uid: &[u8],
        bootstrap: &CertId,
    ) -> Result<Option<Approved>, DecisionError>;

    /// Records that the decision on `request`, which would have handed out `signed`, was not
    /// written.
    fn undecided(&self, signed: &Signed, request: &str, outcome: Undecided);
}

/// What came of a signature whose decision was not written, as the audit record names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Undecided {
    /// The decision was not written, and nothing of it was kept.
    RolledBack,
    /// Whether it was written could not be read back; the register answers.
    Unknown,
}

impl Undecided {
    /// The outcome as the audit record names it.
    #[must_use]
    pub fn outcome(self) -> &'static str {
        match self {
            Undecided::RolledBack => "rolled_back",
            Undecided::Unknown => "unknown",
        }
    }
}

/// What an approval's signature comes to: the decision, and the register entry and Host it
/// prepared, to be written with it.
pub type Signing = (Decision, Option<Approved>);

/// What an approval comes to in the store.
#[derive(Debug, PartialEq, Eq)]
pub enum Approval {
    /// The request waited, was signed and recorded, and is approved with this decision now.
    Approved(Decision),
    /// The store holds the request no longer waiting: another instance decided or withdrew it,
    /// or its window closed. Nothing was signed.
    NotWaiting,
    /// The list names the bootstrap certificate the request came with; the request is withdrawn
    /// and nothing was signed.
    Revoked,
    /// Signing or recording refused it; the request still waits.
    Refused(DecisionError),
    /// The database server refused the transaction that would have decided it, at its commit or
    /// at a statement before it: nothing of it was written, and the request still waits.
    NotCommitted(String),
    /// Another instance holds the request while it decides it, for longer than a wait for a lock
    /// lasts. Nothing was signed here, and the request may still wait.
    Deciding,
}

/// What a rejection comes to in the store.
#[derive(Debug, PartialEq, Eq)]
pub enum Decided {
    /// The request waited and is decided now.
    Written,
    /// The store held no request with that id; its key is rejected before the request arrives.
    Listed,
    /// The store holds the request no longer waiting, or not at all.
    NotWaiting,
    /// Another instance holds the request while it decides it, for longer than a wait for a lock
    /// lasts; nothing was written.
    Deciding,
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
    /// Its bootstrap certificate holds its share of the queue already (ADR-0022); the Client is
    /// told to come back later. `first` is set for the first such refusal of that certificate in
    /// the window, the one that is audited.
    OverShare { first: bool },
    /// No window is open.
    Closed,
}

/// Whether `id` has the form of a request's id, the SHA-256 fingerprint of its key: 64 lowercase
/// hexadecimal digits. A rejection of any other id is refused, since no request can carry it.
#[must_use]
pub fn is_key_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What an operator's rejection came to: whether the queue held the request, or only its key is
/// rejected, before its request arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The request waited in the queue and is rejected.
    Queued,
    /// The queue did not hold the request; its key is rejected when it arrives.
    NotQueued,
}

/// Why an operator's decision could not be applied.
#[derive(Debug, PartialEq, Eq)]
pub enum DecisionError {
    /// No pending request has that id, or it has expired — a `404`.
    NotFound,
    /// The bootstrap certificate the request came with is revoked; the request is withdrawn.
    Revoked,
    /// The bootstrap certificate's subject cannot be carried into a host certificate (ADR-0015
    /// clause 23); the text names the attribute. Nothing is signed.
    Subject(String),
    /// The CA could not sign the request.
    Sign(String),
    /// The decision could not be persisted (ADR-0044 clause 8); the request still waits.
    Store(String),
    /// Another instance is deciding the request at this moment — a `409`. Nothing was decided
    /// here; asked again, the request is found decided, or still waiting.
    Deciding,
}

/// Why the window could not be opened.
#[derive(Debug, PartialEq, Eq)]
pub enum WindowError {
    /// The duration is outside 1 to [`MAX_WINDOW_SECS`] seconds — a `400`.
    Invalid(String),
    /// The window could not be persisted (ADR-0044 clause 8); it is as it was.
    Store(String),
}

/// An operator's decision on a request, as it persists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    /// The issued certificate, PEM, waiting for its host to collect it.
    Approved(String),
    Rejected,
}

/// The window and its requests as they persist (ADR-0044 clause 8): what an [`EnrolmentStore`]
/// keeps, and all it keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Persisted {
    /// When the open window closes; absent while it is closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window_until_ms: Option<u64>,
    #[serde(default)]
    pub(crate) requests: Vec<PersistedRequest>,
    /// The rejected keys, each with when it was rejected: kept beyond the window, by a close and an
    /// end alike.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) rejected: BTreeMap<String, u64>,
}

impl Persisted {
    /// The window closed with its requests: what a close and an end leave, the rejected keys kept.
    fn closed(self) -> Persisted {
        Persisted {
            rejected: self.rejected,
            ..Persisted::default()
        }
    }
}

/// One request as it persists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedRequest {
    pub(crate) id: String,
    arrived_ms: u64,
    key_fingerprint: String,
    requester: Requester,
    csr_pem: String,
    /// The `instance_uid` the request arrived under, hex.
    instance_uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) decision: Option<Decision>,
}

/// What a request comes to in the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Took {
    /// Taken into the queue.
    Added,
    /// Held already, as the store has it: waiting or decided.
    Held(Box<PersistedRequest>),
    /// The queue holds as many requests as it may.
    Full,
    /// The request's bootstrap certificate holds its share of the queue (ADR-0022).
    OverShare,
    /// No window is open.
    Closed,
    /// The request's key is rejected.
    Rejected,
}

/// The enrolment's port (ADR-0009, ADR-0044): where the window and its queue persist. The
/// filesystem adapter ([`FsEnrolmentStore`](crate::fs::FsEnrolmentStore)) keeps them in
/// `enrolment.json`. Each act is one write that decides from what is stored, so the queue's bounds
/// hold for every instance on one database
/// (ADR-0041
/// clause 19). The acts' bodies here read and write the whole, for a store that is its only
/// writer; a store that is shared answers each in one transaction of its own.
pub trait EnrolmentStore: Send + Sync {
    /// What was persisted last, `None` when nothing was.
    ///
    /// # Errors
    /// Returns an error when it exists but cannot be read.
    fn load(&self) -> Result<Option<Persisted>, String>;

    /// Replaces the window and its requests whole: what an import and a test write.
    ///
    /// # Errors
    /// Returns an error when it cannot be written.
    fn save(&self, persisted: &Persisted) -> Result<(), String>;

    /// Takes `request` into the queue while a window is open at `now_ms`, unless its key is
    /// rejected, or the queue holds `max` requests or `max_per_bootstrap` of its bootstrap
    /// certificate, both counted in what is stored. A request held already is answered as it
    /// stands, and counts nothing.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written; nothing is then taken in.
    fn take(
        &self,
        request: &PersistedRequest,
        now_ms: u64,
        max: usize,
        max_per_bootstrap: usize,
    ) -> Result<Took, String> {
        let mut persisted = self.load()?.unwrap_or_default();
        if persisted
            .window_until_ms
            .is_none_or(|until| now_ms >= until)
        {
            return Ok(Took::Closed);
        }
        if persisted.rejected.contains_key(&request.id) {
            return Ok(Took::Rejected);
        }
        if let Some(held) = persisted.requests.iter().find(|r| r.id == request.id) {
            return Ok(Took::Held(Box::new(held.clone())));
        }
        if persisted.requests.len() >= max {
            return Ok(Took::Full);
        }
        let bootstrap = &request.requester.bootstrap;
        let share = persisted
            .requests
            .iter()
            .filter(|r| r.requester.bootstrap == *bootstrap)
            .count();
        if share >= max_per_bootstrap {
            return Ok(Took::OverShare);
        }
        persisted.requests.push(request.clone());
        self.save(&persisted)?;
        Ok(Took::Added)
    }

    /// Decides the waiting request `id`, and answers whether it was decided now: a request the
    /// store does not hold, or holds decided already, is left as it is, and so is one another
    /// instance holds while it decides it.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written; nothing is then decided.
    fn decide(&self, id: &str, decision: &Decision) -> Result<Decided, String> {
        let mut persisted = self.load()?.unwrap_or_default();
        let Some(request) = persisted
            .requests
            .iter_mut()
            .find(|r| r.id == id && r.decision.is_none())
        else {
            return Ok(Decided::NotWaiting);
        };
        request.decision = Some(decision.clone());
        self.save(&persisted)?;
        Ok(Decided::Written)
    }

    /// Rejects the key `id` at `rejected_ms`, kept beyond the window, and decides its request
    /// while it waits: [`Decided::Written`]. A request the store holds decided is left as it is,
    /// its key not rejected: [`Decided::NotWaiting`]. A key the queue does not hold is rejected
    /// before its request arrives: [`Decided::Listed`]. A store other instances share writes it
    /// under the hold [`take`](Self::take) takes, and answers [`Decided::Deciding`] for a request
    /// another instance holds while it decides it.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written; nothing is then rejected.
    fn reject(&self, id: &str, rejected_ms: u64) -> Result<Decided, String> {
        let mut persisted = self.load()?.unwrap_or_default();
        let decided = match persisted.requests.iter_mut().find(|r| r.id == id) {
            Some(request) if request.decision.is_some() => return Ok(Decided::NotWaiting),
            Some(request) => {
                request.decision = Some(Decision::Rejected);
                Decided::Written
            }
            None => Decided::Listed,
        };
        persisted
            .rejected
            .entry(id.to_string())
            .or_insert(rejected_ms);
        self.save(&persisted)?;
        Ok(decided)
    }

    /// Approves the waiting request `id`: `sign` signs it while the request is held, and its
    /// answer is written as the request's decision, with the register entry and the Host it
    /// prepared (ADR-0044 clause 9). A request the store holds no longer waiting, or whose key it
    /// holds rejected, is answered [`Approval::NotWaiting`] before anything is signed, so of two
    /// instances approving one request one signs and records it and the other nothing
    /// (ADR-0041
    /// clause 19). A store other instances share also refuses a request whose bootstrap
    /// certificate, `bootstrap`, the list names, and withdraws it; a store that is its only writer
    /// leaves that to the list in memory. Such a store answers [`Approval::Deciding`] for a
    /// request another instance holds while it decides it, longer than a wait for a lock lasts.
    ///
    /// A store that writes the register in the transaction that decides the request
    /// ([`registers_with_its_decision`](Self::registers_with_its_decision)) does so, and hands what
    /// it committed to `registrar` to take up. This body, for a store that is its only writer,
    /// writes the decision first and then has `registrar` write the register entry and the Host;
    /// one that cannot be written takes the decision back.
    ///
    /// A store that writes the register with its decision answers [`Approval::NotCommitted`] when
    /// the database refused the transaction, so nothing of it was written.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written; nothing is then decided, unless
    /// a store that writes the register with its decision cannot tell whether its commit landed.
    fn approve(
        &self,
        id: &str,
        bootstrap: &CertId,
        sign: &mut dyn FnMut() -> Result<Signing, DecisionError>,
        registrar: &dyn Registrar,
    ) -> Result<Approval, String> {
        let _ = bootstrap;
        let mut persisted = self.load()?.unwrap_or_default();
        if persisted.rejected.contains_key(id) {
            return Ok(Approval::NotWaiting);
        }
        let Some(request) = persisted
            .requests
            .iter_mut()
            .find(|r| r.id == id && r.decision.is_none())
        else {
            return Ok(Approval::NotWaiting);
        };
        let (decision, approved) = match sign() {
            Ok(signing) => signing,
            Err(e) => return Ok(Approval::Refused(e)),
        };
        request.decision = Some(decision.clone());
        if let Err(e) = self.save(&persisted) {
            // What a save that failed left is not known: the request is put back as it was.
            if let Err(again) = self.take_back(id) {
                tracing::warn!(error = %again, "cannot take back an approval that was not written");
            }
            return Err(e);
        }
        if let Some(approved) = &approved {
            if let Err(refused) = registrar.register(approved) {
                // The register entry was not written: the decision is taken back. When that fails
                // too, the next start puts the request back to waiting.
                if let Err(e) = self.take_back(id) {
                    tracing::warn!(error = %e, "cannot take back an approval whose certificate was not registered");
                }
                return Ok(Approval::Refused(refused));
            }
        }
        Ok(Approval::Approved(decision))
    }

    /// Whether [`approve`](Self::approve) writes the register entry and the Host in the
    /// transaction that decides the request, so a decision that does not commit leaves neither:
    /// what PostgreSQL does. A store that is its only writer writes the decision first and has the
    /// register written after it.
    fn registers_with_its_decision(&self) -> bool {
        false
    }

    /// The request `id` as the store holds it now: what an approval whose commit failed reads
    /// back, to tell whether it landed. `None` when the store does not hold it.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    fn read_back(&self, id: &str) -> Result<Option<PersistedRequest>, String> {
        Ok(self
            .load()?
            .and_then(|persisted| persisted.requests.into_iter().find(|r| r.id == id)))
    }

    /// Takes the decision on the request `id` back, so it waits again: what an approval whose
    /// certificate could not be registered does, and what a start does for a decided request
    /// whose certificate the register does not hold (ADR-0044 clause 9). A request the store does
    /// not hold is left.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written.
    fn take_back(&self, id: &str) -> Result<(), String> {
        let Some(mut persisted) = self.load()? else {
            return Ok(());
        };
        let Some(request) = persisted
            .requests
            .iter_mut()
            .find(|r| r.id == id && r.decision.is_some())
        else {
            return Ok(());
        };
        request.decision = None;
        self.save(&persisted)
    }

    /// Withdraws every waiting request whose bootstrap certificate is `serial` under one of
    /// `issuers`, and answers how many: what the revocation of a bootstrap certificate withdraws in
    /// the queue every instance shares (ADR-0038 clause 28).
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written.
    fn withdraw_bootstrap(&self, issuers: &[String], serial: &str) -> Result<usize, String> {
        let Some(mut persisted) = self.load()? else {
            return Ok(0);
        };
        let before = persisted.requests.len();
        persisted.requests.retain(|r| {
            let bootstrap = &r.requester.bootstrap;
            r.decision.is_some()
                || bootstrap.serial != serial
                || !issuers.contains(&bootstrap.issuer)
        });
        let withdrawn = before - persisted.requests.len();
        if withdrawn > 0 {
            self.save(&persisted)?;
        }
        Ok(withdrawn)
    }

    /// Withdraws the requests `ids` names while they wait; a decided one is kept.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written.
    fn withdraw(&self, ids: &[String]) -> Result<(), String> {
        let Some(mut persisted) = self.load()? else {
            return Ok(());
        };
        persisted
            .requests
            .retain(|r| r.decision.is_some() || !ids.contains(&r.id));
        self.save(&persisted)
    }

    /// Opens the window, or moves its end, to `until_ms`; its requests stay.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written.
    fn open_window(&self, until_ms: u64) -> Result<(), String> {
        let mut persisted = self.load()?.unwrap_or_default();
        persisted.window_until_ms = Some(until_ms);
        self.save(&persisted)
    }

    /// Closes the window, and every request expires with it; the rejected keys stay.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written.
    fn close_window(&self) -> Result<(), String> {
        let persisted = self.load()?.unwrap_or_default();
        self.save(&persisted.closed())
    }

    /// Closes the window that ends at `until_ms`, its requests with it, and answers whether it did:
    /// a window stored with another end was moved by another act and stays open. The rejected
    /// keys stay.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or written.
    fn end_window(&self, until_ms: u64) -> Result<bool, String> {
        let persisted = self.load()?.unwrap_or_default();
        if persisted.window_until_ms != Some(until_ms) {
            return Ok(false);
        }
        self.save(&persisted.closed())?;
        Ok(true)
    }
}

#[derive(Clone)]
struct Entry {
    pending: Pending,
    csr_pem: String,
    instance_uid: Vec<u8>,
    decision: Option<Decision>,
}

#[derive(Clone, Default)]
struct State {
    window_until_ms: Option<u64>,
    /// Keyed by the id, which is the key fingerprint.
    entries: BTreeMap<String, Entry>,
    /// The bootstrap certificates refused for their share in this window, each audited once
    /// (ADR-0022). Not persisted: a restart may audit a certificate once more.
    over_share: std::collections::BTreeSet<CertId>,
    /// The rejected keys, as the store keeps them: beyond the window.
    rejected: BTreeMap<String, u64>,
}

impl Entry {
    /// The entry as it persists.
    fn persisted(&self) -> PersistedRequest {
        PersistedRequest {
            id: self.pending.id.clone(),
            arrived_ms: self.pending.arrived_ms,
            key_fingerprint: self.pending.key_fingerprint.clone(),
            requester: self.pending.requester.clone(),
            csr_pem: self.csr_pem.clone(),
            instance_uid: hex::encode(&self.instance_uid),
            decision: self.decision.clone(),
        }
    }

    /// The entry a persisted request restores.
    fn restored(request: PersistedRequest) -> Result<Self, String> {
        let instance_uid = hex::decode(&request.instance_uid)
            .map_err(|e| format!("request {:?}: invalid instance_uid: {e}", request.id))?;
        Ok(Entry {
            pending: Pending {
                id: request.id,
                arrived_ms: request.arrived_ms,
                key_fingerprint: request.key_fingerprint,
                requester: request.requester,
            },
            csr_pem: request.csr_pem,
            instance_uid,
            decision: request.decision,
        })
    }
}

impl State {
    /// What taking `entry` in comes to in memory alone, as [`EnrolmentStore::take`] decides it.
    fn took(&self, entry: &Entry, max: usize, max_per_bootstrap: usize) -> Took {
        if self.entries.len() >= max {
            return Took::Full;
        }
        // A re-sent request has its entry and was answered before; only a new one is counted
        // against its bootstrap certificate's share (ADR-0022).
        let share = self
            .entries
            .values()
            .filter(|held| held.pending.requester.bootstrap == entry.pending.requester.bootstrap)
            .count();
        if share >= max_per_bootstrap {
            return Took::OverShare;
        }
        Took::Added
    }

    fn restored(persisted: Persisted) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        for request in persisted.requests {
            let entry = Entry::restored(request)?;
            entries.insert(entry.pending.id.clone(), entry);
        }
        Ok(State {
            window_until_ms: persisted.window_until_ms,
            entries,
            over_share: std::collections::BTreeSet::new(),
            rejected: persisted.rejected,
        })
    }

    /// The window closed with its requests: the rejected keys stay.
    fn close(&mut self) {
        self.window_until_ms = None;
        self.entries.clear();
        self.over_share.clear();
    }
}

/// What a request held already comes to.
fn answer(decision: Option<&Decision>) -> Submitted {
    match decision {
        None => Submitted::Waiting,
        Some(Decision::Approved(cert)) => Submitted::Issued(cert.clone()),
        Some(Decision::Rejected) => Submitted::Rejected,
    }
}

/// The enrolment window and its queue.
pub struct Enrolment {
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    /// The most requests of one bootstrap certificate the queue holds (ADR-0022).
    max_per_bootstrap: usize,
    /// Where the window and its queue persist; `None` keeps them in memory, as a test does.
    store: Option<Box<dyn EnrolmentStore>>,
    changes: watch::Sender<u64>,
    /// Where a window that runs out records the requests that expired with it (ADR-0039).
    audit: std::sync::OnceLock<Arc<dyn crate::audit::Audit>>,
}

impl Enrolment {
    /// A window and queue held in memory only.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Enrolment {
            clock,
            state: Mutex::new(State::default()),
            max_per_bootstrap: DEFAULT_MAX_PENDING_PER_BOOTSTRAP,
            store: None,
            changes: watch::channel(0).0,
            audit: std::sync::OnceLock::new(),
        }
    }

    /// Restores the window and its queue from `store` (ADR-0044 clause 8), and persists through it
    /// from here on. A window whose end has passed while the Server was down is closed, and its
    /// requests expire with it.
    ///
    /// # Errors
    /// Returns an error when what was persisted cannot be read, or the closed window cannot be
    /// written back.
    pub fn open_on(clock: Arc<dyn Clock>, store: Box<dyn EnrolmentStore>) -> Result<Self, String> {
        let mut state = match store.load()? {
            Some(persisted) => State::restored(persisted)?,
            None => State::default(),
        };
        let ended = state
            .window_until_ms
            .filter(|until| clock.now_ms() >= *until);
        if let Some(until) = ended {
            // Only the window that ended: another instance may have opened it again meanwhile.
            state.close();
            store.end_window(until)?;
        } else if state.window_until_ms.is_none() && !state.entries.is_empty() {
            state.close();
            store.close_window()?;
        }
        Ok(Enrolment {
            clock,
            state: Mutex::new(state),
            max_per_bootstrap: DEFAULT_MAX_PENDING_PER_BOOTSTRAP,
            store: Some(store),
            changes: watch::channel(0).0,
            audit: std::sync::OnceLock::new(),
        })
    }

    /// Holds at most `max` requests of one bootstrap certificate in the queue (ADR-0022), clamped
    /// to 1 to [`MAX_PENDING`]; the configuration refuses anything else at startup.
    #[must_use]
    pub fn with_max_pending_per_bootstrap(mut self, max: usize) -> Self {
        self.max_per_bootstrap = max.clamp(1, MAX_PENDING);
        self
    }

    /// Runs `act` on the store, when one is armed.
    fn on_store(
        &self,
        act: impl FnOnce(&dyn EnrolmentStore) -> Result<(), String>,
    ) -> Result<(), String> {
        match &self.store {
            Some(store) => act(&**store),
            None => Ok(()),
        }
    }

    /// Runs `act` on the store where nothing waits on the answer, logging a failure.
    fn on_store_logged(&self, act: impl FnOnce(&dyn EnrolmentStore) -> Result<(), String>) {
        if let Err(e) = self.on_store(act) {
            tracing::warn!(error = %e, "cannot persist the enrolment window and its requests");
        }
    }

    /// Records a window that runs out in `audit`; set once, at startup.
    pub fn set_audit(&self, audit: Arc<dyn crate::audit::Audit>) {
        let _ = self.audit.set(audit);
    }

    /// Opens the window, or moves its end, to `secs` from now, persisted before it answers when
    /// it closes.
    ///
    /// # Errors
    /// [`WindowError::Invalid`] for a duration outside 1 to [`MAX_WINDOW_SECS`] seconds, and
    /// [`WindowError::Store`] when the window cannot be persisted.
    pub fn open(&self, secs: u64) -> Result<u64, WindowError> {
        if !(1..=MAX_WINDOW_SECS).contains(&secs) {
            return Err(WindowError::Invalid(format!(
                "open_for_secs must be between 1 and {MAX_WINDOW_SECS}, not {secs}"
            )));
        }
        let until = self.clock.now_ms() + secs * 1000;
        let mut state = self.state.lock().expect("enrolment lock");
        self.on_store(|store| store.open_window(until))
            .map_err(WindowError::Store)?;
        state.window_until_ms = Some(until);
        drop(state);
        self.changed();
        Ok(until)
    }

    /// Closes the window now; every pending request expires, and the rejected keys stay. It is
    /// closed in memory whatever happens to the write, the safe direction.
    ///
    /// # Errors
    /// Returns an error when the closed window cannot be persisted: a restart may then find it
    /// open until its end.
    pub fn close(&self) -> Result<(), String> {
        let mut state = self.state.lock().expect("enrolment lock");
        state.close();
        let saved = self.on_store(|store| store.close_window());
        drop(state);
        self.changed();
        saved
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

    /// Takes in a request, or answers it once it has been decided; a request whose key is rejected
    /// is answered so, beyond the window it was rejected in. With a store armed the queue's
    /// bounds are counted in what is stored, so they hold for every instance on one database; a
    /// request another instance took in is answered as the store holds it.
    pub fn submit(&self, request: Request, requester: Requester) -> Submitted {
        let mut state = self.state.lock().expect("enrolment lock");
        if self.expire(&mut state).is_none() {
            return Submitted::Closed;
        }
        if state.rejected.contains_key(&request.key_fingerprint) {
            return Submitted::Rejected;
        }
        if let Some(entry) = state.entries.get(&request.key_fingerprint) {
            return answer(entry.decision.as_ref());
        }
        let id = request.key_fingerprint.clone();
        let bootstrap = requester.bootstrap.clone();
        let entry = Entry {
            pending: Pending {
                id: id.clone(),
                arrived_ms: self.clock.now_ms(),
                key_fingerprint: request.key_fingerprint,
                requester,
            },
            csr_pem: request.csr_pem,
            instance_uid: request.instance_uid,
            decision: None,
        };
        let took = match &self.store {
            Some(store) => store.take(
                &entry.persisted(),
                self.clock.now_ms(),
                MAX_PENDING,
                self.max_per_bootstrap,
            ),
            None => Ok(state.took(&entry, MAX_PENDING, self.max_per_bootstrap)),
        };
        let took = match took {
            Ok(took) => took,
            Err(e) => {
                // Not taken in: the host is told to come back, as it is for a full queue.
                tracing::warn!(error = %e, "cannot persist an enrolment request");
                return Submitted::Full;
            }
        };
        match took {
            Took::Added => {
                state.entries.insert(id, entry);
                drop(state);
                self.changed();
                Submitted::Waiting
            }
            Took::Held(held) => match Entry::restored(*held) {
                Ok(held) => {
                    let answered = answer(held.decision.as_ref());
                    state.entries.insert(id, held);
                    answered
                }
                Err(e) => {
                    tracing::warn!(error = %e, "an enrolment request the store holds does not read");
                    Submitted::Full
                }
            },
            Took::Full => Submitted::Full,
            Took::OverShare => Submitted::OverShare {
                first: state.over_share.insert(bootstrap),
            },
            Took::Closed => Submitted::Closed,
            Took::Rejected => Submitted::Rejected,
        }
    }

    /// The requests waiting for an operator, oldest first; one whose key is rejected waits for
    /// none.
    pub fn pending(&self) -> Vec<Pending> {
        let mut state = self.state.lock().expect("enrolment lock");
        self.expire(&mut state);
        let mut pending: Vec<Pending> = state
            .entries
            .values()
            .filter(|entry| {
                entry.decision.is_none() && !state.rejected.contains_key(&entry.pending.id)
            })
            .map(|entry| entry.pending.clone())
            .collect();
        pending.sort_by_key(|p| p.arrived_ms);
        pending
    }

    /// Approves one request: the CA signs it, and the certificate waits for the Agent to collect.
    /// Answers the serial of the certificate signed.
    ///
    /// # Errors
    /// Returns [`DecisionError::NotFound`] for an unknown, decided or expired id, and for a key
    /// that is rejected, here or in the store, [`DecisionError::Revoked`] when the request's
    /// bootstrap certificate is revoked, [`DecisionError::Subject`] when its subject cannot be
    /// carried into the host certificate (ADR-0015 clause 23), [`DecisionError::Sign`] when the CA
    /// or the register refuses the request, and [`DecisionError::Store`] when the decision could
    /// not be written; the request then still waits.
    ///
    /// `recorder` records the signature and prepares its register entry and Host, which are
    /// written with the decision (ADR-0044 clause 9): a decision that is not written leaves
    /// neither, and `recorder` records the signature as undecided. When a store that writes the
    /// register with its decision cannot tell whether its commit landed, the request is read
    /// back: decided with the certificate signed here, the approval stands.
    ///
    /// The bootstrap certificate the request came with is checked against `revoked` under the same
    /// lock that signs, so a revocation that lands before the approval always wins (ADR-0038
    /// clause 28); such a request is withdrawn and answered [`DecisionError::Revoked`].
    pub fn approve(
        &self,
        id: &str,
        signer: &dyn CertificateSigner,
        host: &str,
        recorder: &dyn Recorder,
        revoked: &dyn Fn(&CertId) -> bool,
    ) -> Result<String, DecisionError> {
        let mut state = self.state.lock().expect("enrolment lock");
        self.expire(&mut state);
        let entry = state
            .entries
            .get(id)
            .filter(|entry| entry.decision.is_none() && !state.rejected.contains_key(id))
            .ok_or(DecisionError::NotFound)?;
        let bootstrap = entry.pending.requester.bootstrap.clone();
        if revoked(&bootstrap) {
            return Err(self.withdrawn(state, id));
        }
        // The bootstrap certificate's values, under the rules they were issued by, and never the
        // request's (ADR-0022 clause 11 and ADR-0015 clause 23).
        let subject = entry.pending.requester.carried.clone().map_err(|e| {
            DecisionError::Subject(format!(
                "the bootstrap certificate's subject cannot be carried into a host \
                     certificate: {e}"
            ))
        })?;
        let (csr_pem, instance_uid) = (entry.csr_pem.clone(), entry.instance_uid.clone());
        // What was signed and recorded, once it is: a decision that is not written after it
        // leaves it undecided.
        let recorded: std::cell::RefCell<Option<(Signed, Option<Approved>)>> =
            std::cell::RefCell::new(None);
        let mut sign = || -> Result<Signing, DecisionError> {
            let signed = signer
                .sign(&csr_pem, host, &subject)
                .map_err(DecisionError::Sign)?;
            let approved = recorder.signed(&signed, id, &instance_uid, &bootstrap)?;
            let decision = Decision::Approved(signed.pem.clone());
            *recorded.borrow_mut() = Some((signed, approved.clone()));
            Ok((decision, approved))
        };
        // Persisted before it is answered, the certificate included, so a restart before the host
        // collects it hands it over all the same (ADR-0044 clause 8). The store signs only a
        // request that still waits there, and holds it meanwhile: one another instance decided or
        // withdrew is gone, and nothing is signed for it.
        let approval = match &self.store {
            Some(store) => store.approve(id, &bootstrap, &mut sign, recorder),
            None => Ok(match sign() {
                Err(e) => Approval::Refused(e),
                Ok((decision, approved)) => {
                    match approved
                        .as_ref()
                        .map(|approved| recorder.register(approved))
                    {
                        Some(Err(e)) => Approval::Refused(e),
                        _ => Approval::Approved(decision),
                    }
                }
            }),
        };
        let recorded = recorded.into_inner();
        let approval = match (approval, &recorded) {
            (Err(e), Some((signed, approved))) => {
                match self.read_back(id, signed, approved.as_ref(), recorder) {
                    Some(decision) => Ok(Approval::Approved(decision)),
                    None => Err(e),
                }
            }
            (approval, _) => approval,
        };
        let undecided = |outcome| {
            if let Some((signed, _)) = &recorded {
                recorder.undecided(signed, id, outcome);
            }
        };
        let approval = match approval {
            Ok(approval) => approval,
            Err(e) => return Err(DecisionError::Store(e)),
        };
        match approval {
            Approval::Approved(decision) => {
                if let Some(entry) = state.entries.get_mut(id) {
                    entry.decision = Some(decision);
                }
                drop(state);
                self.changed();
                Ok(recorded
                    .map(|(signed, _)| signed.facts.id.serial)
                    .unwrap_or_default())
            }
            Approval::NotWaiting => {
                state.entries.remove(id);
                Err(DecisionError::NotFound)
            }
            Approval::Revoked | Approval::Refused(DecisionError::Revoked) => {
                undecided(Undecided::RolledBack);
                Err(self.withdrawn(state, id))
            }
            Approval::Refused(e) => {
                undecided(Undecided::RolledBack);
                Err(e)
            }
            Approval::NotCommitted(e) => {
                undecided(Undecided::RolledBack);
                Err(DecisionError::Store(e))
            }
            Approval::Deciding => Err(DecisionError::Deciding),
        }
    }

    /// Reads back the request `id` after an approval whose store answered an error, when that
    /// store writes the register with its decision and so may have committed it all the same: the
    /// decision when it holds the certificate signed here, taken up through `recorder`; `None`
    /// otherwise, the signature recorded as undecided — rolled back when the request is read back
    /// without it, unknown when it cannot be read back. A store that writes the register after the
    /// decision took the decision back when it answered an error, so its signature is rolled back.
    fn read_back(
        &self,
        id: &str,
        signed: &Signed,
        approved: Option<&Approved>,
        recorder: &dyn Recorder,
    ) -> Option<Decision> {
        let store = self.store.as_ref()?;
        if !store.registers_with_its_decision() {
            recorder.undecided(signed, id, Undecided::RolledBack);
            return None;
        }
        let ours = Decision::Approved(signed.pem.clone());
        match store.read_back(id) {
            Ok(Some(request)) if request.decision.as_ref() == Some(&ours) => {
                tracing::warn!(request = %id, "an approval whose commit answered an error had committed");
                if let Some(approved) = approved {
                    recorder.committed(approved, None);
                }
                Some(ours)
            }
            Ok(_) => {
                recorder.undecided(signed, id, Undecided::RolledBack);
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, request = %id, "cannot read back an approval whose commit answered an error");
                recorder.undecided(signed, id, Undecided::Unknown);
                None
            }
        }
    }

    /// Whether the store writes an approval's register entry with its decision, in one
    /// transaction; a start then has nothing to take back (ADR-0044 clause 9).
    pub fn registers_with_its_decision(&self) -> bool {
        self.store
            .as_ref()
            .is_some_and(|store| store.registers_with_its_decision())
    }

    /// The requests approved and not yet collected, each with its certificate, PEM.
    pub fn approved(&self) -> Vec<(String, String)> {
        let state = self.state.lock().expect("enrolment lock");
        state
            .entries
            .values()
            .filter_map(|entry| match &entry.decision {
                Some(Decision::Approved(pem)) => Some((entry.pending.id.clone(), pem.clone())),
                _ => None,
            })
            .collect()
    }

    /// Takes the decision on the request `id` back, so it waits again: what a start does for an
    /// approved request whose certificate the register does not hold (ADR-0044 clause 9).
    ///
    /// # Errors
    /// Returns an error when the store cannot be written; the request is then left decided.
    pub fn take_back(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().expect("enrolment lock");
        self.on_store(|store| store.take_back(id))?;
        if let Some(entry) = state.entries.get_mut(id) {
            entry.decision = None;
        }
        drop(state);
        self.changed();
        Ok(())
    }

    /// Withdraws the waiting request `id`, whose bootstrap certificate is revoked, and answers
    /// [`DecisionError::Revoked`].
    fn withdrawn(&self, mut state: std::sync::MutexGuard<'_, State>, id: &str) -> DecisionError {
        state.entries.remove(id);
        let ids = [id.to_string()];
        self.on_store_logged(|store| store.withdraw(&ids));
        drop(state);
        self.changed();
        DecisionError::Revoked
    }

    /// Rejects the key `id`, beyond the window: its request is decided while it waits, and a
    /// request for it is refused when it arrives, with the window open or closed then. Answers
    /// whether the queue held the request.
    ///
    /// # Errors
    /// Returns [`DecisionError::NotFound`] for a request the queue holds decided, here or in the
    /// store, [`DecisionError::Deciding`] for one another instance is deciding, and
    /// [`DecisionError::Store`] when the rejection could not be written; nothing is then
    /// rejected.
    pub fn reject(&self, id: &str) -> Result<Rejection, DecisionError> {
        let mut state = self.state.lock().expect("enrolment lock");
        self.expire(&mut state);
        if state
            .entries
            .get(id)
            .is_some_and(|entry| entry.decision.is_some())
        {
            return Err(DecisionError::NotFound);
        }
        let now = self.clock.now_ms();
        let decided = match &self.store {
            Some(store) => store.reject(id, now).map_err(DecisionError::Store)?,
            None if state.entries.contains_key(id) => Decided::Written,
            None => Decided::Listed,
        };
        let rejection = match decided {
            Decided::Written => Rejection::Queued,
            Decided::Listed => Rejection::NotQueued,
            Decided::NotWaiting => {
                state.entries.remove(id);
                return Err(DecisionError::NotFound);
            }
            Decided::Deciding => return Err(DecisionError::Deciding),
        };
        if let Some(entry) = state.entries.get_mut(id) {
            entry.decision = Some(Decision::Rejected);
        }
        state.rejected.entry(id.to_string()).or_insert(now);
        drop(state);
        self.changed();
        Ok(rejection)
    }

    /// Withdraws every waiting request whose bootstrap certificate `revoked` names, and answers how
    /// many: a revoked certificate opens nothing, a request it carried included (ADR-0038
    /// clause 28). A decided request is left: an approved one was signed and recorded already, and
    /// dropping it would revoke nothing — the connection of this certificate is refused, and only
    /// one holding the same key can collect it.
    pub fn withdraw(&self, revoked: &dyn Fn(&CertId) -> bool) -> usize {
        let mut state = self.state.lock().expect("enrolment lock");
        let ids: Vec<String> = state
            .entries
            .values()
            .filter(|entry| entry.decision.is_none() && revoked(&entry.pending.requester.bootstrap))
            .map(|entry| entry.pending.id.clone())
            .collect();
        for id in &ids {
            state.entries.remove(id);
        }
        let withdrawn = ids.len();
        if withdrawn > 0 {
            self.on_store_logged(|store| store.withdraw(&ids));
        }
        drop(state);
        if withdrawn > 0 {
            self.changed();
        }
        withdrawn
    }

    /// Withdraws every waiting request whose bootstrap certificate is `serial` under one of
    /// `issuers`, held here or by another instance in the store, and answers how many: what the
    /// revocation of a bootstrap certificate does to the queue (ADR-0038 clause 28).
    pub fn withdraw_revoked(&self, issuers: &[String], serial: &str) -> usize {
        let named =
            |bootstrap: &CertId| bootstrap.serial == serial && issuers.contains(&bootstrap.issuer);
        let mut state = self.state.lock().expect("enrolment lock");
        let ids: Vec<String> = state
            .entries
            .values()
            .filter(|entry| entry.decision.is_none() && named(&entry.pending.requester.bootstrap))
            .map(|entry| entry.pending.id.clone())
            .collect();
        for id in &ids {
            state.entries.remove(id);
        }
        let withdrawn = match &self.store {
            Some(store) => store
                .withdraw_bootstrap(issuers, serial)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "cannot withdraw the requests of a revoked bootstrap certificate");
                    ids.len()
                }),
            None => ids.len(),
        };
        drop(state);
        if withdrawn > 0 || !ids.is_empty() {
            self.changed();
        }
        withdrawn
    }

    /// Whether the request for `key` was rejected — its connection then ends.
    pub fn is_rejected(&self, key: &str) -> bool {
        let state = self.state.lock().expect("enrolment lock");
        state.rejected.contains_key(key)
            || matches!(
                state
                    .entries
                    .get(key)
                    .and_then(|entry| entry.decision.as_ref()),
                Some(Decision::Rejected)
            )
    }

    /// Reads the window and its queue again from the store, as another instance changed them
    /// (ADR-0041
    /// clauses 9 and 10), and wakes every connection waiting on them, so a request another instance
    /// approved is handed to the connection that asked. It reads under the lock, so no act of this
    /// instance's own lands between the read and the swap; the queue is bounded, so the read is
    /// short. Without a store there is nothing to read.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read; what is held is then kept.
    pub fn reload(&self) -> Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let mut state = self.state.lock().expect("enrolment lock");
        let mut fresh = match store.load()? {
            Some(persisted) => State::restored(persisted)?,
            None => State::default(),
        };
        if fresh.window_until_ms == state.window_until_ms {
            fresh.over_share = std::mem::take(&mut state.over_share);
        }
        *state = fresh;
        drop(state);
        self.changed();
        Ok(())
    }

    /// Fires whenever the window or a request changes, for the connections waiting on them.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// The window's end if it is still open; a window that has run out is closed here, and its
    /// requests expire with it. The rejected keys stay.
    fn expire(&self, state: &mut State) -> Option<u64> {
        let until = state.window_until_ms?;
        if self.clock.now_ms() < until {
            return Some(until);
        }
        let expired = state
            .entries
            .values()
            .filter(|entry| entry.decision.is_none())
            .count();
        state.close();
        // Another instance may have moved the window's end meanwhile: then it did not run out,
        // and nothing expired with it.
        let ended = match &self.store {
            Some(store) => store.end_window(until).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot persist the enrolment window and its requests");
                true
            }),
            None => true,
        };
        if let Some(audit) = self.audit.get().filter(|_| ended) {
            audit.refusal(
                crate::audit::Entry::new("enrolment.window", "expired")
                    .with("until_ms", until)
                    .with("expired_requests", expired),
            );
        }
        self.changes.send_modify(|rev| *rev += 1);
        None
    }

    fn changed(&self) {
        self.changes.send_modify(|rev| *rev += 1);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) struct Manual(pub(crate) AtomicU64);

    impl Clock for Manual {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    pub(crate) struct Echo;

    impl CertificateSigner for Echo {
        fn sign(
            &self,
            csr_pem: &str,
            _host: &str,
            subject: &SubjectValues,
        ) -> Result<Signed, String> {
            Ok(Signed {
                pem: format!("issued for {csr_pem}"),
                facts: crate::revocation::Facts {
                    id: crate::revocation::CertId::new(b"test CA", "1"),
                    issuer_name: "CN=test CA".to_string(),
                    subject: String::new(),
                    key_fingerprint: String::new(),
                    not_after_ms: u64::MAX,
                    host: None,
                    subject_values: subject.clone(),
                },
            })
        }

        fn check_claims(&self, _csr_pem: &str, _sender: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn renewal_proof(
            &self,
            _csr_pem: &str,
        ) -> Result<Option<crate::revocation::Facts>, String> {
            Ok(None)
        }

        fn bootstrap_proof_id(
            &self,
            _csr_pem: &str,
        ) -> Result<Option<crate::revocation::CertId>, String> {
            Ok(None)
        }
    }

    fn enrolment() -> (Enrolment, Arc<Manual>) {
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        (Enrolment::new(clock.clone()), clock)
    }

    pub(crate) fn request(key: &str) -> Request {
        Request {
            csr_pem: format!("csr-{key}"),
            subject: "CN=edge-01".to_string(),
            key_fingerprint: key.to_string(),
            instance_uid: vec![7; 16],
        }
    }

    /// Records each signature through its closure and keeps no register, as a Server without one:
    /// nothing is prepared, registered or taken up, and what was not decided is counted.
    pub(crate) struct Recording<F> {
        pub(crate) record: F,
        pub(crate) undecided: std::sync::Mutex<Vec<(String, Undecided)>>,
    }

    impl<F> Recording<F> {
        pub(crate) fn new(record: F) -> Self {
            Recording {
                record,
                undecided: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    /// Prepares an approval's register entry and Host on `revocations`, writes and takes them up
    /// as the Server does, and counts what it signed, took up and left undecided.
    pub(crate) struct Registering<'a> {
        pub(crate) revocations: &'a crate::revocation::Revocations,
        pub(crate) signed: std::sync::Mutex<Vec<String>>,
        pub(crate) committed: std::sync::Mutex<Vec<String>>,
        pub(crate) undecided: std::sync::Mutex<Vec<(String, String, Undecided)>>,
    }

    impl<'a> Registering<'a> {
        pub(crate) fn on(revocations: &'a crate::revocation::Revocations) -> Self {
            Registering {
                revocations,
                signed: std::sync::Mutex::new(Vec::new()),
                committed: std::sync::Mutex::new(Vec::new()),
                undecided: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl Registrar for Registering<'_> {
        fn register(&self, approved: &Approved) -> Result<(), DecisionError> {
            self.revocations
                .register_approved(approved)
                .map_err(DecisionError::Sign)
        }
        fn committed(&self, approved: &Approved, written: Option<u64>) {
            self.committed
                .lock()
                .expect("committed")
                .push(approved.issued().facts.id.serial.clone());
            self.revocations.took_up_approved(approved, written);
        }
    }

    impl Recorder for Registering<'_> {
        fn signed(
            &self,
            signed: &Signed,
            _: &str,
            instance_uid: &[u8],
            bootstrap: &CertId,
        ) -> Result<Option<Approved>, DecisionError> {
            let approved = self
                .revocations
                .prepare_approved(signed.facts.clone(), instance_uid, bootstrap)
                .map_err(DecisionError::Sign)?;
            self.signed
                .lock()
                .expect("signed")
                .push(signed.facts.id.serial.clone());
            Ok(Some(approved))
        }
        fn undecided(&self, signed: &Signed, request: &str, outcome: Undecided) {
            self.undecided.lock().expect("undecided").push((
                request.to_string(),
                signed.facts.id.serial.clone(),
                outcome,
            ));
        }
    }

    /// Signs each request with a serial of its own, counting from 1, for the host it is asked for.
    #[derive(Default)]
    pub(crate) struct Counting(pub(crate) AtomicU64);

    impl CertificateSigner for Counting {
        fn sign(
            &self,
            csr_pem: &str,
            host: &str,
            subject: &SubjectValues,
        ) -> Result<Signed, String> {
            let serial = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            Ok(Signed {
                pem: format!("serial {serial} for {csr_pem}"),
                facts: crate::revocation::Facts {
                    id: crate::revocation::CertId::new(b"test CA", &serial.to_string()),
                    issuer_name: "CN=test CA".to_string(),
                    subject: String::new(),
                    key_fingerprint: csr_pem.to_string(),
                    not_after_ms: u64::MAX / 2,
                    host: Some(host.to_string()),
                    subject_values: subject.clone(),
                },
            })
        }

        fn check_claims(&self, _csr_pem: &str, _sender: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn renewal_proof(
            &self,
            _csr_pem: &str,
        ) -> Result<Option<crate::revocation::Facts>, String> {
            Ok(None)
        }

        fn bootstrap_proof_id(
            &self,
            _csr_pem: &str,
        ) -> Result<Option<crate::revocation::CertId>, String> {
            Ok(None)
        }
    }

    /// What a test records of a signature: nothing, or a refusal.
    pub(crate) type Record = fn(&Signed, &[u8], &CertId) -> Result<(), DecisionError>;

    /// A [`Recording`] that records nothing and refuses nothing.
    pub(crate) fn kept() -> Recording<Record> {
        Recording::new(|_, _, _| Ok(()))
    }

    impl<F> Registrar for Recording<F> {
        fn register(&self, _: &Approved) -> Result<(), DecisionError> {
            Ok(())
        }
        fn committed(&self, _: &Approved, _: Option<u64>) {}
    }

    impl<F: Fn(&Signed, &[u8], &CertId) -> Result<(), DecisionError>> Recorder for Recording<F> {
        fn signed(
            &self,
            signed: &Signed,
            _: &str,
            instance_uid: &[u8],
            bootstrap: &CertId,
        ) -> Result<Option<Approved>, DecisionError> {
            (self.record)(signed, instance_uid, bootstrap).map(|()| None)
        }
        fn undecided(&self, _: &Signed, request: &str, outcome: Undecided) {
            self.undecided
                .lock()
                .expect("undecided")
                .push((request.to_string(), outcome));
        }
    }

    pub(crate) fn requester() -> Requester {
        Requester {
            bootstrap_subject: "CN=bootstrap".to_string(),
            carried: Ok(SubjectValues::default()),
            bootstrap_fingerprint: "bf".to_string(),
            bootstrap: CertId::new(b"bootstrap CA", "b5"),
            peer: None,
            gateway: None,
        }
    }

    /// Two instances on one store answer a request as the store holds it: one the other approved
    /// is handed out with its certificate, and one the other decided or withdrew is not found,
    /// however the first held it, and signs nothing.
    /// Verifies: ADR-0041
    #[test]
    fn two_instances_on_one_store_answer_a_request_as_the_store_holds_it() {
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let store = Memory::default();
        let a = Enrolment::open_on(clock.clone(), Box::new(store.clone())).expect("open A");
        a.open(600).expect("window");
        let b = Enrolment::open_on(clock.clone(), Box::new(store.clone())).expect("open B");
        let recorded = std::cell::Cell::new(0);
        let record = |_: &Signed, _: &[u8], _: &CertId| {
            recorded.set(recorded.get() + 1);
            Ok(())
        };

        assert_eq!(a.submit(request("k1"), requester()), Submitted::Waiting);
        a.approve("k1", &Echo, "host", &Recording::new(&record), &|_| false)
            .expect("approve on A");
        assert_eq!(
            b.submit(request("k1"), requester()),
            Submitted::Issued("issued for csr-k1".to_string()),
            "B hands out what A approved"
        );

        for (key, decide) in [("k2", "reject"), ("k3", "approve"), ("k4", "withdraw")] {
            assert_eq!(a.submit(request(key), requester()), Submitted::Waiting);
            assert_eq!(
                b.submit(request(key), requester()),
                Submitted::Waiting,
                "B holds {key} waiting, as the store has it"
            );
            match decide {
                "reject" => {
                    a.reject(key).expect("reject on A");
                }
                "approve" => {
                    a.approve(key, &Echo, "host", &Recording::new(&record), &|_| false)
                        .expect("approve on A");
                }
                _ => {
                    let bootstrap = requester().bootstrap;
                    assert_eq!(
                        a.withdraw_revoked(&[bootstrap.issuer], &bootstrap.serial),
                        1
                    );
                }
            }
            let before = recorded.get();
            // A request the store holds decided is not found; one it no longer holds is rejected
            // before it arrives again.
            let rejected = b.reject(key);
            if decide == "withdraw" {
                assert!(rejected.is_ok(), "{key}: {rejected:?}");
            } else {
                assert_eq!(
                    rejected.err(),
                    Some(DecisionError::NotFound),
                    "{key}: {decide}"
                );
            }
            assert_eq!(
                b.submit(request(key), requester()),
                match decide {
                    "approve" => Submitted::Issued(format!("issued for csr-{key}")),
                    _ => Submitted::Rejected,
                },
                "{key}: B answers as the store holds it"
            );
            if decide == "approve" {
                assert_eq!(
                    b.approve(key, &Echo, "host", &Recording::new(&record), &|_| false),
                    Err(DecisionError::NotFound)
                );
            }
            assert_eq!(
                recorded.get(),
                before,
                "{key}: B signed and recorded nothing"
            );
        }
    }

    /// The store as memory: what an Enrolment opened on a clone of it reads is what the first one
    /// persisted — a restart without a filesystem.
    #[derive(Clone, Default)]
    struct Memory(Arc<Mutex<Option<Persisted>>>);

    impl EnrolmentStore for Memory {
        fn load(&self) -> Result<Option<Persisted>, String> {
            Ok(self.0.lock().expect("memory").clone())
        }
        fn save(&self, persisted: &Persisted) -> Result<(), String> {
            *self.0.lock().expect("memory") = Some(persisted.clone());
            Ok(())
        }
    }

    /// An approved enrolment survives a restart: the window stays open until its end, a waiting
    /// request still waits, and the approved one hands its host the certificate it was issued.
    /// Verifies: ADR-0044
    #[test]
    fn an_approved_enrolment_survives_a_restart() {
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let memory = Memory::default();
        let until = {
            let enrolment =
                Enrolment::open_on(clock.clone(), Box::new(memory.clone())).expect("open");
            let until = enrolment.open(600).expect("open the window");
            enrolment.submit(request("a"), requester());
            enrolment.submit(request("b"), requester());
            enrolment.submit(request("c"), requester());
            enrolment
                .approve("a", &Echo, "host", &kept(), &|_| false)
                .expect("approve");
            enrolment.reject("c").expect("reject");
            until
        };

        clock.0.fetch_add(60_000, Ordering::SeqCst);
        let restarted =
            Enrolment::open_on(clock.clone(), Box::new(memory.clone())).expect("reopen");
        assert_eq!(restarted.window(), Some(until), "open until its end");
        assert_eq!(
            restarted.submit(request("a"), requester()),
            Submitted::Issued("issued for csr-a".to_string()),
            "the approved certificate is handed over after the restart"
        );
        assert_eq!(
            restarted
                .pending()
                .into_iter()
                .map(|p| (p.id, p.requester))
                .collect::<Vec<_>>(),
            [("b".to_string(), requester())]
        );
        assert!(restarted.is_rejected("c"));
        restarted
            .approve("b", &Echo, "host", &kept(), &|_| false)
            .expect("the waiting request can still be approved");
    }

    /// A window whose end passed while the Server was down is closed when it is loaded, and its
    /// requests — an approved one included — expire with it.
    /// Verifies: ADR-0044
    #[test]
    fn a_window_whose_end_passed_while_down_is_closed_on_load() {
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let memory = Memory::default();
        {
            let enrolment =
                Enrolment::open_on(clock.clone(), Box::new(memory.clone())).expect("open");
            enrolment.open(60).expect("open the window");
            enrolment.submit(request("a"), requester());
            enrolment
                .approve("a", &Echo, "host", &kept(), &|_| false)
                .expect("approve");
        }
        clock.0.fetch_add(61_000, Ordering::SeqCst);
        let restarted =
            Enrolment::open_on(clock.clone(), Box::new(memory.clone())).expect("reopen");
        assert_eq!(restarted.window(), None);
        assert_eq!(
            memory.load().expect("load"),
            Some(Persisted::default()),
            "closed on disk too"
        );
        restarted.open(60).expect("open again");
        assert_eq!(
            restarted.submit(request("a"), requester()),
            Submitted::Waiting,
            "the host enrols again in the next window"
        );
    }

    /// An act whose result cannot be persisted is not taken: the window stays as it was, and a
    /// decision leaves the request waiting.
    /// Verifies: ADR-0044
    #[test]
    fn an_act_that_cannot_be_persisted_changes_nothing() {
        // It holds what it wrote, as every store does: an act decides from what is stored.
        #[derive(Clone, Default)]
        struct Failing(Arc<std::sync::atomic::AtomicBool>, Memory);
        impl EnrolmentStore for Failing {
            fn load(&self) -> Result<Option<Persisted>, String> {
                self.1.load()
            }
            fn save(&self, persisted: &Persisted) -> Result<(), String> {
                if self.0.load(Ordering::SeqCst) {
                    Err("the disk is full".to_string())
                } else {
                    self.1.save(persisted)
                }
            }
        }
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let store = Failing::default();
        let enrolment = Enrolment::open_on(clock, Box::new(store.clone())).expect("open");
        enrolment.open(600).expect("open the window");
        enrolment.submit(request("a"), requester());
        store.0.store(true, Ordering::SeqCst);
        assert!(matches!(enrolment.open(60), Err(WindowError::Store(_))));
        assert_eq!(
            enrolment.submit(request("b"), requester()),
            Submitted::Full,
            "a request that cannot be kept is not taken in"
        );
        assert!(matches!(
            enrolment.approve("a", &Echo, "host", &kept(), &|_| false),
            Err(DecisionError::Store(_))
        ));
        assert!(matches!(
            enrolment.reject("a"),
            Err(DecisionError::Store(_))
        ));
        assert_eq!(enrolment.pending().len(), 1, "the request still waits");
        assert!(enrolment.close().is_err());
        assert!(!enrolment.is_open(), "closed all the same");
    }

    /// A decision on a request another instance is deciding at this moment is answered so, and
    /// decides nothing: the request still waits here, to be decided once asked again.
    /// Verifies: ADR-0041
    #[test]
    fn a_decision_on_a_request_another_instance_is_deciding_is_answered_so() {
        #[derive(Clone, Default)]
        struct Deciding(Memory);
        impl EnrolmentStore for Deciding {
            fn load(&self) -> Result<Option<Persisted>, String> {
                self.0.load()
            }
            fn save(&self, persisted: &Persisted) -> Result<(), String> {
                self.0.save(persisted)
            }
            fn reject(&self, _: &str, _: u64) -> Result<Decided, String> {
                Ok(Decided::Deciding)
            }
            fn approve(
                &self,
                _: &str,
                _: &CertId,
                _: &mut dyn FnMut() -> Result<Signing, DecisionError>,
                _: &dyn Registrar,
            ) -> Result<Approval, String> {
                Ok(Approval::Deciding)
            }
        }
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let enrolment = Enrolment::open_on(clock, Box::new(Deciding::default())).expect("open");
        enrolment.open(600).expect("open the window");
        enrolment.submit(request("a"), requester());
        assert_eq!(
            enrolment.approve("a", &Echo, "host", &kept(), &|_| false),
            Err(DecisionError::Deciding)
        );
        assert_eq!(enrolment.reject("a"), Err(DecisionError::Deciding));
        assert_eq!(enrolment.pending().len(), 1, "the request still waits here");
        assert!(!enrolment.is_rejected("a"));
    }

    /// Verifies: ADR-0038
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

    /// Verifies: ADR-0038
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
        enrolment
            .approve("a", &Echo, "host", &kept(), &|_| false)
            .expect("approve");
        assert_eq!(
            enrolment.submit(request("a"), requester()),
            Submitted::Issued("issued for csr-a".to_string())
        );
        assert!(
            enrolment.pending().is_empty(),
            "a decided request is no longer pending"
        );
        assert_eq!(
            enrolment.approve("a", &Echo, "host", &kept(), &|_| false),
            Err(DecisionError::NotFound)
        );

        enrolment.submit(request("b"), requester());
        enrolment.reject("b").expect("reject");
        assert_eq!(
            enrolment.submit(request("b"), requester()),
            Submitted::Rejected
        );
        enrolment
            .reject("unknown")
            .expect("an id the queue does not hold is rejected");
        assert_eq!(
            enrolment.submit(request("unknown"), requester()),
            Submitted::Rejected,
            "and its request is refused when it arrives"
        );
    }

    /// Verifies: ADR-0038
    #[test]
    fn closing_the_window_expires_every_request() {
        let (enrolment, clock) = enrolment();
        enrolment.open(60).expect("open");
        enrolment.submit(request("a"), requester());
        enrolment.close().expect("close");
        assert!(enrolment.pending().is_empty());
        enrolment.open(60).expect("open");
        enrolment.submit(request("b"), requester());
        clock.0.fetch_add(61_000, Ordering::SeqCst);
        assert_eq!(
            enrolment.approve("b", &Echo, "host", &kept(), &|_| false),
            Err(DecisionError::NotFound)
        );
    }

    /// Verifies: ADR-0038
    #[test]
    fn the_queue_is_bounded() {
        let (enrolment, _) = enrolment();
        enrolment.open(600).expect("open");
        // One bootstrap certificate per request, so only the queue's own bound applies (ADR-0022).
        for n in 0..MAX_PENDING {
            let one_each = Requester {
                bootstrap: CertId::new(b"bootstrap CA", &format!("{n:x}")),
                ..requester()
            };
            assert_eq!(
                enrolment.submit(request(&n.to_string()), one_each),
                Submitted::Waiting
            );
        }
        assert_eq!(
            enrolment.submit(request("one more"), requester()),
            Submitted::Full
        );
    }

    /// The requests of one bootstrap certificate hold at most their share of the queue; a re-sent
    /// one joins its entry all the same, another certificate's is still queued, and the first
    /// refusal of a certificate in a window is the one marked for the audit.
    /// Verifies: ADR-0022
    #[test]
    fn one_bootstrap_certificate_holds_at_most_its_share() {
        let (enrolment, _) = enrolment();
        let enrolment = enrolment.with_max_pending_per_bootstrap(2);
        enrolment.open(600).expect("open");
        assert_eq!(
            enrolment.submit(request("a"), requester()),
            Submitted::Waiting
        );
        assert_eq!(
            enrolment.submit(request("b"), requester()),
            Submitted::Waiting
        );
        assert_eq!(
            enrolment.submit(request("c"), requester()),
            Submitted::OverShare { first: true }
        );
        assert_eq!(
            enrolment.submit(request("d"), requester()),
            Submitted::OverShare { first: false },
            "audited once per certificate and window"
        );
        assert_eq!(
            enrolment.submit(request("a"), requester()),
            Submitted::Waiting,
            "a re-sent request is never refused"
        );
        let other = Requester {
            bootstrap: CertId::new(b"bootstrap CA", "c6"),
            ..requester()
        };
        assert_eq!(enrolment.submit(request("e"), other), Submitted::Waiting);
        assert_eq!(enrolment.pending().len(), 3);

        enrolment.close().expect("close");
        enrolment.open(600).expect("open again");
        enrolment.submit(request("a"), requester());
        enrolment.submit(request("b"), requester());
        assert_eq!(
            enrolment.submit(request("c"), requester()),
            Submitted::OverShare { first: true },
            "a new window audits again"
        );
    }

    /// The default share is a quarter of the queue.
    /// Verifies: ADR-0022
    #[test]
    fn the_default_share_is_a_quarter_of_the_queue() {
        assert_eq!(DEFAULT_MAX_PENDING_PER_BOOTSTRAP, 256);
        assert_eq!(DEFAULT_MAX_PENDING_PER_BOOTSTRAP * 4, MAX_PENDING);
    }

    /// A signer that fails the test if it is ever asked.
    struct Never;

    impl CertificateSigner for Never {
        fn sign(
            &self,
            _csr_pem: &str,
            _host: &str,
            _subject: &SubjectValues,
        ) -> Result<Signed, String> {
            panic!("a request that must not be signed reached the signer");
        }

        fn check_claims(&self, _csr_pem: &str, _sender: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn renewal_proof(
            &self,
            _csr_pem: &str,
        ) -> Result<Option<crate::revocation::Facts>, String> {
            Ok(None)
        }

        fn bootstrap_proof_id(
            &self,
            _csr_pem: &str,
        ) -> Result<Option<crate::revocation::CertId>, String> {
            Ok(None)
        }
    }

    /// Verifies: ADR-0038
    #[test]
    fn a_request_whose_bootstrap_certificate_is_revoked_is_withdrawn_unsigned() {
        let (enrolment, _clock) = enrolment();
        enrolment.open(600).expect("open");
        enrolment.submit(request("a"), requester());
        let bootstrap = requester().bootstrap;
        let revoked = |id: &CertId| *id == bootstrap;
        assert_eq!(
            enrolment.approve("a", &Never, "host", &kept(), &revoked),
            Err(DecisionError::Revoked)
        );
        assert!(enrolment.pending().is_empty(), "the request is withdrawn");
        assert_eq!(
            enrolment.approve("a", &Never, "host", &kept(), &revoked),
            Err(DecisionError::NotFound),
            "a second approval finds nothing to sign"
        );
    }

    /// An approval signs the values the bootstrap certificate carries, and one whose subject breaks
    /// the rules signs nothing, says why, and leaves the request for the operator to reject.
    /// Verifies: ADR-0022
    #[test]
    fn an_approval_carries_the_bootstrap_values_or_signs_nothing() {
        let (enrolment, _clock) = enrolment();
        enrolment.open(600).expect("open");
        let values = SubjectValues {
            country: "AT".to_string(),
            organization: "Kunde GmbH".to_string(),
            ..SubjectValues::default()
        };
        let carrying = Requester {
            carried: Ok(values.clone()),
            ..requester()
        };
        enrolment.submit(request("good"), carrying);
        let signed = std::cell::RefCell::new(None);
        enrolment
            .approve(
                "good",
                &Echo,
                "host",
                &Recording::new(|issued: &Signed, _: &[u8], _: &CertId| {
                    *signed.borrow_mut() = Some(issued.facts.subject_values.clone());
                    Ok(())
                }),
                &|_| false,
            )
            .expect("approve");
        assert_eq!(signed.into_inner(), Some(values));

        let malformed = Requester {
            carried: Err("organizational_unit appears more than once".to_string()),
            ..requester()
        };
        enrolment.submit(request("bad"), malformed);
        let refused = enrolment.approve("bad", &Never, "host", &kept(), &|_| false);
        assert!(
            matches!(&refused, Err(DecisionError::Subject(e)) if e.contains("organizational_unit")),
            "{refused:?}"
        );
        assert_eq!(enrolment.pending().len(), 1, "the request still waits");
    }

    /// Verifies: ADR-0038
    #[test]
    fn withdrawing_takes_only_the_waiting_requests_of_a_revoked_certificate() {
        let (enrolment, _clock) = enrolment();
        enrolment.open(600).expect("open");
        let other = Requester {
            bootstrap: CertId::new(b"bootstrap CA", "c6"),
            ..requester()
        };
        enrolment.submit(request("revoked-waiting"), requester());
        enrolment.submit(request("revoked-decided"), requester());
        enrolment.submit(request("other"), other);
        enrolment
            .approve("revoked-decided", &Echo, "host", &kept(), &|_| false)
            .expect("approve");
        let bootstrap = requester().bootstrap;
        assert_eq!(enrolment.withdraw(&|id| *id == bootstrap), 1);
        let left: Vec<String> = enrolment.pending().into_iter().map(|p| p.id).collect();
        assert_eq!(
            left,
            ["other"],
            "the other certificate's request still waits"
        );
        assert!(
            matches!(
                enrolment.submit(request("revoked-decided"), requester()),
                Submitted::Issued(_)
            ),
            "a decided request is left to its connection, which the revocation refuses"
        );
    }

    /// How a store that writes the register with its decision answers an approval, for the
    /// read-back tests: the decision written and its answer lost, nothing written and an error,
    /// the register's write refused, or an error and no read back either.
    #[derive(Clone, Copy)]
    enum Commit {
        Landed,
        Lost,
        Refused,
        Unreadable,
        /// The answer lost, and the request decided by another instance meanwhile.
        Foreign,
        /// The database server refused the commit; reading back would fail.
        NotCommitted,
    }

    #[derive(Clone)]
    struct Committing(Memory, Commit);

    impl EnrolmentStore for Committing {
        fn load(&self) -> Result<Option<Persisted>, String> {
            self.0.load()
        }
        fn save(&self, persisted: &Persisted) -> Result<(), String> {
            self.0.save(persisted)
        }
        fn registers_with_its_decision(&self) -> bool {
            true
        }
        fn approve(
            &self,
            id: &str,
            _: &CertId,
            sign: &mut dyn FnMut() -> Result<Signing, DecisionError>,
            _: &dyn Registrar,
        ) -> Result<Approval, String> {
            let (decision, _) = match sign() {
                Ok(signing) => signing,
                Err(e) => return Ok(Approval::Refused(e)),
            };
            match self.1 {
                Commit::Landed => {
                    assert_eq!(self.0.decide(id, &decision), Ok(Decided::Written));
                    Err("the commit's answer was lost".to_string())
                }
                Commit::Lost | Commit::Unreadable => Err("the commit failed".to_string()),
                Commit::Foreign => {
                    let theirs = Decision::Approved("another instance's certificate".to_string());
                    assert_eq!(self.0.decide(id, &theirs), Ok(Decided::Written));
                    Err("the commit's answer was lost".to_string())
                }
                Commit::NotCommitted => {
                    Ok(Approval::NotCommitted("the commit is refused".to_string()))
                }
                Commit::Refused => Ok(Approval::Refused(DecisionError::Sign(
                    "the register is full".to_string(),
                ))),
            }
        }
        fn read_back(&self, id: &str) -> Result<Option<PersistedRequest>, String> {
            match self.1 {
                Commit::Unreadable | Commit::NotCommitted => {
                    Err("the database does not answer".to_string())
                }
                _ => Ok(self
                    .load()?
                    .and_then(|persisted| persisted.requests.into_iter().find(|r| r.id == id))),
            }
        }
    }

    /// A register in `dir`, as the Server on files opens it.
    fn register_in(dir: &std::path::Path) -> crate::revocation::Revocations {
        crate::revocation::Revocations::open(
            Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("register")
    }

    /// A window open on a store that answers approvals as `commit` says, with the request `k`
    /// waiting.
    fn committing(commit: Commit) -> Enrolment {
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let enrolment = Enrolment::open_on(clock, Box::new(Committing(Memory::default(), commit)))
            .expect("open");
        enrolment.open(600).expect("window");
        assert_eq!(
            enrolment.submit(request("k"), requester()),
            Submitted::Waiting
        );
        enrolment
    }

    /// An approval whose commit landed but whose answer was lost is read back: decided with the
    /// certificate signed here, it stands, its register entry is taken up, and nothing is recorded
    /// as undecided (ADR-0044 clause 9).
    /// Verifies: ADR-0050
    #[test]
    fn a_commit_that_landed_and_whose_answer_was_lost_is_read_back_as_approved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let register = register_in(dir.path());
        let recorder = Registering::on(&register);
        let enrolment = committing(Commit::Landed);
        assert_eq!(
            enrolment.approve("k", &Counting::default(), "host-1", &recorder, &|_| false),
            Ok("1".to_string())
        );
        assert_eq!(*recorder.committed.lock().expect("committed"), ["1"]);
        assert!(recorder.undecided.lock().expect("undecided").is_empty());
        assert_eq!(register.issued().len(), 1, "taken up");
        assert_eq!(
            enrolment.submit(request("k"), requester()),
            Submitted::Issued("serial 1 for csr-k".to_string())
        );
    }

    /// A signature whose decision was not written is recorded as undecided: rolled back when the
    /// store refused the register's write or the request is read back waiting, unknown when it
    /// cannot be read back. The request still waits, and nothing is taken up. A signature the
    /// audit record did not take is not recorded as undecided either (ADR-0044 clause 9).
    /// Verifies: ADR-0050
    #[test]
    fn a_signature_whose_decision_was_not_written_is_recorded_undecided() {
        for (commit, outcome, error) in [
            (
                Commit::Lost,
                Undecided::RolledBack,
                DecisionError::Store("the commit failed".to_string()),
            ),
            (
                Commit::Unreadable,
                Undecided::Unknown,
                DecisionError::Store("the commit failed".to_string()),
            ),
            (
                Commit::Refused,
                Undecided::RolledBack,
                DecisionError::Sign("the register is full".to_string()),
            ),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let register = register_in(dir.path());
            let recorder = Registering::on(&register);
            let enrolment = committing(commit);
            assert_eq!(
                enrolment.approve("k", &Counting::default(), "host-1", &recorder, &|_| false),
                Err(error)
            );
            assert_eq!(
                *recorder.undecided.lock().expect("undecided"),
                [("k".to_string(), "1".to_string(), outcome)]
            );
            assert!(recorder.committed.lock().expect("committed").is_empty());
            assert!(register.issued().is_empty());
            assert_eq!(enrolment.pending().len(), 1, "the request still waits");
        }
        let unrecorded = Recording::new(|_: &Signed, _: &[u8], _: &CertId| {
            Err(DecisionError::Sign(
                "the audit record is unavailable".to_string(),
            ))
        });
        let enrolment = committing(Commit::Lost);
        assert_eq!(
            enrolment.approve("k", &Counting::default(), "host-1", &unrecorded, &|_| false),
            Err(DecisionError::Sign(
                "the audit record is unavailable".to_string()
            ))
        );
        assert!(unrecorded.undecided.lock().expect("undecided").is_empty());
    }

    /// An approval whose answer was lost while another instance decided the request with a
    /// certificate of its own is not taken for its own: the read back finds another certificate,
    /// so the signature is rolled back and nothing is taken up (ADR-0044 clause 9).
    /// Verifies: ADR-0050
    #[test]
    fn a_read_back_that_finds_another_certificate_rolls_the_signature_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let register = register_in(dir.path());
        let recorder = Registering::on(&register);
        let enrolment = committing(Commit::Foreign);
        assert_eq!(
            enrolment.approve("k", &Counting::default(), "host-1", &recorder, &|_| false),
            Err(DecisionError::Store(
                "the commit's answer was lost".to_string()
            ))
        );
        assert_eq!(
            *recorder.undecided.lock().expect("undecided"),
            [("k".to_string(), "1".to_string(), Undecided::RolledBack)]
        );
        assert!(recorder.committed.lock().expect("committed").is_empty());
        assert!(register.issued().is_empty(), "nothing taken up");
    }

    /// A commit the database server itself refused is rolled back as it stands: no read back is
    /// asked, so one that would fail cannot leave it unknown (ADR-0044 clause 9).
    /// Verifies: ADR-0050
    #[test]
    fn a_refusal_the_database_answers_is_rolled_back_without_a_read_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let register = register_in(dir.path());
        let recorder = Registering::on(&register);
        let enrolment = committing(Commit::NotCommitted);
        assert_eq!(
            enrolment.approve("k", &Counting::default(), "host-1", &recorder, &|_| false),
            Err(DecisionError::Store("the commit is refused".to_string()))
        );
        assert_eq!(
            *recorder.undecided.lock().expect("undecided"),
            [("k".to_string(), "1".to_string(), Undecided::RolledBack)]
        );
        assert!(register.issued().is_empty());
        assert_eq!(enrolment.pending().len(), 1, "the request still waits");
    }

    /// A key as a request's id is one: 64 lowercase hexadecimal digits.
    pub(crate) fn key(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    /// The ids `enrolment` lists as pending.
    fn pending_ids(enrolment: &Enrolment) -> Vec<String> {
        enrolment.pending().into_iter().map(|p| p.id).collect()
    }

    /// A rejected key stays refused beyond its window, on every instance `open` opens on one
    /// store, and across a restart: rejected in one window and sent again after the window closed
    /// or ran out, a request is answered as rejected and not listed; an id the queue does not hold
    /// is rejected with the window closed, and its request is refused when it arrives. Another key
    /// still enrols. `clock` is the clock the instances share.
    pub(crate) fn a_rejected_key_stays_refused(open: &dyn Fn() -> Enrolment, clock: &Manual) {
        let a = open();
        a.open(600).expect("window");
        let b = open();
        let (k1, k2, k3, k4) = (key(1), key(2), key(3), key(4));

        assert_eq!(a.submit(request(&k1), requester()), Submitted::Waiting);
        a.reject(&k1).expect("reject");
        a.close().expect("close");
        a.open(600).expect("the next window");
        assert_eq!(
            a.submit(request(&k1), requester()),
            Submitted::Rejected,
            "rejected in the window before, which was closed"
        );
        assert!(pending_ids(&a).is_empty(), "{:?}", pending_ids(&a));
        assert!(a.is_rejected(&k1));
        assert_eq!(
            b.submit(request(&k1), requester()),
            Submitted::Rejected,
            "the other instance refuses it too"
        );
        b.reload().expect("reload");
        assert!(pending_ids(&b).is_empty(), "{:?}", pending_ids(&b));
        assert!(b.is_rejected(&k1));

        assert_eq!(a.submit(request(&k2), requester()), Submitted::Waiting);
        a.reject(&k2).expect("reject");
        clock.0.fetch_add(601_000, Ordering::SeqCst);
        assert_eq!(a.window(), None, "the window ran out");
        a.open(600).expect("the next window");
        assert_eq!(
            a.submit(request(&k2), requester()),
            Submitted::Rejected,
            "rejected in the window before, which ran out"
        );

        a.close().expect("close");
        a.reject(&k3)
            .expect("an id the queue does not hold is rejected with the window closed");
        a.open(600).expect("open");
        assert_eq!(
            a.submit(request(&k3), requester()),
            Submitted::Rejected,
            "rejected before it arrived"
        );
        b.reload().expect("reload");
        assert_eq!(b.submit(request(&k3), requester()), Submitted::Rejected);
        assert!(pending_ids(&a).is_empty(), "{:?}", pending_ids(&a));

        let restarted = open();
        for key in [&k1, &k2, &k3] {
            assert_eq!(
                restarted.submit(request(key), requester()),
                Submitted::Rejected,
                "{key} after a restart"
            );
        }
        assert_eq!(
            restarted.submit(request(&k4), requester()),
            Submitted::Waiting,
            "another key still enrols"
        );
        assert_eq!(pending_ids(&restarted), [k4]);
    }

    /// Verifies: ADR-0052
    #[test]
    fn a_rejected_key_stays_refused_on_one_store_in_memory() {
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let store = Memory::default();
        a_rejected_key_stays_refused(
            &|| Enrolment::open_on(clock.clone(), Box::new(store.clone())).expect("open"),
            &clock,
        );
    }

    /// Verifies: ADR-0052
    #[test]
    fn a_rejected_key_stays_refused_on_the_filesystem() {
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let dir = tempfile::tempdir().expect("tempdir");
        a_rejected_key_stays_refused(
            &|| {
                Enrolment::open_on(
                    clock.clone(),
                    Box::new(crate::fs::FsEnrolmentStore::open(dir.path())),
                )
                .expect("open")
            },
            &clock,
        );
    }

    /// An approval signs no rejected key whose request still waits, as an instance of 0.8.0 may
    /// have queued it: the store refuses it, where this instance does not hold the key rejected
    /// yet, and this instance refuses it, where its store would sign it.
    /// Verifies: ADR-0052
    #[test]
    fn an_approval_signs_no_rejected_key() {
        // A store that signs whatever waits, as one of 0.8.0 does.
        #[derive(Clone, Default)]
        struct Unchecking(Memory);
        impl EnrolmentStore for Unchecking {
            fn load(&self) -> Result<Option<Persisted>, String> {
                self.0.load()
            }
            fn save(&self, persisted: &Persisted) -> Result<(), String> {
                self.0.save(persisted)
            }
            fn approve(
                &self,
                _: &str,
                _: &CertId,
                sign: &mut dyn FnMut() -> Result<Signing, DecisionError>,
                _: &dyn Registrar,
            ) -> Result<Approval, String> {
                Ok(match sign() {
                    Ok((decision, _)) => Approval::Approved(decision),
                    Err(e) => Approval::Refused(e),
                })
            }
        }
        let reject_in_the_store = |store: &dyn EnrolmentStore| {
            let mut persisted = store.load().expect("load").expect("saved");
            persisted.rejected.insert(key(1), 1);
            store.save(&persisted).expect("save");
        };
        let clock = Arc::new(Manual(AtomicU64::new(1_000_000)));
        let recorded = std::cell::Cell::new(0);
        let record = |_: &Signed, _: &[u8], _: &CertId| {
            recorded.set(recorded.get() + 1);
            Ok(())
        };

        let store = Memory::default();
        let a = Enrolment::open_on(clock.clone(), Box::new(store.clone())).expect("open");
        a.open(600).expect("window");
        assert_eq!(a.submit(request(&key(1)), requester()), Submitted::Waiting);
        reject_in_the_store(&store);
        assert_eq!(
            a.approve(&key(1), &Echo, "host", &Recording::new(&record), &|_| false),
            Err(DecisionError::NotFound),
            "the store refuses it"
        );

        let store = Unchecking::default();
        let a = Enrolment::open_on(clock.clone(), Box::new(store.clone())).expect("open");
        a.open(600).expect("window");
        assert_eq!(a.submit(request(&key(1)), requester()), Submitted::Waiting);
        reject_in_the_store(&store);
        let b = Enrolment::open_on(clock.clone(), Box::new(store.clone())).expect("open");
        assert!(b.pending().is_empty(), "not listed");
        assert_eq!(
            b.approve(&key(1), &Echo, "host", &Recording::new(&record), &|_| false),
            Err(DecisionError::NotFound),
            "the instance refuses it"
        );
        assert_eq!(recorded.get(), 0, "nothing signed");
    }

    /// Without a store, as a test holds it, a rejected key stays refused beyond its window, and an
    /// id not queued is rejected before its request arrives.
    /// Verifies: ADR-0052
    #[test]
    fn a_rejected_key_stays_refused_without_a_store() {
        let (enrolment, clock) = enrolment();
        enrolment.open(60).expect("open");
        enrolment.submit(request(&key(1)), requester());
        enrolment.reject(&key(1)).expect("reject");
        clock.0.fetch_add(61_000, Ordering::SeqCst);
        enrolment.reject(&key(2)).expect("reject an id not queued");
        enrolment.open(60).expect("open again");
        for n in [1, 2] {
            assert_eq!(
                enrolment.submit(request(&key(n)), requester()),
                Submitted::Rejected
            );
        }
        assert!(enrolment.pending().is_empty());
    }
}
