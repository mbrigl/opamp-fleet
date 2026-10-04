//! What the Server signed, and what it no longer trusts (ADR-0031).
//!
//! The **register** holds every certificate the client CA signed, with the certificate it renewed
//! as its predecessor, so a revocation reaches every renewal made after the certificate it names.
//! An entry stays while it, or a certificate descended from it, is still valid. The **list** holds
//! revoked certificates, by the CA that issued them and serial, and revoked credentials, by the
//! SHA-256 of their `Authorization` value. Both persist through a [`LedgerStore`]; a change is
//! written before it is answered, and every revocation is announced, so an open session can end
//! the moment what admitted it is revoked.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::fleet::Clock;

/// The revocations one list holds at most (ADR-0031 clause 6).
pub const MAX_REVOCATIONS: usize = 100_000;

/// The certificates the register holds at most (ADR-0031 clause 2).
pub const MAX_ISSUED: usize = 100_000;

/// The certificates the register holds at most in one chain — below one root, the first
/// certificate a chain renews from: enough for a Gateway's downstream Agents, few enough that one
/// member cannot fill the register (ADR-0031 clause 2).
pub const MAX_DESCENDANTS_PER_ROOT: usize = 10_000;

/// The room the register keeps for enrolments: a renewal is refused once fewer are left, so
/// renewals alone cannot shut out a new host (ADR-0031 clause 2).
pub const ENROLMENT_RESERVE: usize = 1_000;

/// A certificate by its issuer and serial: the issuer as the SHA-256 of its DER-encoded name, hex,
/// so no text form of a name can fail to match; the serial as lowercase hex without separators or
/// leading zeros.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CertId {
    pub issuer: String,
    pub serial: String,
}

impl CertId {
    #[must_use]
    pub fn new(issuer_der: &[u8], serial: &str) -> Self {
        CertId {
            issuer: name_hash(issuer_der),
            serial: normalize_serial(serial),
        }
    }
}

/// The SHA-256 of a DER-encoded name, hex.
#[must_use]
pub fn name_hash(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

/// `00:0A:1b` and `a1b` are one serial.
#[must_use]
pub fn normalize_serial(serial: &str) -> String {
    let hex: String = serial
        .chars()
        .filter(|c| !matches!(c, ':' | ' '))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let trimmed = hex.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A CA whose certificates can be revoked: the client CA or the bootstrap CA (ADR-0031 clause 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authority {
    /// `client` or `bootstrap` — what an operator names.
    pub role: String,
    /// The SHA-256 of its DER-encoded subject, hex.
    pub subject: String,
    /// Its subject as text, for the operator to read.
    pub name: String,
}

/// What a signed certificate says about itself — what the signer reports back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub id: CertId,
    /// The issuer's name as text, for the operator to read.
    pub issuer_name: String,
    pub subject: String,
    /// SHA-256 of the certified public key, hex.
    pub key_fingerprint: String,
    /// Milliseconds since the Unix epoch.
    pub not_after_ms: u64,
    /// The host the certificate was issued to (ADR-0026 clause 7) — the stable identity a host
    /// keeps across renewals and re-keys; `None` for a certificate an operator provisioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// The certificate a connection presented: what it renews, and the host it speaks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presented {
    pub id: CertId,
    pub host: Option<String>,
}

/// A host this Server issued certificates to (ADR-0026 clause 7).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// A Gateway carries other hosts' Agents, so its certificate binds none of them.
    #[serde(default)]
    pub gateway: bool,
    /// The `instance_uid`s that have reported with this host's certificate, hex.
    #[serde(default)]
    pub instance_uids: BTreeSet<String>,
}

/// The certificates one host may hold at once (ADR-0026 clause 7): the one in force, its renewal,
/// and one more for a renewal whose answer was lost.
pub const MAX_PER_HOST: usize = 3;

/// The `instance_uid`s one host may speak for: a Client carries an Agent per Supervisor.
pub const MAX_UIDS_PER_HOST: usize = 256;

/// A certificate the client CA signed, PEM, and what it says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    pub pem: String,
    pub facts: Facts,
}

/// One certificate the Server signed (ADR-0031 clause 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issued {
    #[serde(flatten)]
    pub facts: Facts,
    /// The `instance_uid` of the message that carried the CSR, hex.
    pub instance_uid: String,
    /// On a renewal, the certificate it renewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor: Option<CertId>,
    pub issued_ms: u64,
}

/// What a revocation names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Revoked {
    /// A serial under every CA of one role — usually one CA; several while a CA file holds a
    /// rotation.
    Certificate {
        authority: String,
        issuers: Vec<String>,
        serial: String,
    },
    /// The SHA-256 of the exact `Authorization` value, hex — never the value itself.
    Credential { sha256: String },
}

