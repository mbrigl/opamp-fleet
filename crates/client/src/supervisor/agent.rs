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
    PackageDownloadDetails, PackageStatus, PackageStatusEnum, PackageStatuses, PackageType,
    RemoteConfigStatus, RemoteConfigStatuses, ServerCapabilities, ServerErrorResponseType,
    ServerToAgent, ServerToAgentFlags,
};
use opamp::uid::InstanceUid;
use tracing::{error, info, warn};

use crate::packages::PackageDownload;
use crate::storage::{InstalledPackage, Storage};

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
    /// A package to download, verify, and hand to the Supervisor (ADR-0015). The state machine
    /// has acknowledged `Installing`; the transport owns the download and verification.
    pub package_download: Option<PackageDownload>,
}

/// The Agent type the Client's own Agent presents as `service.name` (ADR-0028, ADR-0033). It is
/// the shipped binary's name and a constant, not the configured instance name: every Client in a
/// fleet is the same kind of thing, and that is what a type says.
pub const CLIENT_SERVICE_NAME: &str = "opamp-fleet-client";

pub struct AgentState {
    uid: InstanceUid,
    sequence_num: u64,
    /// The operator's name for this Agent, reported as `service.instance.name` (ADR-0033): the
    /// `[[supervisor]]` block's `name`, or the top-level one for the Client's own Agent. Never
    /// `service.name` — that is the type below.
    instance_name: String,
    /// The Agent *type*, reported as `service.name`. A Managed Process that reports one of its own
    /// replaces it in the fold; this is what stands until then, and permanently for a process that
    /// reports nothing.
    service_name: String,
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
    /// Whether this Agent's Managed Process is updated from Server-offered packages (ADR-0015).
    /// Which package that is, is the Server's choice (ADR-0017) — this side only consents.
    accepts_packages: bool,
    /// The only package name this Agent will install (ADR-0020); `None` takes whichever top-level
    /// package the Server selects, which is what a Supervisor does (ADR-0017).
    expected_package: Option<String>,
    /// The name of the top-level package the Server last offered. Learned from the offer, not
    /// configured: it keys the reported `PackageStatuses` map, which the Baseline requires to name
    /// every package the Agent has or is processing.
    offered_name: Option<String>,
    /// The package currently installed, persisted across restarts.
    installed_package: Option<InstalledPackage>,
    /// Progress of the artifact download in flight (ADR-0015), reported as `Downloading` with
    /// `PackageDownloadDetails`; `None` once the bytes are on disk. `[Development]` upstream.
    downloading: Option<PackageDownloadDetails>,
    /// The package hash currently downloading/installing, so a repeated offer of the same hash is
    /// not re-entered while it is in flight.
    installing: Option<PackageDownload>,
    /// The `all_packages_hash` last offered, echoed as `server_provided_all_packages_hash` once
    /// the Agent's package reaches a terminal state — which is what stops the Server re-offering.
    offered_all_packages_hash: Vec<u8>,
    /// What the Agent reports as `server_provided_all_packages_hash`: the offered aggregate once
    /// terminal, empty (or the previous value) while an install is in flight.
    echoed_all_packages_hash: Vec<u8>,
    /// The version and hash the Server last offered for this Agent's package — reported as
    /// `server_offered_version`/`server_offered_hash` (the Baseline requires them while
    /// installing or after a failure).
    server_offered: Option<(String, Vec<u8>)>,
    /// The last install failure for this Agent's package, reported alongside the status.
    package_error: String,
    /// A failure in processing the *offer itself* rather than one package — the Baseline's
    /// `PackageStatuses.error_message`, "set if the Agent encountered an error when processing the
    /// PackagesAvailable message and that error is not related to any particular single package".
    offer_error: String,
    send_package_status: bool,
    /// Operator-defined attributes from `client.toml` (ADR-0012), reported as non-identifying
    /// attributes so Selectors can target them. Reported attributes win on key collision.
    configured_attributes: Vec<(String, String)>,
    /// The deployment's `service.namespace`, when it has one. The Baseline asks for it "if it is
    /// used in the environment where the Agent runs" — which only an operator knows, so it is
    /// configured rather than detected, and absent until it is set.
    namespace: Option<String>,
}

