//! In-memory fleet state and the OpAMP control loop, keyed by Instance UID — never by the
//! connection that carried a message (ADR-0003).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use opamp::proto::{
    any_value, AgentConfigFile, AgentConfigMap, AgentDescription, AgentIdentification,
    AgentRemoteConfig, AgentToServer, AgentToServerFlags, AvailableComponents, ComponentHealth,
    ConnectionSettingsOffers, ConnectionSettingsStatus, Header, Headers, KeyValue,
    OpAmpConnectionSettings, PackageStatuses, PackagesAvailable, RemoteConfigStatus,
    RemoteConfigStatuses, ServerCapabilities, ServerErrorResponse, ServerErrorResponseType,
    ServerToAgent, ServerToAgentFlags,
};
use opamp::uid::InstanceUid;
use prost::Message as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tracing::{info, warn};
use utoipa::ToSchema;

use crate::config::ConnectionOfferConfig;
use crate::configs::{ConfigStore, Configuration, DesiredConfig};
use crate::packages::PackageStore;

/// The package upload limit in force when nothing configures one — roomy, because a real agent
/// binary is (see `server.toml`, `max_package_size_bytes`).
pub const DEFAULT_MAX_PACKAGE_SIZE: usize = 1024 * 1024 * 1024; // 1 GiB

/// The Capability Set this Server declares (see docs/CONFORMANCE.md).
pub const SERVER_CAPABILITIES: u64 = ServerCapabilities::AcceptsStatus as u64
    | ServerCapabilities::OffersRemoteConfig as u64
    | ServerCapabilities::AcceptsEffectiveConfig as u64;

/// Identifies one WebSocket connection for the duplicate detection the Baseline asks of the
/// Server. Never a routing key — Agents are routed by `instance_uid` alone (ADR-0003); this only
/// answers "is this identity already alive on *another* connection?".
pub type ConnId = u64;

/// Which transport a report arrived on. Recorded for the operator; it never keys any state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Http,
    WebSocket,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Transport::Http => "http",
            Transport::WebSocket => "websocket",
        }
    }
}

/// Everything the Server knows about one Agent.
pub struct AgentRecord {
    pub sequence_num: u64,
    pub capabilities: u64,
    pub description: Option<AgentDescription>,
    pub health: Option<ComponentHealth>,
    pub effective_config: Option<String>,
    pub remote_config_status: Option<RemoteConfigStatus>,
    pub transport: Transport,
    pub connected: bool,
    pub last_seen_ms: u64,
    /// An operator-requested restart not yet delivered. Lives on the record, not a connection,
    /// so it reaches the Agent on its next exchange whichever transport carries it.
    pub restart_pending: bool,
    /// The Agent's available components — hash-only until the full map was demanded and arrived.
    pub available_components: Option<AvailableComponents>,
    /// The outcome of the last connection-settings offer this Agent reported (ADR-0014); its
    /// hash is what gates re-offering.
    pub connection_settings_status: Option<ConnectionSettingsStatus>,
    /// The package statuses this Agent last reported (ADR-0015); the
    /// `server_provided_all_packages_hash` inside is what gates re-offering packages.
    pub package_statuses: Option<PackageStatuses>,
    /// The WebSocket connection currently carrying this Agent; `None` for plain HTTP, whose
    /// polling is stateless. Only the owning connection may mark the Agent disconnected, and a
    /// report from a *different* live connection is the duplicate the Baseline wants detected.
    pub owner: Option<ConnId>,
}

/// Why a restart request was refused (`POST /api/v1/agents/{uid}/restart`).
pub enum RestartError {
    /// No Agent of that identity is known.
    UnknownAgent,
    /// The Agent does not declare `AcceptsRestartCommand` — capability negotiation is binding,
    /// so the Server refuses rather than sending a command the Agent would ignore.
    NoCapability,
}

/// The result of processing one `AgentToServer`: the reply to send back on the same transport, and
/// what the transport layer needs to know for its own bookkeeping.
pub struct Processed {
    pub reply: ServerToAgent,
    /// The identity the Agent goes by *after* this message (it may have been reassigned).
    pub uid: Option<InstanceUid>,
    /// The Agent said goodbye; a WebSocket loop drops it from its connection-local set.
    pub disconnected: bool,
}

/// The one `OpAMPConnectionSettings` this Server offers (ADR-0014), precompiled from the
/// `[connection_offer]` section with the hash that gates its delivery.
pub struct ConnectionOffer {
    settings: OpAmpConnectionSettings,
    hash: Vec<u8>,
}

impl ConnectionOffer {
    pub fn from_config(config: &ConnectionOfferConfig) -> Result<Self, String> {
        let settings = OpAmpConnectionSettings {
            destination_endpoint: config.endpoint.clone().unwrap_or_default(),
            headers: config.authorization()?.map(|value| Headers {
                headers: vec![Header {
                    key: "Authorization".to_string(),
                    value,
                }],
            }),
            heartbeat_interval_seconds: config.heartbeat_interval_secs.unwrap_or(0),
            ..Default::default()
        };
        // The hash identifies the offer as a whole — an Agent echoing it needs nothing again.
        let hash = Sha256::digest(settings.encode_to_vec()).to_vec();
        Ok(ConnectionOffer { settings, hash })
    }
}

/// The package store plus the base URL each `download_url` is built from (ADR-0015).
pub struct PackageOffering {
    store: PackageStore,
    download_base: String,
}