/// One entry of the list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    pub id: String,
    pub revoked: Revoked,
    pub revoked_ms: u64,
}

/// Everything the store holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    pub issued: Vec<Issued>,
    pub revocations: Vec<Revocation>,
    pub hosts: BTreeMap<String, Host>,
}

/// The ledger's port (ADR-0006): where it is kept. A certificate is written on its own, so a
/// renewal costs one small write however large the register is.
pub trait LedgerStore: Send + Sync {
    /// The persisted ledger, empty when there is none yet.
    ///
    /// # Errors
    /// Returns an error when a ledger exists but cannot be read.
    fn load(&self) -> Result<Ledger, String>;

    /// Writes one register entry.
    ///
    /// # Errors
    /// Returns an error when it cannot be written.
    fn put_issued(&self, issued: &Issued) -> Result<(), String>;

    /// Removes one register entry.
    ///
    /// # Errors
    /// Returns an error when it cannot be removed.
    fn remove_issued(&self, id: &CertId) -> Result<(), String>;

    /// Replaces the list.
    ///
    /// # Errors
    /// Returns an error when it cannot be written.
    fn save_revocations(&self, revocations: &[Revocation]) -> Result<(), String>;

    /// Replaces the hosts and what each speaks for.
    ///
    /// # Errors
    /// Returns an error when they cannot be written.
    fn save_hosts(&self, hosts: &BTreeMap<String, Host>) -> Result<(), String>;
}

/// Why a revocation was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum RevokeError {
    /// The request names nothing that could be revoked; the text says why.
    Invalid(String),
    /// The list is at [`MAX_REVOCATIONS`].
    Full,
    /// The store could not write it.
    Store(String),
}

/// The SHA-256 of an `Authorization` value, hex.
#[must_use]
pub fn credential_hash(authorization: &str) -> String {
    hex::encode(Sha256::digest(authorization.as_bytes()))
}

/// The check that tells whether `[auth]` accepts an `Authorization` value.
pub type Accepts = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The register and the list, in memory and in their store.
pub struct Revocations {
    store: Box<dyn LedgerStore>,
    clock: Arc<dyn Clock>,
    /// Whether `[auth]` accepts an `Authorization` value: only such a credential can be revoked.
    accepts: Accepts,
    authorities: Vec<Authority>,
    state: Mutex<State>,
    changes: watch::Sender<u64>,
}

struct State {
    issued: BTreeMap<CertId, Issued>,
    revocations: Vec<Revocation>,
    /// The root of every entry's chain: the first certificate, registered or not, it descends
    /// from; an enrolment is its own root.
    roots: BTreeMap<CertId, CertId>,
    /// How many register entries descend from each root.
    descendants: BTreeMap<CertId, usize>,
    /// The earliest `not_after` in the register: nothing can be pruned before it.
    earliest_expiry_ms: u64,
    hosts: BTreeMap<String, Host>,
    /// Which host each bound `instance_uid` belongs to, hex.
    owners: BTreeMap<String, String>,
}

impl State {
    /// Every revoked certificate, by issuer and serial.
    fn revoked_certificates(&self) -> BTreeSet<CertId> {
        self.revocations
            .iter()
            .flat_map(|entry| match &entry.revoked {
                Revoked::Certificate {
                    issuers, serial, ..
                } => issuers
                    .iter()
                    .map(|issuer| CertId {
                        issuer: issuer.clone(),
                        serial: serial.clone(),
                    })
                    .collect::<Vec<_>>(),
                Revoked::Credential { .. } => Vec::new(),
            })
            .collect()
    }

    /// Whether `id`, or a certificate it renewed, is in `revoked` (clause 4).
    fn descends_from(&self, id: &CertId, revoked: &BTreeSet<CertId>) -> bool {
        let mut current = Some(id);
        // A chain is never longer than the register; the bound also ends a cycle a damaged file
        // could hold.
        for _ in 0..=self.issued.len() {
            let Some(id) = current else {
                return false;
            };
            if revoked.contains(id) {
                return true;
            }
            current = self
                .issued
                .get(id)
                .and_then(|issued| issued.predecessor.as_ref());
        }
        false
    }

    /// The root of a registered entry's chain, by walking its predecessor links.
    fn chain_root(&self, id: &CertId) -> CertId {
        let mut current = id.clone();
        // A chain is never longer than the register; the bound also ends a damaged cycle.
        for _ in 0..=self.issued.len() {
            match self
                .issued
                .get(&current)
                .and_then(|issued| issued.predecessor.clone())
            {
                None => return current,
                Some(predecessor) => current = predecessor,
            }
        }
        current
    }

