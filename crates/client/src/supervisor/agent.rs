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
    AgentToServer, AnyValue, AvailableComponents, ComponentHealth, ConnectionSettingsOffers,
    ConnectionSettingsStatus, ConnectionSettingsStatuses, EffectiveConfig, KeyValue,
    RemoteConfigStatus, RemoteConfigStatuses, ServerCapabilities, ServerErrorResponseType,
    ServerToAgent, ServerToAgentFlags,
};
use opamp::uid::InstanceUid;
use tracing::{error, info, warn};

use crate::storage::Storage;

/// The base Capability Set every Agent of this Client declares (see docs/CONFORMANCE.md).
/// Individual Agents declare more via [`AgentState::declare_capability`] — e.g. heartbeats when
/// enabled, restartability only where a Managed Process exists.
pub const AGENT_CAPABILITIES: u64 = AgentCapabilities::ReportsStatus as u64
    | AgentCapabilities::AcceptsRemoteConfig as u64
    | AgentCapabilities::ReportsEffectiveConfig as u64
    | AgentCapabilities::ReportsRemoteConfig as u64
    | AgentCapabilities::ReportsHealth as u64
    | AgentCapabilities::AcceptsOpAmpConnectionSettings as u64
    | AgentCapabilities::ReportsConnectionSettingsStatus as u64;

/// What a handled `ServerToAgent` asks of the transport loop.
#[derive(Debug, Default, PartialEq)]
pub struct Handled {
    /// Something changed that the Server must hear about now (a config outcome, a demanded full
    /// report) — send the next report immediately instead of waiting for the poll interval.
    pub send_report: bool,
    /// The Server is throttling us (`UNAVAILABLE` + retry info): back off this long first.
    pub retry_after: Option<Duration>,
    /// A connection-settings offer to verify by actually connecting (ADR-0014). The state
    /// machine has already acknowledged `APPLYING`; the transport owns the verification, the
    /// switch, and reporting the outcome back through the [`Engine`](crate::engine).
    pub connection_offer: Option<ConnectionSettingsOffers>,
}

