//! One Agent's state machine: builds `AgentToServer` reports and reacts to `ServerToAgent`
//! replies.
//!
//! Transport-agnostic on purpose (ADR-0007): the WebSocket and plain-HTTP loops feed the same
//! state machine, so transport is carriage, never semantics. The [`Engine`](crate::engine)
//! carries n of these over one connection (ADR-0003, ADR-0011) — a Supervisor-backed Agent and
//! the self-Agent fallback are the same state machine.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opamp::proto::{
    any_value, AgentCapabilities, AgentDescription, AgentDisconnect, AgentRemoteConfig,
    AgentToServer, AnyValue, ComponentHealth, EffectiveConfig, KeyValue, RemoteConfigStatus,
    RemoteConfigStatuses, ServerCapabilities, ServerErrorResponseType, ServerToAgent,
    ServerToAgentFlags,
};
use opamp::uid::InstanceUid;
use tracing::{error, info, warn};

use crate::storage::Storage;

/// The Capability Set this Client declares (see docs/CONFORMANCE.md).
pub const AGENT_CAPABILITIES: u64 = AgentCapabilities::ReportsStatus as u64
    | AgentCapabilities::AcceptsRemoteConfig as u64
    | AgentCapabilities::ReportsEffectiveConfig as u64
    | AgentCapabilities::ReportsRemoteConfig as u64
    | AgentCapabilities::ReportsHealth as u64;

/// What a handled `ServerToAgent` asks of the transport loop.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Handled {
    /// Something changed that the Server must hear about now (a config outcome, a demanded full
    /// report) — send the next report immediately instead of waiting for the poll interval.
    pub send_report: bool,
    /// The Server is throttling us (`UNAVAILABLE` + retry info): back off this long first.
    pub retry_after: Option<Duration>,
}

pub struct AgentState {
    uid: InstanceUid,
    sequence_num: u64,
    name: String,
    start_time_ns: u64,
    storage: Storage,
    /// The last stored remote configuration; what `effective_config` echoes unless the Managed
    /// Process reported its own.
    applied: Option<AgentRemoteConfig>,
    status: Option<RemoteConfigStatus>,
    /// The Server's declared Capability Set, once a reply carried it. Capability negotiation is
    /// binding in both directions: we stop reporting what the Server cannot accept.
    server_capabilities: Option<u64>,
    send_full: bool,
    send_status: bool,
    /// A Managed Process stands behind this Agent: a received configuration is acknowledged
    /// `APPLYING` and handed to the process adapter; `APPLIED`/`FAILED` follow its outcome.
    managed: bool,
    /// A received configuration awaiting dispatch to the process adapter.
    pending_apply: Option<AgentRemoteConfig>,
    /// The Managed Process's health — derived or self-reported (ADR-0011). Absent for the
    /// self-Agent, whose health is being alive.
    process_health: Option<ComponentHealth>,
    send_health: bool,
    /// The Managed Process's self-reported description, folded into ours (goal 16).
    process_description: Option<AgentDescription>,
    /// The Managed Process's self-reported effective configuration; replaces the echo.
    process_effective_config: Option<EffectiveConfig>,
    /// Operator-defined attributes from `client.toml` (ADR-0012), reported as non-identifying
    /// attributes so Selectors can target them. Reported attributes win on key collision.
    configured_attributes: Vec<(String, String)>,
}

impl AgentState {
    /// Restores identity and configuration from storage, so a restart reports the same Agent with
    /// the same applied config hash — and is therefore not reconfigured redundantly.
    pub fn new(name: String, storage: Storage) -> std::io::Result<Self> {
        let uid = storage.load_or_create_uid()?;
        let applied = storage.load_remote_config();
        let status = applied.as_ref().map(|config| RemoteConfigStatus {
            last_remote_config_hash: config.config_hash.clone(),
            status: RemoteConfigStatuses::Applied as i32,
            error_message: String::new(),
        });
        info!(agent = %uid, "agent identity ready");
        Ok(AgentState {
            uid,
            sequence_num: 0,
            name,
            start_time_ns: now_ns(),
            storage,
            applied,
            status,
            server_capabilities: None,
            send_full: true,
            send_status: false,
            managed: false,
            pending_apply: None,
            process_health: None,
            send_health: false,
            process_description: None,
            process_effective_config: None,
            configured_attributes: Vec::new(),
        })
    }

