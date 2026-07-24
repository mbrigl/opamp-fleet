//! In-memory fleet state and the OpAMP control loop, keyed by Instance UID — never by the
//! connection that carried a message (ADR-0009).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use opamp::attributes;
use opamp::proto::{
    any_value, AgentConfigMap, AgentConfigObject, AgentDescription, AgentIdentification,
    AgentRemoteConfig, AgentToServer, AgentToServerFlags, AvailableComponents, ComponentHealth,
};
use opamp::uid::InstanceUid;
use serde::Serialize;
use tokio::sync::watch;
use tracing::{info, warn};

/// The Capability Set this Server declares (see docs/CONFORMANCE.md).
pub const SERVER_CAPABILITIES: u64 = ServerCapabilities::AcceptsStatus as u64
    | ServerCapabilities::OffersRemoteConfig as u64
    | ServerCapabilities::AcceptsEffectiveConfig as u64;

/// Identifies one WebSocket connection for the duplicate detection the Baseline asks of the
/// Server. Never a routing key — Agents are routed by `instance_uid` alone (ADR-0009); this only
/// answers "is this identity already alive on *another* connection?".
pub type ConnId = u64;

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
    /// An operator-requested restart not yet delivered. Lives on the record, not a connection,
    /// so it reaches the Agent on its next exchange whichever transport carries it.
    pub restart_pending: bool,
    /// The Agent's available components — hash-only until the full map was demanded and arrived.
    pub available_components: Option<AvailableComponents>,
    /// The WebSocket connection currently carrying this Agent; `None` for plain HTTP, whose
    /// polling is stateless. Only the owning connection may mark the Agent disconnected, and a
    /// report from a *different* live connection is the duplicate the Baseline wants detected.
    pub owner: Option<ConnId>,
}

/// Why a restart request was refused (`POST /api/v1/agents/{uid}/restart`).
pub enum RestartError {
    /// No Agent of that identity is known.
    UnknownAgent,
    /// The Agent does not declare `AcceptsRestartCommand` — capability negotiation is binding,
    /// so the Server refuses rather than sending a command the Agent would ignore.
    NoCapability,
}

    /// No Agent of that identity is known.
    UnknownAgent,
    /// No Agent of that identity is known.
    UnknownAgent,
/// The result of processing one `AgentToServer`: the reply to send back on the same transport, and
/// what the transport layer needs to know for its own bookkeeping.
pub struct Processed {
    pub reply: ServerToAgent,
    /// The identity the Agent goes by *after* this message (it may have been reassigned).
    pub uid: Option<InstanceUid>,
    /// The Agent said goodbye; a WebSocket loop drops it from its connection-local set.
    pub disconnected: bool,
}

    /// against its own endpoint — which is the Agent plane, where the download is served
    /// (ADR-0012). It sits outside Admission (ADR-0017): the artifact's content hash and signature
    /// are what protect it, so no credential rides it.
/// WebSocket loops subscribe to.
pub struct AppState {
    fleet: Mutex<HashMap<InstanceUid, AgentRecord>>,
    push: watch::Sender<u64>,
    /// Hands every WebSocket connection its identity for the duplicate detection.
    next_conn: AtomicU64,
    /// The message size limit both transports enforce, in each direction (the Baseline's MUST).
    max_message_size: usize,
}

