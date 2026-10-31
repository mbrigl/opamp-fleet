//! The Host register (ADR-0022 clause 11, ADR-0024 clauses 15 and 16): every host a client
//! certificate names, the `instance_uid`s it speaks for, whether it is marked as a Gateway, and
//! until when it stands. It is kept with the register and the list in one state and written
//! through the same [`LedgerStore`](super::LedgerStore), since an issuance and a prune change
//! both.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::MutexGuard;

use serde::{Deserialize, Serialize};

use super::{Revocations, State};
use crate::subject::SubjectValues;

/// A Host: what a client certificate's host id names (ADR-0022 clause 11, ADR-0024 clause 15).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// A Gateway carries other hosts' Agents, so its certificate binds none of them.
    #[serde(default)]
    pub gateway: bool,
    /// The `instance_uid`s that have reported with this host's certificate, hex.
    #[serde(default)]
    pub instance_uids: BTreeSet<String>,
    /// The subject values of the client certificate it was issued, or — for a Host made at its
    /// first report — of the certificate it presented.
    #[serde(default, skip_serializing_if = "SubjectValues::is_empty")]
    pub subject: SubjectValues,
    /// When its enrolment was approved, in milliseconds since the Unix epoch; `None` for a Host a
    /// provisioned certificate made at its first report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_ms: Option<u64>,
    /// The latest `not_after` of a certificate naming it that the Server issued or that it
    /// presented, in milliseconds since the Unix epoch (ADR-0024 clause 15). Every issuance and
    /// every admission moves it forward, never back. Once it has passed, a Host not marked as a
    /// Gateway ends with its bindings. `None` for a Host written before it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until_ms: Option<u64>,
}

impl Host {
    /// Moves its valid until forward to `not_after_ms`, never back; answers whether it moved.
    pub(super) fn extend(&mut self, not_after_ms: u64) -> bool {
        if self.valid_until_ms >= Some(not_after_ms) {
            return false;
        }
        self.valid_until_ms = Some(not_after_ms);
        true
    }
}

/// The Agents a host's certificate speaks for (ADR-0024 clause 16).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeaksFor {
    /// A host marked as a Gateway: it carries other hosts' Agents, and the Server keeps no record
    /// of which.
    Any,
    /// The `instance_uid`s bound to the host, hex.
    Agents(BTreeSet<String>),
}

/// A version of the hosts, numbered so no older one is written over a newer.
pub(super) type HostsVersion = (u64, BTreeMap<String, Host>);

/// The `instance_uid`s one host may speak for: a Client carries an Agent per Supervisor.
pub const MAX_UIDS_PER_HOST: usize = 256;

impl Revocations {
    /// Whether a connection presenting a certificate of `host` may report for `instance_uid`
    /// (ADR-0022 clause 11), binding nothing: [`check_report`](Self::check_report) binds it once
    /// the report is admitted.
    ///
    /// # Errors
    /// Returns the refusal.
    pub fn may_report(&self, host: &str, instance_uid: &[u8]) -> Result<(), String> {
        let state = self.state.lock().expect("revocation lock");
        Self::admits(&state, host, &hex::encode(instance_uid)).map(|_| ())
    }

    /// Whether a connection presenting a certificate of `host` may report for `instance_uid`
    /// (ADR-0022 clause 11). An `instance_uid` first heard from a host is bound to it; one bound to
    /// another host is refused. A Gateway's certificate binds nothing — it carries other hosts'
    /// Agents. A certificate an operator provisioned with a host id makes its Host here, at its
    /// first report, with `subject` — the values of the certificate it presented (ADR-0024
    /// clause 15). Every admitted report moves the Host's valid until forward to `not_after_ms`,
    /// the end of the certificate it presented (ADR-0024 clause 15).
    ///
    /// # Errors
    /// Returns the refusal, or an error when a new binding cannot be written.
    pub fn check_report(
        &self,
        host: &str,
        subject: &SubjectValues,
        not_after_ms: u64,
        instance_uid: &[u8],
    ) -> Result<(), String> {
        let uid = hex::encode(instance_uid);
        let mut state = self.state.lock().expect("revocation lock");
        let binds = Self::admits(&state, host, &uid)?;
        if binds {
            state
                .hosts
                .entry(host.to_string())
                .or_insert_with(|| Host {
                    subject: subject.clone(),
                    ..Host::default()
                })
                .instance_uids
                .insert(uid.clone());
            state.owners.insert(uid, host.to_string());
        }
        let extended = state
            .hosts
            .get_mut(host)
            .is_some_and(|entry| entry.extend(not_after_ms));
        if extended {
            state.earliest_expiry_ms = state.earliest_expiry_ms.min(not_after_ms);
        }
        if binds || extended {
            self.save_hosts(state)
        } else {
            Ok(())
        }
    }

