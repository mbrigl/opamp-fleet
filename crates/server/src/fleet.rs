//! In-memory fleet state and the OpAMP control loop, keyed by Instance UID — never by the
//! connection that carried a message (ADR-0003).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use opamp::proto::{
    any_value, AgentConfigFile, AgentConfigMap, AgentDescription, AgentIdentification,
    AgentRemoteConfig, AgentToServer, AgentToServerFlags, ComponentHealth, KeyValue,
    RemoteConfigStatus, RemoteConfigStatuses, ServerCapabilities, ServerErrorResponse,
    ServerErrorResponseType, ServerToAgent, ServerToAgentFlags,
};
use opamp::uid::InstanceUid;
use serde::Serialize;
use tokio::sync::watch;
use tracing::{info, warn};
use utoipa::ToSchema;

use crate::configs::{ConfigStore, Configuration, DesiredConfig};

/// The Capability Set this Server declares (see docs/CONFORMANCE.md).
pub const SERVER_CAPABILITIES: u64 = ServerCapabilities::AcceptsStatus as u64
    | ServerCapabilities::OffersRemoteConfig as u64
    | ServerCapabilities::AcceptsEffectiveConfig as u64;

/// Which transport a report arrived on. Recorded for the operator; it never keys any state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Http,
    WebSocket,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Transport::Http => "http",
            Transport::WebSocket => "websocket",
        }
    }
}

/// Everything the Server knows about one Agent.
pub struct AgentRecord {
    pub sequence_num: u64,
    pub capabilities: u64,
    pub description: Option<AgentDescription>,
    pub health: Option<ComponentHealth>,
    pub effective_config: Option<String>,
    pub remote_config_status: Option<RemoteConfigStatus>,
    pub transport: Transport,
    pub connected: bool,
    pub last_seen_ms: u64,
}

/// The result of processing one `AgentToServer`: the reply to send back on the same transport, and
/// what the transport layer needs to know for its own bookkeeping.
pub struct Processed {
    pub reply: ServerToAgent,
    /// The identity the Agent goes by *after* this message (it may have been reassigned).
    pub uid: Option<InstanceUid>,
    /// The Agent said goodbye; a WebSocket loop drops it from its connection-local set.
    pub disconnected: bool,
}

/// Shared state behind every handler: the fleet, the Configuration store, and the push channel
/// WebSocket loops subscribe to.
pub struct AppState {
    fleet: Mutex<HashMap<InstanceUid, AgentRecord>>,
    configs: ConfigStore,
    push: watch::Sender<u64>,
}

impl AppState {
    /// Builds the state, restoring every persisted Configuration from `config_dir`. A store that
    /// cannot be opened (or holds an unparsable file) fails startup loudly.
    pub fn new(config_dir: PathBuf) -> Result<Self, String> {
        let configs = ConfigStore::open(config_dir)?;
        let restored = configs.list().len();
        if restored > 0 {
            info!(
                configurations = restored,
                "restored the Configuration store"
            );
        }
        Ok(AppState {
            fleet: Mutex::new(HashMap::new()),
            configs,
            push: watch::channel(0).0,
        })
    }

