//! The Agent-record storage port (ADR-0024).
//!
//! The fleet is loaded whole at startup and held in memory; at runtime the store receives writes
//! and deletions for single Agents — `load`, `get`, `put`, `set_labels`, `set_restart_pending`,
//! `remove`, `rekey` — and one operation for a rollout, which writes the revisions it pins and the
//! assignments that pin them together (ADR-0029 clause 2). A report, a label change, a restart
//! request and a rollout each write their own part of a record, so on a store other Servers share
//! the write of one never undoes another's
//! (ADR-draft-the-fleet-record-of-agents-and-hosts clause 2). The integrity questions a deletion
//! asks — is this pinned, which revisions are referenced — are queries of the port too, so an
//! adapter over a database answers them from its rows rather than the caller from a scan. The
//! port speaks the *typed* record: what bytes or rows a backend turns it into is the adapter's
//! own business.
//!
//! The write discipline (no write on a heartbeat, flush on graceful shutdown) deliberately lives
//! in the caller ([`crate::fleet::AppState`]), so no backend can get it wrong.
//!
//! Stored records are secret-bearing whatever the backend: a reported effective configuration is
//! whatever the Managed Process runs, credentials included. The filesystem adapter
//! ([`FsAgentStore`](crate::fs::FsAgentStore)) answers with an owner-only directory; any other
//! adapter must answer with its own access control.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use opamp::proto::{
    AgentDescription, AvailableComponents, ComponentHealth, ConnectionSettingsStatus,
    PackageStatuses, RemoteConfigStatus,
};
use opamp::uid::InstanceUid;
use prost::Message;
use sha2::{Digest, Sha256};

use crate::configs::Pinned;
use crate::fleet::{Assignment, Transport};
use crate::packages::PackageId;

/// Everything about one Agent that survives a restart: what it reported and what an operator
/// queued for it — never what a live connection knows (`connected`, the owning connection).
#[derive(Clone, Debug, PartialEq)]
pub struct PersistedAgent {
    pub sequence_num: u64,
    pub capabilities: u64,
    pub description: Option<AgentDescription>,
    pub health: Option<ComponentHealth>,
    pub effective_config: Option<String>,
    pub remote_config_status: Option<RemoteConfigStatus>,
    pub connection_settings_status: Option<ConnectionSettingsStatus>,
    pub package_statuses: Option<PackageStatuses>,
    pub available_components: Option<AvailableComponents>,
    /// The transport the last report arrived on — informational, never a routing key (ADR-0015).
    pub transport: Transport,
    pub last_seen_ms: u64,
    /// A queued restart is operator intent and survives like any other (ADR-0024).
    pub restart_pending: bool,
    /// What the operator's last rollout released to this Agent (ADR-0026 clause 6): the
    /// Deployment, the Package pinned, and each Configuration's pinned revision. `None` — nothing
    /// has been rolled out.
    pub assignment: Option<crate::fleet::Assignment>,
    /// The operator's labels on this Agent (ADR-0024 clause 21): part of its record, so they follow
    /// a re-key and leave with a forget.
    pub labels: BTreeMap<String, String>,
    /// The Host the Agent last arrived over (ADR-0024 clause 16) — its own on a direct connection,
    /// the Gateway's behind one; `None` without `[client_ca]`. It grants nothing.
    pub host: Option<String>,
}

