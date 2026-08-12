//! The Agent-record storage port and its filesystem adapter (ADR-0051).
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
//! whatever the Managed Process runs, credentials included. The filesystem adapter answers with an
//! owner-only directory; any other adapter must answer with its own access control.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;
use opamp::proto::{
    AgentDescription, AvailableComponents, ComponentHealth, ConnectionSettingsStatus,
    PackageStatuses, RemoteConfigStatus,
};
use opamp::uid::InstanceUid;
use prost::Message;
use serde::{Deserialize, Serialize};
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
    /// The transport the last report arrived on — informational, never a routing key (ADR-0003).
    pub transport: Transport,
    pub last_seen_ms: u64,
    /// A queued restart is operator intent and survives like any other (ADR-0051).
    pub restart_pending: bool,
}

impl PersistedAgent {
    /// A digest over the *durable* content — everything except `last_seen_ms` and `sequence_num`,
    /// which move on every report. This is what the caller's dirty check compares, so the common
    /// heartbeat, which changes nothing else, reaches no adapter at all (ADR-0051).
    pub fn durable_digest(&self) -> [u8; 32] {
        let mut settled = self.clone();
        settled.last_seen_ms = 0;
        settled.sequence_num = 0;
        let envelope = Envelope::from_record(&settled);
        let json = serde_json::to_vec(&envelope).expect("agent record serialize");
        Sha256::digest(&json).into()
    }
}

/// The storage port (ADR-0051): the only thing the fleet logic knows about persistence. A
/// database or an external store is a new implementation of these four operations plus one wiring
/// line — the rest of the Server is, by construction, unaffected.
pub trait AgentStore: Send + Sync {
    /// Every persisted record, once, at startup. A record that cannot be read fails loudly — a
    /// fleet that silently lost members is worse than one that refuses to start (ADR-0008).
    fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String>;

    /// Creates or replaces one record.
    fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String>;

    /// Forgets one record (ADR-0039); removing what is already absent is not an error.
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

/// The default adapter (ADR-0051): one JSON file per Agent under `<config_dir>/agents/`,
/// following the `LabelStore` pattern — temp file plus atomic rename, loud failure on a file
/// that does not parse.
pub struct FsAgentStore {
    dir: PathBuf,
}

/// The on-disk envelope — **this adapter's format, not the port's**. Scalars and the
/// effective-config text stay readable; the wire-typed fields are protobuf bytes base64-inline,
/// the one encoding whose compatibility rules the Baseline already defines (ADR-0006, ADR-0051).
#[derive(Serialize, Deserialize)]
struct Envelope {
    /// The envelope shape, so a future change can migrate deliberately.
    version: u32,
    sequence_num: u64,
    capabilities: u64,
    transport: String,
    last_seen_ms: u64,
    restart_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effective_config: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    health: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_config_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection_settings_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_statuses: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    available_components: Option<String>,
}

const ENVELOPE_VERSION: u32 = 1;

fn encode<M: Message>(message: &Option<M>) -> Option<String> {
    message
        .as_ref()
        .map(|m| base64::engine::general_purpose::STANDARD.encode(m.encode_to_vec()))
}

fn decode<M: Message + Default>(field: &Option<String>, what: &str) -> Result<Option<M>, String> {
    field
        .as_ref()
        .map(|text| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|e| format!("{what} is not base64: {e}"))?;
            M::decode(bytes.as_slice()).map_err(|e| format!("{what} does not decode: {e}"))
        })
        .transpose()
}

impl Envelope {
    fn from_record(record: &PersistedAgent) -> Self {
        Envelope {
            version: ENVELOPE_VERSION,
            sequence_num: record.sequence_num,
            capabilities: record.capabilities,
            transport: match record.transport {
                Transport::Http => "http".to_string(),
                Transport::WebSocket => "websocket".to_string(),
            },
            last_seen_ms: record.last_seen_ms,
            restart_pending: record.restart_pending,
            effective_config: record.effective_config.clone(),
            description: encode(&record.description),
            health: encode(&record.health),
            remote_config_status: encode(&record.remote_config_status),
            connection_settings_status: encode(&record.connection_settings_status),
            package_statuses: encode(&record.package_statuses),
            available_components: encode(&record.available_components),
        }
    }

