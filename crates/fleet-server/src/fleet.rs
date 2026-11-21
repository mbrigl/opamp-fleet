//! In-memory fleet state, keyed by Instance UID — never by the connection that carried a message
//! (CLT-modes-and-gateway). Each context that changes it is a submodule, which imports by name what it takes
//! from the root, the siblings ranked below it, and the crate; the root names no context but in
//! its own `pub use` lines (SRV-fleet-contexts-call-by-rank).

#![deny(clippy::wildcard_imports)]

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use opamp::proto::{
    AgentDescription, AgentIdentification, AvailableComponents, ComponentHealth,
    ConnectionSettingsOffers, ConnectionSettingsStatus, OpAmpConnectionSettings, PackageStatuses,
    RemoteConfigStatus, ServerCapabilities, ServerToAgent, TelemetryConnectionSettings,
};
use opamp::uid::InstanceUid;
use prost::Message as _;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tracing::info;

use crate::agent_store::{AgentStore, PersistedAgent, Presence};
use crate::audit::{Audit, Entry};
use crate::configs::{ConfigBackend, ConfigStore};
use crate::deployments::DeploymentStore;
use crate::enrolment::Enrolment;
use crate::labels::Sources;
use crate::packages::{InstalledVersions, PackageId, PackageStore};
use crate::revocation::{CertId, Facts, Revocations, Signed, SubjectValues};

mod approval;
mod delivery;
mod held;
pub(crate) mod placement;
mod rollout;
mod session;
mod views;
pub use approval::{bad_request, unavailable, MEMBER_DOES_NOT_ENROL, NEEDS_A_PROOF, RETRY_AFTER};
pub use delivery::{StoreFull, UploadReservation};
pub use rollout::{DeploymentReach, PendingConfigurationView, PendingPackageView};
pub use views::{
    AgentListing, AgentQuery, AgentView, HostAgentView, HostAgents, PackageStatusView,
};

/// The package upload limit in force when nothing configures one — roomy, because a real agent
/// binary is (see `server.toml`, `max_package_size_bytes`).
pub const DEFAULT_MAX_PACKAGE_SIZE: usize = 1024 * 1024 * 1024; // 1 GiB

/// The whole-store limit in force when nothing configures one: sixteen per-artifact budgets, room
/// for a real package set across platforms with rollback copies, while still bounding the disk a
/// caller can fill by uploading under many names (see `server.toml`, `max_total_package_bytes`).
pub const DEFAULT_MAX_TOTAL_PACKAGE_SIZE: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB

/// Three times the Baseline's own default heartbeat of 30 seconds (SRV-fleet-record-agents-hosts).
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(90);

/// The most Agent records the fleet holds when nothing configures a ceiling (see `server.toml`,
/// `max_agents`). Generous enough that a real fleet never meets it, low enough that the in-memory
/// map and the per-Agent files it mirrors to disk stay bounded when an unauthenticated endpoint is
/// flooded with fresh, self-asserted UIDs (SEC-admission-by-client-certificate).
pub const DEFAULT_MAX_AGENTS: usize = 100_000;

/// The heartbeat interval an Agent is held to where this Server offers none: the OpAMP default,
/// 30 seconds.
const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(30);

/// The Capability Set this Server declares (see docs/CONFORMANCE.md).
pub const SERVER_CAPABILITIES: u64 = ServerCapabilities::AcceptsStatus as u64
    | ServerCapabilities::OffersRemoteConfig as u64
    | ServerCapabilities::AcceptsEffectiveConfig as u64;

/// Identifies one WebSocket connection for the duplicate detection the Baseline asks of the
/// Server. Never a routing key — Agents are routed by `instance_uid` alone (CLT-modes-and-gateway); this only
/// answers "is this identity already alive on *another* connection?".
pub type ConnId = u64;

/// The result of processing one `AgentToServer`: the reply to send back on the same transport, and
/// what the transport layer needs to know for its own bookkeeping.
pub struct Processed {
    pub reply: ServerToAgent,
    /// The identity the Agent goes by *after* this message (it may have been reassigned).
    pub uid: Option<InstanceUid>,
    /// The Agent said goodbye; a WebSocket loop drops it from its connection-local set.
    pub disconnected: bool,
}

