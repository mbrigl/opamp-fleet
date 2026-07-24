//! In-memory fleet state and the OpAMP control loop, keyed by Instance UID — never by the
//! connection that carried a message (ADR-0009).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use opamp::attributes;
use opamp::proto::{
    any_value, AgentConfigMap, AgentConfigObject, AgentDescription, AgentIdentification,
    AgentRemoteConfig, AgentToServer, AgentToServerFlags, AvailableComponents, ComponentHealth,
    ConnectionSettingsOffers, ConnectionSettingsStatus, Header, Headers, KeyValue,
    OpAmpConnectionSettings, PackageStatuses, PackagesAvailable, RemoteConfigStatus,
    RemoteConfigStatuses, ServerCapabilities, ServerErrorResponse, ServerErrorResponseType,
};
use opamp::uid::InstanceUid;
use prost::Message as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tracing::{info, warn};
use utoipa::ToSchema;

use crate::ca::ClientCa;
use crate::config::ConnectionOfferConfig;

/// The package upload limit in force when nothing configures one — roomy, because a real agent
/// binary is (see `server.toml`, `max_package_size_bytes`).
pub const DEFAULT_MAX_PACKAGE_SIZE: usize = 1024 * 1024 * 1024; // 1 GiB

/// The whole-store limit in force when nothing configures one: sixteen per-artifact budgets, room
/// for a real package set across platforms with rollback copies, while still bounding the disk a
/// caller can fill by uploading under many names (see `server.toml`, `max_total_package_bytes`).
pub const DEFAULT_MAX_TOTAL_PACKAGE_SIZE: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB

/// The Capability Set this Server declares (see docs/CONFORMANCE.md).
pub const SERVER_CAPABILITIES: u64 = ServerCapabilities::AcceptsStatus as u64
    | ServerCapabilities::OffersRemoteConfig as u64
    | ServerCapabilities::AcceptsEffectiveConfig as u64;

/// Identifies one WebSocket connection for the duplicate detection the Baseline asks of the
/// Server. Never a routing key — Agents are routed by `instance_uid` alone (ADR-0009); this only
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
    /// The outcome of the last connection-settings offer this Agent reported (ADR-0018); its
    /// hash is what gates re-offering.
    pub connection_settings_status: Option<ConnectionSettingsStatus>,
    /// The package statuses this Agent last reported (ADR-0019); the
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

    /// No Agent of that identity is known.
    UnknownAgent,
    /// No Agent of that identity is known.
    UnknownAgent,
/// The result of processing one `AgentToServer`: the reply to send back on the same transport, and
/// what the transport layer needs to know for its own bookkeeping.
pub struct Processed {
    pub reply: ServerToAgent,
    /// The identity the Agent goes by *after* this message (it may have been reassigned).
    pub uid: Option<InstanceUid>,
    /// The Agent said goodbye; a WebSocket loop drops it from its connection-local set.
    pub disconnected: bool,
}

/// The one `OpAMPConnectionSettings` this Server offers (ADR-0018), precompiled from the
/// `[connection_offer]` section with the hash that gates its delivery.
pub struct ConnectionOffer {
    settings: OpAmpConnectionSettings,
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
    }
}

/// The package store plus the base URL each `download_url` is built from (ADR-0019).
pub struct PackageOffering {
    store: PackageStore,
    download_base: String,
}

impl PackageOffering {
    /// `download_base` is the advertised absolute URL, or empty for a path the Client resolves
    /// against its own endpoint — which is the Agent plane, where the download is served
    /// (ADR-0012). It sits outside Admission (ADR-0017): the artifact's content hash and signature
    /// are what protect it, so no credential rides it.
            store,
            download_base,
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
    /// The connection settings offered to the fleet (ADR-0018); `None` offers nothing and leaves
    /// `OffersConnectionSettings` undeclared.
    connection_offer: Option<ConnectionOffer>,
    /// The packages offered to the fleet (ADR-0019); `None` offers nothing and leaves
    /// `OffersPackages` undeclared.
    packages: Option<PackageOffering>,
    /// The authority that signs Agent CSRs (ADR-0017); `None` signs nothing and leaves
    /// `AcceptsConnectionSettingsRequest` undeclared.
    client_ca: Option<ClientCa>,
    /// The message size limit both transports enforce, in each direction (the Baseline's MUST).
    max_message_size: usize,
    /// The largest package artifact the REST API accepts on upload (ADR-0019) — a program, not a
    /// message, so it is bounded separately and far more generously.
    max_package_size: usize,
    /// The total size of all stored artifacts the REST API keeps before it refuses a new upload
    /// (ADR-0019): what bounds the store — and so the disk — against many uploads under distinct
    /// names, where `max_package_size` bounds only one.
    max_total_package_bytes: u64,
}