impl AppState {
            push: watch::channel(0).0,
            next_conn: AtomicU64::new(1),
            max_message_size: opamp::frame::DEFAULT_MAX_MESSAGE_SIZE,
    }

    /// Sets the message size limit both transports enforce (the Baseline recommends the default
    /// [`opamp::frame::DEFAULT_MAX_MESSAGE_SIZE`] and asks that it be configurable).
    #[must_use]
    pub fn with_max_message_size(mut self, limit: usize) -> Self {
        self.max_message_size = limit;
        self
    }

    /// The message size limit in force, for the transports to enforce in both directions.
    pub fn max_message_size(&self) -> usize {
        self.max_message_size
    }

    /// A fresh identity for one WebSocket connection.
    pub fn connection_id(&self) -> ConnId {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.push.subscribe()
    }

    }

        let mut fleet = self.fleet.lock().expect("fleet lock");
        self.push.send_modify(|rev| *rev += 1);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
    /// Queues a restart for one Agent (`AcceptsRestartCommand`) and wakes the WebSocket loops so
    /// a connected Agent hears it now; a polling one picks it up on its next exchange.
    pub fn request_restart(&self, uid: &InstanceUid) -> Result<(), RestartError> {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get_mut(uid).ok_or(RestartError::UnknownAgent)?;
        if record.capabilities & opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64 == 0
        {
            return Err(RestartError::NoCapability);
        }
        record.restart_pending = true;
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        info!(agent = %uid, "restart requested");
        Ok(())
    }

        let fleet = self.fleet.lock().expect("fleet lock");
        let mut fleet = self.fleet.lock().expect("fleet lock");
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
    /// The queued restart for this Agent as the Baseline's command-only message, taken exactly
    /// once — `None` when nothing is queued (or the Agent went away).
    pub fn restart_command_for(&self, uid: &InstanceUid) -> Option<ServerToAgent> {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get_mut(uid)?;
        if !record.restart_pending || !record.connected {
            return None;
        }
        record.restart_pending = false;
    }

        let mut fleet = self.fleet.lock().expect("fleet lock");
    }

        let fleet = self.fleet.lock().expect("fleet lock");
    /// The control loop for one report, shared by both transports (ADR-0012): update what we know,
    /// then answer with what the Agent still lacks — the config offer gated by the hash comparison.
    /// `conn` identifies the WebSocket connection that carried the report; `None` for plain HTTP.
    pub fn process(
        &self,
        msg: AgentToServer,
        transport: Transport,
        conn: Option<ConnId>,
    ) -> Processed {
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

        // Duplicate instance_uid detection (a Baseline SHOULD): the same identity alive on
        // *another* WebSocket connection — bad UID generators, cloned VMs — is rekeyed exactly
        // as the Baseline prescribes: mint a fresh uid and answer with AgentIdentification,
        // which the Client adopts. The newcomer starts a record of its own; the incumbent and
        // its connection stay untouched. (Stateless plain-HTTP polling offers nothing to
        // distinguish two pollers by, so detection is WebSocket-only.)
        if let Some(this_conn) = conn {
            let duplicate = fleet.get(&uid).is_some_and(|existing| {
                existing.connected && existing.owner.is_some_and(|owner| owner != this_conn)
            });
            if duplicate {
                let new_uid = InstanceUid::default();
                warn!(duplicate = %uid, new = %new_uid, "duplicate instance_uid; rekeying the newcomer");
                identification = Some(AgentIdentification {
                    new_instance_uid: new_uid.as_bytes().to_vec(),
                });
                uid = new_uid;
            }
        }

        let known = fleet.contains_key(&uid);
                uid: None,
                disconnected: false,
            };
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
                restart_pending: false,
                available_components: None,
                owner: conn,
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
        record.owner = conn;
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
        if let Some(incoming) = msg.available_components {
            // A routine hash-only update must not degrade an already-fetched full map of the
            // same hash; anything else (first sight, or a changed hash) replaces the stored value.
            let keep_stored_full = record.available_components.as_ref().is_some_and(|stored| {
                incoming.components.is_empty()
                    && !stored.components.is_empty()
                    && stored.hash == incoming.hash
            });
            if !keep_stored_full {
                record.available_components = Some(incoming);
            }
        }

        let disconnected = msg.agent_disconnect.is_some();
        if disconnected {
            info!(agent = %uid, "agent disconnected");
            record.connected = false;
            record.owner = None;
        }

        // A hash without the map is an offer to fetch: demand the full component list from an
        // Agent that declared it can report one (the flag is meaningless toward any other).
        if !disconnected
            && record.capabilities
                & opamp::proto::AgentCapabilities::ReportsAvailableComponents as u64
                != 0
            && record
                .available_components
                .as_ref()
                .is_some_and(|ac| ac.components.is_empty())
        {
            reply_flags |= ServerToAgentFlags::ReportAvailableComponents as u64;
        }

        // A queued restart goes out as the Baseline's command-only message: nothing but
        // identity, capabilities, and the command. Anything else the reply would carry —
        // an identity reassignment, a demanded full report — defers the command to the next
        // exchange instead of being combined with it.
        if record.restart_pending
            && !disconnected
            && identification.is_none()
            && reply_flags == 0
            && record.capabilities & opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64
                != 0
        {
            record.restart_pending = false;
            return Processed {
                uid: Some(uid),
                disconnected: false,
            };
        }

        let remote_config = if disconnected {
            None
        } else {
        };

        Processed {
            reply: ServerToAgent {
                instance_uid: uid.as_bytes().to_vec(),
                flags: reply_flags,
                remote_config,
                agent_identification: identification,
                ..Default::default()
            },
            uid: Some(uid),
            disconnected,
        }
    }

    pub fn offer_for(&self, uid: &InstanceUid) -> Option<ServerToAgent> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get(uid)?;
        Some(ServerToAgent {
            instance_uid: uid.as_bytes().to_vec(),
            ..Default::default()
        })
    }

        let fleet = self.fleet.lock().expect("fleet lock");
        let fleet = self.fleet.lock().expect("fleet lock");
        let mut fleet = self.fleet.lock().expect("fleet lock");
    /// Marks the Agents a closing WebSocket connection carried as no longer connected — but only
    /// those the connection still *owns*: after a rekey (or a transport switch) another live
    /// connection may legitimately carry an identity this one once saw, and a closing socket
    /// must not take it down. State stays: the fleet remembers what each Agent last reported.
    pub fn mark_disconnected(&self, uids: &[InstanceUid], conn: ConnId) {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        for uid in uids {
            if let Some(record) = fleet.get_mut(uid) {
                if record.owner == Some(conn) {
                    record.connected = false;
                    record.owner = None;
                }
            }
        }
    }

    pub fn snapshot(&self) -> Vec<AgentView> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let mut agents: Vec<AgentView> = fleet
            .iter()
            .collect();
        agents.sort_by(|a, b| a.instance_uid.cmp(&b.instance_uid));
        agents
    }
}

