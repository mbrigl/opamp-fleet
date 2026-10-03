//! The Agent-record storage port (ADR-0026).
//!
//! The fleet is loaded whole at startup and held in memory; at runtime the store only ever
//! receives writes and deletions for single Agents. That narrow access pattern is what the port
//! states — `load`, `put`, `remove`, `rekey` — and nothing more. The port speaks the *typed*
//! record: what bytes or rows a backend turns it into is the adapter's own business.
//!
//! The write discipline (no write on a heartbeat, flush on graceful shutdown) deliberately lives
//! in the caller ([`crate::fleet::AppState`]), so no backend can get it wrong.
//!
//! Stored records are secret-bearing whatever the backend: a reported effective configuration is
//! whatever the Managed Process runs, credentials included. The filesystem adapter
//! ([`FsAgentStore`](crate::fs::FsAgentStore)) answers with an owner-only directory; any other
//! adapter must answer with its own access control.

use std::collections::{BTreeMap, HashMap};

use opamp::proto::{
    AgentDescription, AvailableComponents, ComponentHealth, ConnectionSettingsStatus,
    PackageStatuses, RemoteConfigStatus,
};
use opamp::uid::InstanceUid;
use prost::Message;
use sha2::{Digest, Sha256};

use crate::fleet::Transport;

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
    /// The transport the last report arrived on — informational, never a routing key (ADR-0009).
    pub transport: Transport,
    pub last_seen_ms: u64,
    /// A queued restart is operator intent and survives like any other (ADR-0026).
    pub restart_pending: bool,
    /// The Configurations the operator rolled out to this Agent (ADR-0027): name → the pinned
    /// revision's hash. `None` marks a record persisted before the ADR, whose assignments the
    /// fleet seeds at startup from what was published then (point 9).
    pub config_assignments: Option<BTreeMap<String, String>>,
    /// The package Sets the operator rolled out to this Agent (ADR-0027), keyed by package name.
    /// `None` marks a record whose seed has not run — it runs when package delivery is armed.
    pub package_assignment: Option<crate::fleet::PackageAssignment>,
}

impl PersistedAgent {
    /// A digest over the *durable* content — everything except `last_seen_ms` and `sequence_num`,
    /// which move on every report. This is what the caller's dirty check compares, so the common
    /// heartbeat, which changes nothing else, reaches no adapter at all (ADR-0026).
    ///
    /// It is computed from the record itself, never from an adapter's format (ADR-0006): each
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
            config_assignments,
            package_assignment,
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
        field(&mut digest, &[u8::from(config_assignments.is_some())]);
        for (name, hash) in config_assignments.iter().flatten() {
            field(&mut digest, name.as_bytes());
            field(&mut digest, hash.as_bytes());
        }
        field(&mut digest, &[u8::from(package_assignment.is_some())]);
        if let Some(assignment) = package_assignment {
            field(&mut digest, assignment.deployment.as_bytes());
            field(&mut digest, assignment.package.agent_type.as_bytes());
            field(&mut digest, assignment.package.version.as_bytes());
        }
        digest.finalize().into()
    }
}

/// The storage port (ADR-0026): the only thing the fleet logic knows about persistence. A
/// database or an external store is a new implementation of these four operations plus one wiring
/// line — the rest of the Server is, by construction, unaffected.
pub trait AgentStore: Send + Sync {
    /// Every persisted record, once, at startup. A record that cannot be read fails loudly — a
    /// fleet that silently lost members is worse than one that refuses to start (ADR-0011).
    fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String>;

    /// Creates or replaces one record.
    fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String>;

    /// Forgets one record (ADR-0026); removing what is already absent is not an error.
    fn remove(&self, uid: &InstanceUid) -> Result<(), String>;

    /// The identity reassignment (`RequestInstanceUid`): one operation, so an adapter with atomic
    /// rename or transactions can make it one step.
    fn rekey(
        &self,
        old: &InstanceUid,
        new: &InstanceUid,
        record: &PersistedAgent,
    ) -> Result<(), String> {
        self.put(new, record)?;
        self.remove(old)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::packages::PackageId;

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
            config_assignments: Some(BTreeMap::from([(
                "base".to_string(),
                "0123abcd".to_string(),
            )])),
            package_assignment: Some(crate::fleet::PackageAssignment {
                deployment: "stable".to_string(),
                package: PackageId::new("otelcol", "1.2.3").expect("package id"),
            }),
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
        let changes: [fn(&mut PersistedAgent); 7] = [
            |r| r.capabilities ^= 1,
            |r| r.effective_config = Some("x: 1".into()),
            |r| r.transport = Transport::Http,
            |r| r.restart_pending = !r.restart_pending,
            |r| r.config_assignments = Some(BTreeMap::new()),
            |r| {
                r.config_assignments = Some(BTreeMap::from([("a".into(), "1".into())]));
            },
            |r| {
                r.package_assignment = Some(crate::fleet::PackageAssignment {
                    deployment: "d".into(),
                    package: PackageId {
                        agent_type: "t".into(),
                        version: "1".into(),
                    },
                });
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
