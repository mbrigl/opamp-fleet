//! The views of the fleet: the Agents each Host carries (SRV-fleet-record-agents-hosts §24), the views of an
//! Agent and of the whole fleet, the list of Agents a page at a time (API-lists-paged-by-cursor §3–§8), and
//! the version a reader of the fleet table is shown (OPS-cargo-decides-product-version). Its entry points are methods of
//! `AppState`.

use std::collections::{BTreeMap, HashMap};

use opamp::attributes;
use opamp::uid::InstanceUid;
use serde::Serialize;

use crate::configs::DesiredConfig;
use crate::deployments::{deployment_for, Deployment, DeploymentStore};
use crate::listing::capability_names;
use crate::packages::PackageId;

use super::placement::{Group, Placement};
use super::rollout::{waiting, Waiting};
use super::{is_stale, AgentRecord, AppState};

impl AppState {
    /// The Agents each Host carries (SRV-fleet-record-agents-hosts §24) — those whose record names it as the Host
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
                    .and_then(|d| attributes::reported(d, key))
                    .unwrap_or_default()
                    .to_string()
            };
            // The names as the list derives them (CLT-what-an-agent-reports §1 and §2), so the two cannot
            // differ.
            let (service_name, service_instance_name) =
                crate::listing::names(record.description.as_ref());
            let entry = hosts.entry(host.clone()).or_default();
            let is_client_agent = record.placement.client_agent == Some(*uid);
            if is_client_agent && record.placement.group == Some(Group::Host(host.clone())) {
                entry.name = Some(reported("host.name")).filter(|name| !name.is_empty());
            }
            entry.agents.push(HostAgentView {
                instance_uid: uid.to_string(),
                service_name,
                service_instance_name,
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
    /// says (SRV-instance-opens-chosen-planes §13).
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
    /// (API-lists-paged-by-cursor §6).
    pub fn keeps_holders(&self) -> bool {
        self.agent_store.is_shared()
    }

    /// One page of the list of Agents
    /// (API-lists-paged-by-cursor §3–§6 and §8):
    /// on a store this Server alone writes, cut from what it holds; on a
    /// shared store read through
    /// [`AgentStore::query_page`](crate::agent_store::AgentStore::query_page), each Agent placed
    /// under the Client Agent the store found for it and with the values of the Host it arrived
    /// over.
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
    /// (API-lists-paged-by-cursor §7):
    /// from memory on a store this Server alone writes, through the port on a shared one.
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
        // "a channel with nothing for me" are not the same empty row (PKG-signed-packages-and-delivery §27).
        let claiming_deployment = claimed.map(|d| d.name.clone()).unwrap_or_default();

        // What a press would release to it now (SRV-deployments-and-pinned-rollouts §9). A conflict, or no
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

/// What the list of Agents is asked
/// (API-lists-paged-by-cursor §3–§6).
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

/// The Agents one Host carries, and the name it is shown by (SRV-fleet-record-agents-hosts §24).
#[derive(Default)]
pub struct HostAgents {
    /// The `host.name` its Client Agent reports; `None` for a Gateway, and while it has no single
    /// Client Agent.
    pub name: Option<String>,
    pub agents: Vec<HostAgentView>,
}

/// One Agent under the Host it arrived over (SRV-fleet-record-agents-hosts §24): enough to group it by its
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
    /// (CLT-what-an-agent-reports). For a Supervisor-backed Agent this is the type its Supervisor states, never one
    /// its Managed Process reports, so every Agent of one configured type reports the same value.
    /// It answers "what is this", never "which one is this": that is
    /// [`service_instance_name`](Self::service_instance_name).
    pub service_name: String,
    /// The operator's name for this Agent — the `[[supervisor]]` block's `name` (CLT-what-an-agent-reports). Empty
    /// for a foreign OpAMP client that reports no `service.instance.name`, which is why the UI
    /// falls back through the type to the UID rather than showing a blank row.
    pub service_instance_name: String,
    /// The release the Agent reports — `MAJOR.MINOR.PATCH`, with the pre-release when it is not a
    /// release build (OPS-cargo-decides-product-version). This is what belongs in a column headed "Version"; the commit the
    /// build came from is [`service_build`](Self::service_build). A reported value that is not a
    /// version at all is passed through unchanged, since a Foreign Agent numbers itself however it
    /// likes.
    pub service_version: String,
    /// Exactly what the Agent reported, commit metadata and all — the answer to "which build is on
    /// that host", which is a question a fleet exists to answer (OPS-cargo-decides-product-version).
    pub service_build: String,
    /// The reported `os.description` (e.g. "Ubuntu 24.04.2 LTS"), falling back to `os.type`.
    pub os: String,
    /// The platform this Agent is offered packages for, `os.type`-`host.arch` as the offer reads
    /// and folds them (`linux-amd64`), an identifying copy standing in for a non-identifying one;
    /// empty when it reports neither, so it is offered no package. The fleet search finds the
    /// Agent by this value.
    pub platform: String,
    /// Every reported identifying attribute — what a Selector can match on (SRV-configurations-compose-one-map).
    pub identifying_attributes: BTreeMap<String, String>,
    /// Every reported non-identifying attribute — Selectors match these too.
    pub non_identifying_attributes: BTreeMap<String, String>,
    /// The Configurations a press of the Deployment claiming this Agent would release to it — the
    /// ones it holds that fit this Agent's type (SRV-deployments-and-pinned-rollouts), in name order. Never what it runs;
    /// that is [`assigned_configurations`](Self::assigned_configurations).
    pub matched_configurations: Vec<String>,
    /// The Configurations rolled out to this Agent (SRV-deployments-and-pinned-rollouts §6), in name order — what its
    /// offer is composed from.
    pub assigned_configurations: Vec<String>,
    /// The Deployment that claims this Agent **now** — whose Selector matches it — or empty when
    /// none does.
    ///
    /// This is not [`assigned_deployment`](Self::assigned_deployment), and the difference is what
    /// tells four states apart that would otherwise look alike (PKG-signed-packages-and-delivery §27). Empty here
    /// with no conflict means the host is in **no channel**: label it, or give it a `channel`
    /// attribute. Set here with nothing assigned and nothing pending means the channel holds
    /// nothing this Agent can take — no Package for its type, or none for its platform. The
    /// operator's next move differs in each case, which is why the Server says which one it is
    /// rather than showing an empty row three ways.
    pub deployment: String,
    /// The Deployment this Agent's release went **through** (SRV-deployments-and-pinned-rollouts §6), or empty when
    /// nothing has been rolled out to it. Pinned as of that act, so it may name a channel that no
    /// longer claims this Agent.
    pub assigned_deployment: String,
    /// The Package rolled out to this Agent (SRV-deployments-and-pinned-rollouts), as `<agent type>@<version>`, or empty.
    ///
    /// It is pinned as of the act that released it: re-aiming its Deployment afterwards, or
    /// putting a newer Package in that channel, changes what is *proposed* and never what this Agent
    /// was already given.
    pub assigned_package: String,
    /// The Configurations waiting for a rollout act toward this Agent (SRV-deployments-and-pinned-rollouts §8):
    /// one its Deployment holds and it is not assigned (`change: "new"`), one whose
    /// saved revision differs from the assigned one (`change: "update"`), one assigned that the
    /// next press drops from a release that keeps something else (`change: "remove"`), or one
    /// assigned while the release holds nothing for the Agent, so the press leaves it with no
    /// assignment and it keeps running what it runs (`change: "kept"`). The Server never acts on
    /// this by itself.
    pub pending_configurations: Vec<PendingConfigurationView>,
    /// The Package waiting for a rollout act toward this Agent (SRV-deployments-and-pinned-rollouts §8) — at most one:
    /// the candidate of the Deployment that claims it, when that is not what it is assigned, or
    /// the assigned one the next press drops (`change: "remove"`, or `"kept"` as for a
    /// Configuration).
    pub pending_packages: Vec<PendingPackageView>,
    /// The Deployment the next press releases through, when the assignment names another one or
    /// none (SRV-deployments-and-pinned-rollouts §9); empty otherwise. A release whose content is unchanged still moves
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
    /// The Agent's package installations (PKG-signed-packages-and-delivery), in name order; empty until reported.
    pub packages: Vec<PackageStatusView>,
    /// Why this Agent is proposed nothing — more than one Deployment claims it, and an Agent
    /// belongs to at most one (PKG-signed-packages-and-delivery §28). The message names every Deployment in the way.
    /// Absent when at most one claims it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_conflict: Option<String>,
    /// What the Agent said about the *offer* rather than about a package it holds — an offer it
    /// refuses outright has no package status to carry the reason, and the Client's own Agent
    /// refusing a package it was not configured to take (CLT-self-update-signed-package) is exactly that case. Empty
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
    /// Nothing has been heard from this Agent for longer than its staleness budget (SRV-fleet-record-agents-hosts).
    ///
    /// Beside [`connected`](Self::connected), never instead of it: that one says a connection
    /// carrying this Agent is open — behind a Gateway, the *Gateway's* — and this one says whether
    /// the Agent itself is still talking. `connected: true, stale: true` is precisely the gatewayed
    /// case, and precisely what an operator needs to be told.
    ///
    /// Only an Agent declaring `ReportsHeartbeat` can be stale: that capability is the promise that
    /// makes silence mean something. Derived on read, never stored.
    pub stale: bool,
    /// The operator's labels on this Agent (SRV-fleet-record-agents-hosts) — matched by Selectors exactly like a
    /// reported attribute, but set here rather than in `supervisor.toml` on the host, so moving a host
    /// between rollout channels is an API call instead of an edit and a restart.
    pub labels: BTreeMap<String, String>,
    /// Labels this Agent's own reports shadow: set, matching nothing, and therefore doing nothing.
    ///
    /// Reported attributes always win (SRV-fleet-record-agents-hosts) — they decide which artifact fits this machine.
    /// A collision is refused when the label is set, so this fills only when an Agent *starts*
    /// reporting a key that was labelled earlier. Shown rather than dropped in silence.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shadowed_labels: Vec<String>,
    /// Labels stored with a key in `fleet.` or `supervisor.`, which no label may speak in
    /// (SRV-fleet-record-agents-hosts §20): ignored by every Selector and by
    /// package matching, and inherited by no Agent. Listed rather than dropped in silence; the next
    /// label set put without them removes them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignored_labels: Vec<String>,
    /// The labels of this Agent's Client Agent, which hold for it below its own (SRV-fleet-record-agents-hosts §19);
    /// empty for the Client Agent itself, for an Agent whose Client the Server cannot
    /// tell, and for an ambiguous one.
    pub inherited_labels: BTreeMap<String, String>,
    /// The Instance UID of this Agent's Client Agent — its own, for the Client Agent — or empty
    /// when the Server cannot tell.
    pub client_agent: String,
    /// The group this Agent's Client is recognised in holds more than one Client Agent, so it
    /// inherits nothing (SRV-fleet-record-agents-hosts §19).
    pub ambiguous: bool,
    /// The Host this Agent last arrived over (SRV-fleet-record-agents-hosts §16), or empty. Behind a Gateway, the
    /// Gateway's.
    pub host: String,
    /// The instance id of the instance that holds it under a live lease
    /// (SRV-instance-opens-chosen-planes §13);
    /// empty without one, and always on the filesystem store.
    pub holder: String,
    /// The host name that instance's lease records; empty when `holder` is.
    pub holder_host: String,
    /// The `fleet.host.*` values its effective description takes from that Host (SRV-fleet-record-agents-hosts §18);
    /// empty behind a Gateway.
    pub host_attributes: BTreeMap<String, String>,
    /// Reported attributes in the Server's `fleet.` space, left out of what Selectors match
    /// (SRV-fleet-record-agents-hosts §20).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignored_attributes: Vec<String>,
}

/// One Configuration waiting for a rollout act toward one Agent (SRV-deployments-and-pinned-rollouts).
#[derive(Serialize)]
pub struct PendingConfigurationView {
    pub name: String,
    /// `new` — held by the claiming Deployment and not yet assigned; `update` — the saved
    /// revision differs from the assigned one; `remove` — assigned, and dropped by the next press,
    /// which applies the smaller map; `kept` — assigned, and the next press leaves the Agent with no
    /// assignment, so it keeps running it.
    pub change: String,
}

/// Whom one Deployment reaches in the fleet as reported so far (PKG-signed-packages-and-delivery): the Agents it claims,
/// the subset a rollout act would actually change, and those another Deployment also claims.
#[derive(Clone, Copy, Default, Debug)]
pub struct DeploymentReach {
    /// Agents this Deployment claims — and no other does. Zero is the aim mistake worth hunting.
    pub claiming: usize,
    /// Of those, the Agents a press would actually move — what it would release differs from what
    /// their assignment pins (SRV-deployments-and-pinned-rollouts §9). Zero with a non-zero `claiming` means everyone is
    /// up to date.
    pub targeted: usize,
    /// Agents this Deployment matches that **another one matches too**. They are offered nothing
    /// new until an operator narrows a Selector (PKG-signed-packages-and-delivery §28).
    pub conflicting: usize,
}

/// The Package waiting for a rollout act toward one Agent (SRV-deployments-and-pinned-rollouts).
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

/// One package's installation state as the REST API and UI see it (PKG-signed-packages-and-delivery).
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
        let name = crate::listing::package_status_name(status.status);
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
        let shown = crate::listing::display_values(
            record.description.as_ref(),
            record.remote_config_status.as_ref(),
        );
        let status = record.remote_config_status.as_ref();
        // In sync means: runs exactly the composed set — trivially true when nothing matches,
        // since an unmatched Agent is deliberately left alone (goal 9).
        let in_sync = match desired {
            None => true,
            Some(d) => {
                status.map(|s| s.last_remote_config_hash.as_slice()) == Some(d.hash.as_slice())
            }
        };
        AgentView {
            instance_uid: uid.to_string(),
            service_name: shown.service_name,
            service_instance_name: shown.instance_name,
            // What the Agent said, and what a reader of a table wants out of it (OPS-cargo-decides-product-version).
            service_version: shown.version,
            service_build: shown.build,
            os: shown.os,
            platform: shown.platform,
            identifying_attributes: shown.identifying,
            non_identifying_attributes: shown.non_identifying,
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
            remote_config_status: shown.config.to_string(),
            remote_config_error: status.map(|s| s.error_message.clone()).unwrap_or_default(),
            in_sync,
            sequence_num: record.sequence_num,
            last_seen_ms: record.last_seen_ms,
            stale,
            shadowed_labels: crate::labels::shadowed(record.description.as_ref(), &record.labels),
            ignored_labels: crate::labels::ignored_labels(&record.labels),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::fixtures::*;
    use crate::revocation::Revocations;
    use opamp::proto::{AgentDescription, KeyValue, RemoteConfigStatus};
    use prost::Message as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

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

    // ---- The list of Agents, a page at a time, alike on every store ----

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
    /// Verifies: API-lists-paged-by-cursor
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
    /// Verifies: API-lists-paged-by-cursor
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

    /// The view names the platform an Agent is offered packages for as the offer reads it: folded
    /// to the canonical tokens, an identifying copy standing in, empty when it reports neither. The
    /// Agents tab shows it and the fleet search finds the Agent by it (API-lists-paged-by-cursor, SRV-configurations-compose-one-map).
    /// Verifies: API-lists-paged-by-cursor, SRV-configurations-compose-one-map
    #[test]
    fn the_view_names_the_platform_the_offer_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = hosted_fleet(dir.path());
        let none = crate::revocation::SubjectValues::default;
        let (raw, identifying, silent) = (
            InstanceUid([0xc1; 16]),
            InstanceUid([0xc2; 16]),
            InstanceUid([0xc3; 16]),
        );
        let service = [("service.name", "otelcol")];
        let in_identifying = [
            ("service.name", "otelcol"),
            ("os.type", "macos"),
            ("host.arch", "aarch64"),
        ];
        report_listed(
            &state,
            "h1",
            none(),
            raw,
            1,
            0,
            &service,
            &[("os.type", "linux"), ("host.arch", "x86_64")],
        );
        report_listed(
            &state,
            "h2",
            none(),
            identifying,
            1,
            0,
            &in_identifying,
            &[],
        );
        report_listed(&state, "h3", none(), silent, 1, 0, &service, &[]);
        assert_eq!(view(&state, &raw).platform, "linux-amd64");
        assert_eq!(view(&state, &identifying).platform, "darwin-arm64");
        assert_eq!(view(&state, &silent).platform, "");
    }

    /// What an Agent's listed record is sorted, filtered and searched by and what its view shows
    /// are one derivation: the service name, the instance name, the version, the OS (its
    /// description, else its type), the platform and the configuration status agree (API-lists-paged-by-cursor,
    /// OPS-cargo-decides-product-version).
    /// Verifies: API-lists-paged-by-cursor, OPS-cargo-decides-product-version
    #[test]
    fn an_agents_listed_record_and_its_view_show_the_same_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = hosted_fleet(dir.path());
        let pairs = |pairs: &[(&str, &str)]| -> Vec<KeyValue> {
            pairs
                .iter()
                .map(|(key, value)| opamp::attributes::string_attr(key, value))
                .collect()
        };
        let described = |identifying: &[(&str, &str)], non_identifying: &[(&str, &str)]| {
            Some(AgentDescription {
                identifying_attributes: pairs(identifying),
                non_identifying_attributes: pairs(non_identifying),
            })
        };
        let status = |status: opamp::proto::RemoteConfigStatuses| {
            Some(RemoteConfigStatus {
                status: status as i32,
                ..Default::default()
            })
        };
        let mut described_os = record_with(0);
        described_os.description = described(
            &[
                ("service.name", "otelcol"),
                ("service.version", "1.2.3-rc.1+abc123"),
            ],
            &[
                ("service.instance.name", "edge-1"),
                ("os.description", "Debian 12"),
                ("os.type", "linux"),
                ("host.arch", "x86_64"),
            ],
        );
        described_os.remote_config_status = status(opamp::proto::RemoteConfigStatuses::Failed);
        let mut typed_os = record_with(0);
        typed_os.description = described(
            &[
                ("service.name", "telegraf"),
                ("service.version", "0.9.0"),
                ("host.arch", "aarch64"),
            ],
            &[("service.instance.name", "edge-2"), ("os.type", "windows")],
        );
        typed_os.remote_config_status = status(opamp::proto::RemoteConfigStatuses::Applied);
        let (first, second) = (InstanceUid([0xd1; 16]), InstanceUid([0xd2; 16]));
        for (uid, record) in [(first, described_os), (second, typed_os)] {
            state.fleet.lock().expect("fleet lock").insert(uid, record);
        }
        for uid in [first, second] {
            let derived = {
                let fleet = state.fleet.lock().expect("fleet lock");
                crate::listing::derive(&fleet[&uid].source(&uid))
            };
            let shown = view(&state, &uid);
            let searched: Vec<&str> = derived.search.lines().collect();
            assert!(!shown.service_instance_name.is_empty() && !shown.platform.is_empty());
            assert_eq!(derived.service_name, shown.service_name);
            assert_eq!(derived.name, shown.service_instance_name);
            assert!(searched.contains(&shown.service_instance_name.to_lowercase().as_str()));
            assert_eq!(derived.version, shown.service_version);
            assert_eq!(derived.os, shown.os);
            assert!(searched.contains(&shown.platform.as_str()));
            assert_eq!(derived.config, shown.remote_config_status);
        }
        assert_eq!(view(&state, &first).os, "Debian 12");
        assert_eq!(view(&state, &first).service_version, "1.2.3-rc.1");
        assert_eq!(view(&state, &first).remote_config_status, "FAILED");
        assert_eq!(view(&state, &second).os, "windows");
        assert_eq!(view(&state, &second).platform, "windows-arm64");
        assert_eq!(view(&state, &second).remote_config_status, "APPLIED");
    }

    /// The Hosts view shows an Agent's service name and instance name as its listed record does:
    /// the service name only from the identifying attributes, the instance name only from the
    /// non-identifying ones (CLT-what-an-agent-reports §1 and §2), so an Agent that reports either in the
    /// other set shows it in neither view.
    /// Verifies: CLT-what-an-agent-reports, SRV-fleet-record-agents-hosts
    #[test]
    fn the_hosts_view_shows_an_agents_names_as_its_listed_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = hosted_fleet(dir.path());
        let pairs = |pairs: &[(&str, &str)]| -> Vec<KeyValue> {
            pairs
                .iter()
                .map(|(key, value)| opamp::attributes::string_attr(key, value))
                .collect()
        };
        let mut placed = record_with(0);
        placed.host = Some("h1".to_string());
        placed.description = Some(AgentDescription {
            identifying_attributes: pairs(&[("service.name", "otelcol")]),
            non_identifying_attributes: pairs(&[("service.instance.name", "edge-1")]),
        });
        let mut swapped = record_with(0);
        swapped.host = Some("h1".to_string());
        swapped.description = Some(AgentDescription {
            identifying_attributes: pairs(&[("service.instance.name", "edge-2")]),
            non_identifying_attributes: pairs(&[("service.name", "telegraf")]),
        });
        let (first, second) = (InstanceUid([0xe1; 16]), InstanceUid([0xe2; 16]));
        for (uid, record) in [(first, placed), (second, swapped)] {
            state.fleet.lock().expect("fleet lock").insert(uid, record);
        }
        let hosts = state.host_agents().expect("hosts");
        let shown = |uid: InstanceUid| {
            let agent = hosts["h1"]
                .agents
                .iter()
                .find(|agent| agent.instance_uid == uid.to_string())
                .expect("on h1");
            (
                agent.service_name.clone(),
                agent.service_instance_name.clone(),
            )
        };
        for uid in [first, second] {
            let derived = {
                let fleet = state.fleet.lock().expect("fleet lock");
                crate::listing::derive(&fleet[&uid].source(&uid))
            };
            assert_eq!(shown(uid).0, derived.service_name);
        }
        assert_eq!(shown(first), ("otelcol".to_string(), "edge-1".to_string()));
        assert_eq!(shown(second), (String::new(), String::new()));
    }

    /// On PostgreSQL an Agent's view and a rollout to it read the Agent and the Agents bound to its
    /// Host, not the whole fleet, and the view inherits the labels of its Client Agent, which this
    /// instance does not hold either.
    /// Verifies: SRV-instance-opens-chosen-planes
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
}
