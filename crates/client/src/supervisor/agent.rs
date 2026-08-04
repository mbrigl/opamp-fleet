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
    ConnectionSettingsStatus, ConnectionSettingsStatuses, EffectiveConfig, KeyValue, PackageStatus,
    PackageStatusEnum, PackageStatuses, PackageType, RemoteConfigStatus, RemoteConfigStatuses,
    ServerCapabilities, ServerErrorResponseType, ServerToAgent, ServerToAgentFlags,
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
    /// The package this Agent's Managed Process is updated from (ADR-0015): the name of a
    /// Server-offered package. `None` means the Agent takes no package offers (and declares
    /// neither package capability).
    package_name: Option<String>,
    /// The package currently installed, persisted across restarts.
    installed_package: Option<InstalledPackage>,
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
    send_package_status: bool,
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
            package_name: None,
            installed_package: None,
            installing: None,
            offered_all_packages_hash: Vec::new(),
            echoed_all_packages_hash: Vec::new(),
            server_offered: None,
            package_error: String::new(),
            send_package_status: false,
            configured_attributes: Vec::new(),
        })
    }

    /// Opts this Agent into package delivery for the named package (ADR-0015): declares
    /// `AcceptsPackages` and `ReportsPackageStatuses`, and restores what it last installed so a
    /// restarted Client reports the version it runs and is not re-offered it.
    pub fn accept_package(&mut self, package_name: String) {
        self.package_name = Some(package_name);
        self.installed_package = self.storage.load_package();
        self.declare_capability(AgentCapabilities::AcceptsPackages);
        self.declare_capability(AgentCapabilities::ReportsPackageStatuses);
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
        if self.package_name.is_some() && (self.send_full || self.send_package_status) {
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
        let Some(name) = &self.package_name else {
            return PackageStatuses::default();
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
        let status = if self.installing.is_some() {
            // A download/install is in flight.
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
            ..Default::default()
        };
        PackageStatuses {
            packages: [(name.clone(), package)].into(),
            server_provided_all_packages_hash: self.echoed_all_packages_hash.clone(),
            error_message: String::new(),
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

    /// Reacts to a `PackagesAvailable` offer for this Agent's one named package.
    fn handle_package_offer(
        &mut self,
        offer: &opamp::proto::PackagesAvailable,
        handled: &mut Handled,
    ) {
        let Some(name) = self.package_name.clone() else {
            return;
        };
        self.offered_all_packages_hash = offer.all_packages_hash.clone();
        let Some(available) = offer.packages.get(&name) else {
            // Our package is not in this offer — nothing for us to install; echo the aggregate so
            // the Server stops offering (its other packages are not our concern).
            self.echoed_all_packages_hash = offer.all_packages_hash.clone();
            self.send_package_status = true;
            return;
        };
        self.server_offered = Some((available.version.clone(), available.hash.clone()));
        // The Baseline distinguishes a `TopLevel` package — the Managed Process's own binary —
        // from an `Addon`. A Supervisor knows how to replace the former and nothing about the
        // latter, and installing an Addon the only way this Client can would overwrite the very
        // binary the addon was meant to extend. So it is refused, and refusing is a *report*: the
        // aggregate hash is echoed with `InstallFailed`, which both tells the operator why and
        // stops the Server offering the same bytes forever.
        if available.r#type == PackageType::Addon as i32 {
            error!(
                package = %name,
                "refusing an addon package: this Client installs top-level packages only"
            );
            self.installing = None;
            self.package_error =
                "this Client installs top-level packages only; the offered package is an addon"
                    .to_string();
            self.echoed_all_packages_hash = offer.all_packages_hash.clone();
            self.send_package_status = true;
            handled.send_report = true;
            return;
        }
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

    /// Closes a package's lifecycle the Supervisor applied (ADR-0015): `Ok(version)` records it
    /// Installed and persists it; `Err` reports InstallFailed (the binary was rolled back). Either
    /// way the offered aggregate is echoed, so the Server stops re-offering the same bytes — a
    /// refusal is a report, not a loop.
    pub fn package_applied(&mut self, hash: Vec<u8>, result: Result<String, String>) {
        self.installing = None;
        match result {
            Ok(version) => {
                let installed = InstalledPackage {
                    name: self.package_name.clone().unwrap_or_default(),
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
    fn a_package_offer_for_the_named_package_is_acknowledged_installing_and_handed_over() {
        use opamp::proto::{
            DownloadableFile, PackageAvailable, PackageStatusEnum, PackagesAvailable,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised("otelcol".to_string(), storage).expect("agent");
        agent.accept_package("otelcol".to_string());
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
    /// package is *be* that binary — so an addon offer must be refused rather than installed over
    /// the process it was meant to extend, and the refusal must be reported.
    #[test]
    fn an_addon_package_is_refused_instead_of_overwriting_the_binary() {
        use opamp::proto::{DownloadableFile, PackageAvailable, PackagesAvailable};
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised("otelcol".to_string(), storage).expect("agent");
        agent.accept_package("otelcol".to_string());
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

        let statuses = agent.next_report().package_statuses.expect("statuses");
        let status = &statuses.packages["otelcol"];
        assert_eq!(status.status, PackageStatusEnum::InstallFailed as i32);
        assert!(
            status.error_message.contains("addon"),
            "the reason names what was refused: {}",
            status.error_message
        );
        // A refusal is a report, not a loop: the aggregate is echoed so the offer ends.
        assert_eq!(statuses.server_provided_all_packages_hash, b"agg-addon");
    }

    #[test]
    fn a_failed_package_reports_installed_failed_and_keeps_the_old_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised("otelcol".to_string(), storage).expect("agent");
        agent.accept_package("otelcol".to_string());
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
