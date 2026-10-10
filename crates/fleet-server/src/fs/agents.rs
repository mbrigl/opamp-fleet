//! The Agent-record store on the filesystem (ADR-0026): the default adapter behind
//! [`AgentStore`](crate::agent_store::AgentStore).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use base64::Engine as _;
use opamp::uid::InstanceUid;
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::agent_store::{AgentStore, PersistedAgent};
use crate::fleet::Transport;
use crate::packages::PackageId;

/// The default adapter (ADR-0026): one JSON file per Agent under `<config_dir>/agents/`,
/// following the `LabelStore` pattern — temp file plus atomic rename, loud failure on a file
/// that does not parse.
pub struct FsAgentStore {
    dir: PathBuf,
}

/// The on-disk envelope — **this adapter's format, not the port's**. Scalars and the
/// effective-config text stay readable; the wire-typed fields are protobuf bytes base64-inline,
/// the one encoding whose compatibility rules the Baseline already defines (ADR-0010, ADR-0026).
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
    /// The config assignments (ADR-0027), name → revision hash. Absent in a file an older Server
    /// wrote, which is exactly the migration marker the fleet reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config_assignments: Option<BTreeMap<String, String>>,
    /// What was rolled out to this Agent (ADR-0027, ADR-0028): the Deployment that released it
    /// and the Package it pinned, as `<agent type>@<version>`. Absent means nothing was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_assignment: Option<PackageAssignmentMeta>,
}

/// One Agent's package assignment as persisted.
#[derive(Serialize, Deserialize)]
struct PackageAssignmentMeta {
    deployment: String,
    package: String,
}

/// Version 2 is ADR-0028's assignment shape, and there is **no reader for version 1**: no legacy
/// store to support, so an envelope this Server did not write is named rather than guessed at.
const ENVELOPE_VERSION: u32 = 2;

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
            transport: record.transport.as_str().to_string(),
            last_seen_ms: record.last_seen_ms,
            restart_pending: record.restart_pending,
            effective_config: record.effective_config.clone(),
            description: encode(&record.description),
            health: encode(&record.health),
            remote_config_status: encode(&record.remote_config_status),
            connection_settings_status: encode(&record.connection_settings_status),
            package_statuses: encode(&record.package_statuses),
            available_components: encode(&record.available_components),
            config_assignments: record.config_assignments.clone(),
            package_assignment: record.package_assignment.as_ref().map(|assignment| {
                PackageAssignmentMeta {
                    deployment: assignment.deployment.clone(),
                    package: assignment.package.to_string(),
                }
            }),
        }
    }

    fn into_record(self) -> Result<PersistedAgent, String> {
        if self.version != ENVELOPE_VERSION {
            // Loud, and it stops the Server: an Agent record is what the fleet knows about a host,
            // and one silently skipped would look exactly like a host that never enrolled.
            return Err(format!(
                "envelope version {} is not the understood {ENVELOPE_VERSION} — this Server reads \
                 no earlier record format. Clear the agents directory; every Agent re-enrols and \
                 re-reports on its next message, and what is lost is the operator's rollouts",
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
            transport: Transport::parse(&self.transport),
            last_seen_ms: self.last_seen_ms,
            restart_pending: self.restart_pending,
            config_assignments: self.config_assignments,
            package_assignment: self
                .package_assignment
                .map(|assignment| {
                    PackageId::parse(&assignment.package)
                        .map(|package| crate::fleet::PackageAssignment {
                            deployment: assignment.deployment,
                            package,
                        })
                        .map_err(|e| format!("invalid package assignment: {e}"))
                })
                .transpose()?,
        })
    }
}

impl FsAgentStore {
    /// Opens the store, creating its directory owner-only — reported effective configurations may
    /// hold credentials (ADR-0026), the same reasoning that guards the package store's metadata.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        super::create_private_dir(&dir)?;
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
        let json =
            serde_json::to_vec_pretty(&Envelope::from_record(record)).expect("agent serialize");
        super::replace(&self.path(uid), &json)
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
    use crate::agent_store::tests::record;

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

    /// ADR-0028 point 34: an envelope written before the ADR has no assignment fields, and they
    /// restore as `None` — the marker the fleet's migration reads. They are not invented as
    /// empty, which would silently un-roll the Agent.
    /// Verifies: ADR-0037
    #[test]
    fn a_record_without_assignments_restores_with_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsAgentStore::open(dir.path().join("agents")).expect("open");
        let uid = InstanceUid::default();
        let mut old = record();
        old.config_assignments = None;
        old.package_assignment = None;
        store.put(&uid, &old).expect("put");
        let text = std::fs::read_to_string(dir.path().join("agents").join(format!("{uid}.json")))
            .expect("read");
        assert!(
            !text.contains("assignments"),
            "absent fields stay absent on disk: {text}"
        );
        let restored = store.load().expect("load");
        assert!(restored[&uid].config_assignments.is_none());
        assert!(restored[&uid].package_assignment.is_none());
    }

    /// Forgetting removes the file (ADR-0026 extended); removing the absent is not an error.
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

    /// A file that does not parse fails startup loudly rather than being skipped (ADR-0009's
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
