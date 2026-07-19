//! What the server remembers about the agents connected to it.
//!
//! Remembering matters: an `AgentToServer` message carries only the fields that *changed* since the
//! agent's previous message. A message without a remote config status does not mean "this agent has
//! no configuration" — it means "nothing new to say about it". Judging each message on its own would
//! make the server re-send the configuration on every heartbeat, and the supervisor would restart its
//! collector for nothing. This is the delta rule, and every field below is overwritten only when the
//! message actually carries it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::watch;

use crate::proto::{
    any_value, AgentCapabilities, AgentDescription, AgentRemoteConfig, AgentToServer,
    AvailableComponents, ComponentHealth, PackageStatusEnum, PackageStatuses, RemoteConfigStatus,
    RemoteConfigStatuses,
};

/// The empty-string key an agent's top-level configuration file is filed under (see
/// [`crate::config`]).
const MAIN_CONFIG_KEY: &str = "";

/// What the server remembers about one connected agent.
#[derive(Default)]
struct Agent {
    uid: Vec<u8>,
    /// The hash of the configuration the agent last reported holding, or empty until it reports one.
    config_hash: Vec<u8>,
    description: Option<AgentDescription>,
    health: Option<ComponentHealth>,
    config_status: Option<RemoteConfigStatus>,
    /// The capabilities the agent last declared (an `AgentCapabilities` bitmask). Always sent by a
    /// conforming agent, so it is folded whenever non-zero.
    capabilities: u64,
    /// The components the agent last reported it can run (`ReportsAvailableComponents`), or `None` if it
    /// has not reported them.
    available_components: Option<AvailableComponents>,
    /// The package statuses the agent last reported (`ReportsPackageStatuses`, ADR-0018), or `None` if it
    /// has not reported any. Carries the agent's `server_provided_all_packages_hash`, which drives the
    /// Server's package-offer comparison.
    package_statuses: Option<PackageStatuses>,
    effective_config: String,
    /// When the last message from this agent arrived, or `None` before its first message.
    last_seen: Option<Instant>,
    /// The `sequence_num` of the agent's previous message, used to detect a gap.
    last_seq: Option<u64>,
}

impl Agent {
    /// Folds one message into the remembered state and reports whether a `sequence_num` gap was
    /// detected — i.e. the number did not advance by exactly one, so the server has missed a message
    /// and must ask the agent to report its full state.
    fn fold(&mut self, msg: &AgentToServer) -> bool {
        self.last_seen = Some(Instant::now());

        if !msg.instance_uid.is_empty() {
            self.uid = msg.instance_uid.clone();
        }
        // The delta rule: absence means "unchanged", so only a message that carries a field updates it.
        if let Some(d) = &msg.agent_description {
            self.description = Some(d.clone());
        }
        if let Some(h) = &msg.health {
            self.health = Some(h.clone());
        }
        if let Some(st) = &msg.remote_config_status {
            self.config_hash = st.last_remote_config_hash.clone();
            self.config_status = Some(st.clone());
        }
        // Capabilities are a required field a conforming agent sets on every message; a zero value means
        // "not carried" here, so — like the other fields — it leaves the remembered value unchanged.
        if msg.capabilities != 0 {
            self.capabilities = msg.capabilities;
        }
        if let Some(ac) = &msg.available_components {
            self.available_components = Some(ac.clone());
        }
        if let Some(ps) = &msg.package_statuses {
            self.package_statuses = Some(ps.clone());
        }
        if let Some(ec) = &msg.effective_config {
            if ec.config_map.is_some() {
                self.effective_config = effective_config_body(ec);
            }
        }

        // The first message from an agent establishes the baseline; only a later message can gap.
        let gap = matches!(self.last_seq, Some(prev) if msg.sequence_num != prev + 1);
        self.last_seq = Some(msg.sequence_num);
        gap
    }

