//! One Agent's state machine: builds `AgentToServer` reports and reacts to `ServerToAgent`
//! replies.
//!
//! Transport-agnostic on purpose (ADR-0012): the WebSocket and plain-HTTP loops feed the same
//! state machine, so transport is carriage, never semantics. The [`Engine`](crate::engine)
//! carries n of these over one connection (ADR-0009, ADR-0015) — a Supervisor-backed Agent and
//! the self-Agent fallback are the same state machine.

use std::time::Duration;

use opamp::attributes::{self, string_array_attr, string_attr};
use opamp::proto::{
    AgentCapabilities, AgentDescription, AgentRemoteConfig, AgentToServer, AvailableComponents,
    ComponentHealth, ConnectionSettingsOffers, ConnectionSettingsStatus,
    ConnectionSettingsStatuses, EffectiveConfig, KeyValue, PackageAvailable,
    PackageDownloadDetails, PackageStatus, PackageStatusEnum, PackageStatuses, PackageType,
    RemoteConfigStatus, RemoteConfigStatuses, ServerCapabilities, ServerToAgent,
};
use opamp::uid::InstanceUid;
use tracing::{error, info, warn};

use crate::supervisor::ports::{AgentStorage, HostFacts, InstalledPackage};
use opamp::client::protocol::{AgentProtocol, ReportContent};

/// The base Capability Set every Agent of this Client declares (see docs/CONFORMANCE.md).
/// Individual Agents declare more via [`AgentState::declare_capability`] — e.g. heartbeats when
/// enabled, restartability only where a Managed Process exists.
pub const AGENT_CAPABILITIES: u64 = AgentCapabilities::ReportsStatus as u64
    | AgentCapabilities::AcceptsRemoteConfig as u64
    | AgentCapabilities::ReportsEffectiveConfig as u64
    | AgentCapabilities::ReportsRemoteConfig as u64
    | AgentCapabilities::ReportsHealth as u64
    | AgentCapabilities::AcceptsOpAmpConnectionSettings as u64
    | AgentCapabilities::ReportsConnectionSettingsStatus as u64
    // The Agent's own telemetry (ADR-0025). Declared unconditionally, unlike the capabilities that
    // describe something this end *has*: these say the Client can report to a destination the
    // Server names, which is true before any destination exists — and declaring them only once one
    // is in force would mean the Server could never make the first offer.
    | AgentCapabilities::ReportsOwnMetrics as u64
    | AgentCapabilities::ReportsOwnTraces as u64
    | AgentCapabilities::ReportsOwnLogs as u64;

/// What a handled `ServerToAgent` asks of the transport loop.
#[derive(Debug, Default, PartialEq)]
pub struct Handled {
    /// Something changed that the Server must hear about now (a config outcome, a demanded full
    /// report) — send the next report immediately instead of waiting for the poll interval.
    pub send_report: bool,
    /// The Server is throttling us (`UNAVAILABLE` + retry info): back off this long first.
    pub retry_after: Option<Duration>,
    /// A connection-settings offer to verify by actually connecting (ADR-0018). The state
    /// machine has already acknowledged `APPLYING`; the transport owns the verification, the
    /// switch, and reporting the outcome back through the [`Engine`](crate::engine).
    pub connection_offer: Option<ConnectionSettingsOffers>,
    /// A package to download, verify, and hand to the Supervisor (ADR-0019). The state machine
    /// has acknowledged `Installing`; the transport owns the download and verification.
    pub package_download: Option<PackageDownload>,
}

/// The Agent type the Client's own Agent presents as `service.name` (ADR-0024, ADR-0023): the role
/// it plays on the host, the Agent that supervises the others. A constant, not the configured
/// instance name — every Client in a fleet is the same kind of thing, and that is what a type says.
///
/// Since ADR-0023 it is also the shipped program's name and its configuration file's — see
/// [`layout::COMPONENT`](crate::service::layout::COMPONENT), which holds the same string as a
/// **separate** constant. It is *not* the service's name: ADR-0014 clause 3 gives the service the
/// product's name, and clause 9 keeps this one off
/// [`PRODUCT_NAME`](crate::product::PRODUCT_NAME) deliberately — the archive member a self-update
/// extracts is the same in every variant build, which is what lets one published package Set
/// serve them all. Derive this from the product and the fleet carries N products where it has one.
///
/// It was called `CLIENT_SERVICE_NAME` until ADR-0014. It never named a service, and with the
/// service now carrying the product's name the old name would read as the one thing it is not.
///
/// The package that carries this Client is named after the type, so `[self_update] package`
/// defaults to this constant.
pub const CLIENT_AGENT_TYPE: &str = "supervisor";

pub struct AgentState {
    /// What OpAMP defines about this Agent: identity, sequence numbers, the two Capability Sets,
    /// what the next report owes, and the status messages (ADR-0033).
    protocol: AgentProtocol,
    /// What this Client reports about it.
    local: Local,
    storage: Box<dyn AgentStorage>,
    /// A received configuration awaiting dispatch to the process adapter.
    pending_apply: Option<AgentRemoteConfig>,
    /// The self-Agent's configuration in flight (ADR-0022): stored only once the apply succeeded,
    /// so a Client restarted mid-apply reports nothing as applied and is re-offered — a status of
    /// `APPLIED` on restart must mean the file was actually rewritten.
    applying: Option<AgentRemoteConfig>,
    /// A Server-commanded restart awaiting dispatch to the process adapter.
    pending_restart: bool,
}

/// What this Client reports about one Agent, and the package bookkeeping behind its status — the
/// content the protocol asks for (ADR-0033).
struct Local {
    /// What the host says about itself (ADR-0024).
    host: Box<dyn HostFacts>,
    /// The operator's name for this Agent, reported as `service.instance.name` (ADR-0024): the
    /// `[[supervisor]]` block's `name`, or the top-level one for the Client's own Agent. Never
    /// `service.name` — that is the type below.
    instance_name: String,
    /// The Agent *type*, reported as `service.name`. A Managed Process that reports one of its own
    /// replaces it in the fold; this is what stands until then, and permanently for a process that
    /// reports nothing.
    service_name: String,
    start_time_ns: u64,
    /// The last stored remote configuration; what `effective_config` echoes unless the Managed
    /// Process reported its own.
    applied: Option<AgentRemoteConfig>,
    /// A Managed Process stands behind this Agent: a received configuration is acknowledged
    /// `APPLYING` and handed to the process adapter; `APPLIED`/`FAILED` follow its outcome.
    managed: bool,
    /// The Managed Process's health — derived or self-reported (ADR-0015). Absent for the
    /// self-Agent, whose health is being alive.
    process_health: Option<ComponentHealth>,
    /// The Managed Process's self-reported description, folded into ours (goal 16).
    process_description: Option<AgentDescription>,
    /// The Managed Process's self-reported effective configuration; replaces the echo.
    process_effective_config: Option<EffectiveConfig>,
    /// The Managed Process's available components, relayed from the Supervisor Endpoint.
    /// Routine reports carry only the hash; the full map goes out when the Server asks.
    available_components: Option<AvailableComponents>,
    /// The pid of the Managed Process while it runs (ADR-0025) — what own metrics are sampled
    /// from. `None` for the Client's own Agent, whose process is this one, and for a Supervisor
    /// between restarts.
    process_pid: Option<u32>,
    /// Whether this Agent's Managed Process is updated from Server-offered packages (ADR-0019).
    /// Which package that is, is the Server's choice — this side only consents.
    accepts_packages: bool,
    /// The only package name this Agent will install (ADR-0021); `None` takes whichever top-level
    /// package the Server offers, which is what a Supervisor does.
    expected_package: Option<String>,
    /// The name of the top-level package the Server last offered. Learned from the offer, not
    /// configured: it keys the reported `PackageStatuses` map, which the Baseline requires to name
    /// every package the Agent has or is processing.
    offered_name: Option<String>,
    /// The package currently installed, persisted across restarts.
    installed_package: Option<InstalledPackage>,
    /// Progress of the artifact download in flight (ADR-0019), reported as `Downloading` with
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
    /// Operator-defined attributes from `supervisor.toml` (ADR-0016), reported as non-identifying
    /// attributes so Selectors can target them. Reported attributes win on key collision.
    configured_attributes: Vec<(String, String)>,
    /// The deployment's `service.namespace`, when it has one. The Baseline asks for it "if it is
    /// used in the environment where the Agent runs" — which only an operator knows, so it is
    /// configured rather than detected, and absent until it is set.
    namespace: Option<String>,
    /// Handling an offer changed the package status; the protocol is told once it is done.
    package_status_owed: bool,
}