impl PackageOffering {
    /// `download_base` is the advertised absolute URL, or empty for a path the Client resolves
    /// against its own endpoint. The download sits on the unauthenticated REST plane (ADR-0013);
    /// the artifact's content hash and signature are what protect it, so no credential rides it.
    pub fn new(store: PackageStore, download_base: String) -> Self {
        PackageOffering {
            store,
            download_base,
        }
    }

    pub fn store(&self) -> &PackageStore {
        &self.store
    }
}

/// Shared state behind every handler: the fleet, the Configuration store, and the push channel
/// WebSocket loops subscribe to.
pub struct AppState {
    fleet: Mutex<HashMap<InstanceUid, AgentRecord>>,
    configs: ConfigStore,
    push: watch::Sender<u64>,
    /// Hands every WebSocket connection its identity for the duplicate detection.
    next_conn: AtomicU64,
    /// The connection settings offered to the fleet (ADR-0014); `None` offers nothing and leaves
    /// `OffersConnectionSettings` undeclared.
    connection_offer: Option<ConnectionOffer>,
    /// The packages offered to the fleet (ADR-0015); `None` offers nothing and leaves
    /// `OffersPackages` undeclared.
    packages: Option<PackageOffering>,
    /// The message size limit both transports enforce, in each direction (the Baseline's MUST).
    max_message_size: usize,
    /// The largest package artifact the REST API accepts on upload (ADR-0015) — a program, not a
    /// message, so it is bounded separately and far more generously.
    max_package_size: usize,
}

impl AppState {
    /// Builds the state, restoring every persisted Configuration from `config_dir`. A store that
    /// cannot be opened (or holds an unparsable file) fails startup loudly.
    pub fn new(config_dir: PathBuf) -> Result<Self, String> {
        let configs = ConfigStore::open(config_dir)?;
        let restored = configs.list().len();
        if restored > 0 {
            info!(
                configurations = restored,
                "restored the Configuration store"
            );
        }
        Ok(AppState {
            fleet: Mutex::new(HashMap::new()),
            configs,
            push: watch::channel(0).0,
            next_conn: AtomicU64::new(1),
            connection_offer: None,
            packages: None,
            max_message_size: opamp::frame::DEFAULT_MAX_MESSAGE_SIZE,
            max_package_size: DEFAULT_MAX_PACKAGE_SIZE,
        })
    }

    /// Sets the message size limit both transports enforce (the Baseline recommends the default
    /// [`opamp::frame::DEFAULT_MAX_MESSAGE_SIZE`] and asks that it be configurable).
    #[must_use]
    pub fn with_max_message_size(mut self, limit: usize) -> Self {
        self.max_message_size = limit;
        self
    }

    /// The message size limit in force, for the transports to enforce in both directions.
    pub fn max_message_size(&self) -> usize {
        self.max_message_size
    }

    /// Sets the largest package artifact the REST API accepts on upload (ADR-0015).
    #[must_use]
    pub fn with_max_package_size(mut self, limit: usize) -> Self {
        self.max_package_size = limit;
        self
    }

    /// The package upload limit in force, for the REST API's package route.
    pub fn max_package_size(&self) -> usize {
        self.max_package_size
    }

    /// Arms the connection-settings offer (ADR-0014); with it the Server declares
    /// `OffersConnectionSettings`.
    #[must_use]
    pub fn with_connection_offer(mut self, offer: Option<ConnectionOffer>) -> Self {
        self.connection_offer = offer;
        self
    }

    /// Arms package delivery (ADR-0015); with a non-empty store the Server declares
    /// `OffersPackages` and `AcceptsPackagesStatus`.
    #[must_use]
    pub fn with_packages(mut self, packages: Option<PackageOffering>) -> Self {
        self.packages = packages;
        self
    }

    /// Read access to the package store, for the REST API's package routes.
    pub fn packages(&self) -> Option<&PackageStore> {
        self.packages.as_ref().map(PackageOffering::store)
    }

    /// The Capability Set this Server declares: the base set, plus `OffersConnectionSettings`
    /// while a connection offer is configured and `OffersPackages` / `AcceptsPackagesStatus`
    /// while a non-empty package store is armed — an undeclared capability is never exercised, a
    /// declared one never hollow.
    fn capabilities(&self) -> u64 {
        let mut caps = SERVER_CAPABILITIES;
        if self.connection_offer.is_some() {
            caps |= ServerCapabilities::OffersConnectionSettings as u64;
        }
        if self.packages.as_ref().is_some_and(|p| !p.store.is_empty()) {
            caps |= ServerCapabilities::OffersPackages as u64
                | ServerCapabilities::AcceptsPackagesStatus as u64;
        }
        caps
    }

    /// A fresh identity for one WebSocket connection.
    pub fn connection_id(&self) -> ConnId {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    /// A receiver that fires whenever any Configuration changes; WebSocket loops use it to push
    /// offers without waiting for the Agent to speak.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.push.subscribe()
    }

    /// Read access to the Configuration store (the REST API's `GET` routes).
    pub fn configurations(&self) -> &ConfigStore {
        &self.configs
    }

    /// Creates or replaces a Configuration, persists it, and wakes every WebSocket loop — the
    /// matching Agents are offered the change without being asked.
    pub fn put_configuration(&self, config: Configuration) -> Result<(), String> {
        let name = config.name.clone();
        self.configs.put(config)?;
        self.push.send_modify(|rev| *rev += 1);
        info!(configuration = %name, "configuration stored and distributed");
        Ok(())
    }

    /// Queues a restart for one Agent (`AcceptsRestartCommand`) and wakes the WebSocket loops so
    /// a connected Agent hears it now; a polling one picks it up on its next exchange.
    pub fn request_restart(&self, uid: &InstanceUid) -> Result<(), RestartError> {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get_mut(uid).ok_or(RestartError::UnknownAgent)?;
        if record.capabilities & opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64 == 0
        {
            return Err(RestartError::NoCapability);
        }
        record.restart_pending = true;
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        info!(agent = %uid, "restart requested");
        Ok(())
    }

