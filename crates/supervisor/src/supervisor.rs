//! The Supervisor: holds one Managed Agent's state, builds the OpAMP status report, and applies the
//! remote configuration the Server sends (the control loop, specification Goal #1).
//!
//! In this first version the "Managed Agent" is stand-in: applying a configuration means writing it
//! to a file as the new effective configuration. A real Collector/Custom Supervisor plugin replaces
//! that step later, behind the same reporting logic.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opamp::v1::{
    any_value, AgentConfigFile, AgentConfigMap, AgentDescription, AgentRemoteConfig, AgentToServer,
    AnyValue, ComponentHealth, EffectiveConfig, KeyValue, RemoteConfigStatus, RemoteConfigStatuses,
    ServerToAgent,
};
use opamp::InstanceUid;

/// Default interval between status reports when the Server does not dictate one.
pub const DEFAULT_POLL: Duration = Duration::from_secs(10);

/// A single Supervisor managing one Managed Agent, as one Agent to the Server.
pub struct Supervisor {
    instance_uid: InstanceUid,
    sequence_num: u64,
    start_time_unix_nano: u64,
    config_path: PathBuf,
    effective_config: Vec<u8>,
    last_remote_config_hash: Vec<u8>,
    remote_config_status: i32,
    remote_config_error: String,
}

impl Supervisor {
    /// Create a Supervisor with a persisted identity and any configuration already on disk.
    #[must_use]
    pub fn new(instance_uid: InstanceUid, config_path: PathBuf, effective_config: Vec<u8>) -> Self {
        Self {
            instance_uid,
            sequence_num: 0,
            start_time_unix_nano: unix_nanos_now(),
            config_path,
            effective_config,
            // Never received a remote config yet: report an empty hash so the Server's first
            // comparison mismatches and it offers its configuration (the control loop kicks in).
            last_remote_config_hash: Vec::new(),
            remote_config_status: RemoteConfigStatuses::Unset as i32,
            remote_config_error: String::new(),
        }
    }

    /// The Agent's Instance UID.
    #[must_use]
    pub fn instance_uid(&self) -> InstanceUid {
        self.instance_uid
    }

    /// Build the next `AgentToServer` report, advancing the sequence number.
    ///
    /// The first version always sends the full state (description, capabilities, health, effective
    /// config, remote-config status). The specification permits omitting unchanged fields as an
    /// optimization; we keep it simple and always include them.
    pub fn build_message(&mut self) -> AgentToServer {
        self.sequence_num += 1;

        AgentToServer {
            instance_uid: self.instance_uid.to_vec(),
            sequence_num: self.sequence_num,
            agent_description: Some(self.agent_description()),
            capabilities: opamp::required_agent_capabilities(),
            health: Some(self.health()),
            effective_config: Some(self.effective_config_message()),
            remote_config_status: Some(self.remote_config_status_message()),
            ..Default::default()
        }
    }

    /// Process a `ServerToAgent` reply. Returns `true` if applying it changed local state, so the
    /// caller can report the new status immediately instead of waiting for the next poll.
    pub fn handle_response(&mut self, msg: ServerToAgent) -> bool {
        if let Some(remote_config) = msg.remote_config {
            return self.apply_remote_config(remote_config);
        }
        false
    }

    /// Apply an offered remote configuration and record the outcome. Returns whether state changed.
    fn apply_remote_config(&mut self, remote_config: AgentRemoteConfig) -> bool {
        let hash = remote_config.config_hash;

        // No redundant reconfiguration: already applied this exact config (Goal #3).
        if !hash.is_empty()
            && hash == self.last_remote_config_hash
            && self.remote_config_status == RemoteConfigStatuses::Applied as i32
        {
            return false;
        }

        let body = first_config_body(remote_config.config);
        match self.write_effective_config(&body) {
            Ok(()) => {
                self.effective_config = body;
                self.last_remote_config_hash = hash;
                self.remote_config_status = RemoteConfigStatuses::Applied as i32;
                self.remote_config_error.clear();
            }
            Err(err) => {
                // Still record the hash we were offered so the Server sees which config failed.
                self.last_remote_config_hash = hash;
                self.remote_config_status = RemoteConfigStatuses::Failed as i32;
                self.remote_config_error = format!("{err:#}");
            }
        }
        true
    }