    /// A receiver that fires whenever any Configuration changes; WebSocket loops use it to push
    /// offers without waiting for the Agent to speak.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.push.subscribe()
    }

    /// Read access to the Configuration store (the REST API's `GET` routes).
    pub fn configurations(&self) -> &ConfigStore {
        &self.configs
    }

    /// Creates or replaces a Configuration, persists it, and wakes every WebSocket loop — the
    /// matching Agents are offered the change without being asked.
    pub fn put_configuration(&self, config: Configuration) -> Result<(), String> {
        let name = config.name.clone();
        self.configs.put(config)?;
        self.push.send_modify(|rev| *rev += 1);
        info!(configuration = %name, "configuration stored and distributed");
        Ok(())
    }

    /// Deletes a Configuration and wakes every WebSocket loop; `false` when none of that name
    /// exists. Agents that applied it keep running it — narrowing never revokes (ADR-0012).
    pub fn delete_configuration(&self, name: &str) -> Result<bool, String> {
        let deleted = self.configs.delete(name)?;
        if deleted {
            self.push.send_modify(|rev| *rev += 1);
            info!(configuration = %name, "configuration deleted");
        }
        Ok(deleted)
    }

    /// The control loop for one report, shared by both transports (ADR-0007): update what we know,
    /// then answer with what the Agent still lacks — the config offer gated by the hash comparison.
    pub fn process(&self, msg: AgentToServer, transport: Transport) -> Processed {
        let Some(mut uid) = InstanceUid::from_wire(&msg.instance_uid) else {
            warn!(
                len = msg.instance_uid.len(),
                "report with a malformed instance_uid"
            );
            return Processed {
                reply: bad_request("instance_uid must be 16 bytes (UUID v7 recommended)"),
                uid: None,
                disconnected: false,
            };
        };

        let mut fleet = self.fleet.lock().expect("fleet lock");
        let mut reply_flags = 0u64;
        let mut identification = None;

        // The Agent asked the Server to assign its identity (AgentToServerFlags_RequestInstanceUid):
        // mint a UUID v7 and re-key the record; the reply tells the Agent to adopt it.
        if msg.flags & AgentToServerFlags::RequestInstanceUid as u64 != 0 {
            let new_uid = InstanceUid::default();
            if let Some(record) = fleet.remove(&uid) {
                fleet.insert(new_uid, record);
            }
            info!(old = %uid, new = %new_uid, "assigned a server-generated instance_uid");
            identification = Some(AgentIdentification {
                new_instance_uid: new_uid.as_bytes().to_vec(),
            });
            uid = new_uid;
        }

        let known = fleet.contains_key(&uid);
        let record = fleet.entry(uid).or_insert_with(|| {
            info!(agent = %uid, transport = transport.as_str(), "new agent");
            AgentRecord {
                sequence_num: msg.sequence_num,
                capabilities: 0,
                description: None,
                health: None,
                effective_config: None,
                remote_config_status: None,
                transport,
                connected: true,
                last_seen_ms: now_ms(),
            }
        });

        // A compressed report (unchanged fields omitted) is only usable if our state is current.
        // A gap in sequence_num — or an Agent we have never seen describing itself with nothing —
        // means state was lost somewhere; the Baseline's recovery is to demand a full report.
        let compressed = msg.agent_description.is_none();
        let gap = known && msg.sequence_num != record.sequence_num.wrapping_add(1);
        if compressed && (!known || gap) {
            reply_flags |= ServerToAgentFlags::ReportFullState as u64;
        }

        record.sequence_num = msg.sequence_num;
        record.transport = transport;
        record.connected = true;
        record.last_seen_ms = now_ms();
        if msg.capabilities != 0 {
            record.capabilities = msg.capabilities;
        }
        if let Some(description) = msg.agent_description {
            record.description = Some(description);
        }
        if let Some(health) = msg.health {
            record.health = Some(health);
        }
        if let Some(effective) = msg.effective_config {
            record.effective_config = Some(config_map_text(effective.config_map.as_ref()));
        }
        if let Some(status) = msg.remote_config_status {
            record.remote_config_status = Some(status);
        }

        let disconnected = msg.agent_disconnect.is_some();
        if disconnected {
            info!(agent = %uid, "agent disconnected");
            record.connected = false;
        }

        // The config offer — composed from the Configurations whose Selectors match this Agent
        // (ADR-0012), gated by the hash comparison, and only toward an Agent that both said
        // goodbye ≠ true and declared AcceptsRemoteConfig (capability negotiation is binding).
        let remote_config = if disconnected {
            None
        } else {
            let desired = self.configs.desired_for(record.description.as_ref());
            offer(record, desired.as_ref())
        };

        Processed {
            reply: ServerToAgent {
                instance_uid: uid.as_bytes().to_vec(),
                capabilities: SERVER_CAPABILITIES,
                flags: reply_flags,
                remote_config,
                agent_identification: identification,
                ..Default::default()
            },
            uid: Some(uid),
            disconnected,
        }
    }

    /// The unsolicited offer a WebSocket loop pushes when a Configuration changes; `None` when
    /// the Agent already runs its composed set (or nothing matches it, or it cannot accept one),
    /// so nothing redundant crosses the wire.
    pub fn offer_for(&self, uid: &InstanceUid) -> Option<ServerToAgent> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get(uid)?;
        let desired = self.configs.desired_for(record.description.as_ref());
        let remote_config = offer(record, desired.as_ref())?;
        Some(ServerToAgent {
            instance_uid: uid.as_bytes().to_vec(),
            capabilities: SERVER_CAPABILITIES,
            remote_config: Some(remote_config),
            ..Default::default()
        })
    }

    /// Marks the Agents a closing WebSocket connection carried as no longer connected. State stays:
    /// the fleet remembers what each Agent last reported.
    pub fn mark_disconnected(&self, uids: &[InstanceUid]) {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        for uid in uids {
            if let Some(record) = fleet.get_mut(uid) {
                record.connected = false;
            }
        }
    }

    /// The REST view of the fleet (`GET /api/v1/agents`).
    pub fn snapshot(&self) -> Vec<AgentView> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let mut agents: Vec<AgentView> = fleet
            .iter()
            .map(|(uid, record)| {
                let desired = self.configs.desired_for(record.description.as_ref());
                let matched = self.configs.matching_names(record.description.as_ref());
                AgentView::from_record(uid, record, desired.as_ref(), matched)
            })
            .collect();
        agents.sort_by(|a, b| a.instance_uid.cmp(&b.instance_uid));
        agents
    }
}