    /// The queued restart for this Agent as the Baseline's command-only message, taken exactly
    /// once — `None` when nothing is queued (or the Agent went away).
    pub fn restart_command_for(&self, uid: &InstanceUid) -> Option<ServerToAgent> {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get_mut(uid)?;
        if !record.restart_pending || !record.connected {
            return None;
        }
        record.restart_pending = false;
        Some(restart_command(uid, self.capabilities()))
    }

    /// Deletes a Configuration and wakes every WebSocket loop; `false` when none of that name
    /// exists. Agents that applied it keep running it — narrowing never revokes (ADR-0012).
    pub fn delete_configuration(&self, name: &str) -> Result<bool, String> {
        let deleted = self.configs.delete(name)?;
        if deleted {
            self.push.send_modify(|rev| *rev += 1);
            info!(configuration = %name, "configuration deleted");
        }
        Ok(deleted)
    }

    /// The control loop for one report, shared by both transports (ADR-0007): update what we know,
    /// then answer with what the Agent still lacks — the config offer gated by the hash comparison.
    /// `conn` identifies the WebSocket connection that carried the report; `None` for plain HTTP.
    pub fn process(
        &self,
        msg: AgentToServer,
        transport: Transport,
        conn: Option<ConnId>,
    ) -> Processed {
        let Some(mut uid) = InstanceUid::from_wire(&msg.instance_uid) else {
            warn!(
                len = msg.instance_uid.len(),
                "report with a malformed instance_uid"
            );
            return Processed {
                reply: bad_request("instance_uid must be 16 bytes (UUID v7 recommended)"),
                uid: None,
                disconnected: false,
            };
        };

        let mut fleet = self.fleet.lock().expect("fleet lock");
        let mut reply_flags = 0u64;
        let mut identification = None;

        // The Agent asked the Server to assign its identity (AgentToServerFlags_RequestInstanceUid):
        // mint a UUID v7 and re-key the record; the reply tells the Agent to adopt it.
        if msg.flags & AgentToServerFlags::RequestInstanceUid as u64 != 0 {
            let new_uid = InstanceUid::default();
            if let Some(record) = fleet.remove(&uid) {
                fleet.insert(new_uid, record);
            }
            info!(old = %uid, new = %new_uid, "assigned a server-generated instance_uid");
            identification = Some(AgentIdentification {
                new_instance_uid: new_uid.as_bytes().to_vec(),
            });
            uid = new_uid;
        }

        // Duplicate instance_uid detection (a Baseline SHOULD): the same identity alive on
        // *another* WebSocket connection — bad UID generators, cloned VMs — is rekeyed exactly
        // as the Baseline prescribes: mint a fresh uid and answer with AgentIdentification,
        // which the Client adopts. The newcomer starts a record of its own; the incumbent and
        // its connection stay untouched. (Stateless plain-HTTP polling offers nothing to
        // distinguish two pollers by, so detection is WebSocket-only.)
        if let Some(this_conn) = conn {
            let duplicate = fleet.get(&uid).is_some_and(|existing| {
                existing.connected && existing.owner.is_some_and(|owner| owner != this_conn)
            });
            if duplicate {
                let new_uid = InstanceUid::default();
                warn!(duplicate = %uid, new = %new_uid, "duplicate instance_uid; rekeying the newcomer");
                identification = Some(AgentIdentification {
                    new_instance_uid: new_uid.as_bytes().to_vec(),
                });
                uid = new_uid;
            }
        }

        let known = fleet.contains_key(&uid);
        let record = fleet.entry(uid).or_insert_with(|| {
            info!(agent = %uid, transport = transport.as_str(), "new agent");
            AgentRecord {
                sequence_num: msg.sequence_num,
                capabilities: 0,
                description: None,
                health: None,
                effective_config: None,
                remote_config_status: None,
                transport,
                connected: true,
                last_seen_ms: now_ms(),
                restart_pending: false,
                available_components: None,
                connection_settings_status: None,
                package_statuses: None,
                owner: conn,
            }
        });

        // A compressed report (unchanged fields omitted) is only usable if our state is current.
        // A gap in sequence_num — or an Agent we have never seen describing itself with nothing —
        // means state was lost somewhere; the Baseline's recovery is to demand a full report.
        let compressed = msg.agent_description.is_none();
        let gap = known && msg.sequence_num != record.sequence_num.wrapping_add(1);
        if compressed && (!known || gap) {
            reply_flags |= ServerToAgentFlags::ReportFullState as u64;
        }

        record.sequence_num = msg.sequence_num;
        record.transport = transport;
        record.connected = true;
        record.owner = conn;
        record.last_seen_ms = now_ms();
        if msg.capabilities != 0 {
            record.capabilities = msg.capabilities;
        }
        if let Some(description) = msg.agent_description {
            record.description = Some(description);
        }
        if let Some(health) = msg.health {
            record.health = Some(health);
        }
        if let Some(effective) = msg.effective_config {
            record.effective_config = Some(config_map_text(effective.config_map.as_ref()));
        }
        if let Some(status) = msg.remote_config_status {
            record.remote_config_status = Some(status);
        }
        if let Some(status) = msg.connection_settings_status {
            if status.status == opamp::proto::ConnectionSettingsStatuses::Failed as i32 {
                warn!(agent = %uid, error = %status.error_message, "connection settings rejected");
            }
            record.connection_settings_status = Some(status);
        }
        if let Some(statuses) = msg.package_statuses {
            for status in statuses.packages.values() {
                if status.status == opamp::proto::PackageStatusEnum::InstallFailed as i32 {
                    warn!(agent = %uid, package = %status.name, error = %status.error_message, "package installation failed");
                }
            }
            // An Agent that refuses the offer itself has no package to hang the reason on, so the
            // report carries it. Logged and surfaced, or a Client refusing every offer it is sent
            // would look like one that is simply not installing anything.
            if !statuses.error_message.is_empty() {
                warn!(agent = %uid, error = %statuses.error_message, "the agent refused the package offer");
            }
            record.package_statuses = Some(statuses);
        }
        if let Some(incoming) = msg.available_components {
            // A routine hash-only update must not degrade an already-fetched full map of the
            // same hash; anything else (first sight, or a changed hash) replaces the stored value.
            let keep_stored_full = record.available_components.as_ref().is_some_and(|stored| {
                incoming.components.is_empty()
                    && !stored.components.is_empty()
                    && stored.hash == incoming.hash
            });
            if !keep_stored_full {
                record.available_components = Some(incoming);
            }
        }

        let disconnected = msg.agent_disconnect.is_some();
        if disconnected {
            info!(agent = %uid, "agent disconnected");
            record.connected = false;
            record.owner = None;
        }

        // A hash without the map is an offer to fetch: demand the full component list from an
        // Agent that declared it can report one (the flag is meaningless toward any other).
        if !disconnected
            && record.capabilities
                & opamp::proto::AgentCapabilities::ReportsAvailableComponents as u64
                != 0
            && record
                .available_components
                .as_ref()
                .is_some_and(|ac| ac.components.is_empty())
        {
            reply_flags |= ServerToAgentFlags::ReportAvailableComponents as u64;
        }

        // A queued restart goes out as the Baseline's command-only message: nothing but
        // identity, capabilities, and the command. Anything else the reply would carry —
        // an identity reassignment, a demanded full report — defers the command to the next
        // exchange instead of being combined with it.
        if record.restart_pending
            && !disconnected
            && identification.is_none()
            && reply_flags == 0
            && record.capabilities & opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64
                != 0
        {
            record.restart_pending = false;
            return Processed {
                reply: restart_command(&uid, self.capabilities()),
                uid: Some(uid),
                disconnected: false,
            };
        }

        // The config offer — composed from the Configurations whose Selectors match this Agent
        // (ADR-0012), gated by the hash comparison, and only toward an Agent that both said
        // goodbye ≠ true and declared AcceptsRemoteConfig (capability negotiation is binding).
        let remote_config = if disconnected {
            None
        } else {
            let desired = self.configs.desired_for(record.description.as_ref());
            offer(record, desired.as_ref())
        };

        // The connection-settings offer (ADR-0014), gated the same way: by capability and by
        // the hash the Agent last reported — the Baseline's own "compare and include" MUST.
        let connection_settings = if disconnected {
            None
        } else {
            self.settings_offer(record)
        };

        // The package offer (ADR-0015), gated by capability and the reported
        // server_provided_all_packages_hash — the Baseline's "compare and include" for packages.
        let packages_available = if disconnected {
            None
        } else {
            self.packages_offer(record)
        };

        Processed {
            reply: ServerToAgent {
                instance_uid: uid.as_bytes().to_vec(),
                capabilities: self.capabilities(),
                flags: reply_flags,
                remote_config,
                connection_settings,
                packages_available,
                agent_identification: identification,
                ..Default::default()
            },
            uid: Some(uid),
            disconnected,
        }
    }