    /// Projects the remembered state into the view the fleet page renders. `want` is the
    /// configuration the server currently distributes, against which the agent's sync is judged.
    fn state(&self, want: Option<&AgentRemoteConfig>) -> AgentState {
        let mut attributes: Vec<(String, String)> = Vec::new();
        let mut context: Vec<(String, String)> = Vec::new();
        if let Some(d) = &self.description {
            for kv in &d.identifying_attributes {
                attributes.push((kv.key.clone(), string_value(kv.value.as_ref())));
            }
            for kv in &d.non_identifying_attributes {
                context.push((kv.key.clone(), string_value(kv.value.as_ref())));
            }
            // Order both so the view does not reshuffle between refreshes.
            attributes.sort_by(|a, b| a.0.cmp(&b.0));
            context.sort_by(|a, b| a.0.cmp(&b.0));
        }

        let (config_status, config_error) = match &self.config_status {
            Some(st) => (status_name(st.status), st.error_message.clone()),
            None => (
                status_name(RemoteConfigStatuses::Unset as i32),
                String::new(),
            ),
        };

        AgentState {
            uid: hex::encode(&self.uid),
            attributes,
            context,
            healthy: self.health.as_ref().is_some_and(|h| h.healthy),
            health_reported: self.health.is_some(),
            health_status: self
                .health
                .as_ref()
                .map(|h| h.status.clone())
                .unwrap_or_default(),
            health_error: self
                .health
                .as_ref()
                .map(|h| h.last_error.clone())
                .unwrap_or_default(),
            config_status,
            config_error,
            config_hash: short_hash(&self.config_hash),
            in_sync: want.is_some_and(|w| w.config_hash == self.config_hash),
            effective_config: self.effective_config.clone(),
            capabilities: capability_names(self.capabilities),
            accepts_restart: self.capabilities & AgentCapabilities::AcceptsRestartCommand as u64
                != 0,
            components: self
                .available_components
                .as_ref()
                .map(component_groups)
                .unwrap_or_default(),
            components_hash: self
                .available_components
                .as_ref()
                .map(|ac| short_hash(&ac.hash))
                .unwrap_or_default(),
            last_seen: humanize_since(self.last_seen),
        }
    }

    /// Projects the remembered state into the machine-friendly shape the REST API serves (ADR-0007):
    /// full hashes (not shortened), numeric timestamps (not "3s ago"), and raw status strings.
    fn api_state(&self, want: Option<&AgentRemoteConfig>) -> AgentDto {
        let attributes = |kvs: &[crate::proto::KeyValue]| {
            let mut out: Vec<AttributeDto> = kvs
                .iter()
                .map(|kv| AttributeDto {
                    key: kv.key.clone(),
                    value: string_value(kv.value.as_ref()),
                })
                .collect();
            out.sort_by(|a, b| a.key.cmp(&b.key));
            out
        };
        let (identity, context) = match &self.description {
            Some(d) => (
                attributes(&d.identifying_attributes),
                attributes(&d.non_identifying_attributes),
            ),
            None => (Vec::new(), Vec::new()),
        };

        let elapsed = self.last_seen.map(|t| t.elapsed());
        AgentDto {
            uid: hex::encode(&self.uid),
            identity,
            context,
            healthy: self.health.as_ref().is_some_and(|h| h.healthy),
            health_reported: self.health.is_some(),
            health_status: self
                .health
                .as_ref()
                .map(|h| h.status.clone())
                .unwrap_or_default(),
            health_error: self
                .health
                .as_ref()
                .map(|h| h.last_error.clone())
                .unwrap_or_default(),
            config_status: match &self.config_status {
                Some(st) => status_name(st.status),
                None => status_name(RemoteConfigStatuses::Unset as i32),
            },
            config_error: self
                .config_status
                .as_ref()
                .map(|st| st.error_message.clone())
                .unwrap_or_default(),
            config_hash: hex::encode(&self.config_hash),
            in_sync: want.is_some_and(|w| w.config_hash == self.config_hash),
            effective_config: self.effective_config.clone(),
            capabilities: capability_names(self.capabilities),
            accepts_restart: self.capabilities & AgentCapabilities::AcceptsRestartCommand as u64
                != 0,
            available_components: self.available_components.as_ref().map(|ac| {
                AvailableComponentsDto {
                    hash: hex::encode(&ac.hash),
                    components: component_groups(ac)
                        .into_iter()
                        .map(|(name, components)| ComponentGroupDto { name, components })
                        .collect(),
                }
            }),
            packages: self.package_statuses.as_ref().map(package_statuses_dto),
            last_seen_seconds_ago: elapsed.map(|d| d.as_secs()),
            last_seen_unix: elapsed.and_then(|d| {
                SystemTime::now()
                    .checked_sub(d)
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|since| since.as_secs())
            }),
        }
    }
}