    /// Releases `instance_uid` from the Host it is bound to, when an operator forgets its Agent
    /// (ADR-0024 clause 10): the next host that reports it binds it.
    ///
    /// # Errors
    /// Returns an error when the release cannot be written; it holds in memory all the same, and
    /// the next write of the hosts carries it.
    pub fn release(&self, instance_uid: &[u8]) -> Result<(), String> {
        let uid = hex::encode(instance_uid);
        let mut state = self.state.lock().expect("revocation lock");
        let Some(host) = state.owners.remove(&uid) else {
            return Ok(());
        };
        if let Some(entry) = state.hosts.get_mut(&host) {
            entry.instance_uids.remove(&uid);
        }
        self.save_hosts(state)
    }

    /// Whether `host` may report for `uid`, and whether that would bind it.
    fn admits(state: &State, host: &str, uid: &str) -> Result<bool, String> {
        if state.hosts.get(host).is_some_and(|entry| entry.gateway) {
            return Ok(false);
        }
        match state.owners.get(uid) {
            Some(owner) if owner == host => return Ok(false),
            Some(owner) => {
                let owner_is_gateway = state.hosts.get(owner).is_some_and(|entry| entry.gateway);
                if !owner_is_gateway {
                    return Err("this certificate belongs to another host than that Agent's".into());
                }
            }
            None => {}
        }
        if state
            .hosts
            .get(host)
            .is_some_and(|entry| entry.instance_uids.len() >= MAX_UIDS_PER_HOST)
        {
            return Err(format!(
                "this host already speaks for {MAX_UIDS_PER_HOST} Agents"
            ));
        }
        Ok(true)
    }

    /// The Agents a certificate naming `host` speaks for (ADR-0024 clause 16): the `instance_uid`s
    /// bound to it, or any Agent for a host marked as a Gateway. A host the register does not know
    /// speaks for none.
    #[must_use]
    pub fn speaks_for(&self, host: &str) -> SpeaksFor {
        let state = self.state.lock().expect("revocation lock");
        match state.hosts.get(host) {
            Some(entry) if entry.gateway => SpeaksFor::Any,
            Some(entry) => SpeaksFor::Agents(entry.instance_uids.clone()),
            None => SpeaksFor::Agents(BTreeSet::new()),
        }
    }

    /// Moves a binding to the `instance_uid` the Server re-keyed an Agent to — a re-key never
    /// orphans a host's certificate (ADR-0022 clause 11).
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
        self.save_hosts(state)
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
        let entry = state.hosts.entry(host.to_string()).or_default();
        entry.gateway = gateway;
        // A Host whose mark is taken away ends with its valid until again (ADR-0024 clause 15).
        let until = entry.valid_until_ms.unwrap_or(0);
        if !gateway {
            state.earliest_expiry_ms = state.earliest_expiry_ms.min(until);
        }
        self.save_hosts(state)?;
        Ok(true)
    }

    /// Writes the hosts as `state` holds them, once its lock is released, so no check of the list
    /// waits for the disk. It returns once they, or a later version of them, are written.
    fn save_hosts(&self, mut state: MutexGuard<'_, State>) -> Result<(), String> {
        let (version, hosts) = Self::hosts_changed(&mut state);
        drop(state);
        self.write_hosts(version, &hosts)
    }

    /// Writes the hosts a prune forgot or an approval wrote. The register does not depend on
    /// them, so a failure is logged and left to the next write of the hosts.
    pub(super) fn write_forgotten(&self, forgotten: Option<HostsVersion>) {
        if let Some((version, hosts)) = forgotten {
            if let Err(e) = self.write_hosts(version, &hosts) {
                tracing::warn!(error = %e, "cannot write the hosts");
            }
        }
    }

    pub(super) fn hosts_changed(state: &mut State) -> HostsVersion {
        state.hosts_version += 1;
        (state.hosts_version, state.hosts.clone())
    }

    fn write_hosts(&self, version: u64, hosts: &BTreeMap<String, Host>) -> Result<(), String> {
        let mut written = self.hosts_written.lock().expect("hosts lock");
        if *written >= version {
            return Ok(());
        }
        self.store.save_hosts(hosts)?;
        *written = version;
        Ok(())
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

    /// Whether `host` is marked as a Gateway, and its subject values — what places an Agent that
    /// arrived over it (ADR-0024 clauses 18 and 19). A host the register does not know is neither a
    /// Gateway nor has values.
    #[must_use]
    pub fn placement(&self, host: &str) -> (bool, SubjectValues) {
        self.state
            .lock()
            .expect("revocation lock")
            .hosts
            .get(host)
            .map(|host| (host.gateway, host.subject.clone()))
            .unwrap_or_default()
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
}