impl PersistedAgent {
    /// A digest over the *durable* content — everything except `last_seen_ms` and `sequence_num`,
    /// which move on every report. This is what the caller's dirty check compares, so the common
    /// heartbeat, which changes nothing else, reaches no adapter at all (ADR-0024).
    ///
    /// It is computed from the record itself, never from an adapter's format (ADR-0009): each
    /// field length-prefixed so neighbours cannot run together, the wire-typed ones in their
    /// protobuf encoding. The digest lives in memory only, so its layout is free to change. The
    /// destructuring names every field, so a new one does not compile until it is placed here.
    pub fn durable_digest(&self) -> [u8; 32] {
        fn field(digest: &mut Sha256, bytes: &[u8]) {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        fn message<M: Message>(digest: &mut Sha256, value: Option<&M>) {
            field(digest, &[u8::from(value.is_some())]);
            field(
                digest,
                &value.map(Message::encode_to_vec).unwrap_or_default(),
            );
        }
        let PersistedAgent {
            sequence_num: _,
            last_seen_ms: _,
            capabilities,
            description,
            health,
            effective_config,
            remote_config_status,
            connection_settings_status,
            package_statuses,
            available_components,
            transport,
            restart_pending,
            assignment,
            labels,
            host,
        } = self;
        let mut digest = Sha256::new();
        field(&mut digest, &capabilities.to_le_bytes());
        message(&mut digest, description.as_ref());
        message(&mut digest, health.as_ref());
        field(&mut digest, &[u8::from(effective_config.is_some())]);
        field(
            &mut digest,
            effective_config.as_deref().unwrap_or_default().as_bytes(),
        );
        message(&mut digest, remote_config_status.as_ref());
        message(&mut digest, connection_settings_status.as_ref());
        message(&mut digest, package_statuses.as_ref());
        message(&mut digest, available_components.as_ref());
        field(&mut digest, transport.as_str().as_bytes());
        field(&mut digest, &[u8::from(*restart_pending)]);
        field(&mut digest, &[u8::from(assignment.is_some())]);
        if let Some(assignment) = assignment {
            field(&mut digest, assignment.deployment.as_bytes());
            field(&mut digest, &[u8::from(assignment.package.is_some())]);
            if let Some(package) = &assignment.package {
                field(&mut digest, package.agent_type.as_bytes());
                field(&mut digest, package.version.as_bytes());
            }
            field(
                &mut digest,
                &(assignment.configurations.len() as u64).to_le_bytes(),
            );
            for (name, hash) in &assignment.configurations {
                field(&mut digest, name.as_bytes());
                field(&mut digest, hash.as_bytes());
            }
        }
        field(&mut digest, &(labels.len() as u64).to_le_bytes());
        for (key, value) in labels {
            field(&mut digest, key.as_bytes());
            field(&mut digest, value.as_bytes());
        }
        field(&mut digest, &[u8::from(host.is_some())]);
        field(&mut digest, host.as_deref().unwrap_or_default().as_bytes());
        digest.finalize().into()
    }
}

/// Where one Agent is connected, as the instance that carries it writes it
/// (ADR-draft-the-fleet-record-of-agents-and-hosts clauses 3 and 4): whether a connection carries
/// it, and when it was last heard from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Presence {
    pub connected: bool,
    pub last_seen_ms: u64,
}

/// What [`AgentStore::take`] found of the holder it replaced
/// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
/// clause 8).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Taken {
    /// The other instance the stored presence named as the holder, by the id of its lease; `None`
    /// where it named this instance or none.
    pub from: Option<String>,
    /// Whether this take is the one that made the holder change more than ten times within a
    /// minute: the Agent is likely a duplicate whose connections two instances take in turn.
    pub likely_duplicate: bool,
    /// The count of takes of the Agent, this one included: what a disconnect this instance writes
    /// must find still stored.
    pub take: u64,
}

/// What [`AgentStore::create_within`] did with the record of an Agent first heard of.
#[derive(Clone, Debug, PartialEq)]
pub enum Creation {
    /// It created the record.
    Created,
    /// A record was stored already, another Server's: this one, as it is stored.
    Found(Box<PersistedAgent>),
    /// The store holds the most records it may, and created none.
    Full,
}