impl AgentState {
    /// Restores identity and configuration from storage, so a restart reports the same Agent with
    /// the same applied config hash — and is therefore not reconfigured redundantly.
    ///
    /// `instance_name` is the operator's name for this Agent; the type it presents is
    /// [`CLIENT_AGENT_TYPE`], since this constructor builds the Client's own Agent. A
    /// Supervisor-backed one comes from [`supervised`](Self::supervised), which is told its type.
    pub fn new(
        instance_name: String,
        storage: impl AgentStorage + 'static,
        host: impl HostFacts + 'static,
    ) -> std::io::Result<Self> {
        let uid = storage.load_or_create_uid()?;
        let applied = storage.load_remote_config();
        let mut protocol = AgentProtocol::new(uid, AGENT_CAPABILITIES);
        if let Some(config) = &applied {
            protocol.restore_remote_config_status(config_status(
                config.config_hash.clone(),
                RemoteConfigStatuses::Applied,
                String::new(),
            ));
        }
        info!(agent = %uid, "agent identity ready");
        Ok(AgentState {
            protocol,
            local: Local {
                start_time_ns: host.now_ns(),
                host: Box::new(host),
                instance_name,
                service_name: CLIENT_AGENT_TYPE.to_string(),
                applied,
                managed: false,
                process_health: None,
                process_description: None,
                process_effective_config: None,
                available_components: None,
                process_pid: None,
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
                configured_attributes: Vec::new(),
                namespace: None,
                package_status_owed: false,
            },
            storage: Box::new(storage),
            pending_apply: None,
            applying: None,
            pending_restart: false,
        })
    }

    /// Opts this Agent into package delivery (ADR-0019): declares `AcceptsPackages` and
    /// `ReportsPackageStatuses`, and restores what it last installed so a restarted Client reports
    /// the version it runs and is not re-offered it.
    ///
    /// It consents; it does not choose. Which artifact arrives is decided on the Server, so a
    /// rollout is aimed centrally rather than from this host's file.
    pub fn accept_packages(&mut self) {
        self.local.accepts_packages = true;
        self.local.installed_package = self.storage.load_package();
        self.declare_capability(AgentCapabilities::AcceptsPackages);
        self.declare_capability(AgentCapabilities::ReportsPackageStatuses);
    }

    /// Opts this Agent into package delivery for **one named package only** (ADR-0021) — what the
    /// Client's own Agent does. A Supervisor takes whichever top-level package the Server offers
    /// it, because the worst case there is a Managed Process that will not start and is rolled
    /// back. The Client has no such safety net: a package written over this binary takes the host
    /// out of reach for good. So this side matches the name and refuses everything else.
    ///
    /// The restored record is held against the version this binary *is*, and dropped when the two
    /// are not the same release. `service uninstall` deliberately keeps the install layout and the
    /// state (ADR-0014), so an operator who reinstalls an older Client comes up on top of the
    /// record its successor wrote — and reporting that record would tell the Server this host runs
    /// a version it does not have. Worse than the wrong line in the fleet view: the offer is gated
    /// on the hash inside it, so the Server would never offer this host the package again. A
    /// record that does not name the running binary is a record about a binary that is gone.
    pub fn accept_packages_named(&mut self, name: String) {
        self.accept_packages();
        let running = fleet_core::version::current();
        if let Some(installed) = &self.local.installed_package {
            // Not string equality: the record holds the version the operator uploaded (`1.2.3`)
            // and this binary calls itself `1.2.3+a1b2c3d`. The same comparison the self-update
            // probe makes before a version is ever pointed at (ADR-0013).
            if !fleet_core::version::same_release(&installed.version, running) {
                warn!(
                    recorded = %installed.version, running = %running,
                    "the installed package record does not name the version this Client runs; \
                     discarding it, so the Server can offer the package again"
                );
                self.local.installed_package = None;
                if let Err(e) = self.storage.forget_package() {
                    warn!(error = %e, "cannot drop the stale installed package record");
                }
            }
        }
        self.local.expected_package = Some(name);
    }

    /// Restores the outcome of a previously applied connection-settings offer (ADR-0018): the
    /// persisted hash reports `APPLIED`, so a restarted Client is not re-offered what it runs.
    pub fn adopt_connection_settings(&mut self, hash: &[u8]) {
        self.protocol.set_connection_settings_status(
            settings_status(
                hash.to_vec(),
                ConnectionSettingsStatuses::Applied,
                String::new(),
            ),
            false,
        );
    }

    /// Closes the connection-settings lifecycle the transport verified (ADR-0018): `APPLIED`
    /// keeps the hash and the switch follows; `FAILED` keeps the hash too — the Baseline's
    /// gating stops the Server re-offering the exact settings this Agent could not use.
    pub fn connection_settings_outcome(&mut self, hash: &[u8], result: Result<(), &str>) {
        let status = match result {
            Ok(()) => settings_status(
                hash.to_vec(),
                ConnectionSettingsStatuses::Applied,
                String::new(),
            ),
            Err(error) => settings_status(
                hash.to_vec(),
                ConnectionSettingsStatuses::Failed,
                error.to_string(),
            ),
        };
        self.protocol.set_connection_settings_status(status, true);
    }

    /// An Agent with a Managed Process behind it (a Supervisor-backed Agent, ADR-0015). Only
    /// such an Agent accepts a restart command — the self-Agent has no process to restart.
    ///
    /// The type is a parameter rather than a builder default because there is no sensible default
    /// for it (ADR-0024): the Client's own type would be a lie, and the instance name in that slot
    /// is exactly the confusion this signature exists to prevent. The caller always knows one —
    /// the block's `service_name`, or the program's file name.
    pub fn supervised(
        instance_name: String,
        service_name: String,
        storage: impl AgentStorage + 'static,
        host: impl HostFacts + 'static,
    ) -> std::io::Result<Self> {
        let mut state = Self::new(instance_name, storage, host)?;
        state.local.service_name = service_name;
        state.local.managed = true;
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
        self.protocol.declare(capability);
    }

    /// Whether this Agent takes Server-offered packages (ADR-0019) — the self-Agent unless
    /// `[self_update]` withdraws the consent (ADR-0021), and every Supervisor, since every Managed
    /// Process is one this Client installs (ADR-0022). Read without side effects, so a startup
    /// check can ask it before anything is polled.
    pub fn accepts_packages(&self) -> bool {
        self.protocol.declares(AgentCapabilities::AcceptsPackages)
    }

    /// Attaches the operator-defined attributes this Agent reports (ADR-0016).
    #[must_use]
    pub fn with_attributes(
        mut self,
        attributes: std::collections::BTreeMap<String, String>,
    ) -> Self {
        self.local.configured_attributes = attributes.into_iter().collect();
        self
    }

    /// Attaches the deployment's `service.namespace`, reported by every Agent this Client presents.
    /// `None` — the default — reports nothing, which is what the Baseline's "if it is used in the
    /// environment" amounts to for a deployment that does not use one.
    #[must_use]
    pub fn with_namespace(mut self, namespace: Option<String>) -> Self {
        self.local.namespace = namespace;
        self
    }

    pub fn uid(&self) -> InstanceUid {
        self.protocol.uid()
    }

    /// The operator's name for this Agent (ADR-0024). The Supervisor's own value, never the
    /// Managed Process's: a process reporting under that key is ignored in `describe`, so this is
    /// the one answer to "which Agent is this" that a human can read.
    pub fn instance_name(&self) -> &str {
        &self.local.instance_name
    }

    /// The Agent *type* this Agent is reported under — `service.name` (ADR-0024).
    ///
    /// The Managed Process's own word where it gives one, the Supervisor's configured type
    /// otherwise — the fold `describe` performs, mirrored here rather than repeated: what the
    /// fleet view shows for an Agent and what its telemetry is labelled with must be the one
    /// answer, and `describe` is too expensive to call per sample, since it reads the host's
    /// addresses live.
    ///
    /// The two answers differ in exactly one case, deliberately. A process reporting
    /// `service.name = ""` blanks the type in `describe`, because the fold replaces by key without
    /// judging the value; here the empty string is not a value (ADR-0020), so the configured type
    /// stands. A label nobody can read is worse than a stale one, and the Selector consequences of
    /// the other reading are ADR-0020's own subject rather than this accessor's.
    pub fn service_name(&self) -> &str {
        self.local
            .process_description
            .as_ref()
            .and_then(|reported| {
                opamp::attributes::string_value(
                    &reported.identifying_attributes,
                    attributes::SERVICE_NAME,
                )
            })
            .unwrap_or(&self.local.service_name)
    }

    /// A configuration stored `APPLYING` and not yet handed to the process adapter, if any.
    pub fn take_pending_apply(&mut self) -> Option<AgentRemoteConfig> {
        self.pending_apply.take()
    }