// What a report's end answers with, which `session` and `approval` share: the module root names
// no context, and `approval` ranks below `session` (SRV-fleet-contexts-call-by-rank).

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
    /// (SRV-instance-opens-chosen-planes §8).
    superseded: bool,
    identification: Option<AgentIdentification>,
    reply_flags: u64,
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
    /// The outcome of the last connection-settings offer this Agent reported (SEC-connection-settings-never-plaintext); its
    /// hash is what gates re-offering.
    pub connection_settings_status: Option<ConnectionSettingsStatus>,
    /// The package statuses this Agent last reported (PKG-signed-packages-and-delivery); the
    /// `server_provided_all_packages_hash` inside is what gates re-offering packages.
    pub package_statuses: Option<PackageStatuses>,
    /// The WebSocket connection currently carrying this Agent; `None` for plain HTTP, whose
    /// polling is stateless. Only the owning connection may mark the Agent disconnected, and a
    /// report from a *different* live connection is the duplicate the Baseline wants detected.
    pub owner: Option<ConnId>,
    /// When the connection that carries it last carried a report of it, in milliseconds since the
    /// epoch — runtime-only. An older connection of its host takes it back once this is a
    /// heartbeat interval old
    /// (SRV-instance-opens-chosen-planes §8).
    pub carried_ms: u64,
    /// Whether this Server heard the Agent report since it started — runtime-only. What it holds
    /// of one it did not is what it loaded, which on a shared store another instance may have
    /// written since.
    pub heard: bool,
    /// The operator's labels for this Agent (SRV-fleet-record-agents-hosts), part of its persisted record (SRV-fleet-record-agents-hosts §21):
    /// they follow a re-key and leave with a forget.
    pub labels: BTreeMap<String, String>,
    /// The Host this Agent last arrived over (SRV-fleet-record-agents-hosts §16): its own on a direct connection,
    /// the Gateway's behind one. For the fleet view and label inheritance; it grants nothing.
    pub host: Option<String>,
    /// Where the Host and the Client Agent place this Agent — derived, never persisted.
    pub placement: Placement,
    /// The instance that holds it, as a shared store read it
    /// (SRV-instance-opens-chosen-planes §13);
    /// `None` for what this Server holds in memory.
    pub holder: Option<crate::agent_store::Holder>,
    /// What the operator's last rollout released to this Agent (SRV-deployments-and-pinned-rollouts §6). **Every offer
    /// is composed from this**; matching only proposes. Written by the rollout acts, persisted like
    /// `restart_pending` — operator intent that survives a restart (SRV-fleet-record-agents-hosts). `None` is what it
    /// says: nothing has been rolled out.
    pub assignment: Option<Assignment>,
}

/// One Agent's assignment (SRV-deployments-and-pinned-rollouts §6): one release, pinned as of the press that wrote it
/// and replaced whole by the next.
///
/// The Deployment is carried alongside the release rather than derived from it, because a
/// Deployment re-aimed or edited after the act must not change what an Agent was given, nor what
/// the fleet view says it was given through. The signature a package offer travels with is the
/// entry's own (PKG-signed-packages-and-delivery).
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

impl Assignment {
    /// Whether a stored assignment keeps the rules its write met, which both stores ask on load
    /// (SRV-state-stores-contract-pair §4): the Deployment's and each Configuration's name keep the name grammar
    /// of [`validate_name`](crate::configs::validate_name), and each pinned revision is named by
    /// a content hash, the form [`revision_hash`](crate::configs::revision_hash) gives. Whether
    /// the revision is still retained is not asked. Each store names the record before the
    /// message.
    ///
    /// # Errors
    /// Returns the rule the assignment breaks.
    pub fn check(&self) -> Result<(), String> {
        let deployment = &self.deployment;
        crate::configs::validate_name(deployment)
            .map_err(|e| format!("the assignment's deployment {deployment:?}: {e}"))?;
        for (name, revision) in &self.configurations {
            crate::configs::validate_name(name)
                .map_err(|e| format!("the assignment's configuration {name:?}: {e}"))?;
            if !crate::blobs::is_hash(revision) {
                return Err(format!(
                    "the assignment's configuration {name:?}: revision {revision:?} is no content \
                     hash, 64 lowercase hexadecimal digits"
                ));
            }
        }
        Ok(())
    }
}