    /// The package offer for one Agent, or `None` when it cannot accept packages, no package's
    /// Selector matches it, or the aggregate hash it last reported already matches the set it
    /// should have.
    ///
    /// Both the offer and the aggregate are computed over *this Agent's* matching packages
    /// (ADR-0017): comparing against a fleet-wide aggregate would re-offer, on every exchange,
    /// packages this Agent is never given.
    fn packages_offer(&self, record: &AgentRecord) -> Option<PackagesAvailable> {
        let offering = self.packages.as_ref()?;
        if record.capabilities & opamp::proto::AgentCapabilities::AcceptsPackages as u64 == 0 {
            return None;
        }
        let description = record.description.as_ref();
        let reported = record
            .package_statuses
            .as_ref()
            .map(|s| s.server_provided_all_packages_hash.as_slice())
            .unwrap_or_default();
        if reported == offering.store.all_packages_hash_for(description).as_slice() {
            return None;
        }
        match offering
            .store
            .offer_for(description, &offering.download_base, None)
        {
            Ok(offer) => offer,
            // Ambiguous targeting: two equally specific Selectors both reach this Agent. Offering
            // neither is the only honest answer — but silence would leave an operator watching a
            // rollout that never starts, so it is logged and shown in the fleet view.
            Err(e) => {
                warn!(error = %e, "refusing to offer a package: ambiguous targeting");
                None
            }
        }
    }

    /// Why this Agent is offered no package, when the reason is the operator's targeting rather
    /// than the absence of one (ADR-0017). `None` when nothing is wrong.
    fn package_conflict(&self, record: &AgentRecord) -> Option<String> {
        let offering = self.packages.as_ref()?;
        if record.capabilities & opamp::proto::AgentCapabilities::AcceptsPackages as u64 == 0 {
            return None;
        }
        offering
            .store
            .offer_for(record.description.as_ref(), "", None)
            .err()
    }