/// The remote-config offer for one Agent, or `None` when the hash comparison says it already has
/// it — the "no redundant reconfiguration" goal in one place. Every matching Configuration is one
/// named entry; the Managed Process does its own merging (ADR-0012).
fn offer(record: &AgentRecord, desired: Option<&DesiredConfig>) -> Option<AgentRemoteConfig> {
    let desired = desired?;
    if record.capabilities & opamp::proto::AgentCapabilities::AcceptsRemoteConfig as u64 == 0 {
        return None;
    }
    let reported = record
        .remote_config_status
        .as_ref()
        .map(|s| s.last_remote_config_hash.as_slice())
        .unwrap_or_default();
    if reported == desired.hash.as_slice() {
        return None;
    }
    Some(AgentRemoteConfig {
        config: Some(AgentConfigMap {
            config_map: desired
                .entries
                .iter()
                .map(|(name, body)| {
                    (
                        name.clone(),
                        AgentConfigFile {
                            body: body.clone().into_bytes(),
                            content_type: String::new(),
                        },
                    )
                })
                .collect(),
        }),
        config_hash: desired.hash.clone(),
    })
}

/// One Agent as the REST API and the UI see it.
#[derive(Serialize, ToSchema)]
pub struct AgentView {
    pub instance_uid: String,
    pub service_name: String,
    pub service_version: String,
    /// The reported `os.description` (e.g. "Ubuntu 24.04.2 LTS"), falling back to `os.type`.
    pub os: String,
    /// Every reported identifying attribute — what a Selector can match on (ADR-0012).
    pub identifying_attributes: BTreeMap<String, String>,
    /// Every reported non-identifying attribute — Selectors match these too.
    pub non_identifying_attributes: BTreeMap<String, String>,
    /// The Configurations currently matching this Agent, in name order.
    pub matched_configurations: Vec<String>,
    /// Hex hash of the composed configuration this Agent should run; empty when nothing matches.
    pub desired_hash: String,
    /// The Capability Set this Agent declared, as capability names from the Baseline's
    /// `AgentCapabilities` (see docs/CONFORMANCE.md).
    pub capabilities: Vec<String>,
    pub transport: String,
    pub connected: bool,
    pub healthy: bool,
    pub health_status: String,
    pub effective_config: String,
    pub remote_config_status: String,
    pub remote_config_error: String,
    pub in_sync: bool,
    pub sequence_num: u64,
    pub last_seen_ms: u64,
}

/// A declared capability bitmask as the names from the Baseline's `AgentCapabilities`. Undefined
/// bits are surfaced verbatim rather than dropped — a peer declaring them is worth seeing.
fn capability_names(mask: u64) -> Vec<String> {
    use opamp::proto::AgentCapabilities as C;
    const KNOWN: [(C, &str); 16] = [
        (C::ReportsStatus, "ReportsStatus"),
        (C::AcceptsRemoteConfig, "AcceptsRemoteConfig"),
        (C::ReportsEffectiveConfig, "ReportsEffectiveConfig"),
        (C::AcceptsPackages, "AcceptsPackages"),
        (C::ReportsPackageStatuses, "ReportsPackageStatuses"),
        (C::ReportsOwnTraces, "ReportsOwnTraces"),
        (C::ReportsOwnMetrics, "ReportsOwnMetrics"),
        (C::ReportsOwnLogs, "ReportsOwnLogs"),
        (
            C::AcceptsOpAmpConnectionSettings,
            "AcceptsOpAMPConnectionSettings",
        ),
        (
            C::AcceptsOtherConnectionSettings,
            "AcceptsOtherConnectionSettings",
        ),
        (C::AcceptsRestartCommand, "AcceptsRestartCommand"),
        (C::ReportsHealth, "ReportsHealth"),
        (C::ReportsRemoteConfig, "ReportsRemoteConfig"),
        (C::ReportsHeartbeat, "ReportsHeartbeat"),
        (C::ReportsAvailableComponents, "ReportsAvailableComponents"),
        (
            C::ReportsConnectionSettingsStatus,
            "ReportsConnectionSettingsStatus",
        ),
    ];
    let mut names = Vec::new();
    let mut undefined = mask;
    for (bit, name) in KNOWN {
        if mask & bit as u64 != 0 {
            names.push(name.to_string());
            undefined &= !(bit as u64);
        }
    }
    if undefined != 0 {
        names.push(format!("unknown bits 0x{undefined:x}"));
    }
    names
}