/// The configuration map of an Agent nothing was rolled out to.
static NO_CONFIGURATIONS: BTreeMap<String, String> = BTreeMap::new();

impl AgentRecord {
    /// What a Selector is matched against (SRV-fleet-record-agents-hosts §18): the Host's values, what the Agent
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

    /// What this Agent last reported as installed, per package name (SRV-deployments-and-pinned-rollouts): the versions
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

    /// What of this record survives a restart (SRV-fleet-record-agents-hosts): everything report-derived or
    /// operator-queued, never what a live connection knows.
    /// Where it is connected, as a presence batch writes it.
    /// What its derived values come from
    /// (API-lists-paged-by-cursor §8).
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
    /// otherwise — connectedness is runtime-only (SRV-fleet-record-agents-hosts). Its placement is derived once the
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

/// Why forgetting an Agent was refused (`DELETE /api/v1/agents/{uid}`, SRV-fleet-record-agents-hosts).
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

/// Why a rollout act (SRV-deployments-and-pinned-rollouts §9) was refused.
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

/// Why deleting a Configuration was refused (SRV-deployments-and-pinned-rollouts §12).
#[derive(Debug)]
pub enum DeleteError {
    /// An Agent's assignment pins it; the message says how many.
    Pinned(String),
    /// The store could not be written.
    Storage(String),
}

/// The one `OpAMPConnectionSettings` this Server offers (SEC-connection-settings-never-plaintext), precompiled from the
/// `[connection_offer]` section with the hash that gates its delivery.
pub struct ConnectionOffer {
    settings: OpAmpConnectionSettings,
}

/// The own-telemetry destinations this Server offers (CLT-what-an-agent-reports), precompiled from
/// `[telemetry_offer]`. Part of the same `ConnectionSettingsOffers` message the OpAMP settings
/// ride, and hashed with them: one offer, one hash, one acknowledgement.
///
/// A field here is `Some` for every signal the section mentions, **including one it withdraws** —
/// a destination whose endpoint is empty (CLT-what-an-agent-reports). That is why a withdrawal counts as something
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
    /// send time, because an offer carries telemetry destinations too (CLT-what-an-agent-reports) and the Agent
    /// acknowledges the message rather than any one part of it.
    pub fn new(settings: OpAmpConnectionSettings) -> Self {
        ConnectionOffer { settings }
    }
}

/// The wall clock's port (DEV-ports-and-adapters): when an Agent was last heard from, and how long ago that
/// is (SRV-fleet-record-agents-hosts). The system clock ([`SystemClock`](crate::clock::SystemClock)) is what the
/// composition root wires.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// The port of the time that has passed, as a lease and a sweep measure it
/// (SRV-instance-opens-chosen-planes §3,
/// SRV-state-stores-contract-pair §14): a clock that counts the time the host was suspended, which the
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

/// The CSR flow's port (SEC-admission-by-client-certificate): what signs an Agent's certificate signing request. The local
/// CA ([`ClientCa`](crate::ca::ClientCa)) is the adapter the composition root wires when
/// `[client_ca]` is configured.
pub trait CertificateSigner: Send + Sync {
    /// The signed certificate, PEM, for a CSR in PEM, and what it says about itself. Its subject
    /// carries `subject` — the values copied from the bootstrap certificate or the predecessor
    /// (CLT-modes-and-gateway §23 and SEC-bootstrap-certificates-and-enrolment §12) — and never the request's. `use_case` is what
    /// asked for it, which the audit entry of a signature with a token key names
    /// (SEC-admission-by-client-certificate §38).
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

    /// The certificate a CSR proves it renews, when it carries a renewal proof (CLT-modes-and-gateway §18);
    /// `Ok(None)` when it carries none.
    ///
    /// # Errors
    /// Returns the `BadRequest` text for a proof that does not hold.
    fn renewal_proof(&self, csr_pem: &str) -> Result<Option<Facts>, String>;

    /// The issuer and serial of the bootstrap certificate a CSR's bootstrap proof names (CLT-modes-and-gateway §18),
    /// read without checking the proof; `Ok(None)` when it carries none.
    ///
    /// # Errors
    /// Returns the `BadRequest` text for a proof that does not parse.
    fn bootstrap_proof_id(&self, csr_pem: &str) -> Result<Option<CertId>, String>;

