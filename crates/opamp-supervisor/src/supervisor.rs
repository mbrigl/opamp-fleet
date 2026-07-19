//! The OpAMP client loop — the hexagonal **domain** (ADR-0009): connect, report identity, apply what
//! the Server sends, report the result. It is generic over the [`ManagedAgent`] port, so the same loop
//! drives an OpAMP-native Collector and a non-OpAMP Foreign Agent alike.
//!
//! This is the Agent side of the control loop the Server drives. The Server compares the config hash
//! the Agent last reported against the one it distributes and sends a configuration only when they
//! differ ([ADR-0006](../../docs/adr/0006-rust-opamp-server-from-spec.md)); the Agent's job is to apply
//! it and report the exact hash back so that comparison converges. For a Collector, correctness is
//! measured against the upstream Go Supervisor oracle
//! ([ADR-0008](../../docs/adr/0008-collector-supervisor-go-reference-compat.md)).
//!
//! The initial supervisor is plain-`ws`, unauthenticated, and does not do package updates — matching
//! the initial Server (ADR-0006). TLS + auth and package delivery are deferred to their own ADRs.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use std::collections::HashMap;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use opamp_proto::frame;
use opamp_proto::proto::{
    any_value, AgentCapabilities, AgentConfigFile, AgentConfigMap, AgentDescription,
    AgentRemoteConfig, AgentToServer, AnyValue, CommandType, ComponentHealth,
    ConnectionSettingsOffers, EffectiveConfig, Headers, KeyValue, PackageAvailable, PackageStatus,
    PackageStatusEnum, PackageStatuses, PackageType, PackagesAvailable, RemoteConfigStatus,
    RemoteConfigStatuses, ServerToAgent, ServerToAgentFlags, TelemetryConnectionSettings,
};

use crate::agent::{
    liveness_health, InstalledPackage, ManagedAgent, OwnTelemetry, TelemetryDestination,
};

/// The key a single-file agent's configuration is filed under in an OpAMP config map. The Server writes
/// the config under the empty-string key (the specification's SHOULD for a single-file agent), so that
/// is where the Agent reads it back from — the two must agree.
const MAIN_CONFIG_KEY: &str = "";

/// The content type the Server tags the config with, echoed back in effective config.
const CONFIG_CONTENT_TYPE: &str = "text/yaml";

/// A non-identifying attribute naming which supervisor manages the agent, so the fleet can tell this
/// project's agents apart from the upstream OpenTelemetry Supervisor's (which does not report it).
const SUPERVISOR_ATTRIBUTE: &str = "opamp.supervisor";

/// Reconnect backoff: start here, double after each failed attempt, capped at [`RECONNECT_MAX`], and
/// reset to the base once a connection is established. Each wait is jittered by [`RECONNECT_JITTER`] so a
/// fleet of supervisors that lost the same Server does not reconnect in lockstep (a thundering herd).
const RECONNECT_BASE: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
/// The randomization factor applied to each reconnect wait, matching the upstream opamp-go client: the
/// actual wait is uniformly in `[(1 - f)·delay, (1 + f)·delay]`.
const RECONNECT_JITTER: f64 = 0.5;

/// How often, between server messages, the supervisor checks that the managed agent is still alive.
const SUPERVISION_INTERVAL: Duration = Duration::from_secs(5);

/// How long, with `automatic_config_rollback` enabled, the supervisor waits for the agent to report
/// healthy on a freshly applied config before it reverts to the last good one. While it waits, the OpAMP
/// loop pauses (no heartbeats go out) — acceptable because applying a config is a rare, bounded event.
const ROLLBACK_HEALTH_TIMEOUT: Duration = Duration::from_secs(10);

/// Crash-restart backoff: an agent that exits again within [`RESTART_STABLE`] of its last restart is
/// treated as crash-looping and backed off — doubling from [`RESTART_BACKOFF_BASE`], capped at
/// [`RESTART_BACKOFF_MAX`] — so a persistently broken agent is not restarted in a tight loop. An agent
/// that stays up at least [`RESTART_STABLE`] is considered stable and resets the backoff to the base.
const RESTART_BACKOFF_BASE: Duration = Duration::from_secs(1);
const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(30);
const RESTART_STABLE: Duration = Duration::from_secs(60);

/// The mandatory capabilities every Agent declares. `ReportsStatus` MUST be set; the rest are exactly
/// the loop this Agent always implements. Own-telemetry bits (ADR-0010) are added per configuration on
/// top of these; we still never claim a capability we do not implement (packages).
const CAPABILITIES: u64 = AgentCapabilities::ReportsStatus as u64
    | AgentCapabilities::AcceptsRemoteConfig as u64
    | AgentCapabilities::ReportsEffectiveConfig as u64
    | AgentCapabilities::ReportsHealth as u64
    | AgentCapabilities::ReportsRemoteConfig as u64
    | AgentCapabilities::ReportsHeartbeat as u64
    | AgentCapabilities::AcceptsRestartCommand as u64
    | AgentCapabilities::AcceptsOpAmpConnectionSettings as u64
    | AgentCapabilities::ReportsAvailableComponents as u64;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Everything the [`Supervisor`] needs to identify itself and persist its identity and state.
pub struct Config {
    pub server_url: String,
    pub instance_uid: [u8; 16],
    /// Where the instance UID is persisted, so a Server-assigned UID survives a restart.
    pub uid_path: PathBuf,
    /// This supervisor's private storage directory: the applied config and its hash live here, so a
    /// supervisor restart resumes without re-applying.
    pub storage_dir: PathBuf,
    /// The reverse-FQDN agent type reported as `service.name`.
    pub service_name: String,
    /// The managed agent's version, reported as `service.version`; `None` if unknown.
    pub agent_version: Option<String>,
    /// The raw bodies of the startup fallback configs, in order — merged and applied before the Server
    /// answers so the agent runs. Empty when none is configured.
    pub fallback: Vec<Vec<u8>>,
    pub heartbeat: Duration,
    /// Extra non-identifying attributes from the supervisor configuration, added to every reported
    /// `AgentDescription` so an operator can label an agent (team, environment, …) from one place.
    pub extra_attributes: Vec<(String, String)>,
    /// The `ReportsOwn{Metrics,Logs,Traces}` capability bits this agent declares and honours (ADR-0010);
    /// `0` for an agent that does not report its own telemetry (e.g. a Foreign Agent).
    pub own_telemetry_capabilities: u64,
    /// Revert to the last healthy configuration when a newly applied one does not make the agent healthy
    /// (`automatic_config_rollback`, ADR-0008). Only useful for an agent that reports its own health.
    pub automatic_config_rollback: bool,
    /// The shared bearer token to present to the Server, or `None` for an unauthenticated connection
    /// (ADR-0012).
    pub auth_token: Option<String>,
    /// A PEM CA certificate to validate a `wss://` Server against, instead of the platform roots (ADR-0012).
    pub tls_ca: Option<Vec<u8>>,
    /// Skip TLS certificate validation — dangerous, development only (ADR-0012).
    pub tls_insecure: bool,
}

/// A snapshot of the OpAMP connection settings, kept so a freshly accepted offer can be reverted if it
/// fails to connect (ADR-0015).
#[derive(Clone)]
struct ConnSnapshot {
    server_url: String,
    auth_token: Option<String>,
    offered_headers: Vec<(String, String)>,
    tls_ca: Option<Vec<u8>>,
    tls_insecure: bool,
}

/// The accepted OpAMP connection settings persisted across restarts, so the Supervisor resumes on the
/// endpoint the Server last re-pointed it to rather than the bootstrap one (ADR-0015).
#[derive(Serialize, Deserialize, Default)]
struct PersistedConnSettings {
    server_url: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    ca_pem: Option<String>,
    #[serde(default)]
    insecure: bool,
}

/// The installed-package state persisted across restarts, so the Supervisor resumes reporting the
/// package it has and de-duplicates the next offer without re-installing (ADR-0018).
#[derive(Serialize, Deserialize, Default)]
struct PersistedPackages {
    /// The top-level package's name (the key it is reported under).
    #[serde(default)]
    name: String,
    /// The last aggregate `all_packages_hash` the Server offered, hex-encoded.
    #[serde(default)]
    all_hash: String,
    /// The installed top-level package, or `None` if none has been installed.
    #[serde(default)]
    installed: Option<InstalledPackage>,
}

/// The running supervisor: its identity, the [`ManagedAgent`] it drives, and the control-loop state it
/// needs to avoid redundant reconfiguration.
pub struct Supervisor<A: ManagedAgent> {
    server_url: String,
    instance_uid: Vec<u8>,
    uid_path: PathBuf,
    storage_dir: PathBuf,
    service_name: String,
    agent_version: Option<String>,
    agent_description: AgentDescription,
    /// Extra non-identifying attributes from configuration, added to every reported description.
    extra_attributes: Vec<(String, String)>,
    /// The capabilities this agent declares — the mandatory loop plus any own-telemetry bits (ADR-0010).
    capabilities: u64,
    agent: A,
    /// The startup fallback config bodies, merged and applied so the agent runs before the Server
    /// answers. Taken once, on `run`.
    fallback: Vec<Vec<u8>>,
    /// The server-provided hash of the configuration currently applied; empty means "none yet".
    applied_hash: Vec<u8>,
    /// The raw (pre-`prepare_config`) body currently applied, echoed as effective config and persisted.
    applied_body: Vec<u8>,
    sequence_num: u64,
    start_time_unix_nano: u64,
    heartbeat: Duration,
    reconnect_delay: Duration,
    /// Current crash-restart backoff, doubled on each rapid re-crash and reset when the agent is stable.
    restart_backoff: Duration,
    /// When the agent was last (re)started after a crash, to tell a crash loop from an isolated exit.
    last_restart: Option<Instant>,
    /// The bearer token presented to the Server on connect, or `None` (ADR-0012).
    auth_token: Option<String>,
    /// A PEM CA to validate a `wss://` Server, and whether to skip validation entirely (ADR-0012).
    tls_ca: Option<Vec<u8>>,
    tls_insecure: bool,
    /// Headers sent on connect from an accepted OpAMP connection-settings offer (ADR-0015); empty means
    /// use the configured bearer token instead.
    offered_headers: Vec<(String, String)>,
    /// Set when an accepted OpAMP connection-settings offer requires reconnecting to a new endpoint.
    reconnect_requested: bool,
    /// Connection settings to restore if a freshly accepted offer fails to connect (ADR-0015 revert).
    pending_revert: Option<ConnSnapshot>,
    /// Whether to revert to the last healthy config when a new one does not become healthy (ADR-0008).
    rollback_enabled: bool,
    /// How long to wait for a freshly applied config to report healthy before rolling back.
    rollback_health_timeout: Duration,
    /// The hash and failure reason of a config we rolled back from, if any. Kept so the same bad config
    /// is re-reported `FAILED` (and never re-applied) rather than restarting the agent again on a
    /// reconnect or a re-offer.
    rolled_back: Option<(Vec<u8>, String)>,
    /// Whether this agent accepts top-level package (binary) updates — i.e. declares `AcceptsPackages` /
    /// `ReportsPackageStatuses` and processes `PackagesAvailable` (ADR-0018).
    packages_enabled: bool,
    /// The top-level package currently installed (its version and Server-provided hash), or `None` if the
    /// Server has not installed one yet. Persisted so a restart resumes without re-installing.
    installed_package: Option<InstalledPackage>,
    /// The aggregate `all_packages_hash` the Server last offered, echoed back in `PackageStatuses` so the
    /// Server's hash comparison converges — whether the install succeeded or failed (ADR-0018).
    package_all_hash: Vec<u8>,
    /// The status of the top-level package to report, kept between messages (delta reporting) and rebuilt
    /// on restart from the installed package.
    package_status: Option<PackageStatus>,
}

