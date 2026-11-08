//! In-memory fleet state and the OpAMP control loop, keyed by Instance UID — never by the
//! connection that carried a message (ADR-0016).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use opamp::attributes;
use opamp::proto::{
    any_value, AgentConfigMap, AgentConfigObject, AgentDescription, AgentIdentification,
    AgentRemoteConfig, AgentToServer, AgentToServerFlags, AvailableComponents, ComponentHealth,
    ConnectionSettingsOffers, ConnectionSettingsStatus, KeyValue, OpAmpConnectionSettings,
    PackageStatuses, PackagesAvailable, RemoteConfigStatus, RemoteConfigStatuses,
    ServerCapabilities, ServerErrorResponse, ServerErrorResponseType, ServerToAgent,
    ServerToAgentFlags, TelemetryConnectionSettings, TlsCertificate,
};
use opamp::uid::InstanceUid;
use prost::Message as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::agent_store::{AgentStore, Creation, PersistedAgent, Pin, Presence};
use crate::audit::{Audit, Entry};
use crate::configs::{ConfigBackend, ConfigStore, Configuration, DesiredConfig, Removal, Revision};
use crate::deployments::{self, deployment_for, Deployment, DeploymentError, DeploymentStore};
use crate::enrolment::{DecisionError, Enrolment, Recorder, Registrar};
use crate::labels::{LabelError, Sources};
use crate::packages::{InstalledVersions, PackageId, PackageStore, Platform, SignatureState};
use crate::revocation::{CertId, Facts, Presented, Revocations, Signed, SpeaksFor, SubjectValues};

/// The package upload limit in force when nothing configures one — roomy, because a real agent
/// binary is (see `server.toml`, `max_package_size_bytes`).
pub const DEFAULT_MAX_PACKAGE_SIZE: usize = 1024 * 1024 * 1024; // 1 GiB

/// The whole-store limit in force when nothing configures one: sixteen per-artifact budgets, room
/// for a real package set across platforms with rollback copies, while still bounding the disk a
/// caller can fill by uploading under many names (see `server.toml`, `max_total_package_bytes`).
pub const DEFAULT_MAX_TOTAL_PACKAGE_SIZE: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB

/// Three times the Baseline's own default heartbeat of 30 seconds (ADR-0030).
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(90);

/// The most Agent records the fleet holds when nothing configures a ceiling (see `server.toml`,
/// `max_agents`). Generous enough that a real fleet never meets it, low enough that the in-memory
/// map and the per-Agent files it mirrors to disk stay bounded when an unauthenticated endpoint is
/// flooded with fresh, self-asserted UIDs (ADR-0026).
pub const DEFAULT_MAX_AGENTS: usize = 100_000;

/// The heartbeat interval an Agent is held to where this Server offers none: the OpAMP default,
/// 30 seconds.
const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(30);

/// The Capability Set this Server declares (see docs/CONFORMANCE.md).
pub const SERVER_CAPABILITIES: u64 = ServerCapabilities::AcceptsStatus as u64
    | ServerCapabilities::OffersRemoteConfig as u64
    | ServerCapabilities::AcceptsEffectiveConfig as u64;

/// Identifies one WebSocket connection for the duplicate detection the Baseline asks of the
/// Server. Never a routing key — Agents are routed by `instance_uid` alone (ADR-0016); this only
/// answers "is this identity already alive on *another* connection?".
pub type ConnId = u64;

/// Which transport a report arrived on. Recorded for the operator; it never keys any state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Http,
    WebSocket,
}

impl Transport {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Transport::Http => "http",
            Transport::WebSocket => "websocket",
        }
    }

    /// The inverse of [`as_str`](Self::as_str). Anything but `websocket` reads as HTTP: the
    /// transport is recorded for the operator and keys nothing, so an unknown spelling is not
    /// worth refusing a stored record over.
    pub(crate) fn parse(text: &str) -> Self {
        match text {
            "websocket" => Transport::WebSocket,
            _ => Transport::Http,
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
    /// The component hash the full map was last demanded for on the Agent's current connection —
    /// runtime-only. An Agent that answers the demand without the map is not asked again for the
    /// same hash, or every answer would draw the next demand.
    pub components_demanded: Option<Vec<u8>>,
    /// The outcome of the last connection-settings offer this Agent reported (ADR-0013); its
    /// hash is what gates re-offering.
    pub connection_settings_status: Option<ConnectionSettingsStatus>,
    /// The package statuses this Agent last reported (ADR-0034); the
    /// `server_provided_all_packages_hash` inside is what gates re-offering packages.
    pub package_statuses: Option<PackageStatuses>,
    /// The WebSocket connection currently carrying this Agent; `None` for plain HTTP, whose
    /// polling is stateless. Only the owning connection may mark the Agent disconnected, and a
    /// report from a *different* live connection is the duplicate the Baseline wants detected.
    pub owner: Option<ConnId>,
    /// When the connection that carries it last carried a report of it, in milliseconds since the
    /// epoch — runtime-only. An older connection of its host takes it back once this is a
    /// heartbeat interval old
    /// (ADR-0025
    /// clause 8).
    pub carried_ms: u64,
    /// Whether this Server heard the Agent report since it started — runtime-only. What it holds
    /// of one it did not is what it loaded, which on a shared store another instance may have
    /// written since.
    pub heard: bool,
    /// The operator's labels for this Agent (ADR-0030), part of its persisted record (ADR-0030
    /// clause 21): they follow a re-key and leave with a forget.
    pub labels: BTreeMap<String, String>,
    /// The Host this Agent last arrived over (ADR-0030 clause 16): its own on a direct connection,
    /// the Gateway's behind one. For the fleet view and label inheritance; it grants nothing.
    pub host: Option<String>,
    /// Where the Host and the Client Agent place this Agent — derived, never persisted.
    pub placement: Placement,
    /// The instance that holds it, as a shared store read it
    /// (ADR-0025
    /// clause 13); `None` for what this Server holds in memory.
    pub holder: Option<crate::agent_store::Holder>,
    /// What the operator's last rollout released to this Agent (ADR-0033 clause 6). **Every offer
    /// is composed from this**; matching only proposes. Written by the rollout acts, persisted like
    /// `restart_pending` — operator intent that survives a restart (ADR-0030). `None` is what it
    /// says: nothing has been rolled out.
    pub assignment: Option<Assignment>,
}

/// One Agent's assignment (ADR-0033 clause 6): one release, pinned as of the press that wrote it
/// and replaced whole by the next.
///
/// The Deployment is carried alongside the release rather than derived from it, because a
/// Deployment re-aimed or edited after the act must not change what an Agent was given, nor what
/// the fleet view says it was given through. The signature a package offer travels with is the
/// entry's own (ADR-0034).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    /// The Deployment the release went through.
    pub deployment: String,
    /// The Package pinned, if any — at most one, because an Agent belongs to at most one
    /// Deployment and a Deployment holds one Package per Agent type. The Baseline's "one top-level
    /// package" is structural here rather than enforced by a rule at the write.
    pub package: Option<PackageId>,
    /// Every Configuration released: its name → the content hash of the revision released, one
    /// of the `ConfigStore`'s retained revisions.
    pub configurations: BTreeMap<String, String>,
}

/// The configuration map of an Agent nothing was rolled out to.
static NO_CONFIGURATIONS: BTreeMap<String, String> = BTreeMap::new();

/// The Agents one Client presents (ADR-0030 clause 19), as far as the Server can tell them apart.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    /// On a direct connection: the Agents arriving over the same Host.
    Host(String),
    /// Behind a Gateway: the Agents arriving over the same Gateway Host that report the same
    /// machine — `host.id`, else `host.name` ([`crate::labels::machine`]).
    Behind { gateway: String, machine: String },
}

/// What the Host an Agent arrived over and its Client Agent add to its effective description
/// (ADR-0030 clauses 18 and 19). Derived from the records and the host register whenever either
/// changes; never persisted.
#[derive(Clone, Debug, Default)]
pub struct Placement {
    /// The `fleet.host.*` values of the Host it arrived over directly; empty behind a Gateway.
    pub host_values: BTreeMap<String, String>,
    /// The Client it belongs to, when the Server can tell.
    pub group: Option<Group>,
    /// Its Client Agent — itself, for the Client Agent — when its group holds exactly one.
    pub client_agent: Option<InstanceUid>,
    /// The labels of its Client Agent; empty for the Client Agent itself.
    pub inherited: BTreeMap<String, String>,
    /// Its group holds more than one Client Agent, so nothing is inherited.
    pub ambiguous: bool,
}

impl AgentRecord {
    /// What a Selector is matched against (ADR-0030 clause 18): the Host's values, what the Agent
    /// reported, its own labels and its Client Agent's, each key from the first that has it.
    ///
    /// Borrowed when it adds nothing to the report, which is the overwhelming majority of Agents —
    /// labelling should cost the fleet view nothing on the hosts nobody has labelled.
    pub fn effective_description(&self) -> Option<Cow<'_, AgentDescription>> {
        let sources = self.sources();
        if sources.adds_nothing() {
            return self.description.as_ref().map(Cow::Borrowed);
        }
        crate::labels::effective_description(&sources).map(Cow::Owned)
    }

    fn sources(&self) -> Sources<'_> {
        Sources {
            host: &self.placement.host_values,
            reported: self.description.as_ref(),
            own: &self.labels,
            inherited: &self.placement.inherited,
        }
    }

    /// The Package rolled out to this Agent, if any.
    fn assigned_package(&self) -> Option<&PackageId> {
        self.assignment.as_ref().and_then(|a| a.package.as_ref())
    }

    /// The Configurations rolled out to this Agent, name → pinned revision hash; empty when
    /// nothing was.
    fn assigned_configurations(&self) -> &BTreeMap<String, String> {
        self.assignment
            .as_ref()
            .map_or(&NO_CONFIGURATIONS, |a| &a.configurations)
    }

    /// What this Agent last reported as installed, per package name (ADR-0033): the versions
    /// the upgrade test measures a Package against. A package reported with an empty
    /// `agent_has_version` — offered, not yet installed — carries no version and is left out, so
    /// it reads as "nothing installed under that name" rather than as an unorderable value.
    fn installed_package_versions(&self) -> InstalledVersions {
        self.package_statuses
            .iter()
            .flat_map(|statuses| statuses.packages.iter())
            .filter(|(_, status)| !status.agent_has_version.is_empty())
            .map(|(name, status)| (name.clone(), status.agent_has_version.clone()))
            .collect()
    }

    /// What of this record survives a restart (ADR-0030): everything report-derived or
    /// operator-queued, never what a live connection knows.
    /// Where it is connected, as a presence batch writes it.
    /// What its derived values come from
    /// (ADR-0031
    /// clause 8).
    fn source<'a>(&'a self, uid: &'a InstanceUid) -> crate::listing::Source<'a> {
        crate::listing::Source {
            uid,
            description: self.description.as_ref(),
            health: self.health.as_ref(),
            remote_config_status: self.remote_config_status.as_ref(),
            package_statuses: self.package_statuses.as_ref(),
            capabilities: self.capabilities,
            transport: self.transport,
        }
    }

    fn presence(&self) -> Presence {
        Presence {
            connected: self.connected,
            last_seen_ms: self.last_seen_ms,
        }
    }

    fn to_persisted(&self) -> PersistedAgent {
        PersistedAgent {
            sequence_num: self.sequence_num,
            capabilities: self.capabilities,
            description: self.description.clone(),
            health: self.health.clone(),
            effective_config: self.effective_config.clone(),
            remote_config_status: self.remote_config_status.clone(),
            connection_settings_status: self.connection_settings_status.clone(),
            package_statuses: self.package_statuses.clone(),
            available_components: self.available_components.clone(),
            transport: self.transport,
            last_seen_ms: self.last_seen_ms,
            restart_pending: self.restart_pending,
            assignment: self.assignment.clone(),
            labels: self.labels.clone(),
            host: self.host.clone(),
        }
    }

    /// A restored record is **disconnected with no owning connection** until live evidence says
    /// otherwise — connectedness is runtime-only (ADR-0030). Its placement is derived once the
    /// fleet is whole.
    fn from_persisted(persisted: PersistedAgent) -> AgentRecord {
        AgentRecord {
            sequence_num: persisted.sequence_num,
            capabilities: persisted.capabilities,
            description: persisted.description,
            health: persisted.health,
            effective_config: persisted.effective_config,
            remote_config_status: persisted.remote_config_status,
            transport: persisted.transport,
            connected: false,
            last_seen_ms: persisted.last_seen_ms,
            restart_pending: persisted.restart_pending,
            available_components: persisted.available_components,
            components_demanded: None,
            connection_settings_status: persisted.connection_settings_status,
            package_statuses: persisted.package_statuses,
            owner: None,
            carried_ms: 0,
            heard: false,
            labels: persisted.labels,
            host: persisted.host,
            placement: Placement::default(),
            holder: None,
            assignment: persisted.assignment,
        }
    }
}

/// Why a restart request was refused (`POST /api/v1/agents/{uid}/restart`).
pub enum RestartError {
    /// No Agent of that identity is known.
    UnknownAgent,
    /// The Agent does not declare `AcceptsRestartCommand` — capability negotiation is binding,
    /// so the Server refuses rather than sending a command the Agent would ignore.
    NoCapability,
    /// The request could not be written.
    Storage(String),
}

/// Why forgetting an Agent was refused (`DELETE /api/v1/agents/{uid}`, ADR-0030).
pub enum ForgetError {
    /// No Agent of that identity is known.
    UnknownAgent,
    /// The Agent is still reporting — connected, and not stale. Forgetting it would drop the
    /// hashes that stop the Server re-offering, so its next exchange would re-apply its
    /// configuration, which for a managed Agent restarts the Managed Process.
    StillReporting,
    /// The store could not be read or written.
    Storage(String),
}

/// Why a rollout act (ADR-0033 clause 9) was refused.
#[derive(Debug)]
pub enum RolloutError {
    /// No Agent of that identity is known.
    UnknownAgent,
    /// The named Deployment does not exist (or package delivery, which arms the Deployments, is
    /// not configured).
    UnknownResource(String),
    /// The act cannot release here: the Agent is in no Deployment, in a conflict, or in another
    /// Deployment than the one named; or the Deployment holds nothing, or a Package with an
    /// unsigned entry.
    NotApplicable(String),
    /// The act could not be persisted.
    Storage(String),
}

/// Why deleting a Configuration was refused (ADR-0033 clause 12).
#[derive(Debug)]
pub enum DeleteError {
    /// An Agent's assignment pins it; the message says how many.
    Pinned(String),
    /// The store could not be written.
    Storage(String),
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

/// The one `OpAMPConnectionSettings` this Server offers (ADR-0013), precompiled from the
/// `[connection_offer]` section with the hash that gates its delivery.
pub struct ConnectionOffer {
    settings: OpAmpConnectionSettings,
}

/// The own-telemetry destinations this Server offers (ADR-0017), precompiled from
/// `[telemetry_offer]`. Part of the same `ConnectionSettingsOffers` message the OpAMP settings
/// ride, and hashed with them: one offer, one hash, one acknowledgement.
///
/// A field here is `Some` for every signal the section mentions, **including one it withdraws** —
/// a destination whose endpoint is empty (ADR-0017). That is why a withdrawal counts as something
/// to offer in [`is_empty`](Self::is_empty): it has to reach the Agent to take effect, and a
/// Server that has it to say declares `OffersConnectionSettings` for it like any other offer.
#[derive(Default, Clone)]
pub struct TelemetryOffer {
    pub own_metrics: Option<TelemetryConnectionSettings>,
    pub own_traces: Option<TelemetryConnectionSettings>,
    pub own_logs: Option<TelemetryConnectionSettings>,
}

impl TelemetryOffer {
    fn is_empty(&self) -> bool {
        self.own_metrics.is_none() && self.own_traces.is_none() && self.own_logs.is_none()
    }
}

impl ConnectionOffer {
    /// The offer of `settings`. Its hash is computed over the whole `ConnectionSettingsOffers` at
    /// send time, because an offer carries telemetry destinations too (ADR-0017) and the Agent
    /// acknowledges the message rather than any one part of it.
    pub fn new(settings: OpAmpConnectionSettings) -> Self {
        ConnectionOffer { settings }
    }
}

/// The wall clock's port (ADR-0009): when an Agent was last heard from, and how long ago that
/// is (ADR-0030). The system clock ([`SystemClock`](crate::clock::SystemClock)) is what the
/// composition root wires.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// The port of the time that has passed, as a lease and a sweep measure it
/// (ADR-0025
/// clause 3, ADR-0023 clause 14): a clock that counts the time the host was suspended, which the
/// binary hands the lease and the package store (on Linux `CLOCK_BOOTTIME`). A reading is the time since an origin of the
/// clock's own: two readings of one clock compare, readings of two clocks do not. The adapters'
/// constructors are in [`crate::clock`].
#[derive(Clone)]
pub struct Elapsed(Arc<dyn Fn() -> Duration + Send + Sync>);

impl Elapsed {
    /// A clock that reads `read`.
    pub fn new(read: impl Fn() -> Duration + Send + Sync + 'static) -> Self {
        Elapsed(Arc::new(read))
    }

    /// The time since this clock's origin.
    #[must_use]
    pub fn now(&self) -> Duration {
        (self.0)()
    }
}

/// What asked for a signature with a token key, as its audit entry names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UseCase {
    /// A host certificate for a valid renewal proof.
    Renewal,
    /// A host certificate for an approved enrolment request.
    EnrolmentApproval,
    /// A host certificate for a member's CSR on its own connection.
    MemberCsr,
    /// A bootstrap certificate for an operator's request.
    BootstrapIssue,
}

impl UseCase {
    /// The name the audit entry gives it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UseCase::Renewal => "renewal",
            UseCase::EnrolmentApproval => "enrolment_approval",
            UseCase::MemberCsr => "member_csr",
            UseCase::BootstrapIssue => "bootstrap_issue",
        }
    }
}

/// The CSR flow's port (ADR-0026): what signs an Agent's certificate signing request. The local
/// CA ([`ClientCa`](crate::ca::ClientCa)) is the adapter the composition root wires when
/// `[client_ca]` is configured.
pub trait CertificateSigner: Send + Sync {
    /// The signed certificate, PEM, for a CSR in PEM, and what it says about itself. Its subject
    /// carries `subject` — the values copied from the bootstrap certificate or the predecessor
    /// (ADR-0016 clause 23 and ADR-0027 clause 12) — and never the request's. `use_case` is what
    /// asked for it, which the audit entry of a signature with a token key names
    /// (ADR-0043
    /// clause 6).
    ///
    /// # Errors
    /// Returns an error when the request does not parse or cannot be signed.
    fn sign(
        &self,
        csr_pem: &str,
        host: &str,
        subject: &SubjectValues,
        use_case: UseCase,
    ) -> Result<Signed, String>;

    /// The certificate a CSR proves it renews, when it carries a renewal proof (ADR-0016
    /// clause 18); `Ok(None)` when it carries none.
    ///
    /// # Errors
    /// Returns the `BadRequest` text for a proof that does not hold.
    fn renewal_proof(&self, csr_pem: &str) -> Result<Option<Facts>, String>;

    /// The issuer and serial of the bootstrap certificate a CSR's bootstrap proof names (ADR-0016
    /// clause 18), read without checking the proof; `Ok(None)` when it carries none.
    ///
    /// # Errors
    /// Returns the `BadRequest` text for a proof that does not parse.
    fn bootstrap_proof_id(&self, csr_pem: &str) -> Result<Option<CertId>, String>;

    /// Checks the request's claims to an `instance_uid` against its sender's (ADR-0026).
    ///
    /// # Errors
    /// Returns the `BadRequest` text for a request that claims another identity.
    fn check_claims(&self, csr_pem: &str, sender: &[u8]) -> Result<(), String>;
}

/// Why the package store refuses an upload: its whole-store ceiling (ADR-0034).
#[derive(Debug, PartialEq, Eq)]
pub enum StoreFull {
    /// The store is already at its ceiling.
    AtLimit { limit: u64 },
    /// Committing an artifact of `size` bytes would take it past the ceiling.
    WouldExceed { size: u64, limit: u64 },
}

impl std::fmt::Display for StoreFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreFull::AtLimit { limit } => write!(
                f,
                "the package store is at its {limit}-byte limit (max_total_package_bytes)"
            ),
            StoreFull::WouldExceed { size, limit } => write!(
                f,
                "storing this {size}-byte artifact would take the package store past its \
                 {limit}-byte limit (max_total_package_bytes)"
            ),
        }
    }
}

/// The bytes of one upload still arriving, counted against the package store's ceiling until it is
/// dropped (ADR-0034). Taken from [`AppState::admit_upload`].
pub struct UploadReservation<'a> {
    state: &'a AppState,
    /// The stored artifacts' bytes when the upload began; walking the store per chunk would cost
    /// more than the chunk.
    stored: u64,
    bytes: u64,
}

impl UploadReservation<'_> {
    /// Counts `more` bytes of this upload as they arrive.
    ///
    /// # Errors
    /// [`StoreFull::WouldExceed`] once the store and every upload in progress pass the ceiling.
    pub fn grow(&mut self, more: u64) -> Result<(), StoreFull> {
        self.bytes += more;
        let uploading = self.state.uploading_bytes.fetch_add(more, Ordering::SeqCst) + more;
        self.within(self.stored, uploading)
    }

    /// Whether the upload, whole now, may be committed: the store as it stands and every upload in
    /// progress, this one included, within the ceiling. With
    /// [`admit_upload`](AppState::admit_upload) before the stream, this is what stops a caller
    /// filling the disk under distinct names.
    ///
    /// # Errors
    /// [`StoreFull::WouldExceed`] when committing it would pass the ceiling.
    pub fn admit_artifact(&self) -> Result<(), StoreFull> {
        self.within(
            self.state.stored_package_bytes(),
            self.state.uploading_bytes.load(Ordering::SeqCst),
        )
    }

    fn within(&self, stored: u64, uploading: u64) -> Result<(), StoreFull> {
        let limit = self.state.max_total_package_bytes;
        if stored.saturating_add(uploading) > limit {
            return Err(StoreFull::WouldExceed {
                size: self.bytes,
                limit,
            });
        }
        Ok(())
    }
}

impl Drop for UploadReservation<'_> {
    fn drop(&mut self) {
        self.state
            .uploading_bytes
            .fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

/// The package store and the Deployments that release from it (ADR-0034). Every `download_url`
/// is a path the Client resolves against its own endpoint — the Agent plane, where the download
/// is served behind the same handshake as `/v1/opamp` (ADR-0034).
pub struct PackageOffering {
    store: PackageStore,
    deployments: DeploymentStore,
}

impl PackageOffering {
    pub fn with_deployments(store: PackageStore, deployments: DeploymentStore) -> Self {
        PackageOffering { store, deployments }
    }

    pub fn store(&self) -> &PackageStore {
        &self.store
    }

    pub fn deployments(&self) -> &DeploymentStore {
        &self.deployments
    }
}

/// Shared state behind every handler: the fleet, the Configuration store, and the push channel
/// WebSocket loops subscribe to.
pub struct AppState {
    fleet: Mutex<HashMap<InstanceUid, AgentRecord>>,
    /// Where Agent records survive a restart (ADR-0030) — the port, never a concrete backend:
    /// the filesystem adapter is merely what [`AppState::new`] wires by default.
    agent_store: Box<dyn AgentStore>,
    /// Each persisted record's durable digest as last written — the dirty check that keeps a
    /// heartbeat from reaching any storage backend (ADR-0030). Locked strictly after `fleet`.
    written: Mutex<HashMap<InstanceUid, [u8; 32]>>,
    configs: ConfigStore,
    /// Which Agents each [`Group`] holds (ADR-0030 clause 19), so placing one Agent touches its
    /// group alone. Locked strictly after `fleet`, and only while it is held.
    groups: Mutex<HashMap<Group, HashSet<InstanceUid>>>,
    push: watch::Sender<u64>,
    /// Hands every WebSocket connection its identity for the duplicate detection.
    next_conn: AtomicU64,
    /// The connection settings offered to the fleet (ADR-0013); `None` offers nothing and leaves
    /// `OffersConnectionSettings` undeclared.
    connection_offer: Option<ConnectionOffer>,
    /// The packages offered to the fleet (ADR-0034); `None` offers nothing and leaves
    /// `OffersPackages` undeclared.
    packages: Option<PackageOffering>,
    /// The authority that signs Agent CSRs (ADR-0026); `None` signs nothing and leaves
    /// `AcceptsConnectionSettingsRequest` undeclared.
    client_ca: Option<Box<dyn CertificateSigner>>,
    /// The enrolment window and its queue (ADR-0026); `None` while `[enrolment]` is not set, and no
    /// host enrols.
    enrolment: Option<Arc<Enrolment>>,
    /// The bootstrap certificates the Server issued (ADR-0027); `None` while `[enrolment]` is not
    /// set.
    bootstrap: Option<Arc<crate::bootstrap::BootstrapCertificates>>,
    /// What the client CA signed and what is revoked (ADR-0026); `None` only where a test serves
    /// without it.
    revocations: Option<Arc<Revocations>>,
    /// The floor of each Agent type (ADR-0035); `None` only where a test serves without it.
    floors: Option<Arc<crate::floors::PackageFloors>>,
    /// The audit record every security decision goes to (ADR-0028); `None` only in tests.
    audit: Option<Arc<dyn Audit>>,
    /// How often an admitted peer may be heard (ADR-0012); `None` only in tests.
    agent_rate: Option<Arc<crate::agent_rate::AgentRate>>,
    /// When an Agent is heard from, and how long ago that was.
    clock: Box<dyn Clock>,
    /// Where Agents send their own telemetry (ADR-0017); empty offers no destination.
    telemetry_offer: TelemetryOffer,
    /// The message size limit both transports enforce, in each direction (the Baseline's MUST).
    max_message_size: usize,
    /// The largest package artifact the REST API accepts on upload (ADR-0034) — a program, not a
    /// message, so it is bounded separately and far more generously.
    max_package_size: usize,
    /// The total size of all stored artifacts the REST API keeps before it refuses a new upload
    /// (ADR-0034): what bounds the store — and so the disk — against many uploads under distinct
    /// names, where `max_package_size` bounds only one.
    max_total_package_bytes: u64,
    /// The bytes of uploads still arriving, counted against `max_total_package_bytes` beside the
    /// stored artifacts, so uploads sent at once cannot pass the ceiling together.
    uploading_bytes: AtomicU64,
    /// How long an Agent that promised to report periodically may be silent before the fleet view
    /// calls it stale (ADR-0030). Overridden by an offered heartbeat interval, which is the period
    /// this Server actually asked for.
    stale_after: Duration,
    /// The most Agent records the fleet holds at once. A report bearing a new `instance_uid` past
    /// this ceiling is refused `Unavailable` rather than admitted, so a peer cycling fresh UIDs —
    /// each of which would pin an in-memory record and a persisted file — cannot exhaust memory or
    /// disk (a self-asserted UID is free to mint, ADR-0026). Existing Agents keep reporting; only
    /// growth past the ceiling is refused. The real defence against a flood is admission by client
    /// certificate (ADR-0026); this is the backstop that bounds what an admitted peer can do.
    max_agents: usize,
    /// How often an instance on a shared store writes the presence of the Agents it carries
    /// (ADR-0030 clause 4); zero where the store keeps none.
    /// What is stored may be one interval old, so it adds to the staleness budget (clause 8).
    presence_interval: Duration,
    /// The presence of each held Agent as the last batch wrote it, and the holder it named, so a
    /// batch writes only what moved since. Locked strictly after `fleet`.
    presence_written: Mutex<PresenceWritten>,
    /// The re-keys whose answer was lost and whose read back failed, by the old identity: the new
    /// one each moved to, if it committed, which the Agent's next report reads back and assigns.
    /// Locked strictly after `fleet`.
    unsure_rekeys: Mutex<HashMap<InstanceUid, PendingRekey>>,
    /// The restarts whose command was not sent and whose put-back the store did not take, each
    /// with when it is tried again ([`retry_restarts`](Self::retry_restarts)). Locked strictly
    /// after `fleet`.
    unput_restarts: Mutex<HashMap<InstanceUid, Backoff>>,
    /// The lease this instance serves under on a shared store, whose instance id the presence
    /// written at a connecting report names as the holder; unset where there is none.
    lease: OnceLock<watch::Receiver<Option<String>>>,
    /// Whether that lease is held by its clock now, which each message of an Agent checks before it
    /// is answered; unset where there is no lease.
    proof: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
    /// The Agents forgotten while a WebSocket carried them, by that connection, until its loop
    /// takes them and ends their session (ADR-0030 clause 10). Locked strictly after `fleet`.
    forgotten: Mutex<HashMap<ConnId, Vec<InstanceUid>>>,
    /// Each Agent's own lock, one of [`AGENT_LOCKS`] by its `instance_uid`. A report holds its
    /// Agent's from its start to its answer, across what it reads and writes in the store, and
    /// takes the fleet lock only to take that into memory; the reading of a holder again, a
    /// request, a delivery and the put-back of a restart, a forget and a label change take it
    /// too. Taken strictly before `fleet`, and never while holding it; of several, in the order
    /// of their index.
    agent_locks: Box<[Mutex<()>]>,
    /// How often an Agent's lock was found taken and waited for.
    agent_lock_waits: AtomicU64,
    /// The Agents whose take a report of this instance is making, from before it is written until
    /// the report holds the Agent in memory, which [`release_taken`](Self::release_taken) waits
    /// for. Locked after an Agent's own lock, and alone.
    taking: Mutex<HashSet<InstanceUid>>,
    /// The new Agents a report on a store this Server alone writes has counted against
    /// `max_agents` and not yet taken into memory.
    reserved_new: AtomicUsize,
}

/// How many locks the Agents' own locks are spread over: two Agents share one only by their
/// `instance_uid`, and then one waits for the other's report.
const AGENT_LOCKS: usize = 1024;

/// How often a report starts again where an Agent it went on from left memory while it read and
/// wrote the store, before it is answered `Unavailable`.
const REPORT_ROUNDS: usize = 3;

/// What a take written before the fleet lock answered
/// ([`take_row`](AppState::take_row)).
enum Took {
    /// None was made: no shared store, no lease, or no record.
    Nothing,
    /// Taken under `holder`, with the count of takes the store answered.
    Taken { holder: String, take: u64 },
    /// Made under `holder`, its answer lost.
    Lost { holder: String },
}

/// Which connection carries an Agent a WebSocket report arrives for, as the duplicate detection
/// sees it
/// (ADR-0025
/// clause 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carried {
    /// None, or the connection the report arrived on.
    Not,
    /// An older connection of the host the report's connection presented: the report's connection
    /// takes the Agent.
    ByOlder,
    /// A newer connection of that host: the report is taken up, and does not take the Agent.
    ByNewer,
    /// A newer connection of that host that carried no report of it for a heartbeat interval: the
    /// report's connection takes the Agent back.
    ByStale,
    /// A connection of another peer, or one where either presented no host: a duplicate.
    ByAnotherPeer,
}

impl Carried {
    /// Who carries `record` for a report on `conn`, which presented a certificate of `host`, at
    /// `now_ms`, a carrier that has carried no report of it for `interval` being stale.
    /// Connection ids grow, so the higher one is the newer connection. The host `record` names is
    /// the one it last arrived over, which the connection that carries it presented.
    fn of(
        record: &AgentRecord,
        conn: ConnId,
        host: Option<&str>,
        now_ms: u64,
        interval: Duration,
    ) -> Self {
        match record.owner {
            Some(owner) if record.connected && owner != conn => {
                let interval_ms = u64::try_from(interval.as_millis()).unwrap_or(u64::MAX);
                if host.is_none() || record.host.as_deref() != host {
                    Carried::ByAnotherPeer
                } else if owner < conn {
                    Carried::ByOlder
                } else if now_ms.saturating_sub(record.carried_ms) >= interval_ms {
                    Carried::ByStale
                } else {
                    Carried::ByNewer
                }
            }
            _ => Carried::Not,
        }
    }
}

/// What memory held of an Agent as its report began: what the report decides its calls to the
/// store from, before it takes the fleet lock to take them up.
struct Seen {
    /// Memory held a record of it.
    known: bool,
    connected: bool,
    owner: Option<ConnId>,
    /// The Host it last arrived over.
    host: Option<String>,
    /// The group it is placed in.
    group: Option<Group>,
    /// The group the report places it in where the report carries no description.
    joined: Option<Group>,
}

impl Seen {
    /// What `record` holds, `joined` the group it joins without a description.
    fn of(
        record: Option<&AgentRecord>,
        joined: impl FnOnce(&AgentRecord) -> Option<Group>,
    ) -> Self {
        match record {
            None => Seen {
                known: false,
                connected: false,
                owner: None,
                host: None,
                group: None,
                joined: None,
            },
            Some(record) => Seen {
                known: true,
                connected: record.connected,
                owner: record.owner,
                host: record.host.clone(),
                group: record.placement.group.clone(),
                joined: joined(record),
            },
        }
    }
}

/// A new Agent counted against `max_agents` on a store the Server alone writes, until the report
/// takes it into memory or ends.
struct Reservation<'a> {
    count: &'a AtomicUsize,
    held: bool,
}

impl<'a> Reservation<'a> {
    /// None held.
    fn none(count: &'a AtomicUsize) -> Self {
        Reservation { count, held: false }
    }

    /// One counted beside the `held` records memory holds and those `count` counts, unless that
    /// reaches `max`. Made under the fleet lock, so two reports do not count past it together.
    fn of(count: &'a AtomicUsize, held: usize, max: usize) -> Self {
        if held.saturating_add(count.load(Ordering::SeqCst)) >= max {
            return Reservation::none(count);
        }
        count.fetch_add(1, Ordering::SeqCst);
        Reservation { count, held: true }
    }

    fn held(&self) -> bool {
        self.held
    }

    /// No longer counted: taken into memory, where it counts, or let go.
    fn release(&mut self) {
        if std::mem::take(&mut self.held) {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

/// An Agent in [`AppState::taking`] while a report makes its take, until the report ends.
struct Taking<'a> {
    taking: &'a Mutex<HashSet<InstanceUid>>,
    uid: InstanceUid,
}

impl<'a> Taking<'a> {
    fn of(taking: &'a Mutex<HashSet<InstanceUid>>, uid: InstanceUid) -> Self {
        taking.lock().expect("taking lock").insert(uid);
        Taking { taking, uid }
    }
}

impl Drop for Taking<'_> {
    fn drop(&mut self) {
        self.taking.lock().expect("taking lock").remove(&self.uid);
    }
}

/// What a report's end answers with, once its record is taken into memory.
struct Ending {
    /// The identity the Agent goes by after the report.
    uid: InstanceUid,
    /// The identity it reported, which the reply is addressed to.
    reported: InstanceUid,
    /// It said goodbye.
    disconnected: bool,
    /// It came on an older connection of the host whose newer one carries the Agent, which its
    /// goodbye does not end and a queued restart is not sent on
    /// (ADR-0025
    /// clause 8).
    superseded: bool,
    identification: Option<AgentIdentification>,
    reply_flags: u64,
}

/// Where a report's end stands once its record is taken into memory.
enum Pending {
    /// Its reply, or a restart to clear first.
    Settled(Settled),
    /// A certificate it asked for to sign first, with what its record held.
    Issue(Box<PersistedAgent>, Vec<u8>),
}

/// A report's end decided under the fleet lock.
enum Settled {
    /// The reply, and what the record held, to persist before it goes out.
    Done(Box<(Processed, PersistedAgent)>),
    /// A queued restart goes out instead once its clear is written; the certificate issued goes
    /// with the reply where it does not.
    Restart(Option<TlsCertificate>),
}

/// The Client Agents of groups another instance carries, with their labels, by group.
type ClientsByGroup = HashMap<Group, Vec<(InstanceUid, BTreeMap<String, String>)>>;

/// How a placement reads the Client Agents of a group that this instance does not hold
/// (ADR-0030 clause 19,
/// ADR-0025
/// clause 8).
enum Elsewhere {
    /// From the store as the group is placed, under the fleet lock.
    Now,
    /// Read before the fleet lock was taken, by group, not yet sifted of what is held; a group
    /// placed that was not read is noted in `missed` and placed again once the lock is let go.
    Read {
        read: ClientsByGroup,
        missed: Vec<Group>,
    },
}

/// What became of an Agent found gone from the store: dropped as forgotten, or kept, as its row
/// moved through a re-key of this instance's own whose answer was lost, or as that is not known.
#[derive(Debug)]
enum Gone {
    Forgotten,
    Moved,
    Unknown(String),
}

/// A re-key of this instance whose answer was lost and whose read back failed: the identity it
/// moved the record to, if it committed, and whether a read since found it did.
#[derive(Clone, Copy, Debug)]
struct PendingRekey {
    new: InstanceUid,
    moved: bool,
}

/// What became of a re-key of this instance whose answer was lost and whose read back failed, read
/// back again at the Agent's next report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnsureRekey {
    /// None is pending for the identity, or the one that was did not move the record.
    None,
    /// It moved the record to this identity.
    Moved(InstanceUid),
    /// It is still not known.
    Unknown,
}

/// What the last presence batch wrote: the holder it named, each Agent's presence, and the count of
/// takes the store last answered for each Agent, which a disconnect is written only under and which
/// outlives the holder it was answered under. The Agents whose take's answer was lost are unsure:
/// their count is read before their disconnect is written, and the count they were answered before
/// is kept apart, for the shutdown flush alone. Every change of what is kept of an Agent moves its
/// generation, so a batch applies its answers only to what did not change meanwhile.
#[derive(Default)]
struct PresenceWritten {
    holder: Option<String>,
    agents: HashMap<InstanceUid, Presence>,
    takes: HashMap<InstanceUid, u64>,
    unsure: HashSet<InstanceUid>,
    /// The count of takes last answered for an Agent whose take since lost its answer, or 0 where
    /// this instance created its record: still the count this instance was last answered, so the
    /// shutdown flush writes the record under it, where the row names no holder and no take was
    /// counted since (ADR-0030 clause 2). No batch names this instance by it. A count answered since
    /// in `takes` outranks it, so an answer leaves it as it is.
    before_lost: HashMap<InstanceUid, u64>,
    generations: HashMap<InstanceUid, u64>,
    generation: u64,
}

impl PresenceWritten {
    /// Starts over under `holder`, where it names another: every presence is written again, and
    /// the counts of takes are kept, since only the database raises them, under any holder.
    fn under(&mut self, holder: &str) {
        if self.holder.as_deref() != Some(holder) {
            self.holder = Some(holder.to_string());
            self.agents.clear();
            self.unsure.clear();
        }
    }

    /// The generation of what is kept of `uid`.
    fn generation(&self, uid: &InstanceUid) -> u64 {
        self.generations.get(uid).copied().unwrap_or(0)
    }

    /// Moves the generation of `uid`: what is kept of it changed.
    fn changed(&mut self, uid: InstanceUid) {
        self.generation += 1;
        self.generations.insert(uid, self.generation);
    }

    /// Keeps `take` as the count of takes of `uid` a take answered, whatever was kept: the take
    /// holds the Agent's own lock, which every take of this instance holds too, so its answer is the
    /// latest, also where the record was forgotten and created again with a count that starts over;
    /// and the generation moves, so a batch in flight drops its answer.
    fn taken(&mut self, uid: InstanceUid, presence: Presence, take: u64) {
        self.agents.insert(uid, presence);
        self.takes.insert(uid, take);
        self.unsure.remove(&uid);
        self.changed(uid);
    }

    /// Notes that the take of `uid` may have been written, though its answer was lost: no count is
    /// known for it any more, so a batch names this instance over a row no live instance holds
    /// whatever its count, and its disconnect waits for a count read back. The count answered
    /// before is kept for the shutdown flush.
    fn lost(&mut self, uid: InstanceUid) {
        if let Some(take) = self.takes.remove(&uid) {
            self.before_lost.insert(uid, take);
        }
        self.unsure.insert(uid);
        self.changed(uid);
    }

    /// Forgets what was written of `uid`, which left what this instance holds.
    fn remove(&mut self, uid: &InstanceUid) {
        self.agents.remove(uid);
        self.takes.remove(uid);
        self.unsure.remove(uid);
        self.before_lost.remove(uid);
        self.changed(*uid);
    }

    /// Moves what was written of `old` to `new`, as a re-key moves the stored row whole.
    fn rekey(&mut self, old: &InstanceUid, new: InstanceUid) {
        if let Some(presence) = self.agents.remove(old) {
            self.agents.insert(new, presence);
        }
        if let Some(take) = self.takes.remove(old) {
            self.takes.insert(new, take);
        }
        if let Some(take) = self.before_lost.remove(old) {
            self.before_lost.insert(new, take);
        }
        if self.unsure.remove(old) {
            self.unsure.insert(new);
        }
        self.changed(*old);
        self.changed(new);
    }

    /// Takes up what a re-key of this instance's own whose answer was lost left in the row it
    /// moved to: the old row copied whole, which named this instance as connected, as nothing
    /// wrote the old row since the re-key took it. What a batch wrote of the old identity
    /// meanwhile found no row, so it is not what the new row holds. The entry makes the next
    /// batch write the new row: a disconnected Agent differs from it, so its disconnect is
    /// written there.
    fn moved_connected(&mut self, new: InstanceUid) {
        self.agents.insert(
            new,
            Presence {
                connected: true,
                last_seen_ms: 0,
            },
        );
        self.changed(new);
    }

    /// Keeps only what concerns an Agent of `fleet`.
    fn retain(&mut self, fleet: &HashMap<InstanceUid, AgentRecord>) {
        self.agents.retain(|uid, _| fleet.contains_key(uid));
        self.takes.retain(|uid, _| fleet.contains_key(uid));
        self.unsure.retain(|uid| fleet.contains_key(uid));
        self.before_lost.retain(|uid, _| fleet.contains_key(uid));
        self.generations.retain(|uid, _| fleet.contains_key(uid));
    }
}

impl AppState {
    /// Builds the state on any backend for the Agent records, their labels included (ADR-0030), and
    /// the Configurations (ADR-0033). The ports are the only thing the fleet logic knows about that
    /// persistence, so a database or an external store is an implementation of [`AgentStore`] or
    /// [`ConfigBackend`] plus one wiring call.
    pub fn with_stores(
        agent_store: Box<dyn AgentStore>,
        configs: Box<dyn ConfigBackend>,
        clock: Box<dyn Clock>,
    ) -> Result<Self, String> {
        let configs = ConfigStore::open(configs)?;
        let restored = configs.list().len();
        if restored > 0 {
            info!(
                configurations = restored,
                "restored the Configuration store"
            );
        }
        // Every restored Agent comes back disconnected — what it last reported is knowledge,
        // whether it is still there is not (ADR-0030) — and in the channel its labels put it in.
        //
        // A record carrying no assignments is simply one nothing has been rolled out to. There is
        // no seed to run: the store this Server would have migrated from is not supported, so an
        // absent assignment means what it says rather than "not migrated yet".
        //
        // On a shared store an instance holds only the Agents it carries, each loaded at its first
        // report here, and loads none at startup
        // (ADR-0025
        // clause 8).
        let mut fleet = HashMap::new();
        let mut written = HashMap::new();
        let restored = if agent_store.is_shared() {
            HashMap::new()
        } else {
            agent_store.load()?
        };
        for (uid, persisted) in restored {
            written.insert(uid, persisted.durable_digest());
            fleet.insert(uid, AgentRecord::from_persisted(persisted));
        }
        if !fleet.is_empty() {
            info!(agents = fleet.len(), "restored the fleet");
        }
        // A rollout cut short leaves at most a revision no assignment references (ADR-0023
        // clause 2); it is collected here, before anything is offered.
        for config in configs.list() {
            configs.retain_only(
                &config.name,
                &agent_store.referenced_revisions(&config.name),
            )?;
        }
        let state = AppState {
            fleet: Mutex::new(fleet),
            agent_store,
            written: Mutex::new(written),
            configs,
            groups: Mutex::new(HashMap::new()),
            push: watch::channel(0).0,
            next_conn: AtomicU64::new(1),
            connection_offer: None,
            packages: None,
            client_ca: None,
            enrolment: None,
            bootstrap: None,
            revocations: None,
            floors: None,
            audit: None,
            agent_rate: None,
            clock,
            telemetry_offer: TelemetryOffer::default(),
            max_message_size: opamp::frame::DEFAULT_MAX_MESSAGE_SIZE,
            max_package_size: DEFAULT_MAX_PACKAGE_SIZE,
            max_total_package_bytes: DEFAULT_MAX_TOTAL_PACKAGE_SIZE,
            uploading_bytes: AtomicU64::new(0),
            stale_after: DEFAULT_STALE_AFTER,
            max_agents: DEFAULT_MAX_AGENTS,
            presence_interval: Duration::ZERO,
            presence_written: Mutex::new(PresenceWritten::default()),
            unsure_rekeys: Mutex::new(HashMap::new()),
            unput_restarts: Mutex::new(HashMap::new()),
            lease: OnceLock::new(),
            proof: OnceLock::new(),
            forgotten: Mutex::new(HashMap::new()),
            agent_locks: (0..AGENT_LOCKS).map(|_| Mutex::new(())).collect(),
            agent_lock_waits: AtomicU64::new(0),
            taking: Mutex::new(HashSet::new()),
            reserved_new: AtomicUsize::new(0),
        };
        state.place_all();
        Ok(state)
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

    /// Sets the largest package artifact the REST API accepts on upload (ADR-0034).
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
    /// (ADR-0034).
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

    /// Whether the package store takes another upload at all (ADR-0034): one already at its
    /// ceiling, counting the uploads still arriving, takes nothing more. Asked before an upload is
    /// streamed, so a gibibyte that would only be rejected is never read. The upload's bytes are
    /// counted through the answer as they arrive, and no longer once it is dropped.
    ///
    /// # Errors
    /// [`StoreFull::AtLimit`] when the store is at or past [`max_total_package_bytes`](Self::max_total_package_bytes).
    pub fn admit_upload(&self) -> Result<UploadReservation<'_>, StoreFull> {
        let limit = self.max_total_package_bytes;
        let stored = self.stored_package_bytes();
        if stored.saturating_add(self.uploading_bytes.load(Ordering::SeqCst)) >= limit {
            return Err(StoreFull::AtLimit { limit });
        }
        Ok(UploadReservation {
            state: self,
            stored,
            bytes: 0,
        })
    }

    /// Arms the connection-settings offer (ADR-0013); with it the Server declares
    /// `OffersConnectionSettings`.
    #[must_use]
    pub fn with_connection_offer(mut self, offer: Option<ConnectionOffer>) -> Self {
        self.connection_offer = offer;
        self
    }

    /// Sets how long a heartbeating Agent may be silent before it reads as stale (ADR-0030).
    #[must_use]
    pub fn with_stale_after(mut self, stale_after: Duration) -> Self {
        self.stale_after = stale_after;
        self
    }

    /// Sets the most Agent records the fleet holds at once — the backstop against a peer minting
    /// fresh UIDs to exhaust memory and disk (ADR-0026 clause 12).
    #[must_use]
    pub fn with_max_agents(mut self, max_agents: usize) -> Self {
        self.max_agents = max_agents;
        self
    }

    /// Sets how often the presence of the Agents this instance carries is written to a shared
    /// store (ADR-0030 clause 4), which the staleness budget
    /// adds (clause 8).
    #[must_use]
    pub fn with_presence_interval(mut self, interval: Duration) -> Self {
        self.presence_interval = interval;
        self
    }

    /// Names the lease this instance serves under on a shared store: the presence it writes when
    /// an Agent connects here names that lease's instance id as the holder, at once
    /// (ADR-0030 clause 11). Set once; a later call is
    /// ignored.
    pub fn serve_under(&self, lease: watch::Receiver<Option<String>>) {
        let _ = self.lease.set(lease);
    }

    /// Answers the messages of Agents only while `holds` answers that the lease is held by its
    /// clock
    /// (ADR-0025
    /// clause 3). Set once; a later call is ignored.
    pub fn answer_while(&self, holds: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = self.proof.set(Box::new(holds));
    }

    /// Whether this instance may answer an Agent now: always without a lease, and with one while
    /// it is held by its clock.
    #[must_use]
    pub fn holds_its_lease(&self) -> bool {
        self.proof.get().is_none_or(|holds| holds())
    }

    /// The staleness budget in force: the heartbeat interval this Server offered when it offered
    /// one — the period it actually asked for — else the configured default. Three of them, not
    /// one: a single missed heartbeat is a lost packet, and a fleet view that flickers on every
    /// hiccup is one nobody trusts. On a shared store the presence interval besides, since what is
    /// stored may be one interval old (ADR-0030 clause 8).
    fn stale_after(&self) -> Duration {
        self.heartbeat_budget() + self.presence_interval
    }

    /// One heartbeat interval: the one this Server offers, else 30 seconds, the default interval
    /// a WebSocket's silence is judged by without one
    /// (ADR-0015
    /// clause 2).
    fn heartbeat_interval(&self) -> Duration {
        self.connection_offer
            .as_ref()
            .map(|offer| offer.settings.heartbeat_interval_seconds)
            .filter(|seconds| *seconds > 0)
            .map_or(DEFAULT_HEARTBEAT, Duration::from_secs)
    }

    /// Three heartbeat intervals, or the configured default.
    fn heartbeat_budget(&self) -> Duration {
        match self
            .connection_offer
            .as_ref()
            .map(|offer| offer.settings.heartbeat_interval_seconds)
            .filter(|seconds| *seconds > 0)
        {
            Some(seconds) => Duration::from_secs(seconds.saturating_mul(3)),
            None => self.stale_after,
        }
    }

    /// Offers the fleet somewhere to send its own telemetry (ADR-0017).
    #[must_use]
    pub fn with_telemetry_offer(mut self, offer: TelemetryOffer) -> Self {
        self.telemetry_offer = offer;
        self
    }

    /// Arms the CSR flow (ADR-0026); with it the Server declares
    /// `AcceptsConnectionSettingsRequest` and signs the requests Agents send.
    #[must_use]
    pub fn with_client_ca(mut self, client_ca: Option<impl CertificateSigner + 'static>) -> Self {
        self.client_ca = client_ca.map(|ca| Box::new(ca) as Box<dyn CertificateSigner>);
        self
    }

    /// Arms enrolment (ADR-0026): a host with a bootstrap certificate may ask for its first one,
    /// and an operator decides.
    #[must_use]
    pub fn with_enrolment(mut self, enrolment: Option<Arc<Enrolment>>) -> Self {
        self.enrolment = enrolment;
        self
    }

    /// Arms the issuing of bootstrap certificates and their record (ADR-0027).
    #[must_use]
    pub fn with_bootstrap(
        mut self,
        bootstrap: Option<Arc<crate::bootstrap::BootstrapCertificates>>,
    ) -> Self {
        self.bootstrap = bootstrap;
        self
    }

    /// The bootstrap certificates the Server issued, while `[enrolment]` is set.
    pub fn bootstrap(&self) -> Option<&Arc<crate::bootstrap::BootstrapCertificates>> {
        self.bootstrap.as_ref()
    }

    /// Arms the register and the revocation list (ADR-0026).
    #[must_use]
    pub fn with_revocations(mut self, revocations: Option<Arc<Revocations>>) -> Self {
        self.revocations = revocations;
        // The Hosts the records name are known now: their values and Gateway marks place them.
        self.place_all();
        self
    }

    /// Marks `host` as a Gateway, or not (ADR-0027 clause 11); answers whether this Server knows
    /// the host. The mark decides whether the Agents arriving over it carry its values and how
    /// they group (ADR-0030 clauses 18 and 19), so every Agent is placed again.
    ///
    /// # Errors
    /// Returns an error when there is no host register or the change cannot be written.
    pub fn set_gateway(&self, host: &str, gateway: bool) -> Result<bool, String> {
        let revocations = self
            .revocations
            .as_ref()
            .ok_or_else(|| "revocation is not configured".to_string())?;
        let known = revocations.set_gateway(host, gateway)?;
        if known {
            self.place_all();
        }
        Ok(known)
    }

    /// Where the Host an Agent arrived over places it (ADR-0030 clauses 18 and 19): the
    /// `fleet.host.*` values of a Host it arrived over directly, and the group its Client is
    /// recognised in. Nothing without a host register, and nothing for an Agent that names no
    /// Host.
    fn locate(&self, record: &AgentRecord) -> (BTreeMap<String, String>, Option<Group>) {
        let (Some(host), Some(revocations)) = (&record.host, &self.revocations) else {
            return (BTreeMap::new(), None);
        };
        let (gateway, subject) = revocations.placement(host);
        if gateway {
            let group =
                crate::labels::machine(record.description.as_ref()).map(|machine| Group::Behind {
                    gateway: host.clone(),
                    machine,
                });
            return (BTreeMap::new(), group);
        }
        let values = subject
            .attributes()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        (values, Some(Group::Host(host.clone())))
    }

    /// Places every Agent again — at startup, once the host register is known, and when a Gateway
    /// mark changes.
    fn place_all(&self) {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let mut groups = self.groups.lock().expect("groups lock");
        groups.clear();
        self.locate_all(&mut fleet, &mut groups);
        for group in groups.keys() {
            self.inherit_held(&mut fleet, &groups, group, &mut Elsewhere::Now);
        }
    }

    /// Locates every Agent of `fleet` and indexes it in its group, so its Client Agent can be
    /// found next.
    fn locate_all(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        groups: &mut HashMap<Group, HashSet<InstanceUid>>,
    ) {
        for (uid, record) in fleet.iter_mut() {
            let (host_values, group) = self.locate(record);
            record.placement = Placement {
                host_values,
                group: group.clone(),
                ..Placement::default()
            };
            if let Some(group) = group {
                groups.entry(group).or_default().insert(*uid);
            }
        }
    }

    /// Places the members of `group` this Server holds under their Client Agent, as [`inherit`]
    /// does. On a shared store the Client Agent of a Host may be carried by another instance, so
    /// the Agents the host register binds to the Host that this instance does not hold are read
    /// from the store, and a Client Agent among them counts as one of the group
    /// (ADR-0025
    /// clause 8). Behind a Gateway, where the register binds no Agent, the Agents that arrived
    /// over the Gateway's Host are read instead ([`arrived_over`](Self::arrived_over)).
    ///
    /// What is read of a group's Client Agents held elsewhere is read as `elsewhere` says: at once,
    /// under the fleet lock, or before it was taken, as a report places its Agent.
    fn inherit_held(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        groups: &HashMap<Group, HashSet<InstanceUid>>,
        group: &Group,
        elsewhere: &mut Elsewhere,
    ) {
        let outside = match elsewhere {
            Elsewhere::Now => self.client_agents_elsewhere(fleet, group),
            Elsewhere::Read { read, missed } => match read.get(group) {
                Some(found) => {
                    let mut outside: Vec<(InstanceUid, BTreeMap<String, String>)> = found
                        .iter()
                        .filter(|(uid, _)| !fleet.contains_key(uid))
                        .cloned()
                        .collect();
                    outside.sort_by_key(|(uid, _)| uid.0);
                    outside
                }
                None => {
                    if !missed.contains(group) {
                        missed.push(group.clone());
                    }
                    Vec::new()
                }
            },
        };
        inherit(fleet, groups, group, &outside);
    }

    /// Reads, before the fleet lock is taken, the Client Agents of each of `wanted` that this
    /// instance does not hold, as [`client_agents_elsewhere`](Self::client_agents_elsewhere) reads
    /// them under it: what a report's placement inherits from, so the lock waits for no read. What
    /// this instance comes to hold meanwhile is sifted out when the group is placed.
    fn read_elsewhere(&self, wanted: &[Group]) -> ClientsByGroup {
        let mut read = HashMap::new();
        for group in wanted {
            if !read.contains_key(group) {
                let found = self.clients_elsewhere(group, &|uid| {
                    self.fleet.lock().expect("fleet lock").contains_key(uid)
                });
                read.insert(group.clone(), found);
            }
        }
        read
    }

    /// Places again each group of `missed` that a report's placement found unread: read first,
    /// then placed under the fleet lock.
    fn place_groups_again(&self, missed: Vec<Group>) {
        if missed.is_empty() {
            return;
        }
        let mut elsewhere = Elsewhere::Read {
            read: self.read_elsewhere(&missed),
            missed: Vec::new(),
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let groups = self.groups.lock().expect("groups lock");
        for group in &missed {
            self.inherit_held(&mut fleet, &groups, group, &mut elsewhere);
        }
    }

    /// The Client Agents of a group that this instance does not hold, with their labels, as
    /// stored; none on a store this Server alone writes. Of a Host, those the host register binds
    /// to it; behind a Gateway, which the register binds none to, those that arrived over the
    /// Gateway's Host and report the group's machine, read through the port's `query_page`
    /// (ADR-0025
    /// clause 8). One that cannot be read is logged, and the group is placed from what this
    /// instance holds.
    fn client_agents_elsewhere(
        &self,
        fleet: &HashMap<InstanceUid, AgentRecord>,
        group: &Group,
    ) -> Vec<(InstanceUid, BTreeMap<String, String>)> {
        self.clients_elsewhere(group, &|uid| fleet.contains_key(uid))
    }

    /// The Client Agents of `group` read from the store, of those `held` says this instance does
    /// not hold, as [`client_agents_elsewhere`](Self::client_agents_elsewhere) answers them.
    fn clients_elsewhere(
        &self,
        group: &Group,
        held: &dyn Fn(&InstanceUid) -> bool,
    ) -> Vec<(InstanceUid, BTreeMap<String, String>)> {
        if !self.agent_store.is_shared() {
            return Vec::new();
        }
        if let Group::Behind { gateway, machine } = group {
            return match self.arrived_over(gateway) {
                Ok(found) => {
                    let mut clients: Vec<(InstanceUid, BTreeMap<String, String>)> = found
                        .into_iter()
                        .filter(|(uid, record)| {
                            crate::labels::is_client_agent(record.description.as_ref())
                                && crate::labels::machine(record.description.as_ref()).as_ref()
                                    == Some(machine)
                                && !held(uid)
                        })
                        .map(|(uid, record)| (uid, record.labels))
                        .collect();
                    clients.sort_by_key(|(uid, _)| uid.0);
                    clients
                }
                Err(e) => {
                    warn!(gateway = %gateway, error = %e, "cannot read the agents behind the gateway that another instance carries");
                    Vec::new()
                }
            };
        }
        let (Group::Host(host), Some(revocations)) = (group, &self.revocations) else {
            return Vec::new();
        };
        let SpeaksFor::Agents(bound) = revocations.speaks_for(host) else {
            return Vec::new();
        };
        let elsewhere: Vec<InstanceUid> = bound
            .iter()
            .filter_map(|uid| parse_hex_uid(uid))
            .filter(|uid| !held(uid))
            .collect();
        if elsewhere.is_empty() {
            return Vec::new();
        }
        match self.agent_store.get_many(&elsewhere) {
            Ok(found) => {
                let mut clients: Vec<(InstanceUid, BTreeMap<String, String>)> = found
                    .into_iter()
                    .filter(|(_, record)| {
                        record.host.as_deref() == Some(host.as_str())
                            && crate::labels::is_client_agent(record.description.as_ref())
                    })
                    .map(|(uid, record)| (uid, record.labels))
                    .collect();
                clients.sort_by_key(|(uid, _)| uid.0);
                clients
            }
            Err(e) => {
                warn!(host = %host, error = %e, "cannot read the agents of the host that another instance carries");
                Vec::new()
            }
        }
    }

    /// Records read from the store, each placed among the others, as [`place_all`](Self::place_all)
    /// places what this Server holds: what the Operator plane shows of them where the fleet is not
    /// in memory.
    fn placed(
        &self,
        records: HashMap<InstanceUid, AgentRecord>,
    ) -> HashMap<InstanceUid, AgentRecord> {
        let mut fleet = records;
        let mut groups = HashMap::new();
        self.locate_all(&mut fleet, &mut groups);
        for group in groups.keys() {
            inherit(&mut fleet, &groups, group, &[]);
        }
        fleet
    }

    /// The fleet as the Operator plane reads it
    /// (ADR-0023 clause 3): on a shared store every record as
    /// stored, connected as its stored presence says, placed among the others, and nothing of what
    /// this instance holds in memory; on a store this Server alone writes, what it holds.
    ///
    /// # Errors
    /// Returns an error when the shared store cannot be read.
    fn read_fleet<T>(
        &self,
        read: impl FnOnce(&HashMap<InstanceUid, AgentRecord>) -> T,
    ) -> Result<T, String> {
        if !self.agent_store.is_shared() {
            return Ok(read(&self.fleet.lock().expect("fleet lock")));
        }
        Ok(read(&self.stored_fleet()?))
    }

    /// The record of `uid` as the Operator plane reads it from the shared store, placed as
    /// [`stored_fleet`](Self::stored_fleet) places it, without the whole fleet: one read of it and
    /// of the Agents the host register binds to the same Host, which are the group its Client
    /// Agent is found in. An Agent that arrived over a Gateway is placed among the Agents that
    /// arrived over the same Host ([`arrived_over`](Self::arrived_over)), since the register knows
    /// no group behind a Gateway, whatever Host it binds the Agent to.
    fn stored_agent(&self, uid: &InstanceUid) -> Result<Option<AgentRecord>, String> {
        let bound = self.revocations.as_ref().and_then(|revocations| {
            let host = revocations.bound_host(uid.as_bytes())?;
            match revocations.speaks_for(&host) {
                SpeaksFor::Agents(bound) => Some(bound),
                SpeaksFor::Any => None,
            }
        });
        let mut uids: Vec<InstanceUid> = bound
            .iter()
            .flatten()
            .filter_map(|uid| parse_hex_uid(uid))
            .collect();
        if !uids.contains(uid) {
            uids.push(*uid);
        }
        let mut read = self.records_of(self.agent_store.query(Some(&uids))?);
        // What places it is the Host it arrived over, not the one it is bound to: an Agent bound to
        // a direct Host may report over a Gateway since.
        let behind_a_gateway = read.get(uid).is_some_and(|record| {
            record.host.as_deref().is_some_and(|host| {
                self.revocations
                    .as_ref()
                    .is_some_and(|revocations| revocations.is_gateway(host))
            })
        });
        if let (true, Some(host)) = (
            behind_a_gateway,
            read.get(uid).and_then(|record| record.host.clone()),
        ) {
            return Ok(self.placed(self.arrived_over(&host)?).remove(uid));
        }
        Ok(self.placed(std::mem::take(&mut read)).remove(uid))
    }

    /// The records a query read, each connected as its stored presence says, not yet placed.
    fn records_of(
        &self,
        read: Vec<(InstanceUid, crate::agent_store::StoredAgent)>,
    ) -> HashMap<InstanceUid, AgentRecord> {
        read.into_iter()
            .map(|(uid, stored)| {
                let mut record = AgentRecord::from_persisted(stored.record);
                record.connected = stored.connected;
                record.holder = stored.holder;
                (uid, record)
            })
            .collect()
    }

    /// Every record the shared store holds, with its stored presence, placed among the others.
    fn stored_fleet(&self) -> Result<HashMap<InstanceUid, AgentRecord>, String> {
        let records = self.records_of(self.agent_store.query(None)?);
        Ok(self.placed(records))
    }

    /// Places one Agent again after what places it may have changed — its Host, its report, or
    /// its identity — and the groups it left and joined with it.
    fn place(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        uid: &InstanceUid,
        elsewhere: &mut Elsewhere,
    ) {
        let Some(record) = fleet.get_mut(uid) else {
            return;
        };
        let (host_values, group) = self.locate(record);
        record.placement.host_values = host_values;
        let left = std::mem::replace(&mut record.placement.group, group.clone());
        if group.is_none() {
            record.placement.client_agent = None;
            record.placement.inherited.clear();
            record.placement.ambiguous = false;
        }
        let mut groups = self.groups.lock().expect("groups lock");
        if left != group {
            if let Some(left) = &left {
                unindex(&mut groups, left, uid);
            }
        }
        if let Some(joined) = &group {
            groups.entry(joined.clone()).or_default().insert(*uid);
            self.inherit_held(fleet, &groups, joined, elsewhere);
        }
        if let Some(left) = left.filter(|left| Some(left) != group.as_ref()) {
            self.inherit_held(fleet, &groups, &left, elsewhere);
        }
    }

    /// Takes an Agent that left the fleet, or its old identity, out of its group, and places what
    /// is left of the group again.
    fn unplace(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        uid: &InstanceUid,
        group: Option<Group>,
        elsewhere: &mut Elsewhere,
    ) {
        let Some(group) = group else {
            return;
        };
        let mut groups = self.groups.lock().expect("groups lock");
        unindex(&mut groups, &group, uid);
        self.inherit_held(fleet, &groups, &group, elsewhere);
    }

    /// The register and the revocation list.
    pub fn revocations(&self) -> Option<&Arc<Revocations>> {
        self.revocations.as_ref()
    }

    /// Arms the floors (ADR-0035).
    #[must_use]
    pub fn with_floors(mut self, floors: Option<Arc<crate::floors::PackageFloors>>) -> Self {
        self.floors = floors;
        self
    }

    /// The floor of each Agent type.
    pub fn floors(&self) -> Option<&Arc<crate::floors::PackageFloors>> {
        self.floors.as_ref()
    }

    /// Arms the audit record (ADR-0028).
    #[must_use]
    pub fn with_audit(mut self, audit: Option<Arc<dyn Audit>>) -> Self {
        self.audit = audit;
        self
    }

    /// The audit record.
    pub fn audit(&self) -> Option<&Arc<dyn Audit>> {
        self.audit.as_ref()
    }

    /// Arms the rate limit on the Agent plane (ADR-0012).
    #[must_use]
    pub fn with_agent_rate(
        mut self,
        agent_rate: Option<Arc<crate::agent_rate::AgentRate>>,
    ) -> Self {
        self.agent_rate = agent_rate;
        self
    }

    /// The rate limit on the Agent plane.
    pub fn agent_rate(&self) -> Option<&Arc<crate::agent_rate::AgentRate>> {
        self.agent_rate.as_ref()
    }

    /// Records a decision this Server is about to act on; without a record it is not taken
    /// (ADR-0028 clause 5).
    fn audited(&self, entry: Entry) -> Result<(), String> {
        match &self.audit {
            Some(audit) => audit
                .record(entry)
                .map_err(|_| "the audit record is unavailable — retry shortly".to_string()),
            None => Ok(()),
        }
    }

    /// Records a refusal; it is refused either way.
    pub(crate) fn audit_refusal(&self, entry: Entry) {
        if let Some(audit) = &self.audit {
            audit.refusal(entry);
        }
    }

    /// Records a certificate the client CA signed on a renewal, before it is offered (ADR-0023
    /// clause 9, ADR-0028 clause 1). An approval records through [`Approving`].
    fn record_issued(
        &self,
        signed: &Signed,
        instance_uid: &[u8],
        predecessor: Option<CertId>,
    ) -> Result<(), String> {
        self.audited(self.signature_entry(signed, instance_uid, predecessor.as_ref(), None))?;
        let Some(revocations) = &self.revocations else {
            return Ok(());
        };
        revocations
            .record(signed.facts.clone(), instance_uid, predecessor)
            .map_err(|e| format!("cannot record the issued certificate: {e}"))
    }

    /// The audit entry of a signature, `issuance.signed` (ADR-0028 clause 1): `predecessor` for a
    /// renewal, and for an enrolment the request it approves.
    fn signature_entry(
        &self,
        signed: &Signed,
        instance_uid: &[u8],
        predecessor: Option<&CertId>,
        request: Option<&str>,
    ) -> Entry {
        Entry::new("issuance.signed", "issued")
            .with(
                "kind",
                if predecessor.is_some() {
                    "renewal"
                } else {
                    "enrolment"
                },
            )
            .with("issuer", signed.facts.issuer_name.clone())
            .with(
                "authority",
                self.revocations.as_ref().and_then(|revocations| {
                    revocations
                        .authority_of(&signed.facts.id.issuer)
                        .map(|authority| authority.role.clone())
                }),
            )
            .with("serial", signed.facts.id.serial.clone())
            .with("subject", signed.facts.subject.clone())
            .with("key_fingerprint", signed.facts.key_fingerprint.clone())
            .with("instance_uid", hex::encode(instance_uid))
            .with(
                "predecessor_serial",
                predecessor.map(|id| id.serial.clone()),
            )
            .with("request", request.map(str::to_string))
    }

    /// What a CSR renews, the host the new certificate is for (ADR-0016 clause 18), and the
    /// subject values it carries on (ADR-0027 clause 12). A renewal proof names the certificate
    /// whose key signed the new one — through a Gateway too — and the host and the values carry
    /// on from it; without one, the certificate the connection presented is renewed, unless its
    /// host is marked as a Gateway (ADR-0016 clause 18). A bootstrap proof makes the request an
    /// enrolment, which a member does not make (ADR-0016 clause 25) — unless it proves the very
    /// certificate the connection presented: a Client cannot tell a certificate an operator
    /// provisioned from a bootstrap certificate, and proves whichever it holds. A certificate that
    /// names no host — one an operator provisioned — gets a host derived from its issuer and
    /// serial, the same on every retry.
    fn renews(
        &self,
        ca: &dyn CertificateSigner,
        csr: &str,
        presented: Option<&Presented>,
    ) -> Result<(Option<CertId>, String, SubjectValues, UseCase), String> {
        let derived = |id: &CertId| fleet_core::host::derived(&id.issuer, &id.serial);
        if let Some(old) = ca.renewal_proof(csr)? {
            if self
                .revocations
                .as_ref()
                .is_some_and(|revocations| revocations.is_certificate_revoked(&old.id))
            {
                return Err("the certificate this request renews is revoked".to_string());
            }
            let host = old.host.clone().unwrap_or_else(|| derived(&old.id));
            return Ok((Some(old.id), host, old.subject_values, UseCase::Renewal));
        }
        // A proof of the certificate presented is that certificate's renewal, and without a
        // certificate presented — `Admission::open`, which no Server serves — nothing is enrolled.
        let enrols = match (ca.bootstrap_proof_id(csr)?, presented) {
            (Some(named), Some(presented)) => named != presented.id,
            _ => false,
        };
        Ok(match presented {
            Some(presented) => {
                let host = presented
                    .host
                    .clone()
                    .unwrap_or_else(|| derived(&presented.id));
                // Behind a Gateway the presented certificate is the Gateway's, and its host carries
                // the mark: a CSR without a proof would hand the Gateway's powers to whoever sent
                // it (ADR-0016 clause 18).
                let gateway = self
                    .revocations
                    .as_ref()
                    .is_some_and(|revocations| revocations.is_gateway(&host));
                if enrols {
                    return Err(if gateway {
                        "a bootstrap proof through a Gateway is an enrolment request, not a renewal"
                            .to_string()
                    } else {
                        MEMBER_DOES_NOT_ENROL.to_string()
                    });
                }
                if gateway {
                    return Err(NEEDS_A_PROOF.to_string());
                }
                (
                    Some(presented.id.clone()),
                    host,
                    presented.subject_values.clone(),
                    UseCase::MemberCsr,
                )
            }
            None => (
                None,
                InstanceUid::default().to_string(),
                SubjectValues::default(),
                UseCase::MemberCsr,
            ),
        })
    }

    /// The enrolment window and its queue, while `[enrolment]` is set.
    pub fn enrolment(&self) -> Option<&Arc<Enrolment>> {
        self.enrolment.as_ref()
    }

    /// Approves one pending enrolment request: the client CA signs it (ADR-0027 clause 10), with
    /// the subject values of the bootstrap certificate the request came with (ADR-0016 clause 23).
    ///
    /// # Errors
    /// Returns [`DecisionError::NotFound`] when enrolment is off or the id is unknown,
    /// [`DecisionError::Subject`] when the bootstrap certificate's subject cannot be carried, and
    /// [`DecisionError::Sign`] when no CA is configured or it refuses the request.
    pub fn approve_enrolment(&self, id: &str) -> Result<String, DecisionError> {
        let enrolment = self.enrolment.as_ref().ok_or(DecisionError::NotFound)?;
        let signer = self.client_ca.as_deref().ok_or_else(|| {
            DecisionError::Sign("this Server issues no client certificates".into())
        })?;
        // A new host: its identity is minted here and carried by every renewal (ADR-0027 clause 11).
        let host = InstanceUid::default().to_string();
        let revoked = |bootstrap: &CertId| {
            self.revocations
                .as_ref()
                .is_some_and(|revocations| revocations.is_certificate_revoked(bootstrap))
        };
        // The Gateway the request came through, named in the approval's audit entry.
        let gateway = enrolment
            .pending()
            .into_iter()
            .find(|pending| pending.id == id)
            .and_then(|pending| pending.requester.gateway);
        let serial = enrolment.approve(id, signer, &host, &Approving(self), &revoked)?;
        // Recorded before the approval is answered (ADR-0023 clause 9). The decision is written
        // already, so a record that cannot take it refuses nothing: the register answers for it.
        if let Err(e) = self.audited(
            Entry::new("enrolment.approved", "approved")
                .with("id", id.to_string())
                .with("serial", serial.clone())
                .with("gateway", gateway),
        ) {
            tracing::warn!(error = %e, request = %id, serial = %serial, "cannot record an approval");
        }
        Ok(serial)
    }

    /// Takes back every approved request whose certificate the register does not hold, so it
    /// waits again, and records each signature as undecided: what a start does with a store that
    /// writes the decision before the register entry, after a write between the two failed
    /// (ADR-0023 clause 9). `read` reads what a certificate, PEM, says about itself. Answers how
    /// many it took back.
    ///
    /// # Errors
    /// Returns an error when a decision cannot be taken back.
    pub fn take_back_unregistered_approvals(
        &self,
        read: &dyn Fn(&str) -> Result<Facts, String>,
    ) -> Result<usize, String> {
        let (Some(enrolment), Some(revocations)) = (&self.enrolment, &self.revocations) else {
            return Ok(0);
        };
        if enrolment.registers_with_its_decision() {
            return Ok(0);
        }
        let mut taken = 0;
        for (id, pem) in enrolment.approved() {
            let facts = match read(&pem) {
                Ok(facts) => facts,
                Err(e) => {
                    tracing::warn!(error = %e, request = %id, "an approved request's certificate does not read");
                    continue;
                }
            };
            if revocations.issued_entry(&facts.id).is_some() {
                continue;
            }
            enrolment.take_back(&id)?;
            Approving(self).undecided(
                &Signed { pem, facts },
                &id,
                crate::enrolment::Undecided::RolledBack,
            );
            taken += 1;
        }
        Ok(taken)
    }

    /// What an enrolment connection is told (ADR-0027 clause 9): the capabilities that say it may
    /// send a CSR, and — once its request is approved — the issued certificate, as an ordinary
    /// connection-settings offer with no private key in it.
    pub fn enrolment_answer(
        &self,
        instance_uid: &[u8],
        certificate: Option<String>,
    ) -> ServerToAgent {
        let mut capabilities = ServerCapabilities::AcceptsStatus as u64;
        if self.client_ca.is_some() {
            capabilities |= ServerCapabilities::AcceptsConnectionSettingsRequest as u64
                | ServerCapabilities::OffersConnectionSettings as u64;
        }
        ServerToAgent {
            instance_uid: instance_uid.to_vec(),
            capabilities,
            connection_settings: certificate.map(|cert| {
                compose_settings_offer(
                    Some(OpAmpConnectionSettings {
                        certificate: Some(TlsCertificate {
                            cert: cert.into_bytes(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    TelemetryOffer::default(),
                )
            }),
            ..Default::default()
        }
    }

    /// Arms package delivery (ADR-0034); with a non-empty store the Server declares
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

    /// Read access to the Deployment store, armed with the package store (ADR-0034).
    pub fn deployment_store(&self) -> Option<&DeploymentStore> {
        self.packages.as_ref().map(PackageOffering::deployments)
    }

    /// The Capability Set this Server declares: the base set, plus `OffersConnectionSettings`
    /// while there is anything to offer and `OffersPackages` / `AcceptsPackagesStatus`
    /// while a non-empty package store is armed — an undeclared capability is never exercised, a
    /// declared one never hollow.
    fn capabilities(&self) -> u64 {
        let mut caps = SERVER_CAPABILITIES;
        // All three ways a `ConnectionSettingsOffers` leaves this Server (ADR-0013 clause 3): the
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

    /// Reads the Configurations and, where package delivery is armed, the Deployments and the
    /// Packages again from their stores, as another instance changed them
    /// (ADR-0025
    /// clause 12). Each one whose flag is set is dropped and read again whole.
    ///
    /// # Errors
    /// Returns an error when a store cannot be read; what it held is then kept.
    pub fn reload_catalogue(
        &self,
        configurations: bool,
        deployments: bool,
        packages: bool,
    ) -> Result<(), String> {
        if configurations {
            self.configs.reload()?;
        }
        if let Some(offering) = &self.packages {
            if deployments {
                offering.deployments().reload()?;
            }
            if packages {
                offering.store().reload()?;
            }
        }
        Ok(())
    }

    /// Takes up what another instance wrote to the records of Agents this one holds
    /// (ADR-0025
    /// clauses 8 and 9): the record of each Agent of `uids` this instance holds, or of every one
    /// it holds where `uids` is `None`, is read again from the store. Its assignment, its labels and
    /// its queued restart are taken into what is held, its group is placed again for its labels,
    /// and one the store no longer holds, forgotten through another instance, is dropped. The
    /// WebSocket loops are woken, so a connected Agent is offered what it was released, or sent the
    /// restart requested, at once. The records are read before the fleet lock is taken, so no
    /// report waits for the read; one that differs from what is held is read again under the lock
    /// and taken up from that read, since an act of this instance made after the first read, which
    /// it never hears announced, wrote the store and what is held together under that lock. That
    /// second read is one read of every record that differs, so the lock is held for one round
    /// trip however many differ.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read; what is held is then kept.
    ///
    /// On a shared store an Agent of `uids` this instance does not hold may be the Client Agent of
    /// a Host whose Agents it holds, carried by another instance: those groups are placed again,
    /// so what they inherit follows its labels, or its forgetting.
    pub fn take_up_agents(&self, uids: Option<&HashSet<InstanceUid>>) -> Result<(), String> {
        if self.agent_store.is_shared() {
            self.place_again_for(uids);
        }
        let held: Vec<InstanceUid> = {
            let fleet = self.fleet.lock().expect("fleet lock");
            match uids {
                None => fleet.keys().copied().collect(),
                Some(uids) => uids
                    .iter()
                    .filter(|uid| fleet.contains_key(uid))
                    .copied()
                    .collect(),
            }
        };
        let stored: Vec<(InstanceUid, Option<PersistedAgent>)> = match uids {
            // The Agents held, in one read, and not the fleet.
            None => {
                let mut all = self.agent_store.get_many(&held)?;
                held.into_iter()
                    .map(|uid| (uid, all.remove(&uid)))
                    .collect()
            }
            Some(_) => held
                .into_iter()
                .map(|uid| Ok((uid, self.agent_store.get(&uid)?)))
                .collect::<Result<_, String>>()?,
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let differing: Vec<InstanceUid> = stored
            .into_iter()
            .filter(|(uid, persisted)| {
                fleet.get(uid).is_some_and(|record| {
                    persisted.as_ref().is_none_or(|persisted| {
                        record.assignment != persisted.assignment
                            || record.restart_pending != persisted.restart_pending
                            || record.labels != persisted.labels
                    })
                })
            })
            .map(|(uid, _)| uid)
            .collect();
        if differing.is_empty() {
            return Ok(());
        }
        let mut again = self.agent_store.get_many(&differing)?;
        let mut moved = 0usize;
        let mut unknown = None;
        for uid in differing {
            let Some(record) = fleet.get_mut(&uid) else {
                continue;
            };
            let Some(persisted) = again.remove(&uid) else {
                // A re-key's read back that fails is tried again with this read.
                if let Gone::Unknown(e) = self.drop_forgotten(&mut fleet, &uid) {
                    unknown = Some(e);
                }
                moved += 1;
                continue;
            };
            let mut changed = false;
            if record.assignment != persisted.assignment {
                record.assignment = persisted.assignment;
                changed = true;
            }
            if record.restart_pending != persisted.restart_pending {
                record.restart_pending = persisted.restart_pending;
                changed = true;
            }
            if changed {
                self.written.lock().expect("written lock").remove(&uid);
            }
            if record.labels != persisted.labels {
                self.take_labels(&mut fleet, &uid, persisted.labels);
                changed = true;
            }
            moved += usize::from(changed);
        }
        drop(fleet);
        if moved > 0 {
            self.push.send_modify(|rev| *rev += 1);
            debug!(
                agents = moved,
                "took up what another instance wrote to the agents' records"
            );
        }
        match unknown {
            Some(e) => Err(format!(
                "cannot read back a re-key whose answer was lost: {e}"
            )),
            None => Ok(()),
        }
    }

    /// Places again each group this instance holds members of whose Client Agent may be one of
    /// `uids` that it does not hold — the group's Client Agent now, or, of a Host, an Agent the
    /// host register binds to it — or every group where `uids` is `None`. Behind a Gateway a
    /// Client Agent another instance carries that the group was not placed under yet is found
    /// only where `uids` is `None`.
    fn place_again_for(&self, uids: Option<&HashSet<InstanceUid>>) {
        let Some(revocations) = &self.revocations else {
            return;
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        // The notified Agents this instance does not hold; `None` for every one.
        let elsewhere: Option<Vec<InstanceUid>> = uids.map(|uids| {
            uids.iter()
                .filter(|uid| !fleet.contains_key(uid))
                .copied()
                .collect()
        });
        if elsewhere.as_ref().is_some_and(Vec::is_empty) {
            return;
        }
        let groups = self.groups.lock().expect("groups lock");
        let due: Vec<Group> = groups
            .iter()
            .filter(|(group, members)| {
                let Some(elsewhere) = &elsewhere else {
                    return true;
                };
                // Behind a Gateway the register binds no Agent; the group's Client Agent is the
                // one it was placed under.
                let bound = match group {
                    Group::Host(host) => match revocations.speaks_for(host) {
                        SpeaksFor::Agents(bound) => bound,
                        SpeaksFor::Any => BTreeSet::new(),
                    },
                    Group::Behind { .. } => BTreeSet::new(),
                };
                elsewhere.iter().any(|uid| {
                    bound.contains(&hex::encode(uid.as_bytes()))
                        || members.iter().any(|member| {
                            fleet
                                .get(member)
                                .and_then(|record| record.placement.client_agent)
                                == Some(*uid)
                        })
                })
            })
            .map(|(group, _)| group.clone())
            .collect();
        for group in &due {
            self.inherit_held(&mut fleet, &groups, group, &mut Elsewhere::Now);
        }
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

    /// Creates a Configuration or replaces its saved revision, and persists it. **Saving only
    /// saves** (ADR-0033): nothing is offered and no WebSocket loop wakes — every Agent keeps
    /// the revision its assignment pins, and the fleet view shows the newer save waiting.
    pub fn save_configuration(
        &self,
        name: &str,
        revision: Revision,
    ) -> Result<Configuration, String> {
        let config = self.configs.put_saved(name, revision)?;
        info!(configuration = %name, "configuration saved — nothing distributed");
        Ok(config)
    }

    /// The Deployment claiming one Agent (ADR-0034), or the conflict that says why none does.
    fn deployment_of(&self, record: &AgentRecord) -> Result<Option<Deployment>, String> {
        let Some(store) = self.deployment_store() else {
            return Ok(None);
        };
        let all = store.snapshot();
        deployment_for(&all, record.effective_description().as_deref()).map(|found| found.cloned())
    }

    /// What a press of `deployment` would release to this Agent (ADR-0033 clause 9) — the
    /// assignment it would write, or `None` when it has nothing for the Agent.
    ///
    /// Two parts, each as of now: the Package the Deployment holds for the Agent's type when it is
    /// a candidate (ADR-0035 clause 5 and ADR-0033 clauses 14, 15, 16 and 17), and every Configuration it holds that fits the Agent's
    /// type, at its saved revision. Where the Deployment holds a Package for the Agent's type that
    /// is no candidate — the Agent already runs it, or it would move the Agent backwards — the
    /// press cannot move the package part, so the Package the assignment pins stays pinned: the
    /// release is replaced whole, and a part the press does not move must not fall out of it
    /// (ADR-0033 clause 20). A Package or Configuration the Deployment no longer holds leaves the
    /// release.
    ///
    /// The rollout acts, the reach counts and the fleet view's waiting changes all ask this one
    /// question, so the count beside a press, the press and the view cannot disagree.
    fn release(
        &self,
        store: &PackageStore,
        deployment: &Deployment,
        record: &AgentRecord,
    ) -> Option<Assignment> {
        let effective = record.effective_description();
        let description = effective.as_deref();
        let configurations = self
            .configs
            .candidates_in(&deployment.configurations, description);
        let agent_type = crate::packages::reported_agent_type(description);
        let held = agent_type.and_then(|agent_type| deployment.package_for(agent_type));
        let package = store
            .candidate(
                deployment,
                description,
                &record.installed_package_versions(),
            )
            .or_else(|| {
                held.and(record.assigned_package())
                    .filter(|pinned| Some(pinned.agent_type.as_str()) == agent_type)
                    .cloned()
            });
        (package.is_some() || !configurations.is_empty()).then(|| Assignment {
            deployment: deployment.name.clone(),
            package,
            configurations,
        })
    }

    /// Writes releases (ADR-0033 clause 6, ADR-0023 clause 2): each Configuration's saved revision
    /// is pinned — copied into the store's retained revisions, its hash the one the assignment
    /// keeps — and the revisions and the records carrying the new assignments go to the store as
    /// one operation. Only once it succeeded are they the fleet's. A failure leaves the fleet's
    /// assignments as they were, the store puts back the records it wrote, and the revisions the
    /// stored assignments do not reference are collected. An Agent whose record the store no
    /// longer holds, forgotten through another instance, is released nothing: it is dropped here
    /// too and its session ended. Answers how many Agents were released to, and the Configurations
    /// the old and new assignments name, whose unreferenced revisions the caller collects.
    ///
    /// Each release comes with the record it is written to, as held or as read from a shared
    /// store, which writes its assignment alone; one this Server holds takes the new assignment
    /// into what it holds.
    fn write_releases(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        releases: Vec<(InstanceUid, PersistedAgent, Option<Assignment>)>,
    ) -> Result<(usize, Vec<String>), RolloutError> {
        let names: BTreeSet<String> = releases
            .iter()
            .flat_map(|(_, _, release)| release.iter())
            .flat_map(|release| release.configurations.keys().cloned())
            .collect();
        let mut touched: Vec<String> = releases
            .iter()
            .flat_map(|(_, persisted, _)| persisted.assignment.iter())
            .flat_map(|assignment| assignment.configurations.keys().cloned())
            .collect();
        let written = self
            .configs
            .pin(&names, |hashes, revisions| {
                let agents: Vec<(InstanceUid, PersistedAgent)> = releases
                    .into_iter()
                    .map(|(uid, mut persisted, release)| {
                        persisted.assignment = release.map(|mut release| {
                            for (name, hash) in release.configurations.iter_mut() {
                                hash.clone_from(&hashes[name]);
                            }
                            release
                        });
                        (uid, persisted)
                    })
                    .collect();
                let wrote = self.agent_store.release(revisions, &agents)?;
                Ok((agents, wrote))
            })
            .map_err(|e| {
                self.collect_revisions(names);
                RolloutError::Storage(e)
            })?;
        let (agents, wrote) = written;
        let (written, forgotten): (Vec<_>, Vec<_>) =
            agents.into_iter().partition(|(uid, _)| wrote.contains(uid));
        for (uid, _) in &forgotten {
            self.drop_forgotten(fleet, uid);
        }
        let count = written.len();
        let mut digests = self.written.lock().expect("written lock");
        for (uid, persisted) in written {
            touched.extend(
                persisted
                    .assignment
                    .iter()
                    .flat_map(|a| a.configurations.keys().cloned()),
            );
            if let Some(record) = fleet.get_mut(&uid) {
                record.assignment.clone_from(&persisted.assignment);
                digests.insert(uid, persisted.durable_digest());
            }
        }
        Ok((count, touched))
    }

    /// Collects every retained revision of `names` no stored assignment references any more — a
    /// question the store answers (ADR-0023 clause 2).
    fn collect_revisions(&self, names: impl IntoIterator<Item = String>) {
        for name in names.into_iter().collect::<BTreeSet<_>>() {
            let referenced = self.agent_store.referenced_revisions(&name);
            if let Err(e) = self.configs.retain_only(&name, &referenced) {
                warn!(configuration = %name, error = %e, "cannot collect unreferenced revisions");
            }
        }
    }

    /// The per-Agent rollout act (ADR-0033 clause 9): releases the Deployment that claims this
    /// Agent to it, replacing its assignment, and wakes the WebSocket loops so a connected Agent
    /// hears it now.
    ///
    /// `named` is what the request body said: nothing, or the Deployment the operator means. Naming
    /// another one is refused, and so is an Agent no Deployment claims or two claim. Naming one is
    /// not a way past a conflict: honouring it would sidestep the conflict for good instead of
    /// fixing it, and make this path the way into a state the fleet-wide act forbids.
    pub fn rollout_to_agent(
        &self,
        uid: &InstanceUid,
        named: Option<&str>,
    ) -> Result<(), RolloutError> {
        // On a shared store the Agent is read as the Operator plane reads it, whichever instance
        // carries it, with the Agents of its Host.
        let stored = if self.agent_store.is_shared() {
            self.stored_agent(uid).map_err(RolloutError::Storage)?
        } else {
            None
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        // One held and not read is released from what is held, and the store answers whether it
        // is still there.
        let record = stored
            .as_ref()
            .or_else(|| fleet.get(uid))
            .ok_or(RolloutError::UnknownAgent)?;
        let (deployment, release) = self.release_to(uid, record, named)?;
        // The record it is written to: what is held of it, else what is stored.
        let base = fleet.get(uid).unwrap_or(record).to_persisted();
        let (released, touched) = self.write_releases(&mut fleet, vec![(*uid, base, release)])?;
        // The release may have replaced a pinned revision nothing references any more.
        self.collect_revisions(touched);
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        if released == 0 {
            return Err(RolloutError::UnknownAgent);
        }
        info!(agent = %uid, deployment = %deployment, "rollout to agent");
        Ok(())
    }

    /// What [`rollout_to_agent`](Self::rollout_to_agent) releases to `record`, and through which
    /// Deployment.
    fn release_to(
        &self,
        uid: &InstanceUid,
        record: &AgentRecord,
        named: Option<&str>,
    ) -> Result<(String, Option<Assignment>), RolloutError> {
        let deployment = self
            .deployment_of(record)
            .map_err(RolloutError::NotApplicable)?
            .ok_or_else(|| {
                RolloutError::NotApplicable(format!(
                    "agent {uid} is in no deployment — no Deployment's Selector matches it, so \
                     there is nothing to roll out to it; label it or re-aim a Deployment"
                ))
            })?;
        if let Some(name) = named {
            if deployment.name != name {
                return Err(RolloutError::NotApplicable(format!(
                    "agent {uid} belongs to deployment {:?}, not {name:?}",
                    deployment.name
                )));
            }
        }
        let store = self
            .offering()
            .map_err(RolloutError::UnknownResource)?
            .store();
        refuse_unreleasable(store, &deployment)?;
        let release = self.release(store, &deployment, record);
        if release == record.assignment {
            refuse_a_package_left_out(store, &deployment, record, release.as_ref())?;
        }
        Ok((deployment.name, release))
    }

    /// The rollout act for a Deployment (ADR-0033 clause 9): releases it to every Agent it claims
    /// and would move, replacing each one's assignment, and returns how many Agents that was.
    ///
    /// An Agent some *other* Deployment also claims is skipped rather than counted — the conflict
    /// is reported on that Agent, and a press that quietly resolved it here would be the ranking
    /// this model removed, wearing a different hat.
    pub fn rollout_deployment(&self, name: &str) -> Result<usize, RolloutError> {
        let offering = self.offering().map_err(RolloutError::UnknownResource)?;
        let store = offering.store();
        // On a shared store every Agent is matched as the Operator plane reads it, whichever
        // instance carries it, and read before the fleet lock is taken, so no report waits for it.
        let stored = self
            .agent_store
            .is_shared()
            .then(|| self.stored_fleet())
            .transpose()
            .map_err(RolloutError::Storage)?;
        // Locked before the Deployments are read, so a delete cannot slip in between and leave
        // assignments naming a channel that is gone (see `delete_deployment`).
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let deployments = offering.deployments().snapshot();
        let deployment = deployments
            .get(name)
            .ok_or_else(|| RolloutError::UnknownResource(format!("no deployment {name:?}")))?;
        refuse_unreleasable(store, deployment)?;
        let mut releases: Vec<(InstanceUid, PersistedAgent, Option<Assignment>)> = Vec::new();
        // What is held and was not read is matched as held — created since the read, or forgotten
        // through another instance meanwhile, which the store answers by writing nothing of it.
        let candidates: Vec<(&InstanceUid, &AgentRecord)> = match &stored {
            Some(stored) => stored
                .iter()
                .chain(fleet.iter().filter(|(uid, _)| !stored.contains_key(uid)))
                .collect(),
            None => fleet.iter().collect(),
        };
        for (uid, record) in candidates {
            let claiming = deployment_for(&deployments, record.effective_description().as_deref());
            match claiming {
                Ok(Some(claimed)) if claimed.name == name => {}
                _ => continue,
            }
            let release = self.release(store, deployment, record);
            if release != record.assignment {
                let base = fleet.get(uid).unwrap_or(record).to_persisted();
                releases.push((*uid, base, release));
            }
        }
        // In one order, so a rollout that fails part-way fails the same way every time.
        releases.sort_by_key(|(uid, _, _)| uid.0);
        let (assigned, touched) = self.write_releases(&mut fleet, releases)?;
        self.collect_revisions(touched);
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        info!(deployment = %name, agents = assigned, "deployment rolled out to every agent it claims");
        Ok(assigned)
    }

    /// Queues a restart for one Agent (`AcceptsRestartCommand`) and wakes the WebSocket loops so
    /// a connected Agent hears it now; a polling one picks it up on its next exchange.
    ///
    /// The request is written to its own column of the record before this answers, and on a shared
    /// store announced to the instance that carries the Agent, which decides from what is stored
    /// (ADR-0030 clause 2).
    pub fn request_restart(&self, uid: &InstanceUid) -> Result<(), RestartError> {
        // A request, a delivery, a try and a forget of one Agent each hold its own lock.
        let _agent = self.agent_lock(uid);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let capabilities = if self.agent_store.is_shared() {
            self.agent_store
                .get(uid)
                .map_err(RestartError::Storage)?
                .ok_or(RestartError::UnknownAgent)?
                .capabilities
        } else {
            fleet
                .get(uid)
                .ok_or(RestartError::UnknownAgent)?
                .capabilities
        };
        if capabilities & opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64 == 0 {
            return Err(RestartError::NoCapability);
        }
        // Operator intent survives a Server restart like any other durable state (ADR-0030).
        self.agent_store
            .set_restart_pending(uid, true)
            .map_err(RestartError::Storage)?
            .ok_or(RestartError::UnknownAgent)?;
        if let Some(record) = fleet.get_mut(uid) {
            record.restart_pending = true;
            self.written.lock().expect("written lock").remove(uid);
        }
        // A put-back still kept is this restart now: two requests before a delivery are one.
        self.drop_kept_restart(uid);
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        info!(agent = %uid, "restart requested");
        Ok(())
    }

    /// Replaces an Agent's labels (ADR-0030), which changes what Selectors match it.
    ///
    /// A key the Agent already reports is **refused**, not applied: `os.type` and `host.arch`
    /// choose which artifact it is offered (ADR-0034) and `service.name` decides which packages fit
    /// it at all (ADR-0034), so a label that outranked them would let a slip here offer this Agent
    /// an artifact built for another machine. Labels annotate; they do not correct.
    ///
    /// A label move changes only what the fleet view **proposes** (ADR-0033): the Agent's
    /// candidates follow its new channel, and nothing is distributed until a rollout act says so.
    ///
    /// The labels are part of the Agent's record (ADR-0030 clause 21) and are written to their own
    /// column of it before this answers; on a shared store the Agent is read from the store, and
    /// the change announced to the instance that carries it. A Client Agent's labels hold for its
    /// Client's other Agents (ADR-0030 clause 19), so its group is placed again.
    pub fn set_labels(
        &self,
        uid: &InstanceUid,
        set: BTreeMap<String, String>,
    ) -> Result<(), LabelError> {
        crate::labels::check_pairs(&set).map_err(LabelError::Invalid)?;
        // Not between a report's read of the record and its taking it into memory.
        let _agent = self.agent_lock(uid);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let description = if self.agent_store.is_shared() {
            self.agent_store
                .get(uid)
                .map_err(LabelError::Storage)?
                .ok_or(LabelError::UnknownAgent)?
                .description
        } else {
            fleet
                .get(uid)
                .ok_or(LabelError::UnknownAgent)?
                .description
                .clone()
        };
        let reported = crate::labels::reported_keys(description.as_ref());
        if let Some(clash) = set.keys().find(|key| reported.iter().any(|r| r == *key)) {
            return Err(LabelError::RestatesReported(clash.clone()));
        }
        if !self
            .agent_store
            .set_labels(uid, &set)
            .map_err(LabelError::Storage)?
        {
            return Err(LabelError::UnknownAgent);
        }
        self.take_labels(&mut fleet, uid, set);
        drop(fleet);
        info!(agent = %uid, "labels set — candidates follow, nothing is distributed");
        Ok(())
    }

    /// Re-keys the stored record of `old` to `new`. Where the store answers an error, the move may
    /// have committed all the same, so it is read back before the Agent is answered: moved, it
    /// stands. A read back that fails leaves the pair for the Agent's next report to read back
    /// (ADR-0025
    /// clause 10).
    fn rekey_reading_back(
        &self,
        old: &InstanceUid,
        new: &InstanceUid,
        record: &PersistedAgent,
    ) -> Result<(), (String, bool)> {
        let Err(e) = self.agent_store.rekey(old, new, record) else {
            return Ok(());
        };
        match self.agent_store.moved(old, new) {
            Ok(true) => {
                info!(%old, %new, error = %e, "the re-key committed though its answer was lost");
                Ok(())
            }
            Ok(false) => Err((e, false)),
            Err(read) => {
                self.unsure_rekeys.lock().expect("rekeys lock").insert(
                    *old,
                    PendingRekey {
                        new: *new,
                        moved: false,
                    },
                );
                Err((
                    format!(
                        "{e}; it could not be read back either, so the Agent's next report reads \
                         it back: {read}"
                    ),
                    true,
                ))
            }
        }
    }

    /// Reads back the re-key of `old` whose answer was lost and whose read back failed, if one
    /// is pending: once the store answers, it is pending no more. One a notification's read found
    /// moved is answered moved without a read. The read runs before the fleet lock is taken, under
    /// the Agent's own lock, as the re-key does, and may wait for the share lock up to the
    /// database's `lock_timeout` within the call's bound.
    fn read_back_unsure_rekey(&self, old: &InstanceUid) -> UnsureRekey {
        let pending = self
            .unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .get(old)
            .copied();
        let Some(PendingRekey { new, moved }) = pending else {
            return UnsureRekey::None;
        };
        if moved {
            self.unsure_rekeys.lock().expect("rekeys lock").remove(old);
            return UnsureRekey::Moved(new);
        }
        match self.agent_store.moved(old, &new) {
            Ok(moved) => {
                self.unsure_rekeys.lock().expect("rekeys lock").remove(old);
                if moved {
                    info!(%old, %new, "a re-key whose answer was lost committed; assigning its identity");
                    UnsureRekey::Moved(new)
                } else {
                    UnsureRekey::None
                }
            }
            Err(e) => {
                warn!(agent = %old, error = %e, "cannot read back a re-key whose answer was lost");
                UnsureRekey::Unknown
            }
        }
    }

    /// Reads back, wherever `old` is found gone from the store, the re-key of it whose answer was
    /// lost and whose read back failed, if one is pending: the identity it moved to, and whether
    /// the store holds it moved. Made without the fleet lock where a report finds the row gone;
    /// [`apply_unsure_rekey`](Self::apply_unsure_rekey) takes it up.
    fn read_unsure_rekey(&self, old: &InstanceUid) -> Option<(InstanceUid, Result<bool, String>)> {
        let pending = self
            .unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .get(old)
            .copied();
        let PendingRekey { new, .. } = pending?;
        Some((new, self.agent_store.moved(old, &new)))
    }

    /// Takes up what [`read_unsure_rekey`](Self::read_unsure_rekey) read of `old`. Moved, what is
    /// held moves to the new identity, the Agent's session stays, and the new identity is assigned
    /// at the next report under `old`; a session that ended meanwhile has its disconnect written to
    /// the new row by the next presence batch, since what is held moves disconnected. Not known,
    /// `old` is kept.
    fn apply_unsure_rekey(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        old: &InstanceUid,
        read: Option<(InstanceUid, Result<bool, String>)>,
        elsewhere: &mut Elsewhere,
    ) -> Gone {
        let Some((new, moved)) = read else {
            return Gone::Forgotten;
        };
        match moved {
            Ok(true) => {
                self.rekey_held(fleet, old, new, elsewhere);
                self.presence_written
                    .lock()
                    .expect("presence lock")
                    .moved_connected(new);
                self.unsure_rekeys
                    .lock()
                    .expect("rekeys lock")
                    .insert(*old, PendingRekey { new, moved: true });
                info!(%old, %new, "a re-key whose answer was lost committed; its identity is assigned at the next report");
                Gone::Moved
            }
            Ok(false) => {
                self.unsure_rekeys.lock().expect("rekeys lock").remove(old);
                Gone::Forgotten
            }
            Err(e) => {
                warn!(agent = %old, error = %e, "cannot read back a re-key whose answer was lost; keeping the agent");
                Gone::Unknown(e)
            }
        }
    }

    /// Moves what this instance holds of `old` to `new`, as a re-key moves the stored row: the
    /// labels go with it (ADR-0030 clause 21), the old identity leaves its group, and what was
    /// written of its presence moves with the row. A put-back of its restart kept for retry moves
    /// too, so the retry queues the restart on the row's new identity
    /// (ADR-0025
    /// clause 8).
    fn rekey_held(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        old: &InstanceUid,
        new: InstanceUid,
        elsewhere: &mut Elsewhere,
    ) {
        if let Some(mut record) = fleet.remove(old) {
            let group = record.placement.group.take();
            fleet.insert(new, record);
            self.unplace(fleet, old, group, elsewhere);
            self.written.lock().expect("written lock").remove(old);
            self.presence_written
                .lock()
                .expect("presence lock")
                .rekey(old, new);
            let mut kept = self.unput_restarts.lock().expect("restarts lock");
            if let Some(backoff) = kept.remove(old) {
                kept.insert(new, backoff);
            }
        }
    }

    /// Drops every pending re-key of `uid`, from it or to it: what an operator's forget does.
    fn drop_unsure_rekeys(&self, uid: &InstanceUid) {
        self.unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .retain(|old, pending| old != uid && pending.new != *uid);
    }

    /// Takes `labels` into the record of `uid` this Server holds, if it holds one, and places its
    /// group again. What the store holds of it is no longer what was last written whole.
    fn take_labels(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        uid: &InstanceUid,
        labels: BTreeMap<String, String>,
    ) {
        let Some(record) = fleet.get_mut(uid) else {
            return;
        };
        record.labels = labels;
        self.written.lock().expect("written lock").remove(uid);
        if let Some(group) = record.placement.group.clone() {
            let groups = self.groups.lock().expect("groups lock");
            self.inherit_held(fleet, &groups, &group, &mut Elsewhere::Now);
        }
    }

    /// Which Deployments hold each Package, keyed by `<agent type>@<version>` — how a Package
    /// answers "whom would this reach", since it does not aim by itself (ADR-0034).
    pub fn deployments_holding(&self) -> BTreeMap<String, Vec<String>> {
        let mut holding: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let Some(store) = self.deployment_store() else {
            return holding;
        };
        // The store lists in name order, so every list is built already sorted.
        for deployment in store.list() {
            for id in deployment.packages.values() {
                holding
                    .entry(id.to_string())
                    .or_default()
                    .push(deployment.name.clone());
            }
        }
        holding
    }

    /// Which Deployments hold each Configuration, by name (ADR-0033 clause 23) — how a
    /// Configuration answers "whom would this reach", since it does not aim by itself.
    pub fn deployments_holding_configurations(&self) -> BTreeMap<String, Vec<String>> {
        let mut holding: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let Some(store) = self.deployment_store() else {
            return holding;
        };
        for deployment in store.list() {
            for name in &deployment.configurations {
                holding
                    .entry(name.clone())
                    .or_default()
                    .push(deployment.name.clone());
            }
        }
        holding
    }

    /// Whom each Deployment reaches, per name — the three counts the fleet view reads.
    ///
    /// Zero has three meanings now, and only the first is a mistake to go hunting for:
    /// `claiming` is zero when the channel aims at nobody (a misspelled label, a Selector nothing
    /// matches); `targeted` is zero when a press would move none of the Agents it claims — each
    /// already holds what it would release — which is nothing to fix; and `conflicting` counts the Agents another Deployment also claims,
    /// which is the one an operator has to resolve before this channel can reach them.
    ///
    /// It answers for the fleet *as reported so far*: a channel aimed at hosts that have not
    /// connected yet legitimately reaches nobody, which is why these are counts to be read rather
    /// than errors to be raised.
    ///
    /// It reads the fleet as [`read_fleet`](Self::read_fleet) does: on a shared store from the
    /// database.
    ///
    /// # Errors
    /// Returns an error when the shared store cannot be read.
    pub fn deployment_reach(&self) -> Result<BTreeMap<String, DeploymentReach>, String> {
        let Ok(offering) = self.offering() else {
            return Ok(BTreeMap::new());
        };
        self.read_fleet(|fleet| self.reach_of(offering, fleet))
    }

    /// Whom each Deployment reaches of `fleet`, as [`deployment_reach`](Self::deployment_reach)
    /// counts it.
    fn reach_of(
        &self,
        offering: &PackageOffering,
        fleet: &HashMap<InstanceUid, AgentRecord>,
    ) -> BTreeMap<String, DeploymentReach> {
        let mut reach: BTreeMap<String, DeploymentReach> = BTreeMap::new();
        let store = offering.store();
        let all = offering.deployments().snapshot();
        for name in all.keys() {
            reach.entry(name.clone()).or_default();
        }
        for record in fleet.values() {
            let effective = record.effective_description();
            match deployment_for(&all, effective.as_deref()) {
                Ok(Some(deployment)) => {
                    let entry = reach.entry(deployment.name.clone()).or_default();
                    entry.claiming += 1;
                    if self.release(store, deployment, record) != record.assignment {
                        entry.targeted += 1;
                    }
                }
                Ok(None) => {}
                // Every channel in the way carries the count: the operator has to look at all of
                // them, not at whichever one happened to be listed first.
                Err(_) => {
                    for (name, deployment) in &all {
                        if deployments::matches(&deployment.selector, effective.as_deref()) {
                            reach.entry(name.clone()).or_default().conflicting += 1;
                        }
                    }
                }
            }
        }
        reach
    }

    /// Forgets everything this Server knows about one Agent (ADR-0030): the record is dropped and
    /// the row leaves the fleet view. Nothing reaches the host — no process is stopped and no
    /// certificate revoked, since a certificate here proves fleet membership and its host, never
    /// which Agent is speaking (ADR-0027 clause 11). A Client still running therefore reappears on its next
    /// report, which this Server answers with `ReportFullState` as it does for any unknown Agent.
    /// The `instance_uid` is released from the Host it was bound to, so that report binds it to
    /// the host whose certificate carries it (ADR-0030 clause 10).
    ///
    /// Refused while the Agent is still reporting: the record holds the hashes that gate
    /// re-offering, so forgetting a live Agent has it offered its configuration again — and the
    /// Collector plugin restarts its Managed Process when a configuration arrives. An operator who
    /// wants a restart asks for one through [`request_restart`](Self::request_restart).
    ///
    /// `connected` alone would not do. Behind a Gateway the open connection is the *Gateway's*, so
    /// a Gatewayed Agent that died still reads as connected; and plain-HTTP polling has no socket
    /// to close, so an Agent that stops polling without saying goodbye stays connected forever.
    /// Silence is the second half of the test — and it is [`is_silent`], not [`is_stale`]: an
    /// Agent that declared no heartbeat is never called stale, and gating on staleness would leave
    /// its row on a decommissioned host permanently unremovable.
    pub fn forget_agent(&self, uid: &InstanceUid) -> Result<(), ForgetError> {
        let _agent = self.agent_lock(uid);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let (budget, now_ms) = (self.stale_after(), self.clock.now_ms());
        if let Some(record) = fleet.get(uid) {
            if record.connected && !is_silent(record, budget, now_ms) {
                return Err(ForgetError::StillReporting);
            }
        }
        // Forgetting that left a stored record behind would be the "remembering under another
        // name" ADR-0030 rejected: the record leaves the store with the row, its labels with it
        // (ADR-0030 clause 21). On a shared store the instance that carries the Agent may be
        // another, so what decides is the presence stored, read and removed in one step
        // (ADR-0030 clause 11).
        // On a shared store an Agent this instance does not hold has its assignment read first, so
        // the revisions only it pinned are collected once it is gone. A record that cannot be
        // read, one that does not decode among them, is still forgotten, and what it pinned is
        // left to the next collection of its Configurations: forgetting is how such a record goes.
        let mut unheld_configurations = Vec::new();
        if self.agent_store.is_shared() {
            if !fleet.contains_key(uid) {
                match self.agent_store.get(uid) {
                    Ok(stored) => {
                        unheld_configurations = stored
                            .and_then(|stored| stored.assignment)
                            .map(|assignment| assignment.configurations.into_keys().collect())
                            .unwrap_or_default();
                    }
                    Err(e) => {
                        warn!(agent = %uid, error = %e, "cannot read the record to forget; forgetting it without collecting what it pinned");
                    }
                }
            }
            let silent_before = now_ms.saturating_sub(budget.as_millis() as u64);
            match self
                .agent_store
                .remove_if_silent(uid, silent_before)
                .map_err(ForgetError::Storage)?
            {
                Some(true) => {}
                Some(false) => return Err(ForgetError::StillReporting),
                // Held but not stored: forgotten or re-keyed through another instance, and not yet
                // heard of here. What is held is forgotten.
                None if fleet.contains_key(uid) => {}
                None => return Err(ForgetError::UnknownAgent),
            }
            self.written.lock().expect("written lock").remove(uid);
        } else {
            if !fleet.contains_key(uid) {
                return Err(ForgetError::UnknownAgent);
            }
            self.unpersist(uid);
        }
        let removed = fleet.remove(uid);
        self.presence_written
            .lock()
            .expect("presence lock")
            .remove(uid);
        self.drop_unsure_rekeys(uid);
        self.drop_kept_restart(uid);
        // Its `instance_uid` leaves the Host it was bound to: whichever host reports it next binds
        // it (ADR-0030 clause 10).
        if let Some(revocations) = &self.revocations {
            if let Err(e) = revocations.release(uid.as_bytes()) {
                warn!(agent = %uid, error = %e, "cannot write the released host binding");
            }
        }
        if let Some(record) = removed {
            // A session that still carries it ends (ADR-0030 clause 10).
            self.end_session(uid, &record);
            // A Client Agent forgotten takes its labels from the Agents that inherited them.
            self.unplace(
                &mut fleet,
                uid,
                record.placement.group.clone(),
                &mut Elsewhere::Now,
            );
            // A pinned revision only this Agent referenced is unreferenced now (ADR-0033).
            self.collect_revisions(record.assigned_configurations().keys().cloned());
        } else {
            self.collect_revisions(unheld_configurations);
        }
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        info!(agent = %uid, "agent forgotten");
        Ok(())
    }

    /// The queued restart for this Agent as the Baseline's command-only message, taken once —
    /// `None` when nothing is queued (or the Agent went away). One its connection does not take is
    /// queued again through [`unsent`](Self::unsent). It is handed out only to `conn`, the
    /// WebSocket connection asking, where that connection carries the Agent: an older connection
    /// of its host that still names it leaves the restart to the newer one
    /// (ADR-0025
    /// clause 8).
    ///
    /// A delivery holds the Agent's own lock, and clears the restart in the store with the fleet
    /// lock let go.
    pub fn restart_command_for(&self, uid: &InstanceUid, conn: ConnId) -> Option<ServerToAgent> {
        let due = |fleet: &HashMap<InstanceUid, AgentRecord>| {
            fleet.get(uid).is_some_and(|record| {
                record.restart_pending && record.connected && record.owner == Some(conn)
            })
        };
        if !due(&self.fleet.lock().expect("fleet lock")) {
            return None;
        }
        let _agent = self.agent_lock(uid);
        if !due(&self.fleet.lock().expect("fleet lock")) {
            return None;
        }
        let cleared = self.clear_restart(uid);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get_mut(uid)?;
        self.take_restart(uid, record, cleared)
            .then(|| restart_command(uid, self.capabilities()))
    }

    /// Clears the queued restart of `record` in the store first, and answers whether it was still
    /// queued there, so it is delivered: one another instance delivered already is not delivered
    /// twice, and a request made after the clear stays queued
    /// (ADR-0030 clause 2). One that cannot be cleared is
    /// kept for the next exchange, and one cleared whose command is not sent is put back by
    /// [`unsent`](Self::unsent).
    ///
    /// The clear is written before the fleet lock is taken, under the Agent's own lock, which
    /// requests, deliveries, forgets and the tries of a put-back take
    /// ([`clear_restart`](Self::clear_restart)); `cleared` is what it answered, and this takes it
    /// into `record`.
    fn take_restart(
        &self,
        uid: &InstanceUid,
        record: &mut AgentRecord,
        cleared: Result<Option<bool>, String>,
    ) -> bool {
        match cleared {
            Ok(was) => {
                record.restart_pending = false;
                self.written.lock().expect("written lock").remove(uid);
                if was == Some(true) {
                    // A put-back still kept is delivered with this one.
                    self.drop_kept_restart(uid);
                }
                was == Some(true)
            }
            Err(e) => {
                warn!(agent = %uid, error = %e, "cannot clear the queued restart; it waits");
                false
            }
        }
    }

    /// Clears the queued restart of `uid` in the store, as [`take_restart`](Self::take_restart)
    /// takes it up.
    fn clear_restart(&self, uid: &InstanceUid) -> Result<Option<bool>, String> {
        self.agent_store.set_restart_pending(uid, false)
    }

    /// A message its connection did not take. A restart command among them is queued again, so
    /// the Agent's next exchange is sent it: on a WebSocket a restart is delivered at least once
    /// wherever the send is seen to fail. That holds on a lease that lapsed meanwhile too, since
    /// setting it again only restores what the Operator asked for. A put-back the store does not
    /// take is kept and tried again by [`retry_restarts`](Self::retry_restarts)
    /// (ADR-0025
    /// clause 8).
    pub fn unsent(&self, message: &ServerToAgent) {
        let restart = opamp::proto::CommandType::Restart as i32;
        if message.command.as_ref().map(|c| c.r#type) != Some(restart) {
            return;
        }
        let Some(uid) = InstanceUid::from_wire(&message.instance_uid) else {
            return;
        };
        self.put_back(&uid, false);
    }

    /// Tries again each put-back of a restart that is due, and answers how many are still kept.
    /// One that fails again waits twice as many calls, up to [`Backoff::MAX_WAIT`]; one that holds
    /// is dropped, and so is one whose Agent's row is gone, forgotten or re-keyed through another
    /// instance, with a warning naming the Agent. One whose row moved through a re-key of this
    /// instance's own not read back yet waits for the read back. The composition root calls this every second, so
    /// the intervals grow to a minute; a process that ends first loses the restarts still kept
    /// (ADR-0025
    /// clause 8).
    pub fn retry_restarts(&self) -> usize {
        for uid in self.due_restarts() {
            self.put_back(&uid, true);
        }
        self.unput_restarts.lock().expect("restarts lock").len()
    }

    /// Counts each kept put-back down one call, and answers those whose try is due.
    fn due_restarts(&self) -> Vec<InstanceUid> {
        let mut kept = self.unput_restarts.lock().expect("restarts lock");
        kept.iter_mut()
            .filter_map(|(uid, backoff)| backoff.tick().then_some(*uid))
            .collect()
    }

    /// Sets the restart of `uid` queued again in the store and in the record this instance holds,
    /// and tells the sessions, under the Agent's own lock, which requests, deliveries and forgets
    /// take too. A `retry` is made only while its put-back is still kept, so one a request, a delivery
    /// or a forget dropped since it was found due is not tried; any put-back drops what is kept
    /// once the store answers.
    /// One the store does not take is kept, or waits longer, for
    /// [`retry_restarts`](Self::retry_restarts). An Agent forgotten meanwhile has nothing to
    /// restart; one whose record the store does not hold, forgotten or re-keyed through another
    /// instance, is logged and dropped, and one whose record a re-key of this instance's own whose
    /// answer was lost may have moved stays kept, for the read back to move it
    /// (ADR-0025
    /// clause 8).
    fn put_back(&self, uid: &InstanceUid, retry: bool) {
        let _agent = self.agent_lock(uid);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        if retry
            && !self
                .unput_restarts
                .lock()
                .expect("restarts lock")
                .contains_key(uid)
        {
            return;
        }
        let answered = self.agent_store.set_restart_pending(uid, true);
        // A re-key of this instance's own whose answer was lost may have moved the row: until its
        // read back moves what is held, and this put-back with it, the put-back stays kept.
        let unsure = matches!(answered, Ok(None))
            && self
                .unsure_rekeys
                .lock()
                .expect("rekeys lock")
                .get(uid)
                .is_some_and(|pending| !pending.moved);
        let mut kept = self.unput_restarts.lock().expect("restarts lock");
        match answered {
            Ok(None) if unsure => {
                let backoff = kept.entry(*uid).or_insert_with(Backoff::first);
                if retry {
                    backoff.failed();
                }
                info!(agent = %uid, "a re-key of the agent is not read back yet; its restart is tried again after it");
            }
            Ok(held) => {
                kept.remove(uid);
                drop(kept);
                if held.is_none() {
                    // Forgotten, or re-keyed through another instance meanwhile: not seen apart.
                    warn!(agent = %uid, "the restart is no longer tried, the Agent's row is gone");
                    return;
                }
                if let Some(record) = fleet.get_mut(uid) {
                    record.restart_pending = true;
                    self.written.lock().expect("written lock").remove(uid);
                }
                drop(fleet);
                self.push.send_modify(|rev| *rev += 1);
                info!(agent = %uid, "the restart command was not sent; it stays queued");
            }
            Err(e) => {
                let backoff = kept.entry(*uid).or_insert_with(Backoff::first);
                if retry {
                    backoff.failed();
                }
                warn!(agent = %uid, error = %e, "cannot queue an unsent restart again yet");
            }
        }
    }

    /// Takes up what another instance announced of the restarts of `uids`, every Agent where a
    /// notification was missed: a request there is this restart now, so a put-back still kept is
    /// dropped. After a missed notification only one whose record shows a restart queued is
    /// (ADR-0025
    /// clause 8).
    ///
    /// # Errors
    /// Returns an error when a record cannot be read after a missed notification; what is kept
    /// stays kept.
    pub fn take_up_restarts(&self, uids: Option<&HashSet<InstanceUid>>) -> Result<(), String> {
        let _fleet = self.fleet.lock().expect("fleet lock");
        let mut kept = self.unput_restarts.lock().expect("restarts lock");
        match uids {
            Some(uids) => kept.retain(|uid, _| !uids.contains(uid)),
            None => {
                let named: Vec<InstanceUid> = kept.keys().copied().collect();
                for uid in named {
                    if self
                        .agent_store
                        .get(&uid)?
                        .is_some_and(|stored| stored.restart_pending)
                    {
                        kept.remove(&uid);
                    }
                }
            }
        }
        Ok(())
    }

    /// Drops the put-back of `uid`'s restart kept for retry, if one is: what a request, a delivery
    /// and a forget of the Agent do
    /// (ADR-0025
    /// clause 8).
    fn drop_kept_restart(&self, uid: &InstanceUid) {
        self.unput_restarts
            .lock()
            .expect("restarts lock")
            .remove(uid);
    }

    /// Deletes a Configuration (ADR-0033 clause 12); `false` when none of that name exists.
    /// **Refused while any Agent's assignment pins it** — the message says how many — because a
    /// deletion must not change what a running Agent receives: those Agents are rolled out through
    /// a Deployment without it first, or forgotten. A deleted Configuration leaves every Deployment
    /// that held it.
    ///
    /// The fleet lock is held across the check and the delete, so a rollout of this Server cannot
    /// pin it in between. A store that other Servers write too counts the stored assignments again
    /// in the act that deletes (ADR-0023 clause 2), so a pin
    /// written through another instance refuses it as well, and takes it out of every Deployment
    /// in that act too.
    pub fn delete_configuration(&self, name: &str) -> Result<bool, DeleteError> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let pinned = |pinning: usize| {
            DeleteError::Pinned(format!(
                "configuration {name:?} is pinned by the assignment of {pinning} Agent(s) — roll \
                 them out through a Deployment without it, or forget them, before deleting it"
            ))
        };
        let pinning = self.agent_store.pinning(Pin::Configuration(name));
        if pinning > 0 {
            return Err(pinned(pinning));
        }
        // The store decides again from what it stores: another instance may have pinned it.
        let deleted = match self.configs.delete(name).map_err(DeleteError::Storage)? {
            Removal::Removed => true,
            Removal::Absent => false,
            Removal::Pinned(pinning) => return Err(pinned(pinning)),
        };
        if deleted {
            if let Some(store) = self.deployment_store() {
                let left = store
                    .drop_configuration(name)
                    .map_err(|e| DeleteError::Storage(e.to_string()))?;
                if !left.is_empty() {
                    info!(configuration = %name, deployments = ?left, "configuration left the deployments that held it");
                }
            }
            info!(configuration = %name, "configuration deleted");
        }
        drop(fleet);
        Ok(deleted)
    }

    /// Puts a Configuration into a Deployment (ADR-0033 clause 5); a Configuration the Server
    /// does not hold is `Missing`. Distributes nothing. Under the fleet lock, so a Configuration
    /// cannot be deleted between the check and the write. A store that other Servers write too
    /// reads the Configuration again in the act that adds it, so one deleted through another
    /// instance is `Missing` as well.
    pub fn put_deployment_configuration(
        &self,
        name: &str,
        configuration: &str,
    ) -> Result<Deployment, DeploymentError> {
        let store = self.deployment_store().ok_or(DeploymentError::NotFound)?;
        let _fleet = self.fleet.lock().expect("fleet lock");
        if self.configs.get(configuration).is_none() {
            return Err(deployments::no_configuration(configuration));
        }
        store.put_configuration(name, configuration)
    }

    /// Takes a Configuration out of a Deployment. Not gated: nothing a Deployment holds is frozen
    /// (ADR-0033 clause 11), and an Agent already released keeps its pinned revision until the next
    /// press, which drops it from the Agent's map.
    pub fn remove_deployment_configuration(
        &self,
        name: &str,
        configuration: &str,
    ) -> Result<Deployment, DeploymentError> {
        self.deployment_store()
            .ok_or(DeploymentError::NotFound)?
            .remove_configuration(name, configuration)
    }

    /// Persists one record when — and only when — its durable content changed since it was last
    /// written (ADR-0030). `last_seen_ms` and `sequence_num` are outside the comparison and ride
    /// along on whatever write happens, so the common heartbeat reaches no storage backend at
    /// all; [`flush_agents`](Self::flush_agents) is what makes them current on a graceful stop.
    /// A write that fails is logged, never fatal: a fleet that keeps running on a full disk beats
    /// one that refuses reports. Answers whether the record is still stored: on a shared store
    /// one forgotten through another instance is not, and is not written again.
    fn persist_if_dirty(&self, uid: &InstanceUid, persisted: &PersistedAgent) -> bool {
        match self.persist(uid, persisted) {
            Ok(stored) => stored,
            Err(e) => {
                warn!(agent = %uid, error = %e, "cannot persist the agent record");
                true
            }
        }
    }

    /// Writes one record when its durable content changed since it was last written, and says
    /// whether the write failed — for an operator's act that must not be answered unwritten — or
    /// found no record to write, as of an Agent forgotten through another instance. The record was
    /// created or read before (ADR-0030 clause 14), so only what a report decides is written of
    /// it, and nothing creates it again.
    ///
    /// `persisted` is what the record held when the report let the fleet lock go, and it is
    /// written after: no other report waits for the write. Two writes of one record do not race,
    /// so an older state never lands after a newer one, since the Agent's own lock, which every
    /// report of it holds from its start to its answer, is held across the write; and a report is
    /// answered only once its record is written (ADR-0030 clause 4). What a store's write may take
    /// is bounded: a PostgreSQL call fails after `postgres::CALL_TIMEOUT` (ADR-0023 clause 24).
    ///
    /// On a shared store, an Agent this instance was answered no count of takes for, its take not
    /// written or its answer lost, has its record written at every report and no digest kept: a
    /// shutdown flush of another instance with a higher sequence number, as of an Agent that started
    /// again, may have written over it, and the next report writes it again. Once a count is
    /// answered, a report that changes nothing durable writes nothing again
    /// (ADR-0025
    /// clause 8).
    fn persist(&self, uid: &InstanceUid, persisted: &PersistedAgent) -> Result<bool, String> {
        let digest = persisted.durable_digest();
        let counted = !self.agent_store.is_shared()
            || self
                .presence_written
                .lock()
                .expect("presence lock")
                .takes
                .contains_key(uid);
        if counted && self.written.lock().expect("written lock").get(uid) == Some(&digest) {
            return Ok(true);
        }
        if !self.agent_store.write_report(uid, persisted)? {
            return Ok(false);
        }
        if !counted {
            self.written.lock().expect("written lock").remove(uid);
            return Ok(true);
        }
        self.written
            .lock()
            .expect("written lock")
            .insert(*uid, digest);
        Ok(true)
    }

    /// Answers a report of an Agent whose record another instance forgot while this one wrote it:
    /// the Agent is dropped here too and its session ended, and the reply offers nothing
    /// (ADR-0030 clause 10).
    ///
    /// Made after the report's write found no row, so what it reads is read before the fleet lock
    /// is taken: the re-key read back, and the Client Agents of the group the Agent leaves.
    fn forgotten_elsewhere(&self, uid: &InstanceUid, reported: &InstanceUid) -> Processed {
        let (held, group) = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let record = fleet.get(uid);
            (
                record.is_some(),
                record.and_then(|record| record.placement.group.clone()),
            )
        };
        let unsure = if held {
            self.read_unsure_rekey(uid)
        } else {
            None
        };
        let mut elsewhere = self.elsewhere_of(group.as_slice());
        let gone = {
            let mut fleet = self.fleet.lock().expect("fleet lock");
            self.drop_forgotten_read(&mut fleet, uid, unsure, &mut elsewhere)
        };
        self.place_again(elsewhere);
        // Kept where its row moved through a re-key of this instance's own: the connection goes
        // on carrying it under the identity it reported.
        let kept = !matches!(gone, Gone::Forgotten);
        self.push.send_modify(|rev| *rev += 1);
        Processed {
            reply: ServerToAgent {
                instance_uid: reported.as_bytes().to_vec(),
                capabilities: self.capabilities(),
                ..Default::default()
            },
            uid: kept.then_some(*uid),
            disconnected: false,
        }
    }

    /// Drops an Agent another instance forgot from what this one holds, and ends its session. One
    /// whose row moved through a re-key of this instance's own whose answer was lost is read back
    /// first ([`read_unsure_rekey`](Self::read_unsure_rekey)), and kept where it moved or
    /// where that is not known. A re-key pending to it is dropped with it.
    fn drop_forgotten(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        uid: &InstanceUid,
    ) -> Gone {
        if !fleet.contains_key(uid) {
            return Gone::Forgotten;
        }
        let read = self.read_unsure_rekey(uid);
        self.drop_forgotten_read(fleet, uid, read, &mut Elsewhere::Now)
    }

    /// [`drop_forgotten`](Self::drop_forgotten) from the re-key read back `unsure` read already.
    fn drop_forgotten_read(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        uid: &InstanceUid,
        unsure: Option<(InstanceUid, Result<bool, String>)>,
        elsewhere: &mut Elsewhere,
    ) -> Gone {
        if !fleet.contains_key(uid) {
            return Gone::Forgotten;
        }
        match self.apply_unsure_rekey(fleet, uid, unsure, elsewhere) {
            Gone::Forgotten => {}
            kept => return kept,
        }
        let Some(record) = fleet.remove(uid) else {
            return Gone::Forgotten;
        };
        self.end_session(uid, &record);
        self.drop_kept_restart(uid);
        self.unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .retain(|_, pending| pending.new != *uid);
        self.written.lock().expect("written lock").remove(uid);
        self.presence_written
            .lock()
            .expect("presence lock")
            .remove(uid);
        self.unplace(fleet, uid, record.placement.group.clone(), elsewhere);
        info!(agent = %uid, "dropped an agent forgotten through another instance");
        Gone::Forgotten
    }

    /// Drops one record from the store and the dirty-check ledger — the storage half of
    /// forgetting (ADR-0030) and of an identity reassignment.
    fn unpersist(&self, uid: &InstanceUid) {
        if let Err(e) = self.agent_store.remove(uid) {
            warn!(agent = %uid, error = %e, "cannot remove the agent record from the store");
        }
        self.written.lock().expect("written lock").remove(uid);
    }

    /// Writes the presence of the Agents this instance carries that moved since the last batch, in
    /// one batch naming `holder`, the instance id of the lease it serves under
    /// (ADR-0030 clause 4): a heartbeat reaches the store at
    /// most once per batch and Agent. An Agent it holds and never carried under `holder`, one
    /// restored at startup among them, is written only once it is connected here, so the first
    /// batch writes the Agents carried and not the fleet; under a new holder the connected ones
    /// are written again. An Agent whose take's answer was lost and that disconnected since has its
    /// holder and count of takes read first, and its disconnect is written under that count where
    /// the row names `holder` and the Agent was not taken here again since it was looked at
    /// (ADR-0025
    /// clause 8). Read under the fleet lock and written after it is released, so no report waits
    /// for the batch; its answers are kept only of the Agents nothing was kept of anew meanwhile. A
    /// batch that fails is written again with the next one. Answers how many it wrote.
    ///
    /// # Errors
    /// Returns an error when the batch cannot be written.
    pub fn write_presence(&self, holder: &str) -> Result<usize, String> {
        type Entry = (InstanceUid, Presence, Option<u64>);
        let (mut batch, mut generations, unsure): (Vec<Entry>, HashMap<InstanceUid, u64>, Vec<_>) = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let mut written = self.presence_written.lock().expect("presence lock");
            written.under(holder);
            written.retain(&fleet);
            let mut batch = Vec::new();
            let mut unsure = Vec::new();
            for (uid, record) in fleet.iter() {
                let presence = record.presence();
                if !presence.connected && written.unsure.contains(uid) {
                    unsure.push((*uid, written.generation(uid)));
                    continue;
                }
                let moved = match written.agents.get(uid) {
                    Some(written) => *written != presence,
                    None => presence.connected,
                };
                if moved {
                    batch.push((*uid, presence, written.takes.get(uid).copied()));
                }
            }
            let generations = batch
                .iter()
                .map(|(uid, ..)| (*uid, written.generation(uid)))
                .collect();
            (batch, generations, unsure)
        };
        let mut read = HashSet::new();
        if !unsure.is_empty() {
            let uids: Vec<InstanceUid> = unsure.iter().map(|(uid, _)| *uid).collect();
            let stored = self.agent_store.takes(&uids)?;
            let fleet = self.fleet.lock().expect("fleet lock");
            let mut written = self.presence_written.lock().expect("presence lock");
            for (uid, generation) in unsure {
                let Some(presence) = fleet.get(&uid).map(AgentRecord::presence) else {
                    continue;
                };
                if written.generation(&uid) != generation || presence.connected {
                    continue;
                }
                match stored.get(&uid) {
                    Some((Some(stored), take)) if stored == holder => {
                        batch.push((uid, presence, Some(*take)));
                        generations.insert(uid, generation);
                        read.insert(uid);
                    }
                    // The row names another holder or none, or is gone: nothing of it is ours.
                    _ => {
                        written.unsure.remove(&uid);
                        written.agents.insert(uid, presence);
                        written.changed(uid);
                    }
                }
            }
        }
        if batch.is_empty() {
            return Ok(0);
        }
        batch.sort_by_key(|(uid, ..)| uid.0);
        let takes = self.agent_store.write_presence(holder, &batch)?;
        let mut written = self.presence_written.lock().expect("presence lock");
        // Under another holder since, the presence is written again anyway; the counts are kept,
        // since they outlive the holder they were answered under.
        let same_holder = written.holder.as_deref() == Some(holder);
        for (uid, presence, take) in &batch {
            if generations.get(uid) != Some(&written.generation(uid)) {
                continue;
            }
            if same_holder {
                written.agents.insert(*uid, *presence);
            }
            if let Some(answered) = takes.get(uid) {
                written.takes.insert(*uid, *answered);
                written.unsure.remove(uid);
            } else if read.contains(uid) {
                written.takes.insert(*uid, take.expect("a count read"));
                written.unsure.remove(uid);
            }
            written.changed(*uid);
        }
        Ok(batch.len())
    }

    /// On a shared store, takes `uid` at once as connected here, naming this instance as its
    /// holder, so that no other instance forgets it as not connected while the next batch is due
    /// (ADR-0030 clause 11), and so that an instance that
    /// carried it before stops carrying it
    /// (ADR-0025
    /// clause 8): one write per connect, never per heartbeat. A take that makes the Agent's holder
    /// change more than ten times within a minute is logged as a likely duplicate. Without a lease
    /// nothing is written. One that fails is logged, and the next batch writes it, where no other
    /// instance took the Agent meanwhile.
    ///
    /// Written before the fleet lock is taken, under the Agent's own lock, which
    /// [`release_taken`](Self::release_taken) takes too; its answer is taken up with the record
    /// ([`took`](Self::took)).
    fn take_row(&self, uid: &InstanceUid, last_seen_ms: u64) -> Took {
        if !self.agent_store.is_shared() {
            return Took::Nothing;
        }
        let Some(holder) = self.lease.get().and_then(|lease| lease.borrow().clone()) else {
            return Took::Nothing;
        };
        match self.agent_store.take(uid, &holder, last_seen_ms) {
            Ok(Some(taken)) => {
                if let Some(from) = &taken.from {
                    debug!(agent = %uid, from = %from, "took the agent from another instance");
                }
                if taken.likely_duplicate {
                    warn!(
                        agent = %uid,
                        "the agent's holder changed more than ten times within a minute; \
                         likely a duplicate instance_uid on two connections"
                    );
                }
                Took::Taken {
                    holder,
                    take: taken.take,
                }
            }
            // Forgotten through another instance: this exchange's write finds no record, and
            // drops it.
            Ok(None) => Took::Nothing,
            Err(e) => {
                warn!(agent = %uid, error = %e, "cannot write that the agent is connected here");
                Took::Lost { holder }
            }
        }
    }

    /// Takes up what [`take_row`](Self::take_row) answered, with the presence the record holds
    /// once the report is taken into it. A take whose answer was lost may have been written: its
    /// disconnect is written only under the count read back then, and its record flushed at the
    /// shutdown only under the count answered before, 0 where `created` this record.
    fn took(&self, uid: &InstanceUid, took: Took, presence: Presence, created: bool) {
        let mut written = self.presence_written.lock().expect("presence lock");
        match took {
            Took::Nothing => {}
            Took::Taken { holder, take } => {
                written.under(&holder);
                written.taken(*uid, presence, take);
            }
            Took::Lost { holder } => {
                written.under(&holder);
                written.lost(*uid);
                // The record this instance created counted no take then.
                if created {
                    written.before_lost.insert(*uid, 0);
                }
            }
        }
    }

    /// Stops carrying each Agent of `uids`, every one this instance carries where `None`, whose
    /// stored presence no longer names this instance as its holder: another instance took it
    /// (ADR-0025
    /// clause 8). It is held as on a closed connection, and its disconnect is not written, since
    /// the row is the other instance's. An Agent the row still names is kept, so of two instances
    /// that take one Agent at once only the one the row does not name stops carrying it. The
    /// holders are read under the own locks of the Agents this instance carries or is taking,
    /// which every take of this instance holds until its report holds the Agent in memory: a take
    /// made here before the read is seen, and one another instance's take overtook before this
    /// instance held the Agent is read once it is held. On a store that keeps no presence, or
    /// without a lease, nothing is read.
    ///
    /// # Errors
    /// Returns an error when the holders cannot be read; what is held is then kept.
    pub fn release_taken(&self, uids: Option<&HashSet<InstanceUid>>) -> Result<(), String> {
        if !self.agent_store.is_shared() {
            return Ok(());
        }
        let Some(holder) = self.lease.get().and_then(|lease| lease.borrow().clone()) else {
            return Ok(());
        };
        let named = |uid: &InstanceUid| uids.is_none_or(|uids| uids.contains(uid));
        let carried = |fleet: &HashMap<InstanceUid, AgentRecord>| -> HashSet<InstanceUid> {
            fleet
                .iter()
                .filter(|(uid, record)| record.connected && named(uid))
                .map(|(uid, _)| *uid)
                .collect()
        };
        let mut concerned = carried(&self.fleet.lock().expect("fleet lock"));
        concerned.extend(
            self.taking
                .lock()
                .expect("taking lock")
                .iter()
                .filter(|uid| named(uid))
                .copied(),
        );
        if concerned.is_empty() {
            return Ok(());
        }
        let _agents = self.agent_locks_of(Some(&concerned));
        let held: Vec<InstanceUid> = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let carried = carried(&fleet);
            concerned
                .into_iter()
                .filter(|uid| carried.contains(uid))
                .collect()
        };
        if held.is_empty() {
            return Ok(());
        }
        let taken: Vec<InstanceUid> = self
            .agent_store
            .holders(&held)?
            .into_iter()
            .filter(|(_, stored)| stored.as_deref() != Some(holder.as_str()))
            .map(|(uid, _)| uid)
            .collect();
        if taken.is_empty() {
            return Ok(());
        }
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let mut written = self.presence_written.lock().expect("presence lock");
        for uid in taken {
            let Some(record) = fleet.get_mut(&uid).filter(|record| record.connected) else {
                continue;
            };
            record.connected = false;
            record.owner = None;
            if written.holder.as_deref() == Some(holder.as_str()) {
                written.agents.insert(uid, record.presence());
            }
            written.unsure.remove(&uid);
            written.changed(uid);
            debug!(agent = %uid, "another instance took the agent; this one no longer carries it");
        }
        Ok(())
    }

    /// The Agents forgotten while connection `conn` carried them, taken once: its loop ends their
    /// session (ADR-0030 clause 10).
    pub fn take_forgotten(&self, conn: ConnId) -> Vec<InstanceUid> {
        self.forgotten
            .lock()
            .expect("forgotten lock")
            .remove(&conn)
            .unwrap_or_default()
    }

    /// Notes that `record`, just forgotten as `uid`, was carried by a live WebSocket, whose loop
    /// then ends its session.
    fn end_session(&self, uid: &InstanceUid, record: &AgentRecord) {
        if let (true, Some(conn)) = (record.connected, record.owner) {
            self.forgotten
                .lock()
                .expect("forgotten lock")
                .entry(conn)
                .or_default()
                .push(*uid);
        }
    }

    /// Writes every record's current state, timestamps and sequence numbers included — the
    /// graceful-shutdown flush (ADR-0030) that lets the ordinary restart restore a fleet whose
    /// `last_seen` is current and whose next compressed report is accepted without a gap. On a
    /// shared store only the records of the Agents this instance heard from are written: of the
    /// others it holds what it loaded, and another instance carries them
    /// (ADR-0030 clause 2). Of those, only the ones no other
    /// instance holds the connection of: an Agent heard here that moved to another instance keeps
    /// what that instance wrote of it. Whether one moved is told by the count of takes this
    /// instance was last answered, never by comparing its clock with another's.
    pub fn flush_agents(&self) {
        let fleet = self.fleet.lock().expect("fleet lock");
        let shared = self.agent_store.is_shared();
        let holder = self.lease.get().and_then(|lease| lease.borrow().clone());
        // The counts outlive the lease they were answered under: a count still stored says no
        // instance took the Agent since, whatever lease this instance serves under now.
        // An Agent whose take lost its answer is flushed under the count answered before it, which
        // a take counted since, also its own lost one, no longer matches.
        let takes = {
            let presence = self.presence_written.lock().expect("presence lock");
            let mut takes = presence.before_lost.clone();
            takes.extend(presence.takes.iter().map(|(uid, take)| (*uid, *take)));
            takes
        };
        let mut written = self.written.lock().expect("written lock");
        for (uid, record) in fleet.iter().filter(|(_, record)| record.heard || !shared) {
            let persisted = record.to_persisted();
            match self.agent_store.write_report_unless_held_elsewhere(
                uid,
                &persisted,
                holder.as_deref(),
                takes.get(uid).copied(),
            ) {
                Ok(true) => {
                    written.insert(*uid, persisted.durable_digest());
                }
                // Forgotten through another instance, or held by another: not written.
                Ok(false) => {}
                Err(e) => warn!(agent = %uid, error = %e, "cannot flush the agent record"),
            }
        }
    }

    /// The control loop for one report, shared by both transports (ADR-0012): update what we know,
    /// then answer with what the Agent still lacks — the config offer gated by the hash comparison.
    /// `conn` identifies the WebSocket connection that carried the report; `None` for plain HTTP.
    ///
    /// The reported `instance_uid` is taken at face value: admission proved fleet membership, not
    /// which Agent is speaking, so within an admitted fleet a report's identity is self-asserted and
    /// not authorized against any other Agent (ADR-0026). The plain-HTTP path in particular offers
    /// nothing to tell two pollers apart; the WebSocket duplicate-`instance_uid` rekey below is
    /// collision handling, not authorization.
    pub fn process(
        &self,
        msg: AgentToServer,
        transport: Transport,
        conn: Option<ConnId>,
    ) -> Processed {
        self.process_presented(msg, transport, conn, None)
    }

    /// [`process`](Self::process) for a connection that presented a certificate: the host it was
    /// issued to binds the Agents it reports for, and a CSR without a renewal proof renews it
    /// unless its host is a Gateway's (ADR-0027 clause 11, ADR-0016).
    pub fn process_presented(
        &self,
        msg: AgentToServer,
        transport: Transport,
        conn: Option<ConnId>,
        presented: Option<&Presented>,
    ) -> Processed {
        let Some(reported) = InstanceUid::from_wire(&msg.instance_uid) else {
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
        // The Agent's own lock, held across everything this report reads and writes in the
        // store: two reports of one Agent are taken up and written in order, a take is not
        // overtaken by the reading of the holder again, and a restart's clear by a request, a try
        // or a forget. The fleet lock is taken only to take into memory what was read and
        // written, so no other Agent's report waits for a call to the store.
        let _agent = self.agent_lock(&reported);
        let mut msg = msg;
        for _ in 0..REPORT_ROUNDS {
            match self.report_round(msg, transport, conn, presented, reported) {
                Ok(processed) => return processed,
                Err(again) => msg = *again,
            }
        }
        warn!(
            agent = %reported,
            "the agent left memory while each round of its report read the store"
        );
        Processed {
            reply: unavailable(
                &msg.instance_uid,
                "the Server cannot take this report up now; retry later",
            ),
            uid: None,
            disconnected: false,
        }
    }

    /// One round of [`process_presented`](Self::process_presented): what the report needs of the
    /// store is read and written first, then taken into memory under the fleet lock. A round
    /// whose Agent left memory meanwhile — forgotten, or moved through another instance — hands
    /// the report back, and the next round goes on from what the store holds then.
    fn report_round(
        &self,
        msg: AgentToServer,
        transport: Transport,
        conn: Option<ConnId>,
        presented: Option<&Presented>,
        reported: InstanceUid,
    ) -> Result<Processed, Box<AgentToServer>> {
        let mut sender = msg.instance_uid.clone();
        // The reply is addressed to the identity the Agent reported, even when it is re-keyed
        // below: the Baseline makes a reply's instance_uid match the message's, and the Agent
        // routes by it — a reply addressed to the new identity reaches no one, and the Agent goes
        // on reporting under the old one. The new identity travels in agent_identification alone.
        let mut uid = reported;
        // One moment for the round's two looks at a stale carrier, so the take-up judges it as the
        // first look did.
        let now_ms = self.clock.now_ms();
        let mut loaded = self.hold_from_store(&uid);
        let mut reply_flags = 0u64;
        let mut identification = None;

        // An instance_uid belongs to the host whose certificate first reported it (ADR-0027
        // clause 11): a certificate of another host does not speak for it, nor re-keys it. Such a
        // reporter is re-keyed — it gets an identity of its own, never the one it claimed. The
        // binding is made only once the report is admitted below.
        let mut binding = None;
        if let (Some(host), Some(revocations)) =
            (presented.and_then(|p| p.host.as_deref()), &self.revocations)
        {
            if revocations.may_report(host, uid.as_bytes()).is_err() {
                let new_uid = InstanceUid::default();
                warn!(
                    claimed = %uid, new = %new_uid, host,
                    "an Agent of another host was claimed; rekeying the reporter"
                );
                self.audit_refusal(
                    Entry::new("identity.claimed", "rekeyed")
                        .with("claimed", uid.to_string())
                        .with("host", host.to_string()),
                );
                identification = Some(AgentIdentification {
                    new_instance_uid: new_uid.as_bytes().to_vec(),
                });
                uid = new_uid;
                // A CSR in the same message must not name the claimed identity either.
                sender = uid.as_bytes().to_vec();
                if let Err(e) = revocations.may_report(host, uid.as_bytes()) {
                    return Ok(Processed {
                        reply: bad_request(&e),
                        uid: None,
                        disconnected: false,
                    });
                }
            }
            binding = Some((host, uid));
        }

        // The Agent asked the Server to assign its identity (AgentToServerFlags_RequestInstanceUid):
        // mint a UUID v7 and re-key the record; the reply tells the Agent to adopt it. A re-key
        // whose answer was lost and could not be read back is read back first, whether the Agent
        // asks again or not: moved, the identity it moved to is assigned now.
        let mut rekeyed = None;
        let unsure = if identification.is_none() {
            self.read_back_unsure_rekey(&uid)
        } else {
            UnsureRekey::None
        };
        let asked = identification.is_none()
            && msg.flags & AgentToServerFlags::RequestInstanceUid as u64 != 0;
        let moves = matches!(unsure, UnsureRekey::Moved(_))
            || (asked && matches!(unsure, UnsureRekey::None));
        let (held, to_move) = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let to_move = (moves && unsure == UnsureRekey::None)
                .then(|| fleet.get(&uid).map(AgentRecord::to_persisted))
                .flatten();
            (fleet.contains_key(&uid), to_move)
        };
        // While it is not known, a report under the old identity of which nothing is held creates
        // no record: the row may have moved.
        if unsure == UnsureRekey::Unknown && !held {
            return Ok(Processed {
                reply: unavailable(
                    &msg.instance_uid,
                    "the Server cannot tell yet whether this Agent's identity moved; retry later",
                ),
                uid: None,
                disconnected: false,
            });
        }
        if moves {
            // The stored record follows the identity in one step (ADR-0030 clause 14), before what
            // is held does; what this report changes is written by the dirty check at this
            // exchange's end. A move that fails leaves the Agent under the identity it has, and
            // the reply assigns none, so the Agent asks again with its next report. Until what is
            // held follows, the move is kept as pending, so a read again that finds the old row
            // gone meanwhile reads it back rather than drop the Agent.
            let (new_uid, moved) = match unsure {
                UnsureRekey::Moved(new_uid) => (new_uid, Ok(())),
                _ => {
                    let new_uid = InstanceUid::default();
                    let moved = match &to_move {
                        None => Ok(()),
                        Some(record) => {
                            self.unsure_rekeys.lock().expect("rekeys lock").insert(
                                uid,
                                PendingRekey {
                                    new: new_uid,
                                    moved: false,
                                },
                            );
                            self.rekey_reading_back(&uid, &new_uid, record)
                                .map_err(|(e, kept)| {
                                    if !kept {
                                        self.unsure_rekeys
                                            .lock()
                                            .expect("rekeys lock")
                                            .remove(&uid);
                                    }
                                    e
                                })
                        }
                    };
                    (new_uid, moved)
                }
            };
            match moved {
                Ok(()) => {
                    // The new identity is placed below.
                    info!(old = %uid, new = %new_uid, "assigned a server-generated instance_uid");
                    identification = Some(AgentIdentification {
                        new_instance_uid: new_uid.as_bytes().to_vec(),
                    });
                    rekeyed = Some((uid, new_uid));
                    uid = new_uid;
                }
                Err(e) => {
                    warn!(
                        agent = %uid, error = %e,
                        "cannot move the agent record; it keeps its identity"
                    );
                }
            }
        }

        // What memory holds of the Agent, read once, the record a re-key moves standing for the
        // one under its new identity.
        //
        // Duplicate instance_uid detection (a Baseline SHOULD): the same identity alive on a
        // WebSocket connection of *another peer* — bad UID generators, cloned VMs — is rekeyed
        // exactly as the Baseline prescribes: mint a fresh uid and answer with
        // AgentIdentification, which the Client adopts. The newcomer starts a record of its own;
        // the incumbent and its connection stay untouched. (Stateless plain-HTTP polling offers
        // nothing to distinguish two pollers by, so detection is WebSocket-only.) An Agent that
        // comes back on a new connection of the host whose certificate its old connection
        // presented is not a duplicate of itself: the newest connection takes it, and the older
        // one's reports no longer take it back
        // (ADR-0025
        // clause 8).
        let arrived_over = self
            .revocations
            .as_ref()
            .and(presented.and_then(|p| p.host.clone()));
        let described = msg.agent_description.is_some();
        let mut superseded = false;
        let mut seen = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let mut record = fleet
                .get(&uid)
                .or_else(|| rekeyed.and_then(|(old, _)| fleet.get(&old)));
            if let Some(this_conn) = conn {
                match record.map_or(Carried::Not, |existing| {
                    Carried::of(
                        existing,
                        this_conn,
                        arrived_over.as_deref(),
                        now_ms,
                        self.heartbeat_interval(),
                    )
                }) {
                    Carried::Not => {}
                    Carried::ByOlder => {
                        info!(
                            agent = %uid,
                            "the agent came back on a new connection of its host, which carries it now"
                        );
                    }
                    Carried::ByStale => {
                        info!(
                            agent = %uid,
                            "the connection of its host that carried the agent carried none of its \
                             reports for a heartbeat interval; this one carries it now"
                        );
                    }
                    Carried::ByNewer => superseded = true,
                    Carried::ByAnotherPeer => {
                        let new_uid = InstanceUid::default();
                        warn!(duplicate = %uid, new = %new_uid, "duplicate instance_uid; rekeying the newcomer");
                        identification = Some(AgentIdentification {
                            new_instance_uid: new_uid.as_bytes().to_vec(),
                        });
                        uid = new_uid;
                        record = None;
                    }
                }
            }
            Seen::of(record, |record| {
                (!described)
                    .then(|| self.group_of(arrived_over.as_deref(), record.description.as_ref()))
                    .flatten()
            })
        };

        // Admitting a genuinely new Agent past the ceiling would let a peer cycling self-asserted
        // UIDs (ADR-0026) grow the in-memory map and its per-Agent disk mirror without bound. Known
        // Agents keep reporting; only a *new* UID at capacity is refused, `Unavailable` so a Client
        // that legitimately raced in retries rather than gives up. The real gate is admission
        // (ADR-0026); this bounds the damage while the endpoint is open. On a shared store the
        // ceiling bounds the records stored, counted by the store as it creates this one, so it
        // holds for the fleet and not for one instance. A record stored already passes none: it
        // is another instance's, which the read before the lock missed, and this report goes on
        // from it as from one read then. On a store this Server alone writes, the new Agent is
        // counted against what memory holds and what other reports counted and have not yet
        // taken into it.
        let mut created = false;
        let mut reserved = Reservation::none(&self.reserved_new);
        if !seen.known {
            let at_ceiling = if self.agent_store.is_shared() {
                let persisted = self
                    .new_record(msg.sequence_num, transport, conn)
                    .to_persisted();
                match self
                    .agent_store
                    .create_within(&uid, &persisted, self.max_agents)
                {
                    Ok(Creation::Created) => {
                        info!(agent = %uid, transport = transport.as_str(), "new agent");
                        created = true;
                        // Held from its commit on, not yet carried, as a record found is: what
                        // another instance commits to it from now on and announces is taken up
                        // into it, and the report goes on from it.
                        let digest = persisted.durable_digest();
                        let mut fleet = self.fleet.lock().expect("fleet lock");
                        if let std::collections::hash_map::Entry::Vacant(held) = fleet.entry(uid) {
                            self.written
                                .lock()
                                .expect("written lock")
                                .insert(uid, digest);
                            held.insert(AgentRecord::from_persisted(persisted));
                        }
                        false
                    }
                    Ok(Creation::Found(stored)) => {
                        let digest = stored.durable_digest();
                        let found = AgentRecord::from_persisted(*stored);
                        let mut fleet = self.fleet.lock().expect("fleet lock");
                        let joined = |record: &AgentRecord| {
                            (!described)
                                .then(|| {
                                    self.group_of(
                                        arrived_over.as_deref(),
                                        record.description.as_ref(),
                                    )
                                })
                                .flatten()
                        };
                        if let Some(record) = fleet.get(&uid) {
                            seen = Seen::of(Some(record), joined);
                        } else {
                            self.written
                                .lock()
                                .expect("written lock")
                                .insert(uid, digest);
                            seen = Seen::of(Some(&found), joined);
                            fleet.insert(uid, found);
                        }
                        loaded = true;
                        false
                    }
                    Ok(Creation::Full) => true,
                    Err(e) => {
                        warn!(agent = %uid, error = %e, "cannot store a new agent's record");
                        return Ok(Processed {
                            reply: unavailable(
                                &msg.instance_uid,
                                "the Server cannot store this Agent's record now; retry later",
                            ),
                            uid: None,
                            disconnected: false,
                        });
                    }
                }
            } else {
                let fleet = self.fleet.lock().expect("fleet lock");
                reserved = Reservation::of(&self.reserved_new, fleet.len(), self.max_agents);
                !reserved.held()
            };
            if at_ceiling {
                warn!(
                    agent = %uid,
                    max_agents = self.max_agents,
                    "refusing a new agent: the fleet is at its record ceiling"
                );
                return Ok(Processed {
                    reply: unavailable(
                        &msg.instance_uid,
                        "the Server is at its Agent-record ceiling; retry later",
                    ),
                    uid: None,
                    disconnected: false,
                });
            }
        }
        if let Some(revocations) = &self.revocations {
            if let Some((host, bound)) = binding {
                let subject = presented
                    .map(|p| p.subject_values.clone())
                    .unwrap_or_default();
                // The binding exists only for a presented certificate that names a host.
                let not_after_ms = presented.map_or(0, |p| p.not_after_ms);
                if let Err(e) =
                    revocations.check_report(host, &subject, not_after_ms, bound.as_bytes())
                {
                    // A record this report created goes with it, from the store and from what
                    // is held; one another instance created stays. A re-key made stands, and what
                    // is held follows it.
                    if created {
                        self.unpersist(&uid);
                        self.fleet.lock().expect("fleet lock").remove(&uid);
                    }
                    self.follow_move(rekeyed, seen.group.clone());
                    return Ok(Processed {
                        reply: bad_request(&e),
                        uid: None,
                        disconnected: false,
                    });
                }
            }
            // A re-key an Agent asks for keeps its host (ADR-0027 clause 11).
            if let Some((old, new)) = rekeyed {
                if let Err(e) = revocations.rebind(old.as_bytes(), new.as_bytes()) {
                    warn!(error = %e, "cannot move the host binding to the new instance_uid");
                }
            }
        }

        // The take of an Agent this connection begins to carry, written now: its answer is taken
        // up with the record below.
        // A report of an older connection of the host whose newer one carries the Agent takes it
        // no more.
        let connects = !superseded && (created || !seen.connected || seen.owner != conn);
        let _taking = connects.then(|| Taking::of(&self.taking, uid));
        let took = if connects {
            self.take_row(&uid, self.clock.now_ms())
        } else {
            Took::Nothing
        };

        // What places it in its Client and under its Host may have changed (ADR-0030): a new
        // record, a new Host, a new description, or a new identity — a re-key took the old one
        // out of its group, and a compressed message carries no description to place it by. The
        // Client Agents of the groups it leaves and joins that another instance carries are read
        // now.
        let moved = seen.host != arrived_over;
        let places = !seen.known || moved || described || rekeyed.is_some() || loaded;
        let joined = if described || !seen.known {
            self.group_of(arrived_over.as_deref(), msg.agent_description.as_ref())
        } else {
            seen.joined.clone()
        };
        let groups: Vec<Group> = if places || rekeyed.is_some() {
            [seen.group.clone(), joined].into_iter().flatten().collect()
        } else {
            Vec::new()
        };
        let mut elsewhere = self.elsewhere_of(&groups);

        let mut fleet = self.fleet.lock().expect("fleet lock");
        if let Some((old, new)) = rekeyed {
            if fleet.contains_key(&old) {
                self.rekey_held(&mut fleet, &old, new, &mut elsewhere);
            }
            let mut pending = self.unsure_rekeys.lock().expect("rekeys lock");
            if pending.get(&old).is_some_and(|pending| pending.new == new) {
                pending.remove(&old);
            }
        }
        // Whether a newer connection of the host took the Agent while this round read and wrote
        // the store, or still carries it: this connection then leaves it to that one.
        let superseded = conn.is_some_and(|this_conn| {
            fleet.get(&uid).is_some_and(|record| {
                Carried::of(
                    record,
                    this_conn,
                    arrived_over.as_deref(),
                    now_ms,
                    self.heartbeat_interval(),
                ) == Carried::ByNewer
            })
        });
        let now_connects = created
            || fleet
                .get(&uid)
                .is_none_or(|record| !record.connected || record.owner != conn);
        let left = (seen.known || created) && rekeyed.is_none() && !fleet.contains_key(&uid);
        // A round that re-keyed the Agent goes on under the identity it assigned: a next round
        // would start from the message under the old one and assign a second. It finds the Agent
        // no longer carried, its connection closed meanwhile, and leaves it so: it wrote no take,
        // and nothing would mark the Agent disconnected again.
        let uncarried = now_connects && !connects && !superseded;
        if left || (uncarried && rekeyed.is_none()) {
            // Forgotten or moved through another instance, or no longer carried, while this
            // round read and wrote the store: the next round goes on from what it holds now.
            drop(fleet);
            self.place_again(elsewhere);
            return Err(Box::new(msg));
        }
        let known = seen.known;
        let record = fleet.entry(uid).or_insert_with(|| {
            info!(agent = %uid, transport = transport.as_str(), "new agent");
            self.new_record(msg.sequence_num, transport, conn)
        });
        reserved.release();

        // A compressed report (unchanged fields omitted) is only usable if our state is current.
        // A gap in sequence_num — or an Agent we have never seen describing itself with nothing —
        // means state was lost somewhere; the Baseline's recovery is to demand a full report.
        let compressed = msg.agent_description.is_none();
        let gap = known && msg.sequence_num != record.sequence_num.wrapping_add(1);
        // Counting from the start again is an Agent that restarted. A gap alone is not: an Agent
        // that never counts shows one on every message.
        let restarted = known && msg.sequence_num < record.sequence_num;
        if compressed && (!known || gap) {
            reply_flags |= ServerToAgentFlags::ReportFullState as u64;
        }

        // The Host it arrived over (ADR-0030 clause 16) — none without a host register.
        let moved = record.host != arrived_over;
        record.host = arrived_over;

        record.sequence_num = msg.sequence_num;
        record.transport = transport;
        // A demand for the component map may have been lost with the previous connection, or with
        // an Agent that restarted, which plain HTTP shows instead of a connection, so either may be
        // asked again.
        if !uncarried && !superseded {
            if !record.connected || record.owner != conn || restarted {
                record.components_demanded = None;
            }
            record.connected = true;
            record.owner = conn;
            // When the take-up commits, not when the round began: a round that waited on the store
            // leaves its carrier as fresh as its report.
            record.carried_ms = self.clock.now_ms();
        }
        record.heard = true;
        record.last_seen_ms = self.clock.now_ms();
        if connects {
            self.took(&uid, took, record.presence(), created);
        }
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
            self.take_up_package_statuses(&uid, record, statuses);
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

        if !known || moved || described || rekeyed.is_some() || loaded {
            self.place(&mut fleet, &uid, &mut elsewhere);
        }
        let record = fleet.get_mut(&uid).expect("the record was just written");

        // The Agent asked to be issued a client certificate (ADR-0026); it is signed once the
        // fleet lock is let go.
        let request = msg
            .connection_settings_request
            .as_ref()
            .and_then(|request| request.opamp.as_ref())
            .and_then(|opamp| opamp.certificate_request.as_ref())
            .map(|request| request.csr.clone());
        let mut ending = Ending {
            uid,
            reported,
            disconnected: msg.agent_disconnect.is_some(),
            superseded,
            identification,
            reply_flags,
        };
        let pending = match request {
            None => Pending::Settled(self.settle(record, &mut ending, None)),
            Some(csr) => Pending::Issue(Box::new(record.to_persisted()), csr),
        };
        drop(fleet);
        self.place_again(elsewhere);
        Ok(self.finish(pending, &sender, presented, ending))
    }

    /// Takes a report's package statuses into `record`: an outcome that moved is audited, and a
    /// failed installation and an offer the Agent refused are logged.
    fn take_up_package_statuses(
        &self,
        uid: &InstanceUid,
        record: &mut AgentRecord,
        statuses: PackageStatuses,
    ) {
        use opamp::proto::PackageStatusEnum as P;
        for (name, status) in &statuses.packages {
            let before = record
                .package_statuses
                .as_ref()
                .and_then(|previous| previous.packages.get(name))
                .map(|previous| (previous.status, previous.agent_has_version.clone()));
            let outcome = if status.status == P::Installed as i32 {
                Some("installed")
            } else if status.status == P::InstallFailed as i32 {
                Some("failed")
            } else {
                None
            };
            if let (Some(outcome), true) = (
                outcome,
                before != Some((status.status, status.agent_has_version.clone())),
            ) {
                self.audit_refusal(
                    Entry::new("package.outcome", outcome)
                        .with("instance_uid", uid.to_string())
                        .with("package", name.clone())
                        .with("version", status.agent_has_version.clone())
                        .with("offered_version", status.server_offered_version.clone())
                        .with(
                            "error",
                            (!status.error_message.is_empty())
                                .then(|| status.error_message.clone()),
                        ),
                );
            }
        }
        for status in statuses.packages.values() {
            if status.status == P::InstallFailed as i32 {
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

    /// Moves what this instance holds to the identity a re-key moved the stored record to, where
    /// a report that made the move is refused before it takes it into memory.
    fn follow_move(&self, rekeyed: Option<(InstanceUid, InstanceUid)>, left: Option<Group>) {
        let Some((old, new)) = rekeyed else {
            return;
        };
        let mut elsewhere = self.elsewhere_of(left.as_slice());
        {
            let mut fleet = self.fleet.lock().expect("fleet lock");
            if fleet.contains_key(&old) {
                self.rekey_held(&mut fleet, &old, new, &mut elsewhere);
            }
            let mut pending = self.unsure_rekeys.lock().expect("rekeys lock");
            if pending.get(&old).is_some_and(|pending| pending.new == new) {
                pending.remove(&old);
            }
        }
        self.place_again(elsewhere);
    }

    /// Under the fleet lock, what a report's end makes of `record` once a certificate it asked
    /// for is `issued` or refused: its goodbye, the demand for its component map, and the reply,
    /// or a queued restart that goes out instead, whose clear is written once the lock is let go.
    fn settle(
        &self,
        record: &mut AgentRecord,
        ending: &mut Ending,
        issued: Option<TlsCertificate>,
    ) -> Settled {
        if ending.disconnected && !ending.superseded {
            info!(agent = %ending.uid, "agent disconnected");
            record.connected = false;
            record.owner = None;
        }

        // A hash without the map is an offer to fetch: demand the full component list from an
        // Agent that declared it can report one (the flag is meaningless toward any other), once
        // per hash on a connection.
        if !ending.disconnected
            && !ending.superseded
            && record.capabilities
                & opamp::proto::AgentCapabilities::ReportsAvailableComponents as u64
                != 0
        {
            if let Some(stored) = record
                .available_components
                .as_ref()
                .filter(|ac| ac.components.is_empty())
            {
                if record.components_demanded.as_ref() != Some(&stored.hash) {
                    record.components_demanded = Some(stored.hash.clone());
                    ending.reply_flags |= ServerToAgentFlags::ReportAvailableComponents as u64;
                }
            }
        }

        // A queued restart goes out as the Baseline's command-only message: nothing but
        // identity, capabilities, and the command. Anything else the reply would carry —
        // an identity reassignment, a demanded full report — defers the command to the next
        // exchange instead of being combined with it.
        if record.restart_pending
            && !ending.disconnected
            && !ending.superseded
            && ending.identification.is_none()
            && ending.reply_flags == 0
            && record.capabilities & opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64
                != 0
        {
            return Settled::Restart(issued);
        }
        Settled::Done(Box::new((
            self.reply_of(record, ending, issued),
            record.to_persisted(),
        )))
    }

    /// The reply to a report, with the offers `record` is due.
    fn reply_of(
        &self,
        record: &AgentRecord,
        ending: &Ending,
        issued: Option<TlsCertificate>,
    ) -> Processed {
        // The config offer — composed from this Agent's assignments (ADR-0033), gated by the
        // hash comparison, and only toward an Agent that both said goodbye ≠ true and declared
        // AcceptsRemoteConfig (capability negotiation is binding). Matching proposes; only an
        // operator's rollout act made anything an assignment.
        let remote_config = if ending.disconnected {
            None
        } else {
            let desired = self.configs.compose(record.assigned_configurations());
            offer(record, desired.as_ref())
        };

        // The connection-settings offer (ADR-0013), gated the same way: by capability and by
        // the hash the Agent last reported — the Baseline's own "compare and include" MUST.
        let connection_settings = if ending.disconnected {
            None
        } else {
            self.settings_offer(record, issued)
        };

        // The package offer (ADR-0034), gated by capability and the reported
        // server_provided_all_packages_hash — the Baseline's "compare and include" for packages.
        let packages_available = if ending.disconnected {
            None
        } else {
            self.packages_offer(record)
        };

        Processed {
            reply: ServerToAgent {
                instance_uid: ending.reported.as_bytes().to_vec(),
                capabilities: self.capabilities(),
                flags: ending.reply_flags,
                remote_config,
                connection_settings,
                packages_available,
                agent_identification: ending.identification.clone(),
                ..Default::default()
            },
            uid: Some(ending.uid),
            disconnected: ending.disconnected,
        }
    }

    /// The end of a report once its record is taken into memory, with the fleet lock let go: a
    /// certificate it asked for is signed and recorded, a queued restart cleared, and everything
    /// the report changed is persisted if it moved the durable state (ADR-0030) — a heartbeat did
    /// not, and writes nothing — before the Agent is answered.
    fn finish(
        &self,
        pending: Pending,
        sender: &[u8],
        presented: Option<&Presented>,
        mut ending: Ending,
    ) -> Processed {
        let settled = match pending {
            Pending::Issue(persisted, csr) => {
                match self.issue(csr, sender, presented, &ending, &persisted) {
                    Ok(issued) => {
                        let mut fleet = self.fleet.lock().expect("fleet lock");
                        let Some(record) = fleet.get_mut(&ending.uid) else {
                            drop(fleet);
                            return self.forgotten_elsewhere(&ending.uid, &ending.reported);
                        };
                        self.settle(record, &mut ending, Some(issued))
                    }
                    Err(answered) => return *answered,
                }
            }
            Pending::Settled(settled) => settled,
        };
        let (processed, persisted) = match settled {
            Settled::Done(done) => *done,
            Settled::Restart(issued) => {
                let cleared = self.clear_restart(&ending.uid);
                let mut fleet = self.fleet.lock().expect("fleet lock");
                let Some(record) = fleet.get_mut(&ending.uid) else {
                    drop(fleet);
                    return self.forgotten_elsewhere(&ending.uid, &ending.reported);
                };
                if self.take_restart(&ending.uid, record, cleared) {
                    let processed = Processed {
                        reply: restart_command(&ending.uid, self.capabilities()),
                        uid: Some(ending.uid),
                        disconnected: false,
                    };
                    (processed, record.to_persisted())
                } else {
                    (
                        self.reply_of(record, &ending, issued),
                        record.to_persisted(),
                    )
                }
            }
        };
        if !self.persist_if_dirty(&ending.uid, &persisted) {
            return self.forgotten_elsewhere(&ending.uid, &ending.reported);
        }
        processed
    }

    /// Signs and records the certificate a report asked for (ADR-0026): signing it here, on the
    /// connection it arrived over, is the whole of the approval, since admission already required
    /// every proof this endpoint asks of any message, which is what the Baseline's flow means by
    /// awaiting one. A request the Server refuses, or one whose record cannot be written, which
    /// the Agent retries (ADR-0028 clause 5), is answered here, the report's record persisted.
    fn issue(
        &self,
        csr: Vec<u8>,
        sender: &[u8],
        presented: Option<&Presented>,
        ending: &Ending,
        persisted: &PersistedAgent,
    ) -> Result<TlsCertificate, Box<Processed>> {
        let uid = ending.uid;
        // `Ok(Err(..))` is a request the Server refuses; `Err(..)` a record it cannot write,
        // which the Agent retries (ADR-0028 clause 5).
        let outcome: Result<Result<Signed, String>, String> = match &self.client_ca {
            // The Baseline's MUST when the Server cannot act on the request. An Agent reaching
            // here ignored the undeclared capability, so it is a client error.
            None => Ok(Err("this Server issues no client certificates".to_string())),
            Some(ca) => match String::from_utf8(csr)
                .map_err(|_| "the certificate signing request is not PEM".to_string())
                .and_then(|csr| {
                    // The message's own instance_uid, before any re-key (ADR-0026).
                    ca.check_claims(&csr, sender)?;
                    let (predecessor, host, subject, use_case) =
                        self.renews(ca.as_ref(), &csr, presented)?;
                    Ok((ca.sign(&csr, &host, &subject, use_case)?, predecessor))
                }) {
                Err(e) => Ok(Err(e)),
                Ok((signed, predecessor)) => self
                    .record_issued(&signed, sender, predecessor)
                    .map(|()| Ok(signed)),
            },
        };
        let refused = match outcome {
            Ok(Ok(signed)) => {
                info!(
                    agent = %uid,
                    serial = %signed.facts.id.serial,
                    "issued a client certificate"
                );
                return Ok(TlsCertificate {
                    cert: signed.pem.into_bytes(),
                    // The Agent generated its own key and keeps it — the point of the CSR flow —
                    // so the Server has nothing to put here and must not invent it.
                    private_key: Vec::new(),
                    ..Default::default()
                });
            }
            Err(e) => {
                warn!(agent = %uid, error = %e, "held back a certificate: the audit record is unavailable");
                unavailable(ending.reported.as_bytes(), &e)
            }
            Ok(Err(e)) => {
                warn!(agent = %uid, error = %e, "refused a certificate signing request");
                self.audit_refusal(
                    Entry::new("issuance.refused", "refused")
                        .with("instance_uid", uid.to_string())
                        .with("reason", e.clone()),
                );
                bad_request(&e)
            }
        };
        // The report's updates are already in the record, so they are persisted even though no
        // certificate is issued (ADR-0030).
        if !self.persist_if_dirty(&uid, persisted) {
            return Err(Box::new(self.forgotten_elsewhere(&uid, &ending.reported)));
        }
        Err(Box::new(Processed {
            reply: refused,
            uid: Some(uid),
            disconnected: false,
        }))
    }

    /// The record of an Agent first heard of now.
    fn new_record(
        &self,
        sequence_num: u64,
        transport: Transport,
        conn: Option<ConnId>,
    ) -> AgentRecord {
        AgentRecord {
            labels: BTreeMap::new(),
            host: None,
            placement: Placement::default(),
            holder: None,
            sequence_num,
            capabilities: 0,
            description: None,
            health: None,
            effective_config: None,
            remote_config_status: None,
            transport,
            connected: true,
            last_seen_ms: self.clock.now_ms(),
            restart_pending: false,
            available_components: None,
            components_demanded: None,
            connection_settings_status: None,
            package_statuses: None,
            owner: conn,
            carried_ms: self.clock.now_ms(),
            heard: true,
            // A new Agent waits (ADR-0033 clause 10): it is assigned nothing until an operator's
            // rollout act says so, and the fleet view shows what it could get.
            assignment: None,
        }
    }

    /// The index of `uid`'s own lock among [`AGENT_LOCKS`].
    fn agent_lock_index(uid: &InstanceUid) -> usize {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::hash::DefaultHasher::new();
        uid.0.hash(&mut hasher);
        (hasher.finish() % AGENT_LOCKS as u64) as usize
    }

    /// Takes the lock at `index` among the Agents' own locks, counting a wait.
    fn agent_lock_at(&self, index: usize) -> std::sync::MutexGuard<'_, ()> {
        let lock = &self.agent_locks[index];
        match lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                self.agent_lock_waits.fetch_add(1, Ordering::SeqCst);
                lock.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            }
        }
    }

    /// Takes `uid`'s own lock. Never taken while the fleet lock is held.
    fn agent_lock(&self, uid: &InstanceUid) -> std::sync::MutexGuard<'_, ()> {
        self.agent_lock_at(Self::agent_lock_index(uid))
    }

    /// Takes the own locks of the Agents of `uids`, every one where `None`, in the order of
    /// their index.
    fn agent_locks_of(
        &self,
        uids: Option<&HashSet<InstanceUid>>,
    ) -> Vec<std::sync::MutexGuard<'_, ()>> {
        let indexes: BTreeSet<usize> = match uids {
            Some(uids) => uids.iter().map(Self::agent_lock_index).collect(),
            None => (0..AGENT_LOCKS).collect(),
        };
        indexes
            .into_iter()
            .map(|index| self.agent_lock_at(index))
            .collect()
    }

    /// How often an Agent's own lock was found taken and waited for.
    pub fn agent_lock_waits(&self) -> u64 {
        self.agent_lock_waits.load(Ordering::SeqCst)
    }

    /// How a report's placement reads the Client Agents of `groups` held elsewhere: read now,
    /// before the fleet lock is taken, on a shared store; on a store this Server alone writes
    /// there is nothing to read.
    fn elsewhere_of(&self, groups: &[Group]) -> Elsewhere {
        if !self.agent_store.is_shared() {
            return Elsewhere::Now;
        }
        Elsewhere::Read {
            read: self.read_elsewhere(groups),
            missed: Vec::new(),
        }
    }

    /// Places again what a placement from `elsewhere` found unread; called with no lock held.
    fn place_again(&self, elsewhere: Elsewhere) {
        if let Elsewhere::Read { missed, .. } = elsewhere {
            self.place_groups_again(missed);
        }
    }

    /// The group an Agent arriving over `host` and described by `description` is placed in, as
    /// [`locate`](Self::locate) places it.
    fn group_of(
        &self,
        host: Option<&str>,
        description: Option<&AgentDescription>,
    ) -> Option<Group> {
        let (Some(host), Some(revocations)) = (host, &self.revocations) else {
            return None;
        };
        if revocations.placement(host).0 {
            return crate::labels::machine(description).map(|machine| Group::Behind {
                gateway: host.to_string(),
                machine,
            });
        }
        Some(Group::Host(host.to_string()))
    }

    /// On a shared store, loads the record of `uid` from it when this Server does not hold one, as
    /// at an Agent's first report here: another instance may have written it, and its labels, its
    /// assignment and its queued restart are what this report goes on from
    /// (ADR-0025
    /// clause 8). Read before the fleet lock is taken, so no other report waits for it. Answers
    /// whether it loaded one; one that cannot be read is logged, and the report goes on as an
    /// Agent's first.
    fn hold_from_store(&self, uid: &InstanceUid) -> bool {
        if !self.agent_store.is_shared() || self.fleet.lock().expect("fleet lock").contains_key(uid)
        {
            return false;
        }
        let stored = match self.agent_store.get(uid) {
            Ok(Some(stored)) => stored,
            Ok(None) => return false,
            Err(e) => {
                warn!(agent = %uid, error = %e, "cannot read the agent's stored record");
                return false;
            }
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        if fleet.contains_key(uid) {
            return false;
        }
        self.written
            .lock()
            .expect("written lock")
            .insert(*uid, stored.durable_digest());
        fleet.insert(*uid, AgentRecord::from_persisted(stored));
        true
    }

    /// The package offer for one Agent, composed from its **assignments** (ADR-0033), or `None`
    /// when it cannot accept packages, is assigned nothing it fits, or the aggregate hash it last
    /// reported already matches what it is assigned.
    ///
    /// Both the offer and the aggregate are computed over *this Agent's* assignments: comparing
    /// against a fleet-wide aggregate would re-offer, on every exchange, packages this Agent is
    /// never given.
    fn packages_offer(&self, record: &AgentRecord) -> Option<PackagesAvailable> {
        let offering = self.packages.as_ref()?;
        if !accepts_packages(record) {
            return None;
        }
        let effective = record.effective_description();
        let description = effective.as_deref();
        let assigned = record.assigned_package();
        let reported = record
            .package_statuses
            .as_ref()
            .map(|s| s.server_provided_all_packages_hash.as_slice())
            .unwrap_or_default();
        if reported
            == offering
                .store
                .assigned_hash_for(assigned, description)
                .as_slice()
        {
            return None;
        }
        offering.store.offer_for_assigned(assigned, description)
    }

    /// Whether a certificate naming `host` may fetch the artifact `(id, platform)` from the download
    /// route (ADR-0034): it is offered to an Agent the host speaks for. The offer's own test
    /// decides (ADR-0034 clause 35), for each such Agent — the `instance_uid`s bound to the host, or
    /// every Agent for a host marked as a Gateway. Nothing is offered without package delivery or a
    /// host register.
    ///
    /// The package store and the host register are read first and released before the fleet is
    /// locked: no two of those locks are held together here.
    ///
    /// On a shared store the Agents are read from it, whichever instance carries them: the
    /// Agents bound to the host in one read, placed among themselves, which are the group their
    /// Client Agent is found in, or for a Gateway every Agent as the Operator plane reads it. One
    /// that cannot be read is logged, and nothing is offered.
    pub fn offers_artifact(&self, host: &str, id: &PackageId, platform: &Platform) -> bool {
        let (Some(offering), Some(revocations)) = (&self.packages, &self.revocations) else {
            return false;
        };
        // What does not depend on the Agent is resolved once, before the fleet is locked.
        let Some(artifact) = offering.store.offerable(id, platform) else {
            return false;
        };
        let speaks_for = revocations.speaks_for(host);
        let stored = match (self.agent_store.is_shared(), &speaks_for) {
            (false, _) => None,
            (true, SpeaksFor::Any) => Some(self.arrived_over(host).map(|found| self.placed(found))),
            (true, SpeaksFor::Agents(uids)) => {
                let uids: Vec<InstanceUid> =
                    uids.iter().filter_map(|uid| parse_hex_uid(uid)).collect();
                Some(self.agent_store.get_many(&uids).map(|found| {
                    self.placed(
                        found
                            .into_iter()
                            .map(|(uid, record)| (uid, AgentRecord::from_persisted(record)))
                            .collect(),
                    )
                }))
            }
        };
        let stored = match stored.transpose() {
            Ok(stored) => stored,
            Err(e) => {
                warn!(host = %host, error = %e, "cannot read the agents the host speaks for");
                return false;
            }
        };
        let held;
        let fleet = match &stored {
            Some(stored) => stored,
            None => {
                held = self.fleet.lock().expect("fleet lock");
                &*held
            }
        };
        let offered = |record: &AgentRecord| {
            // The cheap half of the test first: most Agents are assigned something else.
            accepts_packages(record)
                && record.assigned_package() == Some(id)
                && artifact.offered_to(
                    record.assigned_package(),
                    record.effective_description().as_deref(),
                )
        };
        match speaks_for {
            SpeaksFor::Any => fleet.values().any(offered),
            SpeaksFor::Agents(uids) => uids
                .iter()
                .filter_map(|uid| parse_hex_uid(uid))
                .filter_map(|uid| fleet.get(&uid))
                .any(offered),
        }
    }

    /// Why this Agent is **proposed** nothing: more than one Deployment claims it, and an Agent
    /// belongs to at most one (ADR-0034 clause 28). `None` when nothing is wrong. Every Agent can
    /// be in conflict, since a Deployment carries Configurations as well as Packages (ADR-0033).
    ///
    /// A conflict takes the *candidate* away and never a standing assignment: an Agent already
    /// rolled out to keeps its offer, because nothing distributes or un-distributes by itself
    /// (ADR-0033). Creating an overlapping channel must not withdraw anything from a running host.
    ///
    /// `claim` is what [`deployment_for`] answered for this Agent — `Ok(None)` when package
    /// delivery, which arms the Deployments, is not configured.
    fn package_conflict(claim: &Result<Option<&Deployment>, String>) -> Option<String> {
        claim.as_ref().err().cloned()
    }

    /// The connection-settings offer for one Agent, or `None` when it cannot accept one or its
    /// reported hash says it already runs (or refused) exactly this offer.
    ///
    /// `issued` is a certificate just signed for this Agent (ADR-0026). It overrides the hash gate
    /// — the Agent asked for it in this very exchange — and rides whatever else the standing offer
    /// carries, so one message can hand over a certificate and the endpoint or heartbeat that go
    /// with it, exactly as the Baseline describes.
    fn settings_offer(
        &self,
        record: &AgentRecord,
        issued: Option<TlsCertificate>,
    ) -> Option<ConnectionSettingsOffers> {
        // The own-telemetry destinations (ADR-0017), offered only for the signals this Agent says
        // it can report — the protocol's negotiation rule, and an offer for a capability the peer
        // lacks is one nobody will ever act on.
        let telemetry = TelemetryOffer {
            own_metrics: self.telemetry_offer.own_metrics.clone().filter(|_| {
                record.capabilities & opamp::proto::AgentCapabilities::ReportsOwnMetrics as u64 != 0
            }),
            own_traces: self.telemetry_offer.own_traces.clone().filter(|_| {
                record.capabilities & opamp::proto::AgentCapabilities::ReportsOwnTraces as u64 != 0
            }),
            own_logs: self.telemetry_offer.own_logs.clone().filter(|_| {
                record.capabilities & opamp::proto::AgentCapabilities::ReportsOwnLogs as u64 != 0
            }),
        };

        if let Some(certificate) = issued {
            let mut settings = self
                .connection_offer
                .as_ref()
                .map(|offer| offer.settings.clone())
                .unwrap_or_default();
            settings.certificate = Some(certificate);
            // Not gated, and hashed over the settings as sent: the standing offer's hash would
            // tell the Agent nothing changed, and it would never adopt the certificate.
            return Some(compose_settings_offer(Some(settings), telemetry));
        }

        // An Agent that accepts no OpAMP settings may still report telemetry, so the two are
        // gated separately: with only a telemetry destination to offer, that is the whole offer.
        let settings = self
            .connection_offer
            .as_ref()
            .filter(|_| {
                record.capabilities
                    & opamp::proto::AgentCapabilities::AcceptsOpAmpConnectionSettings as u64
                    != 0
            })
            .map(|offer| offer.settings.clone());
        if settings.is_none() && telemetry.is_empty() {
            return None;
        }
        gate(record, compose_settings_offer(settings, telemetry))
    }

    /// The unsolicited offer a WebSocket loop pushes when a rollout act changes an assignment;
    /// `None` when the Agent already runs both (or is assigned nothing, or it cannot accept
    /// one), so nothing redundant crosses the wire.
    pub fn offer_for(&self, uid: &InstanceUid) -> Option<ServerToAgent> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let record = fleet.get(uid)?;
        let desired = self.configs.compose(record.assigned_configurations());
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

    /// Package delivery, or the error every package route and rollout act answers when it is not
    /// configured.
    fn offering(&self) -> Result<&PackageOffering, String> {
        self.packages
            .as_ref()
            .ok_or_else(|| "package delivery is not configured on this Server".to_string())
    }

    /// The package store, or the same refusal as [`offering`](Self::offering).
    fn package_store(&self) -> Result<&PackageStore, String> {
        self.offering().map(PackageOffering::store)
    }

    /// Whether any Agent's assignment references this Package — the gate that makes an assigned
    /// Package's bytes immutable (ADR-0033 clause 13). The Agent store answers it (ADR-0023
    /// clause 2); the fleet lock keeps a rollout of this Server from pinning it meanwhile. A
    /// store that other Servers write too refuses the write again from the stored assignments, in
    /// the act that writes.
    fn package_set_assigned(&self, id: &PackageId) -> bool {
        let _fleet = self.fleet.lock().expect("fleet lock");
        self.agent_store.pinning(Pin::Package(id)) > 0
    }

    /// Puts a Package into a channel (ADR-0034 clause 25). Not gated: adding one for an Agent type
    /// the channel does not hold surfaces as waiting on the Agents of that type, and replacing the
    /// one it holds is how a rollout proceeds — an Agent already rolled out to keeps the Package
    /// its assignment pins until the next act. A store that other Servers write too reads the
    /// Package in the act that adds it, so one deleted through another instance is `Missing`.
    pub fn put_deployment_package(
        &self,
        name: &str,
        id: &PackageId,
        replace: bool,
    ) -> Result<Deployment, DeploymentError> {
        self.deployment_store()
            .ok_or(DeploymentError::NotFound)?
            .put_package(name, id, replace)
    }

    /// Takes a Package out of a channel. Not gated: nothing a Deployment holds is frozen
    /// (ADR-0033 clause 11) — an assignment pins the Package itself, so an Agent already released
    /// keeps its offer until the next press.
    pub fn remove_deployment_package(
        &self,
        name: &str,
        id: &PackageId,
    ) -> Result<Deployment, DeploymentError> {
        self.deployment_store()
            .ok_or(DeploymentError::NotFound)?
            .remove_package(name, id)
    }

    /// Deletes a Deployment — **refused while an Agent's assignment names it** (ADR-0033 clause 12).
    ///
    /// An assignment points at the channel it was released through, and the fleet view names it.
    /// So the operator ends that first, by an act that says so: rolling those Agents out through
    /// another Deployment, or forgetting them.
    ///
    /// The fleet lock is held across the check and the delete, so a rollout of this Server cannot
    /// assign through the channel in between; a store that other Servers write too counts the
    /// stored assignments again in the act that deletes.
    pub fn delete_deployment(&self, name: &str) -> Result<bool, DeploymentError> {
        let store = self.deployment_store().ok_or(DeploymentError::NotFound)?;
        let fleet = self.fleet.lock().expect("fleet lock");
        let pinning = self.agent_store.pinning(Pin::Deployment(name));
        if pinning > 0 {
            return Err(DeploymentError::Conflict(deployments::pinned(
                name, pinning,
            )));
        }
        let deleted = store.delete(name)?;
        drop(fleet);
        if deleted {
            info!(deployment = %name, "deployment deleted");
        }
        Ok(deleted)
    }

    /// The refusal every write to an assigned Package answers with (ADR-0033 clause 13).
    fn refuse_if_assigned(&self, id: &PackageId) -> Result<(), String> {
        if self.package_set_assigned(id) {
            return Err(crate::packages::assigned(id));
        }
        Ok(())
    }

    /// Creates a Package (ADR-0034). **Nothing is distributed** (ADR-0033), and there is nothing
    /// to update: a Package is its identity and its entries, so creating one that exists is the
    /// same request arriving twice.
    pub fn create_package_set(&self, id: &PackageId) -> Result<(), String> {
        self.package_store()?.create(id)?;
        info!(package = %id, "package stored — nothing distributed");
        Ok(())
    }

    /// Stores one streamed upload as an entry of a Package (ADR-0034, ADR-0023). Refused while the
    /// Package is assigned to an Agent (ADR-0033 clause 13); no push, because saving never
    /// distributes.
    pub fn put_package_entry(
        &self,
        id: &PackageId,
        platform: &Platform,
        bytes: &mut dyn std::io::Read,
    ) -> Result<(), String> {
        self.refuse_if_assigned(id)?;
        self.package_store()?.put_entry_from(id, platform, bytes)?;
        info!(set = %id, platform = %format!("{}-{}", platform.os, platform.arch), "package entry stored");
        Ok(())
    }

    /// Whether an upload for one entry of `id` may begin: the Package exists and is not assigned
    /// to an Agent (ADR-0033 clause 13), so a refused upload is refused before its bytes are
    /// streamed.
    pub fn check_package_upload(&self, id: &PackageId) -> Result<(), String> {
        self.refuse_if_assigned(id)?;
        self.package_store()?.writable(id)
    }

    /// Writes the operator's signature onto one entry of a Package (ADR-0034), and answers what the
    /// Server found when it checked it. Refused while the Package is assigned to an Agent, like
    /// any other write to its entries (ADR-0033 clause 13): the re-offer gate does not cover the
    /// signature, so one changed under a standing offer would never reach the Agent.
    pub fn put_package_entry_signature(
        &self,
        id: &PackageId,
        platform: &Platform,
        signature: Vec<u8>,
    ) -> Result<SignatureState, String> {
        self.refuse_if_assigned(id)?;
        let state = self
            .package_store()?
            .put_signature(id, platform, signature)?;
        info!(set = %id, platform = %platform.tag(), state = state.as_str(), "package entry signed");
        Ok(state)
    }

    /// Takes one entry's signature away (ADR-0034); `Ok(false)` when it carries none. Refused
    /// while the Package is assigned to an Agent (ADR-0033 clause 13).
    pub fn remove_package_entry_signature(
        &self,
        id: &PackageId,
        platform: &Platform,
    ) -> Result<bool, String> {
        self.refuse_if_assigned(id)?;
        let removed = self.package_store()?.remove_signature(id, platform)?;
        if removed {
            info!(set = %id, platform = %platform.tag(), "package entry signature removed");
        }
        Ok(removed)
    }

    /// Deletes one entry of a Package; `Ok(false)` when the Package or entry does not exist.
    /// Refused while the Package is assigned to an Agent (ADR-0033 clause 13).
    pub fn delete_package_entry(
        &self,
        id: &PackageId,
        platform: &Platform,
    ) -> Result<bool, String> {
        self.refuse_if_assigned(id)?;
        let deleted = self.package_store()?.delete_entry(id, platform)?;
        if deleted {
            info!(set = %id, "package entry deleted");
        }
        Ok(deleted)
    }

    /// Deletes a Package (ADR-0033 clause 12); `Ok(false)` when none of that identity exists.
    /// **Refused while any Agent's assignment pins it** — the message says how many — because a
    /// deletion must not change what a running Agent receives. A deleted Package leaves every
    /// Deployment that held it. Nothing is uninstalled either way (ADR-0034).
    ///
    /// The fleet lock is held across the check and the delete, so a rollout of this Server cannot
    /// pin it in between; a store that other Servers write too counts the stored assignments again
    /// in the act that deletes, and takes it out of every Deployment in that act too.
    pub fn delete_package_set(&self, id: &PackageId) -> Result<bool, String> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let pinning = self.agent_store.pinning(Pin::Package(id));
        if pinning > 0 {
            return Err(crate::packages::pinned(id, pinning));
        }
        let packages = self.package_store()?;
        let sweep = packages.remove_set(id)?;
        let deleted = sweep.is_some();
        let dropped = if deleted {
            self.deployment_store()
                .map(|store| store.drop_package(id).map_err(|e| e.to_string()))
                .transpose()
        } else {
            Ok(None)
        };
        drop(fleet);
        // The sweep may wait for the blob store's lock, which another instance holds as long as it
        // sweeps: not under the fleet lock, which every act of this Server waits for.
        if let Some(sweep) = sweep {
            packages.sweep_after(sweep);
        }
        if let Some(left) = dropped? {
            if !left.is_empty() {
                info!(set = %id, deployments = ?left, "package left the deployments that held it");
            }
        }
        if deleted {
            info!(set = %id, "package set deleted");
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
            // An Agent whose re-key a notification found moved goes by its new identity here
            // before the connection is told it.
            let uid = &if fleet.contains_key(uid) {
                *uid
            } else {
                self.unsure_rekeys
                    .lock()
                    .expect("rekeys lock")
                    .get(uid)
                    .filter(|pending| pending.moved)
                    .map_or(*uid, |pending| pending.new)
            };
            if let Some(record) = fleet.get_mut(uid) {
                if record.owner == Some(conn) {
                    record.connected = false;
                    record.owner = None;
                }
            }
        }
        self.forgotten.lock().expect("forgotten lock").remove(&conn);
    }

    /// The Agents each Host carries (ADR-0030 clause 24) — those whose record names it as the Host
    /// they last arrived over — and the name it is displayed by: the `host.name` its Client Agent
    /// reports, when it has exactly one on a direct connection. Shown only; it decides nothing.
    ///
    /// # Errors
    /// Returns an error when the shared store cannot be read.
    pub fn host_agents(&self) -> Result<BTreeMap<String, HostAgents>, String> {
        self.read_fleet(Self::hosts_of)
    }

    /// The Agents each Host of `fleet` carries, as [`host_agents`](Self::host_agents) answers them.
    fn hosts_of(fleet: &HashMap<InstanceUid, AgentRecord>) -> BTreeMap<String, HostAgents> {
        let mut hosts: BTreeMap<String, HostAgents> = BTreeMap::new();
        for (uid, record) in fleet.iter() {
            let Some(host) = &record.host else { continue };
            let reported = |key: &str| {
                record
                    .description
                    .as_ref()
                    .and_then(|d| {
                        attributes::string_value(&d.identifying_attributes, key).or_else(|| {
                            attributes::string_value(&d.non_identifying_attributes, key)
                        })
                    })
                    .unwrap_or_default()
                    .to_string()
            };
            let entry = hosts.entry(host.clone()).or_default();
            let is_client_agent = record.placement.client_agent == Some(*uid);
            if is_client_agent && record.placement.group == Some(Group::Host(host.clone())) {
                entry.name = Some(reported("host.name")).filter(|name| !name.is_empty());
            }
            entry.agents.push(HostAgentView {
                instance_uid: uid.to_string(),
                service_name: reported(attributes::SERVICE_NAME),
                service_instance_name: reported(attributes::SERVICE_INSTANCE_NAME),
                host_name: reported("host.name"),
                client: crate::labels::is_client_agent(record.description.as_ref()),
                client_agent: record
                    .placement
                    .client_agent
                    .map(|uid| uid.to_string())
                    .unwrap_or_default(),
                machine: match &record.placement.group {
                    Some(Group::Behind { machine, .. }) => machine.clone(),
                    _ => String::new(),
                },
                ambiguous: record.placement.ambiguous,
                connected: record.connected,
            });
        }
        for entry in hosts.values_mut() {
            entry
                .agents
                .sort_by(|a, b| a.instance_uid.cmp(&b.instance_uid));
        }
        hosts
    }

    /// The view of the Agents this Server holds in memory, for tests and diagnostics: on a store
    /// it alone writes the whole fleet, on a shared store only the Agents this instance carries.
    /// The Operator plane never shows it; it reads [`fleet_view`](Self::fleet_view) and
    /// [`agent_view`](Self::agent_view).
    pub fn snapshot(&self) -> Vec<AgentView> {
        self.views(&self.fleet.lock().expect("fleet lock"))
    }

    /// The REST view of the fleet (`GET /api/v1/agents`), read as [`read_fleet`](Self::read_fleet)
    /// reads it: on a shared store from the database, each Agent connected as its stored presence
    /// says (ADR-0025
    /// clause 13).
    ///
    /// # Errors
    /// Returns an error when the shared store cannot be read.
    pub fn fleet_view(&self) -> Result<Vec<AgentView>, String> {
        self.read_fleet(|fleet| self.views(fleet))
    }

    /// The view of one Agent, as [`fleet_view`](Self::fleet_view) shows it, `None` when it is not
    /// known; on a shared store read with the Agents of its Host alone.
    ///
    /// # Errors
    /// Returns an error when the shared store cannot be read.
    pub fn agent_view(&self, uid: &InstanceUid) -> Result<Option<AgentView>, String> {
        let deployments = self.deployment_store().map(DeploymentStore::snapshot);
        if !self.agent_store.is_shared() {
            let fleet = self.fleet.lock().expect("fleet lock");
            return Ok(fleet
                .get(uid)
                .map(|record| self.view_of(deployments.as_ref(), uid, record)));
        }
        Ok(self
            .stored_agent(uid)?
            .map(|record| self.view_of(deployments.as_ref(), uid, &record)))
    }

    /// Whether the Agents have a holder to filter by: on a shared store, where each presence
    /// names one
    /// (ADR-0031
    /// clause 6).
    pub fn keeps_holders(&self) -> bool {
        self.agent_store.is_shared()
    }

    /// One page of the list of Agents
    /// (ADR-0031
    /// clauses 3 to 6 and 8): on a store this Server alone writes, cut from what it holds; on a
    /// shared store read through [`AgentStore::query_page`], each Agent placed under the Client
    /// Agent the store found for it and with the values of the Host it arrived over.
    ///
    /// # Errors
    /// Returns an error when the shared store cannot be read.
    pub fn list_agents(&self, query: &AgentQuery) -> Result<AgentListing, String> {
        let deployments = self.deployment_store().map(DeploymentStore::snapshot);
        let seen_before_ms = self
            .clock
            .now_ms()
            .saturating_sub(self.stale_after().as_millis() as u64);
        let position =
            |uid: &InstanceUid, record: &AgentRecord, derived: &crate::listing::Derived| {
                crate::listing::Position {
                    key: crate::listing::agent_key(
                        query.sort,
                        derived,
                        record.connected,
                        record.sequence_num,
                        record.last_seen_ms,
                    ),
                    tiebreak: uid.to_string(),
                }
            };
        if !self.agent_store.is_shared() {
            let fleet = self.fleet.lock().expect("fleet lock");
            let mut matching = Vec::new();
            for (uid, record) in fleet.iter() {
                let derived = crate::listing::derive(&record.source(uid));
                let effective = record.effective_description();
                let candidate = crate::listing::Candidate {
                    derived: &derived,
                    connected: record.connected,
                    stale: crate::listing::is_stale(
                        record.capabilities,
                        record.last_seen_ms,
                        seen_before_ms,
                    ),
                    holder: None,
                    host: record.host.as_deref(),
                    effective: effective.as_deref(),
                    own: &record.labels,
                    inherited: &record.placement.inherited,
                    host_values: &record.placement.host_values,
                };
                if query.filter.admits(&candidate) {
                    matching.push((position(uid, record, &derived), *uid));
                }
            }
            let page = crate::listing::page(
                matching,
                query.descending,
                query.after.as_ref(),
                query.limit,
            );
            let agents = page
                .entries
                .iter()
                .filter_map(|uid| {
                    fleet
                        .get(uid)
                        .map(|record| self.view_of(deployments.as_ref(), uid, record))
                })
                .collect();
            return Ok(AgentListing {
                agents,
                total: page.total,
                next: page.next,
            });
        }
        let page = self
            .agent_store
            .query_page(&crate::agent_store::PageQuery {
                filter: &query.filter,
                sort: query.sort,
                descending: query.descending,
                after: query.after.as_ref(),
                limit: query.limit.saturating_add(1),
                seen_before_ms,
            })?;
        let mut paged = page.agents;
        let more = paged.len() > query.limit;
        paged.truncate(query.limit);
        let mut next = None;
        let mut agents = Vec::with_capacity(paged.len());
        for found in paged {
            let uid = found.uid;
            if more {
                // As the store sorted it, which is what its next page continues after.
                next = Some(found.position.clone());
            }
            let record = self.placed_as_found(found);
            agents.push(self.view_of(deployments.as_ref(), &uid, &record));
        }
        Ok(AgentListing {
            agents,
            total: page.total,
            next,
        })
    }

    /// The distinct reported `key=value` pairs and `service_name` values of the fleet, each sorted,
    /// at most 1000 of each
    /// (ADR-0031
    /// clause 7): from memory on a store this Server alone writes, through the port on a shared one.
    ///
    /// # Errors
    /// Returns an error when the shared store cannot be read.
    pub fn reported_values(&self) -> Result<crate::agent_store::ReportedValues, String> {
        let limit = crate::listing::MAX_LIMIT;
        if self.agent_store.is_shared() {
            return self.agent_store.reported_values(limit);
        }
        let fleet = self.fleet.lock().expect("fleet lock");
        let derived: Vec<crate::listing::Derived> = fleet
            .iter()
            .map(|(uid, record)| crate::listing::derive(&record.source(uid)))
            .collect();
        Ok(crate::agent_store::ReportedValues::of(&derived, limit))
    }

    /// A record a page read, placed as the store found it: under the Client Agent it named, and
    /// with the values of the Host it arrived over, from this instance's register.
    fn placed_as_found(&self, found: crate::agent_store::PagedAgent) -> AgentRecord {
        let mut record = AgentRecord::from_persisted(found.stored.record);
        record.connected = found.stored.connected;
        record.holder = found.stored.holder;
        let (host_values, group) = self.locate(&record);
        record.placement = Placement {
            host_values,
            group,
            client_agent: found.client_agent,
            inherited: found.inherited,
            ambiguous: found.ambiguous,
        };
        record
    }

    /// The records of the Agents that last arrived over `host`, as stored, through
    /// [`AgentStore::query_page`] filtered on that Host
    /// (ADR-0031
    /// clause 9): the Agents behind a Gateway, whichever instance carries them.
    fn arrived_over(&self, host: &str) -> Result<HashMap<InstanceUid, AgentRecord>, String> {
        let filter = crate::listing::AgentFilter {
            arrived_over: Some(host.to_string()),
            ..crate::listing::AgentFilter::default()
        };
        let page = self
            .agent_store
            .query_page(&crate::agent_store::PageQuery {
                filter: &filter,
                sort: crate::listing::AgentSort::Seq,
                descending: false,
                after: None,
                limit: self.max_agents,
                seen_before_ms: 0,
            })?;
        Ok(self.records_of(
            page.agents
                .into_iter()
                .map(|found| (found.uid, found.stored))
                .collect(),
        ))
    }

    /// The view of each Agent of `fleet`, in the order of its `instance_uid`.
    fn views(&self, fleet: &HashMap<InstanceUid, AgentRecord>) -> Vec<AgentView> {
        // One copy of the Deployments for the whole view, rather than one per Agent.
        let deployments = self.deployment_store().map(DeploymentStore::snapshot);
        let mut agents: Vec<AgentView> = fleet
            .iter()
            .map(|(uid, record)| self.view_of(deployments.as_ref(), uid, record))
            .collect();
        agents.sort_by(|a, b| a.instance_uid.cmp(&b.instance_uid));
        agents
    }

    /// The view of one Agent, `deployments` the Deployments it is matched against.
    fn view_of(
        &self,
        deployments: Option<&BTreeMap<String, Deployment>>,
        uid: &InstanceUid,
        record: &AgentRecord,
    ) -> AgentView {
        // One derived description for everything below, so a label can never mean one
        // thing for the offer and another for what the view proposes.
        let effective = record.effective_description();
        let desired = self.configs.compose(record.assigned_configurations());
        let claim = match deployments {
            Some(all) => deployment_for(all, effective.as_deref()),
            None => Ok(None),
        };
        let package_conflict = Self::package_conflict(&claim);
        let claimed = claim.ok().flatten();
        // Which channel claims it *now* — the other half of the answer, so "no channel" and
        // "a channel with nothing for me" are not the same empty row (ADR-0034 clause 27).
        let claiming_deployment = claimed.map(|d| d.name.clone()).unwrap_or_default();

        // What a press would release to it now (ADR-0033 clause 9). A conflict, or no
        // channel at all, proposes nothing — `package_conflict` says why.
        let proposed = self
            .packages()
            .zip(claimed)
            .map(|(store, deployment)| self.release(store, deployment, record));
        let matched = proposed
            .iter()
            .flatten()
            .flat_map(|release| release.configurations.keys().cloned())
            .collect();
        let waits = match (&proposed, claimed) {
            (Some(release), Some(deployment)) => {
                waiting(record.assignment.as_ref(), release.as_ref(), deployment)
            }
            _ => Waiting::default(),
        };

        AgentView::from_record(
            uid,
            record,
            desired.as_ref(),
            matched,
            package_conflict,
            claiming_deployment,
            waits,
            is_stale(record, self.stale_after(), self.clock.now_ms()),
        )
    }
}

/// The Instance UID a host register names in hex, `None` where it is not one.
fn parse_hex_uid(uid: &str) -> Option<InstanceUid> {
    Some(InstanceUid(
        <[u8; 16]>::try_from(hex::decode(uid).ok()?).ok()?,
    ))
}

/// Takes `uid` out of `group`, and the group out of the index once it holds nobody.
fn unindex(groups: &mut HashMap<Group, HashSet<InstanceUid>>, group: &Group, uid: &InstanceUid) {
    if let Some(members) = groups.get_mut(group) {
        members.remove(uid);
        if members.is_empty() {
            groups.remove(group);
        }
    }
}

/// Places the members of one group under their Client Agent (ADR-0030 clause 19): with exactly
/// one, the others inherit its labels; with more than one, nobody inherits and every member is
/// marked ambiguous; with none, nobody inherits. `elsewhere` are the group's Client Agents that
/// `fleet` does not hold, with their labels.
fn inherit(
    fleet: &mut HashMap<InstanceUid, AgentRecord>,
    groups: &HashMap<Group, HashSet<InstanceUid>>,
    group: &Group,
    elsewhere: &[(InstanceUid, BTreeMap<String, String>)],
) {
    let Some(members) = groups.get(group) else {
        return;
    };
    let mut clients: Vec<(InstanceUid, &BTreeMap<String, String>)> = members
        .iter()
        .filter_map(|uid| {
            let record = fleet.get(uid)?;
            crate::labels::is_client_agent(record.description.as_ref())
                .then_some((*uid, &record.labels))
        })
        .collect();
    clients.extend(elsewhere.iter().map(|(uid, labels)| (*uid, labels)));
    let (client_agent, labels) = match clients.as_slice() {
        [(one, labels)] => (Some(*one), (*labels).clone()),
        _ => (None, BTreeMap::new()),
    };
    let ambiguous = clients.len() > 1;
    for uid in members {
        if let Some(record) = fleet.get_mut(uid) {
            record.placement.client_agent = client_agent;
            record.placement.inherited = if client_agent == Some(*uid) {
                BTreeMap::new()
            } else {
                labels.clone()
            };
            record.placement.ambiguous = ambiguous;
        }
    }
}

/// What waits for one Agent's next press — every part in which what the claiming Deployment
/// would release differs from what the assignment pins.
#[derive(Default)]
struct Waiting {
    configurations: Vec<PendingConfigurationView>,
    packages: Vec<PendingPackageView>,
    /// The Deployment the press would release through, when the assignment names another one or
    /// none.
    deployment: String,
}

/// What waits for an Agent's next press (ADR-0033 clauses 6, 8 and 9): the
/// difference between what the claiming Deployment would release and what the assignment pins,
/// per Configuration, for the Package, and for the Deployment the release goes through — so
/// whatever makes a press move this Agent shows here, and the reach count agrees with the view.
///
/// `new` is not pinned yet, `update` is pinned at another revision or version, and `remove` is
/// pinned and leaves the release — the Deployment no longer holds it, or no longer fits it to this
/// Agent — while the release keeps something else, so the smaller assignment is applied. `kept` is
/// pinned while the release holds nothing for the Agent at all: the press leaves it with no
/// assignment, it is offered nothing, and it keeps running what it runs (ADR-0033 clause 9).
fn waiting(
    assigned: Option<&Assignment>,
    release: Option<&Assignment>,
    deployment: &Deployment,
) -> Waiting {
    let dropped = if release.is_some() { "remove" } else { "kept" };
    let pinned = assigned.map_or(&NO_CONFIGURATIONS, |a| &a.configurations);
    let released = release.map_or(&NO_CONFIGURATIONS, |a| &a.configurations);
    let mut configurations: Vec<PendingConfigurationView> = released
        .iter()
        .filter_map(|(name, hash)| {
            let change = match pinned.get(name) {
                Some(pinned) if pinned == hash => return None,
                Some(_) => "update",
                None => "new",
            };
            Some(PendingConfigurationView {
                name: name.clone(),
                change: change.to_string(),
            })
        })
        .chain(
            pinned
                .keys()
                .filter(|name| !released.contains_key(*name))
                .map(|name| PendingConfigurationView {
                    name: name.clone(),
                    change: dropped.to_string(),
                }),
        )
        .collect();
    configurations.sort_by(|a, b| a.name.cmp(&b.name));
    let pinned_package = assigned.and_then(|a| a.package.as_ref());
    let released_package = release.and_then(|a| a.package.as_ref());
    let package = match (pinned_package, released_package) {
        (Some(pinned), Some(released)) if pinned == released => None,
        (Some(_), Some(released)) => Some((released, "update")),
        (None, Some(released)) => Some((released, "new")),
        (Some(pinned), None) => Some((pinned, dropped)),
        (None, None) => None,
    };
    let packages = package
        .map(|(id, change)| PendingPackageView {
            deployment: deployment.name.clone(),
            agent_type: id.agent_type.clone(),
            version: id.version.clone(),
            display_name: id.display_name(),
            change: change.to_string(),
        })
        .into_iter()
        .collect();
    let through = release
        .map(|r| r.deployment.as_str())
        .filter(|through| assigned.map(|a| a.deployment.as_str()) != Some(*through))
        .unwrap_or_default();
    Waiting {
        configurations,
        packages,
        deployment: through.to_string(),
    }
}

/// Whether this Agent declared that it accepts packages — the first condition of every offer
/// (ADR-0034 clause 35).
fn accepts_packages(record: &AgentRecord) -> bool {
    record.capabilities & opamp::proto::AgentCapabilities::AcceptsPackages as u64 != 0
}

/// The remote-config offer for one Agent, or `None` when the hash comparison says it already has
/// it — the "no redundant reconfiguration" goal in one place. Every assigned Configuration is one
/// named entry; the Managed Process does its own merging (ADR-0032).
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
                            // The operator's role, verbatim (ADR-0032). Empty — the default —
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
/// from (ADR-0038).
///
/// A value that is not a version is returned as it stands. `service.version` is whatever an Agent
/// puts there, and a Foreign Agent numbers itself however its own project does — trimming a string
/// this Server does not understand would be inventing a version rather than showing one.
pub(crate) fn display_version(reported: &str) -> String {
    fleet_core::version::identity(reported)
        .unwrap_or(reported)
        .to_string()
}

/// What the list of Agents is asked
/// (ADR-0031
/// clauses 3 to 6).
#[derive(Clone, Debug, Default)]
pub struct AgentQuery {
    pub filter: crate::listing::AgentFilter,
    pub sort: crate::listing::AgentSort,
    pub descending: bool,
    /// The position the page starts after; `None` for the first page.
    pub after: Option<crate::listing::Position>,
    pub limit: usize,
}

/// One page of the list of Agents.
pub struct AgentListing {
    pub agents: Vec<AgentView>,
    /// How many Agents match the filters.
    pub total: usize,
    /// Where the next page starts, when another follows.
    pub next: Option<crate::listing::Position>,
}

/// The Agents one Host carries, and the name it is shown by (ADR-0030 clause 24).
#[derive(Default)]
pub struct HostAgents {
    /// The `host.name` its Client Agent reports; `None` for a Gateway, and while it has no single
    /// Client Agent.
    pub name: Option<String>,
    pub agents: Vec<HostAgentView>,
}

/// One Agent under the Host it arrived over (ADR-0030 clause 24): enough to group it by its
/// Client Agent.
pub struct HostAgentView {
    pub instance_uid: String,
    pub service_name: String,
    pub service_instance_name: String,
    /// The `host.name` it reports, or empty.
    pub host_name: String,
    /// Whether it is a Client Agent: it reports `supervisor.kind.*`.
    pub client: bool,
    /// The Instance UID of its Client Agent — its own, for the Client Agent — or empty.
    pub client_agent: String,
    /// Behind a Gateway, the machine it is grouped by (`host.id=…` or `host.name=…`); empty on
    /// a direct connection.
    pub machine: String,
    /// Its group holds more than one Client Agent.
    pub ambiguous: bool,
    pub connected: bool,
}

/// One Agent as the REST API and the UI see it.
#[derive(Serialize)]
pub struct AgentView {
    pub instance_uid: String,
    /// The Agent *type* — the Baseline's "reverse FQDN that uniquely identifies the Agent type"
    /// (ADR-0017). For a Supervisor-backed Agent this is the type its Supervisor states, never one
    /// its Managed Process reports, so every Agent of one configured type reports the same value.
    /// It answers "what is this", never "which one is this": that is
    /// [`service_instance_name`](Self::service_instance_name).
    pub service_name: String,
    /// The operator's name for this Agent — the `[[supervisor]]` block's `name` (ADR-0017). Empty
    /// for a foreign OpAMP client that reports no `service.instance.name`, which is why the UI
    /// falls back through the type to the UID rather than showing a blank row.
    pub service_instance_name: String,
    /// The release the Agent reports — `MAJOR.MINOR.PATCH`, with the pre-release when it is not a
    /// release build (ADR-0038). This is what belongs in a column headed "Version"; the commit the
    /// build came from is [`service_build`](Self::service_build). A reported value that is not a
    /// version at all is passed through unchanged, since a Foreign Agent numbers itself however it
    /// likes.
    pub service_version: String,
    /// Exactly what the Agent reported, commit metadata and all — the answer to "which build is on
    /// that host", which is a question a fleet exists to answer (ADR-0038).
    pub service_build: String,
    /// The reported `os.description` (e.g. "Ubuntu 24.04.2 LTS"), falling back to `os.type`.
    pub os: String,
    /// Every reported identifying attribute — what a Selector can match on (ADR-0032).
    pub identifying_attributes: BTreeMap<String, String>,
    /// Every reported non-identifying attribute — Selectors match these too.
    pub non_identifying_attributes: BTreeMap<String, String>,
    /// The Configurations a press of the Deployment claiming this Agent would release to it — the
    /// ones it holds that fit this Agent's type (ADR-0033), in name order. Never what it runs;
    /// that is [`assigned_configurations`](Self::assigned_configurations).
    pub matched_configurations: Vec<String>,
    /// The Configurations rolled out to this Agent (ADR-0033 clause 6), in name order — what its
    /// offer is composed from.
    pub assigned_configurations: Vec<String>,
    /// The Deployment that claims this Agent **now** — whose Selector matches it — or empty when
    /// none does.
    ///
    /// This is not [`assigned_deployment`](Self::assigned_deployment), and the difference is what
    /// tells four states apart that would otherwise look alike (ADR-0034 clause 27). Empty here
    /// with no conflict means the host is in **no channel**: label it, or give it a `channel`
    /// attribute. Set here with nothing assigned and nothing pending means the channel holds
    /// nothing this Agent can take — no Package for its type, or none for its platform. The
    /// operator's next move differs in each case, which is why the Server says which one it is
    /// rather than showing an empty row three ways.
    pub deployment: String,
    /// The Deployment this Agent's release went **through** (ADR-0033 clause 6), or empty when
    /// nothing has been rolled out to it. Pinned as of that act, so it may name a channel that no
    /// longer claims this Agent.
    pub assigned_deployment: String,
    /// The Package rolled out to this Agent (ADR-0033), as `<agent type>@<version>`, or empty.
    ///
    /// It is pinned as of the act that released it: re-aiming its Deployment afterwards, or
    /// putting a newer Package in that channel, changes what is *proposed* and never what this Agent
    /// was already given.
    pub assigned_package: String,
    /// The Configurations waiting for a rollout act toward this Agent (ADR-0033 clause 8):
    /// one its Deployment holds and it is not assigned (`change: "new"`), one whose
    /// saved revision differs from the assigned one (`change: "update"`), one assigned that the
    /// next press drops from a release that keeps something else (`change: "remove"`), or one
    /// assigned while the release holds nothing for the Agent, so the press leaves it with no
    /// assignment and it keeps running what it runs (`change: "kept"`). The Server never acts on
    /// this by itself.
    pub pending_configurations: Vec<PendingConfigurationView>,
    /// The Package waiting for a rollout act toward this Agent (ADR-0033 clause 8) — at most one:
    /// the candidate of the Deployment that claims it, when that is not what it is assigned, or
    /// the assigned one the next press drops (`change: "remove"`, or `"kept"` as for a
    /// Configuration).
    pub pending_packages: Vec<PendingPackageView>,
    /// The Deployment the next press releases through, when the assignment names another one or
    /// none (ADR-0033 clause 9); empty otherwise. A release whose content is unchanged still moves
    /// the assignment to the claiming Deployment.
    pub pending_deployment: String,
    /// Hex hash of the composed configuration this Agent should run; empty when it is assigned
    /// nothing.
    pub desired_hash: String,
    /// The Capability Set this Agent declared, as capability names from the Baseline's
    /// `AgentCapabilities` (see docs/CONFORMANCE.md).
    pub capabilities: Vec<String>,
    /// The Agent's available components (top-level names, sorted); empty until reported.
    pub available_components: Vec<String>,
    /// The Agent's package installations (ADR-0034), in name order; empty until reported.
    pub packages: Vec<PackageStatusView>,
    /// Why this Agent is proposed nothing — more than one Deployment claims it, and an Agent
    /// belongs to at most one (ADR-0034 clause 28). The message names every Deployment in the way.
    /// Absent when at most one claims it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_conflict: Option<String>,
    /// What the Agent said about the *offer* rather than about a package it holds — an offer it
    /// refuses outright has no package status to carry the reason, and the Client's own Agent
    /// refusing a package it was not configured to take (ADR-0019) is exactly that case. Empty
    /// when the Agent has nothing to complain about.
    pub package_error: String,
    pub transport: String,
    pub connected: bool,
    pub healthy: bool,
    pub health_status: String,
    /// Why the Agent is unhealthy — `ComponentHealth.last_error`, which the Baseline says SHOULD
    /// be set when `healthy` is false. Empty when the Agent is healthy or gave no reason.
    pub health_error: String,
    pub effective_config: String,
    pub remote_config_status: String,
    pub remote_config_error: String,
    pub in_sync: bool,
    pub sequence_num: u64,
    pub last_seen_ms: u64,
    /// Nothing has been heard from this Agent for longer than its staleness budget (ADR-0030).
    ///
    /// Beside [`connected`](Self::connected), never instead of it: that one says a connection
    /// carrying this Agent is open — behind a Gateway, the *Gateway's* — and this one says whether
    /// the Agent itself is still talking. `connected: true, stale: true` is precisely the gatewayed
    /// case, and precisely what an operator needs to be told.
    ///
    /// Only an Agent declaring `ReportsHeartbeat` can be stale: that capability is the promise that
    /// makes silence mean something. Derived on read, never stored.
    pub stale: bool,
    /// The operator's labels on this Agent (ADR-0030) — matched by Selectors exactly like a
    /// reported attribute, but set here rather than in `supervisor.toml` on the host, so moving a host
    /// between rollout channels is an API call instead of an edit and a restart.
    pub labels: BTreeMap<String, String>,
    /// Labels this Agent's own reports shadow: set, matching nothing, and therefore doing nothing.
    ///
    /// Reported attributes always win (ADR-0030) — they decide which artifact fits this machine.
    /// A collision is refused when the label is set, so this fills only when an Agent *starts*
    /// reporting a key that was labelled earlier. Shown rather than dropped in silence.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shadowed_labels: Vec<String>,
    /// The labels of this Agent's Client Agent, which hold for it below its own (ADR-0030
    /// clause 19); empty for the Client Agent itself, for an Agent whose Client the Server cannot
    /// tell, and for an ambiguous one.
    pub inherited_labels: BTreeMap<String, String>,
    /// The Instance UID of this Agent's Client Agent — its own, for the Client Agent — or empty
    /// when the Server cannot tell.
    pub client_agent: String,
    /// The group this Agent's Client is recognised in holds more than one Client Agent, so it
    /// inherits nothing (ADR-0030 clause 19).
    pub ambiguous: bool,
    /// The Host this Agent last arrived over (ADR-0030 clause 16), or empty. Behind a Gateway, the
    /// Gateway's.
    pub host: String,
    /// The instance id of the instance that holds it under a live lease
    /// (ADR-0025
    /// clause 13); empty without one, and always on the filesystem store.
    pub holder: String,
    /// The host name that instance's lease records; empty when `holder` is.
    pub holder_host: String,
    /// The `fleet.host.*` values its effective description takes from that Host (ADR-0030
    /// clause 18); empty behind a Gateway.
    pub host_attributes: BTreeMap<String, String>,
    /// Reported attributes in the Server's `fleet.` space, left out of what Selectors match
    /// (ADR-0030 clause 20).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignored_attributes: Vec<String>,
}

/// One Configuration waiting for a rollout act toward one Agent (ADR-0033).
#[derive(Serialize)]
pub struct PendingConfigurationView {
    pub name: String,
    /// `new` — held by the claiming Deployment and not yet assigned; `update` — the saved
    /// revision differs from the assigned one; `remove` — assigned, and dropped by the next press,
    /// which applies the smaller map; `kept` — assigned, and the next press leaves the Agent with no
    /// assignment, so it keeps running it.
    pub change: String,
}

/// Whom one Deployment reaches in the fleet as reported so far (ADR-0034): the Agents it claims,
/// the subset a rollout act would actually change, and those another Deployment also claims.
#[derive(Clone, Copy, Default, Debug)]
pub struct DeploymentReach {
    /// Agents this Deployment claims — and no other does. Zero is the aim mistake worth hunting.
    pub claiming: usize,
    /// Of those, the Agents a press would actually move — what it would release differs from what
    /// their assignment pins (ADR-0033 clause 9). Zero with a non-zero `claiming` means everyone is
    /// up to date.
    pub targeted: usize,
    /// Agents this Deployment matches that **another one matches too**. They are offered nothing
    /// new until an operator narrows a Selector (ADR-0034 clause 28).
    pub conflicting: usize,
}

/// The Package waiting for a rollout act toward one Agent (ADR-0033).
#[derive(Serialize)]
pub struct PendingPackageView {
    /// The Deployment that would release it — the channel this Agent belongs to.
    pub deployment: String,
    /// The Agent type this Package is built for — its identity, and its wire name.
    pub agent_type: String,
    pub version: String,
    /// What an operator reads: the Agent type and the version together.
    pub display_name: String,
    /// `new` — nothing is assigned for this Agent type; `update` — another version is; `remove` —
    /// the assigned one, which the next press drops; `kept` — the assigned one, while the next
    /// press leaves the Agent with no assignment.
    pub change: String,
}

/// One package's installation state as the REST API and UI see it (ADR-0034).
#[derive(Serialize)]
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
    pub(crate) fn from_status(status: &opamp::proto::PackageStatus) -> Self {
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
pub(crate) fn capability_names(mask: u64) -> Vec<String> {
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

/// Reported attributes as the API shows them: string values as-is, string arrays (the shape the
/// conventions give `host.ip` and `host.mac`, ADR-0017) joined with a comma, other value kinds in
/// their debug form — the view is for reading, the wire keeps the typed original.
pub(crate) fn attr_map(attributes: &[KeyValue]) -> BTreeMap<String, String> {
    fn text(value: &any_value::Value) -> String {
        match value {
            any_value::Value::StringValue(s) => s.clone(),
            any_value::Value::ArrayValue(list) => list
                .values
                .iter()
                .filter_map(|v| v.value.as_ref())
                .map(text)
                .collect::<Vec<_>>()
                .join(", "),
            other => format!("{other:?}"),
        }
    }
    attributes
        .iter()
        .filter_map(|kv| {
            let value = kv.value.as_ref()?.value.as_ref()?;
            Some((kv.key.clone(), text(value)))
        })
        .collect()
}

impl AgentView {
    #[allow(clippy::too_many_arguments)]
    fn from_record(
        uid: &InstanceUid,
        record: &AgentRecord,
        desired: Option<&DesiredConfig>,
        matched_configurations: Vec<String>,
        package_conflict: Option<String>,
        claiming_deployment: String,
        waits: Waiting,
        stale: bool,
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
        // What the Agent said, and what a reader of a table wants out of it (ADR-0038).
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
            assigned_configurations: record.assigned_configurations().keys().cloned().collect(),
            deployment: claiming_deployment,
            assigned_deployment: record
                .assignment
                .as_ref()
                .map(|a| a.deployment.clone())
                .unwrap_or_default(),
            assigned_package: record
                .assigned_package()
                .map(PackageId::to_string)
                .unwrap_or_default(),
            pending_configurations: waits.configurations,
            pending_packages: waits.packages,
            pending_deployment: waits.deployment,
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
            health_error: record
                .health
                .as_ref()
                .map(|h| h.last_error.clone())
                .unwrap_or_default(),
            effective_config: record.effective_config.clone().unwrap_or_default(),
            remote_config_status: status_name.to_string(),
            remote_config_error: status.map(|s| s.error_message.clone()).unwrap_or_default(),
            in_sync,
            sequence_num: record.sequence_num,
            last_seen_ms: record.last_seen_ms,
            stale,
            shadowed_labels: crate::labels::shadowed(record.description.as_ref(), &record.labels),
            labels: record.labels.clone(),
            inherited_labels: record.placement.inherited.clone(),
            client_agent: record
                .placement
                .client_agent
                .map(|uid| uid.to_string())
                .unwrap_or_default(),
            ambiguous: record.placement.ambiguous,
            host: record.host.clone().unwrap_or_default(),
            holder: record
                .holder
                .as_ref()
                .map(|holder| holder.instance_id.clone())
                .unwrap_or_default(),
            holder_host: record
                .holder
                .as_ref()
                .map(|holder| holder.host.clone())
                .unwrap_or_default(),
            host_attributes: record.placement.host_values.clone(),
            ignored_attributes: crate::labels::ignored(record.description.as_ref()),
        }
    }
}

/// Whether nothing has been heard from this Agent for longer than its budget (ADR-0030).
///
/// Gated on `ReportsHeartbeat`: an Agent that never promised to report periodically is not late,
/// however long it has been quiet, and flagging it would train an operator to ignore the flag.
fn is_stale(record: &AgentRecord, stale_after: Duration, now_ms: u64) -> bool {
    if record.capabilities & opamp::proto::AgentCapabilities::ReportsHeartbeat as u64 == 0 {
        return false;
    }
    is_silent(record, stale_after, now_ms)
}

/// Nothing has been heard from this Agent for longer than `budget` — the plain fact, without the
/// promise [`is_stale`] adds on top of it.
///
/// The two are deliberately not the same test. Calling an Agent *stale* accuses it of being late,
/// which is only fair when it declared `ReportsHeartbeat` and so promised to be punctual. Asking
/// whether it is safe to forget (ADR-0030) is a question about evidence, not about promises: an
/// Agent nobody has heard from cannot be disturbed by being forgotten, whatever it once declared.
fn is_silent(record: &AgentRecord, budget: Duration, now_ms: u64) -> bool {
    now_ms.saturating_sub(record.last_seen_ms) > budget.as_millis() as u64
}

/// When a kept put-back of a restart is tried again, counted in calls of
/// [`AppState::retry_restarts`].
#[derive(Debug, Clone, Copy)]
struct Backoff {
    /// Calls until the next try.
    left: u32,
    /// Calls the next failure waits.
    wait: u32,
}

impl Backoff {
    /// The longest wait between two tries: a minute, at one call a second.
    const MAX_WAIT: u32 = 60;

    /// After the first failure: tried again at the next call.
    fn first() -> Self {
        Backoff { left: 1, wait: 2 }
    }

    /// Counts one call down, and answers whether a try is due.
    fn tick(&mut self) -> bool {
        self.left = self.left.saturating_sub(1);
        self.left == 0
    }

    /// The try failed: the next waits twice as long, up to the bound.
    fn failed(&mut self) {
        self.left = self.wait;
        self.wait = (self.wait * 2).min(Self::MAX_WAIT);
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

/// One `ConnectionSettingsOffers` message, with one hash over everything it carries: the Agent
/// acknowledges the message, not its parts.
fn compose_settings_offer(
    opamp: Option<OpAmpConnectionSettings>,
    telemetry: TelemetryOffer,
) -> ConnectionSettingsOffers {
    let mut offer = ConnectionSettingsOffers {
        opamp,
        own_metrics: telemetry.own_metrics,
        own_traces: telemetry.own_traces,
        own_logs: telemetry.own_logs,
        ..Default::default()
    };
    offer.hash = Sha256::digest(offer.encode_to_vec()).to_vec();
    offer
}

/// The Baseline's gate: send the offer when the Agent's reported hash differs. An APPLYING echo of
/// the same hash keeps it coming — a verification whose outcome was lost (a dropped connection
/// mid-switch) must heal by retry, not hang.
fn gate(record: &AgentRecord, offer: ConnectionSettingsOffers) -> Option<ConnectionSettingsOffers> {
    if let Some(status) = &record.connection_settings_status {
        if status.last_connection_settings_hash == offer.hash
            && status.status != opamp::proto::ConnectionSettingsStatuses::Applying as i32
        {
            return None;
        }
    }
    Some(offer)
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

/// What a CSR with a bootstrap proof over a member's certificate is answered (ADR-0016 clause 25).
pub const MEMBER_DOES_NOT_ENROL: &str =
    "a certificate signing request with a bootstrap proof is an enrolment, and a member does not \
     enrol";

/// What a CSR with neither proof over a Gateway's certificate is answered (ADR-0016 clause 18).
pub const NEEDS_A_PROOF: &str =
    "a certificate requested through a Gateway needs a renewal proof or a bootstrap proof";

/// How long an Agent told `Unavailable` waits before it asks again.
pub const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// The `ServerToAgent` for a report the Server is momentarily unable to accept — the Baseline's
/// `Unavailable`, which unlike `BadRequest` tells the Agent to **retry later** rather than give up,
/// and with `retry_info` says when. It carries the `instance_uid` of the message it answers, the
/// field a Client and a Gateway route a reply by (ADR-0012 clause 27).
pub fn unavailable(instance_uid: &[u8], message: &str) -> ServerToAgent {
    ServerToAgent {
        instance_uid: instance_uid.to_vec(),
        capabilities: SERVER_CAPABILITIES,
        error_response: Some(ServerErrorResponse {
            r#type: ServerErrorResponseType::Unavailable as i32,
            error_message: message.to_string(),
            details: Some(opamp::proto::server_error_response::Details::RetryInfo(
                opamp::proto::RetryInfo {
                    retry_after_nanoseconds: RETRY_AFTER.as_nanos() as u64,
                },
            )),
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

/// Refuses a per-Agent press that would move nothing because the Package the Deployment holds for
/// the Agent's type is not released to it — no upgrade, no entry for its platform — and says why,
/// naming the version it compared against (ADR-0033 clause 19). A press that moves nothing for any
/// other reason is not refused: it is the same request arriving twice.
fn refuse_a_package_left_out(
    store: &crate::packages::PackageStore,
    deployment: &Deployment,
    record: &AgentRecord,
    release: Option<&Assignment>,
) -> Result<(), RolloutError> {
    let effective = record.effective_description();
    let description = effective.as_deref();
    let Some(held) = crate::packages::reported_agent_type(description)
        .and_then(|agent_type| deployment.package_for(agent_type))
    else {
        return Ok(());
    };
    if release.and_then(|r| r.package.as_ref()) == Some(held) {
        return Ok(());
    }
    store
        .fits_agent(held, description, &record.installed_package_versions())
        .map_err(RolloutError::NotApplicable)
}

/// Refuses a rollout of a Deployment that holds nothing, or a Package with an unsigned entry —
/// naming each such Package and its platforms; nothing is released (ADR-0033 clause 9, ADR-0034).
fn refuse_unreleasable(
    store: &crate::packages::PackageStore,
    deployment: &Deployment,
) -> Result<(), RolloutError> {
    if deployment.holds_nothing() {
        return Err(RolloutError::NotApplicable(format!(
            "deployment {:?} holds no package and no configuration — put one in it before rolling \
             it out",
            deployment.name
        )));
    }
    let unsigned = store.unsigned_in(deployment);
    if unsigned.is_empty() {
        return Ok(());
    }
    Err(RolloutError::NotApplicable(format!(
        "deployment {:?} holds packages with unsigned entries: {} — sign every entry on the \
         Packages tab before rolling it out",
        deployment.name,
        unsigned.join("; ")
    )))
}

/// What an approval keeps of what it signed (ADR-0023 clause 9): the signature in the audit
/// record before its decision is written, and its register entry and Host, prepared here, written
/// with the decision and taken up once it is.
struct Approving<'a>(&'a AppState);

/// An approval's refusal by the register.
fn register_refusal(e: String) -> DecisionError {
    if e == crate::revocation::BOOTSTRAP_REVOKED {
        DecisionError::Revoked
    } else {
        DecisionError::Sign(format!("cannot record the issued certificate: {e}"))
    }
}

impl Registrar for Approving<'_> {
    fn register(&self, approved: &crate::revocation::Approved) -> Result<(), DecisionError> {
        match &self.0.revocations {
            Some(revocations) => revocations
                .register_approved(approved)
                .map_err(register_refusal),
            None => Ok(()),
        }
    }

    fn committed(&self, approved: &crate::revocation::Approved, written: Option<u64>) {
        if let Some(revocations) = &self.0.revocations {
            revocations.took_up_approved(approved, written);
        }
    }
}

impl Recorder for Approving<'_> {
    fn signed(
        &self,
        signed: &Signed,
        request: &str,
        instance_uid: &[u8],
        bootstrap: &CertId,
    ) -> Result<Option<crate::revocation::Approved>, DecisionError> {
        let approved = match &self.0.revocations {
            Some(revocations) => Some(
                revocations
                    .prepare_approved(signed.facts.clone(), instance_uid, bootstrap)
                    .map_err(register_refusal)?,
            ),
            None => None,
        };
        self.0
            .audited(
                self.0
                    .signature_entry(signed, instance_uid, None, Some(request)),
            )
            .map_err(DecisionError::Sign)?;
        Ok(approved)
    }

    fn undecided(&self, signed: &Signed, request: &str, outcome: crate::enrolment::Undecided) {
        let entry = Entry::new("issuance.undecided", outcome.outcome())
            .with("issuer", signed.facts.issuer_name.clone())
            .with("serial", signed.facts.id.serial.clone())
            .with("key_fingerprint", signed.facts.key_fingerprint.clone())
            .with("request", request.to_string());
        if let Err(e) = self.0.audited(entry) {
            tracing::warn!(error = %e, request = %request, serial = %signed.facts.id.serial, "cannot record a signature whose decision was not written");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    /// The host derived for a certificate that names none is the one this Server derived before
    /// the derivation moved into `fleet-core`: hosts already in the register keep their ids.
    /// Verifies: ADR-0016
    #[test]
    fn a_derived_host_is_the_one_the_register_already_holds() {
        use sha2::Digest as _;
        let (issuer, serial) = ("5f0a", "1c");
        let digest = sha2::Sha256::digest(format!("{issuer}\n{serial}"));
        let before = opamp::uid::InstanceUid::from_wire(&digest[..16])
            .expect("16 bytes")
            .to_string();
        assert_eq!(fleet_core::host::derived(issuer, serial), before);
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// The view of an array-valued attribute — the shape `host.ip` and `host.mac` arrive in
    /// (ADR-0017): joined for reading, not dumped in debug form.
    #[test]
    fn the_view_joins_a_string_array_attribute() {
        let attrs = vec![
            opamp::attributes::string_attr("host.name", "edge-01"),
            opamp::attributes::string_array_attr(
                "host.ip",
                &["10.0.0.7".into(), "192.168.1.140".into()],
            ),
        ];
        let map = super::attr_map(&attrs);
        assert_eq!(map["host.name"], "edge-01");
        assert_eq!(map["host.ip"], "10.0.0.7, 192.168.1.140");
    }

    /// The gatewayed case, which is why this exists: the connection is up — it is the Gateway's —
    /// and the Agent behind it has stopped talking. Both facts are reported, neither overwrites
    /// the other (ADR-0030).
    // Verifies: ADR-0030
    #[test]
    fn an_agent_that_stopped_reporting_is_stale_while_its_connection_is_up() {
        let mut record = record_with(opamp::proto::AgentCapabilities::ReportsHeartbeat as u64);
        record.connected = true;
        record.last_seen_ms = now_ms() - 120_000;
        assert!(is_stale(&record, Duration::from_secs(90), now_ms()));
        assert!(record.connected, "connectedness is a separate fact");
    }

    /// One missed beat is a lost packet. The budget is three intervals, so a report inside it is
    /// not late.
    #[test]
    fn an_agent_inside_its_budget_is_not_stale() {
        let mut record = record_with(opamp::proto::AgentCapabilities::ReportsHeartbeat as u64);
        record.last_seen_ms = now_ms() - 40_000;
        assert!(!is_stale(&record, Duration::from_secs(90), now_ms()));
    }

    /// An Agent that never promised to report periodically is not late, however long it is quiet —
    /// flagging it would train an operator to ignore the flag.
    #[test]
    fn an_agent_that_promised_no_heartbeat_never_goes_stale() {
        let mut record = record_with(opamp::proto::AgentCapabilities::ReportsStatus as u64);
        record.last_seen_ms = now_ms() - 86_400_000;
        assert!(!is_stale(&record, Duration::from_secs(90), now_ms()));
    }

    /// The offered interval wins over the configured default: it is the period this Server actually
    /// asked for, so it is the one silence should be measured against.
    /// Verifies: ADR-0013
    #[test]
    fn an_offered_heartbeat_interval_sets_the_budget() {
        let dir = tempfile::tempdir().expect("tempdir");
        let offer = ConnectionOffer::from_config(
            &toml::from_str::<crate::config::ConnectionOfferConfig>(
                "heartbeat_interval_secs = 10\n",
            )
            .expect("offer config"),
        );
        let state = AppState::new(dir.path().join("configs"))
            .expect("state")
            .with_connection_offer(Some(offer))
            .with_stale_after(Duration::from_secs(90));
        assert_eq!(
            state.stale_after(),
            Duration::from_secs(30),
            "three intervals"
        );
    }

    /// ADR-0030. The tidy-up case: a host that was decommissioned, its Agent gone with it.
    #[test]
    fn a_disconnected_agent_is_forgotten() {
        let state = forgettable_state();
        let uid = insert(&state, record_with(0));
        assert!(state.forget_agent(&uid).is_ok());
        assert!(state.snapshot().is_empty(), "the row is gone");
    }

    /// The gate: forgetting a live Agent would drop the hashes that stop the Server re-offering,
    /// so its next exchange re-applies its configuration — and a managed process restarts with it.
    #[test]
    fn an_agent_that_is_still_reporting_is_refused() {
        let state = forgettable_state();
        let mut record = record_with(0);
        record.connected = true;
        record.last_seen_ms = now_ms();
        let uid = insert(&state, record);
        assert!(matches!(
            state.forget_agent(&uid),
            Err(ForgetError::StillReporting)
        ));
        assert_eq!(state.snapshot().len(), 1, "the row stays");
    }

    /// The gatewayed case: the connection is up because it is the *Gateway's*, and the Agent behind
    /// it stopped talking long ago. `connected` alone would refuse this forever.
    #[test]
    fn a_connected_agent_that_went_quiet_is_forgotten() {
        let state = forgettable_state();
        let mut record = record_with(opamp::proto::AgentCapabilities::ReportsHeartbeat as u64);
        record.connected = true;
        record.last_seen_ms = now_ms() - 120_000;
        let uid = insert(&state, record);
        assert!(state.forget_agent(&uid).is_ok());
    }

    /// The case that made the rule test silence rather than staleness (ADR-0030): an Agent that
    /// promised no heartbeat is never *stale*, and plain-HTTP polling never clears `connected` —
    /// so gating on the flag would have left this row on a dead host permanently unremovable.
    #[test]
    fn a_silent_agent_is_forgotten_although_it_can_never_be_stale() {
        let state = forgettable_state();
        let mut record = record_with(opamp::proto::AgentCapabilities::ReportsStatus as u64);
        record.connected = true;
        record.transport = Transport::Http;
        record.last_seen_ms = now_ms() - 86_400_000;
        let uid = insert(&state, record);
        assert!(
            !is_stale(
                &record_at(now_ms() - 86_400_000),
                Duration::from_secs(90),
                now_ms()
            ),
            "it declares no heartbeat, so it is never stale"
        );
        assert!(state.forget_agent(&uid).is_ok(), "but it is forgettable");
    }

    #[test]
    fn forgetting_an_agent_that_was_never_known_says_so() {
        let state = forgettable_state();
        assert!(matches!(
            state.forget_agent(&InstanceUid::default()),
            Err(ForgetError::UnknownAgent)
        ));
    }

    fn forgettable_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        // The directory outlives the state only for the length of a test; the Configuration store
        // is not what these exercise.
        AppState::new(dir.keep().join("configs"))
            .expect("state")
            .with_stale_after(Duration::from_secs(90))
    }

    fn insert(state: &AppState, record: AgentRecord) -> InstanceUid {
        let uid = InstanceUid::default();
        state.fleet.lock().expect("fleet lock").insert(uid, record);
        uid
    }

    fn record_at(last_seen_ms: u64) -> AgentRecord {
        let mut record = record_with(opamp::proto::AgentCapabilities::ReportsStatus as u64);
        record.last_seen_ms = last_seen_ms;
        record
    }

    fn record_with(capabilities: u64) -> AgentRecord {
        AgentRecord {
            sequence_num: 1,
            capabilities,
            description: None,
            health: None,
            effective_config: None,
            remote_config_status: None,
            transport: Transport::WebSocket,
            connected: false,
            last_seen_ms: now_ms(),
            restart_pending: false,
            available_components: None,
            components_demanded: None,
            connection_settings_status: None,
            package_statuses: None,
            owner: None,
            carried_ms: 0,
            heard: false,
            labels: BTreeMap::new(),
            host: None,
            placement: Placement::default(),
            holder: None,
            assignment: None,
        }
    }

    /// A full report for the persistence tests: description, capabilities, and a sequence number.
    fn report(uid: &InstanceUid, sequence_num: u64) -> AgentToServer {
        AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            sequence_num,
            capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
            agent_description: Some(AgentDescription {
                identifying_attributes: vec![opamp::attributes::string_attr(
                    "service.name",
                    "otelcol",
                )],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// ADR-0030: the fleet survives a restart — the record is restored with everything the Agent
    /// reported, shown honestly as disconnected until live evidence says otherwise.
    // Verifies: ADR-0030
    #[test]
    fn the_fleet_is_restored_disconnected_after_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        {
            let state = AppState::new(dir.clone()).expect("state");
            state.process(report(&uid, 1), Transport::WebSocket, Some(1));
        }
        let state = AppState::new(dir).expect("reopened state");
        let snapshot = state.snapshot();
        assert_eq!(snapshot.len(), 1, "the row survived the restart");
        assert_eq!(snapshot[0].service_name, "otelcol");
        assert!(
            !snapshot[0].connected,
            "connectedness is runtime-only and never restored"
        );
    }

    /// Every `Unavailable` — a full enrolment queue, the record ceiling, an audit record that cannot
    /// be written — tells the Agent when to ask again, so it retries instead of giving up or
    /// hammering (ADR-0027 clause 9, ADR-0028 clause 5).
    /// Verifies: ADR-0026, ADR-0028, ADR-0012
    #[test]
    fn unavailable_tells_the_agent_when_to_retry() {
        let reply = unavailable(&[7; 16], "busy");
        assert_eq!(reply.instance_uid, [7; 16], "it names the Agent it answers");
        let error = reply.error_response.expect("an error");
        assert_eq!(error.r#type, ServerErrorResponseType::Unavailable as i32);
        match error.details {
            Some(opamp::proto::server_error_response::Details::RetryInfo(info)) => {
                assert_eq!(info.retry_after_nanoseconds, RETRY_AFTER.as_nanos() as u64);
            }
            other => panic!("no retry_info: {other:?}"),
        }
    }

    /// A new `instance_uid` past the record ceiling is refused `Unavailable` and leaves no record,
    /// so a peer minting fresh self-asserted UIDs (ADR-0026) cannot grow the fleet — and its
    /// in-memory map and per-Agent disk mirror — without bound. Agents already known keep reporting.
    #[test]
    fn a_new_agent_past_the_ceiling_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let state = AppState::new(dir).expect("state").with_max_agents(2);

        let a = InstanceUid::default();
        let b = InstanceUid::default();
        state.process(report(&a, 1), Transport::Http, None);
        state.process(report(&b, 1), Transport::Http, None);
        assert_eq!(state.snapshot().len(), 2, "the fleet filled to its ceiling");

        // A third, genuinely new UID: refused, and told to retry rather than give up.
        let c = InstanceUid::default();
        let processed = state.process(report(&c, 1), Transport::Http, None);
        let error = processed.reply.error_response.expect("an error response");
        assert_eq!(
            error.r#type,
            ServerErrorResponseType::Unavailable as i32,
            "a full fleet answers Unavailable, not BadRequest"
        );
        assert!(
            processed.uid.is_none(),
            "the refused report has no identity to route a config to"
        );
        assert_eq!(
            state.snapshot().len(),
            2,
            "no record was created for the refused UID"
        );

        // An Agent already in the fleet keeps reporting even at the ceiling.
        let processed = state.process(report(&a, 2), Transport::Http, None);
        assert!(
            processed.reply.error_response.is_none(),
            "a known Agent is never refused by the ceiling"
        );
        assert_eq!(state.snapshot().len(), 2);
    }

    /// ADR-0030: a restored sequence number means the next compressed heartbeat is accepted in
    /// place of a fleet-wide ReportFullState stampede.
    #[test]
    fn a_restored_agent_is_not_demanded_a_full_report() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        {
            let state = AppState::new(dir.clone()).expect("state");
            state.process(report(&uid, 1), Transport::WebSocket, Some(1));
            state.flush_agents();
        }
        let state = AppState::new(dir).expect("reopened state");
        let heartbeat = AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            sequence_num: 2,
            ..Default::default()
        };
        let processed = state.process(heartbeat, Transport::WebSocket, Some(1));
        assert_eq!(
            processed.reply.flags & ServerToAgentFlags::ReportFullState as u64,
            0,
            "no gap: the restored record carries the sequence"
        );
    }

    /// ADR-0030: a heartbeat exists to change nothing, and it reaches no storage backend — the
    /// stored record still carries the durable state's write, not the heartbeat's.
    // Verifies: ADR-0030
    #[test]
    fn a_heartbeat_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        let state = AppState::new(dir.clone()).expect("state");
        state.process(report(&uid, 1), Transport::WebSocket, Some(1));
        let path = dir.join("agents").join(format!("{uid}.json"));
        let written = std::fs::read_to_string(&path).expect("the report was persisted");
        let heartbeat = AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            sequence_num: 2,
            ..Default::default()
        };
        state.process(heartbeat, Transport::WebSocket, Some(1));
        let after = std::fs::read_to_string(&path).expect("still persisted");
        assert_eq!(written, after, "the heartbeat performed no write");
    }

    /// ADR-0030: forgetting removes the stored record with the row — nothing
    /// remembers under another name.
    #[test]
    fn forgetting_an_agent_removes_its_stored_record() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        let state = AppState::new(dir.clone())
            .expect("state")
            .with_stale_after(Duration::from_secs(90));
        let mut goodbye = report(&uid, 1);
        goodbye.agent_disconnect = Some(opamp::proto::AgentDisconnect {});
        state.process(goodbye, Transport::WebSocket, Some(1));
        let path = dir.join("agents").join(format!("{uid}.json"));
        assert!(path.exists(), "the record was persisted");
        assert!(
            state.forget_agent(&uid).is_ok(),
            "forgettable: disconnected"
        );
        assert!(!path.exists(), "forgetting frees the store too");
    }

    /// ADR-0030: the persisted record follows a reassigned identity — one record, one file.
    #[test]
    fn a_rekeyed_agent_moves_its_stored_record() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        let state = AppState::new(dir.clone()).expect("state");
        state.process(report(&uid, 1), Transport::WebSocket, Some(1));
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let processed = state.process(rekey, Transport::WebSocket, Some(1));
        let new_uid = processed.uid.expect("the new identity");
        assert_eq!(
            processed.reply.instance_uid,
            uid.as_bytes().to_vec(),
            "the reply is addressed to the identity the Agent asked under"
        );
        assert!(!dir.join("agents").join(format!("{uid}.json")).exists());
        assert!(dir.join("agents").join(format!("{new_uid}.json")).exists());
    }

    /// A fleet on `dir` whose Deployments are armed: an empty package store under `packages`, and
    /// the `stable` channel claiming every `otelcol` holding the Configuration `fleet`.
    fn configuring_fleet(dir: &std::path::Path) -> AppState {
        let store = PackageStore::open(dir.join("packages")).expect("store");
        let state = AppState::new(dir.join("configs"))
            .expect("state")
            .with_packages(Some(
                PackageOffering::new(&dir.join("packages"), store).expect("offering"),
            ));
        if state.configurations().get("fleet").is_none() {
            state
                .save_configuration("fleet", body("receivers: {}\n"))
                .expect("save a Configuration nobody has released");
            state
                .deployment_store()
                .expect("deployments")
                .put(
                    "stable",
                    BTreeMap::from([("service.name".to_string(), "otelcol".to_string())]),
                )
                .expect("deployment");
            state
                .put_deployment_configuration("stable", "fleet")
                .expect("hold it");
        }
        state
    }

    /// A top-level revision of `text` for the Agent type `report` describes.
    fn body(text: &str) -> Revision {
        Revision {
            body: text.to_string(),
            role: String::new(),
            service_name: "otelcol".to_string(),
        }
    }

    /// There is no seed. A record carrying no assignment loads as **assigned nothing** — the Server
    /// never invents a rollout at startup — and what its Deployment would release shows up as
    /// waiting instead, which is the one thing an operator has to act on.
    /// Verifies: ADR-0034, ADR-0033
    #[test]
    fn a_record_without_assignments_loads_assigned_to_nothing() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        {
            let state = configuring_fleet(&dir);
            state.process(report(&uid, 1), Transport::WebSocket, Some(1));
            state.rollout_deployment("stable").expect("rollout");
        }
        let record_path = dir
            .join("configs")
            .join("agents")
            .join(format!("{uid}.json"));
        let mut record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).expect("read"))
                .expect("json");
        let fields = record.as_object_mut().expect("object");
        assert!(fields.remove("assignment").is_some(), "it was assigned");
        std::fs::write(&record_path, serde_json::to_vec(&record).expect("json")).expect("write");

        let state = configuring_fleet(&dir);
        let view = &state.snapshot()[0];
        assert!(
            view.assigned_configurations.is_empty(),
            "an absent assignment means nothing was rolled out, not \"not migrated yet\""
        );
        assert_eq!(
            view.pending_configurations[0].change, "new",
            "what it could receive waits for an explicit act (ADR-0033)"
        );
    }

    /// ADR-0033: saving proposes, the acts assign — and an Agent that appears after the
    /// Deployment's act waits for one of its own (point 6).
    /// Verifies: ADR-0033
    #[test]
    fn rollout_acts_assign_and_a_late_agent_waits() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let state = configuring_fleet(&dir);
        let early = InstanceUid::default();
        state.process(report(&early, 1), Transport::Http, None);

        let view = &state.snapshot()[0];
        assert!(
            view.assigned_configurations.is_empty(),
            "saving assigns nothing"
        );
        assert_eq!(view.pending_configurations[0].change, "new");

        assert_eq!(
            state.rollout_deployment("stable").expect("rollout"),
            1,
            "the Deployment's act assigns the one known Agent"
        );
        assert_eq!(state.snapshot()[0].assigned_configurations, ["fleet"]);
        assert_eq!(state.snapshot()[0].assigned_deployment, "stable");

        // The latecomer: a candidate, waiting, assigned nothing — until its own act.
        let late = InstanceUid::default();
        state.process(report(&late, 1), Transport::Http, None);
        let late_view = state
            .snapshot()
            .into_iter()
            .find(|v| v.instance_uid == late.to_string())
            .expect("the late agent");
        assert!(late_view.assigned_configurations.is_empty());
        assert_eq!(late_view.pending_configurations[0].change, "new");
        state
            .rollout_to_agent(&late, None)
            .expect("rollout to agent");
        let late_view = state
            .snapshot()
            .into_iter()
            .find(|v| v.instance_uid == late.to_string())
            .expect("the late agent");
        assert_eq!(late_view.assigned_configurations, ["fleet"]);
        assert!(late_view.pending_configurations.is_empty());
    }

    /// A rollout whose store fails part-way leaves disk and memory agreeing: the record it wrote
    /// first is put back as it was, the fleet keeps its assignments, the revision those still pin
    /// stays, and the one nothing pins is collected. The next press, once the store writes again,
    /// releases to both.
    /// Verifies: ADR-0023
    #[test]
    fn a_rollout_that_fails_part_way_leaves_disk_and_memory_as_they_were() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = configuring_fleet(dir.path());
        let (first, second) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        for uid in [first, second] {
            state.process(report(&uid, 1), Transport::Http, None);
        }
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 2);
        let pinned = state
            .configurations()
            .get("fleet")
            .expect("fleet")
            .retained
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let agents = dir.path().join("configs").join("agents");
        let first_path = agents.join(format!("{first}.json"));
        let before = std::fs::read(&first_path).expect("the first record");

        state
            .save_configuration("fleet", body("receivers: {otlp: {}}\n"))
            .expect("edit");
        // The second record cannot be replaced: a directory holding a file stands in its place.
        let second_path = agents.join(format!("{second}.json"));
        std::fs::remove_file(&second_path).expect("remove");
        std::fs::create_dir_all(second_path.join("in-the-way")).expect("obstacle");

        assert!(matches!(
            state.rollout_deployment("stable"),
            Err(RolloutError::Storage(_))
        ));
        assert_eq!(
            std::fs::read(&first_path).expect("the first record"),
            before,
            "the record written before the failure is put back"
        );
        for view in state.snapshot() {
            assert_eq!(
                view.pending_configurations[0].change, "update",
                "{}",
                view.instance_uid
            );
        }
        let retained = state
            .configurations()
            .get("fleet")
            .expect("fleet")
            .retained
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            retained, pinned,
            "the pinned revision stays, the unpinned one is collected"
        );

        std::fs::remove_dir_all(&second_path).expect("clear the obstacle");
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 2);
    }

    /// ADR-0030: a queued restart is operator intent and survives the Server restarting.
    #[test]
    fn a_queued_restart_survives_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        {
            let state = AppState::new(dir.clone()).expect("state");
            let mut msg = report(&uid, 1);
            msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
            state.process(msg, Transport::WebSocket, Some(1));
            assert!(state.request_restart(&uid).is_ok(), "queued");
        }
        let state = AppState::new(dir).expect("reopened state");
        let fleet = state.fleet.lock().expect("fleet lock");
        assert!(fleet[&uid].restart_pending, "the intent was restored");
    }

    /// A restart is delivered only while the store still holds it queued: one another Server
    /// delivered and cleared already is not sent twice, and one requested after a delivery is sent
    /// again.
    /// Verifies: ADR-0030
    #[test]
    fn a_restart_is_sent_only_while_the_store_holds_it_queued() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uid = InstanceUid::default();
        let state = AppState::new(dir.path().to_path_buf()).expect("state");
        let mut msg = report(&uid, 1);
        msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
        state.process(msg, Transport::WebSocket, Some(1));
        let other = crate::fs::FsAgentStore::open(dir.path()).expect("open");

        assert!(state.request_restart(&uid).is_ok(), "queued");
        assert_eq!(
            other.set_restart_pending(&uid, false),
            Ok(Some(true)),
            "delivered elsewhere"
        );
        assert!(
            state.restart_command_for(&uid, 1).is_none(),
            "not sent twice"
        );
        assert!(!state.fleet.lock().expect("fleet")[&uid].restart_pending);

        assert!(state.request_restart(&uid).is_ok(), "queued again");
        assert!(state.restart_command_for(&uid, 1).is_some(), "sent");
        assert_eq!(
            other.set_restart_pending(&uid, false),
            Ok(Some(false)),
            "and cleared"
        );
        assert!(state.restart_command_for(&uid, 1).is_none(), "once");
    }

    /// A restart whose command was not sent is queued again, on a lease that lapsed meanwhile
    /// too, and the Agent's next connection is sent it, once; a message that carries no restart
    /// leaves nothing queued. Run on each store.
    fn a_restart_whose_send_failed_is_queued_again(state: &AppState, stored: &dyn AgentStore) {
        let uid = InstanceUid([0x30; 16]);
        let restartable = |sequence_num| {
            let mut msg = report(&uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
            msg
        };
        state.process(restartable(1), Transport::WebSocket, Some(1));
        assert!(state.request_restart(&uid).is_ok(), "queued");
        let command = state.restart_command_for(&uid, 1).expect("sent");
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending
        );

        state.unsent(&ServerToAgent {
            instance_uid: uid.as_bytes().to_vec(),
            ..Default::default()
        });
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "a message without the command queues nothing"
        );

        let mut pushes = state.subscribe();
        pushes.mark_unchanged();
        state.answer_while(|| false);
        state.unsent(&command);
        state.answer_while(|| true);
        assert!(
            pushes.has_changed().expect("pushes"),
            "a session that carries the Agent now is told"
        );
        state.mark_disconnected(&[uid], 1);
        assert!(
            stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "queued again"
        );

        let reply = state
            .process(restartable(2), Transport::WebSocket, Some(2))
            .reply;
        assert_eq!(
            reply.command.map(|c| c.r#type),
            Some(opamp::proto::CommandType::Restart as i32),
            "the next connection is sent it"
        );
        let reply = state
            .process(restartable(3), Transport::WebSocket, Some(2))
            .reply;
        assert!(reply.command.is_none(), "once");
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending
        );
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_restart_whose_send_failed_is_queued_again_on_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = AppState::new(dir.path().to_path_buf()).expect("state");
        let stored = crate::fs::FsAgentStore::open(dir.path()).expect("open");
        a_restart_whose_send_failed_is_queued_again(&state, &stored);
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_restart_whose_send_failed_is_queued_again_on_postgres() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let state = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
        );
        let stored = crate::postgres::agents::PgAgentStore::new(database.clone());
        a_restart_whose_send_failed_is_queued_again(&state, &stored);
    }

    /// An instance whose put-backs of restarts fail until `failing` is cleared, on files, with the
    /// store it writes and a second handle on that store, as another instance writes it.
    fn with_failing_restarts() -> (
        AppState,
        Arc<std::sync::atomic::AtomicBool>,
        crate::fs::FsAgentStore,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooked = Hooked::new(Box::new(
            crate::fs::FsAgentStore::open(dir.path()).expect("open"),
        ));
        let failing = hooked.failing_restarts.clone();
        let state = AppState::with_stores(
            Box::new(hooked),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(crate::clock::SystemClock),
        )
        .expect("state");
        let stored = crate::fs::FsAgentStore::open(dir.path()).expect("open");
        (state, failing, stored, dir)
    }

    fn restartable(uid: &InstanceUid, sequence_num: u64) -> AgentToServer {
        let mut msg = report(uid, sequence_num);
        msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
        msg
    }

    /// A restart taken whose send failed and whose put-back the store refused: kept for retry.
    fn kept_put_back(state: &AppState, failing: &std::sync::atomic::AtomicBool, uid: &InstanceUid) {
        state.process(restartable(uid, 1), Transport::WebSocket, Some(1));
        assert!(state.request_restart(uid).is_ok(), "queued");
        let command = state.restart_command_for(uid, 1).expect("sent");
        failing.store(true, Ordering::SeqCst);
        state.unsent(&command);
        failing.store(false, Ordering::SeqCst);
    }

    /// A kept put-back is one restart with a request made meanwhile, and with a delivery of a
    /// restart another instance queued meanwhile: once that one is delivered, retrying queues no
    /// second.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_is_dropped_by_a_request_or_a_delivery_meanwhile() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let uid = InstanceUid([0x33; 16]);
        kept_put_back(&state, &failing, &uid);
        assert!(state.request_restart(&uid).is_ok(), "requested again");
        assert!(state.restart_command_for(&uid, 1).is_some(), "delivered");
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
        assert!(
            state.restart_command_for(&uid, 1).is_none(),
            "no second restart"
        );

        kept_put_back(&state, &failing, &uid);
        assert_eq!(
            stored.set_restart_pending(&uid, true),
            Ok(Some(false)),
            "requested through another instance"
        );
        state.take_up_agents(None).expect("taken up");
        assert!(state.restart_command_for(&uid, 1).is_some(), "delivered");
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
        assert!(
            state.restart_command_for(&uid, 1).is_none(),
            "no second restart"
        );

        // Requested here and delivered through another instance.
        kept_put_back(&state, &failing, &uid);
        assert!(state.request_restart(&uid).is_ok(), "requested again");
        assert_eq!(
            stored.set_restart_pending(&uid, false),
            Ok(Some(true)),
            "delivered through another instance"
        );
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "no second restart"
        );
    }

    /// A kept put-back goes with its Agent's forget, through this instance or one heard of from
    /// another: an Agent that comes back under the same identity is not restarted for it.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_goes_with_its_agents_forget() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let (here, elsewhere) = (InstanceUid([0x34; 16]), InstanceUid([0x35; 16]));
        kept_put_back(&state, &failing, &here);
        kept_put_back(&state, &failing, &elsewhere);
        state.mark_disconnected(&[here, elsewhere], 1);
        assert!(state.forget_agent(&here).is_ok(), "forgotten here");
        stored.remove(&elsewhere).expect("forgotten elsewhere");
        state.take_up_agents(None).expect("taken up");
        for (uid, conn) in [(here, 2), (elsewhere, 3)] {
            state.process(restartable(&uid, 1), Transport::WebSocket, Some(conn));
        }
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
        for uid in [here, elsewhere] {
            assert!(
                !stored
                    .get(&uid)
                    .expect("get")
                    .expect("stored")
                    .restart_pending,
                "the Agent that came back is not restarted"
            );
        }
    }

    /// A try found due is not made once a request and its delivery dropped the put-back before
    /// the try took the fleet lock, so the Agent is not restarted a second time.
    /// Verifies: ADR-0025
    #[test]
    fn a_try_found_due_is_not_made_once_a_request_and_its_delivery_dropped_it() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let uid = InstanceUid([0x36; 16]);
        kept_put_back(&state, &failing, &uid);
        let due = state.due_restarts();
        assert_eq!(due, [uid], "due at the next call");
        // Between finding it due and trying it: requested and delivered.
        assert!(state.request_restart(&uid).is_ok(), "requested again");
        assert!(state.restart_command_for(&uid, 1).is_some(), "delivered");
        for uid in &due {
            state.put_back(uid, true);
        }
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "no second restart"
        );
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
    }

    /// A request through another instance, taken up from its announcement, drops a put-back kept
    /// here, so its delivery there is the one restart; after a missed notification one whose
    /// record shows a restart queued is dropped, and one whose record shows none is kept.
    /// Verifies: ADR-0025
    #[test]
    fn a_request_announced_by_another_instance_drops_a_kept_put_back() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let (announced, queued, other) = (
            InstanceUid([0x37; 16]),
            InstanceUid([0x38; 16]),
            InstanceUid([0x39; 16]),
        );
        for uid in [&announced, &queued, &other] {
            kept_put_back(&state, &failing, uid);
        }
        assert_eq!(
            stored.set_restart_pending(&announced, true),
            Ok(Some(false))
        );
        state
            .take_up_restarts(Some(&HashSet::from([announced])))
            .expect("taken up");
        assert_eq!(
            stored.set_restart_pending(&announced, false),
            Ok(Some(true)),
            "delivered there"
        );

        assert_eq!(stored.set_restart_pending(&queued, true), Ok(Some(false)));
        state.take_up_restarts(None).expect("taken up");
        assert_eq!(
            stored.set_restart_pending(&queued, false),
            Ok(Some(true)),
            "delivered there"
        );

        let mut kept = usize::MAX;
        for _ in 0..4 {
            kept = state.retry_restarts();
        }
        assert_eq!(kept, 0, "every put-back held or was dropped");
        for uid in [announced, queued] {
            assert!(
                !stored
                    .get(&uid)
                    .expect("get")
                    .expect("stored")
                    .restart_pending,
                "no second restart"
            );
        }
        assert!(
            stored
                .get(&other)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "the put-back nothing announced is queued again"
        );
    }

    /// On PostgreSQL the announcement of a request through another instance is what drops a
    /// put-back kept here: once it is taken up, the request's delivery there leaves nothing for a
    /// retry here to restart again.
    /// Verifies: ADR-0025
    #[test]
    fn a_request_announced_by_another_instance_drops_a_kept_put_back_on_postgres() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.shared();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_restarts.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let following = {
            let _entered = runtime.enter();
            crate::instance::follow(&database, a.clone(), None)
        };
        until("the first read once listening", || following.reads() > 0);
        let uid = InstanceUid([0x3a; 16]);
        kept_put_back(&a, &failing, &uid);
        let kept = || a.unput_restarts.lock().expect("restarts lock").len();
        assert_eq!(kept(), 1, "kept for retry");

        let other = crate::postgres::agents::PgAgentStore::new(test.shared());
        assert_eq!(
            other.set_restart_pending(&uid, true),
            Ok(Some(false)),
            "requested through another instance"
        );
        until("the request taken up", || kept() == 0);
        assert_eq!(
            other.set_restart_pending(&uid, false),
            Ok(Some(true)),
            "delivered there"
        );
        assert_eq!(a.retry_restarts(), 0, "nothing is kept");
        assert_eq!(
            other.get(&uid).map(|r| r.map(|r| r.restart_pending)),
            Ok(Some(false)),
            "no second restart"
        );
    }

    /// The wait between two tries of a put-back doubles and stops at a minute of calls.
    #[test]
    fn the_wait_between_tries_of_a_put_back_grows_to_a_minute() {
        let mut backoff = Backoff::first();
        let mut waits = Vec::new();
        for _ in 0..8 {
            let mut calls = 1;
            while !backoff.tick() {
                calls += 1;
            }
            waits.push(calls);
            backoff.failed();
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
    }

    /// A put-back the store does not take is kept and tried again, at intervals that double, until
    /// it holds; the Agent's next connection is then sent the restart. One whose Agent is
    /// forgotten meanwhile is dropped.
    /// Verifies: ADR-0025
    #[test]
    fn a_put_back_the_store_refuses_is_tried_again_until_it_holds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooked = Hooked::new(Box::new(
            crate::fs::FsAgentStore::open(dir.path()).expect("open"),
        ));
        let (failing, set) = (hooked.failing_restarts.clone(), hooked.restarts_set.clone());
        let state = AppState::with_stores(
            Box::new(hooked),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(crate::clock::SystemClock),
        )
        .expect("state");
        let stored = crate::fs::FsAgentStore::open(dir.path()).expect("open");
        let (uid, gone) = (InstanceUid([0x31; 16]), InstanceUid([0x32; 16]));
        let restartable = |uid: &InstanceUid, sequence_num| {
            let mut msg = report(uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
            msg
        };
        for uid in [&uid, &gone] {
            state.process(restartable(uid, 1), Transport::WebSocket, Some(1));
            assert!(state.request_restart(uid).is_ok(), "queued");
        }
        let command = state.restart_command_for(&uid, 1).expect("sent");
        let other = state.restart_command_for(&gone, 1).expect("sent");
        let tries = || set.load(Ordering::SeqCst);
        let base = tries();

        failing.store(true, Ordering::SeqCst);
        state.unsent(&command);
        state.unsent(&other);
        assert_eq!(tries(), base + 2, "each put back once");
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending
        );
        // Tried at the next call, then after two calls, then after four.
        let mut seen = Vec::new();
        for _ in 0..7 {
            state.retry_restarts();
            seen.push(tries() - base);
        }
        assert_eq!(seen, [4, 4, 6, 6, 6, 6, 8], "the intervals double");

        state.mark_disconnected(&[uid, gone], 1);
        assert!(state.forget_agent(&gone).is_ok(), "forgotten");
        failing.store(false, Ordering::SeqCst);
        let mut kept = usize::MAX;
        for _ in 0..8 {
            kept = state.retry_restarts();
            if kept == 0 {
                break;
            }
        }
        assert_eq!(kept, 0, "it held, and the forgotten Agent's was dropped");
        assert!(
            stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "queued again"
        );
        let reply = state
            .process(restartable(&uid, 2), Transport::WebSocket, Some(2))
            .reply;
        assert_eq!(
            reply.command.map(|c| c.r#type),
            Some(opamp::proto::CommandType::Restart as i32),
            "the next connection is sent it"
        );
    }

    /// Whether the record `stored` holds of `uid` has a restart queued; `None` without a record.
    fn queued(stored: &dyn AgentStore, uid: &InstanceUid) -> Option<bool> {
        stored
            .get(uid)
            .expect("get")
            .map(|record| record.restart_pending)
    }

    /// A kept put-back follows the re-key its Agent asks for: the retry queues the restart on the
    /// new identity, and the Agent's next report under it is sent the restart. Run on each store.
    fn a_kept_put_back_follows_a_rekey_the_agent_asks_for(
        state: &AppState,
        failing: &std::sync::atomic::AtomicBool,
        stored: &dyn AgentStore,
    ) {
        let uid = InstanceUid([0x3b; 16]);
        kept_put_back(state, failing, &uid);
        let mut rekey = restartable(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let new = state
            .process(rekey, Transport::WebSocket, Some(1))
            .uid
            .expect("carried");
        assert_ne!(new, uid, "re-keyed");
        assert_eq!(state.retry_restarts(), 0, "the retry held");
        assert_eq!(queued(stored, &uid), None, "the old row is gone");
        assert_eq!(
            queued(stored, &new),
            Some(true),
            "queued on the new identity"
        );
        let reply = state
            .process(restartable(&new, 3), Transport::WebSocket, Some(1))
            .reply;
        assert_eq!(
            reply.command.map(|c| c.r#type),
            Some(opamp::proto::CommandType::Restart as i32),
            "the next report is sent it"
        );
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_follows_a_rekey_the_agent_asks_for_on_files() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        a_kept_put_back_follows_a_rekey_the_agent_asks_for(&state, &failing, &stored);
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_follows_a_rekey_the_agent_asks_for_on_postgres() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_restarts.clone();
        let state = on_postgres(&database, Box::new(hooked));
        state.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let stored = crate::postgres::agents::PgAgentStore::new(database.clone());
        a_kept_put_back_follows_a_rekey_the_agent_asks_for(&state, &failing, &stored);
    }

    /// A kept put-back follows a re-key whose answer was lost and whose read back failed, once a
    /// read again of what is held reads it back as moved: the retry queues the restart on the new
    /// identity.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_follows_a_rekey_read_back_as_moved() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        a_kept_put_back_follows_a_rekey_read_back(&test, false);
    }

    /// A try of a kept put-back that finds the row gone while a re-key of this instance's own is
    /// not read back yet keeps it, and the read back moves it: the restart is queued on the new
    /// identity, not dropped.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_tried_before_its_rekey_is_read_back_follows_it() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        a_kept_put_back_follows_a_rekey_read_back(&test, true);
    }

    /// A kept put-back and a re-key of its Agent whose answer was lost and whose read back failed;
    /// the store answers again, a try is made before the read back where `tried_first`, and the
    /// put-back is tried until it holds.
    fn a_kept_put_back_follows_a_rekey_read_back(
        test: &crate::postgres::testing::TestDatabase,
        tried_first: bool,
    ) {
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let (failing, losing, unreadable) = (
            hooked.failing_restarts.clone(),
            hooked.losing_rekeys.clone(),
            hooked.failing_moved.clone(),
        );
        let a = on_postgres(&database, Box::new(hooked));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let stored = crate::postgres::agents::PgAgentStore::new(database.clone());
        let uid = InstanceUid([0x3c; 16]);
        kept_put_back(&a, &failing, &uid);
        losing.store(true, Ordering::SeqCst);
        unreadable.store(true, Ordering::SeqCst);
        let mut rekey = restartable(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process(rekey, Transport::WebSocket, Some(1));
        losing.store(false, Ordering::SeqCst);
        assert_eq!(answered.uid, Some(uid), "not known: it keeps its identity");
        unreadable.store(false, Ordering::SeqCst);
        if tried_first {
            assert_eq!(
                a.retry_restarts(),
                1,
                "kept while the re-key is not read back"
            );
        }
        a.take_up_agents(None).expect("read again");
        let held = held_by(&a);
        assert_eq!(held.len(), 1);
        let new = InstanceUid::parse(&held[0]).expect("a uid");
        assert_ne!(new, uid, "read back as moved");
        let kept = (0..8).map(|_| a.retry_restarts()).find(|kept| *kept == 0);
        assert_eq!(kept, Some(0), "the retry held");
        assert_eq!(
            queued(&stored, &new),
            Some(true),
            "queued on the new identity"
        );
    }

    /// A put-back whose Agent was re-keyed through another instance while this one retried finds
    /// its record gone: the restart is not tried again, and a warning names the Agent.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_whose_row_another_instance_rekeyed_is_logged() {
        crate::test_support::logged();
        let (state, failing, stored, _dir) = with_failing_restarts();
        let uid = InstanceUid([0x3e; 16]);
        kept_put_back(&state, &failing, &uid);
        state.mark_disconnected(&[uid], 1);
        let new = InstanceUid([0x3f; 16]);
        let record = stored.get(&uid).expect("get").expect("stored");
        stored
            .rekey(&uid, &new, &record)
            .expect("re-keyed through another instance");
        assert_eq!(state.retry_restarts(), 0, "not tried again");
        assert_eq!(queued(&stored, &new), Some(false), "not seen");
        assert_eq!(
            crate::test_support::lines_with(&[
                "the restart is no longer tried, the Agent's row is gone",
                &uid.to_string(),
            ])
            .len(),
            1,
            "a warning names the Agent"
        );
    }

    /// The re-key of a duplicate newcomer moves no row: a put-back kept for the Agent stays with
    /// it, the retry queues the restart on its identity, and the newcomer is sent none.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_stays_with_its_agent_when_a_duplicate_is_rekeyed() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let uid = InstanceUid([0x3d; 16]);
        kept_put_back(&state, &failing, &uid);
        let duplicate = state.process(restartable(&uid, 1), Transport::WebSocket, Some(2));
        let newcomer = duplicate.uid.expect("carried");
        assert_ne!(newcomer, uid, "the newcomer is re-keyed");
        assert!(duplicate.reply.agent_identification.is_some());
        assert!(duplicate.reply.command.is_none());
        assert_eq!(state.retry_restarts(), 0, "the retry held");
        assert_eq!(queued(&stored, &uid), Some(true), "queued on the Agent");
        assert_ne!(
            queued(&stored, &newcomer),
            Some(true),
            "not on the newcomer"
        );
        let reply = state
            .process(restartable(&newcomer, 2), Transport::WebSocket, Some(2))
            .reply;
        assert!(reply.command.is_none(), "the newcomer is sent no restart");
        let reply = state
            .process(restartable(&uid, 2), Transport::WebSocket, Some(1))
            .reply;
        assert_eq!(
            reply.command.map(|c| c.r#type),
            Some(opamp::proto::CommandType::Restart as i32),
            "the Agent is sent it"
        );
    }

    /// ADR-0038: the fleet table shows the release, and the build stays reachable beside it. A
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

    /// The whole-store ceiling (ADR-0034), decided without a request: a store at its limit takes
    /// no upload, an artifact is refused when it would take the store past it, and an upload still
    /// arriving counts until it is dropped.
    #[test]
    fn the_package_store_ceiling_admits_up_to_its_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = AppState::new(dir.path().join("configs"))
            .expect("state")
            .with_max_total_package_bytes(100);
        {
            let mut upload = state
                .admit_upload()
                .expect("an empty store takes an upload");
            assert_eq!(upload.grow(100), Ok(()));
            assert_eq!(upload.admit_artifact(), Ok(()));
            assert!(
                matches!(state.admit_upload(), Err(StoreFull::AtLimit { limit: 100 })),
                "an upload in progress fills the store as much as a stored one"
            );
            assert_eq!(
                upload.grow(1),
                Err(StoreFull::WouldExceed {
                    size: 101,
                    limit: 100
                })
            );
            assert_eq!(
                upload.admit_artifact(),
                Err(StoreFull::WouldExceed {
                    size: 101,
                    limit: 100
                })
            );
        }
        assert!(
            state.admit_upload().is_ok(),
            "a dropped upload counts no more"
        );
        let full = state.with_max_total_package_bytes(0);
        assert!(matches!(
            full.admit_upload(),
            Err(StoreFull::AtLimit { limit: 0 })
        ));
    }

    /// A certificate of one host does not speak for another host's Agent: the reporter is re-keyed
    /// to an identity of its own, and the Agent it claimed keeps its record.
    /// Verifies: ADR-0026, G-17
    #[test]
    fn a_host_cannot_report_for_another_hosts_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let revocations = Arc::new(
            Revocations::open(
                Box::new(crate::fs::FsLedgerStore::open(dir.path()).expect("ledger")),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::new(dir.path().join("configs"))
            .expect("state")
            .with_revocations(Some(revocations));
        let presented = |host: &str, serial: &str| Presented {
            id: CertId::new(b"CA", serial),
            host: Some(host.to_string()),
            subject_values: crate::revocation::SubjectValues::default(),
            not_after_ms: u64::MAX,
        };
        let victim = InstanceUid::default();
        let own = state.process_presented(
            {
                let mut first = AgentToServer {
                    instance_uid: victim.as_bytes().to_vec(),
                    ..Default::default()
                };
                first.capabilities = opamp::proto::AgentCapabilities::ReportsStatus as u64;
                first
            },
            Transport::Http,
            None,
            Some(&presented("h1", "01")),
        );
        assert_eq!(own.uid, Some(victim));
        assert!(own.reply.agent_identification.is_none());

        let claimed = state.process_presented(
            AgentToServer {
                instance_uid: victim.as_bytes().to_vec(),
                flags: AgentToServerFlags::RequestInstanceUid as u64,
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&presented("h2", "02")),
        );
        let new_uid = claimed.uid.expect("an identity");
        assert_ne!(new_uid, victim, "another host spoke for the Agent");
        assert_eq!(
            claimed.reply.instance_uid,
            victim.as_bytes().to_vec(),
            "the reply is addressed to the identity the reporter sent, or it routes to no one"
        );
        assert_eq!(
            claimed
                .reply
                .agent_identification
                .expect("told to adopt it")
                .new_instance_uid,
            new_uid.as_bytes().to_vec()
        );
        assert!(
            state
                .snapshot()
                .iter()
                .any(|agent| agent.instance_uid == victim.to_string()),
            "the claimed Agent's record moved"
        );

        // Having adopted it, the reporter speaks under its new identity and is re-keyed no more:
        // no further record appears for it.
        let adopted = state.process_presented(
            AgentToServer {
                instance_uid: new_uid.as_bytes().to_vec(),
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&presented("h2", "02")),
        );
        assert_eq!(adopted.uid, Some(new_uid));
        assert!(adopted.reply.agent_identification.is_none());
        assert_eq!(state.snapshot().len(), 2, "one record per Agent, no more");
    }

    /// Forgetting an Agent releases its binding: another host's report for it binds it, under the
    /// identity it reported, where before it would have been re-keyed.
    /// Verifies: ADR-0030
    #[test]
    fn forgetting_an_agent_releases_its_binding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let revocations = Arc::new(
            Revocations::open(
                Box::new(crate::fs::FsLedgerStore::open(dir.path()).expect("ledger")),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::new(dir.path().join("configs"))
            .expect("state")
            .with_revocations(Some(revocations.clone()));
        let presented = |host: &str| Presented {
            id: CertId::new(b"CA", host),
            host: Some(host.to_string()),
            subject_values: crate::revocation::SubjectValues::default(),
            not_after_ms: u64::MAX,
        };
        let uid = InstanceUid::default();
        let message = || AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
            ..Default::default()
        };
        let conn = state.connection_id();
        let first = state.process_presented(
            message(),
            Transport::WebSocket,
            Some(conn),
            Some(&presented("h1")),
        );
        assert_eq!(first.uid, Some(uid));
        state.mark_disconnected(&[uid], conn);
        assert!(state.forget_agent(&uid).is_ok(), "forgotten");
        assert_eq!(
            revocations.speaks_for("h1"),
            crate::revocation::SpeaksFor::Agents(std::collections::BTreeSet::new()),
            "released from h1"
        );

        let other = state.process_presented(
            message(),
            Transport::WebSocket,
            Some(state.connection_id()),
            Some(&presented("h2")),
        );
        assert_eq!(other.uid, Some(uid), "bound by h2, not re-keyed");
        assert!(other.reply.agent_identification.is_none());
        assert_eq!(
            revocations.speaks_for("h2"),
            crate::revocation::SpeaksFor::Agents([uid.to_string().replace('-', "")].into())
        );
    }

    /// A report refused at the record ceiling binds no `instance_uid` to the host that sent it,
    /// so it cannot speak for that Agent once there is room — nor lock out the host it belongs to.
    /// Verifies: ADR-0026, ADR-0030
    #[test]
    fn a_report_refused_at_the_ceiling_binds_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let revocations = Arc::new(
            Revocations::open(
                Box::new(crate::fs::FsLedgerStore::open(dir.path()).expect("ledger")),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::new(dir.path().join("configs"))
            .expect("state")
            .with_max_agents(1)
            .with_revocations(Some(revocations.clone()));
        let presented = |host: &str| Presented {
            id: CertId::new(b"CA", host),
            host: Some(host.to_string()),
            subject_values: crate::revocation::SubjectValues::default(),
            not_after_ms: u64::MAX,
        };
        let first = InstanceUid::default();
        let accepted = state.process_presented(
            report(&first, 1),
            Transport::Http,
            None,
            Some(&presented("h1")),
        );
        assert!(accepted.reply.error_response.is_none());

        for flags in [0, AgentToServerFlags::RequestInstanceUid as u64] {
            let refused_uid = InstanceUid::default();
            let mut refused = report(&refused_uid, 1);
            refused.flags = flags;
            let refused =
                state.process_presented(refused, Transport::Http, None, Some(&presented("h2")));
            assert_eq!(
                refused.reply.error_response.expect("refused").r#type,
                ServerErrorResponseType::Unavailable as i32
            );
            assert_eq!(
                revocations.speaks_for("h2"),
                SpeaksFor::Agents(BTreeSet::new()),
                "a refused report bound an instance_uid (flags {flags})"
            );
        }
        assert_eq!(
            revocations.speaks_for("h1"),
            SpeaksFor::Agents([hex::encode(first.as_bytes())].into())
        );
    }

    // ---- Who may fetch an uploaded artifact (ADR-0034) ----

    /// A fleet delivering `otelcol@1.0.0`, uploaded and signed for linux/amd64 and held by the
    /// `stable` channel that claims every `otelcol`, with a host register.
    fn delivering_fleet(dir: &std::path::Path) -> (AppState, Arc<Revocations>, PackageId) {
        let store = PackageStore::open(dir.join("packages")).expect("store");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        store.create(&id).expect("create");
        store
            .put_entry(&id, &linux(), b"v1".to_vec())
            .expect("entry");
        let revocations = Arc::new(
            Revocations::open(
                Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::new(dir.join("configs"))
            .expect("state")
            .with_packages(Some(
                PackageOffering::new(&dir.join("packages"), store).expect("offering"),
            ))
            .with_revocations(Some(revocations.clone()));
        state
            .deployment_store()
            .expect("deployments")
            .put(
                "stable",
                BTreeMap::from([("service.name".to_string(), "otelcol".to_string())]),
            )
            .expect("deployment");
        put_signed(&state, &id);
        (state, revocations, id)
    }

    fn linux() -> Platform {
        Platform::new("linux", "amd64").expect("platform")
    }

    /// Signs `id` for linux/amd64 and puts it into the `stable` channel — saved, released to nobody.
    fn put_signed(state: &AppState, id: &PackageId) {
        state
            .put_package_entry_signature(id, &linux(), vec![1; 64])
            .expect("signature");
        state
            .put_deployment_package("stable", id, true)
            .expect("package");
    }

    /// One report of an `otelcol` Agent on linux/amd64 that accepts packages, over a certificate
    /// naming `host`, echoing `echoed` as the aggregate hash it holds.
    fn report_from(state: &AppState, host: &str, uid: InstanceUid, echoed: &[u8]) -> Processed {
        let attr = opamp::attributes::string_attr;
        state.process_presented(
            AgentToServer {
                instance_uid: uid.as_bytes().to_vec(),
                capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64
                    | opamp::proto::AgentCapabilities::AcceptsPackages as u64
                    | opamp::proto::AgentCapabilities::ReportsPackageStatuses as u64,
                agent_description: Some(AgentDescription {
                    identifying_attributes: vec![attr("service.name", "otelcol")],
                    non_identifying_attributes: vec![
                        attr("os.type", "linux"),
                        attr("host.arch", "amd64"),
                    ],
                }),
                package_statuses: (!echoed.is_empty()).then(|| PackageStatuses {
                    server_provided_all_packages_hash: echoed.to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&Presented {
                id: CertId::new(b"CA", host),
                host: Some(host.to_string()),
                subject_values: crate::revocation::SubjectValues::default(),
                not_after_ms: u64::MAX,
            }),
        )
    }

    /// An artifact is fetched by a host only through an Agent it speaks for: the host whose Agent
    /// it was released to, not another host, not a host the register does not know, and not for
    /// another Platform or version — until that other host is marked as a Gateway.
    /// Verifies: ADR-0034, Q-4
    #[test]
    fn an_artifact_is_offered_to_a_host_only_through_an_agent_it_speaks_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, revocations, id) = delivering_fleet(dir.path());
        report_from(&state, "h1", InstanceUid([1; 16]), &[]);
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);
        // h2 reports an Agent of its own, released nothing.
        state.process_presented(
            AgentToServer {
                instance_uid: vec![2; 16],
                capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&Presented {
                id: CertId::new(b"CA", "h2"),
                host: Some("h2".to_string()),
                subject_values: crate::revocation::SubjectValues::default(),
                not_after_ms: u64::MAX,
            }),
        );

        assert!(state.offers_artifact("h1", &id, &linux()));
        assert!(!state.offers_artifact("h2", &id, &linux()));
        assert!(!state.offers_artifact("unknown", &id, &linux()));
        let windows = Platform::new("windows", "amd64").expect("platform");
        assert!(!state.offers_artifact("h1", &id, &windows));
        let v2 = PackageId::new("otelcol", "2.0.0").expect("id");
        assert!(!state.offers_artifact("h1", &v2, &linux()));

        assert!(revocations.set_gateway("h2", true).expect("mark"));
        assert!(
            state.offers_artifact("h2", &id, &linux()),
            "a Gateway speaks for any Agent"
        );
    }

    /// The offer and the download agree on the capability half of the test (ADR-0034 clause 35): an
    /// Agent that does not declare `AcceptsPackages` is offered nothing, and its host may fetch
    /// nothing through it, although its assignment names a signed Package that fits it.
    /// Verifies: ADR-0034
    #[test]
    fn an_agent_that_accepts_no_packages_is_neither_offered_nor_fetched_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, _, id) = delivering_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_from(&state, "h1", uid, &[]);
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);
        assert!(state.offers_artifact("h1", &id, &linux()));
        let attr = opamp::attributes::string_attr;
        let reply = state.process_presented(
            AgentToServer {
                instance_uid: uid.as_bytes().to_vec(),
                sequence_num: 1,
                capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
                agent_description: Some(AgentDescription {
                    identifying_attributes: vec![attr("service.name", "otelcol")],
                    non_identifying_attributes: vec![
                        attr("os.type", "linux"),
                        attr("host.arch", "amd64"),
                    ],
                }),
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&Presented {
                id: CertId::new(b"CA", "h1"),
                host: Some("h1".to_string()),
                subject_values: crate::revocation::SubjectValues::default(),
                not_after_ms: u64::MAX,
            }),
        );
        assert!(reply.reply.packages_available.is_none(), "no offer");
        assert!(!state.offers_artifact("h1", &id, &linux()), "and no fetch");
    }

    /// An Agent that echoes the aggregate hash of its offer is not sent it again, and can still
    /// fetch it — a retry after a failed install re-reads an offer no longer re-sent.
    /// Verifies: ADR-0034
    #[test]
    fn an_offer_still_stands_after_its_hash_is_echoed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, _, id) = delivering_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_from(&state, "h1", uid, &[]);
        state.rollout_deployment("stable").expect("rollout");
        let offer = report_from(&state, "h1", uid, &[])
            .reply
            .packages_available
            .expect("an offer");
        let echoed = report_from(&state, "h1", uid, &offer.all_packages_hash);
        assert!(echoed.reply.packages_available.is_none(), "the hash gate");
        assert!(state.offers_artifact("h1", &id, &linux()));
    }

    /// A version saved into the channel but not yet released by an operator's press is no one's
    /// offer: not the Agent's host's, not a Gateway's — while the version released before stays.
    /// Verifies: ADR-0034
    #[test]
    fn a_version_waiting_for_its_press_is_offered_to_no_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, revocations, v1) = delivering_fleet(dir.path());
        report_from(&state, "h1", InstanceUid([1; 16]), &[]);
        report_from(&state, "gw", InstanceUid([9; 16]), &[]);
        assert!(revocations.set_gateway("gw", true).expect("mark"));
        state.rollout_deployment("stable").expect("rollout");

        let v2 = PackageId::new("otelcol", "2.0.0").expect("id");
        let store = state.packages().expect("store");
        store.create(&v2).expect("create");
        store
            .put_entry(&v2, &linux(), b"v2".to_vec())
            .expect("entry");
        put_signed(&state, &v2);
        for host in ["h1", "gw"] {
            assert!(!state.offers_artifact(host, &v2, &linux()), "{host}");
            assert!(state.offers_artifact(host, &v1, &linux()), "{host}");
        }
        assert_eq!(state.rollout_deployment("stable").expect("the press"), 2);
        assert!(state.offers_artifact("h1", &v2, &linux()));
    }

    // ---- A Deployment carries Configurations beside its Packages (ADR-0033) ----

    /// One report of an `otelcol` Agent on linux/amd64 over a certificate naming `h1` that takes
    /// both remote configuration and packages, echoing the hashes it holds.
    fn report_holding(
        state: &AppState,
        uid: InstanceUid,
        config_hash: &[u8],
        packages_hash: &[u8],
    ) -> Processed {
        use opamp::proto::AgentCapabilities as C;
        let attr = opamp::attributes::string_attr;
        state.process_presented(
            AgentToServer {
                instance_uid: uid.as_bytes().to_vec(),
                capabilities: C::ReportsStatus as u64
                    | C::AcceptsRemoteConfig as u64
                    | C::AcceptsPackages as u64
                    | C::ReportsPackageStatuses as u64,
                agent_description: Some(AgentDescription {
                    identifying_attributes: vec![attr("service.name", "otelcol")],
                    non_identifying_attributes: vec![
                        attr("os.type", "linux"),
                        attr("host.arch", "amd64"),
                    ],
                }),
                remote_config_status: (!config_hash.is_empty()).then(|| RemoteConfigStatus {
                    last_remote_config_hash: config_hash.to_vec(),
                    status: RemoteConfigStatuses::Applied as i32,
                    ..Default::default()
                }),
                package_statuses: (!packages_hash.is_empty()).then(|| PackageStatuses {
                    server_provided_all_packages_hash: packages_hash.to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&Presented {
                id: CertId::new(b"CA", "h1"),
                host: Some("h1".to_string()),
                subject_values: crate::revocation::SubjectValues::default(),
                not_after_ms: u64::MAX,
            }),
        )
    }

    /// The names of the entries a config offer carries.
    fn offered_names(offer: &AgentRemoteConfig) -> Vec<String> {
        let mut names: Vec<String> = offer
            .config
            .as_ref()
            .expect("a map")
            .config_map
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// `delivering_fleet` with two Configurations in `stable`: `base`, for the Agent's type, and
    /// `fluent-bit-conf`, typed for another Agent type.
    fn mixed_fleet(dir: &std::path::Path) -> (AppState, PackageId) {
        let (state, _, id) = delivering_fleet(dir);
        state
            .save_configuration("base", body("receivers: {}\n"))
            .expect("base");
        let mut fluent_bit = body("[SERVICE]\n");
        fluent_bit.service_name = "fluent-bit".to_string();
        state
            .save_configuration("fluent-bit-conf", fluent_bit)
            .expect("fluent-bit");
        for name in ["base", "fluent-bit-conf"] {
            state
                .put_deployment_configuration("stable", name)
                .expect("hold it");
        }
        (state, id)
    }

    /// A Deployment holding a Package and two Configurations, one typed for another Agent type,
    /// releases to an Agent the Package and the one Configuration that fits it — one act, one
    /// assignment.
    /// Verifies: ADR-0033
    #[test]
    fn a_deployment_releases_its_package_and_the_fitting_configurations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, id) = mixed_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_holding(&state, uid, &[], &[]);
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);

        let reply = report_holding(&state, uid, &[], &[]).reply;
        let config = reply.remote_config.expect("a config offer");
        assert_eq!(
            offered_names(&config),
            ["base"],
            "the typed one does not fit"
        );
        let packages = reply.packages_available.expect("a package offer");
        assert_eq!(
            packages.packages.keys().collect::<Vec<_>>(),
            [&id.agent_type]
        );
        let view = &state.snapshot()[0];
        assert_eq!(view.assigned_deployment, "stable");
        assert_eq!(view.assigned_package, id.to_string());
        assert_eq!(view.assigned_configurations, ["base"]);
        assert!(view.pending_configurations.is_empty());
        assert!(view.pending_packages.is_empty());
    }

    /// The hash gates send only what changed: a new body re-sends the config map and no package
    /// offer, and a new Package re-sends the package offer and no config map — though each is
    /// one press of the whole Deployment.
    /// Verifies: ADR-0033
    #[test]
    fn a_second_press_sends_only_the_part_that_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, _) = mixed_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_holding(&state, uid, &[], &[]);
        state.rollout_deployment("stable").expect("rollout");
        let first = report_holding(&state, uid, &[], &[]).reply;
        let config_hash = first.remote_config.expect("config").config_hash;
        let packages_hash = first
            .packages_available
            .expect("packages")
            .all_packages_hash;
        let settled = report_holding(&state, uid, &config_hash, &packages_hash).reply;
        assert!(settled.remote_config.is_none() && settled.packages_available.is_none());

        state
            .save_configuration("base", body("receivers: {otlp: {}}\n"))
            .expect("edit");
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);
        let reply = report_holding(&state, uid, &config_hash, &packages_hash).reply;
        let config_hash = reply.remote_config.expect("the new map").config_hash;
        assert!(reply.packages_available.is_none(), "no new package offer");

        let v2 = PackageId::new("otelcol", "2.0.0").expect("id");
        let store = state.packages().expect("store");
        store.create(&v2).expect("create");
        store
            .put_entry(&v2, &linux(), b"v2".to_vec())
            .expect("entry");
        put_signed(&state, &v2);
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);
        let reply = report_holding(&state, uid, &config_hash, &packages_hash).reply;
        assert!(reply.remote_config.is_none(), "no new config map");
        assert!(reply.packages_available.is_some(), "the new package offer");
    }

    /// A press of a Deployment that no longer holds a Configuration drops it from the Agent's map;
    /// until the press, the view shows the drop waiting.
    /// Verifies: ADR-0033
    #[test]
    fn a_configuration_the_deployment_dropped_leaves_the_map_on_the_next_press() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, _) = mixed_fleet(dir.path());
        state
            .save_configuration("extra", body("exporters: {}\n"))
            .expect("extra");
        state
            .put_deployment_configuration("stable", "extra")
            .expect("hold it");
        let uid = InstanceUid([1; 16]);
        report_holding(&state, uid, &[], &[]);
        state.rollout_deployment("stable").expect("rollout");
        let offer = report_holding(&state, uid, &[], &[])
            .reply
            .remote_config
            .expect("offer");
        assert_eq!(offered_names(&offer), ["base", "extra"]);

        state
            .remove_deployment_configuration("stable", "extra")
            .expect("drop it");
        let view = &state.snapshot()[0];
        assert_eq!(view.assigned_configurations, ["base", "extra"]);
        assert_eq!(
            view.pending_configurations
                .iter()
                .map(|p| (p.name.as_str(), p.change.as_str()))
                .collect::<Vec<_>>(),
            [("extra", "remove")]
        );
        assert_eq!(
            state.deployment_reach().expect("reach")["stable"].targeted,
            1
        );

        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);
        assert_eq!(state.snapshot()[0].assigned_configurations, ["base"]);
        let offer = report_holding(&state, uid, &offer.config_hash, &[])
            .reply
            .remote_config
            .expect("the smaller map is offered");
        assert_eq!(offered_names(&offer), ["base"]);
    }

    /// A Deployment that keeps nothing for an Agent leaves it with no assignment on the next press,
    /// and the Agent keeps running what it runs: the view shows what it was given as `kept`, never
    /// as `remove`, and the press makes no offer that would take it away. A Package pinned with no
    /// Configuration sends no config map either, so nothing empty ever replaces a running one.
    /// Verifies: ADR-0033
    #[test]
    fn a_release_holding_nothing_for_an_agent_keeps_what_it_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, id) = mixed_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_holding(&state, uid, &[], &[]);
        state.rollout_deployment("stable").expect("rollout");
        let first = report_holding(&state, uid, &[], &[]).reply;
        let config_hash = first.remote_config.expect("config").config_hash;
        let packages_hash = first
            .packages_available
            .expect("packages")
            .all_packages_hash;

        // `fluent-bit-conf` stays, so the Deployment still holds something and may be pressed; none
        // of it fits this Agent.
        state
            .remove_deployment_configuration("stable", "base")
            .expect("drop base");
        state
            .remove_deployment_package("stable", &id)
            .expect("drop the package");
        let view = &state.snapshot()[0];
        let changes = |view: &AgentView| {
            view.pending_configurations
                .iter()
                .map(|p| (p.name.clone(), p.change.clone()))
                .chain(
                    view.pending_packages
                        .iter()
                        .map(|p| (p.display_name.clone(), p.change.clone())),
                )
                .collect::<Vec<_>>()
        };
        assert_eq!(
            changes(view),
            [
                ("base".to_string(), "kept".to_string()),
                (id.display_name(), "kept".to_string())
            ],
            "nothing is removed from an Agent the press leaves with no assignment"
        );
        assert_eq!(
            state.deployment_reach().expect("reach")["stable"].targeted,
            1
        );

        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);
        let view = &state.snapshot()[0];
        assert!(view.assigned_deployment.is_empty() && view.assigned_configurations.is_empty());
        assert!(changes(view).is_empty(), "nothing waits any more");
        let reply = report_holding(&state, uid, &config_hash, &packages_hash).reply;
        assert!(
            reply.remote_config.is_none(),
            "no config map replaces the one it runs"
        );

        // A Package and no Configuration, to an Agent that runs nothing yet: a package offer, and
        // no config map at all.
        state
            .put_deployment_package("stable", &id, true)
            .expect("the package again");
        let fresh = InstanceUid([2; 16]);
        report_holding(&state, fresh, &[], &[]);
        state.rollout_deployment("stable").expect("rollout");
        let reply = report_holding(&state, fresh, &[], &[]).reply;
        assert!(reply.packages_available.is_some(), "the package is offered");
        assert!(
            reply.remote_config.is_none(),
            "an assignment holding no Configuration offers no empty map"
        );
    }

    /// A Deployment that newly claims an Agent with the content its assignment already pins still
    /// moves that assignment on a press, so the view shows the Deployment waiting — the count
    /// beside the press, the per-Agent press and the view agree.
    /// Verifies: ADR-0033
    #[test]
    fn a_new_claiming_deployment_with_the_same_content_waits_in_the_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = configuring_fleet(dir.path());
        let uid = InstanceUid::default();
        state.process(report(&uid, 1), Transport::Http, None);
        state.rollout_deployment("stable").expect("rollout");
        let deployments = state.deployment_store().expect("deployments");
        deployments
            .put(
                "stable",
                BTreeMap::from([("service.name".to_string(), "nobody".to_string())]),
            )
            .expect("re-aim stable away");
        deployments
            .put(
                "canary",
                BTreeMap::from([("service.name".to_string(), "otelcol".to_string())]),
            )
            .expect("canary");
        state
            .put_deployment_configuration("canary", "fleet")
            .expect("the same Configuration");

        let view = &state.snapshot()[0];
        assert_eq!(view.assigned_deployment, "stable");
        assert!(view.pending_configurations.is_empty() && view.pending_packages.is_empty());
        assert_eq!(view.pending_deployment, "canary");
        assert_eq!(
            state.deployment_reach().expect("reach")["canary"].targeted,
            1
        );

        state.rollout_to_agent(&uid, None).expect("the press");
        let view = &state.snapshot()[0];
        assert_eq!(view.assigned_deployment, "canary");
        assert!(view.pending_deployment.is_empty());
        assert_eq!(
            state.deployment_reach().expect("reach")["canary"].targeted,
            0
        );
    }

    /// Editing a Deployment after a rollout — a Configuration's body, a Configuration added, the
    /// Package swapped — changes no Agent's offer until the next press, and the view shows each
    /// change waiting.
    /// Verifies: ADR-0033
    #[test]
    fn editing_a_deployment_changes_no_offer_until_the_next_press() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, _) = mixed_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_holding(&state, uid, &[], &[]);
        state.rollout_deployment("stable").expect("rollout");
        let first = report_holding(&state, uid, &[], &[]).reply;
        let config_hash = first.remote_config.expect("config").config_hash;
        let packages_hash = first
            .packages_available
            .expect("packages")
            .all_packages_hash;

        state
            .save_configuration("base", body("receivers: {otlp: {}}\n"))
            .expect("edit");
        state
            .save_configuration("extra", body("exporters: {}\n"))
            .expect("extra");
        state
            .put_deployment_configuration("stable", "extra")
            .expect("hold it");
        let v2 = PackageId::new("otelcol", "2.0.0").expect("id");
        let store = state.packages().expect("store");
        store.create(&v2).expect("create");
        store
            .put_entry(&v2, &linux(), b"v2".to_vec())
            .expect("entry");
        put_signed(&state, &v2);

        let reply = report_holding(&state, uid, &config_hash, &packages_hash).reply;
        assert!(reply.remote_config.is_none(), "the edits reach nobody yet");
        assert!(
            reply.packages_available.is_none(),
            "the swap reaches nobody yet"
        );
        let view = &state.snapshot()[0];
        assert_eq!(
            view.pending_configurations
                .iter()
                .map(|p| (p.name.as_str(), p.change.as_str()))
                .collect::<Vec<_>>(),
            [("base", "update"), ("extra", "new")]
        );
        assert_eq!(view.pending_packages[0].change, "update");
        assert_eq!(view.pending_packages[0].version, "2.0.0");

        state
            .rollout_to_agent(&uid, Some("stable"))
            .expect("the press");
        let reply = report_holding(&state, uid, &config_hash, &packages_hash).reply;
        assert!(reply.remote_config.is_some() && reply.packages_available.is_some());
    }

    /// The per-Agent act releases only the Deployment that claims the Agent: one in no Deployment,
    /// one two Deployments claim, and naming another Deployment are each refused, as is a
    /// Deployment that holds nothing — and nothing is assigned.
    /// Verifies: ADR-0033
    #[test]
    fn the_per_agent_act_refuses_without_exactly_one_claiming_deployment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, _) = mixed_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_holding(&state, uid, &[], &[]);
        let deployments = state.deployment_store().expect("deployments");

        deployments
            .put(
                "canary",
                BTreeMap::from([("channel".into(), "canary".into())]),
            )
            .expect("canary");
        assert!(matches!(
            state.rollout_to_agent(&uid, Some("canary")),
            Err(RolloutError::NotApplicable(why)) if why.contains("\"stable\"")
        ));

        deployments
            .put(
                "linux",
                BTreeMap::from([("os.type".into(), "linux".into())]),
            )
            .expect("a second channel claiming it");
        assert!(matches!(
            state.rollout_to_agent(&uid, None),
            Err(RolloutError::NotApplicable(why)) if why.contains("linux") && why.contains("stable")
        ));
        assert!(matches!(
            state.rollout_to_agent(&uid, Some("stable")),
            Err(RolloutError::NotApplicable(_))
        ));
        assert_eq!(
            state
                .rollout_deployment("stable")
                .expect("the press skips it"),
            0
        );

        state.delete_deployment("linux").expect("delete");
        state.delete_deployment("stable").expect("delete");
        assert!(matches!(
            state.rollout_to_agent(&uid, None),
            Err(RolloutError::NotApplicable(why)) if why.contains("no deployment")
        ));

        deployments
            .put(
                "empty",
                BTreeMap::from([("os.type".into(), "linux".into())]),
            )
            .expect("a channel holding nothing");
        assert!(matches!(
            state.rollout_to_agent(&uid, None),
            Err(RolloutError::NotApplicable(why)) if why.contains("holds no package and no configuration")
        ));
        assert!(matches!(
            state.rollout_deployment("empty"),
            Err(RolloutError::NotApplicable(_))
        ));
        assert!(state.snapshot()[0].assigned_deployment.is_empty());
    }

    /// A Package backend on files whose sweeps, once armed, wait at a gate for the blob store's
    /// lock, as a sweep on PostgreSQL waits for another instance's.
    struct HeldSweeps {
        inner: crate::fs::FsPackageBackend,
        armed: Arc<std::sync::atomic::AtomicBool>,
        reached: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        go: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl crate::packages::PackageBackend for HeldSweeps {
        fn load(&self) -> Result<Vec<crate::packages::Package>, String> {
            self.inner.load()
        }
        fn create(&self, id: &PackageId) -> Result<(), String> {
            self.inner.create(id)
        }
        fn put_entry(
            &self,
            id: &PackageId,
            entry: &crate::packages::Entry,
            stored: crate::packages::Stored,
        ) -> Result<(), String> {
            self.inner.put_entry(id, entry, stored)
        }
        fn sign(
            &self,
            id: &PackageId,
            platform: &Platform,
            content_hash: &[u8],
            signature: Option<&[u8]>,
        ) -> Result<crate::packages::Signed, String> {
            self.inner.sign(id, platform, content_hash, signature)
        }
        fn remove_entry(&self, id: &PackageId, platform: &Platform) -> Result<(), String> {
            self.inner.remove_entry(id, platform)
        }
        fn remove(&self, id: &PackageId) -> Result<(), String> {
            self.inner.remove(id)
        }
        fn hold_blobs(&self) -> Result<crate::blobs::BlobHold, String> {
            if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
                // Taken out first: the guard of `reached` is not held through the wait, so a
                // second sweep passes the gate.
                let reached = self.reached.lock().expect("reached").take();
                if let Some(reached) = reached {
                    reached.send(()).expect("reached");
                    self.go.lock().expect("go").recv().expect("go");
                }
            }
            Ok(crate::blobs::BlobHold::none())
        }
    }

    /// The deletion of a Package sweeps its blobs once it has let the fleet lock go: while the
    /// sweep waits for the blob store's lock, every other act of the fleet goes on.
    /// Verifies: ADR-0023
    #[test]
    fn a_package_deletion_sweeps_once_it_lets_the_fleet_lock_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let packages = dir.path().join("packages");
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (reached, reaches) = std::sync::mpsc::channel();
        let (go, goes) = std::sync::mpsc::channel();
        let store = PackageStore::with_backend(
            Box::new(HeldSweeps {
                inner: crate::fs::FsPackageBackend::open(&packages).expect("backend"),
                armed: armed.clone(),
                reached: Mutex::new(Some(reached)),
                go: Mutex::new(goes),
            }),
            Box::new(crate::fs::FsBlobStore::open(&packages).expect("blobs")),
            crate::clock::Elapsed::monotonic(),
        )
        .expect("store");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        store.create(&id).expect("create");
        store
            .put_entry(&id, &linux(), b"v1".to_vec())
            .expect("entry");
        let state = Arc::new(
            AppState::new(dir.path().join("configs"))
                .expect("state")
                .with_packages(Some(
                    PackageOffering::new(&packages, store).expect("offering"),
                )),
        );
        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let deleting = std::thread::spawn({
            let (state, id) = (state.clone(), id.clone());
            move || state.delete_package_set(&id)
        });
        reaches
            .recv()
            .expect("the deletion's sweep waits for the lock");
        let (acted, acts) = std::sync::mpsc::channel();
        std::thread::spawn({
            let state = state.clone();
            move || {
                let _ = acted.send(state.delete_configuration("none").map_err(|_| ()));
            }
        });
        assert_eq!(
            acts.recv_timeout(std::time::Duration::from_secs(30)),
            Ok(Ok(false)),
            "an act under the fleet lock goes on while the sweep waits"
        );
        go.send(()).expect("go on");
        assert_eq!(deleting.join().expect("the deletion"), Ok(true));
    }

    /// An entry's deletion sweeps once it has let go of every lock of the Package store: while
    /// its sweep waits for the blob store's lock, the deletion of another Package, which takes
    /// the fleet lock and then the store's, goes on, and so does every act behind the fleet lock.
    /// Verifies: ADR-0023
    #[test]
    fn an_entry_deletion_sweeps_once_it_lets_the_store_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let packages = dir.path().join("packages");
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (reached, reaches) = std::sync::mpsc::channel();
        let (go, goes) = std::sync::mpsc::channel();
        let store = PackageStore::with_backend(
            Box::new(HeldSweeps {
                inner: crate::fs::FsPackageBackend::open(&packages).expect("backend"),
                armed: armed.clone(),
                reached: Mutex::new(Some(reached)),
                go: Mutex::new(goes),
            }),
            Box::new(crate::fs::FsBlobStore::open(&packages).expect("blobs")),
            crate::clock::Elapsed::monotonic(),
        )
        .expect("store");
        let (first, second) = (
            PackageId::new("otelcol", "1.0.0").expect("id"),
            PackageId::new("otelcol", "2.0.0").expect("id"),
        );
        for (id, bytes) in [(&first, b"v1"), (&second, b"v2")] {
            store.create(id).expect("create");
            store
                .put_entry(id, &linux(), bytes.to_vec())
                .expect("entry");
        }
        let state = Arc::new(
            AppState::new(dir.path().join("configs"))
                .expect("state")
                .with_packages(Some(
                    PackageOffering::new(&packages, store).expect("offering"),
                )),
        );
        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let deleting = std::thread::spawn({
            let (state, id) = (state.clone(), first.clone());
            move || state.delete_package_entry(&id, &linux())
        });
        reaches
            .recv()
            .expect("the entry deletion's sweep waits for the lock");
        let (acted, acts) = std::sync::mpsc::channel();
        std::thread::spawn({
            let (state, id) = (state.clone(), second.clone());
            move || {
                let deleted = state.delete_package_set(&id);
                let other = state.delete_configuration("none").map_err(|_| ());
                let _ = acted.send((deleted, other));
            }
        });
        assert_eq!(
            acts.recv_timeout(std::time::Duration::from_secs(30)),
            Ok((Ok(true), Ok(false))),
            "another Package's deletion and an act under the fleet lock go on while the sweep \
             waits"
        );
        go.send(()).expect("go on");
        assert_eq!(deleting.join().expect("the entry deletion"), Ok(true));
    }

    /// Nothing an assignment pins can be deleted — a Configuration, a Package, a Deployment — and
    /// the refusal says how many Agents pin it. After a press without it the deletion succeeds,
    /// and a deleted Configuration or Package leaves every Deployment that held it.
    /// Verifies: ADR-0033
    #[test]
    fn nothing_an_assignment_pins_can_be_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, id) = mixed_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_holding(&state, uid, &[], &[]);
        let deployments = state.deployment_store().expect("deployments");
        deployments
            .put(
                "canary",
                BTreeMap::from([("channel".into(), "canary".into())]),
            )
            .expect("canary");
        state
            .put_deployment_configuration("canary", "base")
            .expect("base in canary too");
        state
            .put_deployment_package("canary", &id, false)
            .expect("the package in canary too");
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);

        assert!(matches!(
            state.delete_configuration("base"),
            Err(DeleteError::Pinned(why)) if why.contains("1 Agent")
        ));
        let refused = state.delete_package_set(&id).expect_err("pinned");
        assert!(refused.contains("1 Agent"), "{refused}");
        assert!(matches!(
            state.delete_deployment("stable"),
            Err(DeploymentError::Conflict(why)) if why.contains("1 Agent")
        ));

        // Taking both out of the channel is not refused, and the next press releases without them.
        state
            .remove_deployment_configuration("stable", "base")
            .expect("nothing a Deployment holds is frozen");
        state
            .remove_deployment_package("stable", &id)
            .expect("nothing a Deployment holds is frozen");
        state
            .save_configuration("other", body("exporters: {}\n"))
            .expect("other");
        state
            .put_deployment_configuration("stable", "other")
            .expect("hold it");
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);
        assert!(state.delete_configuration("base").expect("deleted"));
        assert!(state.delete_package_set(&id).expect("deleted"));
        let canary = deployments.get("canary").expect("canary");
        assert!(
            canary.configurations.is_empty() && canary.packages.is_empty(),
            "both left every Deployment that held them"
        );
        assert!(state.configurations().get("base").is_none());

        // The Deployment goes once its Agents are released through another one.
        state
            .put_deployment_configuration("canary", "other")
            .expect("hold it");
        state
            .set_labels(&uid, BTreeMap::from([("channel".into(), "canary".into())]))
            .unwrap_or_else(|_| panic!("label"));
        deployments
            .put(
                "stable",
                BTreeMap::from([("channel".into(), "stable".into())]),
            )
            .expect("re-aim");
        assert_eq!(state.rollout_deployment("canary").expect("rollout"), 1);
        assert!(state.delete_deployment("stable").expect("deleted"));
    }

    // ---- Hosts, the effective description and label inheritance (ADR-0030) ----

    /// A fleet whose host register is kept in `dir`, so a reopen restores both.
    fn hosted_fleet(dir: &std::path::Path) -> AppState {
        let revocations = Arc::new(
            Revocations::open(
                Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        AppState::new(dir.join("configs"))
            .expect("state")
            .with_revocations(Some(revocations))
    }

    // ---- An approval records its certificate with its decision (ADR-0023 clause 9) ----

    /// A fleet on files in `dir` that enrols: its register, its window and queue, its audit record
    /// in `dir/audit`, and a client CA kept in `dir` across a reopen.
    fn enrolling_fleet(
        dir: &std::path::Path,
    ) -> (AppState, Arc<Enrolment>, Arc<crate::audit_log::AuditLog>) {
        let (cert_file, key_file) = (dir.join("client-ca.pem"), dir.join("client-ca-key.pem"));
        if !cert_file.exists() {
            let key = rcgen::KeyPair::generate().expect("ca key");
            let mut params = rcgen::CertificateParams::default();
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, "Test Fleet Client CA");
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
            let cert = params.self_signed(&key).expect("ca");
            std::fs::write(&cert_file, cert.pem()).expect("ca file");
            std::fs::write(&key_file, key.serialize_pem()).expect("ca key file");
        }
        let ca = crate::ca::ClientCa::from_config(&crate::config::ClientCaConfig {
            cert_file,
            key_file: Some(key_file),
            key_label: None,
            validity_days: 30,
        })
        .expect("client CA");
        let clock: Arc<dyn Clock> = Arc::new(crate::clock::SystemClock);
        let enrolment = Arc::new(
            Enrolment::open_on(
                clock.clone(),
                Box::new(crate::fs::FsEnrolmentStore::open(dir)),
            )
            .expect("enrolment"),
        );
        let audit = Arc::new(
            crate::audit_log::AuditLog::start(
                Box::new(crate::fs::FsAuditStore::open(dir).expect("audit")),
                crate::audit_log::Limits::default(),
                clock,
            )
            .expect("audit"),
        );
        enrolment.set_audit(audit.clone());
        let state = hosted_fleet(dir)
            .with_client_ca(Some(ca))
            .with_enrolment(Some(enrolment.clone()))
            .with_audit(Some(audit.clone()));
        (state, enrolment, audit)
    }

    /// Asks for a certificate for a key of its own under `key`, while the window is open.
    fn enrol(enrolment: &Enrolment, key: &str) {
        let pair = rcgen::KeyPair::generate().expect("key");
        let csr = rcgen::CertificateParams::new(vec!["edge".to_string()])
            .expect("params")
            .serialize_request(&pair)
            .expect("csr")
            .pem()
            .expect("pem");
        assert_eq!(
            enrolment.submit(
                crate::enrolment::Request {
                    csr_pem: csr,
                    subject: String::new(),
                    key_fingerprint: key.to_string(),
                    instance_uid: vec![9; 16],
                },
                crate::enrolment::tests::requester(),
            ),
            crate::enrolment::Submitted::Waiting
        );
    }

    /// What a certificate, PEM, says about itself, as the Server reads it.
    fn read_certificate(pem: &str) -> Result<Facts, String> {
        let der = opamp::tls::certificates(pem.as_bytes())?;
        crate::ca::facts(der.first().ok_or("no certificate")?.as_ref())
    }

    /// The decision `enrolment.json` in `dir` holds on the request `id`.
    fn stored_decision(dir: &std::path::Path, id: &str) -> Option<crate::enrolment::Decision> {
        use crate::enrolment::EnrolmentStore;
        crate::fs::FsEnrolmentStore::open(dir)
            .read_back(id)
            .expect("read")
            .expect("held")
            .decision
    }

    /// The register's directory, which refuses a new entry while it may not be written.
    fn writable(dir: &std::path::Path, writable: bool) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("certificates"),
            std::fs::Permissions::from_mode(if writable { 0o700 } else { 0o500 }),
        )
        .expect("chmod");
    }

    /// On files the decision is written before the register entry: a decision that cannot be
    /// written registers nothing, and an entry that cannot be written takes the decision back, so
    /// the request waits again with nothing registered, each signature is recorded as rolled back,
    /// and approved again it is signed anew (ADR-0023 clause 9).
    /// Verifies: ADR-0023
    #[cfg(unix)]
    #[test]
    fn an_approval_on_files_whose_register_entry_fails_takes_its_decision_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, enrolment, audit) = enrolling_fleet(dir.path());
        enrolment.open(600).expect("window");
        enrol(&enrolment, "k1");
        let queue = dir.path().join("enrolment.json");
        crate::fs::testing::refuse(&queue);
        let unwritten = state.approve_enrolment("k1");
        crate::fs::testing::mend(&queue);
        assert!(
            matches!(unwritten, Err(DecisionError::Store(_))),
            "{unwritten:?}"
        );
        assert!(state
            .revocations
            .as_ref()
            .expect("register")
            .issued()
            .is_empty());
        assert_eq!(
            audited_events(&audit, dir.path(), "issuance.undecided").len(),
            1,
            "the signature whose decision was not written"
        );
        writable(dir.path(), false);
        let refused = state.approve_enrolment("k1");
        writable(dir.path(), true);
        assert!(
            matches!(refused, Err(DecisionError::Sign(_))),
            "{refused:?}"
        );
        assert_eq!(
            stored_decision(dir.path(), "k1"),
            None,
            "the decision is taken back"
        );
        assert_eq!(enrolment.pending().len(), 1, "the request waits again");
        let revocations = state.revocations.as_ref().expect("register");
        assert!(revocations.issued().is_empty());
        assert!(revocations.hosts().is_empty(), "no Host minted");

        let serial = state.approve_enrolment("k1").expect("approved again");
        let issued = revocations.issued();
        assert_eq!(issued.len(), 1);
        assert_eq!(issued[0].facts.id.serial, serial);
        assert!(matches!(
            stored_decision(dir.path(), "k1"),
            Some(crate::enrolment::Decision::Approved(_))
        ));
        let undecided = audited_events(&audit, dir.path(), "issuance.undecided");
        assert_eq!(undecided.len(), 2, "{undecided:#?}");
        assert!(undecided
            .iter()
            .all(|entry| entry["outcome"] == "rolled_back"));
    }

    /// The audit lines in `dir/audit` whose event is `event`, once the writer has written them.
    fn audited_events(
        audit: &crate::audit_log::AuditLog,
        dir: &std::path::Path,
        event: &str,
    ) -> Vec<serde_json::Value> {
        assert!(
            audit.drain(Duration::from_secs(10)),
            "the audit record was not written"
        );
        let files = crate::fs::audit_files(&dir.join("audit")).expect("audit files");
        crate::fs::audit_lines(&files)
            .expect("audit lines")
            .into_iter()
            .map(|(_, line)| serde_json::from_str::<serde_json::Value>(&line).expect("json"))
            .filter(|line| line["event"] == event)
            .collect()
    }

    /// The audit record of an approval whose decision was not written still verifies, and names
    /// that signature as undecided by its serial and request, beside the signature of the approval
    /// that was (ADR-0023 clause 9).
    /// Verifies: ADR-0023
    #[cfg(unix)]
    #[test]
    fn the_audit_record_of_an_approval_that_did_not_commit_verifies_and_names_it_undecided() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, enrolment, audit) = enrolling_fleet(dir.path());
        enrolment.open(600).expect("window");
        enrol(&enrolment, "k1");
        writable(dir.path(), false);
        state.approve_enrolment("k1").expect_err("not registered");
        writable(dir.path(), true);
        let serial = state.approve_enrolment("k1").expect("approved again");

        let signed = audited_events(&audit, dir.path(), "issuance.signed");
        assert_eq!(signed.len(), 2, "{signed:#?}");
        assert!(
            signed.iter().all(|entry| entry["request"] == "k1"),
            "{signed:#?}"
        );
        let undecided = audited_events(&audit, dir.path(), "issuance.undecided");
        assert_eq!(undecided.len(), 1, "{undecided:#?}");
        assert_eq!(undecided[0]["outcome"], "rolled_back");
        assert_eq!(undecided[0]["request"], "k1");
        assert_eq!(undecided[0]["serial"], signed[0]["serial"]);
        assert_ne!(undecided[0]["serial"], serial.as_str());
        assert_eq!(signed[1]["serial"], serial.as_str());
        let (_, verified) = crate::fs::verify_audit(&dir.path().join("audit")).expect("read");
        verified.expect("the chain holds");
    }

    /// A decision found at startup whose certificate the register does not hold, as after a crash
    /// between the two writes, is taken back: the request waits again and its signature is
    /// recorded as undecided, while an approval whose certificate is registered stays decided
    /// (ADR-0023 clause 9).
    /// Verifies: ADR-0023
    #[test]
    fn a_decision_found_at_startup_without_its_register_entry_waits_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (lost, kept) = {
            let (state, enrolment, audit) = enrolling_fleet(dir.path());
            enrolment.open(600).expect("window");
            enrol(&enrolment, "lost");
            enrol(&enrolment, "kept");
            let lost = state.approve_enrolment("lost").expect("approve");
            let kept = state.approve_enrolment("kept").expect("approve");
            assert!(audit.close(Duration::from_secs(10)));
            (lost, kept)
        };
        // What a crash after the decision and before the register entry leaves.
        let entry = std::fs::read_dir(dir.path().join("certificates"))
            .expect("register")
            .map(|file| file.expect("file").path())
            .find(|path| path.to_string_lossy().ends_with(&format!("-{lost}.json")))
            .expect("the lost approval's entry");
        std::fs::remove_file(entry).expect("remove");

        let (state, enrolment, audit) = enrolling_fleet(dir.path());
        assert_eq!(
            state.take_back_unregistered_approvals(&read_certificate),
            Ok(1)
        );
        let waiting: Vec<String> = enrolment.pending().into_iter().map(|p| p.id).collect();
        assert_eq!(waiting, ["lost"]);
        assert_eq!(stored_decision(dir.path(), "lost"), None);
        assert!(
            stored_decision(dir.path(), "kept").is_some(),
            "a registered approval stays"
        );
        let undecided = audited_events(&audit, dir.path(), "issuance.undecided");
        assert_eq!(undecided.len(), 1, "{undecided:#?}");
        assert_eq!(undecided[0]["serial"], lost.as_str());
        assert_eq!(undecided[0]["request"], "lost");
        assert_ne!(lost, kept);
        assert_eq!(
            state.take_back_unregistered_approvals(&read_certificate),
            Ok(0),
            "once"
        );
    }

    fn organization(name: &str) -> crate::revocation::SubjectValues {
        crate::revocation::SubjectValues {
            organization: name.to_string(),
            ..Default::default()
        }
    }

    /// A full report from `uid` describing itself with `pairs`, over a certificate naming `host`
    /// whose subject carries `subject`.
    fn report_over(
        state: &AppState,
        host: &str,
        subject: crate::revocation::SubjectValues,
        uid: InstanceUid,
        pairs: &[(&str, &str)],
    ) -> Processed {
        let mut message = report(&uid, 1);
        message
            .agent_description
            .as_mut()
            .expect("described")
            .non_identifying_attributes = pairs
            .iter()
            .map(|(key, value)| opamp::attributes::string_attr(key, value))
            .collect();
        state.process_presented(
            message,
            Transport::Http,
            None,
            Some(&Presented {
                id: CertId::new(b"CA", host),
                host: Some(host.to_string()),
                subject_values: subject,
                not_after_ms: u64::MAX,
            }),
        )
    }

    /// Whether a Selector of `pairs` matches this Agent — the one test every Selector is decided by.
    fn selects(state: &AppState, uid: &InstanceUid, pairs: &[(&str, &str)]) -> bool {
        let selector: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let fleet = state.fleet.lock().expect("fleet lock");
        let record = fleet.get(uid).expect("known");
        deployments::matches(&selector, record.effective_description().as_deref())
    }

    fn view(state: &AppState, uid: &InstanceUid) -> AgentView {
        state
            .snapshot()
            .into_iter()
            .find(|view| view.instance_uid == uid.to_string())
            .expect("in the view")
    }

    fn label(state: &AppState, uid: &InstanceUid, pairs: &[(&str, &str)]) {
        let set = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        if state.set_labels(uid, set).is_err() {
            panic!("labels refused");
        }
    }

    /// An Agent on a direct connection carries its Host's subject values as `fleet.host.*`; an
    /// Agent that reports `fleet.host.organization` itself is not selected by it, and the attribute
    /// is listed as ignored. Without a host register an Agent names no Host.
    /// Verifies: ADR-0030
    #[test]
    fn a_direct_agent_matches_its_hosts_values_and_none_it_reports_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = hosted_fleet(dir.path());
        let honest = InstanceUid::default();
        report_over(&state, "h1", organization("Kunde GmbH"), honest, &[]);
        assert!(selects(
            &state,
            &honest,
            &[("fleet.host.organization", "Kunde GmbH")]
        ));
        assert_eq!(view(&state, &honest).host, "h1");
        assert_eq!(
            view(&state, &honest).host_attributes["fleet.host.organization"],
            "Kunde GmbH"
        );

        // On a Host of its own whose certificate carries no organization.
        let claimant = InstanceUid::default();
        report_over(
            &state,
            "h2",
            crate::revocation::SubjectValues::default(),
            claimant,
            &[("fleet.host.organization", "Kunde GmbH")],
        );
        assert!(!selects(
            &state,
            &claimant,
            &[("fleet.host.organization", "Kunde GmbH")]
        ));
        assert_eq!(
            view(&state, &claimant).ignored_attributes,
            ["fleet.host.organization"]
        );

        // On the Host whose values it would contradict, the Host's value is what matches.
        let liar = InstanceUid::default();
        report_over(
            &state,
            "h1",
            organization("Kunde GmbH"),
            liar,
            &[("fleet.host.organization", "Mitbewerber AG")],
        );
        assert!(selects(
            &state,
            &liar,
            &[("fleet.host.organization", "Kunde GmbH")]
        ));
        assert!(!selects(
            &state,
            &liar,
            &[("fleet.host.organization", "Mitbewerber AG")]
        ));

        // A Server without a host register has no Hosts, and its records name none.
        let bare = AppState::new(dir.path().join("bare")).expect("state");
        let uid = InstanceUid::default();
        report_over(&bare, "h1", organization("Kunde GmbH"), uid, &[]);
        assert_eq!(view(&bare, &uid).host, "");
        assert!(!selects(
            &bare,
            &uid,
            &[("fleet.host.organization", "Kunde GmbH")]
        ));
    }

    /// A Supervisor Agent matches a Selector on a label set only on its Client Agent, its own label
    /// of the same key wins, the Agents tab is told which labels are inherited, and all of it
    /// survives a restart.
    /// Verifies: ADR-0030
    #[test]
    fn a_supervisor_agent_inherits_its_client_agents_labels_below_its_own() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client = InstanceUid::default();
        let supervisor = InstanceUid::default();
        {
            let state = hosted_fleet(dir.path());
            report_over(
                &state,
                "h1",
                organization("Kunde GmbH"),
                client,
                &[("supervisor.kind.collector", "true")],
            );
            report_over(&state, "h1", organization("Kunde GmbH"), supervisor, &[]);
            label(&state, &client, &[("ring", "early"), ("site", "wien")]);
            label(&state, &supervisor, &[("ring", "late")]);
            assert!(selects(&state, &supervisor, &[("site", "wien")]));
            assert!(selects(&state, &supervisor, &[("ring", "late")]));
            assert!(!selects(&state, &supervisor, &[("ring", "early")]));
            assert!(selects(&state, &client, &[("ring", "early")]));
        }
        let state = hosted_fleet(dir.path());
        assert!(
            selects(&state, &supervisor, &[("site", "wien"), ("ring", "late")]),
            "restored with its own labels and its Client Agent's"
        );
        let seen = view(&state, &supervisor);
        assert_eq!(seen.client_agent, client.to_string());
        assert_eq!(
            seen.inherited_labels,
            BTreeMap::from([
                ("ring".to_string(), "early".to_string()),
                ("site".to_string(), "wien".to_string())
            ])
        );
        assert!(view(&state, &client).inherited_labels.is_empty());

        // The Client Agent's labels cleared, nothing is inherited any more.
        label(&state, &client, &[]);
        assert!(!selects(&state, &supervisor, &[("site", "wien")]));
    }

    /// What another Server wrote to a held record is taken up from the store: a Client Agent's
    /// labels place its Client's other Agent again, so it inherits them, a queued restart is held,
    /// and a record the store no longer holds is dropped.
    /// Verifies: ADR-0025
    #[test]
    fn what_another_server_wrote_is_taken_up_and_places_the_group_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (client, supervisor) = (InstanceUid::default(), InstanceUid::default());
        let state = hosted_fleet(dir.path());
        report_over(
            &state,
            "h1",
            organization("Kunde GmbH"),
            client,
            &[("supervisor.kind.collector", "true")],
        );
        report_over(&state, "h1", organization("Kunde GmbH"), supervisor, &[]);
        let other = crate::fs::FsAgentStore::open(&dir.path().join("configs")).expect("open");
        let labels = BTreeMap::from([("site".to_string(), "wien".to_string())]);
        assert!(other.set_labels(&client, &labels).expect("labels"));
        assert_eq!(
            other.set_restart_pending(&supervisor, true),
            Ok(Some(false))
        );
        assert!(view(&state, &supervisor).inherited_labels.is_empty());

        state
            .take_up_agents(Some(&HashSet::from([client, supervisor])))
            .expect("take up");
        assert_eq!(view(&state, &client).labels, labels);
        assert_eq!(view(&state, &supervisor).inherited_labels, labels);
        assert!(state.fleet.lock().expect("fleet")[&supervisor].restart_pending);

        other.remove(&client).expect("forget");
        state.take_up_agents(None).expect("take up");
        assert!(!state.fleet.lock().expect("fleet").contains_key(&client));
        assert!(
            view(&state, &supervisor).inherited_labels.is_empty(),
            "a forgotten Client Agent's labels are inherited no more"
        );
    }

    /// Behind a Gateway an Agent carries no `fleet.host.*`; the Agents sharing a `host.id` inherit
    /// their Client Agent's labels, and two Client Agents with one `host.id` leave every Agent of it
    /// inheriting nothing and marked ambiguous.
    /// Verifies: ADR-0030
    #[test]
    fn behind_a_gateway_agents_sharing_a_host_id_inherit_unless_ambiguous() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = hosted_fleet(dir.path());
        let gateway = InstanceUid::default();
        report_over(
            &state,
            "gw",
            organization("Gateway GmbH"),
            gateway,
            &[("supervisor.kind.collector", "true"), ("host.id", "gw-1")],
        );
        assert!(state.set_gateway("gw", true).expect("marked"));
        assert!(
            !selects(
                &state,
                &gateway,
                &[("fleet.host.organization", "Gateway GmbH")]
            ),
            "nothing arrives over a Gateway directly"
        );

        let client = InstanceUid::default();
        let supervisor = InstanceUid::default();
        let elsewhere = InstanceUid::default();
        let behind = |uid, pairs: &[(&str, &str)]| {
            report_over(&state, "gw", organization("Gateway GmbH"), uid, pairs);
        };
        behind(
            client,
            &[("supervisor.kind.collector", "true"), ("host.id", "m-1")],
        );
        behind(supervisor, &[("host.id", "m-1")]);
        behind(elsewhere, &[("host.id", "m-2")]);
        label(&state, &client, &[("site", "linz")]);
        assert!(selects(&state, &supervisor, &[("site", "linz")]));
        assert!(!selects(&state, &elsewhere, &[("site", "linz")]));
        let seen = view(&state, &supervisor);
        assert_eq!(seen.host, "gw");
        assert!(seen.host_attributes.is_empty());
        assert!(!seen.ambiguous);

        // A second Client on the same machine: nobody of it can be told apart.
        let second = InstanceUid::default();
        behind(
            second,
            &[("supervisor.kind.collector", "true"), ("host.id", "m-1")],
        );
        assert!(!selects(&state, &supervisor, &[("site", "linz")]));
        for uid in [client, supervisor, second] {
            assert!(view(&state, &uid).ambiguous, "{uid}");
        }
        assert!(!view(&state, &elsewhere).ambiguous);
    }

    /// A Client Agent re-keyed by a compressed message — no description to place it by — stays
    /// its group's Client Agent: its Supervisor Agents keep inheriting its labels, and a label set
    /// on its new identity reaches them.
    /// Verifies: ADR-0030
    #[test]
    fn a_client_agent_rekeyed_without_a_description_keeps_its_group_inheriting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = hosted_fleet(dir.path());
        let client = InstanceUid::default();
        let supervisor = InstanceUid::default();
        report_over(
            &state,
            "h1",
            organization("Kunde GmbH"),
            client,
            &[("supervisor.kind.collector", "true")],
        );
        report_over(&state, "h1", organization("Kunde GmbH"), supervisor, &[]);
        label(&state, &client, &[("site", "wien")]);
        assert!(selects(&state, &supervisor, &[("site", "wien")]));

        let rekey = AgentToServer {
            instance_uid: client.as_bytes().to_vec(),
            sequence_num: 2,
            capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
            flags: AgentToServerFlags::RequestInstanceUid as u64,
            ..Default::default()
        };
        let rekeyed = state
            .process_presented(
                rekey,
                Transport::Http,
                None,
                Some(&Presented {
                    id: CertId::new(b"CA", "h1"),
                    host: Some("h1".to_string()),
                    subject_values: organization("Kunde GmbH"),
                    not_after_ms: u64::MAX,
                }),
            )
            .uid
            .expect("the new identity");
        assert_ne!(rekeyed, client);
        assert!(
            selects(&state, &supervisor, &[("site", "wien")]),
            "the Supervisor Agent still inherits"
        );
        assert_eq!(view(&state, &supervisor).client_agent, rekeyed.to_string());
        assert_eq!(view(&state, &rekeyed).client_agent, rekeyed.to_string());

        label(&state, &rekeyed, &[("site", "linz")]);
        assert!(selects(&state, &supervisor, &[("site", "linz")]));
    }

    /// Labels are part of the Agent's record: a re-keyed Agent keeps them, on disk too, and
    /// forgetting it removes them — it comes back unlabelled.
    /// Verifies: ADR-0030
    #[test]
    fn forgetting_an_agent_removes_its_labels_and_a_rekey_keeps_them() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let state = AppState::new(dir.clone())
            .expect("state")
            .with_stale_after(Duration::from_secs(90));
        let uid = InstanceUid::default();
        state.process(report(&uid, 1), Transport::WebSocket, Some(1));
        label(&state, &uid, &[("rollout", "canary")]);

        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let new_uid = state
            .process(rekey, Transport::WebSocket, Some(1))
            .uid
            .expect("the new identity");
        assert_eq!(view(&state, &new_uid).labels["rollout"], "canary");
        let stored = std::fs::read_to_string(dir.join("agents").join(format!("{new_uid}.json")))
            .expect("stored");
        assert!(stored.contains("canary"), "{stored}");

        let mut goodbye = report(&new_uid, 3);
        goodbye.agent_disconnect = Some(opamp::proto::AgentDisconnect {});
        state.process(goodbye, Transport::WebSocket, Some(1));
        assert!(state.forget_agent(&new_uid).is_ok());
        state.process(report(&new_uid, 4), Transport::WebSocket, Some(2));
        assert!(
            view(&state, &new_uid).labels.is_empty(),
            "it comes back unlabelled"
        );
        assert!(!dir.join("labels").exists(), "no label store is written");
    }

    /// A store that counts what reaches it: the record writes, and each presence batch as it is
    /// written.
    struct Counted {
        inner: crate::fs::FsAgentStore,
        puts: Arc<AtomicU64>,
        batches: Arc<Mutex<Vec<Batch>>>,
    }

    /// One presence batch as it reached the store.
    type Batch = Vec<(InstanceUid, Presence, Option<u64>)>;

    impl AgentStore for Counted {
        fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
            self.inner.load()
        }
        fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String> {
            self.puts.fetch_add(1, Ordering::SeqCst);
            self.inner.put(uid, record)
        }
        fn set_labels(
            &self,
            uid: &InstanceUid,
            labels: &BTreeMap<String, String>,
        ) -> Result<bool, String> {
            self.inner.set_labels(uid, labels)
        }
        fn set_restart_pending(
            &self,
            uid: &InstanceUid,
            pending: bool,
        ) -> Result<Option<bool>, String> {
            self.inner.set_restart_pending(uid, pending)
        }
        fn remove(&self, uid: &InstanceUid) -> Result<(), String> {
            self.inner.remove(uid)
        }
        fn write_presence(
            &self,
            _holder: &str,
            presence: &[(InstanceUid, Presence, Option<u64>)],
        ) -> Result<HashMap<InstanceUid, u64>, String> {
            self.batches
                .lock()
                .expect("batches lock")
                .push(presence.to_vec());
            Ok(HashMap::new())
        }
        fn release(
            &self,
            revisions: &[crate::configs::Pinned],
            agents: &[(InstanceUid, PersistedAgent)],
        ) -> Result<Vec<InstanceUid>, String> {
            self.inner.release(revisions, agents)
        }
        fn pinning(&self, pin: Pin<'_>) -> usize {
            self.inner.pinning(pin)
        }
        fn referenced_revisions(&self, name: &str) -> BTreeSet<String> {
            self.inner.referenced_revisions(name)
        }
    }

    /// A clock the test moves.
    struct Manual(Arc<AtomicU64>);

    impl Clock for Manual {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// An instance carrying 1 000 Agents at a 30-second heartbeat writes their presence in
    /// batches: no heartbeat writes a record or a presence, each batch writes every presence that
    /// moved since the last one, in the order of the `instance_uid`, and nothing when none moved.
    /// A disconnect is written in the next batch, and under a new holder every presence again.
    /// Verifies: ADR-0030
    #[test]
    fn presence_is_written_in_batches_and_never_once_per_heartbeat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (puts, batches) = (
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let now = Arc::new(AtomicU64::new(1_000_000));
        let state = AppState::with_stores(
            Box::new(Counted {
                inner: crate::fs::FsAgentStore::open(dir.path()).expect("agents"),
                puts: puts.clone(),
                batches: batches.clone(),
            }),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(Manual(now.clone())),
        )
        .expect("state")
        .with_presence_interval(Duration::from_secs(30));
        let uids: Vec<InstanceUid> = (0..1000).map(|_| InstanceUid::default()).collect();
        for (conn, uid) in (1..).zip(&uids) {
            state.process(report(uid, 1), Transport::WebSocket, Some(conn));
        }
        let first_reports = puts.load(Ordering::SeqCst);
        assert!(
            batches.lock().expect("batches").is_empty(),
            "no report wrote a presence"
        );
        assert_eq!(state.write_presence("a"), Ok(1000));
        {
            let batches = batches.lock().expect("batches");
            let mut ordered = uids.clone();
            ordered.sort_by_key(|uid| uid.0);
            let batch: Vec<InstanceUid> = batches[0].iter().map(|(uid, ..)| *uid).collect();
            assert_eq!(batch, ordered, "in the order of the instance_uid");
            assert!(batches[0].iter().all(|(_, presence, _)| presence.connected));
        }

        for beat in 2..=4u64 {
            now.fetch_add(30_000, Ordering::SeqCst);
            for (conn, uid) in (1..).zip(&uids) {
                let heartbeat = AgentToServer {
                    instance_uid: uid.as_bytes().to_vec(),
                    sequence_num: beat,
                    ..Default::default()
                };
                state.process(heartbeat, Transport::WebSocket, Some(conn));
            }
            assert_eq!(
                puts.load(Ordering::SeqCst),
                first_reports,
                "no heartbeat writes a record"
            );
            assert_eq!(
                batches.lock().expect("batches").len() as u64,
                beat - 1,
                "no heartbeat writes a presence"
            );
            assert_eq!(
                state.write_presence("a"),
                Ok(1000),
                "one batch for the round"
            );
            let batches = batches.lock().expect("batches");
            let last = batches.last().expect("a batch");
            assert!(last
                .iter()
                .all(|(_, presence, _)| presence.last_seen_ms == now.load(Ordering::SeqCst)));
        }
        assert_eq!(state.write_presence("a"), Ok(0), "nothing moved");

        state.mark_disconnected(&uids[..1], 1);
        assert_eq!(state.write_presence("a"), Ok(1), "the disconnect");
        assert!(
            !batches.lock().expect("batches").last().expect("a batch")[0]
                .1
                .connected
        );
        assert_eq!(
            state.write_presence("b"),
            Ok(999),
            "a new holder writes every connected one"
        );
    }

    /// What is kept of an Agent's presence: a take's answer replaces the count, also a lower one,
    /// as a record forgotten and created again counts from the start, and moves the generation; a
    /// new holder writes every presence again and keeps the counts; a lost answer leaves no count
    /// and makes the Agent unsure until a take answers; a re-key moves all of it to the new
    /// identity.
    /// Verifies: ADR-0030
    #[test]
    fn what_is_kept_of_an_agents_presence_follows_its_takes() {
        let (uid, new) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        let presence = Presence {
            connected: true,
            last_seen_ms: 1,
        };
        let mut written = PresenceWritten::default();
        written.under("a");
        written.taken(uid, presence, 3);
        let first = written.generation(&uid);
        written.taken(uid, presence, 1);
        assert_eq!(written.takes.get(&uid), Some(&1), "a take's answer");
        assert_ne!(
            written.generation(&uid),
            first,
            "a take moves the generation"
        );
        written.under("b");
        assert!(
            written.agents.is_empty(),
            "a new holder writes every presence again"
        );
        assert_eq!(written.takes.get(&uid), Some(&1), "and keeps the counts");
        written.lost(uid);
        assert!(written.unsure.contains(&uid), "a lost answer");
        assert_eq!(written.takes.get(&uid), None, "leaves no count");
        written.agents.insert(uid, presence);
        written.rekey(&uid, new);
        assert_eq!(
            (
                written.agents.get(&new),
                written.takes.get(&new),
                written.unsure.contains(&new)
            ),
            (Some(&presence), None, true),
            "a re-key moves it"
        );
        written.taken(new, presence, 1);
        written.rekey(&new, uid);
        assert_eq!(written.takes.get(&uid), Some(&1), "and its count");
        assert!(!written.takes.contains_key(&new) && !written.unsure.contains(&uid));
        written.lost(uid);
        written.taken(uid, presence, 2);
        assert!(!written.unsure.contains(&uid), "a take answers");
    }

    /// The first presence batch of an instance writes the Agents it carries, not the fleet it
    /// restored: a restored Agent is written once it is connected here.
    /// Verifies: ADR-0030
    #[test]
    fn the_first_presence_batch_writes_only_the_agents_carried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let restored: Vec<InstanceUid> = (0..100).map(|_| InstanceUid::default()).collect();
        {
            let store = crate::fs::FsAgentStore::open(dir.path()).expect("agents");
            for uid in &restored {
                store
                    .put(uid, &crate::agent_store::tests::record())
                    .expect("put");
            }
        }
        let batches = Arc::new(Mutex::new(Vec::new()));
        let state = AppState::with_stores(
            Box::new(Counted {
                inner: crate::fs::FsAgentStore::open(dir.path()).expect("agents"),
                puts: Arc::new(AtomicU64::new(0)),
                batches: batches.clone(),
            }),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(crate::clock::SystemClock),
        )
        .expect("state");
        assert_eq!(state.write_presence("a"), Ok(0), "none is carried");
        state.process(report(&restored[0], 8), Transport::WebSocket, Some(1));
        assert_eq!(state.write_presence("a"), Ok(1), "the one connected here");
        assert_eq!(batches.lock().expect("batches")[0][0].0, restored[0]);
    }

    /// A store passing every call to `inner`, whose next reads of one record can be made to fail,
    /// and which can run an act once right after a read of one record: what lets a test place
    /// another instance's write between the read and what is done with it. It counts the reads of
    /// one record and the reads of several, and its re-keys can be made to fail.
    struct Hooked {
        inner: Box<dyn AgentStore>,
        failing_gets: Arc<AtomicU64>,
        after_get: Arc<Mutex<Option<Act>>>,
        gets: Arc<AtomicU64>,
        batched_gets: Arc<AtomicU64>,
        loads: Arc<AtomicU64>,
        whole_queries: Arc<AtomicU64>,
        failing_rekeys: Arc<std::sync::atomic::AtomicBool>,
        /// Re-keys written whose answer is then lost, as a connection dropped after the commit.
        losing_rekeys: Arc<std::sync::atomic::AtomicBool>,
        /// Read backs of a re-key that fail, as a database that does not answer.
        failing_moved: Arc<std::sync::atomic::AtomicBool>,
        /// The read backs of a re-key asked for.
        moved_reads: Arc<AtomicU64>,
        after_holders: Arc<Mutex<Option<Act>>>,
        /// Takes written whose answer is then lost, as a connection dropped after the commit.
        losing_takes: Arc<std::sync::atomic::AtomicBool>,
        /// Takes refused before anything is written, as a database that does not answer.
        failing_takes: Arc<std::sync::atomic::AtomicBool>,
        /// What runs once after a presence batch committed, before its answers are applied.
        after_presence: Arc<Mutex<Option<Act>>>,
        /// The reads of holders and counts, and what runs once before and after the next of them.
        takes_reads: Arc<AtomicU64>,
        before_takes: Arc<Mutex<Option<Act>>>,
        after_takes: Arc<Mutex<Option<Act>>>,
        /// Restarts set queued that fail, as a database that does not answer, and how many were
        /// asked for.
        failing_restarts: Arc<std::sync::atomic::AtomicBool>,
        restarts_set: Arc<AtomicU64>,
        /// What runs before every call to the store, given the call and the Agent it names.
        probe: Arc<Mutex<Option<Probe>>>,
        /// What runs once after a take committed, before it is answered.
        after_take: Arc<Mutex<Option<Act>>>,
        /// What runs once after a re-key committed, before it is answered.
        after_rekey: Arc<Mutex<Option<Act>>>,
    }

    /// What [`Hooked`] runs once after a read.
    type Act = Box<dyn FnOnce() + Send>;

    /// What [`Hooked`] runs before every call to the store.
    type Probe = Arc<dyn Fn(&'static str, Option<&InstanceUid>) + Send + Sync>;

    /// Runs the probe set in `probe`, if one is, for `call` naming `uid`.
    fn probed(probe: &Mutex<Option<Probe>>, call: &'static str, uid: Option<&InstanceUid>) {
        let probe = probe.lock().expect("hook lock").clone();
        if let Some(probe) = probe {
            probe(call, uid);
        }
    }

    impl Hooked {
        fn new(inner: Box<dyn AgentStore>) -> Self {
            Hooked {
                inner,
                failing_gets: Arc::new(AtomicU64::new(0)),
                after_get: Arc::new(Mutex::new(None)),
                gets: Arc::new(AtomicU64::new(0)),
                batched_gets: Arc::new(AtomicU64::new(0)),
                loads: Arc::new(AtomicU64::new(0)),
                whole_queries: Arc::new(AtomicU64::new(0)),
                failing_rekeys: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                losing_rekeys: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                failing_moved: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                moved_reads: Arc::new(AtomicU64::new(0)),
                after_holders: Arc::new(Mutex::new(None)),
                losing_takes: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                failing_takes: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                after_presence: Arc::new(Mutex::new(None)),
                takes_reads: Arc::new(AtomicU64::new(0)),
                before_takes: Arc::new(Mutex::new(None)),
                after_takes: Arc::new(Mutex::new(None)),
                failing_restarts: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                restarts_set: Arc::new(AtomicU64::new(0)),
                probe: Arc::new(Mutex::new(None)),
                after_take: Arc::new(Mutex::new(None)),
                after_rekey: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl AgentStore for Hooked {
        fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.inner.load()
        }
        fn get(&self, uid: &InstanceUid) -> Result<Option<PersistedAgent>, String> {
            probed(&self.probe, "get", Some(uid));
            let failing = self
                .failing_gets
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if failing {
                return Err("the read is held back".to_string());
            }
            self.gets.fetch_add(1, Ordering::SeqCst);
            let got = self.inner.get(uid);
            let after = self.after_get.lock().expect("hook lock").take();
            if let Some(after) = after {
                after();
            }
            got
        }
        fn get_many(
            &self,
            uids: &[InstanceUid],
        ) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
            probed(&self.probe, "get_many", None);
            self.batched_gets.fetch_add(1, Ordering::SeqCst);
            self.inner.get_many(uids)
        }
        fn query(
            &self,
            only: Option<&[InstanceUid]>,
        ) -> Result<Vec<(InstanceUid, crate::agent_store::StoredAgent)>, String> {
            if only.is_none() {
                self.whole_queries.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.query(only)
        }
        fn query_page(
            &self,
            query: &crate::agent_store::PageQuery<'_>,
        ) -> Result<crate::agent_store::AgentPage, String> {
            probed(&self.probe, "query_page", None);
            self.inner.query_page(query)
        }
        fn reported_values(
            &self,
            limit: usize,
        ) -> Result<crate::agent_store::ReportedValues, String> {
            self.inner.reported_values(limit)
        }
        fn is_shared(&self) -> bool {
            self.inner.is_shared()
        }
        fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String> {
            self.inner.put(uid, record)
        }
        fn write_report(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<bool, String> {
            probed(&self.probe, "write_report", Some(uid));
            self.inner.write_report(uid, record)
        }
        fn write_report_unless_held_elsewhere(
            &self,
            uid: &InstanceUid,
            record: &PersistedAgent,
            holder: Option<&str>,
            take: Option<u64>,
        ) -> Result<bool, String> {
            self.inner
                .write_report_unless_held_elsewhere(uid, record, holder, take)
        }
        fn create_within(
            &self,
            uid: &InstanceUid,
            record: &PersistedAgent,
            max_records: usize,
        ) -> Result<Creation, String> {
            probed(&self.probe, "create_within", Some(uid));
            self.inner.create_within(uid, record, max_records)
        }
        fn set_labels(
            &self,
            uid: &InstanceUid,
            labels: &BTreeMap<String, String>,
        ) -> Result<bool, String> {
            self.inner.set_labels(uid, labels)
        }
        fn set_restart_pending(
            &self,
            uid: &InstanceUid,
            pending: bool,
        ) -> Result<Option<bool>, String> {
            probed(&self.probe, "set_restart_pending", Some(uid));
            if pending {
                self.restarts_set.fetch_add(1, Ordering::SeqCst);
                if self.failing_restarts.load(Ordering::SeqCst) {
                    return Err("the restart is held back".to_string());
                }
            }
            self.inner.set_restart_pending(uid, pending)
        }
        fn remove(&self, uid: &InstanceUid) -> Result<(), String> {
            self.inner.remove(uid)
        }
        fn write_presence(
            &self,
            holder: &str,
            presence: &[(InstanceUid, Presence, Option<u64>)],
        ) -> Result<HashMap<InstanceUid, u64>, String> {
            let written = self.inner.write_presence(holder, presence);
            let after = self.after_presence.lock().expect("hook lock").take();
            if let Some(after) = after {
                after();
            }
            written
        }
        fn takes(
            &self,
            uids: &[InstanceUid],
        ) -> Result<HashMap<InstanceUid, (Option<String>, u64)>, String> {
            self.takes_reads.fetch_add(1, Ordering::SeqCst);
            let before = self.before_takes.lock().expect("hook lock").take();
            if let Some(before) = before {
                before();
            }
            let takes = self.inner.takes(uids);
            let after = self.after_takes.lock().expect("hook lock").take();
            if let Some(after) = after {
                after();
            }
            takes
        }
        fn remove_if_silent(
            &self,
            uid: &InstanceUid,
            silent_before_ms: u64,
        ) -> Result<Option<bool>, String> {
            self.inner.remove_if_silent(uid, silent_before_ms)
        }
        fn take(
            &self,
            uid: &InstanceUid,
            holder: &str,
            last_seen_ms: u64,
        ) -> Result<Option<crate::agent_store::Taken>, String> {
            probed(&self.probe, "take", Some(uid));
            if self.failing_takes.load(Ordering::SeqCst) {
                return Err("the take is not written".to_string());
            }
            let taken = self.inner.take(uid, holder, last_seen_ms);
            let after = self.after_take.lock().expect("hook lock").take();
            if let Some(after) = after {
                after();
            }
            if self.losing_takes.load(Ordering::SeqCst) {
                return Err("the answer of the take is lost".to_string());
            }
            taken
        }
        fn holders(
            &self,
            uids: &[InstanceUid],
        ) -> Result<HashMap<InstanceUid, Option<String>>, String> {
            probed(&self.probe, "holders", None);
            let holders = self.inner.holders(uids);
            let after = self.after_holders.lock().expect("hook lock").take();
            if let Some(after) = after {
                after();
            }
            holders
        }
        fn rekey(
            &self,
            old: &InstanceUid,
            new: &InstanceUid,
            record: &PersistedAgent,
        ) -> Result<(), String> {
            probed(&self.probe, "rekey", Some(old));
            if self.failing_rekeys.load(Ordering::SeqCst) {
                return Err("the move is held back".to_string());
            }
            self.inner.rekey(old, new, record)?;
            let after = self.after_rekey.lock().expect("hook lock").take();
            if let Some(after) = after {
                after();
            }
            if self.losing_rekeys.load(Ordering::SeqCst) {
                return Err("the move's answer is lost".to_string());
            }
            Ok(())
        }
        fn moved(&self, old: &InstanceUid, new: &InstanceUid) -> Result<bool, String> {
            probed(&self.probe, "moved", Some(old));
            self.moved_reads.fetch_add(1, Ordering::SeqCst);
            if self.failing_moved.load(Ordering::SeqCst) {
                return Err("the read back is held back".to_string());
            }
            self.inner.moved(old, new)
        }
        fn release(
            &self,
            revisions: &[crate::configs::Pinned],
            agents: &[(InstanceUid, PersistedAgent)],
        ) -> Result<Vec<InstanceUid>, String> {
            self.inner.release(revisions, agents)
        }
        fn pinning(&self, pin: Pin<'_>) -> usize {
            self.inner.pinning(pin)
        }
        fn referenced_revisions(&self, name: &str) -> BTreeSet<String> {
            self.inner.referenced_revisions(name)
        }
    }

    /// An instance on `database`, its Agent records in `agents`, and the host register on the
    /// database read at once: what another instance binds after this is not known to it.
    fn on_postgres(
        database: &Arc<crate::postgres::Database>,
        agents: Box<dyn AgentStore>,
    ) -> AppState {
        let revocations = Revocations::open(
            Box::new(crate::postgres::ledger::PgLedgerStore::new(
                database.clone(),
            )),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations");
        AppState::with_stores(
            agents,
            Box::new(crate::postgres::configs::PgConfigBackend::new(
                database.clone(),
            )),
            Box::new(crate::clock::SystemClock),
        )
        .expect("state")
        .with_revocations(Some(Arc::new(revocations)))
    }

    /// An instance on PostgreSQL loads no Agent record when it starts, holds none until one
    /// reports to it, and reads again only the ones it holds when it reads everything again.
    /// Verifies: ADR-0025, ADR-0023
    #[test]
    fn an_instance_on_postgres_loads_no_fleet_and_reads_again_only_what_it_holds() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let elsewhere = InstanceUid([0x81; 16]);
        crate::postgres::agents::PgAgentStore::new(database.clone())
            .put(&elsewhere, &crate::agent_store::tests::record())
            .expect("another instance's record");
        let hooked = Hooked::new(Box::new(
            crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open"),
        ));
        let loads = hooked.loads.clone();
        let state = on_postgres(&database, Box::new(hooked));
        assert!(state.snapshot().is_empty(), "it holds no Agent");
        let here = InstanceUid([0x82; 16]);
        state.process(report(&here, 1), Transport::WebSocket, Some(1));
        let held: Vec<String> = state
            .snapshot()
            .into_iter()
            .map(|v| v.instance_uid)
            .collect();
        assert_eq!(
            held,
            [here.to_string()],
            "it holds the Agent that reported to it"
        );
        state.take_up_agents(None).expect("read everything again");
        assert_eq!(
            loads.load(Ordering::SeqCst),
            0,
            "no record loaded but those it holds"
        );
        assert_eq!(
            state.fleet_view().expect("fleet").len(),
            2,
            "both are in the fleet"
        );
    }

    /// On PostgreSQL an Agent's view and a rollout to it read the Agent and the Agents bound to its
    /// Host, not the whole fleet, and the view inherits the labels of its Client Agent, which this
    /// instance does not hold either.
    /// Verifies: ADR-0025
    #[test]
    fn an_agents_view_and_a_rollout_to_it_read_its_host_and_not_the_fleet() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let a = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
        );
        let (client, agent, other) = (
            InstanceUid([0x91; 16]),
            InstanceUid([0x92; 16]),
            InstanceUid([0x93; 16]),
        );
        let subject = crate::revocation::SubjectValues::default;
        report_over(
            &a,
            "h1",
            subject(),
            client,
            &[("supervisor.kind.collector", "true")],
        );
        report_over(&a, "h1", subject(), agent, &[]);
        report_over(&a, "h2", subject(), other, &[]);
        label(&a, &client, &[("ring", "early")]);

        let hooked = Hooked::new(Box::new(
            crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open"),
        ));
        let whole = hooked.whole_queries.clone();
        let dir = tempfile::tempdir().expect("blobs");
        let b = delivering_on(&database, Box::new(hooked), dir.path());
        aim_at_the_early_ring(&b);
        let view = b.agent_view(&agent).expect("read").expect("known");
        assert_eq!(
            view.inherited_labels.get("ring").map(String::as_str),
            Some("early")
        );
        assert_eq!(view.client_agent, client.to_string());
        assert!(b
            .agent_view(&InstanceUid([0x94; 16]))
            .expect("read")
            .is_none());
        assert!(
            b.rollout_to_agent(&agent, None).is_ok(),
            "the Deployment aimed at the inherited label releases to it"
        );
        assert_eq!(
            released_through(&database, &agent).as_deref(),
            Some("early")
        );
        assert_eq!(
            whole.load(Ordering::SeqCst),
            0,
            "no read of the whole fleet"
        );
        assert!(b.snapshot().is_empty());
    }

    /// An instance on PostgreSQL with package delivery, its blobs under `dir`.
    fn delivering_on(
        database: &Arc<crate::postgres::Database>,
        agents: Box<dyn AgentStore>,
        dir: &std::path::Path,
    ) -> AppState {
        let records = crate::Records::Postgres(database.clone());
        let (packages, _) = PackageStore::open_on(
            &records,
            Some(dir),
            &toml::from_str::<crate::config::BlobsConfig>("").expect("blobs"),
            crate::clock::Elapsed::monotonic(),
        )
        .expect("packages");
        on_postgres(database, agents).with_packages(Some(
            PackageOffering::on(&records, packages).expect("deployments"),
        ))
    }

    /// A Deployment `early`, selecting `ring=early`, that holds a Configuration for `otelcol`.
    fn aim_at_the_early_ring(state: &AppState) {
        state
            .save_configuration(
                "collector",
                Revision {
                    body: "receivers: {}\n".to_string(),
                    role: String::new(),
                    service_name: "otelcol".to_string(),
                },
            )
            .expect("save");
        state
            .deployment_store()
            .expect("deployments")
            .put(
                "early",
                BTreeMap::from([("ring".to_string(), "early".to_string())]),
            )
            .expect("deployment");
        state
            .put_deployment_configuration("early", "collector")
            .expect("hold it");
    }

    /// Two instances on `database`, A opened first: a Configuration `base` and a Package A saved,
    /// an empty Deployment `stable`, and a Deployment `canary` that holds both, which B reads
    /// when it opens. What B deletes after, A holds in memory until it takes it up.
    fn holding_base_on_two_instances(
        database: &Arc<crate::postgres::Database>,
        dir: &std::path::Path,
    ) -> (AppState, AppState, PackageId) {
        let on = || {
            delivering_on(
                database,
                Box::new(
                    crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open"),
                ),
                dir,
            )
        };
        let a = on();
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        a.save_configuration("base", body("receivers: {}\n"))
            .expect("save");
        a.create_package_set(&id).expect("package");
        let deployments = a.deployment_store().expect("deployments");
        for name in ["canary", "stable"] {
            deployments
                .put(
                    name,
                    BTreeMap::from([("ring".to_string(), name.to_string())]),
                )
                .expect("deployment");
        }
        a.put_deployment_configuration("canary", "base")
            .expect("base in canary");
        a.put_deployment_package("canary", &id, false)
            .expect("the package in canary");
        (a, on(), id)
    }

    /// The member rows the database stores, as `deployment member` lines.
    fn stored_members(test: &crate::postgres::testing::TestDatabase) -> Vec<Vec<Option<String>>> {
        test.rows(
            "SELECT deployment || ' ' || configuration FROM deployment_configurations UNION ALL \
             SELECT deployment || ' ' || agent_type || '@' || version FROM deployment_packages",
        )
    }

    /// A Configuration and a Package deleted through B leave every Deployment that held them, in
    /// B's memory too, and A, which has not taken up the deletions and still holds both in
    /// memory, is refused when it adds either to a Deployment: the add reads the member it adds
    /// in its own transaction and finds it gone, so no Deployment names it.
    /// Verifies: ADR-0025, ADR-0033
    #[test]
    fn a_member_deleted_through_one_instance_is_refused_by_another_that_has_not_taken_it_up() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let dir = tempfile::tempdir().expect("blobs");
        let (a, b, id) = holding_base_on_two_instances(&database, dir.path());

        assert!(b.delete_configuration("base").expect("deleted"));
        assert_eq!(b.delete_package_set(&id), Ok(true));
        let canary = b
            .deployment_store()
            .expect("deployments")
            .get("canary")
            .expect("canary");
        assert!(
            canary.configurations.is_empty() && canary.packages.is_empty(),
            "both left B's canary: {canary:?}"
        );
        assert!(stored_members(&test).is_empty(), "and the stored one");

        assert!(
            a.configurations().get("base").is_some(),
            "A holds the Configuration in memory still"
        );
        assert!(matches!(
            a.put_deployment_configuration("stable", "base"),
            Err(DeploymentError::Missing(why)) if why.contains("\"base\"")
        ));
        assert!(matches!(
            a.put_deployment_package("stable", &id, false),
            Err(DeploymentError::Missing(why)) if why.contains("otelcol")
        ));
        assert!(
            stored_members(&test).is_empty(),
            "no Deployment names either"
        );
        let stable = a
            .deployment_store()
            .expect("deployments")
            .get("stable")
            .expect("stable");
        assert!(
            stable.configurations.is_empty() && stable.packages.is_empty(),
            "nor does A's stable: {stable:?}"
        );
    }

    /// An add through A that holds the Deployment's row, held at a gate in the database just
    /// before it writes the member's row, while B deletes that member: the add holds the member's
    /// row too, so the deletion waits for its commit, and then takes the member out of every
    /// Deployment in the transaction that deletes it. Whichever commits first, no Deployment names
    /// the deleted member once both have — for a Configuration and for a Package.
    /// Verifies: ADR-0025, ADR-0033
    #[test]
    fn an_add_held_while_another_instance_deletes_the_member_leaves_no_deployment_naming_it() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let dir = tempfile::tempdir().expect("blobs");
        let (a, b, id) = holding_base_on_two_instances(&database, dir.path());
        let race = |table: &str,
                    key: i64,
                    add: &(dyn Fn() -> Result<Deployment, DeploymentError> + Sync),
                    delete: &(dyn Fn() -> bool + Sync)| {
            test.sql(&format!(
                "CREATE FUNCTION fleet_test_hold_{table}() RETURNS trigger LANGUAGE plpgsql AS $$ \
                 BEGIN PERFORM pg_advisory_lock_shared({key}); \
                 PERFORM pg_advisory_unlock_shared({key}); RETURN NEW; END $$; \
                 CREATE TRIGGER fleet_test_hold BEFORE INSERT ON {table} \
                 FOR EACH ROW EXECUTE FUNCTION fleet_test_hold_{table}();"
            ));
            let gate = test.gate(key);
            std::thread::scope(|scope| {
                let added = scope.spawn(add);
                // A holds the Deployment's row and waits at the gate with its transaction open.
                let a_pid = test.once(&format!(
                    "SELECT pid::text FROM pg_locks WHERE locktype = 'advisory' AND NOT granted \
                     AND objid::text = '{key}'"
                ));
                let deleted = scope.spawn(delete);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while !deleted.is_finished()
                    && test
                        .rows(&format!(
                            "SELECT 1 FROM pg_stat_activity \
                             WHERE {a_pid} = ANY(pg_blocking_pids(pid))"
                        ))
                        .is_empty()
                {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "B's deletion neither waits for A nor ends"
                    );
                }
                gate.open();
                assert!(added.join().expect("A").is_ok(), "A's add commits first");
                assert!(deleted.join().expect("B"), "B deletes it");
            });
        };

        race(
            "deployment_configurations",
            0x25_0001,
            &|| a.put_deployment_configuration("stable", "base"),
            &|| b.delete_configuration("base").expect("deleted"),
        );
        race(
            "deployment_packages",
            0x25_0002,
            &|| a.put_deployment_package("stable", &id, false),
            &|| b.delete_package_set(&id).expect("deleted"),
        );
        assert!(
            stored_members(&test).is_empty(),
            "no Deployment names either: {:?}",
            stored_members(&test)
        );
        b.deployment_store()
            .expect("deployments")
            .reload()
            .expect("B reads the Deployments again");
        assert!(b
            .deployment_store()
            .expect("deployments")
            .list()
            .iter()
            .all(
                |deployment| deployment.configurations.is_empty() && deployment.packages.is_empty()
            ));
    }

    /// The Deployment the stored assignment of `uid` was released through.
    fn released_through(
        database: &Arc<crate::postgres::Database>,
        uid: &InstanceUid,
    ) -> Option<String> {
        crate::postgres::agents::PgAgentStore::new(database.clone())
            .get(uid)
            .expect("read")
            .and_then(|stored| stored.assignment)
            .map(|assignment| assignment.deployment)
    }

    /// An Agent bound to a direct Host that reports over a Gateway since is placed by the Gateway
    /// it arrived over: the list, its view and a rollout to it all inherit the labels of the
    /// Client Agent behind that Gateway.
    /// Verifies: ADR-0025, ADR-0030
    #[test]
    fn an_agent_bound_to_a_direct_host_and_arriving_over_a_gateway_is_placed_by_the_gateway() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let a = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
        );
        let (gateway, client, agent) = (
            InstanceUid([0xa1; 16]),
            InstanceUid([0xa2; 16]),
            InstanceUid([0xa3; 16]),
        );
        let subject = crate::revocation::SubjectValues::default;
        report_over(&a, "g", subject(), gateway, &[]);
        assert_eq!(a.set_gateway("g", true), Ok(true));
        report_over(&a, "h1", subject(), agent, &[]);
        report_over(
            &a,
            "g",
            subject(),
            client,
            &[("supervisor.kind.collector", "true"), ("host.name", "m1")],
        );
        report_over(&a, "g", subject(), agent, &[("host.name", "m1")]);
        label(&a, &client, &[("ring", "early")]);

        let dir = tempfile::tempdir().expect("blobs");
        let b = delivering_on(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
            dir.path(),
        );
        aim_at_the_early_ring(&b);
        let inherited =
            |view: &AgentView| (view.inherited_labels.clone(), view.client_agent.clone());
        let expected = (
            BTreeMap::from([("ring".to_string(), "early".to_string())]),
            client.to_string(),
        );
        let listed = b
            .fleet_view()
            .expect("fleet")
            .into_iter()
            .find(|view| view.instance_uid == agent.to_string())
            .expect("listed");
        assert_eq!(inherited(&listed), expected, "the list");
        let viewed = b.agent_view(&agent).expect("read").expect("known");
        assert_eq!(inherited(&viewed), expected, "its view");
        assert!(b.rollout_to_agent(&agent, None).is_ok(), "a rollout to it");
        assert_eq!(
            released_through(&database, &agent).as_deref(),
            Some("early")
        );
    }

    /// A record that does not decode, which every read that meets it fails, can still be
    /// forgotten through an instance that does not hold it, and is gone after, with what its
    /// assignment pinned: the Deployment it was released through may be deleted then.
    /// Verifies: ADR-0023, ADR-0030
    #[test]
    fn a_record_that_does_not_decode_can_be_forgotten() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let uid = InstanceUid([0x84; 16]);
        crate::postgres::agents::PgAgentStore::new(database.clone())
            .put(&uid, &crate::agent_store::tests::record())
            .expect("put");
        test.sql("UPDATE agents SET health = '\\xff'::bytea");
        let dir = tempfile::tempdir().expect("blobs");
        let state = delivering_on(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
            dir.path(),
        );
        assert!(
            matches!(
                state.delete_deployment("stable"),
                Err(DeploymentError::Conflict(_))
            ),
            "its assignment pins the Deployment"
        );
        assert!(state.fleet_view().is_err(), "the fleet cannot be read");
        assert!(
            state.forget_agent(&uid).is_ok(),
            "it is forgotten all the same"
        );
        // Before any read of the fleet, which would take up its absence.
        assert!(
            state.delete_deployment("stable").is_ok(),
            "and pins nothing any more"
        );
        assert!(state.fleet_view().expect("readable again").is_empty());
    }

    /// Forgetting an Agent this instance on PostgreSQL does not hold collects the revision only its
    /// assignment pinned, as forgetting one it holds does.
    /// Verifies: ADR-0033, ADR-0023
    #[test]
    fn forgetting_an_agent_held_nowhere_collects_the_revision_only_it_pinned() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        // A rollout releases through a stored Deployment.
        test.store_deployments(&["stable"]);
        let body = |text: &str| crate::configs::Revision {
            body: text.to_string(),
            role: String::new(),
            service_name: "otelcol".to_string(),
        };
        let (pinned, saved) = (body("v1\n"), body("v2\n"));
        let hash = crate::configs::revision_hash(&pinned);
        let configs = crate::postgres::configs::PgConfigBackend::new(database.clone());
        configs.save("base", &pinned).expect("save");
        let uid = InstanceUid([0x83; 16]);
        let mut stored = crate::agent_store::tests::record();
        stored.assignment = Some(Assignment {
            deployment: "stable".to_string(),
            package: None,
            configurations: BTreeMap::from([("base".to_string(), hash.clone())]),
        });
        let agents = crate::postgres::agents::PgAgentStore::new(database.clone());
        agents.put(&uid, &stored).expect("put");
        agents
            .release(
                &[crate::configs::Pinned {
                    configuration: "base".to_string(),
                    hash: hash.clone(),
                    revision: pinned,
                }],
                &[(uid, stored)],
            )
            .expect("release");
        configs.save("base", &saved).expect("save again");

        let state = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
        );
        let retained = |state: &AppState| {
            state
                .configurations()
                .get("base")
                .expect("base")
                .retained
                .contains_key(&hash)
        };
        assert!(retained(&state), "pinned while the Agent is stored");
        assert!(state.snapshot().is_empty(), "held nowhere");
        assert!(state.forget_agent(&uid).is_ok(), "silent, so forgotten");
        assert!(!retained(&state), "collected with it");
    }

    /// A certificate of `host`.
    fn of_host(host: &str) -> Presented {
        Presented {
            id: CertId::new(b"CA", host),
            host: Some(host.to_string()),
            subject_values: crate::revocation::SubjectValues::default(),
            not_after_ms: u64::MAX,
        }
    }

    /// An Agent that comes back on a new connection of its host keeps its identity, the newest
    /// connection carries it and the move is logged; a report of the older connection is taken up
    /// and leaves it there, so the older one's close leaves it connected. On a connection of
    /// another host the same identity is a duplicate, re-keyed.
    /// Verifies: ADR-0025
    #[test]
    fn an_agent_on_a_new_connection_of_its_host_is_carried_by_it_and_not_rekeyed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let revocations = Arc::new(
            Revocations::open(
                Box::new(crate::fs::FsLedgerStore::open(dir.path()).expect("ledger")),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::new(dir.path().join("configs"))
            .expect("state")
            .with_revocations(Some(revocations));
        let uid = InstanceUid::default();
        let (old, new, other) = (
            state.connection_id(),
            state.connection_id(),
            state.connection_id(),
        );
        let carrier = |state: &AppState| {
            state
                .fleet
                .lock()
                .expect("fleet lock")
                .get(&uid)
                .map(|record| (record.connected, record.owner))
        };
        crate::test_support::logged();
        {
            let host = of_host("edge-1");
            let first = state.process_presented(
                report(&uid, 1),
                Transport::WebSocket,
                Some(old),
                Some(&host),
            );
            assert!(first.reply.agent_identification.is_none());
            let back = state.process_presented(
                report(&uid, 2),
                Transport::WebSocket,
                Some(new),
                Some(&host),
            );
            assert!(
                back.reply.agent_identification.is_none(),
                "not a duplicate of itself"
            );
            assert_eq!(carrier(&state), Some((true, Some(new))));
            let late = state.process_presented(
                report(&uid, 3),
                Transport::WebSocket,
                Some(old),
                Some(&host),
            );
            assert!(late.reply.agent_identification.is_none());
            assert!(late.reply.error_response.is_none(), "taken up");
            assert_eq!(
                carrier(&state),
                Some((true, Some(new))),
                "the older one takes it no more"
            );
            state.mark_disconnected(&[uid], old);
            assert_eq!(carrier(&state), Some((true, Some(new))));

            let clone = state.process_presented(
                report(&uid, 1),
                Transport::WebSocket,
                Some(other),
                Some(&of_host("edge-2")),
            );
            let new_uid = clone
                .reply
                .agent_identification
                .expect("another host's is re-keyed");
            assert_ne!(new_uid.new_instance_uid, uid.as_bytes().to_vec());
        }
        let came_back = crate::test_support::lines_with(&[
            "came back on a new connection of its host",
            &uid.to_string(),
        ]);
        assert_eq!(
            came_back.len(),
            1,
            "the move to the new connection is logged once: {came_back:?}"
        );
    }

    /// A state on files with a host register whose writes of the Hosts run `probe` first, and the
    /// state `probe` may reach once it is made.
    fn with_probed_register(
        dir: &std::path::Path,
        probe: Probe,
        of: &Arc<OnceLock<std::sync::Weak<AppState>>>,
    ) -> Arc<AppState> {
        let revocations = Arc::new(
            Revocations::open(
                Box::new(ProbedLedger {
                    inner: Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
                    probe,
                }),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = Arc::new(
            AppState::new(dir.join("configs"))
                .expect("state")
                .with_revocations(Some(revocations)),
        );
        of.set(Arc::downgrade(&state)).expect("set once");
        state
    }

    /// Who carries `uid` in `state`'s memory: whether connected, and on which connection.
    fn carrier_of(state: &AppState, uid: &InstanceUid) -> Option<(bool, Option<ConnId>)> {
        state
            .fleet
            .lock()
            .expect("fleet lock")
            .get(uid)
            .map(|record| (record.connected, record.owner))
    }

    /// A goodbye the older connection of a host delivers while a newer one carries the Agent does
    /// not end the Agent's connection: it stays connected on the newer one.
    /// Verifies: ADR-0025
    #[test]
    fn a_goodbye_on_an_older_connection_of_the_host_leaves_the_agent_with_the_newer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let of = Arc::new(OnceLock::new());
        let state = with_probed_register(dir.path(), Arc::new(|_, _| {}), &of);
        let uid = InstanceUid::default();
        let (old, new) = (state.connection_id(), state.connection_id());
        let host = of_host("edge-1");
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(old),
            Some(&host),
        );
        state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(new),
            Some(&host),
        );
        let mut goodbye = report(&uid, 2);
        goodbye.agent_disconnect = Some(opamp::proto::AgentDisconnect {});
        let answered =
            state.process_presented(goodbye, Transport::WebSocket, Some(old), Some(&host));
        assert!(
            answered.disconnected,
            "the older connection lets the Agent go"
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(new))));
    }

    /// A queued restart is not sent on an older connection of the host while a newer one carries
    /// the Agent: it stays queued, and the newer connection's next report is sent it.
    /// Verifies: ADR-0025
    #[test]
    fn a_queued_restart_is_not_sent_on_an_older_connection_of_the_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let of = Arc::new(OnceLock::new());
        let state = with_probed_register(dir.path(), Arc::new(|_, _| {}), &of);
        let uid = InstanceUid::default();
        let (old, new) = (state.connection_id(), state.connection_id());
        let host = of_host("edge-1");
        let restartable = |sequence_num| {
            let mut msg = report(&uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
            msg
        };
        state.process_presented(restartable(1), Transport::WebSocket, Some(old), Some(&host));
        state.process_presented(restartable(2), Transport::WebSocket, Some(new), Some(&host));
        assert!(state.request_restart(&uid).is_ok(), "queued");
        let late =
            state.process_presented(restartable(3), Transport::WebSocket, Some(old), Some(&host));
        assert!(
            late.reply.command.is_none(),
            "not sent on the older connection"
        );
        assert!(state.fleet.lock().expect("fleet lock")[&uid].restart_pending);
        let next =
            state.process_presented(restartable(4), Transport::WebSocket, Some(new), Some(&host));
        assert!(next.reply.command.is_some(), "sent on the newer one");
    }

    /// A queued restart pushed to every connection that named the Agent goes out only on the one
    /// that carries it: an older connection of its host that still names it, its peer likely
    /// gone, is handed none, and the restart stays queued for the newer one.
    /// Verifies: ADR-0025
    #[test]
    fn a_pushed_restart_goes_out_only_on_the_connection_that_carries_the_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let of = Arc::new(OnceLock::new());
        let state = with_probed_register(dir.path(), Arc::new(|_, _| {}), &of);
        let uid = InstanceUid::default();
        let (old, new) = (state.connection_id(), state.connection_id());
        let host = of_host("edge-1");
        let restartable = |sequence_num| {
            let mut msg = report(&uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
            msg
        };
        state.process_presented(restartable(1), Transport::WebSocket, Some(old), Some(&host));
        state.process_presented(restartable(2), Transport::WebSocket, Some(new), Some(&host));
        let late =
            state.process_presented(restartable(3), Transport::WebSocket, Some(old), Some(&host));
        assert_eq!(late.uid, Some(uid), "the older connection still names it");
        assert!(state.request_restart(&uid).is_ok(), "queued");
        assert!(
            state.restart_command_for(&uid, old).is_none(),
            "not handed to the older connection"
        );
        assert!(state.fleet.lock().expect("fleet lock")[&uid].restart_pending);
        assert!(
            state.restart_command_for(&uid, new).is_some(),
            "handed to the newer one"
        );
    }

    /// A report of an older connection of the host does not use up the demand for the component
    /// map: the newer connection is asked for it.
    /// Verifies: ADR-0025
    #[test]
    fn an_older_connection_of_the_host_is_not_asked_for_the_component_map() {
        let dir = tempfile::tempdir().expect("tempdir");
        let of = Arc::new(OnceLock::new());
        let state = with_probed_register(dir.path(), Arc::new(|_, _| {}), &of);
        let uid = InstanceUid::default();
        let (old, new) = (state.connection_id(), state.connection_id());
        let host = of_host("edge-1");
        let hashed = |sequence_num| {
            let mut msg = report(&uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::ReportsAvailableComponents as u64;
            msg.available_components = Some(opamp::proto::AvailableComponents {
                hash: vec![sequence_num as u8],
                ..Default::default()
            });
            msg
        };
        let asked = |processed: &Processed| {
            processed.reply.flags & ServerToAgentFlags::ReportAvailableComponents as u64 != 0
        };
        state.process_presented(hashed(1), Transport::WebSocket, Some(old), Some(&host));
        state.process_presented(hashed(2), Transport::WebSocket, Some(new), Some(&host));
        let late = state.process_presented(hashed(3), Transport::WebSocket, Some(old), Some(&host));
        assert!(!asked(&late), "not asked on the older connection");
        let next = state.process_presented(hashed(3), Transport::WebSocket, Some(new), Some(&host));
        assert!(asked(&next), "asked on the newer one");
    }

    /// A state on files whose clock the test moves, with a host register in which `gw-1` is a
    /// Gateway's Host; beside it the clock and a certificate of that Gateway.
    fn with_a_gateway(
        dir: &std::path::Path,
        offer: Option<ConnectionOffer>,
    ) -> (AppState, Arc<AtomicU64>, Presented) {
        let now = Arc::new(AtomicU64::new(1_000_000));
        let revocations = Arc::new(
            Revocations::open(
                Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::with_stores(
            Box::new(crate::fs::FsAgentStore::open(dir).expect("agents")),
            Box::new(crate::fs::FsConfigBackend::open(dir).expect("configs")),
            Box::new(Manual(now.clone())),
        )
        .expect("state")
        .with_revocations(Some(revocations.clone()))
        .with_connection_offer(offer);
        let gateway = of_host("gw-1");
        // Its first report makes its Host; the mark makes it a Gateway's.
        let first = state.connection_id();
        state.process_presented(
            report(&InstanceUid::default(), 1),
            Transport::WebSocket,
            Some(first),
            Some(&gateway),
        );
        assert!(revocations.set_gateway("gw-1", true).expect("mark"));
        (state, now, gateway)
    }

    /// An Agent behind a Gateway that its pool moved from the newer of two open connections to the
    /// older one is taken by its report there once the newer one has carried none of its reports
    /// for a heartbeat interval: it is asked for its component map there, and its goodbye there
    /// ends it.
    /// Verifies: ADR-0025
    #[test]
    fn a_gateway_s_agent_moved_to_an_older_connection_is_taken_there_after_an_interval() {
        crate::test_support::logged();
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, now, gateway) = with_a_gateway(dir.path(), None);
        let uid = InstanceUid::default();
        let (older, newer) = (state.connection_id(), state.connection_id());
        let hashed = |sequence_num: u64| {
            let mut msg = report(&uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::ReportsAvailableComponents as u64;
            msg.available_components = Some(opamp::proto::AvailableComponents {
                hash: vec![7],
                ..Default::default()
            });
            msg
        };
        let asked = |processed: &Processed| {
            processed.reply.flags & ServerToAgentFlags::ReportAvailableComponents as u64 != 0
        };
        state.process_presented(hashed(1), Transport::WebSocket, Some(newer), Some(&gateway));
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(newer))));
        // The Agent restarts, and the pool places it on the older connection; the newer one,
        // carrying other Agents, stays open and carries none of its reports from now on.
        now.fetch_add(DEFAULT_HEARTBEAT.as_millis() as u64, Ordering::SeqCst);
        let taken =
            state.process_presented(hashed(1), Transport::WebSocket, Some(older), Some(&gateway));
        assert_eq!(
            carrier_of(&state, &uid),
            Some((true, Some(older))),
            "the older connection carries it"
        );
        assert!(asked(&taken), "asked for its component map there");
        assert_eq!(
            crate::test_support::lines_with(&["carried none of its reports", &uid.to_string()])
                .len(),
            1,
            "the move to the stale carrier's older connection is logged"
        );
        let mut goodbye = report(&uid, 2);
        goodbye.agent_disconnect = Some(opamp::proto::AgentDisconnect {});
        state.process_presented(goodbye, Transport::WebSocket, Some(older), Some(&gateway));
        assert_eq!(
            carrier_of(&state, &uid),
            Some((false, None)),
            "its goodbye ends it"
        );
    }

    /// Within a heartbeat interval of the newer connection's last report of an Agent, a report on
    /// an older connection of the same Gateway does not take the Agent back.
    /// Verifies: ADR-0025
    #[test]
    fn within_an_interval_an_older_connection_of_a_gateway_does_not_take_the_agent_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, now, gateway) = with_a_gateway(dir.path(), None);
        let uid = InstanceUid::default();
        let (older, newer) = (state.connection_id(), state.connection_id());
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(newer),
            Some(&gateway),
        );
        now.fetch_add(DEFAULT_HEARTBEAT.as_millis() as u64 - 1, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 2),
            Transport::WebSocket,
            Some(older),
            Some(&gateway),
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(newer))));
        // A report on the newer connection carries it on, so a heartbeat later the older one
        // still does not take it.
        state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(newer),
            Some(&gateway),
        );
        now.fetch_add(DEFAULT_HEARTBEAT.as_millis() as u64 - 1, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 4),
            Transport::WebSocket,
            Some(older),
            Some(&gateway),
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(newer))));
    }

    /// The interval this Server offers decides when a Gateway's newer connection is stale: at an
    /// offered 60 seconds, a report on the older connection 30 seconds after the newer one's last
    /// does not take the Agent, and one 60 seconds after does.
    /// Verifies: ADR-0025
    #[test]
    fn the_offered_heartbeat_interval_decides_when_a_carrier_is_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let offer = ConnectionOffer::from_config(
            &toml::from_str::<crate::config::ConnectionOfferConfig>(
                "heartbeat_interval_secs = 60\n",
            )
            .expect("offer config"),
        );
        let (state, now, gateway) = with_a_gateway(dir.path(), Some(offer));
        let uid = InstanceUid::default();
        let (older, newer) = (state.connection_id(), state.connection_id());
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(newer),
            Some(&gateway),
        );
        now.fetch_add(30_000, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 2),
            Transport::WebSocket,
            Some(older),
            Some(&gateway),
        );
        assert_eq!(
            carrier_of(&state, &uid),
            Some((true, Some(newer))),
            "30 s is within the offered interval"
        );
        now.fetch_add(30_000, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(older),
            Some(&gateway),
        );
        assert_eq!(
            carrier_of(&state, &uid),
            Some((true, Some(older))),
            "60 s is one offered interval"
        );
    }

    /// A newer connection whose report waited on the store for a heartbeat interval is as fresh as
    /// its report once it took the Agent: the older connection's next report does not take the
    /// Agent back.
    /// Verifies: ADR-0025
    #[test]
    fn a_carrier_whose_report_waited_an_interval_is_not_stale_once_it_took_the_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = Arc::new(AtomicU64::new(1_000_000));
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe: Probe = {
            let (now, armed) = (now.clone(), armed.clone());
            Arc::new(move |call, _| {
                if call == "save_hosts" && armed.swap(false, Ordering::SeqCst) {
                    // The newer connection's round waits an interval on the store.
                    now.fetch_add(DEFAULT_HEARTBEAT.as_millis() as u64, Ordering::SeqCst);
                }
            })
        };
        let revocations = Arc::new(
            Revocations::open(
                Box::new(ProbedLedger {
                    inner: Box::new(crate::fs::FsLedgerStore::open(dir.path()).expect("ledger")),
                    probe,
                }),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::with_stores(
            Box::new(crate::fs::FsAgentStore::open(dir.path()).expect("agents")),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(Manual(now.clone())),
        )
        .expect("state")
        .with_revocations(Some(revocations));
        let uid = InstanceUid::default();
        let (old, new) = (state.connection_id(), state.connection_id());
        let until = |not_after_ms| Presented {
            not_after_ms,
            ..of_host("edge-1")
        };
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(old),
            Some(&until(1)),
        );
        armed.store(true, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 2),
            Transport::WebSocket,
            Some(new),
            Some(&until(2)),
        );
        assert!(
            !armed.load(Ordering::SeqCst),
            "the newer connection's round waited"
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(new))));
        state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(old),
            Some(&until(2)),
        );
        assert_eq!(
            carrier_of(&state, &uid),
            Some((true, Some(new))),
            "the older connection's backlog does not take it back"
        );
    }

    /// A report of an older connection of the host whose newer connection closes while the report
    /// is taken up is not left without a connection: the older one carries the Agent again.
    /// Verifies: ADR-0025
    #[test]
    fn an_older_connection_takes_the_agent_back_when_the_newer_closes_meanwhile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let of: Arc<OnceLock<std::sync::Weak<AppState>>> = Arc::new(OnceLock::new());
        let uid = InstanceUid::default();
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closing = Arc::new(Mutex::new(None::<ConnId>));
        let probe: Probe = {
            let (of, armed, closing) = (of.clone(), armed.clone(), closing.clone());
            Arc::new(move |call, _| {
                if call == "save_hosts" && armed.swap(false, Ordering::SeqCst) {
                    // The newer connection closes while the older one's report moves its Host's
                    // valid until.
                    let state = of.get().and_then(std::sync::Weak::upgrade).expect("state");
                    let conn = closing.lock().expect("closing").expect("a connection");
                    state.mark_disconnected(&[uid], conn);
                }
            })
        };
        let state = with_probed_register(dir.path(), probe, &of);
        let (old, new) = (state.connection_id(), state.connection_id());
        *closing.lock().expect("closing") = Some(new);
        let until = |not_after_ms| Presented {
            not_after_ms,
            ..of_host("edge-1")
        };
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(old),
            Some(&until(1)),
        );
        state.process_presented(
            report(&uid, 2),
            Transport::WebSocket,
            Some(new),
            Some(&until(1)),
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(new))));
        armed.store(true, Ordering::SeqCst);
        let late = state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(old),
            Some(&until(2)),
        );
        assert!(
            !armed.load(Ordering::SeqCst),
            "the newer connection closed meanwhile"
        );
        assert!(late.reply.error_response.is_none());
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(old))));
    }

    /// What an instance wrote of an Agent's presence, the count of takes among it, leaves with the
    /// Agent wherever it leaves what the instance holds: when it is forgotten here, when it was
    /// forgotten through another instance, and under its old identity when it is re-keyed.
    /// Verifies: ADR-0030
    #[test]
    fn what_was_written_of_an_agents_presence_leaves_with_it() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let on = |lease: &str| {
            let state = on_postgres(
                &database,
                Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
            );
            state.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
            state
        };
        let (a, b) = (
            on("00000000-0000-7000-8000-00000000000a"),
            on("00000000-0000-7000-8000-00000000000b"),
        );
        let kept = |state: &AppState, uid: &InstanceUid| {
            let written = state.presence_written.lock().expect("presence lock");
            (
                written.agents.contains_key(uid),
                written.takes.contains_key(uid),
            )
        };
        let (forgotten, elsewhere, rekeyed) = (
            InstanceUid([0x71; 16]),
            InstanceUid([0x72; 16]),
            InstanceUid([0x73; 16]),
        );
        for (conn, uid) in (1..).zip([forgotten, elsewhere, rekeyed]) {
            a.process(report(&uid, 1), Transport::WebSocket, Some(conn));
            assert_eq!(kept(&a, &uid), (true, true), "taken");
        }

        a.mark_disconnected(&[forgotten], 1);
        a.mark_disconnected(&[elsewhere], 2);
        assert!(a.forget_agent(&forgotten).is_ok(), "forget here");
        assert_eq!(kept(&a, &forgotten), (false, false), "forgotten here");

        assert!(b.forget_agent(&elsewhere).is_ok(), "forget through B");
        // A report that changes the record finds it gone.
        let mut changed = report(&elsewhere, 3);
        changed.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            ..Default::default()
        });
        a.process(changed, Transport::WebSocket, Some(2));
        assert_eq!(
            kept(&a, &elsewhere),
            (false, false),
            "forgotten through another instance"
        );

        let mut rekey = report(&rekeyed, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        a.process(rekey, Transport::WebSocket, Some(3));
        assert_eq!(kept(&a, &rekeyed), (false, false), "re-keyed");
    }

    /// An instance that heard another took an Agent reads its holder under the Agent's own lock: a
    /// take of its own by the Agent's new connection here, made while the holder is read, waits
    /// for the read and keeps the Agent carried.
    /// Verifies: ADR-0025
    #[test]
    fn an_agent_taken_back_here_while_its_holder_is_read_stays_carried() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_holders = hooked.after_holders.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(
            tokio::sync::watch::channel(Some("00000000-0000-7000-8000-00000000000a".to_string())).1,
        );
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        b.serve_under(
            tokio::sync::watch::channel(Some("00000000-0000-7000-8000-00000000000b".to_string())).1,
        );
        let uid = InstanceUid([0x74; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        b.process(report(&uid, 2), Transport::WebSocket, Some(9));
        // While A reads the holder, the Agent's old connection to A closes and it connects to A
        // again, which takes it back.
        let again = a.clone();
        let (taken_back, taking_back) = std::sync::mpsc::channel();
        *after_holders.lock().expect("hook lock") = Some(Box::new(move || {
            std::thread::spawn(move || {
                again.mark_disconnected(&[uid], 7);
                again.process(report(&uid, 3), Transport::WebSocket, Some(8));
                let _ = taken_back.send(());
            });
        }));
        a.release_taken(None).expect("release");
        assert!(
            after_holders.lock().expect("hook lock").is_none(),
            "the take came while the holder was read"
        );
        taking_back
            .recv_timeout(Duration::from_secs(30))
            .expect("the Agent's new connection is answered");
        assert!(view(&a, &uid).connected, "still carried");
    }

    /// On PostgreSQL a first report here whose read of the stored record failed goes on from the
    /// record another instance created, as it is stored: its labels are held, and a report the
    /// binding then refuses leaves it stored and its Agent carried where it is.
    /// Verifies: ADR-0025
    #[test]
    fn a_record_another_instance_created_is_taken_as_stored_and_never_removed() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_gets.clone();
        let a = on_postgres(&database, Box::new(hooked));
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let (bound, labelled) = (InstanceUid([0x61; 16]), InstanceUid([0x62; 16]));
        for uid in [bound, labelled] {
            b.process_presented(report(&uid, 1), Transport::Http, None, Some(&of_host("h1")));
        }
        label(&b, &labelled, &[("tier", "edge")]);

        failing.store(1, Ordering::SeqCst);
        let refused = a.process_presented(
            report(&bound, 2),
            Transport::Http,
            None,
            Some(&of_host("h2")),
        );
        assert!(
            refused.reply.error_response.is_some(),
            "the binding refuses it"
        );
        assert_eq!(
            test.rows(&format!(
                "SELECT instance_uid FROM agents WHERE instance_uid = '{bound}'"
            ))
            .len(),
            1,
            "the record B created stays"
        );

        failing.store(1, Ordering::SeqCst);
        a.process_presented(
            report(&labelled, 2),
            Transport::Http,
            None,
            Some(&of_host("h1")),
        );
        assert_eq!(view(&a, &labelled).labels["tier"], "edge", "held as stored");
    }

    /// A label change made on A after A's take-up read the record, before it took the fleet lock,
    /// is not undone by what that read found: what differs is read again under the lock. A never
    /// hears its own announcement, so nothing else would correct it.
    /// Verifies: ADR-0025
    #[test]
    fn a_take_up_never_undoes_a_newer_act_of_its_own_instance() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_get = hooked.after_get.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let uid = InstanceUid([0x63; 16]);
        a.process(report(&uid, 1), Transport::Http, None);
        label(&b, &uid, &[("tier", "edge")]);

        // A's take-up reads B's labels; then an operator sets other labels through A.
        let on_a = Arc::downgrade(&a);
        *after_get.lock().expect("hook lock") = Some(Box::new(move || {
            label(&on_a.upgrade().expect("A"), &uid, &[("tier", "core")]);
        }));
        a.take_up_agents(Some(&HashSet::from([uid])))
            .expect("take up");
        assert_eq!(view(&a, &uid).labels["tier"], "core", "A's own act stays");
    }

    /// A re-key whose move of the stored record fails leaves the Agent under the identity it has:
    /// the reply assigns no new one, the record stays stored and held under the old uid, and the
    /// session is not ended as one of an Agent forgotten.
    /// Verifies: ADR-0030
    #[test]
    fn a_rekey_whose_move_fails_keeps_the_agent_under_its_identity() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_rekeys.clone();
        let a = on_postgres(&database, Box::new(hooked));
        let uid = InstanceUid([0x67; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));

        failing.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process(rekey, Transport::WebSocket, Some(7));
        assert_eq!(answered.uid, Some(uid), "carried under its identity");
        assert!(
            answered.reply.agent_identification.is_none(),
            "no new identity is assigned"
        );
        assert!(a.take_forgotten(7).is_empty(), "its session goes on");
        assert_eq!(view(&a, &uid).instance_uid, uid.to_string());
        assert_eq!(
            test.rows("SELECT instance_uid FROM agents"),
            [vec![Some(uid.to_string())]],
            "the record stays stored under it"
        );
    }

    /// What differs is read again under the fleet lock in one read, however many records differ:
    /// the lock is held for one round trip, not one per record.
    /// Verifies: ADR-0025
    #[test]
    fn a_take_up_reads_again_what_differs_in_one_read() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let (gets, batched_gets) = (hooked.gets.clone(), hooked.batched_gets.clone());
        let a = on_postgres(&database, Box::new(hooked));
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let uids = [0x64, 0x65, 0x66].map(|byte| InstanceUid([byte; 16]));
        for uid in &uids {
            a.process(report(uid, 1), Transport::Http, None);
            label(&b, uid, &[("tier", "edge")]);
        }
        let (gets_before, batched_before) = (
            gets.load(Ordering::SeqCst),
            batched_gets.load(Ordering::SeqCst),
        );

        a.take_up_agents(Some(&HashSet::from(uids)))
            .expect("take up");
        for uid in &uids {
            assert_eq!(view(&a, uid).labels["tier"], "edge", "{uid} taken up");
        }
        assert_eq!(
            gets.load(Ordering::SeqCst) - gets_before,
            3,
            "one read of each before the lock"
        );
        assert_eq!(
            batched_gets.load(Ordering::SeqCst) - batched_before,
            1,
            "and one read of all three under it"
        );
    }

    // ---- The list of Agents, a page at a time, alike on every store ----

    /// A full report from `uid` over a certificate naming `host`, at sequence number `seq`.
    #[allow(clippy::too_many_arguments)]
    fn report_listed(
        state: &AppState,
        host: &str,
        subject: crate::revocation::SubjectValues,
        uid: InstanceUid,
        seq: u64,
        capabilities: u64,
        identifying: &[(&str, &str)],
        non_identifying: &[(&str, &str)],
    ) {
        let pairs = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(key, value)| opamp::attributes::string_attr(key, value))
                .collect()
        };
        let message = AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            sequence_num: seq,
            capabilities,
            agent_description: Some(AgentDescription {
                identifying_attributes: pairs(identifying),
                non_identifying_attributes: pairs(non_identifying),
            }),
            ..Default::default()
        };
        state.process_presented(
            message,
            Transport::Http,
            None,
            Some(&Presented {
                id: CertId::new(b"CA", host),
                host: Some(host.to_string()),
                subject_values: subject,
                not_after_ms: u64::MAX,
            }),
        );
    }

    const C1: InstanceUid = InstanceUid([0xa1; 16]);
    const A1: InstanceUid = InstanceUid([0xa2; 16]);
    const A2: InstanceUid = InstanceUid([0xa3; 16]);
    const CB: InstanceUid = InstanceUid([0xa5; 16]);
    const B1: InstanceUid = InstanceUid([0xa6; 16]);
    const B2: InstanceUid = InstanceUid([0xa7; 16]);

    /// A fleet of six on `state`, its clock at `now`: on Host `h1` (organization `acme`) a Client
    /// Agent labelled `ring=early` and an Agent `web-1`; on `h2` an Agent `web-2` reporting
    /// `env=prod` and labelled `ring=solo`; behind the Gateway `gw` a Client Agent of machine `m9`
    /// labelled `ring=late`, an Agent `db-1` of that machine, and an Agent of machine `m8`. `web-1`
    /// and `db-1` promise heartbeats and fall silent; `web-2` promises them and reports again.
    fn listed_fleet(state: &AppState, now: &AtomicU64) {
        let heartbeat = opamp::proto::AgentCapabilities::ReportsStatus as u64
            | opamp::proto::AgentCapabilities::ReportsHeartbeat as u64;
        let status = opamp::proto::AgentCapabilities::ReportsStatus as u64;
        let none = crate::revocation::SubjectValues::default;
        let client = [("supervisor.kind.otelcol", "true")];
        report_listed(
            state,
            "h1",
            organization("acme"),
            C1,
            5,
            status,
            &[("service.name", "fleet-agent")],
            &[("supervisor.kind.otelcol", "true"), ("host.name", "m1")],
        );
        report_listed(
            state,
            "h1",
            organization("acme"),
            A1,
            3,
            heartbeat,
            &[("service.name", "otelcol")],
            &[("service.instance.name", "web-1")],
        );
        report_listed(
            state,
            "h2",
            organization("other"),
            A2,
            9,
            heartbeat,
            &[("service.name", "otelcol")],
            &[("service.instance.name", "web-2"), ("env", "prod")],
        );
        report_listed(
            state,
            "gw",
            none(),
            CB,
            7,
            status,
            &[("service.name", "fleet-agent")],
            &[client[0], ("host.name", "m9")],
        );
        assert_eq!(state.set_gateway("gw", true), Ok(true));
        report_listed(
            state,
            "gw",
            none(),
            B1,
            1,
            heartbeat,
            &[("service.name", "otelcol")],
            &[("service.instance.name", "db-1"), ("host.name", "m9")],
        );
        report_listed(
            state,
            "gw",
            none(),
            B2,
            2,
            status,
            &[("service.name", "otelcol")],
            &[("host.name", "m8")],
        );
        label(state, &C1, &[("ring", "early")]);
        label(state, &A2, &[("ring", "solo")]);
        label(state, &CB, &[("ring", "late")]);
        now.fetch_add(600_000, Ordering::SeqCst);
        report_listed(
            state,
            "h2",
            organization("other"),
            A2,
            10,
            heartbeat,
            &[("service.name", "otelcol")],
            &[
                ("service.instance.name", "web-2"),
                ("env", "prod"),
                ("fresh", "yes"),
            ],
        );
    }

    /// The Agents `filter` admits in the order of `sort`, read a page of two at a time, and the
    /// count every page names.
    fn listed(
        state: &AppState,
        filter: crate::listing::AgentFilter,
        sort: crate::listing::AgentSort,
        descending: bool,
    ) -> Vec<InstanceUid> {
        let mut query = AgentQuery {
            filter,
            sort,
            descending,
            after: None,
            limit: 2,
        };
        let mut seen = Vec::new();
        let mut totals = BTreeSet::new();
        loop {
            let page = state.list_agents(&query).expect("a page");
            assert!(page.agents.len() <= 2);
            totals.insert(page.total);
            seen.extend(
                page.agents
                    .iter()
                    .map(|view| InstanceUid::parse(&view.instance_uid).expect("uid")),
            );
            match page.next {
                Some(next) => query.after = Some(next),
                None => break,
            }
        }
        assert_eq!(
            totals,
            BTreeSet::from([seen.len()]),
            "every page counts all"
        );
        seen
    }

    /// The same queries over the same fleet: filtered on what the UI searches, on a Selector that
    /// matches a Host value, a reported attribute, a label and a label inherited from a Client
    /// Agent, behind a Gateway too, on staleness, the type and the Host an Agent arrived over, and
    /// ordered by each key with ties broken by Instance UID, a page at a time.
    fn the_list_answers(state: &AppState) {
        use crate::listing::{AgentFilter, AgentSort, HolderFilter};
        let only = |filter: AgentFilter| {
            let mut found = listed(state, filter, AgentSort::Name, false);
            found.sort_by_key(|uid| uid.0);
            found
        };
        let selector = |pairs: &[(&str, &str)]| AgentFilter {
            selector: pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            ..AgentFilter::default()
        };
        let q = |text: &str| AgentFilter {
            q: Some(text.to_string()),
            ..AgentFilter::default()
        };
        assert_eq!(
            only(selector(&[("ring", "early")])),
            [C1, A1],
            "own and inherited"
        );
        assert_eq!(
            only(selector(&[("ring", "late")])),
            [CB, B1],
            "behind a Gateway"
        );
        assert_eq!(only(selector(&[("ring", "solo")])), [A2]);
        assert_eq!(
            only(selector(&[("fleet.host.organization", "acme")])),
            [C1, A1]
        );
        assert_eq!(only(selector(&[("env", "prod")])), [A2]);
        assert_eq!(
            only(selector(&[("host.name", "m9"), ("ring", "late")])),
            [CB, B1]
        );
        assert_eq!(only(selector(&[("env", "staging")])), []);
        assert_eq!(only(q("ring=late")), [CB, B1]);
        assert_eq!(only(q("acme")), [C1, A1], "the Host's values");
        assert_eq!(only(q("web-")), [A1, A2]);
        assert_eq!(only(q("fresh=yes")), [A2]);
        assert_eq!(
            only(AgentFilter {
                service_name: Some("otelcol".to_string()),
                ..AgentFilter::default()
            }),
            [A1, A2, B1, B2]
        );
        assert_eq!(
            only(AgentFilter {
                stale: Some(true),
                ..AgentFilter::default()
            }),
            [A1, B1]
        );
        assert_eq!(
            only(AgentFilter {
                arrived_over: Some("gw".to_string()),
                ..AgentFilter::default()
            }),
            [CB, B1, B2]
        );
        assert_eq!(
            only(AgentFilter {
                holder: Some(HolderFilter::Instance("elsewhere".to_string())),
                ..AgentFilter::default()
            }),
            []
        );
        assert_eq!(
            only(AgentFilter {
                holder: Some(HolderFilter::None),
                ..AgentFilter::default()
            }),
            [C1, A1, A2, CB, B1, B2]
        );
        let all = AgentFilter::default;
        assert_eq!(
            listed(state, all(), AgentSort::Name, false),
            [B1, C1, CB, B2, A1, A2],
            "db-1, fleet-agent twice by Instance UID, otelcol, web-1, web-2"
        );
        assert_eq!(
            listed(state, all(), AgentSort::Name, true),
            [A2, A1, B2, C1, CB, B1],
            "a tie keeps its Instance UIDs in their order"
        );
        assert_eq!(
            listed(state, all(), AgentSort::Seq, false),
            [B1, B2, A1, C1, CB, A2]
        );
        assert_eq!(
            listed(state, all(), AgentSort::Seen, true)[0],
            A2,
            "heard last"
        );
        let views: BTreeMap<String, AgentView> = state
            .list_agents(&AgentQuery {
                limit: 10,
                ..AgentQuery::default()
            })
            .expect("all")
            .agents
            .into_iter()
            .map(|view| (view.instance_uid.clone(), view))
            .collect();
        let b1 = &views[&B1.to_string()];
        assert_eq!(
            b1.inherited_labels.get("ring").map(String::as_str),
            Some("late")
        );
        assert_eq!(b1.client_agent, CB.to_string());
        assert_eq!(views[&A1.to_string()].client_agent, C1.to_string());
        assert!(views[&B2.to_string()].client_agent.is_empty());
        assert!(views
            .values()
            .all(|view| view.holder.is_empty() && view.holder_host.is_empty()));
        let values = state.reported_values().expect("values");
        assert_eq!(
            values.pairs,
            [
                "env=prod",
                "fresh=yes",
                "host.name=m1",
                "host.name=m8",
                "host.name=m9",
                "service.instance.name=db-1",
                "service.instance.name=web-1",
                "service.instance.name=web-2",
                "service.name=fleet-agent",
                "service.name=otelcol",
                "supervisor.kind.otelcol=true",
            ]
        );
        assert_eq!(values.service_names, ["fleet-agent", "otelcol"]);
    }

    /// A fleet whose clock is `now`, its host register and its records on files in `dir`.
    fn listed_on_files(dir: &std::path::Path, now: &Arc<AtomicU64>) -> AppState {
        let revocations = Revocations::open(
            Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations");
        AppState::with_stores(
            Box::new(crate::fs::FsAgentStore::open(dir).expect("agents")),
            Box::new(crate::fs::FsConfigBackend::open(dir).expect("configs")),
            Box::new(Manual(now.clone())),
        )
        .expect("state")
        .with_revocations(Some(Arc::new(revocations)))
    }

    /// The list of Agents on the filesystem store, cut from memory.
    /// Verifies: ADR-0031
    #[test]
    fn the_list_of_agents_filters_sorts_and_pages_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = Arc::new(AtomicU64::new(1_000_000_000));
        let state = listed_on_files(dir.path(), &now);
        listed_fleet(&state, &now);
        the_list_answers(&state);
    }

    /// The list of Agents on PostgreSQL, read through the port's `query_page`, answers what the
    /// filesystem store answers; a row the release before wrote, without its derived columns, is
    /// derived again before it is read; and a label set on a Client Agent writes that one row and
    /// narrows a Selector to the Agents that inherit it.
    /// Verifies: ADR-0031
    #[test]
    fn the_list_of_agents_filters_sorts_and_pages_alike_on_postgres() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let now = Arc::new(AtomicU64::new(1_000_000_000));
        let revocations = Revocations::open(
            Box::new(crate::postgres::ledger::PgLedgerStore::new(
                database.clone(),
            )),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations");
        let state = AppState::with_stores(
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
            Box::new(crate::postgres::configs::PgConfigBackend::new(
                database.clone(),
            )),
            Box::new(Manual(now.clone())),
        )
        .expect("state")
        .with_revocations(Some(Arc::new(revocations)));
        listed_fleet(&state, &now);
        assert_eq!(
            test.once(&format!(
                "SELECT count(*)::text FROM agents WHERE derived_from IS DISTINCT FROM {}",
                crate::postgres::schema::stamp!()
            )),
            "0",
            "every write wrote the derived columns with the record"
        );
        the_list_answers(&state);

        // As a Server of 0.8.0 leaves a row: its derived columns as they were, or none at all.
        test.sql("UPDATE agents SET derived_from = NULL, display_name = '', search_text = '', reported = '{}', machine = '', client_agent = false");
        let changed = AgentDescription {
            identifying_attributes: vec![opamp::attributes::string_attr("service.name", "otelcol")],
            non_identifying_attributes: vec![
                opamp::attributes::string_attr("service.instance.name", "web-2"),
                opamp::attributes::string_attr("env", "staging"),
                opamp::attributes::string_attr("fresh", "yes"),
            ],
        };
        test.sql(&format!(
            "UPDATE agents SET description = '\\x{}'::bytea WHERE instance_uid = '{A2}'",
            hex::encode(changed.encode_to_vec())
        ));
        assert!(
            state
                .reported_values()
                .expect("values")
                .pairs
                .contains(&"env=staging".to_string()),
            "the values the release before wrote are derived before they are read"
        );
        let staging = crate::listing::AgentFilter {
            selector: BTreeMap::from([("env".to_string(), "staging".to_string())]),
            ..crate::listing::AgentFilter::default()
        };
        assert_eq!(
            listed(&state, staging, crate::listing::AgentSort::Name, false),
            [A2],
            "what the release before wrote is read as its record says"
        );
        test.sql(&format!(
            "UPDATE agents SET description = '\\x{}'::bytea WHERE instance_uid = '{A2}'",
            hex::encode(
                AgentDescription {
                    identifying_attributes: vec![opamp::attributes::string_attr(
                        "service.name",
                        "otelcol"
                    )],
                    non_identifying_attributes: vec![
                        opamp::attributes::string_attr("service.instance.name", "web-2"),
                        opamp::attributes::string_attr("env", "prod"),
                        opamp::attributes::string_attr("fresh", "yes"),
                    ],
                }
                .encode_to_vec()
            )
        ));
        the_list_answers(&state);

        // A display name the database sorts by that its record would derive otherwise, as a row
        // written while the derivation changed: the cursor continues after the value sorted by,
        // so one Agent a page still shows each Agent once.
        test.sql(&format!(
            "UPDATE agents SET display_name = 'b' WHERE instance_uid = '{C1}'"
        ));
        let mut query = AgentQuery {
            limit: 1,
            ..AgentQuery::default()
        };
        let mut seen = Vec::new();
        loop {
            let page = state.list_agents(&query).expect("a page");
            seen.extend(page.agents.iter().map(|view| view.instance_uid.clone()));
            match page.next {
                Some(next) => query.after = Some(next),
                None => break,
            }
        }
        assert_eq!(
            seen,
            [C1, B1, CB, B2, A1, A2].map(|uid| uid.to_string()),
            "by the name stored: b, db-1, fleet-agent, otelcol, web-1, web-2"
        );

        // The read of the Agents behind a Gateway reads no derived column, so it derives none.
        test.sql("UPDATE agents SET derived_from = NULL");
        let behind = state.arrived_over("gw").expect("behind the Gateway");
        let mut behind: Vec<InstanceUid> = behind.into_keys().collect();
        behind.sort_by_key(|uid| uid.0);
        assert_eq!(behind, [CB, B1, B2]);
        assert_eq!(
            test.once("SELECT count(*)::text FROM agents WHERE derived_from IS NOT NULL"),
            "0",
            "nothing was derived again"
        );
        state
            .list_agents(&AgentQuery {
                limit: 1,
                ..AgentQuery::default()
            })
            .expect("a list derives them");

        let written = |uid: &InstanceUid| {
            test.once(&format!(
                "SELECT xmin::text FROM agents WHERE instance_uid = '{uid}'"
            ))
        };
        let before = written(&A1);
        label(&state, &C1, &[("ring", "mid")]);
        let mid = crate::listing::AgentFilter {
            selector: BTreeMap::from([("ring".to_string(), "mid".to_string())]),
            ..crate::listing::AgentFilter::default()
        };
        let mut found = listed(&state, mid, crate::listing::AgentSort::Name, false);
        found.sort_by_key(|uid| uid.0);
        assert_eq!(found, [C1, A1]);
        assert_eq!(
            written(&A1),
            before,
            "no row but the Client Agent's was written"
        );
    }

    /// On PostgreSQL the check of a Gateway's package download, and the view of an Agent behind
    /// it, read the Agents that arrived over the Gateway's Host through the port's `query_page`,
    /// not the whole fleet, and still find the Client Agent behind the Gateway whose labels the
    /// Agent inherits.
    /// Verifies: ADR-0031, ADR-0034
    #[test]
    fn a_gateways_download_and_an_agent_behind_it_read_the_gateways_agents_and_not_the_fleet() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let dir = tempfile::tempdir().expect("blobs");
        let a = delivering_on(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
            dir.path(),
        );
        let (client, agent, other) = (
            InstanceUid([0xb1; 16]),
            InstanceUid([0xb2; 16]),
            InstanceUid([0xb3; 16]),
        );
        let none = crate::revocation::SubjectValues::default;
        let takes = opamp::proto::AgentCapabilities::ReportsStatus as u64
            | opamp::proto::AgentCapabilities::AcceptsPackages as u64
            | opamp::proto::AgentCapabilities::ReportsPackageStatuses as u64;
        report_listed(
            &a,
            "gw",
            none(),
            client,
            1,
            takes,
            &[("service.name", "fleet-agent")],
            &[("supervisor.kind.otelcol", "true"), ("host.name", "m1")],
        );
        assert_eq!(a.set_gateway("gw", true), Ok(true));
        report_listed(
            &a,
            "gw",
            none(),
            agent,
            1,
            takes,
            &[("service.name", "otelcol")],
            &[
                ("os.type", "linux"),
                ("host.arch", "amd64"),
                ("host.name", "m1"),
            ],
        );
        report_listed(
            &a,
            "h2",
            none(),
            other,
            1,
            takes,
            &[("service.name", "otelcol")],
            &[("os.type", "linux"), ("host.arch", "amd64")],
        );
        label(&a, &client, &[("ring", "early")]);
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        let linux = Platform::new("linux", "amd64").expect("platform");
        a.create_package_set(&id).expect("set");
        a.put_package_entry(&id, &linux, &mut &b"v1"[..])
            .expect("entry");
        a.put_package_entry_signature(&id, &linux, vec![1; 64])
            .expect("signature");
        a.deployment_store()
            .expect("deployments")
            .put(
                "early",
                BTreeMap::from([("ring".to_string(), "early".to_string())]),
            )
            .expect("deployment");
        a.put_deployment_package("early", &id, true)
            .expect("hold it");
        assert_eq!(
            a.rollout_deployment("early").expect("rollout"),
            1,
            "the inheriting Agent"
        );

        let hooked = Hooked::new(Box::new(
            crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open"),
        ));
        let whole = hooked.whole_queries.clone();
        let b = delivering_on(&database, Box::new(hooked), dir.path());
        assert!(
            b.offers_artifact("gw", &id, &linux),
            "the Gateway fetches what its Agent was released"
        );
        assert!(
            !b.offers_artifact("h2", &id, &linux),
            "nothing was released over h2"
        );
        let view = b.agent_view(&agent).expect("read").expect("known");
        assert_eq!(
            view.inherited_labels.get("ring").map(String::as_str),
            Some("early")
        );
        assert_eq!(view.client_agent, client.to_string());
        assert_eq!(
            whole.load(Ordering::SeqCst),
            0,
            "no read of the whole fleet"
        );
    }

    /// The hooks of A's store that the presence tests use.
    struct PresenceHooks {
        losing_takes: Arc<std::sync::atomic::AtomicBool>,
        failing_takes: Arc<std::sync::atomic::AtomicBool>,
        after_presence: Arc<Mutex<Option<Act>>>,
        before_takes: Arc<Mutex<Option<Act>>>,
        after_takes: Arc<Mutex<Option<Act>>>,
        takes_reads: Arc<AtomicU64>,
    }

    const LEASE_A: &str = "00000000-0000-7000-8000-00000000000a";
    const LEASE_B: &str = "00000000-0000-7000-8000-00000000000b";

    /// Instance A on `database`, its store hooked, and B, serving under `LEASE_A` and `LEASE_B`;
    /// neither lease is in the database, so neither is live.
    fn hooked_pair(
        database: &Arc<crate::postgres::Database>,
    ) -> (Arc<AppState>, Arc<AppState>, PresenceHooks) {
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let hooks = PresenceHooks {
            losing_takes: hooked.losing_takes.clone(),
            failing_takes: hooked.failing_takes.clone(),
            after_presence: hooked.after_presence.clone(),
            before_takes: hooked.before_takes.clone(),
            after_takes: hooked.after_takes.clone(),
            takes_reads: hooked.takes_reads.clone(),
        };
        let a = Arc::new(on_postgres(database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let b = Arc::new(on_postgres(
            database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        ));
        b.serve_under(tokio::sync::watch::channel(Some(LEASE_B.to_string())).1);
        (a, b, hooks)
    }

    /// The stored holder (`none` where it names none), `connected` and the count of takes of `uid`.
    fn stored_presence(test: &crate::postgres::testing::TestDatabase, uid: &InstanceUid) -> String {
        test.rows(&format!(
            "SELECT coalesce(holder::text, 'none') || ' ' || connected || ' ' || takes \
             FROM agents WHERE instance_uid = '{uid}'"
        ))
        .into_iter()
        .flatten()
        .flatten()
        .collect()
    }

    /// Waits for the system clock to reach its next millisecond, so a report after it moves the
    /// presence an instance last wrote.
    fn next_millisecond() {
        let now = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis()
        };
        let start = now();
        while now() == start {
            std::hint::spin_loop();
        }
    }

    /// An Agent whose connecting take was written and whose answer was lost, and that disconnects
    /// before the next batch, has its row name no holder after that batch: the batch reads the
    /// row's holder and count first and writes the disconnect under the count it read, once.
    /// Verifies: ADR-0025
    #[test]
    fn the_goodbye_of_an_agent_whose_take_lost_its_answer_is_written() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x75; 16]);
        hooks.losing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        hooks.losing_takes.store(false, Ordering::SeqCst);
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 1"));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        assert_eq!(hooks.takes_reads.load(Ordering::SeqCst), 1, "read once");
        a.write_presence(LEASE_A).expect("A's next batch");
        assert_eq!(hooks.takes_reads.load(Ordering::SeqCst), 1, "and no more");
    }

    /// Of two Agents whose takes lost their answers, one B took since is not written by A's batch,
    /// and one A took again after the batch first looked at it and before it read the rows is left
    /// as that take wrote it.
    /// Verifies: ADR-0025
    #[test]
    fn a_lost_take_whose_row_was_taken_since_writes_no_goodbye() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let (moved, back) = (InstanceUid([0x76; 16]), InstanceUid([0x77; 16]));
        hooks.losing_takes.store(true, Ordering::SeqCst);
        a.process(report(&moved, 1), Transport::WebSocket, Some(7));
        a.process(report(&back, 1), Transport::WebSocket, Some(8));
        hooks.losing_takes.store(false, Ordering::SeqCst);
        b.process(report(&moved, 2), Transport::WebSocket, Some(9));
        a.mark_disconnected(&[moved], 7);
        a.mark_disconnected(&[back], 8);
        let again = a.clone();
        *hooks.before_takes.lock().expect("hook lock") = Some(Box::new(move || {
            again.process(report(&back, 2), Transport::WebSocket, Some(10));
        }));
        a.write_presence(LEASE_A).expect("A's batch");
        assert!(
            hooks.before_takes.lock().expect("hook lock").is_none(),
            "the take came before the read"
        );
        assert_eq!(
            stored_presence(&test, &moved),
            format!("{LEASE_B} true 2"),
            "B's take stays"
        );
        assert_eq!(
            stored_presence(&test, &back),
            format!("{LEASE_A} true 2"),
            "A's new take stays"
        );
    }

    /// An Agent re-keyed on A that disconnects before A's next batch has the row under its new
    /// identity name no holder after that batch: what A wrote of its presence moves with it.
    /// Verifies: ADR-0025
    #[test]
    fn the_goodbye_of_an_agent_rekeyed_is_written_under_its_new_identity() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, _hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x78; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process(rekey, Transport::WebSocket, Some(7));
        let new = answered.uid.expect("carried");
        assert_ne!(new, uid, "re-keyed");
        assert_eq!(stored_presence(&test, &new), format!("{LEASE_A} true 1"));
        a.mark_disconnected(&[new], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &new), "none false 1");
    }

    /// Waits until `done`, and fails naming `what` after 30 seconds without.
    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// How long the one call whose answer a test loses waits for it: short of the five seconds of
    /// `lock_timeout` a read back that follows it waits for the row, and long enough for a
    /// database slowed by the tests beside it.
    const LOST_WITHIN: Duration = Duration::from_secs(3);

    /// Runs `act` with every call on `database` bounded by [`LOST_WITHIN`], and the usual bound
    /// again after it.
    fn losing_the_answer_of<T>(database: &crate::postgres::Database, act: impl FnOnce() -> T) -> T {
        database.bound_calls_by(LOST_WITHIN);
        let answered = act();
        database.bound_calls_by(crate::postgres::CALL_TIMEOUT);
        answered
    }

    /// Has the pool of `database` hold three idle connections, so the call after one that lost its
    /// answer is not queued behind that call's commit on its connection.
    fn spare_connections(database: &crate::postgres::Database) {
        database
            .run(|pool| async move {
                let held = (
                    crate::postgres::client(&pool).await?,
                    crate::postgres::client(&pool).await?,
                    crate::postgres::client(&pool).await?,
                );
                drop(held);
                Ok(())
            })
            .expect("three connections");
    }

    /// Instance A on a database of the usual bound, with floors and a revocation list of a
    /// `client` CA, following the notifications on a runtime of its own; it has read everything
    /// once. The one call whose answer a test loses is bounded short ([`LOST_WITHIN`]).
    struct LosingAnswers {
        a: Arc<AppState>,
        database: Arc<crate::postgres::Database>,
        following: crate::instance::Following,
        _runtime: tokio::runtime::Runtime,
    }

    impl LosingAnswers {
        fn on(test: &crate::postgres::testing::TestDatabase) -> Self {
            let database = test.shared();
            let records = crate::Records::Postgres(database.clone());
            let revocations = Revocations::open(
                records.ledger().expect("ledger"),
                Arc::new(crate::clock::SystemClock),
                vec![crate::revocation::Authority {
                    role: "client".to_string(),
                    subject: "ab".repeat(32),
                    name: "CN=test".to_string(),
                }],
            )
            .expect("revocations");
            let a = Arc::new(
                on_postgres(
                    &database,
                    Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
                )
                .with_revocations(Some(Arc::new(revocations)))
                .with_floors(Some(Arc::new(
                    crate::floors::PackageFloors::open(records.floors()).expect("floors"),
                ))),
            );
            a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            let following = {
                let _entered = runtime.enter();
                crate::instance::follow(&database, a.clone(), None)
            };
            until("the first read once listening", || following.reads() > 0);
            spare_connections(&database);
            LosingAnswers {
                a,
                database,
                following,
                _runtime: runtime,
            }
        }

        /// Runs `act`, whose commit `gate` holds, and expects it to fail on its bound; then waits
        /// for the read it calls for, finds `taken_up` not yet, opens the gate and waits for it.
        fn lose_the_answer_of<T: std::fmt::Debug>(
            &self,
            gate: crate::postgres::testing::Gate,
            act: impl FnOnce(&AppState) -> Result<T, String>,
            taken_up: impl Fn(&AppState) -> bool,
        ) {
            let reads = self.following.reads();
            let failed = losing_the_answer_of(&self.database, || act(&self.a))
                .expect_err("the call's answer is lost to its bound");
            assert!(failed.contains("no answer within"), "{failed}");
            until("the read the lost answer calls for", || {
                self.following.reads() > reads
            });
            assert!(!taken_up(&self.a), "the commit is held yet");
            gate.open();
            until("the commit taken up once it landed", || taken_up(&self.a));
        }
    }

    /// A label set whose commit lands after its call lost its answer is held in the memory of the
    /// instance that carries the Agent once it lands, though its own notification is otherwise
    /// passed over.
    /// Verifies: ADR-0025
    #[test]
    fn a_label_whose_answer_was_lost_is_taken_up_once_it_commits() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let losing = LosingAnswers::on(&test);
        let uid = InstanceUid([0x28; 16]);
        losing
            .a
            .process(report(&uid, 1), Transport::WebSocket, Some(7));
        let gate = test.hold_commits("agents", 0x28_0001);
        losing.lose_the_answer_of(
            gate,
            |a| match a.set_labels(&uid, BTreeMap::from([("site".into(), "linz".into())])) {
                Ok(()) => Ok(()),
                Err(LabelError::Storage(e)) => Err(e),
                Err(_) => Err("refused".to_string()),
            },
            |a| {
                a.fleet
                    .lock()
                    .expect("fleet lock")
                    .get(&uid)
                    .map(|r| r.labels.clone())
                    == Some(BTreeMap::from([("site".into(), "linz".into())]))
            },
        );
    }

    /// A floor set whose commit lands after its call lost its answer is served by the instance
    /// that set it once it lands.
    /// Verifies: ADR-0025
    #[test]
    fn a_floor_whose_answer_was_lost_is_taken_up_once_it_commits() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let losing = LosingAnswers::on(&test);
        let gate = test.hold_commits("package_floors", 0x28_0002);
        losing.lose_the_answer_of(
            gate,
            |a| {
                a.floors()
                    .expect("floors")
                    .put("otelcol", "1.2.0", vec![7; 64])
                    .map_err(|e| format!("{e:?}"))
            },
            |a| {
                a.floors()
                    .expect("floors")
                    .all()
                    .get("otelcol")
                    .map(|floor| floor.version.clone())
                    .as_deref()
                    == Some("1.2.0")
            },
        );
    }

    /// A revocation whose commit lands after its call lost its answer is in the list of the
    /// instance that made it once it lands, renewals aside.
    /// Verifies: ADR-0025
    #[test]
    fn a_revocation_whose_answer_was_lost_is_taken_up_once_it_commits() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let losing = LosingAnswers::on(&test);
        let gate = test.hold_commits("revocations", 0x28_0003);
        losing.lose_the_answer_of(
            gate,
            |a| {
                a.revocations()
                    .expect("revocations")
                    .revoke_certificate("client", "0a1b")
                    .map_err(|e| format!("{e:?}"))
            },
            |a| a.revocations().expect("revocations").list().len() == 1,
        );
    }

    /// A re-key whose commit lands after its call lost its answer is read back under a lock that
    /// waits for it: the Agent is assigned the identity its record moved to, one row names it,
    /// under that identity, and no record is created for the old one when it reports again.
    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_whose_answer_was_lost_is_read_back_and_stands() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let losing = LosingAnswers::on(&test);
        let (a, uid) = (losing.a.clone(), InstanceUid([0x29; 16]));
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        let gate = test.hold_commits("agents", 0x28_0004);
        let (answered, answer) = std::sync::mpsc::channel();
        let rekeying = std::thread::spawn({
            let (a, database) = (a.clone(), losing.database.clone());
            move || {
                let mut rekey = report(&uid, 2);
                rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
                let _ = answered.send(losing_the_answer_of(&database, || {
                    a.process(rekey, Transport::WebSocket, Some(7))
                }));
            }
        });
        // Its commit is held; once the call lost its answer, its read back waits for the row.
        until("the read back waiting for the move, or the answer", || {
            rekeying.is_finished() || test.sessions("wait_event IN ('transactionid', 'tuple')") > 0
        });
        gate.open();
        let processed = answer
            .recv_timeout(Duration::from_secs(30))
            .expect("the re-key answers");
        rekeying.join().expect("the re-key's thread");
        let new = processed.uid.expect("carried");
        assert_ne!(new, uid, "re-keyed");
        assert_eq!(
            processed
                .reply
                .agent_identification
                .as_ref()
                .map(|id| id.new_instance_uid.clone()),
            Some(new.as_bytes().to_vec()),
            "the identity it moved to is assigned"
        );
        let stored = || -> Vec<String> {
            test.rows("SELECT instance_uid FROM agents")
                .into_iter()
                .flatten()
                .flatten()
                .collect()
        };
        assert_eq!(
            stored(),
            [new.to_string()],
            "one row, under the new identity"
        );
        a.process(report(&new, 3), Transport::WebSocket, Some(7));
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored(),
            [new.to_string()],
            "and still one after its next report"
        );
        let held: Vec<String> = a.snapshot().into_iter().map(|v| v.instance_uid).collect();
        assert_eq!(
            held,
            [new.to_string()],
            "A holds it under the new identity alone"
        );
    }

    /// A re-key whose answer was lost and whose read back failed is read back at the Agent's next
    /// report, whether it asks again or not, and assigns the identity the record moved to; no other
    /// is minted and no record is created for the old one.
    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_whose_read_back_failed_is_read_back_at_the_next_report() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let (losing, failing) = (hooked.losing_rekeys.clone(), hooked.failing_moved.clone());
        let a = on_postgres(&database, Box::new(hooked));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x2a; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        losing.store(true, Ordering::SeqCst);
        failing.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let first = a.process(rekey, Transport::WebSocket, Some(7));
        losing.store(false, Ordering::SeqCst);
        assert_eq!(first.uid, Some(uid), "not known: it keeps its identity");
        assert!(first.reply.agent_identification.is_none());
        let stored = || -> Vec<String> {
            test.rows("SELECT instance_uid FROM agents")
                .into_iter()
                .flatten()
                .flatten()
                .collect()
        };
        let moved_to = stored();
        assert_eq!(moved_to.len(), 1);
        assert_ne!(moved_to[0], uid.to_string(), "the move committed");
        let unknown = a.process(report(&uid, 3), Transport::WebSocket, Some(7));
        assert_eq!(unknown.uid, Some(uid), "not known still");
        // Held no more, as after a restart of what is held: a report under the old identity is
        // refused while the move is not known, and creates nothing.
        a.fleet.lock().expect("fleet lock").remove(&uid);
        let refused = a.process(report(&uid, 4), Transport::WebSocket, Some(7));
        assert_eq!(refused.uid, None, "not carried");
        assert_eq!(
            refused.reply.error_response.as_ref().map(|e| e.r#type),
            Some(ServerErrorResponseType::Unavailable as i32),
            "refused for now"
        );
        assert_eq!(stored(), moved_to, "no record for the old identity");
        failing.store(false, Ordering::SeqCst);
        let next = a.process(report(&uid, 5), Transport::WebSocket, Some(7));
        let new = next.uid.expect("carried");
        assert_eq!(new.to_string(), moved_to[0], "the identity it moved to");
        assert_eq!(
            next.reply
                .agent_identification
                .as_ref()
                .map(|id| id.new_instance_uid.clone()),
            Some(new.as_bytes().to_vec())
        );
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored(),
            moved_to,
            "one row, under the identity it moved to"
        );
        let held: Vec<String> = a.snapshot().into_iter().map(|v| v.instance_uid).collect();
        assert_eq!(held, moved_to, "A holds it under that identity alone");
    }

    /// A re-key whose answer was lost and whose read back failed, whose commit lands after that:
    /// its own notification that the old identity is forgotten reads the re-key back, and what A
    /// holds moves to the new identity, the Agent's session not ended. A disconnect on that session
    /// under the old identity is written to the new row, and a report under the old identity, on a
    /// new connection, is assigned the new one; one row names the Agent throughout.
    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_whose_read_back_failed_is_taken_up_when_its_commit_lands() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.shared();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_moved.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let following = {
            let _entered = runtime.enter();
            crate::instance::follow(&database, a.clone(), None)
        };
        until("the first read once listening", || following.reads() > 0);
        spare_connections(&database);
        let uid = InstanceUid([0x2c; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.write_presence(LEASE_A).expect("A's batch");
        let gate = test.hold_commits("agents", 0x28_0007);
        failing.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = losing_the_answer_of(&database, || {
            a.process(rekey, Transport::WebSocket, Some(7))
        });
        assert_eq!(answered.uid, Some(uid), "not known: it keeps its identity");
        assert!(answered.reply.agent_identification.is_none());
        failing.store(false, Ordering::SeqCst);
        gate.open();
        let stored = || -> Vec<String> {
            test.rows("SELECT instance_uid FROM agents")
                .into_iter()
                .flatten()
                .flatten()
                .collect()
        };
        until("the move landed", || {
            stored().first().is_some_and(|row| *row != uid.to_string())
        });
        let new = InstanceUid::parse(&stored()[0]).expect("a uid");
        let held = || -> Vec<String> { a.snapshot().into_iter().map(|v| v.instance_uid).collect() };
        until("A holds it under the new identity", || {
            held() == [new.to_string()]
        });
        assert!(a.take_forgotten(7).is_empty(), "the session is not ended");
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert!(
            stored_presence(&test, &new).starts_with("none false"),
            "the disconnect is written to the new row: {}",
            stored_presence(&test, &new)
        );
        let next = a.process(report(&uid, 3), Transport::WebSocket, Some(8));
        assert_eq!(next.uid, Some(new), "assigned the identity it moved to");
        assert_eq!(
            next.reply
                .agent_identification
                .as_ref()
                .map(|id| id.new_instance_uid.clone()),
            Some(new.as_bytes().to_vec())
        );
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored(), [new.to_string()], "one row throughout");
        assert_eq!(held(), [new.to_string()]);
    }

    /// Instance A, its store hooked, following on a runtime of its own, and an Agent it carries on
    /// connection 7 whose re-key lost its answer and whose read back failed; the move has
    /// committed since, and its own notification's read back failed too, so A still holds the
    /// Agent under its old identity. Answers A, the switch that fails the read backs, the old and
    /// the new identity, and what keeps the instance running.
    fn rekey_unknown_after_it_landed(
        test: &crate::postgres::testing::TestDatabase,
    ) -> (
        Arc<AppState>,
        Arc<std::sync::atomic::AtomicBool>,
        InstanceUid,
        InstanceUid,
        tokio::runtime::Runtime,
    ) {
        let database = test.shared();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let (failing, reads) = (hooked.failing_moved.clone(), hooked.moved_reads.clone());
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let following = {
            let _entered = runtime.enter();
            crate::instance::follow(&database, a.clone(), None)
        };
        until("the first read once listening", || following.reads() > 0);
        spare_connections(&database);
        let uid = InstanceUid([0x2d; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.write_presence(LEASE_A).expect("A's batch");
        let gate = test.hold_commits("agents", 0x28_0008);
        failing.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = losing_the_answer_of(&database, || {
            a.process(rekey, Transport::WebSocket, Some(7))
        });
        assert_eq!(answered.uid, Some(uid), "not known: it keeps its identity");
        let asked = reads.load(Ordering::SeqCst);
        gate.open();
        let rows = || -> Vec<String> {
            test.rows("SELECT instance_uid FROM agents")
                .into_iter()
                .flatten()
                .flatten()
                .collect()
        };
        until("the move landed", || {
            rows().first().is_some_and(|row| *row != uid.to_string())
        });
        until("its notification's read back tried", || {
            reads.load(Ordering::SeqCst) > asked
        });
        let new = InstanceUid::parse(&rows()[0]).expect("a uid");
        (a, failing, uid, new, runtime)
    }

    /// The held identities of `a`.
    fn held_by(a: &AppState) -> Vec<String> {
        a.snapshot().into_iter().map(|v| v.instance_uid).collect()
    }

    /// A re-key whose read back fails at its own notification too keeps the Agent and its session
    /// under the old identity, and is read back again until it holds: a session that ended
    /// meanwhile has its disconnect written to the row the move landed in, though a batch wrote
    /// it to the old row before, and the next report
    /// under the old identity is assigned the new one.
    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_whose_read_back_fails_at_its_notification_keeps_the_agent_until_it_holds() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let (a, failing, uid, new, _runtime) = rekey_unknown_after_it_landed(&test);
        assert_eq!(
            held_by(&a),
            [uid.to_string()],
            "kept under the old identity"
        );
        assert!(a.take_forgotten(7).is_empty(), "the session is not ended");
        a.mark_disconnected(&[uid], 7);
        // A batch while the move is not known writes the disconnect to the old row, which is gone.
        a.write_presence(LEASE_A).expect("A's batch");
        failing.store(false, Ordering::SeqCst);
        until("read back again until it held", || {
            held_by(&a) == [new.to_string()]
        });
        a.write_presence(LEASE_A).expect("A's batch");
        assert!(
            stored_presence(&test, &new).starts_with("none false"),
            "the disconnect is written to the new row: {}",
            stored_presence(&test, &new)
        );
        let next = a.process(report(&uid, 3), Transport::WebSocket, Some(8));
        assert_eq!(next.uid, Some(new), "assigned the identity it moved to");
        let rows: Vec<String> = test
            .rows("SELECT instance_uid FROM agents")
            .into_iter()
            .flatten()
            .flatten()
            .collect();
        assert_eq!(rows, [new.to_string()], "one row throughout");
    }

    /// A report whose write finds the old row gone, while the re-key that moved it is not known,
    /// keeps the Agent and its session and the pending re-key, and creates nothing; once the
    /// store answers, the next report is assigned the new identity.
    /// Verifies: ADR-0025
    #[test]
    fn a_report_that_finds_the_old_row_gone_keeps_a_rekey_not_known() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let (a, failing, uid, new, _runtime) = rekey_unknown_after_it_landed(&test);
        let mut moved = report(&uid, 3);
        moved.agent_description = Some(AgentDescription::default());
        let answered = a.process(moved, Transport::WebSocket, Some(7));
        assert_eq!(answered.uid, Some(uid), "the connection still carries it");
        assert_eq!(
            held_by(&a),
            [uid.to_string()],
            "kept under the old identity"
        );
        assert!(a.take_forgotten(7).is_empty(), "the session is not ended");
        failing.store(false, Ordering::SeqCst);
        let next = a.process(report(&uid, 4), Transport::WebSocket, Some(7));
        assert_eq!(next.uid, Some(new), "assigned the identity it moved to");
        a.write_presence(LEASE_A).expect("A's batch");
        let rows: Vec<String> = test
            .rows("SELECT instance_uid FROM agents")
            .into_iter()
            .flatten()
            .flatten()
            .collect();
        assert_eq!(rows, [new.to_string()], "one row, under the new identity");
        assert_eq!(held_by(&a), [new.to_string()]);
    }

    /// The read back of a take still in flight waits for it: an Agent whose take's answer was lost
    /// and that disconnected has its disconnect written under the count that take commits, so the
    /// row names no holder once the batch ran.
    /// Verifies: ADR-0025
    #[test]
    fn the_read_back_of_a_take_waits_for_a_take_still_in_flight() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x2b; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        // A takes it on a new connection, and the take's call gives up before the take is
        // written, which commits after A read its row back.
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        a.mark_disconnected(&[uid], 8);
        let gate = test.gate(0x28_0005);
        std::thread::scope(|scope| {
            let in_flight = scope.spawn(|| {
                test.sql(&format!(
                    "BEGIN; UPDATE agents SET connected = true, holder = '{LEASE_A}', \
                     takes = takes + 1 WHERE instance_uid = '{uid}'; \
                     SELECT pg_advisory_lock_shared({}); COMMIT;",
                    0x28_0005
                ));
            });
            until("the take holding the row", || test.waits_for(0x28_0005));
            let batch = scope.spawn(|| a.write_presence(LEASE_A));
            until("A's batch waiting for the take, or done", || {
                batch.is_finished() || test.sessions("wait_event IN ('transactionid', 'tuple')") > 0
            });
            gate.open();
            in_flight.join().expect("the take's thread");
            batch
                .join()
                .expect("the batch's thread")
                .expect("A's batch");
        });
        assert_eq!(stored_presence(&test, &uid), "none false 2");
    }

    /// A batch held between its commit and its answers, while the Agent is forgotten through B,
    /// comes back through B and reconnects to A, which takes it again, leaves the newer take's count kept, though
    /// the record counts from the start again and the batch answered a greater one: the Agent's
    /// goodbye is then written.
    /// Verifies: ADR-0025
    #[test]
    fn a_batch_answered_after_a_newer_take_keeps_that_takes_count() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x79; 16]);
        for conn in [1, 2] {
            a.process(report(&uid, conn), Transport::WebSocket, Some(conn));
            a.mark_disconnected(&[uid], conn);
        }
        a.process(report(&uid, 3), Transport::WebSocket, Some(3));
        next_millisecond();
        a.process(report(&uid, 4), Transport::WebSocket, Some(3));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 3"));
        let (again, through) = (a.clone(), b.clone());
        *hooks.after_presence.lock().expect("hook lock") = Some(Box::new(move || {
            assert!(through.forget_agent(&uid).is_ok(), "forgotten through B");
            through.process(report(&uid, 1), Transport::WebSocket, Some(5));
            again.mark_disconnected(&[uid], 3);
            again.process(report(&uid, 2), Transport::WebSocket, Some(6));
        }));
        a.write_presence(LEASE_A).expect("A's batch");
        assert!(
            hooks.after_presence.lock().expect("hook lock").is_none(),
            "the take came before the answers"
        );
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 2"));
        a.mark_disconnected(&[uid], 6);
        a.write_presence(LEASE_A).expect("A's next batch");
        assert_eq!(stored_presence(&test, &uid), "none false 2", "the goodbye");
    }

    /// An Agent that A took and that B took since does not name A after A's batch: neither once B
    /// wrote its disconnect, before A heard of the take, nor while B, whose lease is not live,
    /// still names itself; A stops carrying both when it hears of the takes.
    /// Verifies: ADR-0025
    #[test]
    fn a_batch_names_its_instance_over_no_take_counted_since_its_own() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, _hooks) = hooked_pair(&database);
        let (gone, dead) = (InstanceUid([0x7a; 16]), InstanceUid([0x7b; 16]));
        for (conn, uid) in [(7, gone), (8, dead)] {
            a.process(report(&uid, 1), Transport::WebSocket, Some(conn));
        }
        next_millisecond();
        // Heartbeats A has not written yet, then the Agents move to B.
        for (conn, uid) in [(7, gone), (8, dead)] {
            a.process(report(&uid, 2), Transport::WebSocket, Some(conn));
            b.process(report(&uid, 3), Transport::WebSocket, Some(9));
        }
        b.mark_disconnected(&[gone], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(stored_presence(&test, &gone), "none false 2");
        assert_eq!(stored_presence(&test, &dead), format!("{LEASE_B} true 2"));

        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored_presence(&test, &gone),
            "none false 2",
            "B's goodbye stays"
        );
        assert_eq!(
            stored_presence(&test, &dead),
            format!("{LEASE_B} true 2"),
            "B's take stays"
        );
        a.release_taken(None).expect("A hears of the takes");
        for uid in [gone, dead] {
            assert!(!view(&a, &uid).connected, "A no longer carries {uid}");
        }
    }

    /// An Agent that disconnected under a lease A then lost has its record flushed at A's
    /// shutdown, whether or not A wrote a batch under its new lease: the count of takes outlives
    /// the lease it was answered under.
    /// Verifies: ADR-0025
    #[test]
    fn an_agent_disconnected_under_a_lost_lease_is_flushed_at_shutdown() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let a = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let (lease, leases) = tokio::sync::watch::channel(Some(LEASE_A.to_string()));
        a.serve_under(leases);
        let uid = InstanceUid([0x7c; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        let renewed = "00000000-0000-7000-8000-0000000000aa";
        lease.send_replace(Some(renewed.to_string()));
        let sequence = || {
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            ))
        };
        let forget = || {
            test.sql(&format!(
                "UPDATE agents SET sequence_num = 0 WHERE instance_uid = '{uid}'"
            ));
        };

        forget();
        a.flush_agents();
        assert_eq!(
            sequence(),
            [[Some("2".to_string())]],
            "flushed before a batch"
        );
        forget();
        a.write_presence(renewed)
            .expect("A's batch under its new lease");
        a.flush_agents();
        assert_eq!(sequence(), [[Some("2".to_string())]], "and after one");
    }

    /// An Agent whose row named another holder when A's batch read it, and that connected to A
    /// again before the batch went on, with that take's answer lost too, and disconnected, stays
    /// unsure: A's next batch reads it again and writes its goodbye.
    /// Verifies: ADR-0025
    #[test]
    fn an_agent_taken_again_after_the_batch_read_its_row_is_read_again() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x7d; 16]);
        hooks.losing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        b.process(report(&uid, 2), Transport::WebSocket, Some(9));
        a.mark_disconnected(&[uid], 7);
        let again = a.clone();
        *hooks.after_takes.lock().expect("hook lock") = Some(Box::new(move || {
            again.process(report(&uid, 3), Transport::WebSocket, Some(8));
            again.mark_disconnected(&[uid], 8);
        }));
        a.write_presence(LEASE_A).expect("A's batch");
        hooks.losing_takes.store(false, Ordering::SeqCst);
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 3"));
        a.write_presence(LEASE_A).expect("A's next batch");
        assert_eq!(stored_presence(&test, &uid), "none false 3", "the goodbye");
    }

    /// Writes a live lease of `instance_id`, which the presence batches then read as live.
    fn live_lease(test: &crate::postgres::testing::TestDatabase, instance_id: &str) {
        test.sql(&format!(
            "INSERT INTO leases (instance_id, host, must_be_alone, taken_at, renewed_at, expires_at) \
             VALUES ('{instance_id}', 'b', false, now(), now(), now() + interval '1 hour')"
        ));
    }

    /// An Agent that B took and disconnected, which then connects to A again with a take that is
    /// not written, is named by A's next batch: a take that failed leaves A no count, so the batch
    /// takes back a row no instance holds whatever its count, and A, which carries the Agent, is
    /// named.
    /// Verifies: ADR-0025
    #[test]
    fn a_connect_whose_take_was_not_written_is_named_by_the_next_batch() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x91; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        b.process(report(&uid, 2), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 2");
        a.release_taken(None).expect("A hears of the take");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 3), Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        assert!(view(&a, &uid).connected, "A carries it");
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 2",
            "the take is not written"
        );
        a.write_presence(LEASE_A).expect("A's batch");
        a.write_presence(LEASE_A).expect("A's next batch");
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 3"));
    }

    /// An Agent whose take on A was not written, and that disconnected before A's next batch, has
    /// its last record written by A's shutdown flush: the batch read a row that names no holder
    /// and no take since, so what A heard of it last is the newest.
    /// Verifies: ADR-0025
    #[test]
    fn the_shutdown_flush_writes_an_agent_whose_take_was_not_written() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x92; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        a.mark_disconnected(&[uid], 8);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 1",
            "the take is not written"
        );
        test.sql(&format!(
            "UPDATE agents SET sequence_num = 0 WHERE instance_uid = '{uid}'"
        ));
        a.flush_agents();
        assert_eq!(
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            )),
            [[Some("2".to_string())]],
            "the last record is written"
        );
    }

    /// An Agent whose take on A was not written, that B took and let go before A's next batch
    /// read the row, keeps what B flushed of it at A's shutdown flush: the row names no holder,
    /// but B's take counted since the one A was answered.
    /// Verifies: ADR-0025
    #[test]
    fn the_shutdown_flush_of_a_lost_take_keeps_a_take_counted_since() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x93; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        a.mark_disconnected(&[uid], 8);
        b.process(report(&uid, 5), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        b.flush_agents();
        assert_eq!(stored_presence(&test, &uid), "none false 2");
        a.write_presence(LEASE_A).expect("A's batch");
        a.flush_agents();
        assert_eq!(
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            )),
            [[Some("5".to_string())]],
            "B's record stays"
        );
    }

    /// An Agent first heard on A, whose record A created and whose take was not written, and that
    /// disconnected before A's next batch, has its last record written by A's shutdown flush: the
    /// record counted no take when A created it.
    /// Verifies: ADR-0025
    #[test]
    fn the_shutdown_flush_writes_an_agent_created_here_whose_take_was_not_written() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x94; 16]);
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 0",
            "the take is not written"
        );
        test.sql(&format!(
            "UPDATE agents SET sequence_num = 0 WHERE instance_uid = '{uid}'"
        ));
        a.flush_agents();
        assert_eq!(
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            )),
            [[Some("2".to_string())]],
            "the last record is written"
        );
    }

    /// An Agent that B carried and wrote the disconnect of, and that A then carried with a take that
    /// was not written, keeps what A wrote of it at B's shutdown flush: the row names no holder at
    /// the count B was answered, but holds the higher sequence number of A's newer record.
    /// Verifies: ADR-0030
    #[test]
    fn the_shutdown_flush_keeps_a_newer_carriers_record_whose_take_was_not_written() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x95; 16]);
        b.process(report(&uid, 1), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        let mut newer = report(&uid, 2);
        newer.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            status: "running".to_string(),
            ..Default::default()
        });
        a.process(newer, Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 1",
            "A's take is not written, and its report takes nothing"
        );
        let stored = || {
            test.rows(&format!(
                "SELECT sequence_num::text, (health IS NOT NULL)::text FROM agents \
                 WHERE instance_uid = '{uid}'"
            ))
        };
        assert_eq!(
            stored(),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A wrote its report"
        );
        b.flush_agents();
        assert_eq!(
            stored(),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A's newer record stays"
        );
    }

    /// An Agent another instance with a live lease holds is not taken by a report of A, whose take
    /// was not written: the report is written, and the holder and its count stay. The holder's
    /// shutdown flush, as it stops, writes no lower sequence number over A's report.
    /// Verifies: ADR-0025
    #[test]
    fn a_report_whose_take_was_not_written_takes_no_agent_a_live_instance_holds() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        live_lease(&test, LEASE_B);
        let uid = InstanceUid([0x96; 16]);
        b.process(report(&uid, 1), Transport::WebSocket, Some(9));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
        hooks.failing_takes.store(true, Ordering::SeqCst);
        let mut newer = report(&uid, 2);
        newer.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            ..Default::default()
        });
        a.process(newer, Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
        let stored = || {
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            ))
        };
        assert_eq!(stored(), [[Some("2".to_string())]], "A's report is written");
        b.flush_agents();
        assert_eq!(
            stored(),
            [[Some("2".to_string())]],
            "B's flush writes no older sequence number over A's report"
        );
    }

    /// The sequence number and health stored of `uid`, as text.
    fn stored_report(
        test: &crate::postgres::testing::TestDatabase,
        uid: &InstanceUid,
    ) -> Vec<Vec<Option<String>>> {
        test.rows(&format!(
            "SELECT sequence_num::text, (health IS NOT NULL)::text FROM agents \
             WHERE instance_uid = '{uid}'"
        ))
    }

    /// The report of `uid` with `sequence_num` and a health.
    fn healthy_report(uid: &InstanceUid, sequence_num: u64) -> AgentToServer {
        let mut healthy = report(uid, sequence_num);
        healthy.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            ..Default::default()
        });
        healthy
    }

    /// An Agent that B carried up to a high sequence number and wrote the disconnect of, that
    /// started again and is carried by A with a take that was not written, has A's record again at
    /// A's next report, though B's shutdown flush wrote its older record over A's: A, answered no
    /// count for the Agent, writes every report of it.
    /// Verifies: ADR-0030, ADR-0025
    #[test]
    fn a_record_an_older_flush_wrote_over_a_restarted_agent_heals_at_its_next_report() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x98; 16]);
        b.process(report(&uid, 50), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(healthy_report(&uid, 1), Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("1".to_string()), Some("true".to_string())]],
            "A wrote its report"
        );
        b.flush_agents();
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("50".to_string()), Some("false".to_string())]],
            "B's flush finds no holder, its count, and a lower sequence number"
        );
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A's next report writes A's record again"
        );
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored_presence(&test, &uid),
            format!("{LEASE_A} true 2"),
            "the batch takes the Agent and answers a count"
        );
        a.process(report(&uid, 3), Transport::WebSocket, Some(7));
        let sequence = || {
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            ))
        };
        assert_eq!(
            sequence(),
            [[Some("3".to_string())]],
            "no digest was kept before"
        );
        test.sql(&format!(
            "UPDATE agents SET sequence_num = 0 WHERE instance_uid = '{uid}'"
        ));
        a.process(report(&uid, 4), Transport::WebSocket, Some(7));
        assert_eq!(
            sequence(),
            [[Some("0".to_string())]],
            "with a count, a report that changes nothing durable writes nothing"
        );
    }

    /// An Agent that B, stopping, still holds with a live lease, that started again and is carried
    /// by A with a take that was not written, has A's record again at A's next report, though B's
    /// shutdown flush wrote its older record over A's.
    /// Verifies: ADR-0030, ADR-0025
    #[test]
    fn a_record_a_stopping_holders_flush_wrote_over_a_restarted_agent_heals_at_its_next_report() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        live_lease(&test, LEASE_B);
        let uid = InstanceUid([0x99; 16]);
        b.process(report(&uid, 50), Transport::WebSocket, Some(9));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(healthy_report(&uid, 1), Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        b.flush_agents();
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("50".to_string()), Some("false".to_string())]],
            "B's flush, holding the row, finds a lower sequence number"
        );
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A's next report writes A's record again"
        );
    }

    /// An Agent A carried, that was forgotten through B and created again there, and that A then
    /// carries with a take that was not written, keeps what A wrote of it at B's shutdown flush:
    /// the count B was answered of the record created again is the one stored, but the row holds
    /// the higher sequence number of A's report.
    /// Verifies: ADR-0030, ADR-0025
    #[test]
    fn the_shutdown_flush_keeps_a_newer_carriers_record_after_a_record_created_again() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x97; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        assert!(b.forget_agent(&uid).is_ok(), "forgotten through B");
        b.process(report(&uid, 1), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 1",
            "created again"
        );
        hooks.failing_takes.store(true, Ordering::SeqCst);
        let mut newer = report(&uid, 2);
        newer.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            ..Default::default()
        });
        a.process(newer, Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        b.flush_agents();
        assert_eq!(
            test.rows(&format!(
                "SELECT sequence_num::text, (health IS NOT NULL)::text FROM agents \
                 WHERE instance_uid = '{uid}'"
            )),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A's newer record stays"
        );
    }

    /// An Agent whose take was written under A's old lease and whose answer was lost is named by
    /// A's batch under its new lease: A keeps no count from before the lost take to refuse it with.
    /// Verifies: ADR-0025
    #[test]
    fn a_lost_take_under_an_old_lease_is_named_by_the_batch_under_the_new_one() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let losing = hooked.losing_takes.clone();
        let a = on_postgres(&database, Box::new(hooked));
        let (lease, leases) = tokio::sync::watch::channel(Some(LEASE_A.to_string()));
        a.serve_under(leases);
        let uid = InstanceUid([0x93; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        losing.store(true, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        losing.store(false, Ordering::SeqCst);
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 2"));
        let renewed = "00000000-0000-7000-8000-0000000000aa";
        lease.send_replace(Some(renewed.to_string()));
        a.write_presence(renewed)
            .expect("A's batch under its new lease");
        assert_eq!(stored_presence(&test, &uid), format!("{renewed} true 3"));
    }

    /// A batch never names its instance over a live take whose count equals its own: an Agent
    /// forgotten through B and created again there counts its takes from the start, and A, which
    /// has not heard of the forget, writes nothing of it.
    /// Verifies: ADR-0030, ADR-0025
    #[test]
    fn a_batch_never_names_its_instance_over_a_live_take_of_a_record_created_again() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, _hooks) = hooked_pair(&database);
        live_lease(&test, LEASE_B);
        let uid = InstanceUid([0x92; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        next_millisecond();
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        assert!(b.forget_agent(&uid).is_ok(), "forgotten through B");
        b.process(report(&uid, 1), Transport::WebSocket, Some(9));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
        a.write_presence(LEASE_A)
            .expect("A's batch before it hears of the forget");
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
    }

    /// A batch that meets a change of lease between its commit and its answers keeps the counts
    /// it was answered: the Agent it named anew under the lease before is named again under the
    /// next one. Each batch takes back the row its instance's previous lease, now not live, holds
    /// under the count it was answered.
    /// Verifies: ADR-0025
    #[test]
    fn a_batch_that_meets_a_change_of_lease_keeps_the_counts_it_was_answered() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_presence = hooked.after_presence.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        let (lease, leases) = tokio::sync::watch::channel(Some(LEASE_A.to_string()));
        a.serve_under(leases);
        let (uid, other) = (InstanceUid([0x94; 16]), InstanceUid([0x95; 16]));
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 1"));
        let (second, third) = (
            "00000000-0000-7000-8000-0000000000a2",
            "00000000-0000-7000-8000-0000000000a3",
        );
        lease.send_replace(Some(second.to_string()));
        // Between the batch's commit and its answers the lease changes again, and a connect is
        // taken under the new one.
        let again = a.clone();
        *after_presence.lock().expect("hook lock") = Some(Box::new(move || {
            lease.send_replace(Some(third.to_string()));
            again.process(report(&other, 1), Transport::WebSocket, Some(8));
        }));
        a.write_presence(second)
            .expect("A's batch under its second lease");
        assert_eq!(stored_presence(&test, &uid), format!("{second} true 2"));
        a.write_presence(third)
            .expect("A's batch under its third lease");
        assert_eq!(stored_presence(&test, &uid), format!("{third} true 3"));
    }

    /// A report that re-keyed its Agent and then finds it no longer carried, its connection
    /// marked closed meanwhile, goes on under the identity it assigned rather than start again
    /// from the message: a next round would mint a second identity and leave the first record
    /// behind. The Agent stays disconnected, as the closed connection left it.
    /// Verifies: ADR-0025, ADR-0030
    #[test]
    fn a_report_that_rekeyed_its_agent_does_not_start_again() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let of: Arc<OnceLock<std::sync::Weak<AppState>>> = Arc::new(OnceLock::new());
        let uid = InstanceUid([0x9c; 16]);
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe: Probe = {
            let (of, armed) = (of.clone(), armed.clone());
            Arc::new(move |call, _| {
                if call == "rebind" && armed.swap(false, Ordering::SeqCst) {
                    // The connection closes while the report moves the binding.
                    let a = of.get().and_then(std::sync::Weak::upgrade).expect("A");
                    a.mark_disconnected(&[uid], 7);
                }
            })
        };
        let revocations = Revocations::open(
            Box::new(ProbedLedger {
                inner: Box::new(crate::postgres::ledger::PgLedgerStore::new(
                    database.clone(),
                )),
                probe,
            }),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations");
        let a = Arc::new(
            AppState::with_stores(
                Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
                Box::new(crate::postgres::configs::PgConfigBackend::new(
                    database.clone(),
                )),
                Box::new(crate::clock::SystemClock),
            )
            .expect("state")
            .with_revocations(Some(Arc::new(revocations))),
        );
        of.set(Arc::downgrade(&a)).expect("set once");
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let host = of_host("h9c");
        a.process_presented(report(&uid, 1), Transport::WebSocket, Some(7), Some(&host));
        armed.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process_presented(rekey, Transport::WebSocket, Some(7), Some(&host));
        let assigned = answered
            .reply
            .agent_identification
            .as_ref()
            .map(|identification| identification.new_instance_uid.clone())
            .expect("re-keyed");
        let rows = test.rows("SELECT instance_uid FROM agents");
        assert_eq!(rows.len(), 1, "one record: {rows:?}");
        assert_eq!(
            answered.uid.map(|uid| uid.as_bytes().to_vec()),
            Some(assigned)
        );
        let moved = answered.uid.expect("the new identity");
        assert!(
            !view(&a, &moved).connected,
            "left disconnected, as its closed connection left it"
        );
    }

    /// A ledger store that runs a probe before each write a report makes of the host register:
    /// binding an `instance_uid`, writing a Host and moving a binding.
    struct ProbedLedger {
        inner: Box<dyn crate::revocation::LedgerStore>,
        probe: Probe,
    }

    impl crate::revocation::LedgerStore for ProbedLedger {
        fn load(&self) -> Result<crate::revocation::Ledger, String> {
            self.inner.load()
        }
        fn put_issued(&self, issued: &crate::revocation::Issued) -> Result<(), String> {
            self.inner.put_issued(issued)
        }
        fn remove_issued(&self, id: &CertId, now_ms: u64) -> Result<bool, String> {
            self.inner.remove_issued(id, now_ms)
        }
        fn register_issued(
            &self,
            issued: &crate::revocation::Issued,
            bounds: &crate::revocation::RegisterBounds,
        ) -> Result<crate::revocation::Registered, String> {
            self.inner.register_issued(issued, bounds)
        }
        fn register_minting(
            &self,
            issued: &crate::revocation::Issued,
            bounds: &crate::revocation::RegisterBounds,
            mint: Option<(&str, &crate::revocation::Host)>,
            bootstrap: Option<&CertId>,
        ) -> Result<crate::revocation::Registered, String> {
            self.inner.register_minting(issued, bounds, mint, bootstrap)
        }
        fn add_revocation(
            &self,
            entry: &crate::revocation::Revocation,
            bound: usize,
        ) -> Result<crate::revocation::Added, String> {
            self.inner.add_revocation(entry, bound)
        }
        fn list_version(&self) -> Option<u64> {
            self.inner.list_version()
        }
        fn remove_revocation(&self, id: &str) -> Result<bool, String> {
            self.inner.remove_revocation(id)
        }
        fn bind(&self, instance_uid: &str, host: &str) -> Result<crate::revocation::Bound, String> {
            (self.probe)("bind", None);
            self.inner.bind(instance_uid, host)
        }
        fn bind_seen(
            &self,
            instance_uid: &str,
            host: &str,
            row: &crate::revocation::Host,
        ) -> Result<crate::revocation::Bound, String> {
            (self.probe)("bind_seen", None);
            self.inner.bind_seen(instance_uid, host, row)
        }
        fn unbind(&self, instance_uid: &str) -> Result<Option<String>, String> {
            self.inner.unbind(instance_uid)
        }
        fn rebind(
            &self,
            old: &str,
            new: &str,
        ) -> Result<Option<(String, crate::revocation::Bound)>, String> {
            (self.probe)("rebind", None);
            self.inner.rebind(old, new)
        }
        fn unbind_host(&self, host: &str) -> Result<(), String> {
            self.inner.unbind_host(host)
        }
        fn writes_hosts_by_act(&self) -> bool {
            self.inner.writes_hosts_by_act()
        }
        fn write_host_act(&self, act: &crate::revocation::HostAct) -> Result<bool, String> {
            (self.probe)("write_host_act", None);
            self.inner.write_host_act(act)
        }
        fn save_hosts(
            &self,
            hosts: &BTreeMap<String, crate::revocation::Host>,
        ) -> Result<(), String> {
            (self.probe)("save_hosts", None);
            self.inner.save_hosts(hosts)
        }
    }

    /// A probe that notes each call with whether the fleet lock of the state `of` names was free
    /// as it was made.
    fn lock_probe(
        of: &Arc<OnceLock<std::sync::Weak<AppState>>>,
        seen: &Arc<Mutex<Vec<(&'static str, bool)>>>,
    ) -> Probe {
        let (of, seen) = (of.clone(), seen.clone());
        Arc::new(move |call, _| {
            let free = of
                .get()
                .and_then(std::sync::Weak::upgrade)
                .is_none_or(|state| state.fleet.try_lock().is_ok());
            seen.lock().expect("seen lock").push((call, free));
        })
    }

    /// No call a report makes to the database holds the fleet lock: the creation of a new Agent's
    /// record, the binding of its `instance_uid` and the write of its Host, its take, the read of
    /// its group's Client Agents another instance carries, the write of what it reported, and a
    /// re-key it asks for with the move of its binding are each made with the lock free, so no
    /// other report waits for them.
    /// Verifies: ADR-0025, ADR-0030
    #[test]
    fn a_report_holds_the_fleet_lock_across_no_call_to_the_database() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        // Another instance carries an Agent of the same Host, so placing this one reads it.
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let elsewhere = InstanceUid([0x90; 16]);
        b.process_presented(
            report(&elsewhere, 1),
            Transport::WebSocket,
            Some(1),
            Some(&of_host("h91")),
        );
        let (of, seen) = (Arc::new(OnceLock::new()), Arc::new(Mutex::new(Vec::new())));
        let probe = lock_probe(&of, &seen);
        *hooked.probe.lock().expect("hook lock") = Some(probe.clone());
        let revocations = Revocations::open(
            Box::new(ProbedLedger {
                inner: Box::new(crate::postgres::ledger::PgLedgerStore::new(
                    database.clone(),
                )),
                probe,
            }),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations");
        let a = Arc::new(
            AppState::with_stores(
                Box::new(hooked),
                Box::new(crate::postgres::configs::PgConfigBackend::new(
                    database.clone(),
                )),
                Box::new(crate::clock::SystemClock),
            )
            .expect("state")
            .with_revocations(Some(Arc::new(revocations))),
        );
        of.set(Arc::downgrade(&a)).expect("set once");
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);

        let uid = InstanceUid([0x91; 16]);
        let host = of_host("h91");
        a.process_presented(report(&uid, 1), Transport::WebSocket, Some(7), Some(&host));
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process_presented(rekey, Transport::WebSocket, Some(7), Some(&host));
        assert!(
            answered.reply.agent_identification.is_some(),
            "re-keyed: {:?}",
            answered.reply
        );

        let seen = seen.lock().expect("seen lock").clone();
        for call in [
            "create_within",
            "get_many",
            "bind_seen",
            "write_host_act",
            "take",
            "write_report",
            "rekey",
            "rebind",
        ] {
            assert!(
                seen.iter().any(|(made, _)| *made == call),
                "{call} was made: {seen:?}"
            );
        }
        let held: Vec<&(&str, bool)> = seen.iter().filter(|(_, free)| !free).collect();
        assert!(held.is_empty(), "the fleet lock was held across {held:?}");
    }

    /// While one Agent's report waits for a call to its store, another Agent's report is answered:
    /// what the fleet lock guards is applied before and after the call, never across it.
    /// Verifies: ADR-0025
    #[test]
    fn another_agents_report_is_answered_while_one_agents_write_waits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooked = Hooked::new(Box::new(
            crate::fs::FsAgentStore::open(dir.path()).expect("agents"),
        ));
        let (held, other) = (InstanceUid([0x92; 16]), InstanceUid([0x93; 16]));
        let (entered, entering) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        *hooked.probe.lock().expect("hook lock") = Some(Arc::new(move |call, uid| {
            if call == "write_report" && uid == Some(&held) {
                let _ = entered.send(());
                let _ = released
                    .lock()
                    .expect("release lock")
                    .recv_timeout(Duration::from_secs(30));
            }
        }));
        let state = Arc::new(
            AppState::with_stores(
                Box::new(hooked),
                Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
                Box::new(crate::clock::SystemClock),
            )
            .expect("state"),
        );
        let waiting = {
            let state = state.clone();
            std::thread::spawn(move || {
                state.process(report(&held, 1), Transport::WebSocket, Some(1))
            })
        };
        entering
            .recv_timeout(Duration::from_secs(30))
            .expect("the held Agent's write is under way");
        let (answered, answering) = std::sync::mpsc::channel();
        {
            let state = state.clone();
            std::thread::spawn(move || {
                let _ =
                    answered.send(state.process(report(&other, 1), Transport::WebSocket, Some(2)));
            });
        }
        let other_answer = answering.recv_timeout(Duration::from_secs(30));
        let _ = release.send(());
        let held_answer = waiting.join().expect("the held report");
        let other_answer = other_answer.expect("the other Agent is answered while the write waits");
        assert_eq!(other_answer.uid, Some(other));
        assert_eq!(held_answer.uid, Some(held));
    }

    /// An instance whose take of an Agent another instance's take overtakes before it holds the
    /// Agent in memory stops carrying it once it hears of that take: hearing of it waits for the
    /// report under way here, so it reads the holder after this instance holds the Agent, and the
    /// newest connection keeps the Agent.
    /// Verifies: ADR-0025
    #[test]
    fn an_instance_whose_take_is_overtaken_before_it_holds_the_agent_stops_carrying_it() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_take = hooked.after_take.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let b = Arc::new(on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        ));
        b.serve_under(tokio::sync::watch::channel(Some(LEASE_B.to_string())).1);
        let uid = InstanceUid([0x94; 16]);
        b.process(report(&uid, 1), Transport::WebSocket, Some(3));

        // A's take commits; then B takes the Agent again, and A hears of it while its report has
        // not yet taken the Agent into what it holds.
        let (heard, hearing) = std::sync::mpsc::channel();
        let waits_before = a.agent_lock_waits();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let (a, b, finished) = (Arc::downgrade(&a), b.clone(), finished.clone());
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                // The Agent's connection to B closed, and it opens another to B.
                b.mark_disconnected(&[uid], 3);
                b.process(report(&uid, 2), Transport::WebSocket, Some(4));
                let a = a.upgrade().expect("A");
                let (on_a, done) = (a.clone(), finished.clone());
                std::thread::spawn(move || {
                    let read = on_a.release_taken(Some(&HashSet::from([uid])));
                    done.store(true, Ordering::SeqCst);
                    let _ = heard.send(read);
                });
                // Hearing waits for this report, or is done before it goes on.
                until("A hears of B's take", || {
                    a.agent_lock_waits() > waits_before || finished.load(Ordering::SeqCst)
                });
            }));
        }
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        hearing
            .recv_timeout(Duration::from_secs(30))
            .expect("A heard of the take")
            .expect("A read the holder");
        assert!(!view(&a, &uid).connected, "A no longer carries it");
        assert!(
            stored_presence(&test, &uid).starts_with(&format!("{LEASE_B} true")),
            "B holds it: {}",
            stored_presence(&test, &uid)
        );
    }

    /// What another instance commits to a new Agent's record between its creation here and the
    /// end of the report that created it is taken into what this instance holds when it hears of
    /// it: the record is held from its creation on, so the take-up finds it, and the report goes
    /// on from it rather than from a record of its own. A label is what such a commit can carry:
    /// a rollout then releases nothing and a restart is refused, since the record names neither a
    /// description nor a capability before the report that created it is written, and what is
    /// held agrees with the store on all three.
    /// Verifies: ADR-0025
    #[test]
    fn what_another_instance_commits_to_a_new_agent_while_its_report_runs_is_held() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_take = hooked.after_take.clone();
        let blobs = tempfile::tempdir().expect("blobs");
        let a = Arc::new(delivering_on(&database, Box::new(hooked), blobs.path()));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let b = Arc::new(delivering_on(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
            blobs.path(),
        ));
        aim_at_the_early_ring(&b);
        let uid = InstanceUid([0x98; 16]);
        {
            let (a, b) = (Arc::downgrade(&a), b.clone());
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                // The record is committed; B labels it, rolls its Deployment out to it and asks
                // it to restart, and A hears of it before its report holds the Agent.
                label(&b, &uid, &[("ring", "early")]);
                b.rollout_to_agent(&uid, None)
                    .expect("the rollout through B");
                assert!(
                    matches!(b.request_restart(&uid), Err(RestartError::NoCapability)),
                    "a restart is refused before the record names its capabilities"
                );
                let a = a.upgrade().expect("A");
                a.take_up_agents(Some(&HashSet::from([uid])))
                    .expect("A takes it up");
            }));
        }
        let created = a.process(restartable(&uid, 1), Transport::WebSocket, Some(7));
        assert_eq!(created.uid, Some(uid));
        let stored = crate::postgres::agents::PgAgentStore::new(database.clone())
            .get(&uid)
            .expect("read")
            .expect("stored");
        let held = view(&a, &uid);
        assert_eq!(held.labels, stored.labels, "the label is held");
        assert_eq!(held.labels.get("ring").map(String::as_str), Some("early"));
        assert_eq!(
            held.assigned_deployment,
            stored
                .assignment
                .map(|assignment| assignment.deployment)
                .unwrap_or_default(),
            "the assignment held is the one stored"
        );
        assert!(!stored.restart_pending && created.reply.command.is_none());
    }

    /// A new Agent's record is held from its creation on but not carried until its report holds
    /// it: a presence batch run meanwhile writes nothing of it, and the Agent is not connected
    /// here until the report ends.
    /// Verifies: ADR-0025
    #[test]
    fn a_new_agents_record_is_held_from_its_creation_but_not_carried_before_its_report_ends() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_take = hooked.after_take.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x9d; 16]);
        let (looked, look) = std::sync::mpsc::channel();
        {
            let a = Arc::downgrade(&a);
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                let a = a.upgrade().expect("A");
                let held = a
                    .fleet
                    .lock()
                    .expect("fleet lock")
                    .get(&uid)
                    .map(|record| (record.connected, record.owner));
                let _ = looked.send((held, a.write_presence(LEASE_A)));
            }));
        }
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        let (held, batch) = look.recv_timeout(Duration::from_secs(30)).expect("looked");
        assert_eq!(held, Some((false, None)), "held, not carried");
        assert_eq!(batch, Ok(0), "no presence written of it");
        assert!(view(&a, &uid).connected, "carried once the report ends");
    }

    /// A new Agent's report that the host register refuses leaves no record, neither stored nor
    /// held: the record its creation made goes from both.
    /// Verifies: ADR-0025
    #[test]
    fn a_new_agents_report_the_register_refuses_leaves_no_record_held_or_stored() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let probe = hooked.probe.clone();
        let a = on_postgres(&database, Box::new(hooked));
        let b = Arc::new(on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        ));
        let uid = InstanceUid([0x99; 16]);
        let subject = crate::revocation::SubjectValues::default;
        {
            // Before A creates the record, B binds the Agent to another host, and its record is
            // forgotten again, the binding staying: A's creation goes ahead, and the register
            // then refuses the report.
            let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let sql = test.sql_runner();
            *probe.lock().expect("hook lock") = Some(Arc::new(move |call, named| {
                if call != "create_within"
                    || named != Some(&uid)
                    || !armed.swap(false, Ordering::SeqCst)
                {
                    return;
                }
                report_over(&b, "h2", subject(), uid, &[]);
                sql(&format!("DELETE FROM agents WHERE instance_uid = '{uid}'"));
            }));
        }
        let refused = report_over(&a, "h1", subject(), uid, &[]);
        assert_eq!(refused.uid, None, "refused");
        assert!(
            !a.fleet.lock().expect("fleet lock").contains_key(&uid),
            "not held"
        );
        assert!(
            test.rows(&format!(
                "SELECT instance_uid FROM agents WHERE instance_uid = '{uid}'"
            ))
            .is_empty(),
            "not stored"
        );
    }

    /// A new Agent forgotten through another instance between its creation here and the end of
    /// the report that created it is created anew by a next round of that report, so what is held
    /// is stored.
    /// Verifies: ADR-0025, ADR-0030
    #[test]
    fn a_new_agent_forgotten_while_its_report_runs_is_created_anew() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_take = hooked.after_take.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x9a; 16]);
        {
            let on = Arc::downgrade(&a);
            let gone = format!("DELETE FROM agents WHERE instance_uid = '{uid}'");
            let test = test.sql_runner();
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                test(&gone);
                on.upgrade()
                    .expect("A")
                    .take_up_agents(Some(&HashSet::from([uid])))
                    .expect("A hears the Agent forgotten");
            }));
        }
        let answered = a.process(report(&uid, 1), Transport::WebSocket, Some(8));
        assert_eq!(answered.uid, Some(uid), "carried");
        assert!(view(&a, &uid).connected, "held");
        assert_eq!(
            test.rows(&format!(
                "SELECT instance_uid FROM agents WHERE instance_uid = '{uid}'"
            ))
            .len(),
            1,
            "stored anew"
        );
    }

    /// A restart requested while a delivery of the one queued before clears it is not lost: the
    /// request waits for the delivery, which holds the Agent's own lock across its clear, and is
    /// queued after it, so the Agent is sent it next.
    /// Verifies: ADR-0025
    #[test]
    fn a_restart_requested_while_a_delivery_clears_one_stays_queued() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooked = Hooked::new(Box::new(
            crate::fs::FsAgentStore::open(dir.path()).expect("agents"),
        ));
        let probe = hooked.probe.clone();
        let state = Arc::new(
            AppState::with_stores(
                Box::new(hooked),
                Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
                Box::new(crate::clock::SystemClock),
            )
            .expect("state"),
        );
        let uid = InstanceUid([0x95; 16]);
        state.process(restartable(&uid, 1), Transport::WebSocket, Some(1));
        assert!(state.request_restart(&uid).is_ok(), "queued");

        let (requested, requesting) = std::sync::mpsc::channel();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let waits_before = state.agent_lock_waits();
        {
            let (on, finished) = (Arc::downgrade(&state), finished.clone());
            *probe.lock().expect("hook lock") = Some(Arc::new(move |call, _| {
                if call != "set_restart_pending" || !armed.swap(false, Ordering::SeqCst) {
                    return;
                }
                let state = on.upgrade().expect("state");
                let (again, done, requested) = (state.clone(), finished.clone(), requested.clone());
                std::thread::spawn(move || {
                    let request = again.request_restart(&uid).is_ok();
                    done.store(true, Ordering::SeqCst);
                    let _ = requested.send(request);
                });
                until("the request waits for the delivery or is done", || {
                    state.agent_lock_waits() > waits_before || finished.load(Ordering::SeqCst)
                });
            }));
        }
        let delivered = state.process(restartable(&uid, 2), Transport::WebSocket, Some(1));
        assert!(
            delivered.reply.command.is_some(),
            "the first restart is delivered"
        );
        assert!(
            requesting
                .recv_timeout(Duration::from_secs(30))
                .expect("the second request is answered"),
            "queued again"
        );
        assert!(
            state.restart_command_for(&uid, 1).is_some(),
            "the second restart is sent next"
        );
    }

    /// A label set while a report reads the Agent's record from the store, before it holds it, is
    /// not undone by what the read found: the label waits for the report, which holds the Agent's
    /// own lock, and is then taken into the record held.
    /// Verifies: ADR-0025
    #[test]
    fn a_label_set_while_a_report_reads_the_record_is_not_undone() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_get = hooked.after_get.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let uid = InstanceUid([0x96; 16]);
        b.process(report(&uid, 1), Transport::Http, None);
        label(&b, &uid, &[("tier", "edge")]);

        let (labelled, labelling) = std::sync::mpsc::channel();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waits_before = a.agent_lock_waits();
        {
            let (on, finished) = (Arc::downgrade(&a), finished.clone());
            *after_get.lock().expect("hook lock") = Some(Box::new(move || {
                let a = on.upgrade().expect("A");
                let (again, done) = (a.clone(), finished.clone());
                std::thread::spawn(move || {
                    label(&again, &uid, &[("tier", "core")]);
                    done.store(true, Ordering::SeqCst);
                    let _ = labelled.send(());
                });
                until("the label waits for the report or is done", || {
                    a.agent_lock_waits() > waits_before || finished.load(Ordering::SeqCst)
                });
            }));
        }
        a.process(report(&uid, 2), Transport::Http, None);
        labelling
            .recv_timeout(Duration::from_secs(30))
            .expect("the label is set");
        assert_eq!(view(&a, &uid).labels["tier"], "core", "the label set stays");
    }

    /// A report whose Agent leaves memory while it reads and writes the store — forgotten through
    /// another instance — starts again from what the store holds then, and the Agent is held
    /// under a record created anew.
    /// Verifies: ADR-0025, ADR-0030
    #[test]
    fn a_report_whose_agent_leaves_memory_meanwhile_starts_again() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_take = hooked.after_take.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x97; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        {
            let on = Arc::downgrade(&a);
            let gone = format!("DELETE FROM agents WHERE instance_uid = '{uid}'");
            let test = test.sql_runner();
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                test(&gone);
                on.upgrade()
                    .expect("A")
                    .take_up_agents(Some(&HashSet::from([uid])))
                    .expect("A hears the Agent forgotten");
            }));
        }
        let answered = a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        assert_eq!(answered.uid, Some(uid), "carried");
        assert!(view(&a, &uid).connected, "held");
        assert_eq!(
            test.rows(&format!(
                "SELECT instance_uid FROM agents WHERE instance_uid = '{uid}'"
            ))
            .len(),
            1,
            "stored anew"
        );
    }

    /// A report whose Agent leaves memory in each of its rounds is answered `Unavailable`, so the
    /// Agent retries, rather than starting again without end.
    /// Verifies: ADR-0025
    #[test]
    fn a_report_whose_agent_leaves_memory_in_every_round_is_answered_unavailable() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let probe = hooked.probe.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x98; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        let rounds = Arc::new(AtomicU64::new(0));
        {
            let (on, rounds) = (Arc::downgrade(&a), rounds.clone());
            let other = crate::postgres::agents::PgAgentStore::new(database.clone());
            let gone = format!("DELETE FROM agents WHERE instance_uid = '{uid}'");
            let test = test.sql_runner();
            *probe.lock().expect("hook lock") = Some(Arc::new(move |call, _| {
                if call != "take" {
                    return;
                }
                rounds.fetch_add(1, Ordering::SeqCst);
                // Forgotten and heard of here, then stored again by another instance.
                test(&gone);
                on.upgrade()
                    .expect("A")
                    .take_up_agents(Some(&HashSet::from([uid])))
                    .expect("A hears the Agent forgotten");
                other
                    .put(&uid, &crate::agent_store::tests::record())
                    .expect("stored again");
            }));
        }
        let answered = a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        assert_eq!(rounds.load(Ordering::SeqCst), REPORT_ROUNDS as u64);
        assert!(
            answered
                .reply
                .error_response
                .as_ref()
                .is_some_and(|error| error.error_message.contains("retry later")),
            "answered unavailable: {:?}",
            answered.reply
        );
        assert_eq!(answered.uid, None);
    }

    /// A re-key an Agent asked for that a read again finds made before the report holds the Agent
    /// under its new identity keeps the Agent: the move is pending until then, so the read again
    /// reads it back and moves what is held, and the session goes on, its labels with it.
    /// Verifies: ADR-0025, ADR-0030
    #[test]
    fn a_rekey_a_read_again_finds_before_the_report_holds_it_keeps_the_agent() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_rekey = hooked.after_rekey.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x99; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        label(&a, &uid, &[("tier", "edge")]);
        {
            let on = Arc::downgrade(&a);
            *after_rekey.lock().expect("hook lock") = Some(Box::new(move || {
                // As after a missed notification: everything held is read again.
                on.upgrade()
                    .expect("A")
                    .take_up_agents(None)
                    .expect("read again");
            }));
        }
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process(rekey, Transport::WebSocket, Some(7));
        let new = answered.uid.expect("carried");
        assert_ne!(new, uid, "re-keyed");
        assert!(a.take_forgotten(7).is_empty(), "its session goes on");
        assert_eq!(view(&a, &new).labels["tier"], "edge", "its labels with it");
    }
}
