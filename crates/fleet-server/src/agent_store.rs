//! The Agent-record storage port (ADR-0024).
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

use std::collections::HashMap;

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
    /// The transport the last report arrived on — informational, never a routing key (ADR-0015).
    pub transport: Transport,
    pub last_seen_ms: u64,
    /// A queued restart is operator intent and survives like any other (ADR-0024).
    pub restart_pending: bool,
    /// What the operator's last rollout released to this Agent (ADR-0034 clause 5): the
    /// Deployment, the Package pinned, and each Configuration's pinned revision. `None` — nothing
    /// has been rolled out.
    pub assignment: Option<crate::fleet::Assignment>,
}

impl PersistedAgent {
    /// A digest over the *durable* content — everything except `last_seen_ms` and `sequence_num`,
    /// which move on every report. This is what the caller's dirty check compares, so the common
    /// heartbeat, which changes nothing else, reaches no adapter at all (ADR-0024).
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
            assignment,
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
        digest.finalize().into()
    }
}

/// The storage port (ADR-0024): the only thing the fleet logic knows about persistence. A
/// database or an external store is a new implementation of these four operations plus one wiring
/// line — the rest of the Server is, by construction, unaffected.
pub trait AgentStore: Send + Sync {
    /// Every persisted record, once, at startup. A record that cannot be read fails loudly — a
    /// fleet that silently lost members is worse than one that refuses to start (ADR-0010).
    fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String>;

    /// Creates or replaces one record.
    fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String>;

    /// Forgets one record (ADR-0024); removing what is already absent is not an error.
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
    use std::collections::BTreeMap;

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
        let changes: [fn(&mut PersistedAgent); 9] = [
            |r| r.capabilities ^= 1,
            |r| r.effective_config = Some("x: 1".into()),
            |r| r.transport = Transport::Http,
            |r| r.restart_pending = !r.restart_pending,
            |r| r.assignment = None,
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