impl AgentState {
    /// Restores identity and configuration from storage, so a restart reports the same Agent with
    /// the same applied config hash — and is therefore not reconfigured redundantly.
    ///
    /// `instance_name` is the operator's name for this Agent; the type it presents is
    /// [`CLIENT_SERVICE_NAME`], since this constructor builds the Client's own Agent. A
    /// Supervisor-backed one comes from [`supervised`](Self::supervised), which is told its type.
    pub fn new(instance_name: String, storage: Storage) -> std::io::Result<Self> {
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
            instance_name,
            service_name: CLIENT_SERVICE_NAME.to_string(),
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
            accepts_packages: false,
            expected_package: None,
            offered_name: None,
            installed_package: None,
            downloading: None,
            installing: None,
            offered_all_packages_hash: Vec::new(),
            echoed_all_packages_hash: Vec::new(),
            server_offered: None,
            package_error: String::new(),
            offer_error: String::new(),
            send_package_status: false,
            configured_attributes: Vec::new(),
            namespace: None,
        })
    }

    /// Opts this Agent into package delivery (ADR-0015): declares `AcceptsPackages` and
    /// `ReportsPackageStatuses`, and restores what it last installed so a restarted Client reports
    /// the version it runs and is not re-offered it.
    ///
    /// It consents; it does not choose. Which artifact arrives is decided by the Selector on the
    /// Server (ADR-0017), so a rollout is aimed centrally rather than from this host's file.
    pub fn accept_packages(&mut self) {
        self.accepts_packages = true;
        self.installed_package = self.storage.load_package();
        self.declare_capability(AgentCapabilities::AcceptsPackages);
        self.declare_capability(AgentCapabilities::ReportsPackageStatuses);
    }

    /// Opts this Agent into package delivery for **one named package only** (ADR-0020) — what the
    /// Client's own Agent does. A Supervisor takes whichever top-level package the Server selects
    /// for it (ADR-0017), because the worst case there is a Managed Process that will not start
    /// and is rolled back. The Client has no such safety net: a package with an empty Selector
    /// reaches every consenting Agent, and one written over this binary takes the host out of
    /// reach for good. So this side matches the name and refuses everything else.
    pub fn accept_packages_named(&mut self, name: String) {
        self.accept_packages();
        self.expected_package = Some(name);
    }

    /// The package this Agent is processing or has: the one being installed, else the installed
    /// one, else the one last offered. `None` until the Server offers anything — an Agent that has
    /// no package reports none, which is what "all packages the Agent has" amounts to.
    fn package_name(&self) -> Option<String> {
        self.installing
            .as_ref()
            .map(|d| d.name.clone())
            .or_else(|| self.installed_package.as_ref().map(|p| p.name.clone()))
            .or_else(|| self.offered_name.clone())
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
    ///
    /// The type is a parameter rather than a builder default because there is no sensible default
    /// for it (ADR-0033): the Client's own type would be a lie, and the instance name in that slot
    /// is exactly the confusion this signature exists to prevent. The caller always knows one —
    /// the block's `service_name`, or the program's file name.
    pub fn supervised(
        instance_name: String,
        service_name: String,
        storage: Storage,
    ) -> std::io::Result<Self> {
        let mut state = Self::new(instance_name, storage)?;
        state.service_name = service_name;
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

    /// Attaches the deployment's `service.namespace`, reported by every Agent this Client presents.
    /// `None` — the default — reports nothing, which is what the Baseline's "if it is used in the
    /// environment" amounts to for a deployment that does not use one.
    #[must_use]
    pub fn with_namespace(mut self, namespace: Option<String>) -> Self {
        self.namespace = namespace;
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
        // Merged per attribute, not replaced wholesale: two sources describe the same process —
        // the version probe, which reports `service.version` and nothing else, and the
        // opampextension, which reports everything else — and whichever speaks second must not
        // erase what the first said. On the same key the later report wins, so a swapped binary's
        // probed version replaces the one it succeeded.
        let merged = self
            .process_description
            .get_or_insert_with(AgentDescription::default);
        for attr in &description.identifying_attributes {
            upsert_attr(&mut merged.identifying_attributes, attr);
        }
        for attr in &description.non_identifying_attributes {
            upsert_attr(&mut merged.non_identifying_attributes, attr);
        }
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
        if self.accepts_packages && (self.send_full || self.send_package_status) {
            msg.package_statuses = Some(self.package_statuses());
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
        self.send_package_status = false;
        msg
    }

    /// This Agent's package status as one `PackageStatuses`: its single package's state, plus the
    /// `server_provided_all_packages_hash` the Server compares to gate re-offering.
    fn package_statuses(&self) -> PackageStatuses {
        // Every package the Agent has or is processing — which is at most one, and none until
        // the Server has offered something. The aggregate still rides an empty map: it is what
        // tells the Server this Agent is in sync with an offer of nothing.
        let Some(name) = self.package_name() else {
            return PackageStatuses {
                packages: Default::default(),
                server_provided_all_packages_hash: self.echoed_all_packages_hash.clone(),
                error_message: self.offer_error.clone(),
            };
        };
        // `agent_has_*` is what the Agent actually runs — the last successful install, if any.
        let (has_version, has_hash) = self
            .installed_package
            .as_ref()
            .map(|p| {
                (
                    p.version.clone(),
                    hex::decode(&p.hash_hex).unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        let status = if self.downloading.is_some() {
            // Still fetching the artifact. Reported apart from `Installing` because a download of
            // a few hundred megabytes is the part that takes minutes — the Server would otherwise
            // watch a silent `Installing` and have no way to tell progress from a hang.
            PackageStatusEnum::Downloading
        } else if self.installing.is_some() {
            // Downloaded and verified; the Supervisor is applying it.
            PackageStatusEnum::Installing
        } else if !self.package_error.is_empty() {
            // The last attempt failed — a refusal is a report, not a silence.
            PackageStatusEnum::InstallFailed
        } else if self.installed_package.is_some() {
            PackageStatusEnum::Installed
        } else {
            PackageStatusEnum::InstallPending
        };
        // While installing, `agent_has_version` is still the old one (we have not switched yet).
        let (version, hash) = (has_version, has_hash);
        let (offered_version, offered_hash) = self
            .server_offered
            .clone()
            .unwrap_or_else(|| (String::new(), Vec::new()));
        let package = PackageStatus {
            name: name.clone(),
            agent_has_version: version,
            agent_has_hash: hash,
            server_offered_version: offered_version,
            server_offered_hash: offered_hash,
            status: status as i32,
            error_message: self.package_error.clone(),
            // "Should only be set if status is Downloading" — so it rides exactly that status.
            download_details: self.downloading,
        };
        PackageStatuses {
            packages: [(name.clone(), package)].into(),
            server_provided_all_packages_hash: self.echoed_all_packages_hash.clone(),
            error_message: self.offer_error.clone(),
        }
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

        // A package offer (ADR-0015): act only on this Agent's named package. Download and
        // verification are the transport's; the state machine acknowledges Installing and hands
        // over the coordinates.
        if let Some(available) = reply.packages_available.as_ref() {
            self.handle_package_offer(available, &mut handled);
        }

        handled
    }

    /// Reacts to a `PackagesAvailable` offer: the Server selected what this Agent may have
    /// (ADR-0017), so this side takes the one **top-level** package out of the offer — the binary
    /// of its Managed Process — and ignores addons, which a Supervisor has no way to apply.
    fn handle_package_offer(
        &mut self,
        offer: &opamp::proto::PackagesAvailable,
        handled: &mut Handled,
    ) {
        if !self.accepts_packages {
            return;
        }
        self.offered_all_packages_hash = offer.all_packages_hash.clone();
        // The Baseline: "There is normally only one top-level package, which implements the
        // primary functionality of the Agent." The Server refuses to create an overlap, so more
        // than one here means a peer that does not — refused rather than picked from at random.
        let mut top_level = offer
            .packages
            .iter()
            .filter(|(_, available)| available.r#type != PackageType::Addon as i32);
        let Some((name, available)) = top_level.next() else {
            // Nothing top-level for us. An empty offer is simply "nothing for this Agent"; an
            // offer of addons only is an operator error — a Supervisor replaces one binary and
            // knows nothing about addons — so that one is reported rather than passed over.
            if !offer.packages.is_empty() {
                error!(
                    packages = offer.packages.len(),
                    "refusing an offer of addons only: this Client installs top-level packages only"
                );
                self.installing = None;
                self.offer_error =
                    "this Client installs top-level packages only; the offer carries addons only"
                        .to_string();
                handled.send_report = true;
            }
            self.echoed_all_packages_hash = offer.all_packages_hash.clone();
            self.send_package_status = true;
            return;
        };
        if let Some((second, _)) = top_level.next() {
            error!(
                first = %name, second = %second,
                "refusing an offer with two top-level packages: an Agent has one binary to replace"
            );
            self.installing = None;
            self.offer_error = format!(
                "the Server offered two top-level packages ({name:?} and {second:?}); \
                 an Agent has one binary to replace"
            );
            self.echoed_all_packages_hash = offer.all_packages_hash.clone();
            self.send_package_status = true;
            handled.send_report = true;
            return;
        }
        let name = name.clone();
        // The Client's own Agent takes one named package and nothing else (ADR-0020). A
        // fleet-wide package with an empty Selector reaches every consenting Agent, so without
        // this an artifact meant for a Collector would be written over this binary and the host
        // would be gone. Refused and reported, never silently ignored.
        if let Some(expected) = &self.expected_package {
            if &name != expected {
                error!(
                    offered = %name, expected = %expected,
                    "refusing a package this Agent was not configured to take"
                );
                self.installing = None;
                self.offer_error = format!(
                    "this Agent installs only the package {expected:?}; the Server offered {name:?}"
                );
                self.echoed_all_packages_hash = offer.all_packages_hash.clone();
                self.send_package_status = true;
                handled.send_report = true;
                return;
            }
        }
        // A usable offer clears whatever the last unusable one complained about.
        self.offer_error.clear();
        self.offered_name = Some(name.clone());
        self.server_offered = Some((available.version.clone(), available.hash.clone()));
        let installed_hash = self
            .installed_package
            .as_ref()
            .map(|p| hex::decode(&p.hash_hex).unwrap_or_default());
        if installed_hash.as_deref() == Some(available.hash.as_slice()) {
            // Already running this package: in sync — echo the aggregate to end the offer.
            self.echoed_all_packages_hash = offer.all_packages_hash.clone();
            self.send_package_status = true;
            return;
        }
        let in_flight = self
            .installing
            .as_ref()
            .is_some_and(|d| d.hash == available.hash);
        if in_flight {
            return; // Already downloading/installing this exact package.
        }
        let Some(file) = &available.file else {
            warn!(package = %name, "package offer carries no downloadable file; ignoring");
            return;
        };
        let download = PackageDownload {
            name: name.clone(),
            version: available.version.clone(),
            hash: available.hash.clone(),
            download_url: file.download_url.clone(),
            content_hash: file.content_hash.clone(),
            signature: file.signature.clone(),
        };
        info!(package = %name, version = %available.version, "package offered; installing");
        self.installing = Some(download.clone());
        self.package_error.clear();
        self.send_package_status = true;
        handled.send_report = true;
        handled.package_download = Some(download);
    }

    /// Records how far the artifact download has got (ADR-0015), so the next report carries
    /// `Downloading` with the details. The Baseline only *permits* these interim reports; without
    /// them a multi-hundred-megabyte download is indistinguishable from a stuck install.
    pub fn package_downloading(&mut self, details: PackageDownloadDetails) {
        self.downloading = Some(details);
        self.send_package_status = true;
    }

    /// The artifact is downloaded and verified; what follows is the Supervisor applying it, which
    /// the Baseline reports as `Installing` rather than `Downloading`.
    pub fn package_downloaded(&mut self) {
        self.downloading = None;
        self.send_package_status = true;
    }

    /// Closes a package's lifecycle the Supervisor applied (ADR-0015): `Ok(version)` records it
    /// Installed and persists it; `Err` reports InstallFailed (the binary was rolled back). Either
    /// way the offered aggregate is echoed, so the Server stops re-offering the same bytes — a
    /// refusal is a report, not a loop.
    pub fn package_applied(&mut self, hash: Vec<u8>, result: Result<String, String>) {
        // Keep the name the outcome is about: `installing` is cleared here, and a failure leaves
        // no installed record to fall back on, but the status still has to name its package.
        if let Some(name) = self.installing.as_ref().map(|d| d.name.clone()) {
            self.offered_name = Some(name);
        }
        self.installing = None;
        self.downloading = None;
        match result {
            Ok(version) => {
                let installed = InstalledPackage {
                    name: self.package_name().unwrap_or_default(),
                    version: version.clone(),
                    hash_hex: hex::encode(&hash),
                };
                if let Err(e) = self.storage.store_package(&installed) {
                    warn!(error = %e, "cannot persist the installed package record");
                }
                info!(version = %version, "package installed");
                self.installed_package = Some(installed);
                self.package_error.clear();
            }
            Err(error) => {
                error!(error = %error, "package installation failed");
                self.package_error = error;
            }
        }
        self.echoed_all_packages_hash = self.offered_all_packages_hash.clone();
        self.send_package_status = true;
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
        // `service.name` is the Agent *type* — the Baseline's "reverse FQDN that uniquely
        // identifies the Agent type" (ADR-0033). It used to carry the instance name, which a
        // Managed Process reporting its own type then destroyed; the instance name now has its own
        // key below, out of the way of the fold.
        let mut identifying_attributes = vec![string_attr("service.name", &self.service_name)];
        // The Baseline lists `service.namespace` second, among what identifies the Agent — it says
        // *which* deployment this service belongs to, so it belongs beside the name rather than
        // among the tags an operator hangs on it.
        if let Some(namespace) = &self.namespace {
            identifying_attributes.push(string_attr("service.namespace", namespace));
        }
        // `service.version` is the *Agent's* version. The self-Agent is the Client, so its baked
        // version is the truth; a Supervisor-backed Agent stands for its Managed Process, whose
        // version only the process itself can report (folded in below, goal 16) — never invented
        // from the Client's.
        if !self.managed {
            identifying_attributes.push(string_attr("service.version", crate::version::version()));
        }
        identifying_attributes.push(string_attr("service.instance.id", &self.uid.to_string()));
        // The rest of what the Baseline asks for "to describe where the Agent runs": `os.*` and
        // `host.*`. Every one of them is best effort, and what the platform cannot answer is left
        // out rather than filled in — an absent attribute says "unknown", where one carrying a
        // placeholder would say something false that a Selector could then match.
        let os = os_info();
        let mut non_identifying_attributes = vec![
            // The operator's name for this Agent (ADR-0033). Non-identifying because the Baseline
            // has no key for a human instance name and admits "any user-defined attributes the end
            // user would like to associate with this Agent" here; identity itself stays
            // `service.instance.id`. A Selector can match it, which is how ADR-0017's "pin one
            // host" is expressed for a machine running several Supervisors.
            string_attr("service.instance.name", &self.instance_name),
            string_attr("os.type", os_type()),
            string_attr("host.arch", host_arch()),
        ];
        for (key, value) in [
            ("os.name", os.name.as_deref()),
            ("os.version", os.version.as_deref()),
            ("os.description", os.description.as_deref()),
            ("host.name", host_name()),
            ("host.id", host_id()),
        ] {
            if let Some(value) = value {
                non_identifying_attributes.push(string_attr(key, value));
            }
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
        // Fold in what the Managed Process reported about itself — except the two attributes that
        // are the Supervisor's to state: the Agent the Server sees is the Supervisor, keyed by the
        // Supervisor's uid (goal 16) and called what the operator called it (ADR-0033). A process
        // cannot know either, so a value it reports under those keys is not an improvement. Its
        // `service.name` deliberately *does* win — a Collector's `dist.name` is a better type than
        // anything this file can infer.
        if let Some(reported) = &self.process_description {
            for attr in &reported.identifying_attributes {
                if attr.key != "service.instance.id" {
                    upsert_attr(&mut description.identifying_attributes, attr);
                }
            }
            for attr in &reported.non_identifying_attributes {
                if attr.key != "service.instance.name" {
                    upsert_attr(&mut description.non_identifying_attributes, attr);
                }
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

/// OpenTelemetry semantic-convention value for `host.arch` — the convention says `amd64`/`arm64`
/// where Rust's constant says `x86_64`/`aarch64` (ADR-0031).
///
/// The Baseline points at the conventions for these keys, and the Collector's `opampextension`
/// reports `runtime.GOARCH`, which is already this vocabulary. Reporting Rust's spelling instead
/// meant the *same host* changed architecture depending on whether a Collector was running on it:
/// a Managed Process's attributes are folded over the Supervisor's, so `amd64` overwrote `x86_64`
/// and any Selector written against one of them stopped matching.
fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// What the operating system says about itself — the Baseline's `os.*`. Read **once** per process
/// and as one answer: two of the three platforms have to be asked by running a program, and asking
/// them once per attribute would start three processes to learn what one printout already holds.
/// A field the platform does not answer stays `None` and is then not reported at all.
#[derive(Default)]
struct OsInfo {
    /// `os.description` — the human-readable line: "Ubuntu 24.04.2 LTS".
    description: Option<String>,
    /// `os.name` — the system's own name, without a version: "Ubuntu", "macOS", "Windows".
    name: Option<String>,
    /// `os.version` — the version that name is at: "24.04", "15.5", "10.0.26100.2033".
    version: Option<String>,
}

fn os_info() -> &'static OsInfo {
    static INFO: std::sync::OnceLock<OsInfo> = std::sync::OnceLock::new();
    INFO.get_or_init(read_os_info)
}

#[cfg(target_os = "linux")]
fn read_os_info() -> OsInfo {
    // os-release(5): NAME="Ubuntu", VERSION_ID="24.04", PRETTY_NAME="Ubuntu 24.04.2 LTS".
    let Ok(text) = std::fs::read_to_string("/etc/os-release") else {
        return OsInfo::default();
    };
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|value| value.trim().trim_matches(['"', '\'']).to_string())
            .filter(|value| !value.is_empty())
    };
    OsInfo {
        description: field("PRETTY_NAME"),
        name: field("NAME"),
        version: field("VERSION_ID"),
    }
}

#[cfg(target_os = "macos")]
fn read_os_info() -> OsInfo {
    // `sw_vers` prints ProductName/ProductVersion/BuildVersion lines, e.g. "macOS" / "15.5".
    let Ok(output) = std::process::Command::new("sw_vers").output() else {
        return OsInfo::default();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim_start_matches(':').trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let name = field("ProductName");
    let version = field("ProductVersion");
    let description = match (&name, &version) {
        (Some(name), Some(version)) => Some(format!("{name} {version}")),
        (Some(name), None) => Some(name.clone()),
        _ => None,
    };
    OsInfo {
        description,
        name,
        version,
    }
}

#[cfg(windows)]
fn read_os_info() -> OsInfo {
    // `cmd /c ver` prints e.g. "Microsoft Windows [Version 10.0.26100.2033]".
    let Ok(output) = std::process::Command::new("cmd")
        .args(["/c", "ver"])
        .output()
    else {
        return OsInfo::default();
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return OsInfo::default();
    }
    // The version is what stands between "[Version " and "]". When that shape is not what this
    // Windows printed, the line is still the description — which is the whole of what this
    // platform offers instead of a structured answer.
    let version = text
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(inside, _)| inside.trim_start_matches("Version").trim().to_string())
        .filter(|value| !value.is_empty());
    OsInfo {
        description: Some(text),
        name: Some("Windows".to_string()),
        version,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn read_os_info() -> OsInfo {
    OsInfo::default()
}

/// The host's name (`host.name`) — read once. ADR-0017 twice offers a Selector on this attribute
/// as the way to pin one host to one artifact, so a fleet that does not report it cannot be aimed
/// at a machine at all.
fn host_name() -> Option<&'static str> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(read_host_name).as_deref()
}

#[cfg(unix)]
fn read_host_name() -> Option<String> {
    // HOST_NAME_MAX is 64 on Linux and 255 on macOS; 256 holds either. A name that had to be
    // truncated is not required to be terminated, which is why the end is scanned for rather
    // than assumed.
    let mut buffer = vec![0u8; 256];
    // SAFETY: `buffer` is writable for `buffer.len()` bytes and outlives the call.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    let name = String::from_utf8_lossy(&buffer[..end]).trim().to_string();
    (!name.is_empty()).then_some(name)
}

#[cfg(windows)]
fn read_host_name() -> Option<String> {
    // The SCM starts the service with the machine's environment, where COMPUTERNAME is always
    // set — so this one answer costs no process, unlike every other one this platform gives.
    std::env::var("COMPUTERNAME")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

#[cfg(not(any(unix, windows)))]
fn read_host_name() -> Option<String> {
    None
}

/// The host's installation identity (`host.id`) — read once. What still names the machine after it
/// has been renamed, which `host.name` by itself does not.
fn host_id() -> Option<&'static str> {
    static ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ID.get_or_init(read_host_id).as_deref()
}

#[cfg(target_os = "linux")]
fn read_host_id() -> Option<String> {
    // machine-id(5): 32 hex characters, generated once per installation. The D-Bus copy is where
    // it lives on systems that do not populate /etc/machine-id.
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .into_iter()
        .find_map(|path| {
            let value = std::fs::read_to_string(path).ok()?.trim().to_string();
            (!value.is_empty()).then_some(value)
        })
}

#[cfg(target_os = "macos")]
fn read_host_id() -> Option<String> {
    // ioreg prints `"IOPlatformUUID" = "…"` among the platform device's properties.
    let output = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let value = text
        .lines()
        .find(|line| line.contains("IOPlatformUUID"))?
        .split_once('=')?
        .1
        .trim()
        .trim_matches('"')
        .to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(windows)]
fn read_host_id() -> Option<String> {
    // The MachineGuid the installer writes. `reg query` prints
    // "    MachineGuid    REG_SZ    <guid>" and needs no registry binding to read.
    let output = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let value = text
        .lines()
        .find(|line| line.contains("MachineGuid"))?
        .split_whitespace()
        .last()?
        .to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn read_host_id() -> Option<String> {
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
                        role: String::new(),
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

    /// Every reported attribute as `key -> value`, both lists together: what the Server actually
    /// receives, and therefore what a Selector matches against.
    fn reported(description: &AgentDescription) -> std::collections::BTreeMap<String, String> {
        description
            .identifying_attributes
            .iter()
            .chain(&description.non_identifying_attributes)
            .map(|kv| {
                let value = match kv.value.as_ref().and_then(|v| v.value.as_ref()) {
                    Some(opamp::proto::any_value::Value::StringValue(s)) => s.clone(),
                    other => panic!("{} must be a string, got {other:?}", kv.key),
                };
                (kv.key.clone(), value)
            })
            .collect()
    }

    /// Best effort means **absent**, never blank. An attribute reported as an empty string is one
    /// a Selector can be written against and match, so a platform that cannot answer — this
    /// container has no `/etc/machine-id`, so it cannot answer `host.id` — must leave the key off
    /// entirely rather than report nothing under it.
    #[test]
    fn nothing_the_platform_cannot_answer_is_reported_as_an_empty_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (key, value) in reported(&make_agent(dir.path()).describe()) {
            assert!(!value.is_empty(), "{key} is reported as an empty value");
        }
    }

    /// The defect ADR-0033 exists for: a Collector's `opampextension` reports the type it was
    /// built with, and folding that in used to overwrite the operator's name for the Supervisor —
    /// so every Collector of one distribution collapsed onto one name in the fleet view. Both
    /// values must survive, each in its own key, each won by the side that actually knows it.
    #[test]
    fn a_process_reporting_its_type_does_not_take_the_operators_name_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol-edge-01".to_string(),
            "otelcol".to_string(),
            storage,
        )
        .expect("agent");

        let before = reported(&agent.describe());
        assert_eq!(
            before.get("service.name").map(String::as_str),
            Some("otelcol")
        );
        assert_eq!(
            before.get("service.instance.name").map(String::as_str),
            Some("otelcol-edge-01")
        );

        // The extension connects and states its `dist.name` — and, being a Collector, knows
        // nothing about the Supervisor that owns it, so its guess at an instance name is worthless.
        agent.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.name", "otelcol-contrib")],
            non_identifying_attributes: vec![string_attr(
                "service.instance.name",
                "some-collector",
            )],
        });

        let after = reported(&agent.describe());
        assert_eq!(
            after.get("service.name").map(String::as_str),
            Some("otelcol-contrib"),
            "the process states the better type and wins the type"
        );
        assert_eq!(
            after.get("service.instance.name").map(String::as_str),
            Some("otelcol-edge-01"),
            "but it cannot rename the Supervisor the operator configured"
        );
    }

    /// The Client's own Agent is one *kind* of thing across the whole fleet, so its type is the
    /// shipped binary's name (ADR-0028) and not whatever the operator called this instance —
    /// which is what makes `[self_update] package = "opamp-fleet-client"` line up with a Selector
    /// on the type (ADR-0033).
    #[test]
    fn the_clients_own_agent_reports_its_type_and_its_configured_name_separately() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let agent = AgentState::new("edge-fra1".to_string(), storage).expect("agent");
        let attributes = reported(&agent.describe());
        assert_eq!(
            attributes.get("service.name").map(String::as_str),
            Some(CLIENT_SERVICE_NAME)
        );
        assert_eq!(
            attributes.get("service.instance.name").map(String::as_str),
            Some("edge-fra1")
        );
    }

    /// `[supervisor.attributes]` is a fallback for keys nothing else reports, so it must not be a
    /// second way to set the two attributes the Supervisor itself owns.
    #[test]
    fn configured_attributes_cannot_restate_the_type_or_the_instance_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let agent = AgentState::supervised("edge-01".to_string(), "otelcol".to_string(), storage)
            .expect("agent")
            .with_attributes(
                [
                    ("service.name".to_string(), "hijacked".to_string()),
                    ("service.instance.name".to_string(), "hijacked".to_string()),
                ]
                .into(),
            );
        let attributes = reported(&agent.describe());
        assert_eq!(
            attributes.get("service.name").map(String::as_str),
            Some("otelcol")
        );
        assert_eq!(
            attributes.get("service.instance.name").map(String::as_str),
            Some("edge-01")
        );
    }

    /// ADR-0017 twice offers "a Selector matching that host's `host.name`" as the way to pin one
    /// host to one artifact. That only works if an Agent reports the attribute, which for a long
    /// time it did not.
    #[cfg(unix)]
    #[test]
    fn the_agent_reports_the_host_name_a_selector_would_pin_it_by() {
        let dir = tempfile::tempdir().expect("tempdir");
        let attributes = reported(&make_agent(dir.path()).describe());
        assert!(
            attributes.contains_key("host.name"),
            "no host.name among {attributes:?}"
        );
    }

    /// The Baseline names `os.version` beside `os.type`. `os.description` does not stand in for it:
    /// it is prose, and nothing can compare prose. Asserted against the file the values are read
    /// from, so this states a fact about the mapping rather than about the machine it runs on.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_os_is_reported_as_a_name_and_a_version_not_only_as_prose() {
        let Ok(release) = std::fs::read_to_string("/etc/os-release") else {
            return;
        };
        let field = |key: &str| {
            release
                .lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .map(|value| value.trim().trim_matches(['"', '\'']).to_string())
                .filter(|value| !value.is_empty())
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let attributes = reported(&make_agent(dir.path()).describe());
        assert_eq!(attributes.get("os.name").cloned(), field("NAME"));
        // `VERSION_ID` is optional in os-release(5) — a rolling distribution carries none — so
        // what is asserted is that the two agree, present or absent.
        assert_eq!(attributes.get("os.version").cloned(), field("VERSION_ID"));
        assert_ne!(
            attributes.get("os.name").map(String::as_str),
            Some(os_type()),
            "the distribution's NAME, not the os.type"
        );
    }

    /// `service.namespace` is the Baseline's one conditional attribute — "if it is used in the
    /// environment where the Agent runs" — so it is silent until an operator configures it, and
    /// then it *identifies* the Agent rather than tagging it.
    #[test]
    fn the_service_namespace_is_absent_until_configured_and_then_identifies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let named = |description: &AgentDescription| {
            let in_identifying = description
                .identifying_attributes
                .iter()
                .any(|kv| kv.key == "service.namespace");
            let in_non_identifying = description
                .non_identifying_attributes
                .iter()
                .any(|kv| kv.key == "service.namespace");
            (in_identifying, in_non_identifying)
        };

        let plain = make_agent(&dir.path().join("plain")).describe();
        assert_eq!(named(&plain), (false, false));

        let storage = Storage::new(dir.path().join("configured")).expect("storage");
        let configured = AgentState::new("test-agent".to_string(), storage)
            .expect("agent")
            .with_namespace(Some("telemetry".to_string()))
            .describe();
        assert_eq!(
            named(&configured),
            (true, false),
            "it belongs among what identifies the Agent, and only there"
        );
        assert_eq!(
            reported(&configured).get("service.namespace").cloned(),
            Some("telemetry".to_string())
        );
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
        let mut supervised =
            AgentState::supervised("otelcol".to_string(), "otelcol".to_string(), storage)
                .expect("agent");
        assert_eq!(version_of(&supervised), None);

        supervised.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.version", "0.142.0")],
            non_identifying_attributes: vec![],
        });
        assert_eq!(version_of(&supervised).as_deref(), Some("0.142.0"));
    }

    /// Two sources describe the same Managed Process — the version probe, which reports
    /// `service.version` alone after every package swap, and the opampextension, which reports
    /// everything else. Replacing rather than merging would make each new probe erase the
    /// extension's self-report, and each self-report erase the probed version.
    #[test]
    fn what_the_process_reports_about_itself_accumulates_across_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("supervised")).expect("storage");
        let mut agent =
            AgentState::supervised("otelcol".to_string(), "otelcol".to_string(), storage)
                .expect("agent");

        // The extension's self-report: everything but the version, which it happens not to state.
        agent.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.name", "otelcol-contrib")],
            non_identifying_attributes: vec![string_attr("host.id", "abc")],
        });
        // The probe, after a swap: the version and nothing else.
        agent.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.version", "0.158.0")],
            non_identifying_attributes: vec![],
        });

        let described = agent.describe();
        let value = |attrs: &[KeyValue], key: &str| {
            attrs
                .iter()
                .find(|kv| kv.key == key)
                .and_then(|kv| kv.value.clone())
                .and_then(|v| v.value)
                .map(|v| match v {
                    any_value::Value::StringValue(s) => s,
                    other => format!("{other:?}"),
                })
        };
        assert_eq!(
            value(&described.identifying_attributes, "service.version").as_deref(),
            Some("0.158.0"),
            "the probed version survives"
        );
        assert_eq!(
            value(&described.identifying_attributes, "service.name").as_deref(),
            Some("otelcol-contrib"),
            "and does not erase what the extension reported"
        );
        assert_eq!(
            value(&described.non_identifying_attributes, "host.id").as_deref(),
            Some("abc")
        );

        // A later self-report of the same key wins — a restarted process states the truth.
        agent.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.version", "0.159.0")],
            non_identifying_attributes: vec![],
        });
        assert_eq!(
            value(&agent.describe().identifying_attributes, "service.version").as_deref(),
            Some("0.159.0")
        );
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
        let mut supervised =
            AgentState::supervised("s".to_string(), "s".to_string(), storage).expect("agent");
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

    /// ADR-0020: the Client's own Agent takes one named package and refuses everything else. A
    /// package with an empty Selector reaches every consenting Agent (ADR-0017), so without this
    /// the first fleet-wide Collector artifact an operator uploads would be written over the
    /// Client and take the host out of reach.
    #[test]
    fn the_self_agent_refuses_a_package_it_was_not_configured_to_take() {
        use opamp::proto::{DownloadableFile, PackageAvailable, PackagesAvailable};

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::new("opamp-fleet-client".to_string(), storage).expect("agent");
        agent.accept_packages_named("opamp-client".to_string());
        let _ = agent.next_report();

        let offer = |name: &str| PackagesAvailable {
            packages: [(
                name.to_string(),
                PackageAvailable {
                    version: "2.0.0".to_string(),
                    file: Some(DownloadableFile {
                        download_url: "/x".to_string(),
                        content_hash: b"chash".to_vec(),
                        ..Default::default()
                    }),
                    hash: b"pkg-hash".to_vec(),
                    ..Default::default()
                },
            )]
            .into(),
            all_packages_hash: b"agg-1".to_vec(),
        };

        // A Collector's package, offered fleet-wide: refused, and nothing is downloaded.
        let handled = agent.handle(&ServerToAgent {
            packages_available: Some(offer("otelcol")),
            ..Default::default()
        });
        assert!(
            handled.package_download.is_none(),
            "nothing is fetched for a package this Agent does not take"
        );
        assert!(handled.send_report, "the refusal is reported, not silent");
        let statuses = agent
            .next_report()
            .package_statuses
            .expect("a package status");
        assert!(
            statuses.error_message.contains("opamp-client"),
            "the refusal names what this Agent would accept: {:?}",
            statuses.error_message
        );
        assert_eq!(
            statuses.server_provided_all_packages_hash, b"agg-1",
            "the aggregate is echoed so the Server stops re-offering it"
        );

        // The configured one is taken.
        let handled = agent.handle(&ServerToAgent {
            packages_available: Some(offer("opamp-client")),
            ..Default::default()
        });
        let download = handled
            .package_download
            .expect("the named package is taken");
        assert_eq!(download.name, "opamp-client");
    }

    #[test]
    fn a_package_offer_for_the_named_package_is_acknowledged_installing_and_handed_over() {
        use opamp::proto::{
            DownloadableFile, PackageAvailable, PackageStatusEnum, PackagesAvailable,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent =
            AgentState::supervised("otelcol".to_string(), "otelcol".to_string(), storage)
                .expect("agent");
        agent.accept_packages();
        let _ = agent.next_report();

        // The two package capabilities are declared once a package is accepted.
        let caps = agent.next_report().capabilities;
        assert_ne!(caps & AgentCapabilities::AcceptsPackages as u64, 0);
        assert_ne!(caps & AgentCapabilities::ReportsPackageStatuses as u64, 0);

        let offer = PackagesAvailable {
            packages: [(
                "otelcol".to_string(),
                PackageAvailable {
                    version: "2.0.0".to_string(),
                    file: Some(DownloadableFile {
                        download_url: "/api/v1/packages/otelcol/file".to_string(),
                        content_hash: b"chash".to_vec(),
                        ..Default::default()
                    }),
                    hash: b"pkg-hash".to_vec(),
                    ..Default::default()
                },
            )]
            .into(),
            all_packages_hash: b"agg-1".to_vec(),
        };
        let handled = agent.handle(&ServerToAgent {
            packages_available: Some(offer.clone()),
            ..Default::default()
        });
        assert!(handled.send_report);
        let download = handled.package_download.expect("a download");
        assert_eq!(download.name, "otelcol");
        assert_eq!(download.version, "2.0.0");
        assert_eq!(download.content_hash, b"chash");

        // The next report acknowledges Installing.
        let statuses = agent.next_report().package_statuses.expect("statuses");
        assert_eq!(
            statuses.packages["otelcol"].status,
            PackageStatusEnum::Installing as i32
        );

        // While the artifact is on the wire the status is Downloading, carrying how far it has
        // got — the Baseline's interim reporting, which is what keeps a minutes-long transfer
        // distinguishable from a stuck install.
        agent.package_downloading(PackageDownloadDetails {
            download_percent: 42.5,
            download_bytes_per_second: 1_048_576.0,
        });
        let status = agent
            .next_report()
            .package_statuses
            .expect("statuses")
            .packages["otelcol"]
            .clone();
        assert_eq!(status.status, PackageStatusEnum::Downloading as i32);
        let details = status.download_details.expect("details while downloading");
        assert!((details.download_percent - 42.5).abs() < f64::EPSILON);
        assert!((details.download_bytes_per_second - 1_048_576.0).abs() < f64::EPSILON);
        assert_eq!(
            status.server_offered_hash, b"pkg-hash",
            "the Baseline requires the offered hash while downloading"
        );
        assert!(
            status.error_message.is_empty(),
            "downloading is not an error"
        );

        // Bytes in, applying: back to Installing, and the details go with the status they belong
        // to ("should only be set if status is Downloading").
        agent.package_downloaded();
        let status = agent
            .next_report()
            .package_statuses
            .expect("statuses")
            .packages["otelcol"]
            .clone();
        assert_eq!(status.status, PackageStatusEnum::Installing as i32);
        assert!(status.download_details.is_none());

        // A repeat of the same offer while in flight is not re-entered.
        let again = agent.handle(&ServerToAgent {
            packages_available: Some(offer),
            ..Default::default()
        });
        assert!(again.package_download.is_none(), "no re-download in flight");

        // The Supervisor installed it: Installed, at the offered version, aggregate echoed.
        agent.package_applied(b"pkg-hash".to_vec(), Ok("2.0.0".to_string()));
        let statuses = agent.next_report().package_statuses.expect("statuses");
        let status = &statuses.packages["otelcol"];
        assert_eq!(status.status, PackageStatusEnum::Installed as i32);
        assert_eq!(status.agent_has_version, "2.0.0");
        assert_eq!(statuses.server_provided_all_packages_hash, b"agg-1");

        // A re-offer of the same package is now recognised as already installed — no re-download.
        let settled = agent.handle(&ServerToAgent {
            packages_available: Some(PackagesAvailable {
                packages: [(
                    "otelcol".to_string(),
                    PackageAvailable {
                        version: "2.0.0".to_string(),
                        file: Some(DownloadableFile {
                            download_url: "/x".to_string(),
                            content_hash: b"chash".to_vec(),
                            ..Default::default()
                        }),
                        hash: b"pkg-hash".to_vec(),
                        ..Default::default()
                    },
                )]
                .into(),
                all_packages_hash: b"agg-1".to_vec(),
            }),
            ..Default::default()
        });
        assert!(settled.package_download.is_none(), "already installed");
    }

    /// An `Addon` is not a Managed Process's binary, and the only thing this Client can do with a
    /// package is *be* that binary — so an offer carrying nothing but addons is refused rather
    /// than installed over the process they were meant to extend, and the refusal is reported.
    #[test]
    fn an_addon_package_is_refused_instead_of_overwriting_the_binary() {
        use opamp::proto::{DownloadableFile, PackageAvailable, PackagesAvailable};
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent =
            AgentState::supervised("otelcol".to_string(), "otelcol".to_string(), storage)
                .expect("agent");
        agent.accept_packages();
        let _ = agent.next_report();

        let handled = agent.handle(&ServerToAgent {
            packages_available: Some(PackagesAvailable {
                packages: [(
                    "otelcol".to_string(),
                    PackageAvailable {
                        r#type: PackageType::Addon as i32,
                        version: "2.0.0".to_string(),
                        file: Some(DownloadableFile {
                            download_url: "/api/v1/packages/otelcol/file".to_string(),
                            content_hash: b"chash".to_vec(),
                            ..Default::default()
                        }),
                        hash: b"addon-hash".to_vec(),
                    },
                )]
                .into(),
                all_packages_hash: b"agg-addon".to_vec(),
            }),
            ..Default::default()
        });
        assert!(
            handled.package_download.is_none(),
            "an addon is never downloaded, let alone swapped over the binary"
        );
        assert!(handled.send_report, "the refusal is reported at once");

        // The failure is about the offer, not about one package — which is exactly what the
        // Baseline's `PackageStatuses.error_message` is for ("not related to any particular
        // single package"), so it rides there and the packages map stays empty.
        let statuses = agent.next_report().package_statuses.expect("statuses");
        assert!(
            statuses.error_message.contains("addon"),
            "the reason names what was refused: {}",
            statuses.error_message
        );
        assert!(statuses.packages.is_empty(), "nothing was installed");
        // A refusal is a report, not a loop: the aggregate is echoed so the offer ends.
        assert_eq!(statuses.server_provided_all_packages_hash, b"agg-addon");
    }

    #[test]
    fn a_failed_package_reports_installed_failed_and_keeps_the_old_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent =
            AgentState::supervised("otelcol".to_string(), "otelcol".to_string(), storage)
                .expect("agent");
        agent.accept_packages();
        agent.offered_all_packages_hash = b"agg-2".to_vec();
        agent.server_offered = Some(("9.9.9".to_string(), b"bad".to_vec()));
        agent.installing = Some(crate::packages::PackageDownload {
            name: "otelcol".to_string(),
            version: "9.9.9".to_string(),
            hash: b"bad".to_vec(),
            download_url: String::new(),
            content_hash: Vec::new(),
            signature: Vec::new(),
        });

        agent.package_applied(b"bad".to_vec(), Err("would not stay up".to_string()));
        let statuses = agent.next_report().package_statuses.expect("statuses");
        let status = &statuses.packages["otelcol"];
        assert_eq!(
            status.status,
            opamp::proto::PackageStatusEnum::InstallFailed as i32
        );
        assert_eq!(status.error_message, "would not stay up");
        // A failure is a report, not a loop: the aggregate is echoed so the Server stops re-offering.
        assert_eq!(statuses.server_provided_all_packages_hash, b"agg-2");
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