    /// The verdict on an apply — the process adapter's for a Supervisor-backed Agent, the
    /// Engine's Supervisor-set apply for the self-Agent (ADR-0022): closes the `APPLYING` →
    /// `APPLIED`/`FAILED` lifecycle (goal 4, end to end).
    pub fn config_applied(&mut self, hash: Vec<u8>, result: Result<(), String>) {
        // The self-Agent's offer is persisted only now, on success: its hash is what a restarted
        // Client reports as applied, and that must never get ahead of the file (ADR-0022).
        if let Some(applying) = self.applying.take() {
            if result.is_ok() && applying.config_hash == hash {
                match self.storage.store_remote_config(&applying) {
                    Ok(()) => self.local.applied = Some(applying),
                    Err(e) => warn!(error = %e, "cannot store the applied configuration"),
                }
            }
        }
        self.protocol.set_remote_config_status(match result {
            Ok(()) => config_status(hash, RemoteConfigStatuses::Applied, String::new()),
            Err(error) => config_status(hash, RemoteConfigStatuses::Failed, error),
        });
    }

    /// The Managed Process's health changed — derived or self-reported.
    pub fn set_process_health(&mut self, health: ComponentHealth) {
        self.local.process_health = Some(health);
        self.protocol.health_changed();
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
            .local
            .process_description
            .get_or_insert_with(AgentDescription::default);
        for attr in &description.identifying_attributes {
            upsert_attr(&mut merged.identifying_attributes, attr);
        }
        for attr in &description.non_identifying_attributes {
            upsert_attr(&mut merged.non_identifying_attributes, attr);
        }
        self.protocol.force_full();
    }

    /// The Managed Process reported its own effective configuration; report that instead of
    /// echoing the written files.
    pub fn set_process_effective_config(&mut self, config: EffectiveConfig) {
        self.local.process_effective_config = Some(config);
        self.protocol.effective_config_changed();
    }

    /// The Managed Process reported its available components. Only now does the Agent declare
    /// `ReportsAvailableComponents` — a capability without components would be a false promise —
    /// and the next full report carries the hash (the Server flags for the full map on demand).
    pub fn set_available_components(&mut self, components: AvailableComponents) {
        self.local.available_components = Some(components);
        self.declare_capability(AgentCapabilities::ReportsAvailableComponents);
        self.protocol.force_full();
    }

    /// The next report starts from a full status snapshot again — after (re)connecting, after an
    /// exchange failed, or when the Server demanded it.
    pub fn force_full(&mut self) {
        self.protocol.force_full();
    }

    /// The next `AgentToServer`: which fields it carries is the protocol's to decide, what they
    /// hold is this Agent's (ADR-0033).
    pub fn next_report(&mut self) -> AgentToServer {
        self.protocol.next_report(&self.local)
    }

    /// The final message of a connection: the Baseline requires `agent_disconnect` in it.
    pub fn disconnect_message(&mut self) -> AgentToServer {
        self.protocol.disconnect_message()
    }

    /// Reacts to one `ServerToAgent`: the protocol settles its part, and what is left — a
    /// command, a configuration, an offer — is this Agent's to decide.
    pub fn handle(&mut self, reply: &ServerToAgent) -> Handled {
        let received = self.protocol.receive(reply);
        let mut handled = Handled {
            send_report: received.report_now,
            retry_after: received.retry_after,
            ..Handled::default()
        };

        if let Some(command) = received.command {
            if command == opamp::proto::CommandType::Restart as i32 && self.local.managed {
                info!("the server commanded a restart");
                self.pending_restart = true;
            } else {
                // Restart is the only command the Baseline defines; and the self-Agent never
                // declares AcceptsRestartCommand, so a command toward it is a Server error.
                warn!(r#type = command, "ignoring an unsupported command");
            }
            return handled;
        }

        if let Some(new_uid) = received.new_uid {
            if let Err(e) = self.storage.save_uid(&new_uid) {
                warn!(error = %e, "cannot persist the new identity");
            }
        }

        if received.components_requested && self.local.available_components.is_some() {
            self.protocol.send_components_full();
            handled.send_report = true;
        }

        if let Some(remote_config) = received.remote_config {
            self.apply(remote_config);
            handled.send_report = true;
        }

        // A connection-settings offer (ADR-0018): acknowledge APPLYING and hand it to the
        // transport, which alone can verify by actually connecting — the Baseline's MUST. Only
        // an offer this Agent already runs (APPLIED, same hash) is not re-entered; a re-offer
        // after FAILED or a lost in-flight verification retries.
        if let Some(offers) = received.connection_settings {
            let applied = self.protocol.connection_settings_status().is_some_and(|s| {
                s.last_connection_settings_hash == offers.hash
                    && s.status == ConnectionSettingsStatuses::Applied as i32
            });
            if carries_settings(offers) && !applied {
                info!(hash = %hex::encode(&offers.hash), "connection settings offered; applying");
                self.protocol.set_connection_settings_status(
                    settings_status(
                        offers.hash.clone(),
                        ConnectionSettingsStatuses::Applying,
                        String::new(),
                    ),
                    true,
                );
                handled.send_report = true;
                handled.connection_offer = Some(offers.clone());
            }
        }

        // A package offer (ADR-0019): act only on this Agent's package. Download and verification
        // are the transport's; the state machine acknowledges Installing and hands over the
        // coordinates.
        if let Some(available) = received.packages_available {
            self.local.handle_package_offer(available, &mut handled);
            if std::mem::take(&mut self.local.package_status_owed) {
                self.protocol.package_status_changed();
            }
        }

        handled
    }

    /// Records how far the artifact download has got (ADR-0019), so the next report carries
    /// `Downloading` with the details. The Baseline only *permits* these interim reports; without
    /// them a multi-hundred-megabyte download is indistinguishable from a stuck install.
    pub fn package_downloading(&mut self, details: PackageDownloadDetails) {
        self.local.downloading = Some(details);
        self.protocol.package_status_changed();
    }

    /// The artifact is downloaded and verified; what follows is the Supervisor applying it, which
    /// the Baseline reports as `Installing` rather than `Downloading`.
    pub fn package_downloaded(&mut self) {
        self.local.downloading = None;
        self.protocol.package_status_changed();
    }

    /// Closes a package's lifecycle the Supervisor applied (ADR-0019): `Ok(version)` records it
    /// Installed and persists it; `Err` reports InstallFailed (the binary was rolled back). Either
    /// way the offered aggregate is echoed, so the Server stops re-offering the same bytes — a
    /// refusal is a report, not a loop.
    pub fn package_applied(&mut self, hash: Vec<u8>, result: Result<String, String>) {
        let local = &mut self.local;
        // Keep the name the outcome is about: `installing` is cleared here, and a failure leaves
        // no installed record to fall back on, but the status still has to name its package.
        if let Some(name) = local.installing.as_ref().map(|d| d.name.clone()) {
            local.offered_name = Some(name);
        }
        local.installing = None;
        local.downloading = None;
        match result {
            Ok(version) => {
                let installed = InstalledPackage {
                    name: local.package_name().unwrap_or_default(),
                    version: version.clone(),
                    hash_hex: hex::encode(&hash),
                };
                if let Err(e) = self.storage.store_package(&installed) {
                    warn!(error = %e, "cannot persist the installed package record");
                }
                info!(version = %version, "package installed");
                local.installed_package = Some(installed);
                local.package_error.clear();
            }
            Err(error) => {
                error!(error = %error, "package installation failed");
                local.package_error = error;
            }
        }
        local.echoed_all_packages_hash = local.offered_all_packages_hash.clone();
        self.protocol.package_status_changed();
    }

    /// Takes an offered configuration in and reports `APPLYING`; the outcome closes the
    /// lifecycle through [`config_applied`](Self::config_applied). Success and failure alike
    /// carry the hash the status refers to (a rejected configuration is a report, not a silence).
    ///
    /// For a Supervisor-backed Agent the entry files are stored now — the process adapter is
    /// pointed at them — and the apply is handed over as pending. The self-Agent's configuration
    /// is its Supervisor set (ADR-0022): it is left pending for the Engine's apply and stored
    /// only once that succeeded, so a restart mid-apply reports nothing as applied and the
    /// Server offers again.
    fn apply(&mut self, config: &AgentRemoteConfig) {
        let status = if self.local.managed {
            match self.storage.store_remote_config(config) {
                Ok(()) => {
                    info!(hash = %hex::encode(&config.config_hash), "remote configuration stored; applying");
                    self.local.applied = Some(config.clone());
                    self.pending_apply = Some(config.clone());
                    config_status(
                        config.config_hash.clone(),
                        RemoteConfigStatuses::Applying,
                        String::new(),
                    )
                }
                Err(e) => {
                    error!(error = %e, "cannot store the remote configuration");
                    config_status(
                        config.config_hash.clone(),
                        RemoteConfigStatuses::Failed,
                        format!("cannot store the configuration: {e}"),
                    )
                }
            }
        } else {
            info!(hash = %hex::encode(&config.config_hash), "remote configuration received; applying to the supervisor set");
            self.pending_apply = Some(config.clone());
            self.applying = Some(config.clone());
            config_status(
                config.config_hash.clone(),
                RemoteConfigStatuses::Applying,
                String::new(),
            )
        };
        self.protocol.set_remote_config_status(status);
    }

    /// Whether a Managed Process stands behind this Agent — false for the Client's own Agent,
    /// which is the one that carries what belongs to the connection rather than to a process.
    pub fn is_managed(&self) -> bool {
        self.local.managed
    }

    /// This Agent's description, for the Resource of its own telemetry (ADR-0025).
    pub fn description(&self) -> AgentDescription {
        self.describe()
    }

    fn describe(&self) -> AgentDescription {
        self.local.describe(&self.protocol.uid())
    }

    /// The Managed Process's pid changed — it started, or it is gone (ADR-0025).
    pub fn set_process_pid(&mut self, pid: Option<u32>) {
        self.local.process_pid = pid;
    }

    /// The pid own metrics are sampled from: this process for the Client's own Agent, the Managed
    /// Process for a Supervisor-backed one — and nothing while that process is not running.
    pub fn process_pid(&self) -> Option<u32> {
        match self.local.managed {
            false => Some(self.local.host.process_id()),
            true => self.local.process_pid,
        }
    }

    /// Queues a certificate signing request for the next report (ADR-0017).
    pub fn request_certificate(&mut self, csr: Vec<u8>) {
        self.protocol.request_certificate(csr);
    }

    /// Whether this Server signs certificates at all. Starts pessimistic: sending a CSR to a
    /// Server that never declared the capability would be answered with the Baseline's
    /// `BadRequest`, and the protocol's negotiation rule says not to exercise what the peer has not
    /// declared.
    pub fn server_signs_certificates(&self) -> bool {
        self.protocol
            .server_declared(ServerCapabilities::AcceptsConnectionSettingsRequest)
    }
}

impl ReportContent for Local {
    fn description(&self, uid: &InstanceUid) -> AgentDescription {
        self.describe(uid)
    }