    fn write_effective_config(&self, body: &[u8]) -> std::io::Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.config_path, body)
    }

    fn agent_description(&self) -> AgentDescription {
        AgentDescription {
            identifying_attributes: vec![
                string_attr("service.name", "io.opamp-fleet.supervisor"),
                string_attr("service.version", env!("CARGO_PKG_VERSION")),
                string_attr("service.instance.id", &self.instance_uid.to_string()),
            ],
            non_identifying_attributes: vec![
                string_attr("os.type", std::env::consts::OS),
                string_attr("host.arch", std::env::consts::ARCH),
            ],
        }
    }

    fn health(&self) -> ComponentHealth {
        ComponentHealth {
            healthy: true,
            start_time_unix_nano: self.start_time_unix_nano,
            status: "running".to_string(),
            status_time_unix_nano: unix_nanos_now(),
            ..Default::default()
        }
    }

    fn effective_config_message(&self) -> EffectiveConfig {
        let mut config_map = HashMap::new();
        config_map.insert(
            String::new(),
            AgentConfigFile {
                body: self.effective_config.clone(),
                content_type: "text/plain".to_string(),
            },
        );
        EffectiveConfig {
            config_map: Some(AgentConfigMap { config_map }),
        }
    }

    fn remote_config_status_message(&self) -> RemoteConfigStatus {
        RemoteConfigStatus {
            last_remote_config_hash: self.last_remote_config_hash.clone(),
            status: self.remote_config_status,
            error_message: self.remote_config_error.clone(),
        }
    }
}

/// Extract the single config body from an offered `AgentConfigMap` (the empty key by convention,
/// else the first entry). The first version manages one config file per Agent.
fn first_config_body(config: Option<AgentConfigMap>) -> Vec<u8> {
    let Some(map) = config else {
        return Vec::new();
    };
    if let Some(file) = map.config_map.get("") {
        return file.body.clone();
    }
    map.config_map
        .into_values()
        .next()
        .map(|file| file.body)
        .unwrap_or_default()
}

fn string_attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
    }
}

fn unix_nanos_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::config_hash;

    fn offer(body: &[u8]) -> ServerToAgent {
        let mut config_map = HashMap::new();
        config_map.insert(
            String::new(),
            AgentConfigFile {
                body: body.to_vec(),
                content_type: "text/plain".to_string(),
            },
        );
        ServerToAgent {
            remote_config: Some(AgentRemoteConfig {
                config: Some(AgentConfigMap { config_map }),
                config_hash: config_hash(body),
            }),
            ..Default::default()
        }
    }

    fn test_supervisor() -> (Supervisor, tempdir::Guard) {
        let guard = tempdir::Guard::new();
        let sup = Supervisor::new(
            InstanceUid::generate(),
            guard.path().join("effective-config.txt"),
            Vec::new(),
        );
        (sup, guard)
    }

    #[test]
    fn applies_offered_config_and_reports_applied() {
        let (mut sup, guard) = test_supervisor();

        let changed = sup.handle_response(offer(b"receivers: {}"));

        assert!(changed);
        assert_eq!(sup.effective_config, b"receivers: {}");
        assert_eq!(
            sup.remote_config_status,
            RemoteConfigStatuses::Applied as i32
        );
        assert_eq!(sup.last_remote_config_hash, config_hash(b"receivers: {}"));
        assert_eq!(
            std::fs::read(guard.path().join("effective-config.txt")).unwrap(),
            b"receivers: {}"
        );
    }

    #[test]
    fn does_not_reapply_the_same_config() {
        let (mut sup, _guard) = test_supervisor();
        assert!(sup.handle_response(offer(b"same")));
        // Same hash again → no change (Goal #3, no redundant reconfiguration).
        assert!(!sup.handle_response(offer(b"same")));
    }

    #[test]
    fn empty_reply_changes_nothing() {
        let (mut sup, _guard) = test_supervisor();
        assert!(!sup.handle_response(ServerToAgent::default()));
        assert_eq!(sup.remote_config_status, RemoteConfigStatuses::Unset as i32);
    }

    #[test]
    fn sequence_number_advances_each_report() {
        let (mut sup, _guard) = test_supervisor();
        assert_eq!(sup.build_message().sequence_num, 1);
        assert_eq!(sup.build_message().sequence_num, 2);
    }

    /// Minimal unique-temp-dir helper so tests do not touch each other's files or the repo.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU32, Ordering};

        static COUNTER: AtomicU32 = AtomicU32::new(0);

        pub struct Guard(PathBuf);

        impl Guard {
            pub fn new() -> Self {
                let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                let dir = std::env::temp_dir()
                    .join(format!("opamp-fleet-sup-test-{}-{n}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                Self(dir)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