impl AppState {
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
            configs,
            push: watch::channel(0).0,
            next_conn: AtomicU64::new(1),
            connection_offer: None,
            packages: None,
            client_ca: None,
            max_message_size: opamp::frame::DEFAULT_MAX_MESSAGE_SIZE,
            max_package_size: DEFAULT_MAX_PACKAGE_SIZE,
            max_total_package_bytes: DEFAULT_MAX_TOTAL_PACKAGE_SIZE,
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

    /// Sets the largest package artifact the REST API accepts on upload (ADR-0019).
    #[must_use]
    pub fn with_max_package_size(mut self, limit: usize) -> Self {
        self.max_package_size = limit;
        self
    }

    /// The package upload limit in force, for the REST API's package route.
    pub fn max_package_size(&self) -> usize {
        self.max_package_size
    }

    /// Sets the whole-store size limit the REST API enforces before accepting a new upload
    /// (ADR-0019).
    #[must_use]
    pub fn with_max_total_package_bytes(mut self, limit: u64) -> Self {
        self.max_total_package_bytes = limit;
        self
    }

    /// The whole-store size limit in force, for the REST API's upload route.
    pub fn max_total_package_bytes(&self) -> u64 {
        self.max_total_package_bytes
    }

    /// The total bytes of stored package artifacts right now, or `0` when package delivery is not
    /// configured. What the upload route checks against [`max_total_package_bytes`](Self::max_total_package_bytes).
    pub fn stored_package_bytes(&self) -> u64 {
        self.packages
            .as_ref()
            .map_or(0, |p| p.store().total_bytes())
    }

    /// Arms the connection-settings offer (ADR-0018); with it the Server declares
    /// `OffersConnectionSettings`.
    #[must_use]
    pub fn with_connection_offer(mut self, offer: Option<ConnectionOffer>) -> Self {
        self.connection_offer = offer;
        self
    }

    /// Arms the CSR flow (ADR-0017); with it the Server declares
    /// `AcceptsConnectionSettingsRequest` and signs the requests Agents send.
    #[must_use]
    pub fn with_client_ca(mut self, client_ca: Option<ClientCa>) -> Self {
        self.client_ca = client_ca;
        self
    }

    /// Arms package delivery (ADR-0019); with a non-empty store the Server declares
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
    /// while there is anything to offer and `OffersPackages` / `AcceptsPackagesStatus`
    /// while a non-empty package store is armed — an undeclared capability is never exercised, a
    /// declared one never hollow.
    fn capabilities(&self) -> u64 {
        let mut caps = SERVER_CAPABILITIES;
        // All three ways a `ConnectionSettingsOffers` leaves this Server (ADR-0018 clause 3): the
        // standing `[connection_offer]`, the own-telemetry destinations of `[telemetry_offer]`, and
        // the certificate a `[client_ca]` issues in answer to a CSR — which travels as an ordinary
        // offer. Keying the bit on the first alone left the other two exercising a capability this
        // Server had not declared, which is exactly what the sentence above promises never happens.
        if self.connection_offer.is_some()
            || !self.telemetry_offer.is_empty()
            || self.client_ca.is_some()
        {
            caps |= ServerCapabilities::OffersConnectionSettings as u64;
        }
        if self.packages.as_ref().is_some_and(|p| !p.store.is_empty()) {
            caps |= ServerCapabilities::OffersPackages as u64
                | ServerCapabilities::AcceptsPackagesStatus as u64;
        }
        if self.client_ca.is_some() {
            caps |= ServerCapabilities::AcceptsConnectionSettingsRequest as u64;
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

    pub fn save_configuration(
        &self,
        name: &str,
        revision: Revision,
    ) -> Result<Configuration, String> {
        Ok(config)
    }

        &self,
        let mut fleet = self.fleet.lock().expect("fleet lock");
        self.push.send_modify(|rev| *rev += 1);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        }
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
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

        let fleet = self.fleet.lock().expect("fleet lock");
        let mut fleet = self.fleet.lock().expect("fleet lock");
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
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

    pub fn delete_configuration(&self, name: &str) -> Result<bool, String> {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let deleted = self.configs.delete(name)?;
        if deleted {
            self.push.send_modify(|rev| *rev += 1);
        }
        Ok(deleted)
    }

        let fleet = self.fleet.lock().expect("fleet lock");
    /// The control loop for one report, shared by both transports (ADR-0012): update what we know,
    /// then answer with what the Agent still lacks — the config offer gated by the hash comparison.
    /// `conn` identifies the WebSocket connection that carried the report; `None` for plain HTTP.
    ///
    /// The reported `instance_uid` is taken at face value: admission proved fleet membership, not
    /// which Agent is speaking, so within an admitted fleet a report's identity is self-asserted and
    /// not authorized against any other Agent (ADR-0017). The plain-HTTP path in particular offers
    /// nothing to tell two pollers apart; the WebSocket duplicate-`instance_uid` rekey below is
    /// collision handling, not authorization.
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
                uid: None,
                disconnected: false,
            };
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