/// One agent's last reported state, as the fleet page renders it. All fields are already
/// display-ready: the template only escapes and lays them out.
pub struct AgentState {
    pub uid: String,
    /// The agent's identifying attributes (who it is).
    pub attributes: Vec<(String, String)>,
    /// The agent's non-identifying attributes (where it runs, which supervisor manages it, …).
    pub context: Vec<(String, String)>,
    pub healthy: bool,
    /// Whether the agent reported any health at all. `false` means "not reported yet", which is not
    /// the same as unhealthy — the view distinguishes the two.
    pub health_reported: bool,
    pub health_status: String,
    /// The agent's self-reported reason when unhealthy (OpAMP `ComponentHealth.last_error`).
    pub health_error: String,
    /// The OpAMP status name (`APPLIED`, `FAILED`, `APPLYING`, `UNSET`) the agent reported.
    pub config_status: String,
    pub config_error: String,
    pub config_hash: String,
    /// Whether the agent holds the configuration the server currently distributes.
    pub in_sync: bool,
    pub effective_config: String,
    /// The capabilities the agent declares, as OpAMP names (`ReportsHealth`, `AcceptsRemoteConfig`, …).
    pub capabilities: Vec<String>,
    /// Whether the agent declares `AcceptsRestartCommand`, so the view offers a restart button (ADR-0011).
    pub accepts_restart: bool,
    /// The components the agent reports it can run, grouped by kind (`receivers` → `otlp`, …). Empty
    /// when the agent has not reported any.
    pub components: Vec<(String, Vec<String>)>,
    /// A short hash of the reported available components, empty when none were reported.
    pub components_hash: String,
    pub last_seen: String,
}

/// One agent as the REST API serializes it (ADR-0007): a stable, machine-friendly JSON shape —
/// distinct from [`AgentState`], which is formatted for the HTML page.
#[derive(Serialize)]
pub struct AgentDto {
    /// The agent's instance UID, full hex.
    pub uid: String,
    /// Identifying attributes (who it is), sorted by key.
    pub identity: Vec<AttributeDto>,
    /// Non-identifying attributes (where it runs, which supervisor manages it), sorted by key.
    pub context: Vec<AttributeDto>,
    pub healthy: bool,
    /// Whether the agent reported any health at all (`false` = not reported, not "unhealthy").
    pub health_reported: bool,
    pub health_status: String,
    /// The agent's self-reported reason when unhealthy (OpAMP `ComponentHealth.last_error`).
    pub health_error: String,
    /// The OpAMP status name (`APPLIED`, `FAILED`, `APPLYING`, `UNSET`).
    pub config_status: String,
    pub config_error: String,
    /// The hash of the configuration the agent holds, full hex.
    pub config_hash: String,
    /// Whether the agent holds the configuration the server currently distributes.
    pub in_sync: bool,
    pub effective_config: String,
    /// The capabilities the agent declares, as OpAMP names (`ReportsHealth`, `AcceptsRemoteConfig`, …).
    pub capabilities: Vec<String>,
    /// Whether the agent declares `AcceptsRestartCommand` (ADR-0011).
    pub accepts_restart: bool,
    /// The components the agent reports it can run, or `null` when it has not reported them.
    pub available_components: Option<AvailableComponentsDto>,
    /// The packages the agent reports it has or is installing, or `null` when it reports none (ADR-0018).
    pub packages: Option<PackageStatusesDto>,
    /// Seconds since the agent was last heard from, or `null` before its first message.
    pub last_seen_seconds_ago: Option<u64>,
    /// Absolute wall-clock time the agent was last heard from, Unix seconds, or `null`.
    pub last_seen_unix: Option<u64>,
}

/// A key/value attribute in an [`AgentDto`].
#[derive(Serialize)]
pub struct AttributeDto {
    pub key: String,
    pub value: String,
}

/// The components an agent reports it can run, as the REST API serializes them (ADR-0007).
#[derive(Serialize)]
pub struct AvailableComponentsDto {
    /// The agent-calculated hash of its available components, full hex.
    pub hash: String,
    /// The component groups (`receivers`, `processors`, …), each with its member component names.
    pub components: Vec<ComponentGroupDto>,
}

/// One group of available components (e.g. all `receivers`) in an [`AvailableComponentsDto`].
#[derive(Serialize)]
pub struct ComponentGroupDto {
    pub name: String,
    pub components: Vec<String>,
}

/// The package statuses an agent reports, as the REST API serializes them (ADR-0018).
#[derive(Serialize)]
pub struct PackageStatusesDto {
    /// The aggregate hash of the package set the agent last received, full hex.
    pub all_packages_hash: String,
    /// Each package's status, ordered by name so the view is stable.
    pub packages: Vec<PackageStatusDto>,
}