    /// The root a certificate `id` renewing `predecessor` has.
    fn root_below(&self, predecessor: Option<&CertId>, id: &CertId) -> CertId {
        match predecessor {
            None => id.clone(),
            Some(predecessor) => self
                .roots
                .get(predecessor)
                .cloned()
                .unwrap_or_else(|| predecessor.clone()),
        }
    }

    fn insert(&mut self, issued: Issued) {
        let id = issued.facts.id.clone();
        self.remove(&id);
        let root = self.root_below(issued.predecessor.as_ref(), &id);
        *self.descendants.entry(root.clone()).or_default() += 1;
        self.roots.insert(id.clone(), root);
        self.earliest_expiry_ms = self.earliest_expiry_ms.min(issued.facts.not_after_ms);
        self.issued.insert(id, issued);
    }

    fn remove(&mut self, id: &CertId) {
        if self.issued.remove(id).is_none() {
            return;
        }
        if let Some(root) = self.roots.remove(id) {
            if let Some(count) = self.descendants.get_mut(&root) {
                *count -= 1;
                if *count == 0 {
                    self.descendants.remove(&root);
                }
            }
        }
    }
}

impl Revocations {
    /// Loads the ledger, and drops what expired while the Server was down.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    pub fn open(
        store: Box<dyn LedgerStore>,
        clock: Arc<dyn Clock>,
        accepts: Accepts,
        authorities: Vec<Authority>,
    ) -> Result<Self, String> {
        let ledger = store.load()?;
        let revocations = Revocations {
            accepts,
            authorities,
            state: Mutex::new({
                let mut state = State {
                    issued: BTreeMap::new(),
                    revocations: ledger.revocations,
                    roots: BTreeMap::new(),
                    descendants: BTreeMap::new(),
                    earliest_expiry_ms: u64::MAX,
                    owners: ledger
                        .hosts
                        .iter()
                        .flat_map(|(host, entry)| {
                            entry
                                .instance_uids
                                .iter()
                                .map(move |uid| (uid.clone(), host.clone()))
                        })
                        .collect(),
                    hosts: ledger.hosts,
                };
                for issued in ledger.issued {
                    state.earliest_expiry_ms =
                        state.earliest_expiry_ms.min(issued.facts.not_after_ms);
                    state.issued.insert(issued.facts.id.clone(), issued);
                }
                // Each root by the predecessor links themselves, not by the order entries load in.
                let ids: Vec<CertId> = state.issued.keys().cloned().collect();
                for id in ids {
                    let root = state.chain_root(&id);
                    *state.descendants.entry(root.clone()).or_default() += 1;
                    state.roots.insert(id, root);
                }
                state
            }),
            store,
            clock,
            changes: watch::channel(0).0,
        };
        {
            let mut state = revocations.state.lock().expect("revocation lock");
            revocations.prune(&mut state)?;
        }
        Ok(revocations)
    }

    /// The CA an issuer hash belongs to, when it is one of this Server's.
    #[must_use]
    pub fn authority_of(&self, issuer: &str) -> Option<&Authority> {
        self.authorities.iter().find(|a| a.subject == issuer)
    }

    /// The register entry of a certificate, if the client CA signed it.
    #[must_use]
    pub fn issued_entry(&self, id: &CertId) -> Option<Issued> {
        self.state
            .lock()
            .expect("revocation lock")
            .issued
            .get(id)
            .cloned()
    }

    /// Records a certificate the client CA signed, before it is handed out (clause 2).
    ///
    /// # Errors
    /// Returns an error when the register is full — for a renewal, [`ENROLMENT_RESERVE`] short of
    /// full — when the chain already holds [`MAX_DESCENDANTS_PER_ROOT`] certificates, or when the
    /// store cannot write it; the certificate must then not be offered.
    pub fn record(
        &self,
        facts: Facts,
        instance_uid: &[u8],
        predecessor: Option<CertId>,
    ) -> Result<(), String> {
        let mut state = self.state.lock().expect("revocation lock");
        self.prune(&mut state)?;
        let limit = if predecessor.is_some() {
            MAX_ISSUED - ENROLMENT_RESERVE
        } else {
            MAX_ISSUED
        };
        if state.issued.len() >= limit {
            return Err(format!(
                "the register holds {} certificates already",
                state.issued.len()
            ));
        }
        let root = state.root_below(predecessor.as_ref(), &facts.id);
        if state.descendants.get(&root).copied().unwrap_or(0) >= MAX_DESCENDANTS_PER_ROOT {
            return Err("this certificate's chain has been renewed too often".to_string());
        }
        if let Some(host) = &facts.host {
            let now = self.clock.now_ms();
            let live = state
                .issued
                .values()
                .filter(|issued| {
                    issued.facts.host.as_ref() == Some(host) && issued.facts.not_after_ms > now
                })
                .count();
            if live >= MAX_PER_HOST {
                return Err(format!(
                    "the host already holds {MAX_PER_HOST} valid certificates — one is renewed \
                     when it has run two thirds of its life"
                ));
            }
        }
        let issued = Issued {
            facts,
            instance_uid: hex::encode(instance_uid),
            predecessor,
            issued_ms: self.clock.now_ms(),
        };
        self.store.put_issued(&issued)?;
        state.insert(issued);
        Ok(())
    }

    /// Whether a connection presenting a certificate of `host` may report for `instance_uid`
    /// (ADR-0026 clause 7). An `instance_uid` first heard from a host is bound to it; one bound to
    /// another host is refused. A Gateway's certificate binds nothing — it carries other hosts'
    /// Agents.
    ///
    /// # Errors
    /// Returns the refusal, or an error when a new binding cannot be written.
    pub fn check_report(&self, host: &str, instance_uid: &[u8]) -> Result<(), String> {
        let uid = hex::encode(instance_uid);
        let mut state = self.state.lock().expect("revocation lock");
        if state.hosts.get(host).is_some_and(|entry| entry.gateway) {
            return Ok(());
        }
        match state.owners.get(&uid) {
            Some(owner) if owner == host => return Ok(()),
            Some(owner) => {
                let owner_is_gateway = state.hosts.get(owner).is_some_and(|entry| entry.gateway);
                if !owner_is_gateway {
                    return Err("this certificate belongs to another host than that Agent's".into());
                }
            }
            None => {}
        }
        let entry = state.hosts.entry(host.to_string()).or_default();
        if entry.instance_uids.len() >= MAX_UIDS_PER_HOST {
            return Err(format!(
                "this host already speaks for {MAX_UIDS_PER_HOST} Agents"
            ));
        }
        entry.instance_uids.insert(uid.clone());
        state.owners.insert(uid, host.to_string());
        self.store.save_hosts(&state.hosts)
    }

    /// Moves a binding to the `instance_uid` the Server re-keyed an Agent to — a re-key never
    /// orphans a host's certificate (ADR-0026 clause 7).
    ///
    /// # Errors
    /// Returns an error when the binding cannot be written.
    pub fn rebind(&self, old: &[u8], new: &[u8]) -> Result<(), String> {
        let (old, new) = (hex::encode(old), hex::encode(new));
        let mut state = self.state.lock().expect("revocation lock");
        let Some(host) = state.owners.remove(&old) else {
            return Ok(());
        };
        if let Some(entry) = state.hosts.get_mut(&host) {
            entry.instance_uids.remove(&old);
            entry.instance_uids.insert(new.clone());
        }
        state.owners.insert(new, host);
        self.store.save_hosts(&state.hosts)
    }

    /// Marks `host` as a Gateway, or not; answers whether this Server knows the host.
    ///
    /// # Errors
    /// Returns an error when the change cannot be written.
    pub fn set_gateway(&self, host: &str, gateway: bool) -> Result<bool, String> {
        let mut state = self.state.lock().expect("revocation lock");
        let known = state.hosts.contains_key(host)
            || state
                .issued
                .values()
                .any(|issued| issued.facts.host.as_deref() == Some(host));
        if !known {
            return Ok(false);
        }
        state.hosts.entry(host.to_string()).or_default().gateway = gateway;
        self.store.save_hosts(&state.hosts)?;
        Ok(true)
    }

    /// Every host this Server knows, and how many valid certificates each holds.
    #[must_use]
    pub fn hosts(&self) -> Vec<(String, Host, usize)> {
        let state = self.state.lock().expect("revocation lock");
        let now = self.clock.now_ms();
        let mut names: BTreeSet<String> = state.hosts.keys().cloned().collect();
        names.extend(
            state
                .issued
                .values()
                .filter_map(|issued| issued.facts.host.clone()),
        );
        names
            .into_iter()
            .map(|name| {
                let live = state
                    .issued
                    .values()
                    .filter(|issued| {
                        issued.facts.host.as_ref() == Some(&name) && issued.facts.not_after_ms > now
                    })
                    .count();
                let host = state.hosts.get(&name).cloned().unwrap_or_default();
                (name, host, live)
            })
            .collect()
    }

    /// Every certificate in the register, oldest first.
    #[must_use]
    pub fn issued(&self) -> Vec<Issued> {
        let state = self.state.lock().expect("revocation lock");
        let mut issued: Vec<Issued> = state.issued.values().cloned().collect();
        issued.sort_by_key(|issued| issued.issued_ms);
        issued
    }

    /// Revokes the certificate with `serial` from the CA of `authority` — `client` or
    /// `bootstrap` (clause 3).
    ///
    /// # Errors
    /// Refuses an authority this Server does not have, a serial that is not hex, a full list, and
    /// a failed write.
    pub fn revoke_certificate(
        &self,
        authority: &str,
        serial: &str,
    ) -> Result<Revocation, RevokeError> {
        let issuers: Vec<String> = self
            .authorities
            .iter()
            .filter(|a| a.role == authority)
            .map(|a| a.subject.clone())
            .collect();
        if issuers.is_empty() {
            let roles: BTreeSet<&str> = self.authorities.iter().map(|a| a.role.as_str()).collect();
            return Err(RevokeError::Invalid(format!(
                "this Server has no {authority:?} CA; name one of {roles:?}"
            )));
        }
        let serial_text = normalize_serial(serial);
        if serial.trim().is_empty() || !serial_text.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(RevokeError::Invalid(format!(
                "the serial {serial:?} is not hexadecimal"
            )));
        }
        self.add(Revoked::Certificate {
            authority: authority.to_string(),
            issuers,
            serial: serial_text,
        })
    }

    /// Revokes a credential `[auth]` accepts (clause 5).
    ///
    /// # Errors
    /// Refuses a credential `[auth]` does not accept, a full list, and a failed write.
    pub fn revoke_credential(&self, authorization: &str) -> Result<Revocation, RevokeError> {
        if !(self.accepts)(authorization) {
            return Err(RevokeError::Invalid(
                "no credential of [auth] has this value".into(),
            ));
        }
        self.add(Revoked::Credential {
            sha256: credential_hash(authorization),
        })
    }

    fn add(&self, revoked: Revoked) -> Result<Revocation, RevokeError> {
        let mut state = self.state.lock().expect("revocation lock");
        if let Some(existing) = state.revocations.iter().find(|e| e.revoked == revoked) {
            return Ok(existing.clone());
        }
        if state.revocations.len() >= MAX_REVOCATIONS {
            return Err(RevokeError::Full);
        }
        let entry = Revocation {
            id: entry_id(&revoked),
            revoked,
            revoked_ms: self.clock.now_ms(),
        };
        state.revocations.push(entry.clone());
        if let Err(e) = self.store.save_revocations(&state.revocations) {
            state.revocations.pop();
            return Err(RevokeError::Store(e));
        }
        drop(state);
        self.changes.send_modify(|rev| *rev += 1);
        Ok(entry)
    }

    /// The list, oldest first (clause 7).
    #[must_use]
    pub fn list(&self) -> Vec<Revocation> {
        self.state
            .lock()
            .expect("revocation lock")
            .revocations
            .clone()
    }

    /// Lifts one revocation; `Ok(false)` for an unknown id (clause 7). It takes effect at the next
    /// connection and reopens nothing, so it is not announced.
    ///
    /// # Errors
    /// Returns an error when the store cannot write it.
    pub fn lift(&self, id: &str) -> Result<bool, String> {
        let mut state = self.state.lock().expect("revocation lock");
        let Some(index) = state.revocations.iter().position(|e| e.id == id) else {
            return Ok(false);
        };
        let entry = state.revocations.remove(index);
        if let Err(e) = self.store.save_revocations(&state.revocations) {
            state.revocations.insert(index, entry);
            return Err(e);
        }
        Ok(true)
    }

    /// Whether `certificate`, or a certificate it renewed, is revoked (clause 4).
    #[must_use]
    pub fn is_certificate_revoked(&self, certificate: &CertId) -> bool {
        let state = self.state.lock().expect("revocation lock");
        let revoked = state.revoked_certificates();
        !revoked.is_empty() && state.descends_from(certificate, &revoked)
    }

    /// Every certificate of the CAs of `role` that is revoked, itself or through a certificate it
    /// renewed, with its chain already resolved: what a Gateway refuses
    /// (ADR-0031 clause 12). A certificate the register never held is on it as revoked.
    #[must_use]
    pub fn revoked_certificates(&self, role: &str) -> BTreeSet<CertId> {
        let issuers: BTreeSet<&str> = self
            .authorities
            .iter()
            .filter(|authority| authority.role == role)
            .map(|authority| authority.subject.as_str())
            .collect();
        let state = self.state.lock().expect("revocation lock");
        let revoked = state.revoked_certificates();
        if revoked.is_empty() {
            return revoked;
        }
        let mut named: BTreeSet<CertId> = revoked
            .iter()
            .filter(|id| issuers.contains(id.issuer.as_str()))
            .cloned()
            .collect();
        named.extend(
            state
                .issued
                .keys()
                .filter(|id| issuers.contains(id.issuer.as_str()))
                .filter(|id| state.descends_from(id, &revoked))
                .cloned(),
        );
        named
    }

    /// Whether `host` is marked as a Gateway.
    #[must_use]
    pub fn is_gateway(&self, host: &str) -> bool {
        self.state
            .lock()
            .expect("revocation lock")
            .hosts
            .get(host)
            .is_some_and(|host| host.gateway)
    }

    /// Whether the `Authorization` value with this hash is revoked.
    #[must_use]
    pub fn is_credential_revoked(&self, sha256: &str) -> bool {
        self.state
            .lock()
            .expect("revocation lock")
            .revocations
            .iter()
            .any(|entry| matches!(&entry.revoked, Revoked::Credential { sha256: s } if s == sha256))
    }

    /// Announces every new revocation; an open session checks itself against the list on each.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Drops the register entries that have expired and from which no valid certificate descends,
    /// and the revocations of certificates those entries held (clause 6). A revoked ancestor of a
    /// valid renewal stays, so the renewal stays revoked.
    fn prune(&self, state: &mut State) -> Result<(), String> {
        let now = self.clock.now_ms();
        if now < state.earliest_expiry_ms {
            return Ok(());
        }
        let mut needed: BTreeSet<CertId> = BTreeSet::new();
        for issued in state.issued.values() {
            if issued.facts.not_after_ms <= now {
                continue;
            }
            let mut current = Some(&issued.facts.id);
            for _ in 0..=state.issued.len() {
                let Some(id) = current else { break };
                if !needed.insert(id.clone()) {
                    break;
                }
                current = state
                    .issued
                    .get(id)
                    .and_then(|entry| entry.predecessor.as_ref());
            }
        }
        let gone: Vec<CertId> = state
            .issued
            .keys()
            .filter(|id| !needed.contains(*id))
            .cloned()
            .collect();
        // Kept entries that have expired are revoked ancestors' chains: the next prune is due when
        // the first entry still valid expires.
        state.earliest_expiry_ms = state
            .issued
            .values()
            .map(|issued| issued.facts.not_after_ms)
            .filter(|not_after| *not_after > now)
            .min()
            .unwrap_or(u64::MAX);
        if gone.is_empty() {
            return Ok(());
        }
        for id in &gone {
            self.store.remove_issued(id)?;
            state.remove(id);
        }
        let before = state.revocations.len();
        state.revocations.retain(|entry| match &entry.revoked {
            Revoked::Certificate {
                issuers, serial, ..
            } => !gone
                .iter()
                .any(|id| &id.serial == serial && issuers.contains(&id.issuer)),
            Revoked::Credential { .. } => true,
        });
        if state.revocations.len() != before {
            self.store.save_revocations(&state.revocations)?;
        }
        Ok(())
    }
}

