//! In-memory fleet state (ADR-0005).
//!
//! Holds one record per connected Agent plus the single remote configuration the Server wants the
//! fleet to run. [`Fleet::process`] is the control loop: it folds an `AgentToServer` report into the
//! record and offers the remote configuration only when the Agent's reported Config hash differs
//! from the desired one (specification Goal #3, no redundant reconfiguration).

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use opamp::v1::{
    any_value, AgentConfigFile, AgentConfigMap, AgentRemoteConfig, AgentToServer, AnyValue,
    EffectiveConfig, RemoteConfigStatuses, ServerToAgent,
};
use opamp::InstanceUid;
use serde::Serialize;

/// The Server's view of the whole fleet, plus the configuration it wants every Agent to run.
pub struct Fleet {
    inner: RwLock<State>,
}

struct State {
    agents: HashMap<InstanceUid, AgentRecord>,
    /// The remote configuration the Server wants the fleet to run (empty = nothing to offer yet).
    desired_config: Vec<u8>,
    /// `config_hash(desired_config)`, cached; empty when there is no desired config.
    desired_hash: Vec<u8>,
}

struct AgentRecord {
    first_seen_ms: u64,
    last_seen_ms: u64,
    sequence_num: u64,
    service_name: String,
    service_version: String,
    os: String,
    healthy: bool,
    health_status: String,
    effective_config: String,
    remote_config_status: i32,
    remote_config_error: String,
    reported_hash: Vec<u8>,
}

/// A serializable snapshot of one Agent for the JSON API / UI.
#[derive(Serialize)]
pub struct AgentView {
    pub instance_uid: String,
    pub service_name: String,
    pub service_version: String,
    pub os: String,
    pub healthy: bool,
    pub health_status: String,
    pub sequence_num: u64,
    pub effective_config: String,
    pub remote_config_status: String,
    pub remote_config_error: String,
    pub reported_hash: String,
    pub in_sync: bool,
    pub last_seen_ms: u64,
    pub first_seen_ms: u64,
}

/// A serializable snapshot of the desired remote configuration for the JSON API / UI.
#[derive(Serialize)]
pub struct ConfigView {
    pub config: String,
    pub hash: String,
}

impl Fleet {
    /// Create an empty fleet with no desired configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(State {
                agents: HashMap::new(),
                desired_config: Vec::new(),
                desired_hash: Vec::new(),
            }),
        }
    }

    /// Fold an Agent report into the fleet and produce the reply, offering the remote configuration
    /// only when the Agent's reported Config hash differs from the desired one.
    pub fn process(&self, uid: InstanceUid, msg: AgentToServer) -> ServerToAgent {
        let mut state = self.inner.write().expect("fleet lock poisoned");
        let now = unix_millis_now();

        {
            let record = state
                .agents
                .entry(uid)
                .or_insert_with(|| AgentRecord::new(now));
            record.update(now, &msg);
        }

        // Decide whether to offer the desired configuration (borrows of disjoint fields end above).
        let reported = state
            .agents
            .get(&uid)
            .map(|r| r.reported_hash.clone())
            .unwrap_or_default();

        let mut reply = ServerToAgent {
            instance_uid: uid.to_vec(),
            capabilities: opamp::server_capabilities(),
            ..Default::default()
        };
        if !state.desired_hash.is_empty() && reported != state.desired_hash {
            reply.remote_config = Some(AgentRemoteConfig {
                config: Some(single_file_config(&state.desired_config)),
                config_hash: state.desired_hash.clone(),
            });
        }
        reply
    }

    /// Replace the desired remote configuration and return its Config hash (hex).
    pub fn set_desired_config(&self, config: Vec<u8>) -> String {
        let mut state = self.inner.write().expect("fleet lock poisoned");
        state.desired_hash = if config.is_empty() {
            Vec::new()
        } else {
            opamp::config_hash(&config)
        };
        let hash = opamp::hex(&state.desired_hash);
        state.desired_config = config;
        hash
    }

    /// The desired remote configuration for the UI.
    pub fn desired_config(&self) -> ConfigView {
        let state = self.inner.read().expect("fleet lock poisoned");
        ConfigView {
            config: String::from_utf8_lossy(&state.desired_config).into_owned(),
            hash: opamp::hex(&state.desired_hash),
        }
    }

    /// A snapshot of every Agent, newest connection first, for the JSON API / UI.
    pub fn snapshot(&self) -> Vec<AgentView> {
        let state = self.inner.read().expect("fleet lock poisoned");
        let mut views: Vec<AgentView> = state
            .agents
            .iter()
            .map(|(uid, r)| r.view(uid, &state.desired_hash))
            .collect();
        views.sort_by_key(|a| a.first_seen_ms);
        views
    }
}

impl Default for Fleet {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRecord {
    fn new(now_ms: u64) -> Self {
        Self {
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
            sequence_num: 0,
            service_name: String::new(),
            service_version: String::new(),
            os: String::new(),
            healthy: false,
            health_status: String::new(),
            effective_config: String::new(),
            remote_config_status: RemoteConfigStatuses::Unset as i32,
            remote_config_error: String::new(),
            reported_hash: Vec::new(),
        }
    }