/// One package's reported status in a [`PackageStatusesDto`].
#[derive(Serialize)]
pub struct PackageStatusDto {
    pub name: String,
    /// The version the agent currently has installed (empty if none).
    pub agent_has_version: String,
    /// The version the Server offered (empty if this package was installed locally, not offered).
    pub server_offered_version: String,
    /// The status name: `Installed`, `Installing`, `Downloading`, `InstallPending`, or `InstallFailed`.
    pub status: String,
    /// The failure reason when the status is `InstallFailed`, else empty.
    pub error: String,
}

/// Projects the reported `PackageStatuses` into the API DTO, ordered by package name.
fn package_statuses_dto(ps: &PackageStatuses) -> PackageStatusesDto {
    let mut packages: Vec<PackageStatusDto> = ps
        .packages
        .values()
        .map(|p| PackageStatusDto {
            name: p.name.clone(),
            agent_has_version: p.agent_has_version.clone(),
            server_offered_version: p.server_offered_version.clone(),
            status: package_status_name(p.status),
            error: p.error_message.clone(),
        })
        .collect();
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    PackageStatusesDto {
        all_packages_hash: hex::encode(&ps.server_provided_all_packages_hash),
        packages,
    }
}

/// Renders a `PackageStatusEnum` discriminant as its bare OpAMP name (`Installed`, `InstallFailed`, …).
fn package_status_name(status: i32) -> String {
    let name = PackageStatusEnum::try_from(status)
        .unwrap_or(PackageStatusEnum::InstallPending)
        .as_str_name();
    name.strip_prefix("PackageStatusEnum_")
        .unwrap_or(name)
        .to_string()
}

/// The connected agents, keyed by an opaque per-connection id. One connection is one agent; the id is
/// the server's handle on the connection, and the agent's own instance UID lives inside the state.
pub struct Fleet {
    agents: Mutex<HashMap<u64, Agent>>,
    /// Bumped on every fleet change — a connect, a report, a disconnect — so REST clients can be pushed
    /// updates over SSE instead of polling (ADR-0007).
    changed: watch::Sender<u64>,
}

impl Fleet {
    pub fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            agents: Mutex::new(HashMap::new()),
            changed,
        }
    }

    /// Signals watchers (the SSE endpoint) that the fleet changed.
    fn notify_changed(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// A receiver that wakes whenever the fleet changes, for the SSE stream (ADR-0007).
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Registers a freshly accepted connection.
    pub fn connect(&self, id: u64) {
        self.agents
            .lock()
            .expect("fleet lock poisoned")
            .insert(id, Agent::default());
        self.notify_changed();
    }

    /// Forgets a connection and returns the disconnected agent's instance UID (for logging), if it
    /// had reported one.
    pub fn disconnect(&self, id: u64) -> Option<String> {
        let removed = self.agents.lock().expect("fleet lock poisoned").remove(&id);
        if removed.is_some() {
            self.notify_changed();
        }
        let agent = removed?;
        (!agent.uid.is_empty()).then(|| short_hash(&agent.uid))
    }

    /// Folds a message into the connection's remembered state, returning what the response needs: the
    /// hash of the configuration the agent now holds, and whether a `sequence_num` gap was detected.
    pub fn fold(&self, id: u64, msg: &AgentToServer) -> Folded {
        let folded = {
            let mut agents = self.agents.lock().expect("fleet lock poisoned");
            let agent = agents.entry(id).or_default();
            let gap = agent.fold(msg);
            Folded {
                config_hash: agent.config_hash.clone(),
                report_full_state: gap,
                package_all_hash: agent
                    .package_statuses
                    .as_ref()
                    .map(|ps| ps.server_provided_all_packages_hash.clone())
                    .unwrap_or_default(),
            }
        };
        self.notify_changed();
        folded
    }

    /// Whether some connected agent has reported the given instance UID — the guard for a targeted
    /// restart, so the API can answer `404` when no such agent is connected (ADR-0011).
    pub fn is_connected(&self, uid: &[u8]) -> bool {
        self.agents
            .lock()
            .expect("fleet lock poisoned")
            .values()
            .any(|a| a.uid == uid)
    }

    /// The instance UID of the agent behind a connection, once it has reported one. Needed to address
    /// a pushed configuration to the right agent.
    pub fn uid_of(&self, id: u64) -> Option<Vec<u8>> {
        let agents = self.agents.lock().expect("fleet lock poisoned");
        agents
            .get(&id)
            .filter(|a| !a.uid.is_empty())
            .map(|a| a.uid.clone())
    }

    /// The last reported state of every connected agent, ordered by instance UID so the view does not
    /// reshuffle between refreshes. `want` is the configuration currently distributed.
    pub fn snapshot(&self, want: Option<&AgentRemoteConfig>) -> Vec<AgentState> {
        let agents = self.agents.lock().expect("fleet lock poisoned");
        let mut states: Vec<AgentState> = agents.values().map(|a| a.state(want)).collect();
        states.sort_by(|a, b| a.uid.cmp(&b.uid));
        states
    }

    /// Like [`Fleet::snapshot`], but in the machine-friendly shape the REST API serves (ADR-0007).
    pub fn api_snapshot(&self, want: Option<&AgentRemoteConfig>) -> Vec<AgentDto> {
        let agents = self.agents.lock().expect("fleet lock poisoned");
        let mut states: Vec<AgentDto> = agents.values().map(|a| a.api_state(want)).collect();
        states.sort_by(|a, b| a.uid.cmp(&b.uid));
        states
    }
}