impl<A: ManagedAgent> Supervisor<A> {
    pub fn new(config: Config, agent: A) -> Self {
        let start_time_unix_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let agent_description = agent_description(
            &config.service_name,
            config.agent_version.as_deref(),
            &config.instance_uid,
        );
        // Declare the package capabilities only when the agent can actually install a binary update — we
        // never claim a capability we do not honour (ADR-0018).
        let packages_enabled = agent.accepts_packages();
        let mut capabilities = CAPABILITIES | config.own_telemetry_capabilities;
        if packages_enabled {
            capabilities |= AgentCapabilities::AcceptsPackages as u64
                | AgentCapabilities::ReportsPackageStatuses as u64;
        }
        Self {
            server_url: config.server_url,
            instance_uid: config.instance_uid.to_vec(),
            uid_path: config.uid_path,
            storage_dir: config.storage_dir,
            service_name: config.service_name,
            agent_version: config.agent_version,
            agent_description,
            extra_attributes: config.extra_attributes,
            capabilities,
            agent,
            fallback: config.fallback,
            applied_hash: Vec::new(),
            applied_body: Vec::new(),
            sequence_num: 0,
            start_time_unix_nano,
            heartbeat: config.heartbeat,
            reconnect_delay: RECONNECT_BASE,
            restart_backoff: RESTART_BACKOFF_BASE,
            last_restart: None,
            auth_token: config.auth_token,
            tls_ca: config.tls_ca,
            tls_insecure: config.tls_insecure,
            offered_headers: Vec::new(),
            reconnect_requested: false,
            pending_revert: None,
            rollback_enabled: config.automatic_config_rollback,
            rollback_health_timeout: ROLLBACK_HEALTH_TIMEOUT,
            rolled_back: None,
            packages_enabled,
            installed_package: None,
            package_all_hash: Vec::new(),
            package_status: None,
        }
    }

    /// Runs until the process is stopped: bring the agent up (resuming the last applied config if one
    /// is on disk, else the fallback), then keep an OpAMP session open, reconnecting with backoff.
    pub async fn run(&mut self) {
        // Learn the agent-authoritative identity before the first Server report (a no-op for adapters,
        // e.g. a Foreign Agent, that have no discovery channel).
        self.agent.bootstrap().await;

        // Seed the agent with the last own-telemetry destination the Server offered, if any, so the
        // resumed/fallback config already reports to it (ADR-0010).
        let own_telemetry = self.load_own_telemetry();
        if !own_telemetry.is_empty() {
            self.agent.set_own_telemetry(own_telemetry);
        }

        // Resume on the OpAMP connection settings the Server last re-pointed us to (ADR-0015).
        self.load_conn_settings();

        // Resume the installed-package state, so a restart reports what it has without re-installing (ADR-0018).
        self.load_packages();

        if !self.resume_last_config().await {
            self.apply_fallback().await;
        }

        loop {
            let result = self.serve_once().await;

            // An accepted OpAMP connection-settings offer re-points the connection: reconnect at once to
            // the new endpoint, no backoff (ADR-0015).
            if std::mem::take(&mut self.reconnect_requested) {
                info!(server = %self.server_url, "reconnecting on newly offered OpAMP connection settings");
                continue;
            }

            if let Err(e) = &result {
                // If a freshly accepted offer failed to connect, revert to the previous settings so a
                // bad offer cannot strand the agent off the fleet (ADR-0015).
                if let Some(previous) = self.pending_revert.take() {
                    warn!(error = %e, server = %previous.server_url, "offered OpAMP connection settings failed; reverting");
                    self.restore_conn(previous);
                    self.persist_conn_settings();
                    continue;
                }
                warn!(error = %e, delay_secs = self.reconnect_delay.as_secs(), "OpAMP session ended; reconnecting");
            }
            tokio::time::sleep(jittered(self.reconnect_delay)).await;
            self.reconnect_delay = (self.reconnect_delay * 2).min(RECONNECT_MAX);
        }
    }

    /// Gracefully stops the managed agent. Called by the Host once its run loop has been cancelled on
    /// shutdown, so the agent terminates cleanly instead of being hard-killed on drop.
    pub async fn shutdown(&mut self) {
        self.agent.shutdown().await;
    }

    /// Brings the agent up on the last configuration that applied, if it is recorded in the storage dir,
    /// so a supervisor restart does not re-apply — and needlessly restart the agent for — a config it is
    /// already running. Returns whether it resumed.
    async fn resume_last_config(&mut self) -> bool {
        let (Ok(body), Ok(hash_hex)) = (
            std::fs::read(self.storage_dir.join("applied.config")),
            std::fs::read_to_string(self.storage_dir.join("applied.hash")),
        ) else {
            return false;
        };
        let Ok(hash) = hex::decode(hash_hex.trim()) else {
            return false;
        };
        let prepared = self.agent.prepare_config(body.clone());
        match self.agent.apply(&prepared).await {
            Ok(()) => {
                info!(hash = %short(&hash), "resumed agent on the last applied configuration");
                self.applied_hash = hash;
                self.applied_body = body;
                true
            }
            Err(e) => {
                error!(error = %e, "cannot resume agent on the last applied configuration");
                false
            }
        }
    }

    /// Applies the startup fallback configuration so the agent runs before the Server answers: the
    /// configured files (one or more) are merged in order — the same way a multi-file remote config is —
    /// then prepared and applied. A no-op when none is configured.
    async fn apply_fallback(&mut self) {
        if self.fallback.is_empty() {
            return;
        }
        let files: Vec<(String, Vec<u8>)> = std::mem::take(&mut self.fallback)
            .into_iter()
            .enumerate()
            .map(|(i, body)| (i.to_string(), body))
            .collect();
        let count = files.len();
        let Some(body) = self.agent.merge_config(&files) else {
            error!("the fallback configuration carried no usable files");
            return;
        };
        let prepared = self.agent.prepare_config(body.clone());
        match self.agent.apply(&prepared).await {
            Ok(()) => {
                self.applied_body = body;
                info!(files = count, "agent started on the fallback configuration");
            }
            Err(e) => error!(error = %e, "cannot start agent on the fallback configuration"),
        }
    }