    fn health(&self) -> ComponentHealth {
        self.health()
    }

    fn effective_config(&self) -> EffectiveConfig {
        match &self.process_effective_config {
            Some(reported) => reported.clone(),
            None => EffectiveConfig {
                config_map: self.applied.as_ref().and_then(|c| c.config.clone()),
            },
        }
    }

    fn package_statuses(&self) -> Option<PackageStatuses> {
        self.accepts_packages.then(|| self.package_statuses())
    }

    fn available_components(&self) -> Option<&AvailableComponents> {
        self.available_components.as_ref()
    }
}

impl Local {
    /// The package this Agent is processing or has: the one being installed, else the installed
    /// one, else the one last offered — and for the Client's own Agent, else the one it consents
    /// to, which it knows from its own configuration before any offer arrives (ADR-0021).
    ///
    /// That last fallback is what lets this Client state a version for its own package from the
    /// first report on. A Supervisor has no such name: which package it gets is the Server's
    /// choice, so before an offer there is nothing to key a status by, and `None` is then the
    /// whole of "all packages the Agent has".
    fn package_name(&self) -> Option<String> {
        self.installing
            .as_ref()
            .map(|d| d.name.clone())
            .or_else(|| self.installed_package.as_ref().map(|p| p.name.clone()))
            .or_else(|| self.offered_name.clone())
            .or_else(|| self.expected_package.clone())
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
        //
        // For the Client's own Agent there is one without an install record too: *this process*.
        // A Client that arrived by `.deb`, `.rpm`, MSI or by hand has installed no package, and
        // reporting nothing there says "nothing installed under this name" — which since ADR-0027
        // is precisely the answer that lets a Set of the version it already runs reach it, and a
        // Set *older* than it downgrade it. The binary knows what it is; the record only says how
        // it got here.
        let (has_version, has_hash) = self
            .installed_package
            .as_ref()
            .map(|p| {
                (
                    p.version.clone(),
                    hex::decode(&p.hash_hex).unwrap_or_default(),
                )
            })
            .or_else(|| {
                // The *identity* of what this binary reports, not the whole string: a version
                // recorded by an install carries the operator's spelling, without the build
                // metadata this binary appends (ADR-0013), and the two have to read alike in the
                // fleet view. Nothing is lost — metadata takes no part in a comparison.
                self.expected_package.as_ref().and_then(|_| {
                    fleet_core::version::identity(fleet_core::version::current())
                        .map(|version| (version.to_string(), Vec::new()))
                })
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
        } else if !has_version.is_empty() {
            // Installed, whether a package put it there or an installer did: the status describes
            // what is on the host under this name, not how it arrived.
            PackageStatusEnum::Installed
        } else {
            PackageStatusEnum::InstallPending
        };
        let (offered_version, offered_hash) = self
            .server_offered
            .clone()
            .unwrap_or_else(|| (String::new(), Vec::new()));
        let package = PackageStatus {
            name: name.clone(),
            // While installing this is still the old one (we have not switched yet).
            agent_has_version: has_version,
            agent_has_hash: has_hash,
            server_offered_version: offered_version,
            server_offered_hash: offered_hash,
            status: status as i32,
            error_message: self.package_error.clone(),
            // "Should only be set if status is Downloading" — so it rides exactly that status.
            download_details: self.downloading,
        };
        PackageStatuses {
            packages: [(name, package)].into(),
            server_provided_all_packages_hash: self.echoed_all_packages_hash.clone(),
            error_message: self.offer_error.clone(),
        }
    }

    /// Reacts to a `PackagesAvailable` offer: the Server selected what this Agent may have
    /// (ADR-0020), so this side takes the one **top-level** package out of the offer — the binary
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
            if offer.packages.is_empty() {
                self.echoed_all_packages_hash = offer.all_packages_hash.clone();
                self.package_status_owed = true;
            } else {
                error!(
                    packages = offer.packages.len(),
                    "refusing an offer of addons only: this Client installs top-level packages only"
                );
                self.refuse_offer(
                    offer,
                    "this Client installs top-level packages only; the offer carries addons only"
                        .to_string(),
                    handled,
                );
            }
            return;
        };
        if let Some((second, _)) = top_level.next() {
            error!(
                first = %name, second = %second,
                "refusing an offer with two top-level packages: an Agent has one binary to replace"
            );
            self.refuse_offer(
                offer,
                format!(
                    "the Server offered two top-level packages ({name:?} and {second:?}); \
                     an Agent has one binary to replace"
                ),
                handled,
            );
            return;
        }
        let name = name.clone();
        // The Client's own Agent takes one named package and nothing else (ADR-0021). A
        // fleet-wide package with an empty Selector reaches every consenting Agent, so without
        // this an artifact meant for a Collector would be written over this binary and the host
        // would be gone. Refused and reported, never silently ignored.
        if let Some(expected) = self.expected_package.as_ref().filter(|e| **e != name) {
            error!(
                offered = %name, expected = %expected,
                "refusing a package this Agent was not configured to take"
            );
            let error = format!(
                "this Agent installs only the package {expected:?}; the Server offered {name:?}"
            );
            self.refuse_offer(offer, error, handled);
            return;
        }
        // A usable offer clears whatever the last unusable one complained about.
        self.offer_error.clear();
        self.offered_name = Some(name.clone());
        self.server_offered = Some((available.version.clone(), available.hash.clone()));
        if self.already_has(available) {
            // Already running this package: in sync — echo the aggregate to end the offer.
            self.echoed_all_packages_hash = offer.all_packages_hash.clone();
            self.package_status_owed = true;
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
            // The Baseline asks the Agent to send these on the GET; the Server fills them for a
            // referenced source from what the operator said it needs (ADR-0019).
            headers: file
                .headers
                .as_ref()
                .map(|headers| {
                    headers
                        .headers
                        .iter()
                        .map(|header| (header.key.clone(), header.value.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        };
        info!(package = %name, version = %available.version, "package offered; installing");
        self.installing = Some(download.clone());
        self.package_error.clear();
        self.package_status_owed = true;
        handled.send_report = true;
        handled.package_download = Some(download);
    }

    /// Refuses an offer this Agent cannot act on, and says why: nothing is left installing, the
    /// reason is reported as the offer's own error, and the aggregate is echoed at once so the
    /// Server stops re-offering it — a refusal is a report, not a loop.
    fn refuse_offer(
        &mut self,
        offer: &opamp::proto::PackagesAvailable,
        error: String,
        handled: &mut Handled,
    ) {
        self.installing = None;
        self.offer_error = error;
        self.echoed_all_packages_hash = offer.all_packages_hash.clone();
        self.package_status_owed = true;
        handled.send_report = true;
    }

    /// Whether this offer is what the Agent already has, so the offer ends with an echo rather
    /// than a download.
    ///
    /// For the package that carries the Client itself (ADR-0021, ADR-0023) that question is
    /// answered by the version *this process runs* — since ADR-0027, what a program reports about
    /// itself outranks what a record says was once installed here. The record's hash would
    /// otherwise end an offer of the very bytes this host is not running: a state directory that
    /// outlived its binary claims a version, the Server offers it again, and the claim is what
    /// swallows the offer.
    ///
    /// A Supervisor has no such answer — the Managed Process's version is the process's own, and a
    /// package numbers it in whatever space the operator chose — so there the installed hash stays
    /// the test: the same bytes are the same package.
    fn already_has(&self, available: &PackageAvailable) -> bool {
        if self.expected_package.is_some() {
            return fleet_core::version::same_release(
                fleet_core::version::current(),
                &available.version,
            );
        }
        self.installed_package
            .as_ref()
            .map(|p| hex::decode(&p.hash_hex).unwrap_or_default())
            .as_deref()
            == Some(available.hash.as_slice())
    }

    fn describe(&self, uid: &InstanceUid) -> AgentDescription {
        // `service.name` is the Agent *type* — the Baseline's "reverse FQDN that uniquely
        // identifies the Agent type" (ADR-0024). It used to carry the instance name, which a
        // Managed Process reporting its own type then destroyed; the instance name now has its own
        // key below, out of the way of the fold.
        let mut identifying_attributes =
            vec![string_attr(attributes::SERVICE_NAME, &self.service_name)];
        // The Baseline lists `service.namespace` second, among what identifies the Agent — it says
        // *which* deployment this service belongs to, so it belongs beside the name rather than
        // among the tags an operator hangs on it.
        if let Some(namespace) = &self.namespace {
            identifying_attributes.push(string_attr(attributes::SERVICE_NAMESPACE, namespace));
        }
        // `service.version` is the *Agent's* version. The self-Agent is the Client, so its baked
        // version is the truth; a Supervisor-backed Agent stands for its Managed Process, whose
        // version only the process itself can report (folded in below, goal 16) — never invented
        // from the Client's.
        if !self.managed {
            identifying_attributes.push(string_attr(
                attributes::SERVICE_VERSION,
                fleet_core::version::current(),
            ));
        }
        identifying_attributes.push(string_attr("service.instance.id", &uid.to_string()));
        // The rest of what the Baseline asks for "to describe where the Agent runs": `os.*` and
        // `host.*`. Every one of them is best effort, and what the platform cannot answer is left
        // out rather than filled in — an absent attribute says "unknown", where one carrying a
        // placeholder would say something false that a Selector could then match.
        let os = self.host.os();
        let mut non_identifying_attributes = vec![
            // The operator's name for this Agent (ADR-0024). Non-identifying because the Baseline
            // has no key for a human instance name and admits "any user-defined attributes the end
            // user would like to associate with this Agent" here; identity itself stays
            // `service.instance.id`. A Selector can match it, which is how ADR-0020's "pin one
            // host" is expressed for a machine running several Supervisors.
            string_attr(attributes::SERVICE_INSTANCE_NAME, &self.instance_name),
            string_attr(attributes::OS_TYPE, os_type()),
            string_attr(attributes::HOST_ARCH, host_arch()),
        ];
        for (key, value) in [
            ("os.name", os.name.as_deref()),
            ("os.version", os.version.as_deref()),
            ("os.build_id", os.build_id.as_deref()),
            (attributes::OS_DESCRIPTION, os.description.as_deref()),
            ("host.name", self.host.host_name()),
            ("host.id", self.host.host_id()),
            ("host.cpu.model.name", self.host.cpu_model()),
        ] {
            if let Some(value) = value {
                non_identifying_attributes.push(string_attr(key, value));
            }
        }
        // The host's network addresses — the conventions' `host.ip` and `host.mac` (ADR-0024),
        // both arrays and both "excluding loopback interfaces". Read live rather than once, so a
        // DHCP move is reported instead of the address the process happened to start with; a host
        // with nothing to say reports no attribute rather than an empty array.
        let (ips, macs) = self.host.addresses();
        for (key, values) in [("host.ip", ips), ("host.mac", macs)] {
            if !values.is_empty() {
                non_identifying_attributes.push(string_array_attr(key, &values));
            }
        }
        let mut description = AgentDescription {
            identifying_attributes,
            non_identifying_attributes,
        };
        // Operator-defined attributes (ADR-0016) — added only where nothing is reported under the
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
        // Supervisor's uid (goal 16) and called what the operator called it (ADR-0024). A process
        // cannot know either, so a value it reports under those keys is not an improvement. Its
        // `service.name` deliberately *does* win — a Collector's `dist.name` is a better type than
        // anything this file can infer.
        //
        // The exemption is by *key*, in both lists, and not by the list the Supervisor happens to
        // put each one in: the Server resolves a Selector against the identifying attributes first
        // and the non-identifying ones only after (`configs.rs`), so a process reporting either key
        // among the other list's would win every Selector while the fleet row went on showing the
        // Supervisor's value.
        if let Some(reported) = &self.process_description {
            let supervisors_own = |key: &str| {
                key == "service.instance.id" || key == attributes::SERVICE_INSTANCE_NAME
            };
            for attr in &reported.identifying_attributes {
                if !supervisors_own(&attr.key) {
                    upsert_attr(&mut description.identifying_attributes, attr);
                }
            }
            for attr in &reported.non_identifying_attributes {
                if !supervisors_own(&attr.key) {
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
                status_time_unix_nano: self.host.now_ns(),
                ..Default::default()
            },
            // The self-Agent's health is being alive.
            None => ComponentHealth {
                healthy: true,
                start_time_unix_nano: self.start_time_ns,
                status: "running".to_string(),
                status_time_unix_nano: self.host.now_ns(),
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
    fleet_core::platform::canonical_os(std::env::consts::OS)
}

/// OpenTelemetry semantic-convention value for `host.arch` — the convention says `amd64`/`arm64`
/// where Rust's constant says `x86_64`/`aarch64` (ADR-0020).
///
/// The Baseline points at the conventions for these keys, and the Collector's `opampextension`
/// reports `runtime.GOARCH`, which is already this vocabulary. Reporting Rust's spelling instead
/// meant the *same host* changed architecture depending on whether a Collector was running on it:
/// a Managed Process's attributes are folded over the Supervisor's, so `amd64` overwrote `x86_64`
/// and any Selector written against one of them stopped matching.
fn host_arch() -> &'static str {
    fleet_core::platform::canonical_arch(std::env::consts::ARCH)
}

/// A remote-configuration status for `hash`. `error_message` is empty except for `FAILED`.
fn config_status(
    hash: Vec<u8>,
    status: RemoteConfigStatuses,
    error_message: String,
) -> RemoteConfigStatus {
    RemoteConfigStatus {
        last_remote_config_hash: hash,
        status: status as i32,
        error_message,
    }
}

/// Whether an offer carries anything this Client can put in force (ADR-0018 clause 4): OpAMP
/// settings, or a destination for one of the three own-telemetry signals.
///
/// `other_connections` deliberately does not count. `AcceptsOtherConnectionSettings` is undeclared,
/// so a conforming Server never sends one — and acknowledging what cannot be applied is the lie this
/// whole path exists to prevent.
fn carries_settings(offers: &ConnectionSettingsOffers) -> bool {
    offers.opamp.is_some()
        || offers.own_metrics.is_some()
        || offers.own_traces.is_some()
        || offers.own_logs.is_some()
}

/// A connection-settings status for `hash` (ADR-0018). `error_message` is empty except for
/// `FAILED`.
fn settings_status(
    hash: Vec<u8>,
    status: ConnectionSettingsStatuses,
    error_message: String,
) -> ConnectionSettingsStatus {
    ConnectionSettingsStatus {
        last_connection_settings_hash: hash,
        status: status as i32,
        error_message,
    }
}

/// One offered package the transport must download, verify, and hand to the Supervisor. Built by
/// the Agent state machine from a `PackageAvailable`; the raw fields it needs travel here.
#[derive(Clone, PartialEq, Eq)]
pub struct PackageDownload {
    pub name: String,
    pub version: String,
    /// The package hash the reported status refers to.
    pub hash: Vec<u8>,
    /// The `download_url` from the offer — absolute, or a path resolved against the endpoint.
    pub download_url: String,
    /// The expected SHA-256 of the artifact.
    pub content_hash: Vec<u8>,
    /// The Ed25519 signature over the artifact; empty means unsigned.
    pub signature: Vec<u8>,
    /// The headers the offer says this download needs — a referenced source's credential
    /// (ADR-0019), which the Server fills from the operator's configuration. The Baseline: *"The
    /// Agent SHOULD include the HTTP headers provided in the headers field for the GET request."*
    ///
    /// Raw pairs rather than the wire type, like every other field here: what the download needs is
    /// a name and a value, not a protobuf message.
    pub headers: Vec<(String, String)>,
}

/// Written by hand rather than derived, because a header value is a credential.
///
/// This struct travels inside `Handled`, which derives `Debug`; a single `debug!(?handled)` added
/// later would otherwise put a fleet credential in the log file that ADR-0014 writes to disk in
/// service mode. Keys are printed — they are what a diagnosis needs — and values never are.
impl std::fmt::Debug for PackageDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageDownload")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("hash", &self.hash)
            .field("download_url", &self.download_url)
            .field("content_hash", &self.content_hash)
            .field("signature", &self.signature)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(key, _)| format!("{key}: <redacted>"))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use opamp::proto::{AgentConfigMap, AgentConfigObject};
    use opamp::proto::{ServerErrorResponseType, ServerToAgentFlags};
    use std::collections::HashMap;

    /// ADR-0018 clause 4: an offer that names a telemetry destination is actionable, whether or not
    /// it carries OpAMP settings — and one that carries nothing this Client applies is not.
    /// Verifies: ADR-0041
    #[test]
    fn an_offer_carries_settings_when_it_names_anything_this_client_applies() {
        assert!(carries_settings(&ConnectionSettingsOffers {
            own_metrics: Some(opamp::proto::TelemetryConnectionSettings::default()),
            ..Default::default()
        }));
        assert!(carries_settings(&ConnectionSettingsOffers {
            opamp: Some(opamp::proto::OpAmpConnectionSettings::default()),
            ..Default::default()
        }));
        assert!(!carries_settings(&ConnectionSettingsOffers::default()));
    }

    /// `/proc/cpuinfo` always names an x86 model, so on this platform the attribute must be
    /// there — everywhere else it stays best effort.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn the_cpu_model_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let attrs = reported(&make_agent(dir.path()).describe());
        assert!(
            attrs
                .get("host.cpu.model.name")
                .is_some_and(|model| !model.is_empty()),
            "no host.cpu.model.name in {attrs:?}"
        );
    }

    fn make_agent(dir: &std::path::Path) -> AgentState {
        let storage = Storage::new(dir.to_path_buf()).expect("storage");
        AgentState::new("test-agent".to_string(), storage, crate::host::SystemHost).expect("agent")
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

    #[test]
    fn configured_attributes_are_reported_but_never_shadow_reported_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let agent = AgentState::new("test-agent".to_string(), storage, crate::host::SystemHost)
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
    /// receives, and therefore what a Selector matches against. A string array (`host.ip`,
    /// `host.mac`, ADR-0024) reads joined, as the Server's view joins it.
    fn reported(description: &AgentDescription) -> std::collections::BTreeMap<String, String> {
        description
            .identifying_attributes
            .iter()
            .chain(&description.non_identifying_attributes)
            .map(|kv| {
                let value = match kv.value.as_ref().and_then(|v| v.value.as_ref()) {
                    Some(opamp::proto::any_value::Value::StringValue(s)) => s.clone(),
                    Some(opamp::proto::any_value::Value::ArrayValue(list)) => list
                        .values
                        .iter()
                        .filter_map(|v| match v.value.as_ref() {
                            Some(opamp::proto::any_value::Value::StringValue(s)) => {
                                Some(s.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                    other => panic!("{} must be a string or string array, got {other:?}", kv.key),
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

    /// The defect ADR-0024 exists for: a Collector's `opampextension` reports the type it was
    /// built with, and folding that in used to overwrite the operator's name for the Supervisor —
    /// so every Collector of one distribution collapsed onto one name in the fleet view. Both
    /// values must survive, each in its own key, each won by the side that actually knows it.
    // Verifies: ADR-0024
    #[test]
    fn a_process_reporting_its_type_does_not_take_the_operators_name_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol-edge-01".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
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

    /// And the exemption holds by *key*, not by the list the Supervisor states each one in. A
    /// process reporting either of them among its *identifying* attributes would otherwise win
    /// every Selector — the Server resolves those before the non-identifying ones — while the fleet
    /// row, which reads the other list, went on showing the operator's value.
    #[test]
    fn the_supervisors_own_attributes_survive_whichever_list_a_process_reports_them_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol-edge-01".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        let uid = reported(&agent.describe())
            .get("service.instance.id")
            .cloned()
            .expect("the Supervisor's uid");

        // Both keys, each in the list the fold does *not* exempt it in.
        agent.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.instance.name", "some-collector")],
            non_identifying_attributes: vec![string_attr("service.instance.id", "not-the-uid")],
        });

        let description = agent.describe();
        assert!(
            !description
                .identifying_attributes
                .iter()
                .any(|kv| kv.key == "service.instance.name"),
            "an instance name among the identifying attributes would win every Selector"
        );
        let after = reported(&description);
        assert_eq!(
            after.get("service.instance.name").map(String::as_str),
            Some("otelcol-edge-01"),
            "the operator's name stands, wherever the process put its own"
        );
        assert_eq!(
            after.get("service.instance.id"),
            Some(&uid),
            "and identity stays the Supervisor's uid"
        );
    }

    /// The Client's own Agent is one *kind* of thing across the whole fleet, so its type is the
    /// constant `supervisor` (ADR-0023) and not whatever the operator called this instance — which
    /// is what lets one Selector on the type aim at every Client in the fleet at once (ADR-0024).
    /// Verifies: ADR-0047
    #[test]
    fn the_clients_own_agent_reports_its_type_and_its_configured_name_separately() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let agent = AgentState::new("edge-fra1".to_string(), storage, crate::host::SystemHost)
            .expect("agent");
        let attributes = reported(&agent.describe());
        assert_eq!(
            attributes.get("service.name").map(String::as_str),
            Some(CLIENT_AGENT_TYPE)
        );
        assert_eq!(
            attributes.get("service.instance.name").map(String::as_str),
            Some("edge-fra1")
        );
    }

    /// ADR-0023 pins the value, not just the separation: the type is `supervisor`. ADR-0023 then
    /// gave the program, its service and its configuration file the same word, so what began as
    /// the Agent's *role* is now the one name this thing has anywhere — which is the point, and
    /// which is why the two constants are asserted to agree rather than to differ.
    /// Verifies: ADR-0047
    #[test]
    fn the_clients_own_agent_type_is_the_one_name_this_program_has() {
        assert_eq!(CLIENT_AGENT_TYPE, "supervisor");
        assert_eq!(
            CLIENT_AGENT_TYPE,
            crate::service::layout::COMPONENT,
            "the type, the program and the service are one word since ADR-0023"
        );
    }

    /// `[supervisor.attributes]` is a fallback for keys nothing else reports, so it must not be a
    /// second way to set the two attributes the Supervisor itself owns.
    // Verifies: ADR-0024
    #[test]
    fn configured_attributes_cannot_restate_the_type_or_the_instance_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let agent = AgentState::supervised(
            "edge-01".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
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

    /// ADR-0020 twice offers "a Selector matching that host's `host.name`" as the way to pin one
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
        let configured =
            AgentState::new("test-agent".to_string(), storage, crate::host::SystemHost)
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
            Some(fleet_core::version::current())
        );

        // A Supervisor-backed Agent reports no version until its Managed Process states one.
        let storage = Storage::new(dir.path().join("supervised")).expect("storage");
        let mut supervised = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        assert_eq!(version_of(&supervised), None);

        supervised.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.version", "0.142.0")],
            non_identifying_attributes: vec![],
        });
        assert_eq!(version_of(&supervised).as_deref(), Some("0.142.0"));
    }

    /// The type the metrics are labelled with is the type the fleet view shows — the process's own
    /// word where it gives one, the configured type otherwise. Two answers to "what is this Agent"
    /// would be worse than none: a series labelled `otelcol` beside a fleet row reading
    /// `otelcol-contrib` is a question, not a fact.
    #[test]
    fn the_reported_type_is_the_processs_own_word_where_it_gives_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("supervised")).expect("storage");
        let mut agent = AgentState::supervised(
            "edge-01".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        assert_eq!(agent.service_name(), "otelcol");

        agent.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.name", "otelcol-contrib")],
            non_identifying_attributes: vec![],
        });
        assert_eq!(agent.service_name(), "otelcol-contrib");
        assert_eq!(
            reported(&agent.describe()).get("service.name").cloned(),
            Some("otelcol-contrib".to_string()),
            "the accessor mirrors the fold rather than diverging from it"
        );

        // The one case where the two part company, asserted so that it is a decision rather than a
        // surprise: an empty string is not a value here (ADR-0020), while the fold replaces by key
        // without judging the value.
        agent.set_process_description(AgentDescription {
            identifying_attributes: vec![string_attr("service.name", "")],
            non_identifying_attributes: vec![],
        });
        assert_eq!(agent.service_name(), "otelcol");
        assert_eq!(
            reported(&agent.describe()).get("service.name").cloned(),
            Some(String::new())
        );
    }

    /// Two sources describe the same Managed Process — the version probe, which reports
    /// `service.version` alone after every package swap, and the opampextension, which reports
    /// everything else. Replacing rather than merging would make each new probe erase the
    /// extension's self-report, and each self-report erase the probed version.
    #[test]
    fn what_the_process_reports_about_itself_accumulates_across_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("supervised")).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
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
                    opamp::proto::any_value::Value::StringValue(s) => s,
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
        let mut supervised = AgentState::supervised(
            "s".to_string(),
            "s".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
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

    /// ADR-0021: the Client's own Agent takes one named package and refuses everything else. A
    /// package with an empty Selector reaches every consenting Agent (ADR-0020), so without this
    /// the first fleet-wide Collector artifact an operator uploads would be written over the
    /// Client and take the host out of reach.
    /// Verifies: ADR-0044
    #[test]
    fn the_self_agent_refuses_a_package_it_was_not_configured_to_take() {
        use opamp::proto::{DownloadableFile, PackageAvailable, PackagesAvailable};

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::new(
            "opamp-fleet-client".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
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

    /// One offer of a package, by name and hash — the shape both restore tests below hand to the
    /// Agent to ask "would you take this again?".
    fn package_offer(name: &str, version: &str, hash: &[u8]) -> opamp::proto::PackagesAvailable {
        use opamp::proto::{DownloadableFile, PackageAvailable, PackagesAvailable};
        PackagesAvailable {
            packages: [(
                name.to_string(),
                PackageAvailable {
                    version: version.to_string(),
                    file: Some(DownloadableFile {
                        download_url: "/x".to_string(),
                        content_hash: b"chash".to_vec(),
                        ..Default::default()
                    }),
                    hash: hash.to_vec(),
                    ..Default::default()
                },
            )]
            .into(),
            all_packages_hash: b"agg-1".to_vec(),
        }
    }

    /// A record naming a version this binary is not is a record about a binary that is gone:
    /// `service uninstall` keeps the state, so reinstalling an older Client lands on top of what
    /// its successor wrote. Reported, it would tell the Server a version this host does not run —
    /// and the hash gate inside it would stop the package ever being offered here again.
    #[test]
    fn a_package_record_from_another_version_is_discarded_on_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        storage
            .store_package(&InstalledPackage {
                name: "opamp-client".to_string(),
                version: "9.9.9".to_string(),
                hash_hex: hex::encode(b"pkg-hash"),
            })
            .expect("store the record a successor left behind");

        let mut agent = AgentState::new(
            "opamp-fleet-client".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        agent.accept_packages_named("opamp-client".to_string());

        // What is claimed is what this binary *is* — never the version the record named.
        let statuses = agent
            .next_report()
            .package_statuses
            .expect("a package status");
        let reported = statuses
            .packages
            .get("opamp-client")
            .expect("the package this Client consents to is named from the first report");
        assert_eq!(
            Some(reported.agent_has_version.as_str()),
            fleet_core::version::identity(fleet_core::version::current()),
            "a version this Client does not run must not be reported as installed"
        );
        assert!(
            reported.agent_has_hash.is_empty(),
            "and no hash is invented for bytes no package delivered"
        );
        assert!(
            !dir.path().join("installed-package.json").exists(),
            "the stale record is dropped from disk, not only from memory"
        );

        // And the very bytes the record named are taken again, rather than echoed as in sync.
        let handled = agent.handle(&ServerToAgent {
            packages_available: Some(package_offer("opamp-client", "9.9.9", b"pkg-hash")),
            ..Default::default()
        });
        let download = handled
            .package_download
            .expect("the package is offered to this Client again");
        assert_eq!(download.name, "opamp-client");
    }

    /// The other half, and the one that matters more: an ordinary restart must keep its record.
    /// A Client that discarded it would reinstall the version it already runs on every start.
    #[test]
    fn a_package_record_for_the_running_version_survives_a_restart() {
        use opamp::proto::PackageStatusEnum;

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        storage
            .store_package(&InstalledPackage {
                name: "opamp-client".to_string(),
                // What the Server was told to offer: the release, without the build metadata this
                // binary carries (ADR-0013).
                version: fleet_core::version::parse(fleet_core::version::current())
                    .expect("this build's version parses")
                    .identity()
                    .to_string(),
                hash_hex: hex::encode(b"pkg-hash"),
            })
            .expect("store");

        let mut agent = AgentState::new(
            "opamp-fleet-client".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        agent.accept_packages_named("opamp-client".to_string());

        let statuses = agent
            .next_report()
            .package_statuses
            .expect("a package status");
        let status = statuses.packages.get("opamp-client").expect("the package");
        assert_eq!(status.status, PackageStatusEnum::Installed as i32);
        assert!(dir.path().join("installed-package.json").exists());

        // Offered the version it runs, this Client is in sync and downloads nothing.
        let running =
            fleet_core::version::identity(fleet_core::version::current()).expect("this version");
        let handled = agent.handle(&ServerToAgent {
            packages_available: Some(package_offer("opamp-client", running, b"pkg-hash")),
            ..Default::default()
        });
        assert!(
            handled.package_download.is_none(),
            "a restarted Client must not reinstall the version it already runs"
        );
    }

    /// ADR-0027 point 15: for the package that carries this Client, *already installed* is what this
    /// process runs — never a hash in a record. The same bytes can be published under a new version,
    /// and a record about a binary that is gone must not swallow the offer that would replace it:
    /// the Server offers because the Agent reports running something older, and a Client that
    /// answered "in sync" from its record would strand the host exactly where ADR-0027 found it.
    // Verifies: ADR-0027
    #[test]
    fn the_clients_own_offer_is_settled_by_the_version_it_runs_not_by_a_recorded_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        storage
            .store_package(&InstalledPackage {
                name: "opamp-client".to_string(),
                version: fleet_core::version::identity(fleet_core::version::current())
                    .expect("this version")
                    .to_string(),
                hash_hex: hex::encode(b"pkg-hash"),
            })
            .expect("store");

        let mut agent = AgentState::new(
            "opamp-fleet-client".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        agent.accept_packages_named("opamp-client".to_string());

        // The record's own hash, under a version this Client does not run: taken, not echoed.
        let handled = agent.handle(&ServerToAgent {
            packages_available: Some(package_offer("opamp-client", "9.9.9", b"pkg-hash")),
            ..Default::default()
        });
        let download = handled
            .package_download
            .expect("a version this Client does not run is installed, whatever the record holds");
        assert_eq!(download.version, "9.9.9");

        // A Supervisor keeps the hash test: the Managed Process's version is numbered in whatever
        // space the operator chose, so the same bytes are the same package and nothing else is.
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        storage
            .store_package(&InstalledPackage {
                name: "otelcol".to_string(),
                version: "2.0.0".to_string(),
                hash_hex: hex::encode(b"pkg-hash"),
            })
            .expect("store");
        let mut supervised = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        supervised.accept_packages();
        let handled = supervised.handle(&ServerToAgent {
            packages_available: Some(package_offer("otelcol", "2.0.0", b"pkg-hash")),
            ..Default::default()
        });
        assert!(
            handled.package_download.is_none(),
            "the same bytes are the package a Supervisor already installed"
        );
    }

    /// Verifies: ADR-0042
    #[test]
    fn a_package_offer_for_the_named_package_is_acknowledged_installing_and_handed_over() {
        use opamp::proto::{
            DownloadableFile, PackageAvailable, PackageStatusEnum, PackagesAvailable,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
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
    /// Verifies: ADR-0042, ADR-0045
    #[test]
    fn an_addon_package_is_refused_instead_of_overwriting_the_binary() {
        use opamp::proto::{DownloadableFile, PackageAvailable, PackagesAvailable};
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
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

    /// A reply declaring a Capability Set, so a test can say what the Server accepts.
    fn declaring(capabilities: u64) -> ServerToAgent {
        ServerToAgent {
            capabilities,
            ..Default::default()
        }
    }

    /// ADR-0018 clause 14: once the Server has declared its capabilities, package status stops going
    /// to one that cannot take it. The Baseline makes this a MUST in both directions, and until now
    /// only two of seven Server bits changed any behaviour here.
    /// Verifies: ADR-0041
    #[test]
    fn package_statuses_stop_once_the_server_says_it_accepts_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        agent.accept_packages();

        // A Server that takes status reports and offers configuration, but no package status.
        agent.handle(&declaring(
            ServerCapabilities::AcceptsStatus as u64
                | ServerCapabilities::OffersRemoteConfig as u64,
        ));
        agent.force_full();
        assert!(
            agent.next_report().package_statuses.is_none(),
            "an undeclared capability must not be exercised"
        );

        // …and it comes back the moment the Server declares the bit.
        agent.handle(&declaring(
            ServerCapabilities::AcceptsStatus as u64
                | ServerCapabilities::AcceptsPackagesStatus as u64,
        ));
        agent.force_full();
        assert!(agent.next_report().package_statuses.is_some());
    }

    /// And the optimistic half (clause 1): before the Server has said anything there is nothing to
    /// obey, so the first report — which necessarily precedes any declaration — carries everything.
    /// Verifies: ADR-0041
    #[test]
    fn package_statuses_ride_until_the_server_has_spoken() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        agent.accept_packages();
        assert!(agent.next_report().package_statuses.is_some());
    }

    /// ADR-0018 clause 13, and the reason the naive gate is wrong: a Server may send an offer
    /// *without* declaring `OffersConnectionSettings` — this project's own does exactly that for a
    /// `[telemetry_offer]`-only or `[client_ca]`-only configuration. Withholding the acknowledgement
    /// would leave its hash gate open and have it re-offer for ever, so the offer arms the report.
    /// Verifies: ADR-0041
    #[test]
    fn a_connection_settings_status_is_reported_to_a_server_that_offered_without_declaring_the_bit()
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent =
            AgentState::new("self".to_string(), storage, crate::host::SystemHost).expect("agent");

        agent.handle(&ServerToAgent {
            // Only the one bit every Server MUST set — no OffersConnectionSettings.
            capabilities: ServerCapabilities::AcceptsStatus as u64,
            connection_settings: Some(opamp::proto::ConnectionSettingsOffers {
                hash: b"offer-1".to_vec(),
                own_logs: Some(opamp::proto::TelemetryConnectionSettings {
                    destination_endpoint: "https://collector.example:4318/v1/logs".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        });

        let status = agent
            .next_report()
            .connection_settings_status
            .expect("the offer arms the report whatever the bitmask says");
        assert_eq!(status.last_connection_settings_hash, b"offer-1");
    }

    /// The other side of clause 2, and the only shape where the gate is actually observable: a
    /// **restarted** Client holds a status from its persisted settings (ADR-0018) without any offer
    /// having arrived in this process. Sent to a Server that declares only the mandatory bit, that
    /// status exercises a capability the Server never claimed — so it is withheld until the Server
    /// either declares the bit or offers something.
    /// Verifies: ADR-0041
    #[test]
    fn a_restored_connection_settings_status_is_withheld_from_a_server_that_never_offers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent =
            AgentState::new("self".to_string(), storage, crate::host::SystemHost).expect("agent");
        // What `runtime.rs` does at startup when `connection-settings.pb` exists.
        agent.adopt_connection_settings(b"persisted-1");

        agent.handle(&declaring(ServerCapabilities::AcceptsStatus as u64));
        agent.force_full();
        assert!(
            agent.next_report().connection_settings_status.is_none(),
            "an undeclared capability must not be exercised"
        );

        // Declaring the bit brings it straight back.
        agent.handle(&declaring(
            ServerCapabilities::AcceptsStatus as u64
                | ServerCapabilities::OffersConnectionSettings as u64,
        ));
        agent.force_full();
        assert!(agent.next_report().connection_settings_status.is_some());
    }

    /// ADR-0018 clause 16, written as an assertion so the deliberate non-gate cannot be silently
    /// reversed by someone applying the MUST field by field. `OffersRemoteConfig` says the Server
    /// *can offer* configuration; what licenses this inbound status is `AcceptsStatus`, and gating
    /// it would silence the hash the Server's re-offer decision depends on.
    /// Verifies: ADR-0041
    #[test]
    fn a_remote_config_status_rides_to_a_server_that_offers_no_remote_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");

        // A configuration was offered and acknowledged at some point…
        agent.handle(&ServerToAgent {
            remote_config: Some(AgentRemoteConfig {
                config: Some(AgentConfigMap {
                    config_map: Default::default(),
                }),
                config_hash: b"cfg-1".to_vec(),
            }),
            ..Default::default()
        });
        // …and the Server now declares nothing but the mandatory bit.
        agent.handle(&declaring(ServerCapabilities::AcceptsStatus as u64));
        agent.force_full();

        let status = agent
            .next_report()
            .remote_config_status
            .expect("the config hash must keep flowing, or the Server re-offers for ever");
        assert_eq!(status.last_remote_config_hash, b"cfg-1");
    }

    /// The headers a `DownloadableFile` names travel to the download that has to use them — the
    /// credential a referenced source needs (ADR-0019), which the Server fills from the operator's
    /// configuration.
    /// Verifies: ADR-0042
    #[test]
    fn a_package_offer_hands_its_download_headers_to_the_transport() {
        use opamp::proto::{
            DownloadableFile, Header, Headers, PackageAvailable, PackagesAvailable,
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        agent.accept_packages();

        let handled = agent.handle(&ServerToAgent {
            packages_available: Some(PackagesAvailable {
                packages: [(
                    "otelcol".to_string(),
                    PackageAvailable {
                        r#type: PackageType::TopLevel as i32,
                        version: "1.2.3".to_string(),
                        file: Some(DownloadableFile {
                            download_url: "https://mirror.example/otelcol.tar.gz".to_string(),
                            content_hash: b"hash".to_vec(),
                            signature: Vec::new(),
                            headers: Some(Headers {
                                headers: vec![Header {
                                    key: "X-Api-Key".to_string(),
                                    value: "operator-secret".to_string(),
                                }],
                            }),
                        }),
                        hash: b"pkg".to_vec(),
                    },
                )]
                .into(),
                all_packages_hash: b"agg".to_vec(),
            }),
            ..Default::default()
        });

        let download = handled.package_download.expect("a download");
        assert_eq!(
            download.headers,
            vec![("X-Api-Key".to_string(), "operator-secret".to_string())]
        );
    }

    /// Verifies: ADR-0042
    #[test]
    fn a_failed_package_reports_installed_failed_and_keeps_the_old_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut agent = AgentState::supervised(
            "otelcol".to_string(),
            "otelcol".to_string(),
            storage,
            crate::host::SystemHost,
        )
        .expect("agent");
        agent.accept_packages();
        agent.local.offered_all_packages_hash = b"agg-2".to_vec();
        agent.local.server_offered = Some(("9.9.9".to_string(), b"bad".to_vec()));
        agent.local.installing = Some(PackageDownload {
            name: "otelcol".to_string(),
            version: "9.9.9".to_string(),
            hash: b"bad".to_vec(),
            download_url: String::new(),
            content_hash: Vec::new(),
            signature: Vec::new(),
            headers: Vec::new(),
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

    /// Verifies: ADR-0041
    #[test]
    fn a_connection_offer_is_acknowledged_applying_and_handed_to_the_transport() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut agent = make_agent(dir.path());
        let _ = agent.next_report();

        // Every Agent declares it can accept and report on connection settings (ADR-0018).
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

    /// Verifies: ADR-0041
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

    /// The self-Agent's offer is acknowledged `APPLYING` and left pending for the Engine's
    /// Supervisor-set apply (ADR-0022); the verdict closes the lifecycle, and only then does the
    /// configuration echo as effective.
    /// Verifies: ADR-0041
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
        assert_eq!(status.status, RemoteConfigStatuses::Applying as i32);
        assert_eq!(status.last_remote_config_hash, b"hash-1");
        assert!(agent.take_pending_apply().is_some());

        agent.config_applied(b"hash-1".to_vec(), Ok(()));
        let done = agent.next_report();
        let status = done.remote_config_status.expect("status");
        assert_eq!(status.status, RemoteConfigStatuses::Applied as i32);
        assert_eq!(status.last_remote_config_hash, b"hash-1");
        assert!(done.effective_config.is_some());
    }

    /// A restart reports `APPLIED` only for a configuration whose apply actually finished
    /// (ADR-0022): the offer is persisted on the verdict, never on receipt, so a Client
    /// restarted mid-apply reports nothing and the Server offers again.
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
        // Interrupted mid-apply: nothing was persisted, the restarted Agent reports no status.
        {
            let mut interrupted = make_agent(dir.path());
            assert!(interrupted.next_report().remote_config_status.is_none());
            interrupted.handle(&ServerToAgent {
                remote_config: Some(remote_config(b"x: 1\n", b"hash-1")),
                ..Default::default()
            });
            interrupted.config_applied(b"hash-1".to_vec(), Ok(()));
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

    /// Verifies: ADR-0041
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