    /// An Agent with a Managed Process behind it (a Supervisor-backed Agent, ADR-0011).
    pub fn supervised(name: String, storage: Storage) -> std::io::Result<Self> {
        let mut state = Self::new(name, storage)?;
        state.managed = true;
        Ok(state)
    }

    /// Attaches the operator-defined attributes this Agent reports (ADR-0012).
    #[must_use]
    pub fn with_attributes(
        mut self,
        attributes: std::collections::BTreeMap<String, String>,
    ) -> Self {
        self.configured_attributes = attributes.into_iter().collect();
        self
    }

    pub fn uid(&self) -> InstanceUid {
        self.uid
    }

    /// A configuration stored `APPLYING` and not yet handed to the process adapter, if any.
    pub fn take_pending_apply(&mut self) -> Option<AgentRemoteConfig> {
        self.pending_apply.take()
    }

    /// The process adapter's verdict on an [`ApplyConfig`](super::ports::ProcessCommand): closes
    /// the `APPLYING` → `APPLIED`/`FAILED` lifecycle (goal 4, end to end).
    pub fn config_applied(&mut self, hash: Vec<u8>, result: Result<(), String>) {
        self.status = Some(match result {
            Ok(()) => RemoteConfigStatus {
                last_remote_config_hash: hash,
                status: RemoteConfigStatuses::Applied as i32,
                error_message: String::new(),
            },
            Err(error) => RemoteConfigStatus {
                last_remote_config_hash: hash,
                status: RemoteConfigStatuses::Failed as i32,
                error_message: error,
            },
        });
        self.send_status = true;
    }

    /// The Managed Process's health changed — derived or self-reported.
    pub fn set_process_health(&mut self, health: ComponentHealth) {
        self.process_health = Some(health);
        self.send_health = true;
    }

    /// The Managed Process reported its own description (through the Supervisor Endpoint); fold
    /// it into ours — identity stays the Supervisor's (goal 16).
    pub fn set_process_description(&mut self, description: AgentDescription) {
        self.process_description = Some(description);
        self.send_full = true;
    }

    /// The Managed Process reported its own effective configuration; report that instead of
    /// echoing the written files.
    pub fn set_process_effective_config(&mut self, config: EffectiveConfig) {
        self.process_effective_config = Some(config);
        self.send_status = true;
    }

    /// The next report starts from a full status snapshot again — after (re)connecting, after an
    /// exchange failed, or when the Server demanded it.
    pub fn force_full(&mut self) {
        self.send_full = true;
    }

    /// The next `AgentToServer`. Unchanged fields are omitted, as the Baseline recommends: a
    /// routine poll carries only identity and sequence number; a full snapshot goes out when
    /// [`force_full`](Self::force_full) was called, and the config-status fields whenever they
    /// changed.
    pub fn next_report(&mut self) -> AgentToServer {
        self.sequence_num += 1;
        let mut msg = AgentToServer {
            instance_uid: self.uid.as_bytes().to_vec(),
            sequence_num: self.sequence_num,
            capabilities: AGENT_CAPABILITIES,
            ..Default::default()
        };
        if self.send_full {
            msg.agent_description = Some(self.describe());
        }
        if self.send_full || self.send_health {
            msg.health = Some(self.health());
        }
        if self.send_full || self.send_status {
            msg.remote_config_status = self.status.clone();
            if self.server_accepts_effective_config() {
                msg.effective_config = Some(match &self.process_effective_config {
                    Some(reported) => reported.clone(),
                    None => EffectiveConfig {
                        config_map: self.applied.as_ref().and_then(|c| c.config.clone()),
                    },
                });
            }
        }
        self.send_full = false;
        self.send_status = false;
        self.send_health = false;
        msg
    }