    /// One OpAMP session: connect, send the full state, then apply whatever the Server sends until the
    /// connection closes or errors.
    async fn serve_once(&mut self) -> Result<(), String> {
        info!(server = %self.server_url, "connecting to OpAMP server");
        // Build the handshake request so a bearer token can be attached, and a TLS connector so a
        // `wss://` server is validated against the configured roots / CA (ADR-0012).
        let mut request = self
            .server_url
            .as_str()
            .into_client_request()
            .map_err(|e| format!("invalid server URL {}: {e}", self.server_url))?;
        // Headers from an accepted connection-settings offer take precedence over the configured bearer
        // token (ADR-0015); otherwise the configured token is sent as a Bearer header (ADR-0012).
        if !self.offered_headers.is_empty() {
            for (key, value) in &self.offered_headers {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(key.as_bytes()),
                    HeaderValue::from_str(value),
                ) {
                    request.headers_mut().insert(name, value);
                }
            }
        } else if let Some(token) = &self.auth_token {
            let value = format!("Bearer {token}")
                .parse()
                .map_err(|e| format!("invalid auth token: {e}"))?;
            request.headers_mut().insert(AUTHORIZATION, value);
        }
        let connector = crate::tls::connector(self.tls_ca.as_deref(), self.tls_insecure)?;
        let (mut ws, _) =
            tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector)
                .await
                .map_err(|e| format!("cannot connect to {}: {e}", self.server_url))?;
        info!("connected to OpAMP server");
        self.reconnect_delay = RECONNECT_BASE;
        // The connection came up: a freshly accepted offer is confirmed, so there is nothing to revert.
        self.pending_revert = None;

        let first = self.full_state_report();
        self.send(&mut ws, first).await?;

        let mut supervision = tokio::time::interval(SUPERVISION_INTERVAL);
        supervision.tick().await; // the first tick fires immediately; the agent was just started.

        let mut heartbeat = tokio::time::interval(self.heartbeat);
        heartbeat.tick().await; // skip the immediate first tick; the full-state report just went out.

        // A handle to the agent's change signal, awaited without borrowing the agent.
        let change_signal = self.agent.change_signal();

        loop {
            if heartbeat.period() != self.heartbeat {
                heartbeat = tokio::time::interval(self.heartbeat);
                heartbeat.tick().await;
            }
            tokio::select! {
                incoming = ws.next() => {
                    let Some(frame) = incoming else { return Ok(()); };
                    match frame.map_err(|e| format!("websocket error: {e}"))? {
                        Message::Binary(data) => {
                            let msg: ServerToAgent = frame::decode(&data)
                                .map_err(|e| format!("cannot decode ServerToAgent: {e}"))?;
                            self.handle(&mut ws, msg).await?;
                            // An accepted connection-settings offer ends this session so `run` reconnects
                            // to the newly offered endpoint (ADR-0015).
                            if self.reconnect_requested {
                                return Ok(());
                            }
                        }
                        Message::Close(_) => {
                            info!("server closed the connection");
                            return Ok(());
                        }
                        Message::Text(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                    }
                }
                _ = supervision.tick() => {
                    self.supervise(&mut ws).await?;
                }
                _ = heartbeat.tick() => {
                    let beat = self.next_report();
                    debug!(seq = beat.sequence_num, "sending heartbeat");
                    self.send(&mut ws, beat).await?;
                }
                _ = change_signal.changed() => {
                    let report = self.agent_status_report();
                    self.send(&mut ws, report).await?;
                }
            }
        }
    }

    /// A report carrying the agent's current health, effective config, description, and available
    /// components, sent when the agent reports a change.
    fn agent_status_report(&mut self) -> AgentToServer {
        let status = self.agent.status();
        let mut report = self.next_report();
        report.health = Some(status.health.clone());
        if status.effective_config.is_some() {
            report.effective_config = Some(self.effective_config());
        }
        if status.agent_description.is_some() {
            report.agent_description = Some(self.report_description());
        }
        if status.available_components.is_some() {
            report.available_components = status.available_components;
        }
        report
    }

    /// The agent description to report: the agent's own (agent-authoritative) when it has reported one
    /// — with `service.instance.id` overridden to the supervisor's stable UID — falling back to the
    /// synthesized description until the agent reports one. Always tagged with which supervisor manages
    /// it.
    fn report_description(&self) -> AgentDescription {
        let mut description = match self.agent.status().agent_description {
            Some(mut reported) => {
                set_string_attribute(
                    &mut reported.identifying_attributes,
                    "service.instance.id",
                    &crate::uid::format(&self.instance_uid),
                );
                reported
            }
            None => self.agent_description.clone(),
        };
        set_string_attribute(
            &mut description.non_identifying_attributes,
            SUPERVISOR_ATTRIBUTE,
            &format!("opamp-supervisor/{} (rust)", env!("CARGO_PKG_VERSION")),
        );
        for (key, value) in &self.extra_attributes {
            set_string_attribute(&mut description.non_identifying_attributes, key, value);
        }
        description
    }

    /// If the agent has crashed, reports it unhealthy and restarts it on the last good configuration,
    /// reporting healthy again once it is back. Reporting the crash rather than restarting silently is
    /// what makes a degraded agent visible.
    async fn supervise(&mut self, ws: &mut Ws) -> Result<(), String> {
        let Some(reason) = self.agent.supervise().await else {
            return Ok(());
        };

        // Distinguish an isolated exit from a crash loop: an agent that dies again soon after its last
        // restart is backed off (doubling) so it is not restarted in a tight loop; a stable one resets.
        let rapid = self
            .last_restart
            .is_some_and(|t| t.elapsed() < RESTART_STABLE);
        self.restart_backoff = escalate_backoff(self.restart_backoff, rapid);

        warn!(
            %reason,
            backoff_secs = self.restart_backoff.as_secs(),
            "agent exited unexpectedly; restarting on the last good configuration after backoff"
        );

        let mut down = self.next_report();
        down.health = Some(self.health(false, format!("agent exited unexpectedly: {reason}")));
        self.send(ws, down).await?;

        // The down report reaches the Server before we wait, so a crash-looping agent is visible even
        // while it is being backed off.
        tokio::time::sleep(self.restart_backoff).await;
        self.last_restart = Some(Instant::now());

        match self.agent.restart().await {
            Ok(()) => {
                let mut up = self.next_report();
                up.health = Some(self.health(true, String::new()));
                self.send(ws, up).await?;
                info!("agent restarted after an unexpected exit");
            }
            Err(e) => {
                error!(error = %e, "cannot restart the agent after it exited");
                let mut still_down = self.next_report();
                still_down.health = Some(self.health(false, e));
                self.send(ws, still_down).await?;
            }
        }
        Ok(())
    }

    /// Handles one `ServerToAgent`: adopt a Server-assigned instance UID, honour a full-state request,
    /// a restart command, and an offered configuration.
    async fn handle(&mut self, ws: &mut Ws, msg: ServerToAgent) -> Result<(), String> {
        let full_state_requested = msg.flags & ServerToAgentFlags::ReportFullState as u64 != 0;
        let restart_requested = is_restart_command(&msg);
        let heartbeat_interval = heartbeat_override(&msg);

        if let Some(id) = msg.agent_identification {
            if id.new_instance_uid.len() == 16 && id.new_instance_uid != self.instance_uid {
                self.adopt_instance_uid(id.new_instance_uid);
                let report = self.full_state_report();
                self.send(ws, report).await?;
            }
        }
        if let Some(interval) = heartbeat_interval {
            if interval != self.heartbeat {
                info!(
                    seconds = interval.as_secs(),
                    "server set the heartbeat interval"
                );
                self.heartbeat = interval;
            }
        }
        if full_state_requested {
            info!("server requested full state");
            let report = self.full_state_report();
            self.send(ws, report).await?;
        }
        if restart_requested {
            info!("server requested an agent restart");
            if let Err(e) = self.agent.restart().await {
                error!(error = %e, "the requested agent restart failed");
            }
            let mut report = self.next_report();
            report.health = Some(self.current_health());
            self.send(ws, report).await?;
        }
        // OpAMP connection re-point (ADR-0015): if the Server offers new settings for our own OpAMP
        // connection, adopt them; the session ends after this handler so `run` reconnects.
        if let Some(cs) = &msg.connection_settings {
            if self.apply_opamp_connection_offer(cs) {
                info!(server = %self.server_url, "server offered new OpAMP connection settings");
            }
        }
        // Own-telemetry offer (ADR-0010): update the agent's reporting destination. If it changed, the
        // running config must be re-applied to take effect — unless a remote config below already does.
        let telemetry_changed = if let Some(cs) = &msg.connection_settings {
            let offered = own_telemetry_from(cs, self.capabilities);
            if self.agent.set_own_telemetry(offered.clone()) {
                info!("server offered new own-telemetry connection settings");
                self.persist_own_telemetry(&offered);
                true
            } else {
                false
            }
        } else {
            false
        };

        let mut reconfigured = false;
        if let Some(remote) = msg.remote_config {
            reconfigured = self.apply_remote_config(ws, remote).await?;
        }
        if telemetry_changed && !reconfigured {
            self.reapply_running_config(ws).await?;
        }

        // Package offer (ADR-0018): download, hash-verify, swap the binary, and report the outcome. The
        // Server sends this only when its aggregate package hash differs from what we last reported.
        if let Some(available) = msg.packages_available {
            if self.packages_enabled {
                self.apply_packages(ws, available).await?;
            }
        }
        Ok(())
    }

    /// Re-applies the configuration currently running so a change that is not carried by a new remote
    /// config — an own-telemetry offer (ADR-0010) — takes effect. A no-op until something is running.
    async fn reapply_running_config(&mut self, ws: &mut Ws) -> Result<(), String> {
        if self.applied_body.is_empty() {
            return Ok(());
        }
        let prepared = self.agent.prepare_config(self.applied_body.clone());
        let mut report = self.next_report();
        match self.agent.apply(&prepared).await {
            Ok(()) => {
                report.effective_config = Some(self.effective_config());
                report.health = Some(self.current_health());
                info!("re-applied the running configuration for updated own-telemetry settings");
            }
            Err(e) => {
                error!(error = %e, "cannot re-apply the running configuration for own telemetry");
                report.health = Some(self.health(false, "agent failed to re-apply config".into()));
            }
        }
        self.send(ws, report).await
    }

    /// Adopts an instance UID the Server assigned: persisted so a restart keeps the assigned identity,
    /// and the agent description is rebuilt because `service.instance.id` embeds the UID.
    fn adopt_instance_uid(&mut self, new_uid: Vec<u8>) {
        info!(uid = %crate::uid::format(&new_uid), "adopting the Server-assigned instance UID");
        if let Err(e) = std::fs::write(&self.uid_path, &new_uid) {
            warn!(error = %e, "cannot persist the Server-assigned instance UID");
        }
        self.instance_uid = new_uid;
        self.agent_description = agent_description(
            &self.service_name,
            self.agent_version.as_deref(),
            &self.instance_uid,
        );
    }

    /// Applies a remote configuration and reports the outcome. Skips the work when the offered hash is
    /// already the applied one, so an unchanged re-offer never restarts the agent. Returns whether the
    /// agent was (re)configured — so the caller knows the running config already carries the current
    /// own-telemetry settings and need not re-apply for them (ADR-0010).
    async fn apply_remote_config(
        &mut self,
        ws: &mut Ws,
        remote: AgentRemoteConfig,
    ) -> Result<bool, String> {
        let hash = remote.config_hash;
        if hash == self.applied_hash {
            return Ok(false);
        }
        // A config we already rolled back from is not applied again — that would just restart the agent
        // onto the same broken config. Re-report it FAILED so the Server knows its latest config did not
        // take, and do nothing else (ADR-0008 rollback).
        if let Some((failed_hash, error)) = &self.rolled_back {
            if &hash == failed_hash {
                let status = failed_status(hash, error.clone());
                let mut report = self.next_report();
                report.remote_config_status = Some(status);
                self.send(ws, report).await?;
                return Ok(false);
            }
        }
        let files = sorted_config_files(remote.config);
        let Some(body) = self.agent.merge_config(&files) else {
            warn!("remote config carried no usable config files; ignoring");
            return Ok(false);
        };
        let prepared = self.agent.prepare_config(body.clone());

        // Remember the config currently running, to roll back to if the new one does not become healthy.
        let last_good_body = self.applied_body.clone();
        let last_good_hash = self.applied_hash.clone();

        info!(hash = %short(&hash), "applying remote configuration");
        let mut applying = self.next_report();
        applying.remote_config_status = Some(RemoteConfigStatus {
            last_remote_config_hash: hash.clone(),
            status: RemoteConfigStatuses::Applying as i32,
            error_message: String::new(),
        });
        self.send(ws, applying).await?;

        let mut report = self.next_report();
        match self.agent.apply(&prepared).await {
            Ok(()) => {
                // With rollback enabled and a good config to fall back to, confirm the agent becomes
                // healthy on the new config before committing; if it does not, revert (ADR-0008).
                if self.rollback_enabled
                    && !last_good_hash.is_empty()
                    && !self.confirm_health().await
                {
                    self.rollback(ws, hash, last_good_body, last_good_hash)
                        .await?;
                    return Ok(true);
                }
                self.commit_applied(&body, &hash);
                report.remote_config_status = Some(applied_status(hash));
                report.effective_config = Some(self.effective_config());
                report.health = Some(self.current_health());
                info!("remote configuration APPLIED");
            }
            Err(e) => {
                error!(error = %e, "remote configuration FAILED");
                report.remote_config_status = Some(failed_status(hash, e));
                report.health = Some(self.health(false, "agent failed to apply config".into()));
            }
        }
        self.send(ws, report).await?;
        Ok(true)
    }

    /// Waits for the agent to report itself healthy on the config just applied, up to
    /// [`Supervisor::rollback_health_timeout`]. Returns `true` if it did, `false` if it timed out (never
    /// reported, or stayed unhealthy). Reads the agent's *own* reported health — not the liveness
    /// fallback — and only after the fresh collector has reported, because the adapter clears the
    /// previous process's health on apply.
    async fn confirm_health(&mut self) -> bool {
        let signal = self.agent.change_signal();
        let deadline = tokio::time::Instant::now() + self.rollback_health_timeout;
        loop {
            if self
                .agent
                .reported_health()
                .is_some_and(|health| health.healthy)
            {
                return true;
            }
            tokio::select! {
                _ = signal.changed() => {}
                _ = tokio::time::sleep_until(deadline) => return false,
            }
        }
    }

    /// Commits a successfully applied config: records it as the running config, persists it, and clears
    /// any earlier rollback (a new good config supersedes the failure).
    fn commit_applied(&mut self, body: &[u8], hash: &[u8]) {
        self.applied_hash = hash.to_vec();
        self.applied_body = body.to_vec();
        self.persist_applied(body, hash);
        self.rolled_back = None;
    }

    /// Reverts to the last healthy config after a new one failed to become healthy: re-applies the good
    /// config, restores it as the running config, remembers the failed hash so it is not retried, and
    /// reports the new config `FAILED` with the agent's health error (ADR-0008 rollback).
    async fn rollback(
        &mut self,
        ws: &mut Ws,
        failed_hash: Vec<u8>,
        good_body: Vec<u8>,
        good_hash: Vec<u8>,
    ) -> Result<(), String> {
        let error = format!(
            "configuration rolled back: the collector did not become healthy ({})",
            self.current_health().last_error
        );
        warn!(hash = %short(&failed_hash), "remote configuration did not become healthy; rolling back to the last good configuration");

        let prepared = self.agent.prepare_config(good_body.clone());
        let apply_result = self.agent.apply(&prepared).await;
        self.applied_body = good_body;
        self.applied_hash = good_hash;
        self.persist_applied(&self.applied_body.clone(), &self.applied_hash.clone());
        self.rolled_back = Some((failed_hash.clone(), error.clone()));

        let mut report = self.next_report();
        report.remote_config_status = Some(failed_status(failed_hash, error));
        match apply_result {
            Ok(()) => {
                report.effective_config = Some(self.effective_config());
                report.health = Some(self.current_health());
            }
            Err(e) => {
                error!(error = %e, "cannot roll back to the last good configuration");
                report.health = Some(self.health(false, format!("rollback failed: {e}")));
            }
        }
        self.send(ws, report).await
    }

    /// Processes a `PackagesAvailable` offer for the single top-level package (the agent's binary,
    /// ADR-0018): download, verify the content hash, install and restart onto it, confirm health (rolling
    /// back a binary that does not become healthy), and report `PackageStatuses` at each step. The
    /// aggregate `all_packages_hash` is echoed back whatever the outcome, so the Server's comparison
    /// converges and a failed install is not re-offered in a loop.
    async fn apply_packages(
        &mut self,
        ws: &mut Ws,
        available: PackagesAvailable,
    ) -> Result<(), String> {
        self.package_all_hash = available.all_packages_hash.clone();

        // First increment: exactly one top-level package; addons are out of scope (ADR-0018).
        let Some((name, pkg)) = take_top_level(available.packages) else {
            self.persist_packages();
            let report = self.package_report();
            return self.send(ws, report).await;
        };
        let version = pkg.version.clone();
        let offered_hash = pkg.hash.clone();

        // De-duplicate: an offer whose hash matches the installed package needs no work (ADR-0018) — this
        // is what stops an unchanged re-offer from restarting the collector.
        if self
            .installed_package
            .as_ref()
            .is_some_and(|p| p.hash == offered_hash)
        {
            self.package_status = Some(self.package_status_for(
                &name,
                &version,
                &offered_hash,
                PackageStatusEnum::Installed,
                String::new(),
            ));
            self.persist_packages();
            let report = self.package_report();
            return self.send(ws, report).await;
        }

        let Some(file) = pkg.file else {
            return self
                .fail_package(
                    ws,
                    &name,
                    &version,
                    &offered_hash,
                    "the package carried no downloadable file".to_string(),
                )
                .await;
        };

        info!(package = %name, %version, "downloading offered package");
        self.report_package_step(
            ws,
            &name,
            &version,
            &offered_hash,
            PackageStatusEnum::Downloading,
        )
        .await?;

        let headers = header_pairs(file.headers.as_ref());
        let bytes = match crate::download::get(
            &file.download_url,
            &headers,
            self.tls_ca.as_deref(),
            self.tls_insecure,
        )
        .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                return self
                    .fail_package(
                        ws,
                        &name,
                        &version,
                        &offered_hash,
                        format!("download failed: {e}"),
                    )
                    .await;
            }
        };

        // Verify the content hash before the bytes are ever made executable (the spec's integrity check,
        // ADR-0018). A mismatch aborts without touching the running collector.
        if !file.content_hash.is_empty()
            && Sha256::digest(&bytes).as_slice() != file.content_hash.as_slice()
        {
            return self
                .fail_package(
                    ws,
                    &name,
                    &version,
                    &offered_hash,
                    "downloaded package failed content-hash verification".to_string(),
                )
                .await;
        }

        info!(package = %name, %version, "installing offered package");
        self.report_package_step(
            ws,
            &name,
            &version,
            &offered_hash,
            PackageStatusEnum::Installing,
        )
        .await?;

        let had_previous = self.installed_package.is_some();
        if let Err(e) = self.agent.install_package(&bytes).await {
            return self
                .fail_package(
                    ws,
                    &name,
                    &version,
                    &offered_hash,
                    format!("install failed: {e}"),
                )
                .await;
        }

        // Confirm the new binary becomes healthy before committing, when rollback is enabled and there is
        // a previous binary to fall back to — mirroring config rollback (ADR-0008/0018).
        if self.rollback_enabled && had_previous && !self.confirm_health().await {
            let error = format!(
                "package rolled back: the collector did not become healthy ({})",
                self.current_health().last_error
            );
            warn!(package = %name, "installed package did not become healthy; rolling back to the previous binary");
            if let Err(e) = self.agent.rollback_package().await {
                error!(error = %e, "cannot roll back to the previous collector binary");
            }
            return self
                .fail_package(ws, &name, &version, &offered_hash, error)
                .await;
        }

        self.installed_package = Some(InstalledPackage {
            version: version.clone(),
            hash: offered_hash.clone(),
        });
        self.package_status = Some(self.package_status_for(
            &name,
            &version,
            &offered_hash,
            PackageStatusEnum::Installed,
            String::new(),
        ));
        self.persist_packages();
        info!(package = %name, %version, "package INSTALLED");
        let mut report = self.package_report();
        report.health = Some(self.current_health());
        self.send(ws, report).await
    }

    /// Records and reports an in-progress package step (Downloading / Installing).
    async fn report_package_step(
        &mut self,
        ws: &mut Ws,
        name: &str,
        version: &str,
        offered_hash: &[u8],
        status: PackageStatusEnum,
    ) -> Result<(), String> {
        self.package_status =
            Some(self.package_status_for(name, version, offered_hash, status, String::new()));
        let report = self.package_report();
        self.send(ws, report).await
    }

    /// Records a failed package install and reports it (`InstallFailed` with the reason). The failed
    /// offer's hash is *not* installed, and because we still echo the Server's `all_packages_hash` the
    /// Server does not loop re-offering the same broken binary (ADR-0018).
    async fn fail_package(
        &mut self,
        ws: &mut Ws,
        name: &str,
        version: &str,
        offered_hash: &[u8],
        error: String,
    ) -> Result<(), String> {
        error!(package = %name, %version, %error, "package InstallFailed");
        self.package_status = Some(self.package_status_for(
            name,
            version,
            offered_hash,
            PackageStatusEnum::InstallFailed,
            error,
        ));
        self.persist_packages();
        let mut report = self.package_report();
        report.health = Some(self.current_health());
        self.send(ws, report).await
    }

    /// Builds a `PackageStatus` for the top-level package: what the agent currently has (the installed
    /// package, or empty), what the Server offered, the status, and any error.
    fn package_status_for(
        &self,
        name: &str,
        offered_version: &str,
        offered_hash: &[u8],
        status: PackageStatusEnum,
        error: String,
    ) -> PackageStatus {
        PackageStatus {
            name: name.to_string(),
            agent_has_version: self
                .installed_package
                .as_ref()
                .map(|p| p.version.clone())
                .unwrap_or_default(),
            agent_has_hash: self
                .installed_package
                .as_ref()
                .map(|p| p.hash.clone())
                .unwrap_or_default(),
            server_offered_version: offered_version.to_string(),
            server_offered_hash: offered_hash.to_vec(),
            status: status as i32,
            error_message: error,
            ..Default::default()
        }
    }

    /// A report carrying the current `PackageStatuses`, for the Downloading/Installing/Installed steps.
    fn package_report(&mut self) -> AgentToServer {
        let mut report = self.next_report();
        report.package_statuses = self.package_statuses();
        report
    }

    /// The `PackageStatuses` to report — the top-level package's status and the aggregate hash the Server
    /// last offered — or `None` for an agent that does not do packages (ADR-0018).
    fn package_statuses(&self) -> Option<PackageStatuses> {
        if !self.packages_enabled {
            return None;
        }
        let mut packages = HashMap::new();
        if let Some(status) = &self.package_status {
            packages.insert(status.name.clone(), status.clone());
        }
        Some(PackageStatuses {
            packages,
            server_provided_all_packages_hash: self.package_all_hash.clone(),
            error_message: String::new(),
        })
    }

    /// Persists the installed-package state (its name, version, hash, and the last aggregate hash) so a
    /// restart reports what it has and de-duplicates the next offer without re-installing (ADR-0018).
    fn persist_packages(&self) {
        if !self.packages_enabled {
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&self.storage_dir) {
            warn!(error = %e, "cannot create the supervisor storage directory");
            return;
        }
        let persisted = PersistedPackages {
            name: self
                .package_status
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_default(),
            all_hash: hex::encode(&self.package_all_hash),
            installed: self.installed_package.clone(),
        };
        match serde_yaml::to_string(&persisted) {
            Ok(yaml) => {
                if let Err(e) = std::fs::write(self.storage_dir.join("packages.yaml"), yaml) {
                    warn!(error = %e, "cannot persist the package state");
                }
            }
            Err(e) => warn!(error = %e, "cannot serialize the package state"),
        }
    }

    /// Loads the persisted installed-package state, so a restart resumes reporting it (ADR-0018).
    fn load_packages(&mut self) {
        if !self.packages_enabled {
            return;
        }
        let Ok(bytes) = std::fs::read(self.storage_dir.join("packages.yaml")) else {
            return;
        };
        let Ok(persisted) = serde_yaml::from_slice::<PersistedPackages>(&bytes) else {
            return;
        };
        self.package_all_hash = hex::decode(persisted.all_hash.trim()).unwrap_or_default();
        self.installed_package = persisted.installed;
        // Rebuild the installed package's status so it is reported before any new offer arrives.
        if let (Some(installed), false) = (&self.installed_package, persisted.name.is_empty()) {
            self.package_status = Some(PackageStatus {
                name: persisted.name,
                agent_has_version: installed.version.clone(),
                agent_has_hash: installed.hash.clone(),
                server_offered_version: installed.version.clone(),
                server_offered_hash: installed.hash.clone(),
                status: PackageStatusEnum::Installed as i32,
                ..Default::default()
            });
        }
        if self.installed_package.is_some() {
            info!("resumed the installed-package state");
        }
    }

    /// A report carrying the Agent's full state — identity, health, and the configuration it holds.
    fn full_state_report(&mut self) -> AgentToServer {
        let mut report = self.next_report();
        report.agent_description = Some(self.report_description());
        report.health = Some(self.current_health());
        report.available_components = self.agent.status().available_components;
        if let Some((failed_hash, error)) = &self.rolled_back {
            // The Server's latest config failed and was rolled back; report that (not the good config's
            // hash) so the Server does not keep re-sending it, and echo the good config as effective.
            report.remote_config_status = Some(failed_status(failed_hash.clone(), error.clone()));
            report.effective_config = Some(self.effective_config());
        } else if !self.applied_hash.is_empty() {
            report.remote_config_status = Some(applied_status(self.applied_hash.clone()));
            report.effective_config = Some(self.effective_config());
        }
        report.package_statuses = self.package_statuses();
        report
    }

    /// The base of every message: identity, the next sequence number, and the mandatory capabilities.
    fn next_report(&mut self) -> AgentToServer {
        self.sequence_num += 1;
        AgentToServer {
            instance_uid: self.instance_uid.clone(),
            sequence_num: self.sequence_num,
            capabilities: self.capabilities,
            ..Default::default()
        }
    }

    /// Persists the applied raw config and its hash in the storage dir, so a restart resumes on it.
    fn persist_applied(&self, body: &[u8], hash: &[u8]) {
        if let Err(e) = std::fs::create_dir_all(&self.storage_dir) {
            warn!(error = %e, "cannot create the supervisor storage directory");
            return;
        }
        if let Err(e) = std::fs::write(self.storage_dir.join("applied.config"), body) {
            warn!(error = %e, "cannot persist the applied config");
        }
        if let Err(e) = std::fs::write(self.storage_dir.join("applied.hash"), hex::encode(hash)) {
            warn!(error = %e, "cannot persist the applied config hash");
        }
    }

    /// The file the last Server-offered own-telemetry settings are persisted to (ADR-0010).
    fn own_telemetry_path(&self) -> PathBuf {
        self.storage_dir.join("own_telemetry.yaml")
    }

    /// Persists the last own-telemetry offer, so a supervisor restart resumes reporting to the same
    /// destination without waiting for the Server to re-offer it (ADR-0010).
    fn persist_own_telemetry(&self, settings: &OwnTelemetry) {
        if let Err(e) = std::fs::create_dir_all(&self.storage_dir) {
            warn!(error = %e, "cannot create the supervisor storage directory");
            return;
        }
        match serde_yaml::to_string(settings) {
            Ok(yaml) => {
                if let Err(e) = std::fs::write(self.own_telemetry_path(), yaml) {
                    warn!(error = %e, "cannot persist own-telemetry settings");
                }
            }
            Err(e) => warn!(error = %e, "cannot serialize own-telemetry settings"),
        }
    }

    /// Loads the last persisted own-telemetry offer, or an empty one if none is recorded (ADR-0010).
    fn load_own_telemetry(&self) -> OwnTelemetry {
        std::fs::read(self.own_telemetry_path())
            .ok()
            .and_then(|bytes| serde_yaml::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Applies an OpAMP connection-settings offer (ADR-0015). Re-points the connection when the offer
    /// carries a non-empty endpoint and the effective settings (endpoint, headers, or TLS) differ from
    /// the current ones: it snapshots the current settings for revert, adopts the offered ones, persists
    /// them, and requests a reconnect. A heartbeat-only offer (empty endpoint) or an unchanged offer is
    /// ignored. Returns whether it re-pointed.
    fn apply_opamp_connection_offer(&mut self, cs: &ConnectionSettingsOffers) -> bool {
        if self.capabilities & AgentCapabilities::AcceptsOpAmpConnectionSettings as u64 == 0 {
            return false;
        }
        let Some(opamp) = &cs.opamp else {
            return false;
        };
        if opamp.destination_endpoint.is_empty() {
            return false; // a heartbeat-only offer carries no endpoint; not a re-point
        }
        let new_headers: Vec<(String, String)> = opamp
            .headers
            .as_ref()
            .map(|h| {
                h.headers
                    .iter()
                    .map(|kv| (kv.key.clone(), kv.value.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let (new_ca, new_insecure) = match &opamp.tls {
            Some(tls) => (
                (!tls.ca_pem_contents.is_empty()).then(|| tls.ca_pem_contents.clone().into_bytes()),
                tls.insecure_skip_verify,
            ),
            None => (self.tls_ca.clone(), self.tls_insecure),
        };
        if opamp.destination_endpoint == self.server_url
            && new_headers == self.offered_headers
            && new_ca == self.tls_ca
            && new_insecure == self.tls_insecure
        {
            return false; // nothing actually changed
        }

        self.pending_revert = Some(self.conn_snapshot());
        self.server_url = opamp.destination_endpoint.clone();
        self.offered_headers = new_headers;
        self.tls_ca = new_ca;
        self.tls_insecure = new_insecure;
        self.persist_conn_settings();
        self.reconnect_requested = true;
        true
    }

    /// A snapshot of the current OpAMP connection settings, for reverting a failed offer (ADR-0015).
    fn conn_snapshot(&self) -> ConnSnapshot {
        ConnSnapshot {
            server_url: self.server_url.clone(),
            auth_token: self.auth_token.clone(),
            offered_headers: self.offered_headers.clone(),
            tls_ca: self.tls_ca.clone(),
            tls_insecure: self.tls_insecure,
        }
    }

    /// Restores a connection-settings snapshot after a failed offer (ADR-0015).
    fn restore_conn(&mut self, snapshot: ConnSnapshot) {
        self.server_url = snapshot.server_url;
        self.auth_token = snapshot.auth_token;
        self.offered_headers = snapshot.offered_headers;
        self.tls_ca = snapshot.tls_ca;
        self.tls_insecure = snapshot.tls_insecure;
    }

    /// The file the accepted OpAMP connection settings are persisted to (ADR-0015).
    fn conn_settings_path(&self) -> PathBuf {
        self.storage_dir.join("opamp_connection.yaml")
    }

    /// Persists the accepted OpAMP connection settings so a restart resumes on the re-pointed endpoint.
    fn persist_conn_settings(&self) {
        if let Err(e) = std::fs::create_dir_all(&self.storage_dir) {
            warn!(error = %e, "cannot create the supervisor storage directory");
            return;
        }
        let persisted = PersistedConnSettings {
            server_url: self.server_url.clone(),
            headers: self.offered_headers.clone(),
            ca_pem: self
                .tls_ca
                .as_ref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
            insecure: self.tls_insecure,
        };
        match serde_yaml::to_string(&persisted) {
            Ok(yaml) => {
                if let Err(e) = std::fs::write(self.conn_settings_path(), yaml) {
                    warn!(error = %e, "cannot persist the OpAMP connection settings");
                }
            }
            Err(e) => warn!(error = %e, "cannot serialize the OpAMP connection settings"),
        }
    }

    /// Loads the last accepted OpAMP connection settings, if any, so the Supervisor resumes on the
    /// re-pointed endpoint rather than the configured one (ADR-0015).
    fn load_conn_settings(&mut self) {
        let Ok(bytes) = std::fs::read(self.conn_settings_path()) else {
            return;
        };
        let Ok(persisted) = serde_yaml::from_slice::<PersistedConnSettings>(&bytes) else {
            return;
        };
        if persisted.server_url.is_empty() {
            return;
        }
        self.server_url = persisted.server_url;
        self.offered_headers = persisted.headers;
        self.tls_ca = persisted.ca_pem.map(String::into_bytes);
        self.tls_insecure = persisted.insecure;
        info!(server = %self.server_url, "resumed on the last accepted OpAMP connection settings");
    }

    /// The effective configuration to report. Prefers what the agent reports (its *actual* running
    /// config), falling back to echoing the bytes we applied.
    fn effective_config(&self) -> EffectiveConfig {
        if let Some(effective) = self.agent.status().effective_config {
            return effective;
        }
        EffectiveConfig {
            config_map: Some(AgentConfigMap {
                config_map: [(
                    MAIN_CONFIG_KEY.to_string(),
                    AgentConfigFile {
                        body: self.applied_body.clone(),
                        content_type: CONFIG_CONTENT_TYPE.to_string(),
                    },
                )]
                .into_iter()
                .collect(),
            }),
        }
    }

    /// The health to report — whatever the agent currently reports.
    fn current_health(&self) -> ComponentHealth {
        self.agent.status().health
    }

    /// A liveness-shaped health for the domain's own apply-failed / crash reports.
    fn health(&self, healthy: bool, last_error: String) -> ComponentHealth {
        liveness_health(healthy, last_error, self.start_time_unix_nano)
    }

    async fn send(&mut self, ws: &mut Ws, msg: AgentToServer) -> Result<(), String> {
        ws.send(Message::Binary(frame::encode(&msg).into()))
            .await
            .map_err(|e| format!("cannot send report: {e}"))
    }
}

/// A reconnect wait randomized around `delay` by [`RECONNECT_JITTER`], using the process clock as the
/// entropy source (no extra dependency) — enough to de-synchronize a fleet, not a cryptographic need.
fn jittered(delay: Duration) -> Duration {
    apply_jitter(delay, clock_fraction())
}

/// The pure jitter: scales `delay` by a factor uniformly in `[1 - f, 1 + f]` as `fraction` runs `0..1`
/// (with `f` = [`RECONNECT_JITTER`]). Split out from the clock so the bounds are unit-testable.
fn apply_jitter(delay: Duration, fraction: f64) -> Duration {
    let factor = 1.0 - RECONNECT_JITTER + fraction.clamp(0.0, 1.0) * (2.0 * RECONNECT_JITTER);
    delay.mul_f64(factor)
}

/// A pseudo-random fraction in `[0, 1)` from the sub-second part of the wall clock — sufficient entropy
/// to spread reconnect waits across a fleet.
fn clock_fraction() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}

/// The next crash-restart backoff: doubled (capped at [`RESTART_BACKOFF_MAX`]) when the agent is
/// crash-looping, reset to [`RESTART_BACKOFF_BASE`] when it has been stable.
fn escalate_backoff(current: Duration, rapid: bool) -> Duration {
    if rapid {
        (current * 2).min(RESTART_BACKOFF_MAX)
    } else {
        RESTART_BACKOFF_BASE
    }
}

/// An `APPLIED` remote-config status for a hash — the config took effect.
fn applied_status(hash: Vec<u8>) -> RemoteConfigStatus {
    RemoteConfigStatus {
        last_remote_config_hash: hash,
        status: RemoteConfigStatuses::Applied as i32,
        error_message: String::new(),
    }
}

/// A `FAILED` remote-config status for a hash, carrying the error the Server should see.
fn failed_status(hash: Vec<u8>, error: String) -> RemoteConfigStatus {
    RemoteConfigStatus {
        last_remote_config_hash: hash,
        status: RemoteConfigStatuses::Failed as i32,
        error_message: error,
    }
}

/// Whether the server asked the agent to restart (`ServerToAgentCommand`, the AcceptsRestartCommand
/// capability).
fn is_restart_command(msg: &ServerToAgent) -> bool {
    msg.command
        .as_ref()
        .is_some_and(|c| c.r#type == CommandType::Restart as i32)
}

/// The own-telemetry destinations from a `ConnectionSettingsOffers`, honouring only the signals this
/// agent declares (ADR-0010): a disabled or un-offered signal is left unset.
fn own_telemetry_from(cs: &ConnectionSettingsOffers, capabilities: u64) -> OwnTelemetry {
    let for_signal = |offered: &Option<TelemetryConnectionSettings>, cap: AgentCapabilities| {
        (capabilities & cap as u64 != 0)
            .then(|| destination_from(offered.as_ref()))
            .flatten()
    };
    OwnTelemetry {
        metrics: for_signal(&cs.own_metrics, AgentCapabilities::ReportsOwnMetrics),
        logs: for_signal(&cs.own_logs, AgentCapabilities::ReportsOwnLogs),
        traces: for_signal(&cs.own_traces, AgentCapabilities::ReportsOwnTraces),
    }
}

/// A [`TelemetryDestination`] from an offered `TelemetryConnectionSettings`, or `None` when no endpoint
/// is offered (an empty endpoint means "no destination").
fn destination_from(offered: Option<&TelemetryConnectionSettings>) -> Option<TelemetryDestination> {
    let offered = offered?;
    if offered.destination_endpoint.is_empty() {
        return None;
    }
    let headers = offered
        .headers
        .as_ref()
        .map(|h| {
            h.headers
                .iter()
                .map(|kv| (kv.key.clone(), kv.value.clone()))
                .collect()
        })
        .unwrap_or_default();
    Some(TelemetryDestination {
        endpoint: offered.destination_endpoint.clone(),
        headers,
    })
}

/// A server-dictated heartbeat interval, if the server offered one greater than zero.
fn heartbeat_override(msg: &ServerToAgent) -> Option<Duration> {
    let seconds = msg
        .connection_settings
        .as_ref()?
        .opamp
        .as_ref()?
        .heartbeat_interval_seconds;
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

/// Picks the single top-level package to install from an offer's package map (ADR-0018): the entry of
/// `PackageType::TopLevel`, chosen deterministically by name so a repeated offer resolves the same way.
/// Addons are ignored in this increment. `None` when no top-level package is offered.
fn take_top_level(
    mut packages: HashMap<String, PackageAvailable>,
) -> Option<(String, PackageAvailable)> {
    let mut names: Vec<String> = packages.keys().cloned().collect();
    names.sort();
    let name = names
        .into_iter()
        .find(|name| packages[name].r#type == PackageType::TopLevel as i32)?;
    let pkg = packages.remove(&name)?;
    Some((name, pkg))
}

/// The `(key, value)` header pairs from an offered `DownloadableFile.headers` (e.g. the `Authorization`
/// the Server attached), for the download request (ADR-0018).
fn header_pairs(headers: Option<&Headers>) -> Vec<(String, String)> {
    headers
        .map(|h| {
            h.headers
                .iter()
                .map(|kv| (kv.key.clone(), kv.value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The offered config map as an ordered list of `(key, body)`, sorted by key — the order in which the
/// files are merged into the applied config, matching the Go supervisor. Empty when nothing is offered.
fn sorted_config_files(config: Option<AgentConfigMap>) -> Vec<(String, Vec<u8>)> {
    let Some(map) = config else {
        return Vec::new();
    };
    let mut files: Vec<(String, Vec<u8>)> = map
        .config_map
        .into_iter()
        .map(|(key, file)| (key, file.body))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// The Agent's self-description: the identifying attributes an OpenTelemetry Agent SHOULD report, so
/// the fleet UI can name it.
fn agent_description(
    service_name: &str,
    agent_version: Option<&str>,
    instance_uid: &[u8],
) -> AgentDescription {
    let mut identifying = vec![
        key_value("service.name", service_name),
        key_value("service.instance.id", &crate::uid::format(instance_uid)),
    ];
    if let Some(version) = agent_version {
        identifying.push(key_value("service.version", version));
    }
    AgentDescription {
        identifying_attributes: identifying,
        non_identifying_attributes: host_attributes(),
    }
}

/// Attributes describing where the Agent runs.
fn host_attributes() -> Vec<KeyValue> {
    let mut attrs = vec![
        key_value("os.type", std::env::consts::OS),
        key_value("host.arch", std::env::consts::ARCH),
    ];
    if let Some(name) = hostname() {
        attrs.push(key_value("host.name", &name));
    }
    attrs
}

fn hostname() -> Option<String> {
    if let Ok(name) = std::env::var("HOSTNAME") {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn key_value(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
    }
}

/// Sets a string-valued attribute, replacing any existing entry for `key`.
fn set_string_attribute(attributes: &mut Vec<KeyValue>, key: &str, value: &str) {
    let entry = key_value(key, value);
    match attributes.iter_mut().find(|kv| kv.key == key) {
        Some(existing) => *existing = entry,
        None => attributes.push(entry),
    }
}

/// A short, human-readable form of a hash for a log line.
fn short(bytes: &[u8]) -> String {
    hex::encode(&bytes[..bytes.len().min(6)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentStatus, ChangeSignal};

    /// A test double for [`ManagedAgent`]: returns a fixed status and records applied configs.
    #[derive(Default)]
    struct FakeAgent {
        status: AgentStatus,
        applied: Vec<Vec<u8>>,
        /// The health the agent reports about itself, for exercising the rollback health check.
        reported_health: Option<ComponentHealth>,
        /// Whether this fake accepts package updates (ADR-0018).
        accepts_packages: bool,
    }

    impl ManagedAgent for FakeAgent {
        async fn apply(&mut self, config: &[u8]) -> Result<(), String> {
            self.applied.push(config.to_vec());
            Ok(())
        }
        async fn restart(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn status(&self) -> AgentStatus {
            self.status.clone()
        }
        fn reported_health(&self) -> Option<ComponentHealth> {
            self.reported_health.clone()
        }
        fn change_signal(&self) -> ChangeSignal {
            ChangeSignal::never()
        }
        async fn supervise(&mut self) -> Option<String> {
            None
        }
        fn accepts_packages(&self) -> bool {
            self.accepts_packages
        }
    }

    fn healthy() -> ComponentHealth {
        ComponentHealth {
            healthy: true,
            status: "Running".to_string(),
            ..Default::default()
        }
    }

    fn unhealthy(error: &str) -> ComponentHealth {
        ComponentHealth {
            healthy: false,
            status: "Errored".to_string(),
            last_error: error.to_string(),
            ..Default::default()
        }
    }

    fn supervisor_with(agent: FakeAgent) -> Supervisor<FakeAgent> {
        Supervisor::new(
            Config {
                server_url: "ws://localhost/v1/opamp".to_string(),
                instance_uid: [0u8; 16],
                uid_path: std::path::PathBuf::from("/tmp/opamp-sup-test-uid"),
                storage_dir: std::path::PathBuf::from("/tmp/opamp-sup-test-store"),
                service_name: "io.opentelemetry.collector".to_string(),
                agent_version: None,
                fallback: Vec::new(),
                heartbeat: Duration::from_secs(30),
                extra_attributes: Vec::new(),
                own_telemetry_capabilities: 0,
                automatic_config_rollback: false,
                auth_token: None,
                tls_ca: None,
                tls_insecure: false,
            },
            agent,
        )
    }

    fn supervisor_with_attributes(attrs: Vec<(String, String)>) -> Supervisor<FakeAgent> {
        let mut sup = supervisor_with(FakeAgent::default());
        sup.extra_attributes = attrs;
        sup
    }

    fn effective_body(ec: EffectiveConfig) -> Vec<u8> {
        ec.config_map.unwrap().config_map[MAIN_CONFIG_KEY]
            .body
            .clone()
    }

    fn config_file(body: &[u8]) -> AgentConfigFile {
        AgentConfigFile {
            body: body.to_vec(),
            content_type: CONFIG_CONTENT_TYPE.to_string(),
        }
    }

    #[test]
    fn sorted_config_files_orders_by_key() {
        let map = AgentConfigMap {
            config_map: [
                ("zeta.yaml".to_string(), config_file(b"z")),
                (MAIN_CONFIG_KEY.to_string(), config_file(b"main")),
                ("alpha.yaml".to_string(), config_file(b"a")),
            ]
            .into_iter()
            .collect(),
        };
        let keys: Vec<String> = sorted_config_files(Some(map))
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, ["", "alpha.yaml", "zeta.yaml"]);
    }

    #[test]
    fn sorted_config_files_is_empty_when_absent() {
        assert!(sorted_config_files(None).is_empty());
    }

    #[test]
    fn default_merge_config_prefers_the_main_key() {
        let agent = FakeAgent::default();
        let files = vec![
            ("alpha.yaml".to_string(), b"a".to_vec()),
            (MAIN_CONFIG_KEY.to_string(), b"main".to_vec()),
        ];
        assert_eq!(agent.merge_config(&files), Some(b"main".to_vec()));
        assert_eq!(agent.merge_config(&[]), None);
    }

    fn keys(d: &AgentDescription) -> Vec<&str> {
        d.identifying_attributes
            .iter()
            .map(|kv| kv.key.as_str())
            .collect()
    }

    #[test]
    fn agent_description_reports_identity_version_and_host() {
        let d = agent_description("io.opentelemetry.collector", Some("0.156.0"), &[0u8; 16]);
        let ids = keys(&d);
        assert!(ids.contains(&"service.name"));
        assert!(ids.contains(&"service.instance.id"));
        assert!(ids.contains(&"service.version"));
        assert!(!d.non_identifying_attributes.is_empty());
    }

    #[test]
    fn agent_description_omits_version_when_unknown() {
        let d = agent_description("x", None, &[0u8; 16]);
        assert!(!keys(&d).contains(&"service.version"));
    }

    #[test]
    fn effective_config_prefers_the_agents_report() {
        let reported = EffectiveConfig {
            config_map: Some(AgentConfigMap {
                config_map: [(
                    MAIN_CONFIG_KEY.to_string(),
                    AgentConfigFile {
                        body: b"REPORTED-BY-AGENT".to_vec(),
                        content_type: CONFIG_CONTENT_TYPE.to_string(),
                    },
                )]
                .into_iter()
                .collect(),
            }),
        };
        let sup = supervisor_with(FakeAgent {
            status: AgentStatus {
                effective_config: Some(reported),
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(effective_body(sup.effective_config()), b"REPORTED-BY-AGENT");
    }

    #[test]
    fn effective_config_falls_back_to_written_bytes() {
        let mut sup = supervisor_with(FakeAgent::default());
        sup.applied_body = b"WRITTEN-BY-SUPERVISOR".to_vec();
        assert_eq!(
            effective_body(sup.effective_config()),
            b"WRITTEN-BY-SUPERVISOR"
        );
    }

    #[test]
    fn current_health_is_what_the_agent_reports() {
        let sup = supervisor_with(FakeAgent {
            status: AgentStatus {
                health: ComponentHealth {
                    healthy: false,
                    last_error: "agent says so".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        });
        let health = sup.current_health();
        assert!(!health.healthy);
        assert_eq!(health.last_error, "agent says so");
    }

    #[test]
    fn report_description_tags_the_supervisor_and_pins_instance_id() {
        let reported = AgentDescription {
            identifying_attributes: vec![
                key_value("service.name", "otelcol"),
                key_value("service.instance.id", "wrong-should-be-overridden"),
            ],
            non_identifying_attributes: vec![],
        };
        let sup = supervisor_with(FakeAgent {
            status: AgentStatus {
                agent_description: Some(reported),
                ..Default::default()
            },
            ..Default::default()
        });
        let d = sup.report_description();
        // The supervisor tag is present.
        assert!(d
            .non_identifying_attributes
            .iter()
            .any(|kv| kv.key == SUPERVISOR_ATTRIBUTE));
        // service.instance.id is pinned to the supervisor's UID, not the agent's value.
        let id = d
            .identifying_attributes
            .iter()
            .find(|kv| kv.key == "service.instance.id")
            .and_then(|kv| kv.value.as_ref())
            .and_then(|v| v.value.as_ref());
        match id {
            Some(any_value::Value::StringValue(s)) => {
                assert_eq!(s, &crate::uid::format(&[0u8; 16]))
            }
            _ => panic!("service.instance.id must be a string"),
        }
    }

    #[test]
    fn capabilities_declare_status_and_the_loop() {
        for cap in [
            AgentCapabilities::ReportsStatus,
            AgentCapabilities::AcceptsRemoteConfig,
            AgentCapabilities::ReportsEffectiveConfig,
            AgentCapabilities::ReportsHealth,
            AgentCapabilities::ReportsHeartbeat,
            AgentCapabilities::AcceptsRestartCommand,
            AgentCapabilities::AcceptsOpAmpConnectionSettings,
            AgentCapabilities::ReportsAvailableComponents,
        ] {
            assert_ne!(CAPABILITIES & cap as u64, 0, "{cap:?} must be declared");
        }
        assert_eq!(CAPABILITIES & AgentCapabilities::AcceptsPackages as u64, 0);
    }

    #[test]
    fn restart_command_is_recognised() {
        use opamp_proto::proto::ServerToAgentCommand;
        let msg = ServerToAgent {
            command: Some(ServerToAgentCommand {
                r#type: CommandType::Restart as i32,
            }),
            ..Default::default()
        };
        assert!(is_restart_command(&msg));
        assert!(!is_restart_command(&ServerToAgent::default()));
    }

    #[test]
    fn escalate_backoff_doubles_when_rapid_and_resets_when_stable() {
        // A crash loop doubles the backoff up to the cap.
        let mut b = RESTART_BACKOFF_BASE;
        b = escalate_backoff(b, true);
        assert_eq!(b, RESTART_BACKOFF_BASE * 2);
        for _ in 0..10 {
            b = escalate_backoff(b, true);
        }
        assert_eq!(b, RESTART_BACKOFF_MAX, "backoff is capped");
        // A stable agent resets to the base.
        assert_eq!(escalate_backoff(b, false), RESTART_BACKOFF_BASE);
    }

    #[test]
    fn apply_jitter_spans_the_randomization_band() {
        let base = Duration::from_secs(10);
        // fraction 0 → lower bound (1 - f)·base, 1 → upper bound (1 + f)·base, 0.5 → base itself.
        assert_eq!(
            apply_jitter(base, 0.0),
            base.mul_f64(1.0 - RECONNECT_JITTER)
        );
        assert_eq!(
            apply_jitter(base, 1.0),
            base.mul_f64(1.0 + RECONNECT_JITTER)
        );
        assert_eq!(apply_jitter(base, 0.5), base);
        // The live jitter (clock-driven) always stays inside the band, and never exceeds it.
        for _ in 0..1000 {
            let d = jittered(base);
            assert!(d >= base.mul_f64(1.0 - RECONNECT_JITTER));
            assert!(d <= base.mul_f64(1.0 + RECONNECT_JITTER));
        }
    }

    #[test]
    fn report_description_includes_configured_attributes() {
        let sup = supervisor_with_attributes(vec![
            ("team".to_string(), "telemetry".to_string()),
            ("deployment.environment".to_string(), "staging".to_string()),
        ]);
        let d = sup.report_description();
        let has = |key: &str, val: &str| {
            d.non_identifying_attributes.iter().any(|kv| {
                kv.key == key
                    && matches!(
                        kv.value.as_ref().and_then(|v| v.value.as_ref()),
                        Some(any_value::Value::StringValue(s)) if s == val
                    )
            })
        };
        assert!(has("team", "telemetry"));
        assert!(has("deployment.environment", "staging"));
        // The supervisor tag is still present alongside the configured attributes.
        assert!(d
            .non_identifying_attributes
            .iter()
            .any(|kv| kv.key == SUPERVISOR_ATTRIBUTE));
    }

    #[test]
    fn own_telemetry_from_honours_only_declared_signals() {
        use opamp_proto::proto::{Header, Headers};
        let cs = ConnectionSettingsOffers {
            own_metrics: Some(TelemetryConnectionSettings {
                destination_endpoint: "https://otlp.example/v1/metrics".to_string(),
                headers: Some(Headers {
                    headers: vec![Header {
                        key: "Authorization".to_string(),
                        value: "Bearer x".to_string(),
                    }],
                }),
                ..Default::default()
            }),
            own_logs: Some(TelemetryConnectionSettings {
                destination_endpoint: "https://otlp.example/v1/logs".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        // With only metrics declared, the offered logs destination is ignored.
        let caps = AgentCapabilities::ReportsOwnMetrics as u64;
        let only_metrics = own_telemetry_from(&cs, caps);
        let metrics = only_metrics.metrics.as_ref().unwrap();
        assert_eq!(metrics.endpoint, "https://otlp.example/v1/metrics");
        assert_eq!(
            metrics.headers.get("Authorization").map(String::as_str),
            Some("Bearer x")
        );
        assert!(only_metrics.logs.is_none());
        assert!(only_metrics.traces.is_none());

        // With metrics and logs declared, both are taken; traces was not offered.
        let caps = AgentCapabilities::ReportsOwnMetrics as u64
            | AgentCapabilities::ReportsOwnLogs as u64
            | AgentCapabilities::ReportsOwnTraces as u64;
        let both = own_telemetry_from(&cs, caps);
        assert!(both.metrics.is_some());
        assert!(both.logs.is_some());
        assert!(both.traces.is_none());
    }

    #[test]
    fn declared_capabilities_include_configured_own_telemetry_bits() {
        let mut sup = supervisor_with(FakeAgent::default());
        sup.capabilities = CAPABILITIES | AgentCapabilities::ReportsOwnMetrics as u64;
        let report = sup.next_report();
        assert_ne!(
            report.capabilities & AgentCapabilities::ReportsOwnMetrics as u64,
            0
        );
        // A signal not declared is not claimed.
        assert_eq!(
            report.capabilities & AgentCapabilities::ReportsOwnTraces as u64,
            0
        );
    }

    #[tokio::test]
    async fn confirm_health_is_true_once_the_agent_reports_healthy() {
        let mut sup = supervisor_with(FakeAgent {
            reported_health: Some(healthy()),
            ..Default::default()
        });
        sup.rollback_health_timeout = Duration::from_millis(50);
        assert!(
            sup.confirm_health().await,
            "a healthy report confirms at once"
        );
    }

    #[tokio::test]
    async fn confirm_health_times_out_when_the_agent_never_becomes_healthy() {
        // Reported but unhealthy, and the fake never signals a change, so it can only time out.
        let mut sup = supervisor_with(FakeAgent {
            reported_health: Some(unhealthy("pipeline failed to start")),
            ..Default::default()
        });
        sup.rollback_health_timeout = Duration::from_millis(50);
        assert!(
            !sup.confirm_health().await,
            "an unhealthy agent is not confirmed"
        );

        // Never reported at all (no health, no signal) — also a timeout.
        let mut never = supervisor_with(FakeAgent::default());
        never.rollback_health_timeout = Duration::from_millis(50);
        assert!(!never.confirm_health().await);
    }

    #[test]
    fn commit_applied_records_the_config_and_clears_a_prior_rollback() {
        let mut sup = supervisor_with(FakeAgent::default());
        sup.rolled_back = Some((b"old-bad".to_vec(), "was rolled back".to_string()));
        sup.commit_applied(b"body", b"good-hash");
        assert_eq!(sup.applied_hash, b"good-hash");
        assert_eq!(sup.applied_body, b"body");
        assert!(
            sup.rolled_back.is_none(),
            "a new good config clears the rollback"
        );
    }

    #[test]
    fn full_state_reports_a_rolled_back_config_as_failed() {
        let mut sup = supervisor_with(FakeAgent::default());
        sup.applied_hash = b"good-hash".to_vec();
        sup.applied_body = b"good-body".to_vec();
        sup.rolled_back = Some((b"bad-hash".to_vec(), "did not become healthy".to_string()));
        let report = sup.full_state_report();
        let status = report.remote_config_status.expect("a status is reported");
        // The Server sees the *failed* config's hash (not the good one), so it does not resend it.
        assert_eq!(status.last_remote_config_hash, b"bad-hash");
        assert_eq!(status.status, RemoteConfigStatuses::Failed as i32);
        assert_eq!(status.error_message, "did not become healthy");
    }

    #[test]
    fn full_state_reports_the_applied_config_when_nothing_was_rolled_back() {
        let mut sup = supervisor_with(FakeAgent::default());
        sup.applied_hash = b"good-hash".to_vec();
        sup.applied_body = b"good-body".to_vec();
        let report = sup.full_state_report();
        let status = report.remote_config_status.expect("a status is reported");
        assert_eq!(status.last_remote_config_hash, b"good-hash");
        assert_eq!(status.status, RemoteConfigStatuses::Applied as i32);
    }

    #[test]
    fn heartbeat_override_reads_a_positive_interval() {
        use opamp_proto::proto::{ConnectionSettingsOffers, OpAmpConnectionSettings};
        let msg = ServerToAgent {
            connection_settings: Some(ConnectionSettingsOffers {
                opamp: Some(OpAmpConnectionSettings {
                    heartbeat_interval_seconds: 45,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(heartbeat_override(&msg), Some(Duration::from_secs(45)));
        assert_eq!(heartbeat_override(&ServerToAgent::default()), None);
    }

    fn opamp_offer(endpoint: &str, auth: Option<&str>) -> ConnectionSettingsOffers {
        use opamp_proto::proto::{Header, Headers, OpAmpConnectionSettings};
        ConnectionSettingsOffers {
            opamp: Some(OpAmpConnectionSettings {
                destination_endpoint: endpoint.to_string(),
                headers: auth.map(|value| Headers {
                    headers: vec![Header {
                        key: "Authorization".to_string(),
                        value: value.to_string(),
                    }],
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn opamp_connection_offer_repoints_arms_revert_and_is_idempotent() {
        let mut sup = supervisor_with(FakeAgent::default());
        sup.server_url = "ws://old/v1/opamp".to_string();

        let offer = opamp_offer("wss://new/v1/opamp", Some("Bearer rotated"));
        assert!(
            sup.apply_opamp_connection_offer(&offer),
            "a new endpoint re-points"
        );
        assert_eq!(sup.server_url, "wss://new/v1/opamp");
        assert_eq!(
            sup.offered_headers,
            vec![("Authorization".to_string(), "Bearer rotated".to_string())]
        );
        assert!(sup.reconnect_requested);
        assert!(
            sup.pending_revert.is_some(),
            "the previous settings are armed for revert"
        );

        // Re-offering the identical settings changes nothing.
        sup.reconnect_requested = false;
        assert!(!sup.apply_opamp_connection_offer(&offer));
        assert!(!sup.reconnect_requested);
    }

    #[test]
    fn heartbeat_only_offer_does_not_repoint() {
        use opamp_proto::proto::OpAmpConnectionSettings;
        let mut sup = supervisor_with(FakeAgent::default());
        let cs = ConnectionSettingsOffers {
            opamp: Some(OpAmpConnectionSettings {
                heartbeat_interval_seconds: 30,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!sup.apply_opamp_connection_offer(&cs));
        assert!(!sup.reconnect_requested);
    }

    #[test]
    fn opamp_connection_offer_ignored_without_the_capability() {
        let mut sup = supervisor_with(FakeAgent::default());
        sup.capabilities &= !(AgentCapabilities::AcceptsOpAmpConnectionSettings as u64);
        assert!(!sup.apply_opamp_connection_offer(&opamp_offer("wss://new/", None)));
    }

    #[test]
    fn revert_restores_the_previous_connection_settings() {
        let mut sup = supervisor_with(FakeAgent::default());
        sup.server_url = "ws://old/v1/opamp".to_string();
        let snapshot = sup.conn_snapshot();
        sup.server_url = "wss://new/".to_string();
        sup.offered_headers = vec![("X".to_string(), "y".to_string())];
        sup.restore_conn(snapshot);
        assert_eq!(sup.server_url, "ws://old/v1/opamp");
        assert!(sup.offered_headers.is_empty());
    }

    // --- Package distribution (ADR-0018) ---

    use opamp_proto::proto::{DownloadableFile, Header, Headers, PackageAvailable};

    fn package(version: &str, hash: &[u8], type_: PackageType) -> PackageAvailable {
        PackageAvailable {
            r#type: type_ as i32,
            version: version.to_string(),
            file: Some(DownloadableFile {
                download_url: "http://dev:4321/packages/otelcol".to_string(),
                content_hash: vec![1, 2, 3],
                ..Default::default()
            }),
            hash: hash.to_vec(),
        }
    }

    fn package_supervisor() -> Supervisor<FakeAgent> {
        let seq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("opamp-sup-pkg-{}-{}", std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        let mut sup = supervisor_with(FakeAgent {
            accepts_packages: true,
            ..Default::default()
        });
        sup.packages_enabled = true;
        sup.storage_dir = dir;
        sup
    }

    #[test]
    fn a_package_capable_agent_declares_the_package_capabilities() {
        let capable = Supervisor::new(
            package_config(),
            FakeAgent {
                accepts_packages: true,
                ..Default::default()
            },
        );
        assert_ne!(
            capable.capabilities & AgentCapabilities::AcceptsPackages as u64,
            0
        );
        assert_ne!(
            capable.capabilities & AgentCapabilities::ReportsPackageStatuses as u64,
            0
        );
        // An agent that does not accept packages declares neither bit.
        let plain = supervisor_with(FakeAgent::default());
        assert_eq!(
            plain.capabilities
                & (AgentCapabilities::AcceptsPackages as u64
                    | AgentCapabilities::ReportsPackageStatuses as u64),
            0
        );
    }

    fn package_config() -> Config {
        Config {
            server_url: "ws://localhost/v1/opamp".to_string(),
            instance_uid: [0u8; 16],
            uid_path: std::path::PathBuf::from("/tmp/opamp-sup-pkgcap-uid"),
            storage_dir: std::path::PathBuf::from("/tmp/opamp-sup-pkgcap-store"),
            service_name: "io.opentelemetry.collector".to_string(),
            agent_version: None,
            fallback: Vec::new(),
            heartbeat: Duration::from_secs(30),
            extra_attributes: Vec::new(),
            own_telemetry_capabilities: 0,
            automatic_config_rollback: false,
            auth_token: None,
            tls_ca: None,
            tls_insecure: false,
        }
    }

    #[test]
    fn take_top_level_picks_the_top_level_package_ignoring_addons() {
        let packages: HashMap<String, PackageAvailable> = [
            (
                "z-addon".to_string(),
                package("1", b"a", PackageType::Addon),
            ),
            (
                "otelcol".to_string(),
                package("2", b"b", PackageType::TopLevel),
            ),
        ]
        .into_iter()
        .collect();
        let (name, pkg) = take_top_level(packages).expect("a top-level package is picked");
        assert_eq!(name, "otelcol");
        assert_eq!(pkg.version, "2");

        // No top-level entry → nothing to install.
        let only_addon: HashMap<String, PackageAvailable> =
            [("a".to_string(), package("1", b"a", PackageType::Addon))]
                .into_iter()
                .collect();
        assert!(take_top_level(only_addon).is_none());
    }

    #[test]
    fn header_pairs_extracts_offered_headers() {
        let headers = Headers {
            headers: vec![Header {
                key: "Authorization".to_string(),
                value: "Bearer x".to_string(),
            }],
        };
        assert_eq!(
            header_pairs(Some(&headers)),
            vec![("Authorization".to_string(), "Bearer x".to_string())]
        );
        assert!(header_pairs(None).is_empty());
    }

    #[test]
    fn package_statuses_reports_the_installed_package_and_all_hash() {
        let mut sup = package_supervisor();
        // Nothing installed yet, but enabled → an (empty) statuses message with the aggregate hash.
        sup.package_all_hash = vec![9, 9];
        let statuses = sup
            .package_statuses()
            .expect("enabled agents report statuses");
        assert_eq!(statuses.server_provided_all_packages_hash, vec![9, 9]);
        assert!(statuses.packages.is_empty());

        // With an installed package and its status, it is reported under its name.
        sup.installed_package = Some(InstalledPackage {
            version: "2.0.0".to_string(),
            hash: vec![7],
        });
        sup.package_status = Some(sup.package_status_for(
            "otelcol",
            "2.0.0",
            &[7],
            PackageStatusEnum::Installed,
            String::new(),
        ));
        let statuses = sup.package_statuses().unwrap();
        let status = &statuses.packages["otelcol"];
        assert_eq!(status.status, PackageStatusEnum::Installed as i32);
        assert_eq!(status.agent_has_version, "2.0.0");
        assert_eq!(status.server_offered_version, "2.0.0");

        // A non-package agent reports no statuses at all.
        let plain = supervisor_with(FakeAgent::default());
        assert!(plain.package_statuses().is_none());
    }

    #[test]
    fn package_status_for_carries_agent_has_and_server_offered() {
        let mut sup = package_supervisor();
        sup.installed_package = Some(InstalledPackage {
            version: "1.0.0".to_string(),
            hash: vec![1],
        });
        // A failed install of a newer version: agent still has 1.0.0, Server offered 2.0.0.
        let status = sup.package_status_for(
            "otelcol",
            "2.0.0",
            &[2],
            PackageStatusEnum::InstallFailed,
            "boom".to_string(),
        );
        assert_eq!(status.agent_has_version, "1.0.0");
        assert_eq!(status.agent_has_hash, vec![1]);
        assert_eq!(status.server_offered_version, "2.0.0");
        assert_eq!(status.server_offered_hash, vec![2]);
        assert_eq!(status.status, PackageStatusEnum::InstallFailed as i32);
        assert_eq!(status.error_message, "boom");
    }

    #[test]
    fn persist_and_load_packages_round_trips() {
        let mut sup = package_supervisor();
        let dir = sup.storage_dir.clone();
        sup.package_all_hash = vec![0xaa, 0xbb];
        sup.installed_package = Some(InstalledPackage {
            version: "2.0.0".to_string(),
            hash: vec![0xde, 0xad],
        });
        sup.package_status = Some(sup.package_status_for(
            "otelcol",
            "2.0.0",
            &[0xde, 0xad],
            PackageStatusEnum::Installed,
            String::new(),
        ));
        sup.persist_packages();

        // A fresh supervisor on the same storage dir resumes the installed package and its status.
        let mut resumed = package_supervisor();
        resumed.storage_dir = dir;
        resumed.load_packages();
        assert_eq!(resumed.package_all_hash, vec![0xaa, 0xbb]);
        let installed = resumed
            .installed_package
            .expect("resumed the installed package");
        assert_eq!(installed.version, "2.0.0");
        assert_eq!(installed.hash, vec![0xde, 0xad]);
        let status = resumed.package_status.expect("resumed the reported status");
        assert_eq!(status.name, "otelcol");
        assert_eq!(status.status, PackageStatusEnum::Installed as i32);
    }
}