/// Reported attributes as the API shows them: string values as-is, other value kinds in their
/// debug form — the view is for reading, the wire keeps the typed original.
fn attr_map(attributes: &[KeyValue]) -> BTreeMap<String, String> {
    attributes
        .iter()
        .filter_map(|kv| {
            let value = kv.value.as_ref()?.value.as_ref()?;
            let text = match value {
                any_value::Value::StringValue(s) => s.clone(),
                other => format!("{other:?}"),
            };
            Some((kv.key.clone(), text))
        })
        .collect()
}

impl AgentView {
    fn from_record(
        uid: &InstanceUid,
        record: &AgentRecord,
        desired: Option<&DesiredConfig>,
        matched_configurations: Vec<String>,
    ) -> Self {
        let (identifying, non_identifying) = match &record.description {
            Some(d) => (
                attr_map(&d.identifying_attributes),
                attr_map(&d.non_identifying_attributes),
            ),
            None => (BTreeMap::new(), BTreeMap::new()),
        };
        let lookup = |map: &BTreeMap<String, String>, key: &str| -> String {
            map.get(key).cloned().unwrap_or_default()
        };
        let status = record.remote_config_status.as_ref();
        let status_name = match status.map(|s| s.status) {
            Some(s) if s == RemoteConfigStatuses::Applied as i32 => "APPLIED",
            Some(s) if s == RemoteConfigStatuses::Applying as i32 => "APPLYING",
            Some(s) if s == RemoteConfigStatuses::Failed as i32 => "FAILED",
            _ => "UNSET",
        };
        // In sync means: runs exactly the composed set — trivially true when nothing matches,
        // since an unmatched Agent is deliberately left alone (goal 9).
        let in_sync = match desired {
            None => true,
            Some(d) => {
                status.map(|s| s.last_remote_config_hash.as_slice()) == Some(d.hash.as_slice())
            }
        };
        AgentView {
            instance_uid: uid.to_string(),
            service_name: lookup(&identifying, "service.name"),
            service_version: lookup(&identifying, "service.version"),
            os: match lookup(&non_identifying, "os.description") {
                description if !description.is_empty() => description,
                _ => lookup(&non_identifying, "os.type"),
            },
            identifying_attributes: identifying,
            non_identifying_attributes: non_identifying,
            matched_configurations,
            desired_hash: desired.map(|d| hex::encode(&d.hash)).unwrap_or_default(),
            capabilities: capability_names(record.capabilities),
            transport: record.transport.as_str().to_string(),
            connected: record.connected,
            healthy: record.health.as_ref().map(|h| h.healthy).unwrap_or(false),
            health_status: record
                .health
                .as_ref()
                .map(|h| h.status.clone())
                .unwrap_or_default(),
            effective_config: record.effective_config.clone().unwrap_or_default(),
            remote_config_status: status_name.to_string(),
            remote_config_error: status.map(|s| s.error_message.clone()).unwrap_or_default(),
            in_sync,
            sequence_num: record.sequence_num,
            last_seen_ms: record.last_seen_ms,
        }
    }
}

/// The `ServerToAgent` for a report the Server cannot make sense of.
pub fn bad_request(message: &str) -> ServerToAgent {
    ServerToAgent {
        capabilities: SERVER_CAPABILITIES,
        error_response: Some(ServerErrorResponse {
            r#type: ServerErrorResponseType::BadRequest as i32,
            error_message: message.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Renders a reported config map for the operator: single unnamed entry as-is, named entries with
/// a `# <name>` heading.
fn config_map_text(map: Option<&AgentConfigMap>) -> String {
    let Some(map) = map else {
        return String::new();
    };
    let mut entries: Vec<_> = map.config_map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(name, file)| {
            let body = String::from_utf8_lossy(&file.body);
            if name.is_empty() {
                body.into_owned()
            } else {
                format!("# {name}\n{body}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_names_decode_known_bits_and_surface_undefined_ones() {
        use opamp::proto::AgentCapabilities as C;
        assert!(capability_names(0).is_empty());
        assert_eq!(
            capability_names(C::ReportsStatus as u64 | C::ReportsHealth as u64),
            ["ReportsStatus", "ReportsHealth"]
        );
        let with_undefined = capability_names(C::ReportsStatus as u64 | 1 << 60);
        assert_eq!(
            with_undefined,
            ["ReportsStatus", "unknown bits 0x1000000000000000"]
        );
    }
}