    /// The connection-settings offer for one Agent, or `None` when it cannot accept one or its
    /// reported hash says it already runs (or refused) exactly this offer.
    fn settings_offer(&self, record: &AgentRecord) -> Option<ConnectionSettingsOffers> {
        let offer = self.connection_offer.as_ref()?;
        if record.capabilities
            & opamp::proto::AgentCapabilities::AcceptsOpAmpConnectionSettings as u64
            == 0
        {
            return None;
        }
        // The Baseline's gate: include the offer when the reported hash differs. An APPLYING
        // echo of the same hash keeps the offer coming — a verification whose outcome was lost
        // (a dropped connection mid-switch) must heal by retry, not hang.
        if let Some(status) = &record.connection_settings_status {
            if status.last_connection_settings_hash == offer.hash
                && status.status != opamp::proto::ConnectionSettingsStatuses::Applying as i32
            {
                return None;
            }
        }
        Some(ConnectionSettingsOffers {
            hash: offer.hash.clone(),
            opamp: Some(offer.settings.clone()),
            ..Default::default()
        })
    }

    /// The unsolicited offer a WebSocket loop pushes when a Configuration or package changes;
    /// `None` when the Agent already runs both (or nothing matches it, or it cannot accept one),
    /// so nothing redundant crosses the wire.
    pub fn offer_for(&self, uid: &InstanceUid) -> Option<ServerToAgent> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get(uid)?;
        let desired = self.configs.desired_for(record.description.as_ref());
        let remote_config = offer(record, desired.as_ref());
        let packages_available = self.packages_offer(record);
        if remote_config.is_none() && packages_available.is_none() {
            return None;
        }
        Some(ServerToAgent {
            instance_uid: uid.as_bytes().to_vec(),
            capabilities: self.capabilities(),
            remote_config,
            packages_available,
            ..Default::default()
        })
    }

    /// Creates or replaces a package (ADR-0015) from a streamed upload, persists it, and wakes
    /// every WebSocket loop so a matching connected Agent is offered it now.
    pub fn put_package(
        &self,
        name: String,
        version: String,
        addon: bool,
        signature: Option<Vec<u8>>,
        staged: &std::path::Path,
    ) -> Result<(), String> {
        let store = self
            .packages
            .as_ref()
            .ok_or("package delivery is not configured on this Server")?
            .store();
        store.put_staged(name.clone(), version, addon, signature, staged)?;
        self.push.send_modify(|rev| *rev += 1);
        info!(package = %name, "package stored and offered");
        Ok(())
    }

    /// Where an upload for `name` is streamed before it becomes a package.
    pub fn package_staging_path(&self, name: &str) -> Result<std::path::PathBuf, String> {
        self.packages
            .as_ref()
            .ok_or("package delivery is not configured on this Server")?
            .store()
            .staging_path(name)
    }

    /// Points a package at an artifact hosted elsewhere (ADR-0018) and wakes every WebSocket loop,
    /// so a targeted Agent is offered the new address now rather than at its next poll.
    pub fn set_package_source(
        &self,
        name: &str,
        version: &str,
        addon: bool,
        content_hash: Vec<u8>,
        signature: Option<Vec<u8>>,
        source: crate::packages::Source,
    ) -> Result<(), String> {
        let store = self
            .packages
            .as_ref()
            .ok_or("package delivery is not configured on this Server")?
            .store();
        store.set_source(name, version, addon, content_hash, signature, source)?;
        self.push.send_modify(|rev| *rev += 1);
        info!(package = %name, "package now referenced from its source");
        Ok(())
    }

    /// Sets a package's Selector (ADR-0017) and wakes every WebSocket loop, so an Agent that the
    /// change newly targets is offered it now rather than at its next poll. Returns the package's
    /// version, for the response.
    pub fn set_package_selector(
        &self,
        name: &str,
        selector: BTreeMap<String, String>,
    ) -> Result<String, String> {
        let store = self
            .packages
            .as_ref()
            .ok_or("package delivery is not configured on this Server")?
            .store();
        store.set_selector(name, selector)?;
        let version = store
            .list()
            .into_iter()
            .find(|p| p.name == name)
            .map(|p| p.version)
            .unwrap_or_default();
        self.push.send_modify(|rev| *rev += 1);
        info!(package = %name, "package selector changed");
        Ok(version)
    }

    /// Deletes a package; `Ok(false)` when none of that name exists.
    pub fn delete_package(&self, name: &str) -> Result<bool, String> {
        let store = self
            .packages
            .as_ref()
            .ok_or("package delivery is not configured on this Server")?
            .store();
        let deleted = store.delete(name)?;
        if deleted {
            self.push.send_modify(|rev| *rev += 1);
            info!(package = %name, "package deleted");
        }
        Ok(deleted)
    }

    /// Marks the Agents a closing WebSocket connection carried as no longer connected — but only
    /// those the connection still *owns*: after a rekey (or a transport switch) another live
    /// connection may legitimately carry an identity this one once saw, and a closing socket
    /// must not take it down. State stays: the fleet remembers what each Agent last reported.
    pub fn mark_disconnected(&self, uids: &[InstanceUid], conn: ConnId) {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        for uid in uids {
            if let Some(record) = fleet.get_mut(uid) {
                if record.owner == Some(conn) {
                    record.connected = false;
                    record.owner = None;
                }
            }
        }
    }

    /// The REST view of the fleet (`GET /api/v1/agents`).
    pub fn snapshot(&self) -> Vec<AgentView> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let mut agents: Vec<AgentView> = fleet
            .iter()
            .map(|(uid, record)| {
                let desired = self.configs.desired_for(record.description.as_ref());
                let matched = self.configs.matching_names(record.description.as_ref());
                let package_conflict = self.package_conflict(record);
                AgentView::from_record(uid, record, desired.as_ref(), matched, package_conflict)
            })
            .collect();
        agents.sort_by(|a, b| a.instance_uid.cmp(&b.instance_uid));
        agents
    }
}