    fn into_record(self) -> Result<PersistedAgent, String> {
        if self.version != ENVELOPE_VERSION {
            return Err(format!(
                "envelope version {} is not the understood {ENVELOPE_VERSION}",
                self.version
            ));
        }
        Ok(PersistedAgent {
            sequence_num: self.sequence_num,
            capabilities: self.capabilities,
            description: decode(&self.description, "description")?,
            health: decode(&self.health, "health")?,
            effective_config: self.effective_config,
            remote_config_status: decode(&self.remote_config_status, "remote_config_status")?,
            connection_settings_status: decode(
                &self.connection_settings_status,
                "connection_settings_status",
            )?,
            package_statuses: decode(&self.package_statuses, "package_statuses")?,
            available_components: decode(&self.available_components, "available_components")?,
            transport: match self.transport.as_str() {
                "websocket" => Transport::WebSocket,
                _ => Transport::Http,
            },
            last_seen_ms: self.last_seen_ms,
            restart_pending: self.restart_pending,
        })
    }
}

impl FsAgentStore {
    /// Opens the store, creating its directory owner-only — reported effective configurations may
    /// hold credentials (ADR-0051), the same reasoning that guards the package store's metadata.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
        }
        Ok(FsAgentStore { dir })
    }

    fn path(&self, uid: &InstanceUid) -> PathBuf {
        self.dir.join(format!("{uid}.json"))
    }
}

impl AgentStore for FsAgentStore {
    fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
        let mut records = HashMap::new();
        let entries = std::fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| format!("cannot read the Agent identity from {}", path.display()))?;
            let uid = InstanceUid::parse(stem)
                .ok_or_else(|| format!("{} is not named after an Instance UID", path.display()))?;
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let envelope: Envelope = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            let record = envelope
                .into_record()
                .map_err(|e| format!("cannot restore {}: {e}", path.display()))?;
            records.insert(uid, record);
        }
        Ok(records)
    }

    fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String> {
        let path = self.path(uid);
        let temp = self.dir.join(format!("{uid}.json.tmp"));
        let json =
            serde_json::to_vec_pretty(&Envelope::from_record(record)).expect("agent serialize");
        std::fs::write(&temp, json).map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path).map_err(|e| format!("cannot persist {}: {e}", path.display()))
    }

    fn remove(&self, uid: &InstanceUid) -> Result<(), String> {
        let path = self.path(uid);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot delete {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> PersistedAgent {
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
        }
    }

    /// The round trip the whole decision rests on: what was written is what is restored, wire
    /// types and all.
    #[test]
    fn a_record_survives_the_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsAgentStore::open(dir.path().join("agents")).expect("open");
        let uid = InstanceUid::default();
        store.put(&uid, &record()).expect("put");

        let reopened = FsAgentStore::open(dir.path().join("agents")).expect("reopen");
        let restored = reopened.load().expect("load");
        assert!(restored[&uid] == record(), "the record round-trips whole");
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

    /// Forgetting removes the file (ADR-0039 extended); removing the absent is not an error.
    #[test]
    fn remove_deletes_and_tolerates_absence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsAgentStore::open(dir.path().join("agents")).expect("open");
        let uid = InstanceUid::default();
        store.put(&uid, &record()).expect("put");
        store.remove(&uid).expect("remove");
        assert!(store.load().expect("load").is_empty());
        store.remove(&uid).expect("removing the absent is fine");
    }

    /// The identity reassignment: the record follows the Agent, the old key leaves the store.
    #[test]
    fn rekey_moves_the_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsAgentStore::open(dir.path().join("agents")).expect("open");
        let (old, new) = (InstanceUid::default(), InstanceUid::default());
        store.put(&old, &record()).expect("put");
        store.rekey(&old, &new, &record()).expect("rekey");
        let restored = store.load().expect("load");
        assert!(restored.contains_key(&new) && !restored.contains_key(&old));
    }

    /// A file that does not parse fails startup loudly rather than being skipped (ADR-0008's
    /// principle, as every store here applies it).
    #[test]
    fn a_corrupt_record_fails_the_load_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsAgentStore::open(dir.path().join("agents")).expect("open");
        let uid = InstanceUid::default();
        let path = dir.path().join("agents").join(format!("{uid}.json"));
        std::fs::write(&path, "not json").expect("write");
        let err = store.load().expect_err("must refuse");
        assert!(err.contains(&format!("{uid}")), "names the file: {err}");
    }
}