        // The Agent asked to be issued a client certificate (ADR-0017). Signing it here, on the
        // connection it arrived over, is the whole of the approval: admission already required
        // every proof this endpoint asks of any message, which is what the Baseline's flow means
        // by awaiting one.
        let issued = match msg
            .connection_settings_request
            .as_ref()
            .and_then(|request| request.opamp.as_ref())
            .and_then(|opamp| opamp.certificate_request.as_ref())
        {
            None => None,
            Some(request) => {
                let outcome = match &self.client_ca {
                    // The Baseline's MUST when the Server cannot act on the request. An Agent
                    // reaching here ignored the undeclared capability, so it is a client error.
                    None => Err("this Server issues no client certificates".to_string()),
                    Some(ca) => String::from_utf8(request.csr.clone())
                        .map_err(|_| "the certificate signing request is not PEM".to_string())
                        .and_then(|csr| ca.sign(&csr)),
                };
                match outcome {
                    Ok(cert) => {
                        info!(agent = %uid, "issued a client certificate");
                        Some(TlsCertificate {
                            cert: cert.into_bytes(),
                            // The Agent generated its own key and keeps it — the point of the CSR
                            // flow — so the Server has nothing to put here and must not invent it.
                            private_key: Vec::new(),
                            ..Default::default()
                        })
                    }
                    Err(e) => {
                        warn!(agent = %uid, error = %e, "refused a certificate signing request");
                        return Processed {
                            reply: bad_request(&e),
                            uid: Some(uid),
                            disconnected: false,
                        };
                    }
                }
            }
        };

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

        let remote_config = if disconnected {
            None
        } else {
            offer(record, desired.as_ref())
        };

        // The connection-settings offer (ADR-0018), gated the same way: by capability and by
        // the hash the Agent last reported — the Baseline's own "compare and include" MUST.
        let connection_settings = if disconnected {
            None
        } else {
            self.settings_offer(record, issued)
        };

        // The package offer (ADR-0019), gated by capability and the reported
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

    ///
    fn packages_offer(&self, record: &AgentRecord) -> Option<PackagesAvailable> {
        let offering = self.packages.as_ref()?;
        if record.capabilities & opamp::proto::AgentCapabilities::AcceptsPackages as u64 == 0 {
            return None;
        }
        let reported = record
            .package_statuses
            .as_ref()
            .map(|s| s.server_provided_all_packages_hash.as_slice())
            .unwrap_or_default();
            return None;
        }
    }

    fn package_conflict(&self, record: &AgentRecord) -> Option<String> {
        if record.capabilities & opamp::proto::AgentCapabilities::AcceptsPackages as u64 == 0 {
            return None;
        }
    }

    /// The connection-settings offer for one Agent, or `None` when it cannot accept one or its
    /// reported hash says it already runs (or refused) exactly this offer.
    ///
    /// `issued` is a certificate just signed for this Agent (ADR-0017). It overrides the hash gate
    /// — the Agent asked for it in this very exchange — and rides whatever else the standing offer
    /// carries, so one message can hand over a certificate and the endpoint or credential that go
    /// with it, exactly as the Baseline describes.
    fn settings_offer(
        &self,
        record: &AgentRecord,
        issued: Option<TlsCertificate>,
    ) -> Option<ConnectionSettingsOffers> {
        if let Some(certificate) = issued {
            let mut settings = self
                .connection_offer
                .as_ref()
                .map(|offer| offer.settings.clone())
                .unwrap_or_default();
            settings.certificate = Some(certificate);
                opamp: Some(settings),
                ..Default::default()
            // Its own hash, over the settings as sent: the standing offer's would tell the Agent
            // nothing changed, and it would never adopt the certificate.
        }
                return None;
            }
            opamp: Some(offer.settings.clone()),
            ..Default::default()
    }

    pub fn offer_for(&self, uid: &InstanceUid) -> Option<ServerToAgent> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get(uid)?;
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

            .packages
            .as_ref()
            .ok_or("package delivery is not configured on this Server")?
        let fleet = self.fleet.lock().expect("fleet lock");
        let fleet = self.fleet.lock().expect("fleet lock");
        platform: &crate::packages::Platform,
        platform: &crate::packages::Platform,
        platform: &crate::packages::Platform,
        staged: &std::path::Path,
    ) -> Result<(), String> {
        Ok(())
    }

    pub fn package_staging_path(
        &self,
        platform: &crate::packages::Platform,
    ) -> Result<std::path::PathBuf, String> {
    }

        &self,
        platform: &crate::packages::Platform,
        content_hash: Vec<u8>,
        source: crate::packages::Source,
    ) -> Result<(), String> {
        Ok(())
    }

        &self,
        platform: &crate::packages::Platform,
    ) -> Result<bool, String> {
        if deleted {
        }
        Ok(deleted)
    }

        let mut fleet = self.fleet.lock().expect("fleet lock");
        if deleted {
            self.push.send_modify(|rev| *rev += 1);
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
                let package_conflict = self.package_conflict(record);
            })
            .collect();
        agents.sort_by(|a, b| a.instance_uid.cmp(&b.instance_uid));
        agents
    }
}