pub struct AgentState {
    uid: InstanceUid,
    sequence_num: u64,
    name: String,
    /// This Agent's declared Capability Set: the base set plus whatever
    /// [`declare_capability`](Self::declare_capability) added. Carried in every report, so the
    /// Server's cached mask follows on the next exchange.
    capabilities: u64,
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
    /// A Server-commanded restart awaiting dispatch to the process adapter.
    pending_restart: bool,
    /// The Managed Process's health — derived or self-reported (ADR-0011). Absent for the
    /// self-Agent, whose health is being alive.
    process_health: Option<ComponentHealth>,
    send_health: bool,
    /// The Managed Process's self-reported description, folded into ours (goal 16).
    process_description: Option<AgentDescription>,
    /// The Managed Process's self-reported effective configuration; replaces the echo.
    process_effective_config: Option<EffectiveConfig>,
    /// The Managed Process's available components, relayed from the Supervisor Endpoint.
    /// Routine reports carry only the hash; the full map goes out when the Server asks.
    available_components: Option<AvailableComponents>,
    /// The Server flagged `ReportAvailableComponents`: the next report carries the full map.
    send_components_full: bool,
    /// The outcome of the last connection-settings offer (ADR-0014): `APPLYING` on receipt,
    /// `APPLIED`/`FAILED` once the transport verified. Its hash stops the Server re-offering.
    connection_settings_status: Option<ConnectionSettingsStatus>,
    send_settings_status: bool,
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
            capabilities: AGENT_CAPABILITIES,
            start_time_ns: now_ns(),
            storage,
            applied,
            status,
            server_capabilities: None,
            send_full: true,
            send_status: false,
            managed: false,
            pending_apply: None,
            pending_restart: false,
            process_health: None,
            send_health: false,
            process_description: None,
            process_effective_config: None,
            available_components: None,
            send_components_full: false,
            connection_settings_status: None,
            send_settings_status: false,
            configured_attributes: Vec::new(),
        })
    }

    /// Restores the outcome of a previously applied connection-settings offer (ADR-0014): the
    /// persisted hash reports `APPLIED`, so a restarted Client is not re-offered what it runs.
    pub fn adopt_connection_settings(&mut self, hash: &[u8]) {
        self.connection_settings_status = Some(ConnectionSettingsStatus {
            last_connection_settings_hash: hash.to_vec(),
            status: ConnectionSettingsStatuses::Applied as i32,
            error_message: String::new(),
        });
    }

    /// Closes the connection-settings lifecycle the transport verified (ADR-0014): `APPLIED`
    /// keeps the hash and the switch follows; `FAILED` keeps the hash too — the Baseline's
    /// gating stops the Server re-offering the exact settings this Agent could not use.
    pub fn connection_settings_outcome(&mut self, hash: &[u8], result: Result<(), &str>) {
        self.connection_settings_status = Some(match result {
            Ok(()) => ConnectionSettingsStatus {
                last_connection_settings_hash: hash.to_vec(),
                status: ConnectionSettingsStatuses::Applied as i32,
                error_message: String::new(),
            },
            Err(error) => ConnectionSettingsStatus {
                last_connection_settings_hash: hash.to_vec(),
                status: ConnectionSettingsStatuses::Failed as i32,
                error_message: error.to_string(),
            },
        });
        self.send_settings_status = true;
    }

    /// An Agent with a Managed Process behind it (a Supervisor-backed Agent, ADR-0011). Only
    /// such an Agent accepts a restart command — the self-Agent has no process to restart.
    pub fn supervised(name: String, storage: Storage) -> std::io::Result<Self> {
        let mut state = Self::new(name, storage)?;
        state.managed = true;
        state.declare_capability(AgentCapabilities::AcceptsRestartCommand);
        Ok(state)
    }

    /// A restart the Server commanded and the process adapter has not been handed yet.
    pub fn take_pending_restart(&mut self) -> bool {
        std::mem::take(&mut self.pending_restart)
    }

    /// Adds one capability to this Agent's declared set — heartbeats when enabled, and bits an
    /// Agent only earns situationally (a Managed Process to restart, components to report).
    pub fn declare_capability(&mut self, capability: AgentCapabilities) {
        self.capabilities |= capability as u64;
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

    /// The Managed Process reported its available components. Only now does the Agent declare
    /// `ReportsAvailableComponents` — a capability without components would be a false promise —
    /// and the next full report carries the hash (the Server flags for the full map on demand).
    pub fn set_available_components(&mut self, components: AvailableComponents) {
        self.available_components = Some(components);
        self.declare_capability(AgentCapabilities::ReportsAvailableComponents);
        self.send_full = true;
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
            capabilities: self.capabilities,
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
        if self.send_full || self.send_settings_status {
            msg.connection_settings_status = self.connection_settings_status.clone();
        }
        // Available components ride the Baseline's two-step shape: the hash in every full
        // snapshot, the full map only when the Server demanded it via ReportAvailableComponents.
        if let Some(components) = &self.available_components {
            if self.send_components_full {
                msg.available_components = Some(components.clone());
            } else if self.send_full {
                msg.available_components = Some(AvailableComponents {
                    components: Default::default(),
                    hash: components.hash.clone(),
                });
            }
        }
        self.send_full = false;
        self.send_status = false;
        self.send_health = false;
        self.send_components_full = false;
        self.send_settings_status = false;
        msg
    }

    /// The final message of a connection: the Baseline requires `agent_disconnect` in it.
    pub fn disconnect_message(&mut self) -> AgentToServer {
        self.sequence_num += 1;
        AgentToServer {
            instance_uid: self.uid.as_bytes().to_vec(),
            sequence_num: self.sequence_num,
            capabilities: self.capabilities,
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

        // A command message carries only identity, capabilities, and the command — the Baseline
        // says every other field is to be ignored, so this branch returns before touching them.
        if let Some(command) = &reply.command {
            if command.r#type == opamp::proto::CommandType::Restart as i32 && self.managed {
                info!("the server commanded a restart");
                self.pending_restart = true;
            } else {
                // Restart is the only command the Baseline defines; and the self-Agent never
                // declares AcceptsRestartCommand, so a command toward it is a Server error.
                warn!(r#type = command.r#type, "ignoring an unsupported command");
            }
            return handled;
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

        if reply.flags & ServerToAgentFlags::ReportAvailableComponents as u64 != 0
            && self.available_components.is_some()
        {
            self.send_components_full = true;
            handled.send_report = true;
        }

        if let Some(remote_config) = &reply.remote_config {
            self.apply(remote_config);
            handled.send_report = true;
        }

        // A connection-settings offer (ADR-0014): acknowledge APPLYING and hand it to the
        // transport, which alone can verify by actually connecting — the Baseline's MUST. Only
        // an offer this Agent already runs (APPLIED, same hash) is not re-entered; a re-offer
        // after FAILED or a lost in-flight verification retries.
        if let Some(offers) = &reply.connection_settings {
            let applied = self.connection_settings_status.as_ref().is_some_and(|s| {
                s.last_connection_settings_hash == offers.hash
                    && s.status == ConnectionSettingsStatuses::Applied as i32
            });
            if offers.opamp.is_some() && !applied {
                info!(hash = %hex::encode(&offers.hash), "connection settings offered; verifying");
                self.connection_settings_status = Some(ConnectionSettingsStatus {
                    last_connection_settings_hash: offers.hash.clone(),
                    status: ConnectionSettingsStatuses::Applying as i32,
                    error_message: String::new(),
                });
                self.send_settings_status = true;
                handled.send_report = true;
                handled.connection_offer = Some(offers.clone());
            }
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
    fn declared_capabilities_ride_every_report_and_the_goodbye() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let base = agent.next_report().capabilities;
        assert_eq!(base, AGENT_CAPABILITIES);
        assert_eq!(base & AgentCapabilities::ReportsHeartbeat as u64, 0);

        agent.declare_capability(AgentCapabilities::ReportsHeartbeat);
        let declared = agent.next_report().capabilities;
        assert_eq!(declared, base | AgentCapabilities::ReportsHeartbeat as u64);
        assert_eq!(agent.disconnect_message().capabilities, declared);
    }

    #[test]
    fn a_restart_command_is_queued_by_supervised_agents_and_ignores_other_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("supervised")).expect("storage");
        let mut supervised = AgentState::supervised("s".to_string(), storage).expect("agent");
        assert_ne!(
            supervised.next_report().capabilities & AgentCapabilities::AcceptsRestartCommand as u64,
            0,
            "a supervised agent declares restartability"
        );

        // A command message per the Baseline: every field besides identity, capabilities, and
        // the command is ignored — the piggybacked remote_config must not be applied.
        let command_with_config = ServerToAgent {
            command: Some(opamp::proto::ServerToAgentCommand {
                r#type: opamp::proto::CommandType::Restart as i32,
            }),
            remote_config: Some(remote_config(b"x: 1\n", b"sneaky")),
            ..Default::default()
        };
        supervised.handle(&command_with_config);
        assert!(supervised.take_pending_restart());
        assert!(!supervised.take_pending_restart(), "taken exactly once");
        assert!(
            supervised.take_pending_apply().is_none(),
            "the piggybacked config is ignored"
        );

        // The self-Agent never declares the capability and ignores the command.
        let mut this = make_agent(&dir.path().join("self"));
        assert_eq!(
            this.next_report().capabilities & AgentCapabilities::AcceptsRestartCommand as u64,
            0
        );
        this.handle(&command_with_config);
        assert!(!this.take_pending_restart());
    }

    #[test]
    fn a_connection_offer_is_acknowledged_applying_and_handed_to_the_transport() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let _ = agent.next_report();

        // Every Agent declares it can accept and report on connection settings (ADR-0014).
        let caps = agent.next_report().capabilities;
        assert_ne!(
            caps & AgentCapabilities::AcceptsOpAmpConnectionSettings as u64,
            0
        );
        assert_ne!(
            caps & AgentCapabilities::ReportsConnectionSettingsStatus as u64,
            0
        );

        let offer = ConnectionSettingsOffers {
            hash: b"offer-1".to_vec(),
            opamp: Some(opamp::proto::OpAmpConnectionSettings {
                destination_endpoint: "wss://new/v1/opamp".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let handled = agent.handle(&ServerToAgent {
            connection_settings: Some(offer.clone()),
            ..Default::default()
        });
        assert!(handled.send_report);
        assert_eq!(handled.connection_offer, Some(offer.clone()));

        // The next report acknowledges APPLYING with the offer hash.
        let status = agent
            .next_report()
            .connection_settings_status
            .expect("status");
        assert_eq!(status.last_connection_settings_hash, b"offer-1");
        assert_eq!(status.status, ConnectionSettingsStatuses::Applying as i32);

        // The transport verified: APPLIED, and the same offer is not re-entered.
        agent.connection_settings_outcome(b"offer-1", Ok(()));
        let applied = agent
            .next_report()
            .connection_settings_status
            .expect("status");
        assert_eq!(applied.status, ConnectionSettingsStatuses::Applied as i32);
        let handled = agent.handle(&ServerToAgent {
            connection_settings: Some(offer),
            ..Default::default()
        });
        assert_eq!(
            handled.connection_offer, None,
            "an already-applied offer is not verified again"
        );
    }

    #[test]
    fn a_failed_offer_still_reports_the_hash_so_the_server_stops_reoffering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let _ = agent.next_report();

        let offer = ConnectionSettingsOffers {
            hash: b"offer-2".to_vec(),
            opamp: Some(opamp::proto::OpAmpConnectionSettings::default()),
            ..Default::default()
        };
        agent.handle(&ServerToAgent {
            connection_settings: Some(offer.clone()),
            ..Default::default()
        });
        let _ = agent.next_report();
        agent.connection_settings_outcome(b"offer-2", Err("could not connect"));

        let failed = agent
            .next_report()
            .connection_settings_status
            .expect("status");
        assert_eq!(failed.status, ConnectionSettingsStatuses::Failed as i32);
        assert_eq!(failed.last_connection_settings_hash, b"offer-2");
        assert_eq!(failed.error_message, "could not connect");

        // A re-offer of the failed hash is retried (the Server may have fixed the credential).
        let handled = agent.handle(&ServerToAgent {
            connection_settings: Some(offer),
            ..Default::default()
        });
        assert!(handled.connection_offer.is_some());
    }

    #[test]
    fn available_components_report_the_hash_and_the_map_only_on_demand() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let _ = agent.next_report();

        let full = AvailableComponents {
            components: HashMap::from([(
                "receiver/otlp".to_string(),
                opamp::proto::ComponentDetails::default(),
            )]),
            hash: b"components-hash".to_vec(),
        };
        agent.set_available_components(full.clone());

        // The next (full) report declares the bit and carries the hash only.
        let report = agent.next_report();
        assert_ne!(
            report.capabilities & AgentCapabilities::ReportsAvailableComponents as u64,
            0
        );
        let carried = report.available_components.expect("the hash announcement");
        assert!(carried.components.is_empty());
        assert_eq!(carried.hash, full.hash);

        // The Server demands the full map: exactly the next report carries it, once.
        let handled = agent.handle(&ServerToAgent {
            flags: ServerToAgentFlags::ReportAvailableComponents as u64,
            ..Default::default()
        });
        assert!(handled.send_report);
        let demanded = agent.next_report().available_components.expect("the map");
        assert!(demanded.components.contains_key("receiver/otlp"));
        assert!(agent.next_report().available_components.is_none());
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