/// The remote-config offer for one Agent, or `None` when the hash comparison says it already has
/// it — the "no redundant reconfiguration" goal in one place. Every matching Configuration is one
/// named entry; the Managed Process does its own merging (ADR-0012).
fn offer(record: &AgentRecord, desired: Option<&DesiredConfig>) -> Option<AgentRemoteConfig> {
    let desired = desired?;
    if record.capabilities & opamp::proto::AgentCapabilities::AcceptsRemoteConfig as u64 == 0 {
        return None;
    }
    let reported = record
        .remote_config_status
        .as_ref()
        .map(|s| s.last_remote_config_hash.as_slice())
        .unwrap_or_default();
    if reported == desired.hash.as_slice() {
        return None;
    }
    Some(AgentRemoteConfig {
        config: Some(AgentConfigMap {
            config_map: desired
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.name.clone(),
                        AgentConfigFile {
                            body: entry.body.clone().into_bytes(),
                            content_type: String::new(),
                            // The operator's role, verbatim (ADR-0016). Empty — the default —
                            // leaves the field unset, which is top-level configuration and what
                            // every Configuration predating that decision carries.
                            role: entry.role.clone(),
                        },
                    )
                })
                .collect(),
        }),
        config_hash: desired.hash.clone(),
    })
}

/// The version a reader of the fleet table wants: the release, without the commit the build came
/// from (ADR-0029).
///
/// A value that is not a version is returned as it stands. `service.version` is whatever an Agent
/// puts there, and a Foreign Agent numbers itself however its own project does — trimming a string
/// this Server does not understand would be inventing a version rather than showing one.
fn display_version(reported: &str) -> String {
    opamp::version::identity(reported)
        .unwrap_or(reported)
        .to_string()
}

/// One Agent as the REST API and the UI see it.
#[derive(Serialize, ToSchema)]
pub struct AgentView {
    pub instance_uid: String,
    pub service_name: String,
    /// The release the Agent reports — `MAJOR.MINOR.PATCH`, with the pre-release when it is not a
    /// release build (ADR-0029). This is what belongs in a column headed "Version"; the commit the
    /// build came from is [`service_build`](Self::service_build). A reported value that is not a
    /// version at all is passed through unchanged, since a Foreign Agent numbers itself however it
    /// likes.
    pub service_version: String,
    /// Exactly what the Agent reported, commit metadata and all — the answer to "which build is on
    /// that host", which is a question a fleet exists to answer (ADR-0029).
    pub service_build: String,
    /// The reported `os.description` (e.g. "Ubuntu 24.04.2 LTS"), falling back to `os.type`.
    pub os: String,
    /// Every reported identifying attribute — what a Selector can match on (ADR-0012).
    pub identifying_attributes: BTreeMap<String, String>,
    /// Every reported non-identifying attribute — Selectors match these too.
    pub non_identifying_attributes: BTreeMap<String, String>,
    /// The Configurations currently matching this Agent, in name order.
    pub matched_configurations: Vec<String>,
    /// Hex hash of the composed configuration this Agent should run; empty when nothing matches.
    pub desired_hash: String,
    /// The Capability Set this Agent declared, as capability names from the Baseline's
    /// `AgentCapabilities` (see docs/CONFORMANCE.md).
    pub capabilities: Vec<String>,
    /// The Agent's available components (top-level names, sorted); empty until reported.
    pub available_components: Vec<String>,
    /// The Agent's package installations (ADR-0015), in name order; empty until reported.
    pub packages: Vec<PackageStatusView>,
    /// Why this Agent is offered no package although it accepts them — two equally specific
    /// Selectors both reach it (ADR-0017). Absent when the targeting is unambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_conflict: Option<String>,
    /// What the Agent said about the *offer* rather than about a package it holds — an offer it
    /// refuses outright has no package status to carry the reason, and the Client's own Agent
    /// refusing a package it was not configured to take (ADR-0020) is exactly that case. Empty
    /// when the Agent has nothing to complain about.
    pub package_error: String,
    pub transport: String,
    pub connected: bool,
    pub healthy: bool,
    pub health_status: String,
    pub effective_config: String,
    pub remote_config_status: String,
    pub remote_config_error: String,
    pub in_sync: bool,
    pub sequence_num: u64,
    pub last_seen_ms: u64,
}

/// One package's installation state as the REST API and UI see it (ADR-0015).
#[derive(Serialize, ToSchema)]
pub struct PackageStatusView {
    pub name: String,
    /// The version the Agent has installed; empty if it has none.
    pub version: String,
    /// `Downloading`, `Installing`, `Installed`, `InstallPending`, or `InstallFailed`.
    pub status: String,
    /// The failure reason when `status` is `InstallFailed`.
    pub error: String,
    /// How far the artifact download has got, as a percentage. Present only while `Downloading`,
    /// and only when the download source stated a size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_percent: Option<f64>,
    /// The download's current rate in bytes per second. Present only while `Downloading`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_bytes_per_second: Option<f64>,
}