/// A stable id: the same revocation twice is one entry.
fn entry_id(revoked: &Revoked) -> String {
    let text = match revoked {
        Revoked::Certificate {
            authority, serial, ..
        } => format!("certificate\n{authority}\n{serial}"),
        Revoked::Credential { sha256 } => format!("credential\n{sha256}"),
    };
    hex::encode(&Sha256::digest(text.as_bytes())[..8])
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

    #[derive(Clone, Default)]
    struct Memory(Arc<Mutex<Ledger>>);

    impl LedgerStore for Memory {
        fn load(&self) -> Result<Ledger, String> {
            Ok(self.0.lock().expect("lock").clone())
        }
        fn put_issued(&self, issued: &Issued) -> Result<(), String> {
            let mut ledger = self.0.lock().expect("lock");
            ledger.issued.retain(|i| i.facts.id != issued.facts.id);
            ledger.issued.push(issued.clone());
            Ok(())
        }
        fn remove_issued(&self, id: &CertId) -> Result<(), String> {
            self.0
                .lock()
                .expect("lock")
                .issued
                .retain(|i| &i.facts.id != id);
            Ok(())
        }
        fn save_revocations(&self, revocations: &[Revocation]) -> Result<(), String> {
            self.0.lock().expect("lock").revocations = revocations.to_vec();
            Ok(())
        }
        fn save_hosts(&self, hosts: &BTreeMap<String, Host>) -> Result<(), String> {
            self.0.lock().expect("lock").hosts = hosts.clone();
            Ok(())
        }
    }

    const NOW: u64 = 1_000_000_000;
    const CA: &[u8] = b"fleet client CA";
    const TOKEN: &str = "Bearer fleet-token";

    fn open(store: &Memory) -> (Revocations, Arc<Manual>) {
        let clock = Arc::new(Manual(AtomicU64::new(NOW)));
        let revocations = Revocations::open(
            Box::new(store.clone()),
            clock.clone(),
            Arc::new(|authorization: &str| authorization == TOKEN),
            vec![Authority {
                role: "client".into(),
                subject: name_hash(CA),
                name: "CN=fleet client CA".into(),
            }],
        )
        .expect("open");
        (revocations, clock)
    }

    fn id(serial: &str) -> CertId {
        CertId::new(CA, serial)
    }

    fn facts(serial: &str, not_after_ms: u64) -> Facts {
        Facts {
            id: id(serial),
            issuer_name: "CN=fleet client CA".into(),
            subject: "CN=edge-01".into(),
            key_fingerprint: format!("key-{serial}"),
            not_after_ms,
            host: None,
        }
    }

    fn on_host(serial: &str, host: &str) -> Facts {
        Facts {
            host: Some(host.to_string()),
            ..facts(serial, NOW + 1_000_000)
        }
    }

    /// A host holds a bounded number of valid certificates, an Agent is spoken for by the host
    /// that first reported it alone, a re-key keeps its host, and a Gateway speaks for any Agent.
    /// Verifies: ADR-0026
    #[test]
    fn a_host_is_bounded_and_speaks_only_for_its_own_agents() {
        let store = Memory::default();
        let (revocations, _) = open(&store);
        for serial in ["a1", "a2", "a3"] {
            revocations
                .record(on_host(serial, "h1"), &[1; 16], None)
                .expect("within the bound");
        }
        assert!(
            revocations
                .record(on_host("a4", "h1"), &[1; 16], None)
                .is_err(),
            "a fourth valid certificate for one host"
        );
        revocations
            .record(on_host("b1", "h2"), &[2; 16], None)
            .expect("another host");

        revocations
            .check_report("h1", &[1; 16])
            .expect("first report binds");
        revocations
            .check_report("h1", &[1; 16])
            .expect("its own Agent");
        assert!(
            revocations.check_report("h2", &[1; 16]).is_err(),
            "another host spoke for h1's Agent"
        );
        revocations.rebind(&[1; 16], &[3; 16]).expect("rebind");
        revocations
            .check_report("h1", &[3; 16])
            .expect("the re-keyed Agent");
        assert!(revocations.check_report("h2", &[3; 16]).is_err());
        revocations
            .check_report("h2", &[1; 16])
            .expect("the old identity is free");

        assert!(!revocations.set_gateway("unknown", true).expect("set"));
        assert!(revocations.set_gateway("h2", true).expect("set"));
        revocations
            .check_report("h2", &[3; 16])
            .expect("a Gateway speaks for any Agent");

        // The bindings survive a restart.
        let (reopened, _) = open(&store);
        assert!(reopened.check_report("h3", &[3; 16]).is_err());
        reopened
            .check_report("h2", &[9; 16])
            .expect("still a Gateway");
    }

    /// Verifies: ADR-0031
    #[test]
    fn a_revocation_follows_every_renewal() {
        let (revocations, _) = open(&Memory::default());
        let far = NOW + 1_000_000;
        revocations
            .record(facts("a1", far), &[1; 16], None)
            .expect("record");
        revocations
            .record(facts("b2", far), &[1; 16], Some(id("a1")))
            .expect("record");
        revocations
            .record(facts("c3", far), &[1; 16], Some(id("b2")))
            .expect("record");
        revocations
            .record(facts("d4", far), &[2; 16], None)
            .expect("record");

        revocations
            .revoke_certificate("client", "00:A1")
            .expect("revoke");
        assert!(revocations.is_certificate_revoked(&id("a1")));
        assert!(
            revocations.is_certificate_revoked(&id("c3")),
            "a renewal escaped the revocation of its ancestor"
        );
        assert!(!revocations.is_certificate_revoked(&id("d4")));
        assert!(matches!(
            revocations.revoke_certificate("bootstrap", "01"),
            Err(RevokeError::Invalid(_))
        ));
    }

    /// The revoked ancestor of a valid renewal outlives its own expiry, and so does every link
    /// between them.
    /// Verifies: ADR-0031
    #[test]
    fn a_renewal_stays_revoked_after_its_revoked_ancestor_expires() {
        let (revocations, clock) = open(&Memory::default());
        revocations
            .record(facts("a1", NOW + 1_000), &[1; 16], None)
            .expect("record");
        revocations
            .record(facts("b2", NOW + 2_000), &[1; 16], Some(id("a1")))
            .expect("record");
        revocations
            .record(facts("c3", NOW + 9_000), &[1; 16], Some(id("b2")))
            .expect("record");
        revocations
            .revoke_certificate("client", "a1")
            .expect("revoke");
        clock.0.fetch_add(5_000, Ordering::SeqCst);
        revocations
            .record(facts("e5", NOW + 9_000), &[3; 16], None)
            .expect("a record prunes");
        assert!(
            revocations.is_certificate_revoked(&id("c3")),
            "c3 escaped once a1 and b2 expired"
        );
        assert_eq!(revocations.list().len(), 1);
    }

    /// Verifies: ADR-0031
    #[test]
    fn a_credential_is_kept_by_its_hash_alone() {
        let store = Memory::default();
        let (revocations, _) = open(&store);
        assert!(matches!(
            revocations.revoke_credential("Bearer typo"),
            Err(RevokeError::Invalid(_))
        ));
        let entry = revocations.revoke_credential(TOKEN).expect("revoke");
        assert!(revocations.is_credential_revoked(&credential_hash(TOKEN)));
        let persisted = format!("{:?}", store.load().expect("load"));
        assert!(!persisted.contains("fleet-token"), "{persisted}");
        assert_eq!(
            revocations.revoke_credential(TOKEN).expect("again").id,
            entry.id,
            "one value, one entry"
        );
    }

    /// Verifies: ADR-0031
    #[test]
    fn the_list_survives_a_restart_and_can_be_lifted() {
        let store = Memory::default();
        {
            let (revocations, _) = open(&store);
            revocations
                .revoke_certificate("client", "ff")
                .expect("revoke");
        }
        let (revocations, _) = open(&store);
        let listed = revocations.list();
        assert_eq!(listed.len(), 1);
        assert!(revocations.is_certificate_revoked(&id("ff")));
        assert!(revocations.lift(&listed[0].id).expect("lift"));
        assert!(
            !revocations.lift(&listed[0].id).expect("lift"),
            "already gone"
        );
        let (reopened, _) = open(&store);
        assert!(reopened.list().is_empty());
    }

    /// Verifies: ADR-0031
    #[test]
    fn the_list_and_the_register_are_bounded() {
        let store = Memory::default();
        store
            .save_revocations(
                &(0..MAX_REVOCATIONS)
                    .map(|n| Revocation {
                        id: n.to_string(),
                        revoked: Revoked::Credential {
                            sha256: n.to_string(),
                        },
                        revoked_ms: NOW,
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("seed");
        let (revocations, _) = open(&store);
        assert_eq!(
            revocations.revoke_certificate("client", "abcdef0123"),
            Err(RevokeError::Full)
        );

        let (revocations, _) = open(&Memory::default());
        let far = NOW + 1_000_000;
        revocations
            .record(facts("1", far), &[1; 16], None)
            .expect("record");
        // Renewing the renewals does not escape the bound: it is the chain's, not each link's.
        let mut previous = id("1");
        for n in 1..MAX_DESCENDANTS_PER_ROOT {
            let serial = format!("{:x}", n + 16);
            revocations
                .record(facts(&serial, far), &[1; 16], Some(previous.clone()))
                .expect("record");
            if n % 2 == 0 {
                previous = id(&serial);
            }
        }
        assert!(
            revocations
                .record(facts("ffffff", far), &[1; 16], Some(previous))
                .is_err(),
            "one chain filled the register"
        );
    }

    /// Verifies: ADR-0031
    #[test]
    fn an_expired_register_entry_is_dropped_with_its_revocation() {
        let (revocations, clock) = open(&Memory::default());
        revocations
            .record(facts("a1", NOW + 1_000), &[1; 16], None)
            .expect("record");
        revocations
            .revoke_certificate("client", "a1")
            .expect("revoke");
        revocations
            .revoke_certificate("client", "bb")
            .expect("one the register never held");
        clock.0.fetch_add(1_000, Ordering::SeqCst);
        revocations
            .record(facts("cc", NOW + 9_000), &[2; 16], None)
            .expect("a record prunes");
        assert_eq!(revocations.issued().len(), 1);
        let left = revocations.list();
        assert_eq!(left.len(), 1);
        assert!(matches!(&left[0].revoked, Revoked::Certificate { serial, .. } if serial == "bb"));
    }

    #[test]
    fn a_serial_that_is_not_hex_is_refused() {
        let (revocations, _) = open(&Memory::default());
        assert!(matches!(
            revocations.revoke_certificate("client", "xyz"),
            Err(RevokeError::Invalid(_))
        ));
    }

    /// A reload finds every chain's root from its links, whatever order the entries load in.
    /// Verifies: ADR-0031
    #[test]
    fn a_reload_keeps_each_chain_under_its_root() {
        let store = Memory::default();
        let far = NOW + 1_000_000;
        {
            let (revocations, clock) = open(&store);
            revocations
                .record(facts("a1", far), &[1; 16], None)
                .expect("record");
            // A clock that stepped back: the renewal sorts before its predecessor.
            clock.0.fetch_sub(10, Ordering::SeqCst);
            revocations
                .record(facts("b2", far), &[1; 16], Some(id("a1")))
                .expect("record");
        }
        let (revocations, _) = open(&store);
        let state = revocations.state.lock().expect("lock");
        assert_eq!(state.roots.get(&id("b2")), Some(&id("a1")));
        assert_eq!(state.descendants.get(&id("a1")), Some(&2));
    }
}