    /// The final message of a connection: the Baseline requires `agent_disconnect` in it.
    pub fn disconnect_message(&mut self) -> AgentToServer {
        self.sequence_num += 1;
        AgentToServer {
            instance_uid: self.uid.as_bytes().to_vec(),
            sequence_num: self.sequence_num,
            capabilities: AGENT_CAPABILITIES,
            agent_disconnect: Some(AgentDisconnect {}),
            ..Default::default()
        }
    }

    /// Reacts to one `ServerToAgent`.
    pub fn handle(&mut self, reply: &ServerToAgent) -> Handled {
        let mut handled = Handled::default();

        if reply.capabilities != 0 {
            self.server_capabilities = Some(reply.capabilities);
        }

        if let Some(response) = &reply.error_response {
            error!(message = %response.error_message, "the server reported an error");
            if response.r#type == ServerErrorResponseType::Unavailable as i32 {
                let nanos = match &response.details {
                    Some(opamp::proto::server_error_response::Details::RetryInfo(info)) => {
                        info.retry_after_nanoseconds
                    }
                    _ => 30_000_000_000, // no hint: be gentle and stay away half a minute
                };
                handled.retry_after = Some(Duration::from_nanos(nanos));
            }
            return handled;
        }

        // The Server may reassign our identity (AgentIdentification); adopt it for all further
        // communication, persistently.
        if let Some(identification) = &reply.agent_identification {
            match InstanceUid::from_wire(&identification.new_instance_uid) {
                Some(new_uid) => {
                    info!(old = %self.uid, new = %new_uid, "adopting a server-assigned identity");
                    self.uid = new_uid;
                    if let Err(e) = self.storage.save_uid(&new_uid) {
                        warn!(error = %e, "cannot persist the new identity");
                    }
                }
                None => warn!("ignoring a malformed server-assigned instance_uid"),
            }
        }

        if reply.flags & ServerToAgentFlags::ReportFullState as u64 != 0 {
            self.send_full = true;
            handled.send_report = true;
        }

        if let Some(remote_config) = &reply.remote_config {
            self.apply(remote_config);
            handled.send_report = true;
        }