    /// Checks the request's claims to an `instance_uid` against its sender's (SEC-admission-by-client-certificate).
    ///
    /// # Errors
    /// Returns the `BadRequest` text for a request that claims another identity.
    fn check_claims(&self, csr_pem: &str, sender: &[u8]) -> Result<(), String>;
}

// The types of the state the contexts keep in `AppState` and its records: the module root names
// no context (SRV-fleet-contexts-call-by-rank), so each stands here, and its methods stay with the
// context that owns it.

/// The Agents one Client presents (SRV-fleet-record-agents-hosts §19), as far as the Server can tell them apart.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    /// On a direct connection: the Agents arriving over the same Host.
    Host(String),
    /// Behind a Gateway: the Agents arriving over the same Gateway Host that report the same
    /// machine — `host.id`, else `host.name` ([`crate::labels::machine`]).
    Behind { gateway: String, machine: String },
}

/// What the Host an Agent arrived over and its Client Agent add to its effective description
/// (SRV-fleet-record-agents-hosts §18 and §19). Derived from the records and the host register whenever either
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

/// Which Agents each [`Group`] holds (SRV-fleet-record-agents-hosts §19), so placing one Agent touches its group
/// alone. Locked strictly after `fleet`, and only while it is held.
#[derive(Default)]
struct Groups(Mutex<HashMap<Group, HashSet<InstanceUid>>>);

/// How many locks the Agents' own locks are spread over: two Agents share one only by their
/// `instance_uid`, and then one waits for the other's report.
const AGENT_LOCKS: usize = 1024;

/// A re-key of this instance whose answer was lost and whose read back failed: the identity it
/// moved the record to, if it committed, and whether a read since found it did.
#[derive(Clone, Copy, Debug)]
struct PendingRekey {
    new: InstanceUid,
    moved: bool,
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
    /// counted since (SRV-fleet-record-agents-hosts §2). No batch names this instance by it. A count answered since
    /// in `takes` outranks it, so an answer leaves it as it is.
    before_lost: HashMap<InstanceUid, u64>,
    generations: HashMap<InstanceUid, u64>,
    generation: u64,
}

/// What an instance keeps of each Agent beside its record and its group: one entry of four parts,
/// each keyed by the Agent's `instance_uid`. A forget drops it ([`Kept::forget`]) and a re-key
/// moves it ([`Kept::rekey`]) as one, each naming every part, so a part added later does not
/// compile until both say what becomes of it. Each part keeps its own lock, every one taken
/// strictly after `fleet`.
struct Kept {
    /// Each persisted record's durable digest as last written — the dirty check that keeps a
    /// heartbeat from reaching any storage backend (SRV-fleet-record-agents-hosts).
    written: Mutex<HashMap<InstanceUid, [u8; 32]>>,
    /// The presence of each held Agent as the last batch wrote it, and the holder it named, so a
    /// batch writes only what moved since.
    presence_written: Mutex<PresenceWritten>,
    /// The re-keys whose answer was lost and whose read back failed, by the old identity: the new
    /// one each moved to, if it committed, which the Agent's next report reads back and assigns.
    unsure_rekeys: Mutex<HashMap<InstanceUid, PendingRekey>>,
    /// The restarts whose command was not sent and whose put-back the store did not take, each
    /// with when it is tried again ([`retry_restarts`](AppState::retry_restarts)).
    unput_restarts: Mutex<HashMap<InstanceUid, Backoff>>,
}