/// What an assignment can pin (ADR-0026 clause 12): what a deletion asks about before it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pin<'a> {
    /// A Configuration, at any of its revisions.
    Configuration(&'a str),
    /// A Package.
    Package(&'a PackageId),
    /// A Deployment, as the one a release went through.
    Deployment(&'a str),
}

impl Pin<'_> {
    /// Whether `assignment` pins it.
    #[must_use]
    pub fn pinned_by(&self, assignment: &Assignment) -> bool {
        match self {
            Pin::Configuration(name) => assignment.configurations.contains_key(*name),
            Pin::Package(id) => assignment.package.as_ref() == Some(*id),
            Pin::Deployment(name) => assignment.deployment == *name,
        }
    }
}

/// The storage port (ADR-0024, ADR-0029): the only thing the fleet logic knows about persistence. A
/// database or an external store is a new implementation of these operations plus one wiring
/// line — the rest of the Server is, by construction, unaffected.
pub trait AgentStore: Send + Sync {
    /// Every persisted record, once, at startup. A record that cannot be read fails loudly — a
    /// fleet that silently lost members is worse than one that refuses to start (ADR-0008).
    fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String>;

    /// The record of `uid` as stored now, `None` when none is: what an instance reads again when
    /// another instance announced a change to it
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 9). The default reads every record through [`load`](Self::load), as a store that
    /// is its only writer may; a shared store, and a wrapper of one, overrides it, or each read
    /// reads the whole fleet.
    ///
    /// # Errors
    /// Returns an error when the record cannot be read.
    fn get(&self, uid: &InstanceUid) -> Result<Option<PersistedAgent>, String> {
        Ok(self.load()?.remove(uid))
    }

    /// The records of `uids` as stored now, in one read, none for a uid with no record: what an
    /// instance reads again under its fleet lock, where one read per record would hold the lock
    /// for as many round trips. The default reads every record through [`load`](Self::load), as a
    /// store that is its only writer may; a shared store, and a wrapper of one, overrides it, or
    /// the read under the fleet lock reads the whole fleet.
    ///
    /// # Errors
    /// Returns an error when the records cannot be read.
    fn get_many(
        &self,
        uids: &[InstanceUid],
    ) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
        let mut all = self.load()?;
        Ok(uids
            .iter()
            .filter_map(|uid| all.remove_entry(uid))
            .collect())
    }

    /// Whether other Servers write this store too, so the fleet reads an Agent it does not hold
    /// from it rather than from what it loaded, and decides an act on an Agent from what it stores
    /// (ADR-draft-the-servers-state-and-its-stores clause 3). A store that is its one Server's alone
    /// says no.
    fn is_shared(&self) -> bool {
        false
    }

    /// Creates one record, or replaces the part of it a report decides: what the Agent reported,
    /// the transport, `last_seen_ms` and the Host it arrived over. The labels, the assignment and
    /// `restart_pending` of a record that exists stay as they are stored, since the acts that
    /// decide them write them (ADR-draft-the-fleet-record-of-agents-and-hosts clause 2). A store
    /// whose one Server is its only writer may write the whole record, which that Server holds.
    fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String>;

    /// Writes the part of a stored record a report decides, as [`put`](Self::put) does of one that
    /// exists, and creates none: what the fleet writes of an Agent whose record it created or read.
    /// Answers whether there was a record. On a shared store none means another Server forgot the
    /// Agent, and writing it again would undo that and pass the bound of
    /// [`create_within`](Self::create_within) (ADR-0024 clause 14). A store whose one Server is its
    /// only writer writes the record whole.
    ///
    /// # Errors
    /// Returns an error when the record cannot be written.
    fn write_report(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<bool, String> {
        self.put(uid, record)?;
        Ok(true)
    }

    /// [`write_report`](Self::write_report) at the shutdown flush: on a shared store only where
    /// the stored holder is `holder`, or is none and the stored count of takes is still `take`,
    /// the count this Server was last answered for the Agent, so an Agent that moved to another
    /// Server keeps what that Server wrote of it, also once it disconnected there, however the
    /// Servers' clocks differ (ADR-draft-the-fleet-record-of-agents-and-hosts clause 2). `holder`
    /// is the instance id of this Server's lease, `None` without one, and `take` is `None` where
    /// this Server was answered none. Answers whether it wrote the record. A store whose one
    /// Server is its only writer writes as `write_report` does.
    ///
    /// # Errors
    /// Returns an error when the record cannot be written.
    fn write_report_unless_held_elsewhere(
        &self,
        uid: &InstanceUid,
        record: &PersistedAgent,
        holder: Option<&str>,
        take: Option<u64>,
    ) -> Result<bool, String> {
        let _ = (holder, take);
        self.write_report(uid, record)
    }

    /// Creates the record of an Agent first heard of, unless the store holds `max_records` records
    /// already. A shared store counts them where they are stored, in the step that creates it, so
    /// the bound holds for every Server on it, however many create at once (ADR-0021 clause 12).
    /// A record stored already is answered as it is stored, written nothing, and passes no bound.
    /// A store whose one Server holds every record bounds them in memory, and creates it.
    ///
    /// # Errors
    /// Returns an error when the records cannot be counted, or the record read or written.
    fn create_within(
        &self,
        uid: &InstanceUid,
        record: &PersistedAgent,
        max_records: usize,
    ) -> Result<Creation, String> {
        let _ = max_records;
        self.put(uid, record)?;
        Ok(Creation::Created)
    }

    /// Sets the labels of one record and nothing else, and announces it to the other Servers on the
    /// store. Answers whether there was a record to set them on.
    ///
    /// # Errors
    /// Returns an error when the record cannot be written.
    fn set_labels(
        &self,
        uid: &InstanceUid,
        labels: &BTreeMap<String, String>,
    ) -> Result<bool, String>;

    /// Sets or clears the queued restart of one record and nothing else, and announces a request
    /// to the other Servers on the store. Answers what it was before, `None` when there is no
    /// record: the Server that delivers a restart clears it first and delivers it only when it was
    /// still queued, so a request another Server set after that clear stays queued, and one another
    /// Server delivered already is not delivered twice.
    ///
    /// # Errors
    /// Returns an error when the record cannot be written.
    fn set_restart_pending(&self, uid: &InstanceUid, pending: bool)
        -> Result<Option<bool>, String>;

    /// Forgets one record (ADR-0024); removing what is already absent is not an error.
    fn remove(&self, uid: &InstanceUid) -> Result<(), String>;

    /// Writes the presence of the Agents `holder` carries, one batch, in the order of their
    /// `instance_uid` whatever order they are given in, so two batches over the same Agents never
    /// deadlock (ADR-draft-the-fleet-record-of-agents-and-hosts clause 4). Each Agent comes with
    /// the count of takes `holder` was last answered for it, `None` where it was answered none. A
    /// connected Agent names `holder` as the instance that holds its connection only where the
    /// stored presence names `holder`, or none, or an instance without a live lease: a batch read
    /// before another instance took the Agent never undoes that take, and writes nothing of it.
    /// Naming `holder` anew counts as a take. A disconnect is written only of an Agent whose stored
    /// presence names `holder` and still holds the count it comes with, so it never undoes another
    /// instance's take, nor a take of `holder` made since, however the instances' clocks differ;
    /// `last_seen_ms` never moves back. A record no longer stored is written nothing. Answers the
    /// count of takes of each connected Agent it wrote. A store whose one Server holds every
    /// presence in memory keeps none, writes nothing, and answers none.
    ///
    /// # Errors
    /// Returns an error when the batch cannot be written; nothing of it is then written.
    fn write_presence(
        &self,
        holder: &str,
        presence: &[(InstanceUid, Presence, Option<u64>)],
    ) -> Result<HashMap<InstanceUid, u64>, String> {
        let _ = (holder, presence);
        Ok(HashMap::new())
    }

    /// Takes an Agent whose connection `holder` now carries, outside the batches: one conditional
    /// step that names `holder` as the instance that holds its connection, writes it connected and
    /// last heard from at `last_seen_ms`, which never moves back, counts the take, and answers the
    /// count and the holder it replaced. A shared store announces the take to the other Servers when
    /// the holder it replaced is another Server, live lease or not, so that one stops carrying it,
    /// and counts each change of holder, so the take that makes it change more than ten times
    /// within a minute says so
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 8). Answers `None` when there is no record. A store whose one Server holds every
    /// presence in memory keeps none, and writes nothing.
    ///
    /// # Errors
    /// Returns an error when the record cannot be read or written; nothing is then written.
    fn take(
        &self,
        uid: &InstanceUid,
        holder: &str,
        last_seen_ms: u64,
    ) -> Result<Option<Taken>, String> {
        let _ = (uid, holder, last_seen_ms);
        Ok(Some(Taken::default()))
    }

    /// The holder each stored presence of `uids` names, by the id of its lease, `None` where it
    /// names none; a record not stored is left out. What an instance that heard of a take reads
    /// to decide whether it still carries the Agent
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 8).
    ///
    /// # Errors
    /// Returns an error when the records cannot be read, and on a store that keeps no presence.
    fn holders(
        &self,
        uids: &[InstanceUid],
    ) -> Result<HashMap<InstanceUid, Option<String>>, String> {
        let _ = uids;
        Err("this store keeps no presence; its Server decides from what it holds".to_string())
    }

    /// Forgets one record of a shared store when its stored presence lets it go: when no instance
    /// with a live lease holds its connection, or it was last heard from before `silent_before_ms`
    /// (ADR-draft-the-fleet-record-of-agents-and-hosts clause 11). The presence is read and the
    /// record removed in one step, so no report written meanwhile is forgotten unseen. Answers
    /// `None` when there is no record, `Some(false)` when it is still reporting and is kept.
    ///
    /// # Errors
    /// Returns an error when the record cannot be read or removed, and on a store that keeps no
    /// presence, whose one Server decides from what it holds.
    fn remove_if_silent(
        &self,
        uid: &InstanceUid,
        silent_before_ms: u64,
    ) -> Result<Option<bool>, String> {
        let _ = (uid, silent_before_ms);
        Err("this store keeps no presence; its Server decides from what it holds".to_string())
    }

    /// The identity reassignment (`RequestInstanceUid`): one operation, so an adapter with atomic
    /// rename or transactions can make it one step. A shared store moves the record as it is
    /// stored and writes nothing of `record`, so what another Server set stays, moves nothing of
    /// a record it no longer holds, and announces the old identity forgotten, so another Server
    /// holding it drops it.
    ///
    /// # Errors
    /// Returns an error when the record cannot be moved; it then stays under `old`.
    fn rekey(
        &self,
        old: &InstanceUid,
        new: &InstanceUid,
        record: &PersistedAgent,
    ) -> Result<(), String> {
        self.put(new, record)?;
        self.remove(old)
    }

    /// A rollout (ADR-0029 clause 2): the revisions it pins, each added to its Configuration's
    /// retained revisions by its hash and no other revision touched, and the records carrying the
    /// assignments that pin them, as one operation. A database adapter holds each pinned
    /// Configuration's row against a collection until it commits, so no other instance collects a
    /// revision it pins ([`ConfigBackend::collect`](crate::configs::ConfigBackend::collect)). An
    /// adapter that
    /// cannot make it one step writes the revisions first, so a failure part-way leaves at most a
    /// revision nothing references — which the next load collects — and never an assignment
    /// pinning a revision that was not written. A record that cannot be written leaves the records
    /// as they were: an adapter that writes them one by one puts back the ones it already wrote,
    /// and what [`pinning`](Self::pinning) and
    /// [`referenced_revisions`](Self::referenced_revisions) answer follows what it stores. Of each
    /// record a shared store writes the assignment alone, and of a record it no longer holds
    /// nothing; it writes them in the order of their `instance_uid` whatever order they are given
    /// in, so two releases over the same Agents on two Servers wait for each other and never
    /// deadlock. Answers the Agents whose records it wrote, in the order given: on a shared store
    /// not one forgotten through another Server, whose assignment a rollout therefore neither
    /// counts nor holds.
    ///
    /// # Errors
    /// Returns an error when the revisions or a record cannot be written.
    fn release(
        &self,
        revisions: &[Pinned],
        agents: &[(InstanceUid, PersistedAgent)],
    ) -> Result<Vec<InstanceUid>, String>;

    /// How many stored assignments pin `pin` (ADR-0026 clause 12).
    fn pinning(&self, pin: Pin<'_>) -> usize;

    /// The revision hashes of the Configuration `name` that stored assignments reference — what
    /// its retained revisions are collected down to (ADR-0026).
    fn referenced_revisions(&self, name: &str) -> BTreeSet<String>;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn record() -> PersistedAgent {
        PersistedAgent {
            sequence_num: 7,
            capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
            description: Some(AgentDescription {
                identifying_attributes: vec![opamp::attributes::string_attr(
                    "service.name",
                    "otelcol",
                )],
                ..Default::default()
            }),
            health: Some(ComponentHealth {
                healthy: true,
                ..Default::default()
            }),
            effective_config: Some("receivers: {}".to_string()),
            remote_config_status: Some(RemoteConfigStatus {
                last_remote_config_hash: vec![1, 2, 3],
                ..Default::default()
            }),
            connection_settings_status: None,
            package_statuses: None,
            available_components: None,
            transport: Transport::WebSocket,
            last_seen_ms: 123,
            restart_pending: true,
            assignment: Some(crate::fleet::Assignment {
                deployment: "stable".to_string(),
                package: Some(PackageId::new("otelcol", "1.2.3").expect("package id")),
                configurations: BTreeMap::from([("base".to_string(), "0123abcd".to_string())]),
            }),
            labels: BTreeMap::from([("rollout".to_string(), "canary".to_string())]),
            host: Some("0192f0c4-host".to_string()),
        }
    }

    /// The dirty check's foundation: a report that only moves the timestamp and the sequence
    /// number — a heartbeat — has the same durable digest, so the caller writes nothing.
    #[test]
    fn a_heartbeat_does_not_change_the_durable_digest() {
        let settled = record();
        let mut beaten = record();
        beaten.last_seen_ms += 30_000;
        beaten.sequence_num += 1;
        assert_eq!(settled.durable_digest(), beaten.durable_digest());

        let mut changed = record();
        changed.health = Some(ComponentHealth {
            healthy: false,
            ..Default::default()
        });
        assert_ne!(settled.durable_digest(), changed.durable_digest());
    }

    /// Every durable field reaches the digest, and an absent value is not an empty one: a change
    /// the digest missed would never be written.
    #[test]
    fn every_durable_change_moves_the_digest() {
        let settled = record().durable_digest();
        let changes: [fn(&mut PersistedAgent); 12] = [
            |r| r.capabilities ^= 1,
            |r| r.effective_config = Some("x: 1".into()),
            |r| r.transport = Transport::Http,
            |r| r.restart_pending = !r.restart_pending,
            |r| r.assignment = None,
            |r| r.labels.clear(),
            |r| {
                r.labels.insert("rollout".into(), "stable".into());
            },
            |r| r.host = None,
            |r| {
                if let Some(a) = r.assignment.as_mut() {
                    a.configurations.insert("a".into(), "1".into());
                }
            },
            |r| {
                if let Some(a) = r.assignment.as_mut() {
                    a.package = None;
                }
            },
            |r| {
                if let Some(a) = r.assignment.as_mut() {
                    a.deployment = "d".into();
                }
            },
            |r| {
                if let Some(a) = r.assignment.as_mut() {
                    a.package = Some(PackageId {
                        agent_type: "t".into(),
                        version: "1".into(),
                    });
                }
            },
        ];
        for (i, change) in changes.iter().enumerate() {
            let mut changed = record();
            change(&mut changed);
            assert_ne!(
                changed.durable_digest(),
                settled,
                "change {i} left the digest"
            );
        }
    }
}