        handled
    }

    /// Takes an offered configuration in: store it, then either acknowledge it directly (the
    /// self-Agent: storing *is* applying) or report `APPLYING` and leave it pending for the
    /// process adapter, whose outcome closes the lifecycle. Success and failure alike carry the
    /// hash the status refers to (a rejected configuration is a report, not a silence).
    fn apply(&mut self, config: &AgentRemoteConfig) {
        match self.storage.store_remote_config(config) {
            Ok(()) if self.managed => {
                info!(hash = %hex::encode(&config.config_hash), "remote configuration stored; applying");
                self.applied = Some(config.clone());
                self.status = Some(RemoteConfigStatus {
                    last_remote_config_hash: config.config_hash.clone(),
                    status: RemoteConfigStatuses::Applying as i32,
                    error_message: String::new(),
                });
                self.pending_apply = Some(config.clone());
            }
            Ok(()) => {
                info!(hash = %hex::encode(&config.config_hash), "remote configuration applied");
                self.applied = Some(config.clone());
                self.status = Some(RemoteConfigStatus {
                    last_remote_config_hash: config.config_hash.clone(),
                    status: RemoteConfigStatuses::Applied as i32,
                    error_message: String::new(),
                });
            }
            Err(e) => {
                error!(error = %e, "cannot store the remote configuration");
                self.status = Some(RemoteConfigStatus {
                    last_remote_config_hash: config.config_hash.clone(),
                    status: RemoteConfigStatuses::Failed as i32,
                    error_message: format!("cannot store the configuration: {e}"),
                });
            }
        }
        self.send_status = true;
    }

    fn server_accepts_effective_config(&self) -> bool {
        // Until the Server has declared anything, report optimistically; once it has, its word is
        // binding ("Interoperability of Partial Implementations").
        self.server_capabilities
            .map(|caps| caps & ServerCapabilities::AcceptsEffectiveConfig as u64 != 0)
            .unwrap_or(true)
    }

    fn describe(&self) -> AgentDescription {
        let mut identifying_attributes = vec![string_attr("service.name", &self.name)];
        // `service.version` is the *Agent's* version. The self-Agent is the Client, so its baked
        // version is the truth; a Supervisor-backed Agent stands for its Managed Process, whose
        // version only the process itself can report (folded in below, goal 16) — never invented
        // from the Client's.
        if !self.managed {
            identifying_attributes.push(string_attr("service.version", crate::version::version()));
        }
        identifying_attributes.push(string_attr("service.instance.id", &self.uid.to_string()));
        let mut non_identifying_attributes = vec![
            string_attr("os.type", os_type()),
            string_attr("host.arch", std::env::consts::ARCH),
        ];
        if let Some(os) = os_description() {
            non_identifying_attributes.push(string_attr("os.description", os));
        }
        let mut description = AgentDescription {
            identifying_attributes,
            non_identifying_attributes,
        };
        // Operator-defined attributes (ADR-0012) — added only where nothing is reported under the
        // same key, so what the code (and below, the Managed Process) reports always wins.
        for (key, value) in &self.configured_attributes {
            let taken = |list: &[KeyValue]| list.iter().any(|kv| kv.key == *key);
            if !taken(&description.identifying_attributes)
                && !taken(&description.non_identifying_attributes)
            {
                description
                    .non_identifying_attributes
                    .push(string_attr(key, value));
            }
        }
        // Fold in what the Managed Process reported about itself — except its identity: the
        // Agent the Server sees is the Supervisor, keyed by the Supervisor's uid (goal 16).
        if let Some(reported) = &self.process_description {
            for attr in &reported.identifying_attributes {
                if attr.key != "service.instance.id" {
                    upsert_attr(&mut description.identifying_attributes, attr);
                }
            }
            for attr in &reported.non_identifying_attributes {
                upsert_attr(&mut description.non_identifying_attributes, attr);
            }
        }
        description
    }

    fn health(&self) -> ComponentHealth {
        match &self.process_health {
            Some(health) => health.clone(),
            None if self.managed => ComponentHealth {
                healthy: false,
                status: "starting".to_string(),
                status_time_unix_nano: now_ns(),
                ..Default::default()
            },
            // The self-Agent's health is being alive.
            None => ComponentHealth {
                healthy: true,
                start_time_unix_nano: self.start_time_ns,
                status: "running".to_string(),
                status_time_unix_nano: now_ns(),
                ..Default::default()
            },
        }
    }
}

fn upsert_attr(attrs: &mut Vec<KeyValue>, attr: &KeyValue) {
    match attrs.iter_mut().find(|existing| existing.key == attr.key) {
        Some(existing) => existing.value = attr.value.clone(),
        None => attrs.push(attr.clone()),
    }
}

fn string_attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
    }
}

/// OpenTelemetry semantic-convention value for `os.type` (Rust says "macos", the convention
/// "darwin").
fn os_type() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// Human-readable operating-system description (OTel `os.description`, e.g. "Ubuntu 24.04.2
/// LTS") — best effort per platform, computed once, absent when the platform gives none.
fn os_description() -> Option<&'static str> {
    static DESCRIPTION: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    DESCRIPTION.get_or_init(read_os_description).as_deref()
}