impl Kept {
    /// Starts with `written`, the digests of the records restored, and nothing else.
    fn with_written(written: HashMap<InstanceUid, [u8; 32]>) -> Self {
        Kept {
            written: Mutex::new(written),
            presence_written: Mutex::new(PresenceWritten::default()),
            unsure_rekeys: Mutex::new(HashMap::new()),
            unput_restarts: Mutex::new(HashMap::new()),
        }
    }
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

/// The package store and the Deployments that release from it (PKG-signed-packages-and-delivery). Every `download_url`
/// is a path the Client resolves against its own endpoint — the Agent plane, where the download
/// is served behind the same handshake as `/v1/opamp` (PKG-signed-packages-and-delivery).
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
    /// Where Agent records survive a restart (SRV-fleet-record-agents-hosts) — the port, never a concrete backend:
    /// the filesystem adapter is merely what [`AppState::new`] wires by default.
    agent_store: Box<dyn AgentStore>,
    /// What this instance keeps of each Agent beside its record and its group, dropped by a forget
    /// and moved by a re-key as one entry. Its parts are locked strictly after `fleet`.
    kept: Kept,
    configs: ConfigStore,
    /// Which Agents each [`Group`] holds, owned by [`placement`].
    groups: Groups,
    push: watch::Sender<u64>,
    /// Hands every WebSocket connection its identity for the duplicate detection.
    next_conn: AtomicU64,
    /// The connection settings offered to the fleet (SEC-connection-settings-never-plaintext); `None` offers nothing and leaves
    /// `OffersConnectionSettings` undeclared.
    connection_offer: Option<ConnectionOffer>,
    /// The packages offered to the fleet (PKG-signed-packages-and-delivery); `None` offers nothing and leaves
    /// `OffersPackages` undeclared.
    packages: Option<PackageOffering>,
    /// The authority that signs Agent CSRs (SEC-admission-by-client-certificate); `None` signs nothing and leaves
    /// `AcceptsConnectionSettingsRequest` undeclared.
    client_ca: Option<Box<dyn CertificateSigner>>,
    /// The enrolment window and its queue (SEC-admission-by-client-certificate); `None` while `[enrolment]` is not set, and no
    /// host enrols.
    enrolment: Option<Arc<Enrolment>>,
    /// The bootstrap certificates the Server issued (SEC-bootstrap-certificates-and-enrolment); `None` while `[enrolment]` is not
    /// set.
    bootstrap: Option<Arc<crate::bootstrap::BootstrapCertificates>>,
    /// What the client CA signed and what is revoked (SEC-admission-by-client-certificate); `None` only where a test serves
    /// without it.
    revocations: Option<Arc<Revocations>>,
    /// The floor of each Agent type (PKG-versions-high-water-mark-floor); `None` only where a test serves without it.
    floors: Option<Arc<crate::floors::PackageFloors>>,
    /// The audit record every security decision goes to (SEC-append-only-audit-record); `None` only in tests.
    audit: Option<Arc<dyn Audit>>,
    /// How often an admitted peer may be heard (SEC-tls13-bounded-input-chosen-planes); `None` only in tests.
    agent_rate: Option<Arc<crate::agent_rate::AgentRate>>,
    /// When an Agent is heard from, and how long ago that was.
    clock: Box<dyn Clock>,
    /// Where Agents send their own telemetry (CLT-what-an-agent-reports); empty offers no destination.
    telemetry_offer: TelemetryOffer,
    /// The message size limit both transports enforce, in each direction (the Baseline's MUST).
    max_message_size: usize,
    /// The largest package artifact the REST API accepts on upload (PKG-signed-packages-and-delivery) — a program, not a
    /// message, so it is bounded separately and far more generously.
    max_package_size: usize,
    /// The total size of all stored artifacts the REST API keeps before it refuses a new upload
    /// (PKG-signed-packages-and-delivery): what bounds the store — and so the disk — against many uploads under distinct
    /// names, where `max_package_size` bounds only one.
    max_total_package_bytes: u64,
    /// The bytes of uploads still arriving, counted against `max_total_package_bytes` beside the
    /// stored artifacts, so uploads sent at once cannot pass the ceiling together.
    uploading_bytes: AtomicU64,
    /// How long an Agent that promised to report periodically may be silent before the fleet view
    /// calls it stale (SRV-fleet-record-agents-hosts). Overridden by an offered heartbeat interval, which is the period
    /// this Server actually asked for.
    stale_after: Duration,
    /// The most Agent records the fleet holds at once. A report bearing a new `instance_uid` past
    /// this ceiling is refused `Unavailable` rather than admitted, so a peer cycling fresh UIDs —
    /// each of which would pin an in-memory record and a persisted file — cannot exhaust memory or
    /// disk (a self-asserted UID is free to mint, SEC-admission-by-client-certificate). Existing Agents keep reporting; only
    /// growth past the ceiling is refused. The real defence against a flood is admission by client
    /// certificate (SEC-admission-by-client-certificate); this is the backstop that bounds what an admitted peer can do.
    max_agents: usize,
    /// How often an instance on a shared store writes the presence of the Agents it carries
    /// (SRV-fleet-record-agents-hosts §4); zero where the store keeps none.
    /// What is stored may be one interval old, so it adds to the staleness budget (clause 8).
    presence_interval: Duration,
    /// The lease this instance serves under on a shared store, whose instance id the presence
    /// written at a connecting report names as the holder; unset where there is none.
    lease: OnceLock<watch::Receiver<Option<String>>>,
    /// Whether that lease is held by its clock now, which each message of an Agent checks before it
    /// is answered; unset where there is no lease.
    proof: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
    /// The Agents forgotten while a WebSocket carried them, by that connection, until its loop
    /// takes them and ends their session (SRV-fleet-record-agents-hosts §10). Locked strictly after `fleet`.
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

impl AppState {
    /// The state on these stores, built as [`with_stores`](Self::with_stores) builds it but with no
    /// Agent placed yet.
    fn unplaced(
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
        // whether it is still there is not (SRV-fleet-record-agents-hosts) — and in the channel its labels put it in.
        //
        // A record carrying no assignments is simply one nothing has been rolled out to. There is
        // no seed to run: the store this Server would have migrated from is not supported, so an
        // absent assignment means what it says rather than "not migrated yet".
        //
        // On a shared store an instance holds only the Agents it carries, each loaded at its first
        // report here, and loads none at startup
        // (SRV-instance-opens-chosen-planes §8).
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
        // A rollout cut short leaves at most a revision no assignment references (SRV-state-stores-contract-pair §2);
        // it is collected here, before anything is offered.
        for config in configs.list() {
            configs.retain_only(
                &config.name,
                &agent_store.referenced_revisions(&config.name),
            )?;
        }
        Ok(AppState {
            fleet: Mutex::new(fleet),
            agent_store,
            kept: Kept::with_written(written),
            configs,
            groups: Groups::default(),
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
            lease: OnceLock::new(),
            proof: OnceLock::new(),
            forgotten: Mutex::new(HashMap::new()),
            agent_locks: (0..AGENT_LOCKS).map(|_| Mutex::new(())).collect(),
            agent_lock_waits: AtomicU64::new(0),
            taking: Mutex::new(HashSet::new()),
            reserved_new: AtomicUsize::new(0),
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

    /// Sets the largest package artifact the REST API accepts on upload (PKG-signed-packages-and-delivery).
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
    /// (PKG-signed-packages-and-delivery).
    #[must_use]
    pub fn with_max_total_package_bytes(mut self, limit: u64) -> Self {
        self.max_total_package_bytes = limit;
        self
    }

    /// The whole-store size limit in force, for the REST API's upload route.
    pub fn max_total_package_bytes(&self) -> u64 {
        self.max_total_package_bytes
    }

    /// Arms the connection-settings offer (SEC-connection-settings-never-plaintext); with it the Server declares
    /// `OffersConnectionSettings`.
    #[must_use]
    pub fn with_connection_offer(mut self, offer: Option<ConnectionOffer>) -> Self {
        self.connection_offer = offer;
        self
    }

    /// Sets how long a heartbeating Agent may be silent before it reads as stale (SRV-fleet-record-agents-hosts).
    #[must_use]
    pub fn with_stale_after(mut self, stale_after: Duration) -> Self {
        self.stale_after = stale_after;
        self
    }

    /// Sets the most Agent records the fleet holds at once — the backstop against a peer minting
    /// fresh UIDs to exhaust memory and disk (SEC-admission-by-client-certificate §12).
    #[must_use]
    pub fn with_max_agents(mut self, max_agents: usize) -> Self {
        self.max_agents = max_agents;
        self
    }

    /// Sets how often the presence of the Agents this instance carries is written to a shared
    /// store (SRV-fleet-record-agents-hosts §4), which the staleness budget
    /// adds (clause 8).
    #[must_use]
    pub fn with_presence_interval(mut self, interval: Duration) -> Self {
        self.presence_interval = interval;
        self
    }

    /// Names the lease this instance serves under on a shared store: the presence it writes when
    /// an Agent connects here names that lease's instance id as the holder, at once
    /// (SRV-fleet-record-agents-hosts §11). Set once; a later call is
    /// ignored.
    pub fn serve_under(&self, lease: watch::Receiver<Option<String>>) {
        let _ = self.lease.set(lease);
    }

    /// Answers the messages of Agents only while `holds` answers that the lease is held by its
    /// clock
    /// (SRV-instance-opens-chosen-planes §3).
    /// Set once; a later call is ignored.
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
    /// stored may be one interval old (SRV-fleet-record-agents-hosts §8).
    fn stale_after(&self) -> Duration {
        self.heartbeat_budget() + self.presence_interval
    }

    /// One heartbeat interval: the one this Server offers, else 30 seconds, the default interval
    /// a WebSocket's silence is judged by without one
    /// (API-silent-websocket-is-ended §2).
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

    /// Offers the fleet somewhere to send its own telemetry (CLT-what-an-agent-reports).
    #[must_use]
    pub fn with_telemetry_offer(mut self, offer: TelemetryOffer) -> Self {
        self.telemetry_offer = offer;
        self
    }

    /// Arms the CSR flow (SEC-admission-by-client-certificate); with it the Server declares
    /// `AcceptsConnectionSettingsRequest` and signs the requests Agents send.
    #[must_use]
    pub fn with_client_ca(mut self, client_ca: Option<impl CertificateSigner + 'static>) -> Self {
        self.client_ca = client_ca.map(|ca| Box::new(ca) as Box<dyn CertificateSigner>);
        self
    }

    /// Arms enrolment (SEC-admission-by-client-certificate): a host with a bootstrap certificate may ask for its first one,
    /// and an operator decides.
    #[must_use]
    pub fn with_enrolment(mut self, enrolment: Option<Arc<Enrolment>>) -> Self {
        self.enrolment = enrolment;
        self
    }

    /// Arms the issuing of bootstrap certificates and their record (SEC-bootstrap-certificates-and-enrolment).
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

    /// The register and the revocation list.
    pub fn revocations(&self) -> Option<&Arc<Revocations>> {
        self.revocations.as_ref()
    }

    /// Arms the floors (PKG-versions-high-water-mark-floor).
    #[must_use]
    pub fn with_floors(mut self, floors: Option<Arc<crate::floors::PackageFloors>>) -> Self {
        self.floors = floors;
        self
    }

    /// The floor of each Agent type.
    pub fn floors(&self) -> Option<&Arc<crate::floors::PackageFloors>> {
        self.floors.as_ref()
    }

    /// Arms the audit record (SEC-append-only-audit-record).
    #[must_use]
    pub fn with_audit(mut self, audit: Option<Arc<dyn Audit>>) -> Self {
        self.audit = audit;
        self
    }

    /// The audit record.
    pub fn audit(&self) -> Option<&Arc<dyn Audit>> {
        self.audit.as_ref()
    }

    /// Arms the rate limit on the Agent plane (SEC-tls13-bounded-input-chosen-planes).
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
    /// (SEC-append-only-audit-record §5).
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

    /// The enrolment window and its queue, while `[enrolment]` is set.
    pub fn enrolment(&self) -> Option<&Arc<Enrolment>> {
        self.enrolment.as_ref()
    }

    /// Arms package delivery (PKG-signed-packages-and-delivery); with a non-empty store the Server declares
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

    /// Read access to the Deployment store, armed with the package store (PKG-signed-packages-and-delivery).
    pub fn deployment_store(&self) -> Option<&DeploymentStore> {
        self.packages.as_ref().map(PackageOffering::deployments)
    }

    /// The Capability Set this Server declares: the base set, plus `OffersConnectionSettings`
    /// while there is anything to offer and `OffersPackages` / `AcceptsPackagesStatus`
    /// while a non-empty package store is armed — an undeclared capability is never exercised, a
    /// declared one never hollow.
    fn capabilities(&self) -> u64 {
        let mut caps = SERVER_CAPABILITIES;
        // All three ways a `ConnectionSettingsOffers` leaves this Server (SEC-connection-settings-never-plaintext §3): the
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
}

/// The Instance UID a host register names in hex, `None` where it is not one.
fn parse_hex_uid(uid: &str) -> Option<InstanceUid> {
    Some(InstanceUid(
        <[u8; 16]>::try_from(hex::decode(uid).ok()?).ok()?,
    ))
}

/// Whether nothing has been heard from this Agent for longer than its budget (SRV-fleet-record-agents-hosts).
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
/// whether it is safe to forget (SRV-fleet-record-agents-hosts) is a question about evidence, not about promises: an
/// Agent nobody has heard from cannot be disturbed by being forgotten, whatever it once declared.
fn is_silent(record: &AgentRecord, budget: Duration, now_ms: u64) -> bool {
    now_ms.saturating_sub(record.last_seen_ms) > budget.as_millis() as u64
}

// The test helpers the contexts share.
#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {

    /// The host derived for a certificate that names none is the one this Server derived before
    /// the derivation moved into `fleet-core`: hosts already in the register keep their ids.
    /// Verifies: CLT-modes-and-gateway
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

    /// The gatewayed case, which is why this exists: the connection is up — it is the Gateway's —
    /// and the Agent behind it has stopped talking. Both facts are reported, neither overwrites
    /// the other (SRV-fleet-record-agents-hosts).
    // Verifies: SRV-fleet-record-agents-hosts
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
    /// Verifies: SEC-connection-settings-never-plaintext
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

    use super::*;
    use crate::deployments;
    use crate::fleet::fixtures::*;
    use crate::revocation::Presented;
    use opamp::proto::{AgentToServer, AgentToServerFlags};

    // ---- Hosts, the effective description and label inheritance (SRV-fleet-record-agents-hosts) ----

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

    /// An Agent on a direct connection carries its Host's subject values as `fleet.host.*`; an
    /// Agent that reports `fleet.host.organization` itself is not selected by it, and the attribute
    /// is listed as ignored. Without a host register an Agent names no Host.
    /// Verifies: SRV-fleet-record-agents-hosts
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
    /// Verifies: SRV-fleet-record-agents-hosts
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

    /// A label in the Supervisor's space that the record already holds — stored before the Server
    /// refused one — is ignored after a restart: no Selector matches it on the Agent or on its
    /// Client's other Agents, it is not inherited, the fleet row lists it as ignored, and the next
    /// label set put without it removes it.
    /// Verifies: SRV-fleet-record-agents-hosts
    #[test]
    fn a_stored_label_in_the_supervisors_space_matches_nothing() {
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
        }
        let store = crate::fs::FsAgentStore::open(&dir.path().join("configs")).expect("open");
        let stored = BTreeMap::from([
            ("supervisor.kind.x".to_string(), "true".to_string()),
            ("ring".to_string(), "early".to_string()),
        ]);
        for uid in [client, supervisor] {
            assert!(store.set_labels(&uid, &stored).expect("stored"));
        }
        let state = hosted_fleet(dir.path());
        for uid in [client, supervisor] {
            assert!(!selects(&state, &uid, &[("supervisor.kind.x", "true")]));
            assert!(selects(&state, &uid, &[("ring", "early")]));
            let fleet = state.fleet.lock().expect("fleet lock");
            let effective = fleet[&uid].effective_description();
            assert!(
                !crate::labels::reported_keys(effective.as_deref()).contains(&"supervisor.kind.x"),
                "it never reaches the effective description"
            );
            drop(fleet);
            assert_eq!(view(&state, &uid).ignored_labels, ["supervisor.kind.x"]);
        }
        assert_eq!(
            view(&state, &supervisor).inherited_labels,
            BTreeMap::from([("ring".to_string(), "early".to_string())])
        );
        label(&state, &client, &[("ring", "early")]);
        assert!(view(&state, &client).ignored_labels.is_empty());
    }

    /// Behind a Gateway an Agent carries no `fleet.host.*`; the Agents sharing a `host.id` inherit
    /// their Client Agent's labels, and two Client Agents with one `host.id` leave every Agent of it
    /// inheriting nothing and marked ambiguous.
    /// Verifies: SRV-fleet-record-agents-hosts
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
    /// Verifies: SRV-fleet-record-agents-hosts
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
}