/// The remote-config offer for one Agent, or `None` when the hash comparison says it already has
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
                        AgentConfigObject {
        }),
        config_hash: desired.hash.clone(),
    })
}

/// The version a reader of the fleet table wants: the release, without the commit the build came
/// from (ADR-0013).
///
/// A value that is not a version is returned as it stands. `service.version` is whatever an Agent
/// puts there, and a Foreign Agent numbers itself however its own project does — trimming a string
/// this Server does not understand would be inventing a version rather than showing one.
fn display_version(reported: &str) -> String {
    opamp::version::identity(reported)
        .unwrap_or(reported)
        .to_string()
}

/// One Agent as the REST API and the UI see it.
pub struct AgentView {
    pub instance_uid: String,
    pub service_name: String,
    /// The release the Agent reports — `MAJOR.MINOR.PATCH`, with the pre-release when it is not a
    /// release build (ADR-0013). This is what belongs in a column headed "Version"; the commit the
    /// build came from is [`service_build`](Self::service_build). A reported value that is not a
    /// version at all is passed through unchanged, since a Foreign Agent numbers itself however it
    /// likes.
    pub service_version: String,
    /// Exactly what the Agent reported, commit metadata and all — the answer to "which build is on
    /// that host", which is a question a fleet exists to answer (ADR-0013).
    pub service_build: String,
    pub os: String,
    /// The Agent's available components (top-level names, sorted); empty until reported.
    pub available_components: Vec<String>,
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
            Some(d) => (
            ),
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
        // What the Agent said, and what a reader of a table wants out of it (ADR-0013).
        let service_build = lookup(&identifying, attributes::SERVICE_VERSION);
        AgentView {
            instance_uid: uid.to_string(),
            service_name: lookup(&identifying, attributes::SERVICE_NAME),
            service_instance_name: lookup(&non_identifying, attributes::SERVICE_INSTANCE_NAME),
            service_version: display_version(&service_build),
            service_build,
            os: match lookup(&non_identifying, attributes::OS_DESCRIPTION) {
                _ => lookup(&non_identifying, attributes::OS_TYPE),
            available_components: record
                .available_components
                .as_ref()
                .map(|ac| {
                    let mut names: Vec<String> = ac.components.keys().cloned().collect();
                    names.sort_unstable();
                    names
                })
                .unwrap_or_default(),
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

/// The Baseline's command-only message: identity, capabilities, and the restart — nothing else.
    ServerToAgent {
        instance_uid: uid.as_bytes().to_vec(),
        command: Some(opamp::proto::ServerToAgentCommand {
            r#type: opamp::proto::CommandType::Restart as i32,
        }),
        ..Default::default()
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

    ServerToAgent {
        capabilities: SERVER_CAPABILITIES,
        error_response: Some(ServerErrorResponse {
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
            instance_uid: uid.as_bytes().to_vec(),
            instance_uid: uid.as_bytes().to_vec(),
    /// ADR-0013: the fleet table shows the release, and the build stays reachable beside it. A
    /// Foreign Agent that numbers itself in its own way is shown as it reported.
    #[test]
    fn the_displayed_version_drops_the_commit_and_keeps_the_pre_release() {
        assert_eq!(super::display_version("0.1.1+799e36a"), "0.1.1");
        assert_eq!(super::display_version("0.1.1-dev+799e36a"), "0.1.1-dev");
        assert_eq!(super::display_version("0.1.1"), "0.1.1");
        // Not a version this Server understands — shown rather than trimmed into something else.
        assert_eq!(super::display_version("v2.9-nightly"), "v2.9-nightly");
        assert_eq!(super::display_version(""), "");
    }