impl PackageStatusView {
    fn from_status(status: &opamp::proto::PackageStatus) -> Self {
        use opamp::proto::PackageStatusEnum as S;
        let name = match status.status {
            s if s == S::Installed as i32 => "Installed",
            s if s == S::Installing as i32 => "Installing",
            s if s == S::InstallPending as i32 => "InstallPending",
            s if s == S::InstallFailed as i32 => "InstallFailed",
            s if s == S::Downloading as i32 => "Downloading",
            _ => "Unknown",
        };
        // The Baseline carries these only with `Downloading`, and a percentage of zero means the
        // source never said how big the artifact is — not that nothing has arrived.
        let details = status.download_details.filter(|_| name == "Downloading");
        PackageStatusView {
            name: status.name.clone(),
            version: status.agent_has_version.clone(),
            status: name.to_string(),
            error: status.error_message.clone(),
            download_percent: details
                .map(|d| d.download_percent)
                .filter(|percent| *percent > 0.0),
            download_bytes_per_second: details.map(|d| d.download_bytes_per_second),
        }
    }
}

/// A declared capability bitmask as the names from the Baseline's `AgentCapabilities`. Undefined
/// bits are surfaced verbatim rather than dropped — a peer declaring them is worth seeing.
fn capability_names(mask: u64) -> Vec<String> {
    use opamp::proto::AgentCapabilities as C;
    const KNOWN: [(C, &str); 16] = [
        (C::ReportsStatus, "ReportsStatus"),
        (C::AcceptsRemoteConfig, "AcceptsRemoteConfig"),
        (C::ReportsEffectiveConfig, "ReportsEffectiveConfig"),
        (C::AcceptsPackages, "AcceptsPackages"),
        (C::ReportsPackageStatuses, "ReportsPackageStatuses"),
        (C::ReportsOwnTraces, "ReportsOwnTraces"),
        (C::ReportsOwnMetrics, "ReportsOwnMetrics"),
        (C::ReportsOwnLogs, "ReportsOwnLogs"),
        (
            C::AcceptsOpAmpConnectionSettings,
            "AcceptsOpAMPConnectionSettings",
        ),
        (
            C::AcceptsOtherConnectionSettings,
            "AcceptsOtherConnectionSettings",
        ),
        (C::AcceptsRestartCommand, "AcceptsRestartCommand"),
        (C::ReportsHealth, "ReportsHealth"),
        (C::ReportsRemoteConfig, "ReportsRemoteConfig"),
        (C::ReportsHeartbeat, "ReportsHeartbeat"),
        (C::ReportsAvailableComponents, "ReportsAvailableComponents"),
        (
            C::ReportsConnectionSettingsStatus,
            "ReportsConnectionSettingsStatus",
        ),
    ];
    let mut names = Vec::new();
    let mut undefined = mask;
    for (bit, name) in KNOWN {
        if mask & bit as u64 != 0 {
            names.push(name.to_string());
            undefined &= !(bit as u64);
        }
    }
    if undefined != 0 {
        names.push(format!("unknown bits 0x{undefined:x}"));
    }
    names
}

/// Reported attributes as the API shows them: string values as-is, other value kinds in their
/// debug form — the view is for reading, the wire keeps the typed original.
fn attr_map(attributes: &[KeyValue]) -> BTreeMap<String, String> {
    attributes
        .iter()
        .filter_map(|kv| {
            let value = kv.value.as_ref()?.value.as_ref()?;
            let text = match value {
                any_value::Value::StringValue(s) => s.clone(),
                other => format!("{other:?}"),
            };
            Some((kv.key.clone(), text))
        })
        .collect()
}

impl AgentView {
    fn from_record(
        uid: &InstanceUid,
        record: &AgentRecord,
        desired: Option<&DesiredConfig>,
        matched_configurations: Vec<String>,
        package_conflict: Option<String>,
    ) -> Self {
        let (identifying, non_identifying) = match &record.description {
            Some(d) => (
                attr_map(&d.identifying_attributes),
                attr_map(&d.non_identifying_attributes),
            ),
            None => (BTreeMap::new(), BTreeMap::new()),
        };
        let lookup = |map: &BTreeMap<String, String>, key: &str| -> String {
            map.get(key).cloned().unwrap_or_default()
        };
        let status = record.remote_config_status.as_ref();
        let status_name = match status.map(|s| s.status) {
            Some(s) if s == RemoteConfigStatuses::Applied as i32 => "APPLIED",
            Some(s) if s == RemoteConfigStatuses::Applying as i32 => "APPLYING",
            Some(s) if s == RemoteConfigStatuses::Failed as i32 => "FAILED",
            _ => "UNSET",
        };
        // In sync means: runs exactly the composed set — trivially true when nothing matches,
        // since an unmatched Agent is deliberately left alone (goal 9).
        let in_sync = match desired {
            None => true,
            Some(d) => {
                status.map(|s| s.last_remote_config_hash.as_slice()) == Some(d.hash.as_slice())
            }
        };
        // What the Agent said, and what a reader of a table wants out of it (ADR-0029).
        let service_build = lookup(&identifying, "service.version");
        AgentView {
            instance_uid: uid.to_string(),
            service_name: lookup(&identifying, "service.name"),
            service_version: display_version(&service_build),
            service_build,
            os: match lookup(&non_identifying, "os.description") {
                description if !description.is_empty() => description,
                _ => lookup(&non_identifying, "os.type"),
            },
            identifying_attributes: identifying,
            non_identifying_attributes: non_identifying,
            matched_configurations,
            desired_hash: desired.map(|d| hex::encode(&d.hash)).unwrap_or_default(),
            capabilities: capability_names(record.capabilities),
            available_components: record
                .available_components
                .as_ref()
                .map(|ac| {
                    let mut names: Vec<String> = ac.components.keys().cloned().collect();
                    names.sort_unstable();
                    names
                })
                .unwrap_or_default(),
            package_conflict,
            package_error: record
                .package_statuses
                .as_ref()
                .map(|s| s.error_message.clone())
                .unwrap_or_default(),
            packages: record
                .package_statuses
                .as_ref()
                .map(|s| {
                    let mut views: Vec<PackageStatusView> = s
                        .packages
                        .values()
                        .map(PackageStatusView::from_status)
                        .collect();
                    views.sort_by(|a, b| a.name.cmp(&b.name));
                    views
                })
                .unwrap_or_default(),
            transport: record.transport.as_str().to_string(),
            connected: record.connected,
            healthy: record.health.as_ref().map(|h| h.healthy).unwrap_or(false),
            health_status: record
                .health
                .as_ref()
                .map(|h| h.status.clone())
                .unwrap_or_default(),
            effective_config: record.effective_config.clone().unwrap_or_default(),
            remote_config_status: status_name.to_string(),
            remote_config_error: status.map(|s| s.error_message.clone()).unwrap_or_default(),
            in_sync,
            sequence_num: record.sequence_num,
            last_seen_ms: record.last_seen_ms,
        }
    }
}