impl Default for Fleet {
    fn default() -> Self {
        Self::new()
    }
}

/// What [`Fleet::fold`] hands back to the connection loop.
pub struct Folded {
    /// The hash of the configuration the agent last reported holding.
    pub config_hash: Vec<u8>,
    /// Whether the server missed a message and must set the `ReportFullState` flag in its response.
    pub report_full_state: bool,
    /// The aggregate `all_packages_hash` the agent last reported, empty if it reports no packages —
    /// drives the Server's package-offer comparison (ADR-0018).
    pub package_all_hash: Vec<u8>,
}

/// Flattens the config map an agent reports into something displayable. The agent may report several
/// named files; the collector's own configuration is the one filed under the main config key, so it
/// is preferred and the rest are ignored rather than concatenated into nonsense.
fn effective_config_body(ec: &crate::proto::EffectiveConfig) -> String {
    let Some(map) = &ec.config_map else {
        return String::new();
    };
    if let Some(f) = map.config_map.get(MAIN_CONFIG_KEY) {
        return String::from_utf8_lossy(&f.body).into_owned();
    }
    let mut names: Vec<&String> = map.config_map.keys().collect();
    names.sort();
    let mut out = String::new();
    for name in names {
        let body = String::from_utf8_lossy(&map.config_map[name].body);
        out.push_str(&format!("# {name}\n{body}\n"));
    }
    out
}