#[cfg(target_os = "linux")]
fn read_os_description() -> Option<String> {
    // os-release(5): PRETTY_NAME="Ubuntu 24.04.2 LTS"
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim().trim_matches(['"', '\'']).to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(target_os = "macos")]
fn read_os_description() -> Option<String> {
    // `sw_vers` prints ProductName/ProductVersion/BuildVersion lines, e.g. "macOS" / "15.5".
    let output = std::process::Command::new("sw_vers").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim_start_matches(':').trim().to_string())
    };
    match (field("ProductName"), field("ProductVersion")) {
        (Some(name), Some(version)) => Some(format!("{name} {version}")),
        (Some(name), None) => Some(name),
        _ => None,
    }
}

#[cfg(windows)]
fn read_os_description() -> Option<String> {
    // `cmd /c ver` prints e.g. "Microsoft Windows [Version 10.0.26100.2033]".
    let output = std::process::Command::new("cmd")
        .args(["/c", "ver"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn read_os_description() -> Option<String> {
    None
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::proto::{AgentConfigFile, AgentConfigMap};
    use std::collections::HashMap;

    fn make_agent(dir: &std::path::Path) -> AgentState {
        let storage = Storage::new(dir.to_path_buf()).expect("storage");
        AgentState::new("test-agent".to_string(), storage).expect("agent")
    }

    fn remote_config(body: &[u8], hash: &[u8]) -> AgentRemoteConfig {
        AgentRemoteConfig {
            config: Some(AgentConfigMap {
                config_map: HashMap::from([(
                    String::new(),
                    AgentConfigFile {
                        body: body.to_vec(),
                        content_type: String::new(),
                    },
                )]),
            }),
            config_hash: hash.to_vec(),
        }
    }

    #[test]
    fn configured_attributes_are_reported_but_never_shadow_reported_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let agent = AgentState::new("test-agent".to_string(), storage)
            .expect("agent")
            .with_attributes(
                [
                    ("env".to_string(), "prod".to_string()),
                    // Collides with what the code reports — the reported value must win.
                    ("os.type".to_string(), "configured".to_string()),
                ]
                .into(),
            );

        let description = agent.describe();
        let value = |key: &str| {
            description
                .non_identifying_attributes
                .iter()
                .find(|kv| kv.key == key)
                .and_then(|kv| kv.value.as_ref())
                .and_then(|v| v.value.as_ref())
                .map(|v| match v {
                    opamp::proto::any_value::Value::StringValue(s) => s.clone(),
                    other => format!("{other:?}"),
                })
        };
        assert_eq!(value("env").as_deref(), Some("prod"));
        assert_eq!(value("os.type").as_deref(), Some(os_type()));
        assert_eq!(
            description
                .non_identifying_attributes
                .iter()
                .filter(|kv| kv.key == "os.type")
                .count(),
            1
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_description_names_the_distribution_not_only_the_kernel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let description = make_agent(dir.path()).describe();
        let os = description
            .non_identifying_attributes
            .iter()
            .find(|kv| kv.key == "os.description")
            .expect("an os.description on a distribution with /etc/os-release");
        let text = match &os.value.as_ref().and_then(|v| v.value.as_ref()) {
            Some(opamp::proto::any_value::Value::StringValue(s)) => s.clone(),
            other => panic!("os.description must be a string, got {other:?}"),
        };
        assert!(!text.is_empty());
        assert_ne!(text, "linux", "the PRETTY_NAME, not the os.type");
    }

    #[test]
    fn only_the_self_agent_carries_the_client_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let version_of = |agent: &AgentState| {
            agent
                .describe()
                .identifying_attributes
                .iter()
                .find(|kv| kv.key == "service.version")
                .and_then(|kv| kv.value.clone())
                .and_then(|v| v.value)
                .map(|v| match v {
                    opamp::proto::any_value::Value::StringValue(s) => s,
                    other => format!("{other:?}"),
                })
        };

        // The self-Agent *is* the Client — its baked version is the Agent's version.
        let this = make_agent(&dir.path().join("self"));
        assert_eq!(
            version_of(&this).as_deref(),
            Some(crate::version::version())
        );

        // A Supervisor-backed Agent reports no version until its Managed Process states one.
        let storage = Storage::new(dir.path().join("supervised")).expect("storage");
        let mut supervised = AgentState::supervised("otelcol".to_string(), storage).expect("agent");
        assert_eq!(version_of(&supervised), None);

        supervised.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.version", "0.142.0")],
            non_identifying_attributes: vec![],
        });
        assert_eq!(version_of(&supervised).as_deref(), Some("0.142.0"));
    }

    #[test]
    fn first_report_is_full_then_compressed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());

        let first = agent.next_report();
        assert!(first.agent_description.is_some());
        assert!(first.health.is_some());
        assert_eq!(first.sequence_num, 1);

        let second = agent.next_report();
        assert!(second.agent_description.is_none());
        assert!(second.health.is_none());
        assert_eq!(second.sequence_num, 2);
    }

    #[test]
    fn an_offer_is_applied_and_acknowledged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let _ = agent.next_report();

        let handled = agent.handle(&ServerToAgent {
            remote_config: Some(remote_config(b"x: 1\n", b"hash-1")),
            ..Default::default()
        });
        assert!(handled.send_report);

        let ack = agent.next_report();
        let status = ack.remote_config_status.expect("status");
        assert_eq!(status.status, RemoteConfigStatuses::Applied as i32);
        assert_eq!(status.last_remote_config_hash, b"hash-1");
        assert!(ack.effective_config.is_some());
    }

    #[test]
    fn the_applied_config_survives_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut agent = make_agent(dir.path());
            let _ = agent.next_report();
            agent.handle(&ServerToAgent {
                remote_config: Some(remote_config(b"x: 1\n", b"hash-1")),
                ..Default::default()
            });
        }
        let mut restarted = make_agent(dir.path());
        let report = restarted.next_report();
        let status = report.remote_config_status.expect("status");
        assert_eq!(status.last_remote_config_hash, b"hash-1");
        assert_eq!(status.status, RemoteConfigStatuses::Applied as i32);
    }

    #[test]
    fn report_full_state_forces_a_full_report() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let _ = agent.next_report();
        let _ = agent.next_report();

        let handled = agent.handle(&ServerToAgent {
            flags: ServerToAgentFlags::ReportFullState as u64,
            ..Default::default()
        });
        assert!(handled.send_report);
        assert!(agent.next_report().agent_description.is_some());
    }

    #[test]
    fn a_server_assigned_identity_is_adopted_and_persisted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let new_uid = InstanceUid::default();
        agent.handle(&ServerToAgent {
            agent_identification: Some(opamp::proto::AgentIdentification {
                new_instance_uid: new_uid.as_bytes().to_vec(),
            }),
            ..Default::default()
        });
        assert_eq!(agent.uid(), new_uid);
        assert_eq!(make_agent(dir.path()).uid(), new_uid);
    }

    #[test]
    fn unavailable_yields_a_retry_hint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let handled = agent.handle(&ServerToAgent {
            error_response: Some(opamp::proto::ServerErrorResponse {
                r#type: ServerErrorResponseType::Unavailable as i32,
                details: Some(opamp::proto::server_error_response::Details::RetryInfo(
                    opamp::proto::RetryInfo {
                        retry_after_nanoseconds: 5_000_000_000,
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(handled.retry_after, Some(Duration::from_secs(5)));
        assert!(!handled.send_report);
    }

    #[test]
    fn effective_config_respects_the_servers_capability_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        // A server that only accepts status: stop sending effective config.
        agent.handle(&ServerToAgent {
            capabilities: ServerCapabilities::AcceptsStatus as u64,
            ..Default::default()
        });
        agent.force_full();
        assert!(agent.next_report().effective_config.is_none());
    }
}
