//! In-memory fleet state and the OpAMP control loop, keyed by Instance UID — never by the
//! connection that carried a message (ADR-0003).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use opamp::proto::{
    AgentConfigFile, AgentConfigMap, AgentDescription, AgentIdentification, AgentRemoteConfig,
    AgentToServer, AgentToServerFlags, ComponentHealth, RemoteConfigStatus, RemoteConfigStatuses,
    ServerCapabilities, ServerErrorResponse, ServerErrorResponseType, ServerToAgent,
    ServerToAgentFlags,
};
use opamp::uid::InstanceUid;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tracing::{info, warn};

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

/// The configuration the Server wants the fleet to run, with its identity — the hash that gates
/// every push (specification: no redundant reconfiguration).
#[derive(Clone)]
pub struct DesiredConfig {
    pub body: String,
    pub hash: Vec<u8>,
}

impl DesiredConfig {
    pub fn new(body: String) -> Self {
        let hash = Sha256::digest(body.as_bytes()).to_vec();
        DesiredConfig { body, hash }
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

/// Shared state behind every handler: the fleet, the desired configuration, and the push channel
/// WebSocket loops subscribe to.
pub struct AppState {
    fleet: Mutex<HashMap<InstanceUid, AgentRecord>>,
    desired: RwLock<Option<DesiredConfig>>,
    fleet_config_file: PathBuf,
    push: watch::Sender<u64>,
}

impl AppState {
    /// Builds the state, restoring the desired configuration from disk when one was persisted.
    pub fn new(fleet_config_file: PathBuf) -> Self {
        let desired = match std::fs::read_to_string(&fleet_config_file) {
            Ok(body) if !body.trim().is_empty() => {
                info!(file = %fleet_config_file.display(), "restored the fleet configuration");
                Some(DesiredConfig::new(body))
            }
            _ => None,
        };
        AppState {
            fleet: Mutex::new(HashMap::new()),
            desired: RwLock::new(desired),
            fleet_config_file,
            push: watch::channel(0).0,
        }
    }

    /// A receiver that fires whenever the desired configuration changes; WebSocket loops use it to
    /// push offers without waiting for the Agent to speak.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.push.subscribe()
    }

    pub fn desired_config(&self) -> Option<DesiredConfig> {
        self.desired.read().expect("desired lock").clone()
    }

    /// Replaces the desired configuration, persists it, and wakes every WebSocket loop.
    pub fn set_desired_config(&self, body: String) -> Result<DesiredConfig, String> {
        let config = DesiredConfig::new(body);
        std::fs::write(&self.fleet_config_file, &config.body)
            .map_err(|e| format!("cannot persist {}: {e}", self.fleet_config_file.display()))?;
        *self.desired.write().expect("desired lock") = Some(config.clone());
        self.push.send_modify(|rev| *rev += 1);
        info!(hash = %hex::encode(&config.hash), "fleet configuration updated");
        Ok(config)
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

        // The config offer — gated by the hash comparison, and only toward an Agent that both said
        // goodbye ≠ true and declared AcceptsRemoteConfig (capability negotiation is binding).
        let remote_config = if disconnected {
            None
        } else {
            offer(record, self.desired_config().as_ref())
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

    /// The unsolicited offer a WebSocket loop pushes when the desired configuration changes; `None`
    /// when the Agent already runs it (or cannot accept one), so nothing redundant crosses the wire.
    pub fn offer_for(&self, uid: &InstanceUid) -> Option<ServerToAgent> {
        let desired = self.desired_config();
        let fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get(uid)?;
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

    /// The REST view of the fleet (`GET /api/agents`).
    pub fn snapshot(&self) -> Vec<AgentView> {
        let desired = self.desired_config();
        let fleet = self.fleet.lock().expect("fleet lock");
        let mut agents: Vec<AgentView> = fleet
            .iter()
            .map(|(uid, record)| AgentView::from_record(uid, record, desired.as_ref()))
            .collect();
        agents.sort_by(|a, b| a.instance_uid.cmp(&b.instance_uid));
        agents
    }
}

/// The remote-config offer for one Agent, or `None` when the hash comparison says it already has
/// it — the "no redundant reconfiguration" goal in one place.
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
            config_map: HashMap::from([(
                String::new(),
                AgentConfigFile {
                    body: desired.body.clone().into_bytes(),
                    content_type: String::new(),
                },
            )]),
        }),
        config_hash: desired.hash.clone(),
    })
}

/// One Agent as the REST API and the UI see it.
#[derive(Serialize)]
pub struct AgentView {
    pub instance_uid: String,
    pub service_name: String,
    pub service_version: String,
    pub os: String,
    pub transport: &'static str,
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

impl AgentView {
    fn from_record(
        uid: &InstanceUid,
        record: &AgentRecord,
        desired: Option<&DesiredConfig>,
    ) -> Self {
        let attr = |list: &[opamp::proto::KeyValue], key: &str| -> String {
            list.iter()
                .find(|kv| kv.key == key)
                .and_then(|kv| kv.value.as_ref())
                .and_then(|v| v.value.as_ref())
                .map(|v| match v {
                    opamp::proto::any_value::Value::StringValue(s) => s.clone(),
                    other => format!("{other:?}"),
                })
                .unwrap_or_default()
        };
        let (name, version, os) = match &record.description {
            Some(d) => (
                attr(&d.identifying_attributes, "service.name"),
                attr(&d.identifying_attributes, "service.version"),
                attr(&d.non_identifying_attributes, "os.type"),
            ),
            None => (String::new(), String::new(), String::new()),
        };
        let status = record.remote_config_status.as_ref();
        let status_name = match status.map(|s| s.status) {
            Some(s) if s == RemoteConfigStatuses::Applied as i32 => "APPLIED",
            Some(s) if s == RemoteConfigStatuses::Applying as i32 => "APPLYING",
            Some(s) if s == RemoteConfigStatuses::Failed as i32 => "FAILED",
            _ => "UNSET",
        };
        let in_sync = match desired {
            None => true,
            Some(d) => {
                status.map(|s| s.last_remote_config_hash.as_slice()) == Some(d.hash.as_slice())
            }
        };
        AgentView {
            instance_uid: uid.to_string(),
            service_name: name,
            service_version: version,
            os,
            transport: record.transport.as_str(),
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