    fn update(&mut self, now_ms: u64, msg: &AgentToServer) {
        self.last_seen_ms = now_ms;
        self.sequence_num = msg.sequence_num;

        if let Some(desc) = &msg.agent_description {
            self.service_name = attr(&desc.identifying_attributes, "service.name");
            self.service_version = attr(&desc.identifying_attributes, "service.version");
            self.os = attr(&desc.non_identifying_attributes, "os.type");
        }
        if let Some(health) = &msg.health {
            self.healthy = health.healthy;
            self.health_status = health.status.clone();
        }
        if let Some(effective) = &msg.effective_config {
            self.effective_config = effective_config_text(effective);
        }
        if let Some(status) = &msg.remote_config_status {
            self.remote_config_status = status.status;
            self.remote_config_error = status.error_message.clone();
            self.reported_hash = status.last_remote_config_hash.clone();
        }
    }

    fn view(&self, uid: &InstanceUid, desired_hash: &[u8]) -> AgentView {
        AgentView {
            instance_uid: uid.to_string(),
            service_name: self.service_name.clone(),
            service_version: self.service_version.clone(),
            os: self.os.clone(),
            healthy: self.healthy,
            health_status: self.health_status.clone(),
            sequence_num: self.sequence_num,
            effective_config: self.effective_config.clone(),
            remote_config_status: status_label(self.remote_config_status).to_string(),
            remote_config_error: self.remote_config_error.clone(),
            reported_hash: opamp::hex(&self.reported_hash),
            // In sync when the Agent reports the hash the Server wants (or there is nothing to want).
            in_sync: desired_hash.is_empty() || self.reported_hash == desired_hash,
            last_seen_ms: self.last_seen_ms,
            first_seen_ms: self.first_seen_ms,
        }
    }
}

fn single_file_config(config: &[u8]) -> AgentConfigMap {
    let mut config_map = HashMap::new();
    config_map.insert(
        String::new(),
        AgentConfigFile {
            body: config.to_vec(),
            content_type: "text/plain".to_string(),
        },
    );
    AgentConfigMap { config_map }
}

fn effective_config_text(effective: &EffectiveConfig) -> String {
    let Some(map) = &effective.config_map else {
        return String::new();
    };
    let file = map
        .config_map
        .get("")
        .or_else(|| map.config_map.values().next());
    file.map(|f| String::from_utf8_lossy(&f.body).into_owned())
        .unwrap_or_default()
}

fn attr(attributes: &[opamp::v1::KeyValue], key: &str) -> String {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| any_value_to_string(kv.value.as_ref()))
        .unwrap_or_default()
}

fn any_value_to_string(value: Option<&AnyValue>) -> String {
    match value.and_then(|v| v.value.as_ref()) {
        Some(any_value::Value::StringValue(s)) => s.clone(),
        Some(any_value::Value::BoolValue(b)) => b.to_string(),
        Some(any_value::Value::IntValue(i)) => i.to_string(),
        Some(any_value::Value::DoubleValue(d)) => d.to_string(),
        Some(other) => format!("{other:?}"),
        None => String::new(),
    }
}

fn status_label(status: i32) -> &'static str {
    match RemoteConfigStatuses::try_from(status) {
        Ok(RemoteConfigStatuses::Applied) => "APPLIED",
        Ok(RemoteConfigStatuses::Applying) => "APPLYING",
        Ok(RemoteConfigStatuses::Failed) => "FAILED",
        _ => "UNSET",
    }
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::v1::{AgentDescription, ComponentHealth, RemoteConfigStatus};

    fn report(uid: &InstanceUid, seq: u64, reported_hash: Vec<u8>) -> AgentToServer {
        AgentToServer {
            instance_uid: uid.to_vec(),
            sequence_num: seq,
            agent_description: Some(AgentDescription {
                identifying_attributes: vec![],
                non_identifying_attributes: vec![],
            }),
            health: Some(ComponentHealth {
                healthy: true,
                status: "running".into(),
                ..Default::default()
            }),
            remote_config_status: Some(RemoteConfigStatus {
                last_remote_config_hash: reported_hash,
                status: RemoteConfigStatuses::Unset as i32,
                error_message: String::new(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn offers_config_when_hash_differs_then_stops_when_it_matches() {
        let fleet = Fleet::new();
        let uid = InstanceUid::generate();
        let hash = fleet.set_desired_config(b"receivers: {}".to_vec());

        // Agent reports an empty hash → Server offers the config.
        let reply = fleet.process(uid, report(&uid, 1, Vec::new()));
        let offered = reply.remote_config.expect("config offered");
        assert_eq!(opamp::hex(&offered.config_hash), hash);

        // Agent now reports the desired hash → Server offers nothing (Goal #3).
        let reply = fleet.process(uid, report(&uid, 2, offered.config_hash));
        assert!(reply.remote_config.is_none());
    }

    #[test]
    fn no_offer_without_desired_config() {
        let fleet = Fleet::new();
        let uid = InstanceUid::generate();
        let reply = fleet.process(uid, report(&uid, 1, Vec::new()));
        assert!(reply.remote_config.is_none());
    }

    #[test]
    fn snapshot_reflects_the_report() {
        let fleet = Fleet::new();
        let uid = InstanceUid::generate();
        fleet.process(uid, report(&uid, 5, Vec::new()));

        let snapshot = fleet.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].instance_uid, uid.to_string());
        assert_eq!(snapshot[0].sequence_num, 5);
        assert!(snapshot[0].healthy);
    }
}
