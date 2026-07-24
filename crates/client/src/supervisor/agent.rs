//! One Agent's state machine: builds `AgentToServer` reports and reacts to `ServerToAgent`
//! replies.
//!
//! Transport-agnostic on purpose (ADR-0012): the WebSocket and plain-HTTP loops feed the same
//! state machine, so transport is carriage, never semantics. The [`Engine`](crate::engine)
//! carries n of these over one connection (ADR-0009, ADR-0015) — a Supervisor-backed Agent and
//! the self-Agent fallback are the same state machine.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opamp::proto::{
    AgentCapabilities, AgentDescription, AgentDisconnect, AgentRemoteConfig, AgentToServer,
    AvailableComponents, ComponentHealth, ConnectionSettingsOffers, ConnectionSettingsStatus,
};
use opamp::uid::InstanceUid;
use tracing::{error, info, warn};


/// The base Capability Set every Agent of this Client declares (see docs/CONFORMANCE.md).
/// Individual Agents declare more via [`AgentState::declare_capability`] — e.g. heartbeats when
/// enabled, restartability only where a Managed Process exists.
pub const AGENT_CAPABILITIES: u64 = AgentCapabilities::ReportsStatus as u64
    | AgentCapabilities::AcceptsRemoteConfig as u64
    | AgentCapabilities::ReportsEffectiveConfig as u64
    | AgentCapabilities::ReportsRemoteConfig as u64

/// What a handled `ServerToAgent` asks of the transport loop.
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
    /// The Managed Process's health — derived or self-reported (ADR-0015). Absent for the
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
}

impl AgentState {
    /// Restores identity and configuration from storage, so a restart reports the same Agent with
    /// the same applied config hash — and is therefore not reconfigured redundantly.
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
        })
    }

        let running = opamp::version::current();
            error_message: String::new(),
        });
                error_message: String::new(),
            },
    /// An Agent with a Managed Process behind it (a Supervisor-backed Agent, ADR-0015). Only
    /// such an Agent accepts a restart command — the self-Agent has no process to restart.
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

    pub fn uid(&self) -> InstanceUid {
        self.uid
    }

    /// A configuration stored `APPLYING` and not yet handed to the process adapter, if any.
    pub fn take_pending_apply(&mut self) -> Option<AgentRemoteConfig> {
        self.pending_apply.take()
    }

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

                    error_message: String::new(),
                });
        handled
    }

            handled.send_report = true;
    fn apply(&mut self, config: &AgentRemoteConfig) {
            }
        }
        self.send_status = true;
    }

    fn server_accepts_effective_config(&self) -> bool {
        // Until the Server has declared anything, report optimistically; once it has, its word is
        // binding ("Interoperability of Partial Implementations").
        self.server_capabilities
            .unwrap_or(true)
    }

    fn describe(&self) -> AgentDescription {
        let mut identifying_attributes =
            vec![string_attr(attributes::SERVICE_NAME, &self.service_name)];
            identifying_attributes.push(string_attr(attributes::SERVICE_NAMESPACE, namespace));
            identifying_attributes.push(string_attr(
                attributes::SERVICE_VERSION,
                opamp::version::current(),
            ));
            string_attr(attributes::SERVICE_INSTANCE_NAME, &self.instance_name),
            string_attr(attributes::OS_TYPE, os_type()),
            string_attr(attributes::HOST_ARCH, host_arch()),
            (attributes::OS_DESCRIPTION, os.description.as_deref()),
        let mut description = AgentDescription {
        };
        if let Some(reported) = &self.process_description {
            let supervisors_own = |key: &str| {
                key == "service.instance.id" || key == attributes::SERVICE_INSTANCE_NAME
            };
            for attr in &reported.identifying_attributes {
                    upsert_attr(&mut description.identifying_attributes, attr);
                }
            }
            for attr in &reported.non_identifying_attributes {
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

/// OpenTelemetry semantic-convention value for `os.type` (Rust says "macos", the convention
/// "darwin").
fn os_type() -> &'static str {
    attributes::canonical_os(std::env::consts::OS)
}

    attributes::canonical_arch(std::env::consts::ARCH)
pub(crate) fn host_name() -> Option<&'static str> {
fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::proto::{AgentConfigMap, AgentConfigObject};
    use std::collections::HashMap;

        let dir = tempfile::tempdir().expect("tempdir");
    fn make_agent(dir: &std::path::Path) -> AgentState {
        let storage = Storage::new(dir.to_path_buf()).expect("storage");
        AgentState::new("test-agent".to_string(), storage).expect("agent")
    }

    fn remote_config(body: &[u8], hash: &[u8]) -> AgentRemoteConfig {
        AgentRemoteConfig {
            config: Some(AgentConfigMap {
                config_map: HashMap::from([(
                    String::new(),
                    AgentConfigObject {
                        role: String::new(),
                        body: body.to_vec(),
                        content_type: String::new(),
                    },
                )]),
            }),
            config_hash: hash.to_vec(),
        }
    }

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
                .find(|kv| kv.key == key)
                .and_then(|kv| kv.value.as_ref())
                .and_then(|v| v.value.as_ref())
                .map(|v| match v {
                    opamp::proto::any_value::Value::StringValue(s) => s.clone(),
                    other => format!("{other:?}"),
                })
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
            Some(opamp::version::current())
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
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

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let handled = agent.handle(&ServerToAgent {
        let handled = agent.handle(&ServerToAgent {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let handled = agent.handle(&ServerToAgent {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
                version: opamp::version::parse(opamp::version::current())
        let handled = agent.handle(&ServerToAgent {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let handled = agent.handle(&ServerToAgent {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let handled = agent.handle(&ServerToAgent {
            ..Default::default()
        });
        assert!(handled.send_report);
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let _ = agent.next_report();

        let handled = agent.handle(&ServerToAgent {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        agent.handle(&ServerToAgent {
            capabilities: ServerCapabilities::AcceptsStatus as u64,
                ..Default::default()
            }),
            ..Default::default()
        });
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        agent.handle(&ServerToAgent {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let handled = agent.handle(&ServerToAgent {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
                ..Default::default()
            }),
            ..Default::default()
        let handled = agent.handle(&ServerToAgent {
            ..Default::default()
        });
        assert!(handled.send_report);
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let _ = agent.next_report();
        agent.handle(&ServerToAgent {
        let handled = agent.handle(&ServerToAgent {
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
        assert_eq!(status.last_remote_config_hash, b"hash-1");
        assert_eq!(status.status, RemoteConfigStatuses::Applied as i32);
        assert_eq!(status.last_remote_config_hash, b"hash-1");
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
                remote_config: Some(remote_config(b"x: 1\n", b"hash-1")),
                ..Default::default()
            });
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