/// Extracts an attribute's string value, defaulting to empty for non-string or absent values — agent
/// identifying attributes are string-valued in practice.
fn string_value(value: Option<&crate::proto::AnyValue>) -> String {
    match value.and_then(|v| v.value.as_ref()) {
        Some(any_value::Value::StringValue(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Renders a `RemoteConfigStatuses` discriminant as its bare OpAMP name (`APPLIED`, `FAILED`, …). An
/// unknown discriminant — a status from a newer protocol version — shows as `UNSET` rather than a
/// number.
fn status_name(status: i32) -> String {
    let name = RemoteConfigStatuses::try_from(status)
        .unwrap_or(RemoteConfigStatuses::Unset)
        .as_str_name();
    name.strip_prefix("RemoteConfigStatuses_")
        .unwrap_or(name)
        .to_string()
}

/// Decodes an `AgentCapabilities` bitmask into the set OpAMP capability names, sorted by bit so the
/// order is stable. A bit the server does not know (from a newer protocol) is shown as its hex value
/// rather than dropped.
fn capability_names(capabilities: u64) -> Vec<String> {
    (0..u64::BITS)
        .map(|bit| 1u64 << bit)
        .filter(|flag| capabilities & flag != 0)
        .map(|flag| {
            i32::try_from(flag)
                .ok()
                .and_then(|value| AgentCapabilities::try_from(value).ok())
                .map(|cap| {
                    let name = cap.as_str_name();
                    name.strip_prefix("AgentCapabilities_")
                        .unwrap_or(name)
                        .to_string()
                })
                .unwrap_or_else(|| format!("0x{flag:x}"))
        })
        .collect()
}

/// Projects an agent's reported available components into `(group, member names)` pairs, both sorted, so
/// the fleet view and the API present a stable, readable list (`receivers` → `otlp`, `hostmetrics`, …).
fn component_groups(components: &AvailableComponents) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = components
        .components
        .iter()
        .map(|(group, details)| {
            let mut members: Vec<String> = details.sub_component_map.keys().cloned().collect();
            members.sort();
            (group.clone(), members)
        })
        .collect();
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    groups
}

/// Renders a hash or instance UID for humans; the full value is noise in a table cell or a log line.
fn short_hash(bytes: &[u8]) -> String {
    const N: usize = 6;
    hex::encode(&bytes[..bytes.len().min(N)])
}

/// Renders how long ago an agent was last heard from.
fn humanize_since(last_seen: Option<Instant>) -> String {
    let Some(t) = last_seen else {
        return "never".to_string();
    };
    let d = t.elapsed();
    if d < Duration::from_secs(1) {
        return "just now".to_string();
    }
    let secs = d.as_secs();
    let text = if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    };
    format!("{text} ago")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{
        AgentConfigFile, AgentConfigMap, AnyValue, ComponentDetails, EffectiveConfig, KeyValue,
    };

    fn kv(key: &str, val: &str) -> KeyValue {
        KeyValue {
            key: key.to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(val.to_string())),
            }),
        }
    }

    #[test]
    fn delta_rule_keeps_unreported_fields() {
        let mut agent = Agent::default();
        agent.fold(&AgentToServer {
            instance_uid: vec![0xaa; 16],
            sequence_num: 1,
            health: Some(ComponentHealth {
                healthy: true,
                status: "ok".into(),
                ..Default::default()
            }),
            ..Default::default()
        });
        // A later heartbeat carries no health; the remembered health must survive.
        agent.fold(&AgentToServer {
            instance_uid: vec![0xaa; 16],
            sequence_num: 2,
            ..Default::default()
        });

        let st = agent.state(None);
        assert!(st.healthy);
        assert_eq!(st.health_status, "ok");
    }

    #[test]
    fn sequence_gap_is_detected_but_not_on_the_first_message() {
        let mut agent = Agent::default();
        assert!(
            !agent.fold(&AgentToServer {
                sequence_num: 5,
                ..Default::default()
            }),
            "baseline"
        );
        assert!(
            !agent.fold(&AgentToServer {
                sequence_num: 6,
                ..Default::default()
            }),
            "contiguous"
        );
        assert!(
            agent.fold(&AgentToServer {
                sequence_num: 8,
                ..Default::default()
            }),
            "gap at 7"
        );
        assert!(
            !agent.fold(&AgentToServer {
                sequence_num: 9,
                ..Default::default()
            }),
            "back in step"
        );
    }

    #[test]
    fn config_status_and_sync_track_the_reported_hash() {
        let want = AgentRemoteConfig {
            config_hash: vec![1, 2, 3],
            ..Default::default()
        };

        let mut agent = Agent::default();
        agent.fold(&AgentToServer {
            sequence_num: 1,
            remote_config_status: Some(RemoteConfigStatus {
                last_remote_config_hash: vec![1, 2, 3],
                status: RemoteConfigStatuses::Applied as i32,
                error_message: String::new(),
            }),
            ..Default::default()
        });
        let st = agent.state(Some(&want));
        assert_eq!(st.config_status, "APPLIED");
        assert!(st.in_sync);

        // The agent now reports a different hash: out of sync with what the server distributes.
        agent.fold(&AgentToServer {
            sequence_num: 2,
            remote_config_status: Some(RemoteConfigStatus {
                last_remote_config_hash: vec![9, 9],
                status: RemoteConfigStatuses::Applied as i32,
                error_message: String::new(),
            }),
            ..Default::default()
        });
        assert!(!agent.state(Some(&want)).in_sync);
    }

    #[test]
    fn failed_status_carries_the_error() {
        let mut agent = Agent::default();
        agent.fold(&AgentToServer {
            sequence_num: 1,
            remote_config_status: Some(RemoteConfigStatus {
                last_remote_config_hash: vec![7],
                status: RemoteConfigStatuses::Failed as i32,
                error_message: "cannot load config".into(),
            }),
            ..Default::default()
        });
        let st = agent.state(None);
        assert_eq!(st.config_status, "FAILED");
        assert_eq!(st.config_error, "cannot load config");
    }

    #[test]
    fn health_distinguishes_absent_from_unhealthy_and_carries_the_reason() {
        // No health reported yet: this is "not reported", not "unhealthy".
        let mut agent = Agent::default();
        agent.fold(&AgentToServer {
            sequence_num: 1,
            ..Default::default()
        });
        let st = agent.state(None);
        assert!(!st.health_reported, "no health message means not reported");
        assert!(!st.healthy);
        assert!(st.health_error.is_empty());

        // Reported unhealthy, with the agent's own reason (ComponentHealth.last_error).
        agent.fold(&AgentToServer {
            sequence_num: 2,
            health: Some(ComponentHealth {
                healthy: false,
                last_error: "no pipelines configured".into(),
                ..Default::default()
            }),
            ..Default::default()
        });
        let st = agent.state(None);
        assert!(st.health_reported);
        assert!(!st.healthy);
        assert_eq!(st.health_error, "no pipelines configured");
    }

    #[test]
    fn description_attributes_are_sorted_and_string_valued() {
        let mut agent = Agent::default();
        agent.fold(&AgentToServer {
            sequence_num: 1,
            agent_description: Some(AgentDescription {
                identifying_attributes: vec![
                    kv("service.version", "1.0"),
                    kv("service.name", "otelcol"),
                ],
                non_identifying_attributes: vec![kv("opamp.supervisor", "opamp-supervisor (rust)")],
            }),
            ..Default::default()
        });
        let st = agent.state(None);
        assert_eq!(
            st.attributes,
            vec![
                ("service.name".to_string(), "otelcol".to_string()),
                ("service.version".to_string(), "1.0".to_string()),
            ]
        );
        // Non-identifying attributes (which supervisor, where it runs) are projected separately.
        assert_eq!(
            st.context,
            vec![(
                "opamp.supervisor".to_string(),
                "opamp-supervisor (rust)".to_string()
            )]
        );
    }

    #[test]
    fn api_state_uses_full_hashes_and_numeric_timestamps() {
        let want = AgentRemoteConfig {
            config_hash: vec![0xab, 0xcd, 0xef],
            ..Default::default()
        };
        let mut agent = Agent::default();
        agent.fold(&AgentToServer {
            instance_uid: vec![0x01, 0x02],
            sequence_num: 1,
            remote_config_status: Some(RemoteConfigStatus {
                last_remote_config_hash: vec![0xab, 0xcd, 0xef],
                status: RemoteConfigStatuses::Applied as i32,
                error_message: String::new(),
            }),
            agent_description: Some(AgentDescription {
                identifying_attributes: vec![kv("service.name", "otelcol")],
                non_identifying_attributes: vec![kv("opamp.supervisor", "rust")],
            }),
            ..Default::default()
        });

        let dto = agent.api_state(Some(&want));
        assert_eq!(dto.uid, "0102");
        // Full hex hash, not the shortened display form.
        assert_eq!(dto.config_hash, "abcdef");
        assert!(dto.in_sync);
        assert_eq!(dto.config_status, "APPLIED");
        assert_eq!(dto.identity[0].key, "service.name");
        assert_eq!(dto.context[0].key, "opamp.supervisor");
        // Numeric timestamps, not "3s ago".
        assert!(dto.last_seen_seconds_ago.is_some());
        assert!(dto.last_seen_unix.is_some());
    }

    #[test]
    fn effective_config_prefers_the_main_key() {
        let ec = EffectiveConfig {
            config_map: Some(AgentConfigMap {
                config_map: [
                    (
                        MAIN_CONFIG_KEY.to_string(),
                        AgentConfigFile {
                            body: b"main: yes\n".to_vec(),
                            content_type: String::new(),
                        },
                    ),
                    (
                        "other".to_string(),
                        AgentConfigFile {
                            body: b"other: no\n".to_vec(),
                            content_type: String::new(),
                        },
                    ),
                ]
                .into_iter()
                .collect(),
            }),
        };
        assert_eq!(effective_config_body(&ec), "main: yes\n");
    }

    #[test]
    fn fleet_snapshot_is_ordered_and_reflects_connections() {
        let fleet = Fleet::new();
        fleet.connect(1);
        fleet.connect(2);
        fleet.fold(
            1,
            &AgentToServer {
                instance_uid: vec![0xbb],
                sequence_num: 1,
                ..Default::default()
            },
        );
        fleet.fold(
            2,
            &AgentToServer {
                instance_uid: vec![0xaa],
                sequence_num: 1,
                ..Default::default()
            },
        );

        let snap = fleet.snapshot(None);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].uid, "aa", "ordered by uid");
        assert_eq!(snap[1].uid, "bb");

        assert_eq!(fleet.disconnect(2).as_deref(), Some("aa"));
        assert_eq!(fleet.snapshot(None).len(), 1);
    }

    #[test]
    fn folds_capabilities_and_decodes_their_names() {
        let mut agent = Agent::default();
        agent.fold(&AgentToServer {
            sequence_num: 1,
            capabilities: AgentCapabilities::ReportsHealth as u64
                | AgentCapabilities::AcceptsRemoteConfig as u64,
            ..Default::default()
        });
        // Decoded to names, ordered by bit (AcceptsRemoteConfig 0x2 before ReportsHealth 0x800).
        assert_eq!(
            agent.state(None).capabilities,
            vec![
                "AcceptsRemoteConfig".to_string(),
                "ReportsHealth".to_string()
            ]
        );
        // A later heartbeat that does not carry capabilities keeps the remembered ones (delta rule).
        agent.fold(&AgentToServer {
            sequence_num: 2,
            ..Default::default()
        });
        assert_eq!(agent.state(None).capabilities.len(), 2);
    }

    #[test]
    fn capability_names_shows_unknown_bits_as_hex() {
        let caps = AgentCapabilities::AcceptsRemoteConfig as u64 | (1u64 << 40);
        let names = capability_names(caps);
        assert!(names.contains(&"AcceptsRemoteConfig".to_string()));
        assert!(names.iter().any(|n| n == "0x10000000000"), "{names:?}");
    }

    #[test]
    fn folds_and_groups_available_components() {
        let group = |members: &[&str]| ComponentDetails {
            sub_component_map: members
                .iter()
                .map(|m| (m.to_string(), ComponentDetails::default()))
                .collect(),
            ..Default::default()
        };
        let ac = AvailableComponents {
            hash: vec![0xde, 0xad],
            components: [
                ("receivers".to_string(), group(&["otlp", "hostmetrics"])),
                ("exporters".to_string(), group(&["debug"])),
            ]
            .into_iter()
            .collect(),
        };
        let mut agent = Agent::default();
        agent.fold(&AgentToServer {
            sequence_num: 1,
            available_components: Some(ac),
            ..Default::default()
        });

        // Groups and members are both sorted, so the view is stable across refreshes.
        let st = agent.state(None);
        assert_eq!(
            st.components,
            vec![
                ("exporters".to_string(), vec!["debug".to_string()]),
                (
                    "receivers".to_string(),
                    vec!["hostmetrics".to_string(), "otlp".to_string()]
                ),
            ]
        );
        assert_eq!(st.components_hash, "dead");

        // The API projection mirrors it, with the full hash.
        let dto = agent.api_state(None);
        let components = dto.available_components.expect("components reported");
        assert_eq!(components.hash, "dead");
        assert_eq!(components.components[0].name, "exporters");
        assert_eq!(
            components.components[1].components,
            vec!["hostmetrics", "otlp"]
        );
    }

    #[test]
    fn agents_without_reports_expose_empty_capabilities_and_no_components() {
        let agent = Agent::default();
        assert!(agent.state(None).capabilities.is_empty());
        assert!(!agent.state(None).accepts_restart);
        assert!(agent.state(None).components.is_empty());
        assert!(agent.api_state(None).available_components.is_none());
    }

    #[test]
    fn accepts_restart_tracks_the_capability_and_is_connected_finds_the_agent() {
        let fleet = Fleet::new();
        fleet.connect(1);
        fleet.fold(
            1,
            &AgentToServer {
                instance_uid: vec![0xab, 0xcd],
                sequence_num: 1,
                capabilities: AgentCapabilities::AcceptsRestartCommand as u64,
                ..Default::default()
            },
        );
        assert!(
            fleet.is_connected(&[0xab, 0xcd]),
            "the reported uid is connected"
        );
        assert!(!fleet.is_connected(&[0x00]), "an unknown uid is not");
        assert!(fleet.snapshot(None)[0].accepts_restart);
    }

    #[test]
    fn fold_reports_full_state_on_gap() {
        let fleet = Fleet::new();
        fleet.connect(1);
        assert!(
            !fleet
                .fold(
                    1,
                    &AgentToServer {
                        sequence_num: 1,
                        ..Default::default()
                    }
                )
                .report_full_state
        );
        assert!(
            fleet
                .fold(
                    1,
                    &AgentToServer {
                        sequence_num: 3,
                        ..Default::default()
                    }
                )
                .report_full_state
        );
    }
}