/// The remote-config offer for one Agent, or `None` when the hash comparison says it already has
/// named entry; the Managed Process does its own merging (ADR-0016).
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
                        AgentConfigObject {
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
/// from (ADR-0013).
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
    /// release build (ADR-0013). This is what belongs in a column headed "Version"; the commit the
    /// build came from is [`service_build`](Self::service_build). A reported value that is not a
    /// version at all is passed through unchanged, since a Foreign Agent numbers itself however it
    /// likes.
    pub service_version: String,
    /// Exactly what the Agent reported, commit metadata and all — the answer to "which build is on
    /// that host", which is a question a fleet exists to answer (ADR-0013).
    pub service_build: String,
    /// The reported `os.description` (e.g. "Ubuntu 24.04.2 LTS"), falling back to `os.type`.
    pub os: String,
    /// Every reported identifying attribute — what a Selector can match on (ADR-0016).
    pub identifying_attributes: BTreeMap<String, String>,
    /// Every reported non-identifying attribute — Selectors match these too.
    pub non_identifying_attributes: BTreeMap<String, String>,
    pub matched_configurations: Vec<String>,
    pub desired_hash: String,
    /// The Capability Set this Agent declared, as capability names from the Baseline's
    /// `AgentCapabilities` (see docs/CONFORMANCE.md).
    pub capabilities: Vec<String>,
    /// The Agent's available components (top-level names, sorted); empty until reported.
    pub available_components: Vec<String>,
    /// The Agent's package installations (ADR-0019), in name order; empty until reported.
    pub packages: Vec<PackageStatusView>,
    /// Why this Agent is offered no package although it accepts them — two equally specific
    /// Selectors both reach it (ADR-0020). Absent when the targeting is unambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_conflict: Option<String>,
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

#[derive(Serialize, ToSchema)]
#[derive(Serialize, ToSchema)]
/// One package's installation state as the REST API and UI see it (ADR-0019).
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

fn attr_map(attributes: &[KeyValue]) -> BTreeMap<String, String> {
    attributes
        .iter()
        .filter_map(|kv| {
            let value = kv.value.as_ref()?.value.as_ref()?;
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
        // What the Agent said, and what a reader of a table wants out of it (ADR-0013).
        let service_build = lookup(&identifying, attributes::SERVICE_VERSION);
        AgentView {
            instance_uid: uid.to_string(),
            service_name: lookup(&identifying, attributes::SERVICE_NAME),
            service_instance_name: lookup(&non_identifying, attributes::SERVICE_INSTANCE_NAME),
            service_version: display_version(&service_build),
            service_build,
            os: match lookup(&non_identifying, attributes::OS_DESCRIPTION) {
                description if !description.is_empty() => description,
                _ => lookup(&non_identifying, attributes::OS_TYPE),
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
                .package_statuses
                .as_ref()
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

    ServerToAgent {
        capabilities: SERVER_CAPABILITIES,
        error_response: Some(ServerErrorResponse {
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
            instance_uid: uid.as_bytes().to_vec(),
            instance_uid: uid.as_bytes().to_vec(),
                    body: "receivers: {}\n".to_string(),
                    role: String::new(),
                    service_name: String::new(),
                },
            )
    /// ADR-0013: the fleet table shows the release, and the build stays reachable beside it. A
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