/// The Baseline's command-only message: identity, capabilities, and the restart — nothing else.
fn restart_command(uid: &InstanceUid, capabilities: u64) -> ServerToAgent {
    ServerToAgent {
        instance_uid: uid.as_bytes().to_vec(),
        capabilities,
        command: Some(opamp::proto::ServerToAgentCommand {
            r#type: opamp::proto::CommandType::Restart as i32,
        }),
        ..Default::default()
    }
}

/// The `ServerToAgent` for a report the Server cannot make sense of.
pub fn bad_request(message: &str) -> ServerToAgent {
    ServerToAgent {
        capabilities: SERVER_CAPABILITIES,
        error_response: Some(ServerErrorResponse {
            r#type: ServerErrorResponseType::BadRequest as i32,
            error_message: message.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Renders a reported config map for the operator: single unnamed entry as-is, named entries with
/// a `# <name>` heading.
fn config_map_text(map: Option<&AgentConfigMap>) -> String {
    let Some(map) = map else {
        return String::new();
    };
    let mut entries: Vec<_> = map.config_map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(name, file)| {
            let body = String::from_utf8_lossy(&file.body);
            if name.is_empty() {
                body.into_owned()
            } else {
                format!("# {name}\n{body}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    /// ADR-0029: the fleet table shows the release, and the build stays reachable beside it. A
    /// Foreign Agent that numbers itself in its own way is shown as it reported.
    #[test]
    fn the_displayed_version_drops_the_commit_and_keeps_the_pre_release() {
        assert_eq!(super::display_version("0.1.1+799e36a"), "0.1.1");
        assert_eq!(super::display_version("0.1.1-dev+799e36a"), "0.1.1-dev");
        assert_eq!(super::display_version("0.1.1"), "0.1.1");
        // Not a version this Server understands — shown rather than trimmed into something else.
        assert_eq!(super::display_version("v2.9-nightly"), "v2.9-nightly");
        assert_eq!(super::display_version(""), "");
    }

    use super::*;

    #[test]
    fn capability_names_decode_known_bits_and_surface_undefined_ones() {
        use opamp::proto::AgentCapabilities as C;
        assert!(capability_names(0).is_empty());
        assert_eq!(
            capability_names(C::ReportsStatus as u64 | C::ReportsHealth as u64),
            ["ReportsStatus", "ReportsHealth"]
        );
        let with_undefined = capability_names(C::ReportsStatus as u64 | 1 << 60);
        assert_eq!(
            with_undefined,
            ["ReportsStatus", "unknown bits 0x1000000000000000"]
        );
    }

    /// What an operator sees while a package is on the wire. A status the view does not know
    /// reads as "Unknown", which is worse than useless during a rollout — so `Downloading` and
    /// its progress are part of the view, and the progress belongs to that status alone.
    #[test]
    fn the_package_view_shows_a_download_in_progress() {
        use opamp::proto::{PackageDownloadDetails, PackageStatus, PackageStatusEnum};

        let downloading = PackageStatusView::from_status(&PackageStatus {
            name: "otelcol".to_string(),
            status: PackageStatusEnum::Downloading as i32,
            download_details: Some(PackageDownloadDetails {
                download_percent: 42.5,
                download_bytes_per_second: 1_048_576.0,
            }),
            ..Default::default()
        });
        assert_eq!(downloading.status, "Downloading");
        assert_eq!(downloading.download_percent, Some(42.5));
        assert_eq!(downloading.download_bytes_per_second, Some(1_048_576.0));

        // A percentage is only meaningful when the source stated a size; zero means it did not.
        let sizeless = PackageStatusView::from_status(&PackageStatus {
            name: "otelcol".to_string(),
            status: PackageStatusEnum::Downloading as i32,
            download_details: Some(PackageDownloadDetails {
                download_percent: 0.0,
                download_bytes_per_second: 2048.0,
            }),
            ..Default::default()
        });
        assert_eq!(sizeless.download_percent, None);
        assert_eq!(sizeless.download_bytes_per_second, Some(2048.0));

        // Every other status carries no progress, whatever the Agent sent.
        let installing = PackageStatusView::from_status(&PackageStatus {
            name: "otelcol".to_string(),
            status: PackageStatusEnum::Installing as i32,
            download_details: Some(PackageDownloadDetails {
                download_percent: 99.0,
                download_bytes_per_second: 1.0,
            }),
            ..Default::default()
        });
        assert_eq!(installing.status, "Installing");
        assert_eq!(installing.download_percent, None);
        assert_eq!(installing.download_bytes_per_second, None);
    }
}
