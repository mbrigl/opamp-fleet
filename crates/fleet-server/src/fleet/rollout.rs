//! Configurations, Deployments and rollouts (SRV-configurations-compose-one-map, SRV-deployments-and-pinned-rollouts): the catalogue of
//! Configurations, the Configurations and Packages a Deployment releases, the rollout acts that
//! assign them to an Agent, what waits for an Agent's next press, the offer of a remote
//! configuration, labels, and the restart an operator requests. Its entry points are methods of
//! `AppState`.
//!
//! It also holds the types of what waits for an Agent and of a Deployment's reach, which `views`
//! shows: `rollout` computes them and ranks below `views` (SRV-fleet-contexts-call-by-rank).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use opamp::proto::{AgentConfigMap, AgentConfigObject, AgentRemoteConfig, ServerToAgent};
use opamp::uid::InstanceUid;
use serde::Serialize;
use tracing::{info, warn};

use crate::agent_store::{PersistedAgent, Pin};
use crate::configs::{ConfigStore, Configuration, DesiredConfig, Removal, Revision};
use crate::deployments::{self, deployment_for, Deployment, DeploymentError};
use crate::labels::LabelError;
use crate::packages::PackageStore;

use super::delivery::PackageOffering;
use super::{
    AgentRecord, AppState, Assignment, ConnId, DeleteError, RestartError, RolloutError,
    NO_CONFIGURATIONS,
};

impl AppState {
    /// Reads the Configurations and, where package delivery is armed, the Deployments and the
    /// Packages again from their stores, as another instance changed them
    /// (SRV-instance-opens-chosen-planes §12).
    /// Each one whose flag is set is dropped and read again whole.
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

    /// Read access to the Configuration store (the REST API's `GET` routes).
    pub fn configurations(&self) -> &ConfigStore {
        &self.configs
    }

    /// Creates a Configuration or replaces its saved revision, and persists it. **Saving only
    /// saves** (SRV-deployments-and-pinned-rollouts): nothing is offered and no WebSocket loop wakes — every Agent keeps
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

    /// The Deployment claiming one Agent (PKG-signed-packages-and-delivery), or the conflict that says why none does.
    fn deployment_of(&self, record: &AgentRecord) -> Result<Option<Deployment>, String> {
        let Some(store) = self.deployment_store() else {
            return Ok(None);
        };
        let all = store.snapshot();
        deployment_for(&all, record.effective_description().as_deref()).map(|found| found.cloned())
    }

    /// What a press of `deployment` would release to this Agent (SRV-deployments-and-pinned-rollouts §9) — the
    /// assignment it would write, or `None` when it has nothing for the Agent.
    ///
    /// Two parts, each as of now: the Package the Deployment holds for the Agent's type when it is
    /// a candidate (PKG-versions-high-water-mark-floor §5 and SRV-deployments-and-pinned-rollouts §14, §15, §16 and §17), and every Configuration it holds that fits the Agent's
    /// type, at its saved revision. Where the Deployment holds a Package for the Agent's type that
    /// is no candidate — the Agent already runs it, or it would move the Agent backwards — the
    /// press cannot move the package part, so the Package the assignment pins stays pinned: the
    /// release is replaced whole, and a part the press does not move must not fall out of it
    /// (SRV-deployments-and-pinned-rollouts §20). A Package or Configuration the Deployment no longer holds leaves the
    /// release.
    ///
    /// The rollout acts, the reach counts and the fleet view's waiting changes all ask this one
    /// question, so the count beside a press, the press and the view cannot disagree.
    pub(super) fn release(
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

    /// Writes releases (SRV-deployments-and-pinned-rollouts §6, SRV-state-stores-contract-pair §2): each Configuration's saved revision
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
        let mut digests = self.kept.written.lock().expect("written lock");
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
    /// question the store answers (SRV-state-stores-contract-pair §2).
    pub(super) fn collect_revisions(&self, names: impl IntoIterator<Item = String>) {
        for name in names.into_iter().collect::<BTreeSet<_>>() {
            let referenced = self.agent_store.referenced_revisions(&name);
            if let Err(e) = self.configs.retain_only(&name, &referenced) {
                warn!(configuration = %name, error = %e, "cannot collect unreferenced revisions");
            }
        }
    }

    /// The per-Agent rollout act (SRV-deployments-and-pinned-rollouts §9): releases the Deployment that claims this
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

    /// The rollout act for a Deployment (SRV-deployments-and-pinned-rollouts §9): releases it to every Agent it claims
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
    /// (SRV-fleet-record-agents-hosts §2).
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
        // Operator intent survives a Server restart like any other durable state (SRV-fleet-record-agents-hosts).
        self.agent_store
            .set_restart_pending(uid, true)
            .map_err(RestartError::Storage)?
            .ok_or(RestartError::UnknownAgent)?;
        if let Some(record) = fleet.get_mut(uid) {
            record.restart_pending = true;
            self.kept.written.lock().expect("written lock").remove(uid);
        }
        // A put-back still kept is this restart now: two requests before a delivery are one.
        self.drop_kept_restart(uid);
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        info!(agent = %uid, "restart requested");
        Ok(())
    }

    /// Replaces an Agent's labels (SRV-fleet-record-agents-hosts), which changes what Selectors match it.
    ///
    /// A key the Agent already reports is **refused**, not applied: `os.type` and `host.arch`
    /// choose which artifact it is offered (PKG-signed-packages-and-delivery) and `service.name` decides which packages fit
    /// it at all (PKG-signed-packages-and-delivery), so a label that outranked them would let a slip here offer this Agent
    /// an artifact built for another machine. Labels annotate; they do not correct.
    ///
    /// A label move changes only what the fleet view **proposes** (SRV-deployments-and-pinned-rollouts): the Agent's
    /// candidates follow its new channel, and nothing is distributed until a rollout act says so.
    ///
    /// The labels are part of the Agent's record (SRV-fleet-record-agents-hosts §21) and are written to their own
    /// column of it before this answers; on a shared store the Agent is read from the store, and
    /// the change announced to the instance that carries it. A Client Agent's labels hold for its
    /// Client's other Agents (SRV-fleet-record-agents-hosts §19), so its group is placed again.
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

    /// Which Deployments hold each Package, keyed by `<agent type>@<version>` — how a Package
    /// answers "whom would this reach", since it does not aim by itself (PKG-signed-packages-and-delivery).
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

    /// Which Deployments hold each Configuration, by name (SRV-deployments-and-pinned-rollouts §23) — how a
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

    /// The queued restart for this Agent as the Baseline's command-only message, taken once —
    /// `None` when nothing is queued (or the Agent went away). One its connection does not take is
    /// queued again through [`unsent`](Self::unsent). It is handed out only to `conn`, the
    /// WebSocket connection asking, where that connection carries the Agent: an older connection
    /// of its host that still names it leaves the restart to the newer one
    /// (SRV-instance-opens-chosen-planes §8).
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
    /// (SRV-fleet-record-agents-hosts §2). One that cannot be cleared is
    /// kept for the next exchange, and one cleared whose command is not sent is put back by
    /// [`unsent`](Self::unsent).
    ///
    /// The clear is written before the fleet lock is taken, under the Agent's own lock, which
    /// requests, deliveries, forgets and the tries of a put-back take
    /// ([`clear_restart`](Self::clear_restart)); `cleared` is what it answered, and this takes it
    /// into `record`.
    pub(super) fn take_restart(
        &self,
        uid: &InstanceUid,
        record: &mut AgentRecord,
        cleared: Result<Option<bool>, String>,
    ) -> bool {
        match cleared {
            Ok(was) => {
                record.restart_pending = false;
                self.kept.written.lock().expect("written lock").remove(uid);
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
    pub(super) fn clear_restart(&self, uid: &InstanceUid) -> Result<Option<bool>, String> {
        self.agent_store.set_restart_pending(uid, false)
    }

    /// Deletes a Configuration (SRV-deployments-and-pinned-rollouts §12); `false` when none of that name exists.
    /// **Refused while any Agent's assignment pins it** — the message says how many — because a
    /// deletion must not change what a running Agent receives: those Agents are rolled out through
    /// a Deployment without it first, or forgotten. A deleted Configuration leaves every Deployment
    /// that held it.
    ///
    /// The fleet lock is held across the check and the delete, so a rollout of this Server cannot
    /// pin it in between. A store that other Servers write too counts the stored assignments again
    /// in the act that deletes (SRV-state-stores-contract-pair §2), so a pin
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
        let pinning = self.pinning(Pin::Configuration(name));
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

    /// Puts a Configuration into a Deployment (SRV-deployments-and-pinned-rollouts §5); a Configuration the Server
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
    /// (SRV-deployments-and-pinned-rollouts §11), and an Agent already released keeps its pinned revision until the next
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

    /// Deletes a Deployment — **refused while an Agent's assignment names it** (SRV-deployments-and-pinned-rollouts §12).
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
        let pinning = self.pinning(Pin::Deployment(name));
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

/// What waits for one Agent's next press — every part in which what the claiming Deployment
/// would release differs from what the assignment pins.
#[derive(Default)]
pub(super) struct Waiting {
    pub(super) configurations: Vec<PendingConfigurationView>,
    pub(super) packages: Vec<PendingPackageView>,
    /// The Deployment the press would release through, when the assignment names another one or
    /// none.
    pub(super) deployment: String,
}

/// What waits for an Agent's next press (SRV-deployments-and-pinned-rollouts §6, §8 and §9): the
/// difference between what the claiming Deployment would release and what the assignment pins,
/// per Configuration, for the Package, and for the Deployment the release goes through — so
/// whatever makes a press move this Agent shows here, and the reach count agrees with the view.
///
/// `new` is not pinned yet, `update` is pinned at another revision or version, and `remove` is
/// pinned and leaves the release — the Deployment no longer holds it, or no longer fits it to this
/// Agent — while the release keeps something else, so the smaller assignment is applied. `kept` is
/// pinned while the release holds nothing for the Agent at all: the press leaves it with no
/// assignment, it is offered nothing, and it keeps running what it runs (SRV-deployments-and-pinned-rollouts §9).
pub(super) fn waiting(
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

/// The remote-config offer for one Agent, or `None` when the hash comparison says it already has
/// it — the "no redundant reconfiguration" goal in one place. Every assigned Configuration is one
/// named entry; the Managed Process does its own merging (SRV-configurations-compose-one-map).
pub(super) fn offer(
    record: &AgentRecord,
    desired: Option<&DesiredConfig>,
) -> Option<AgentRemoteConfig> {
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
                            // The operator's role, verbatim (SRV-configurations-compose-one-map). Empty — the default —
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

/// The Baseline's command-only message: identity, capabilities, and the restart — nothing else.
pub(super) fn restart_command(uid: &InstanceUid, capabilities: u64) -> ServerToAgent {
    ServerToAgent {
        instance_uid: uid.as_bytes().to_vec(),
        capabilities,
        command: Some(opamp::proto::ServerToAgentCommand {
            r#type: opamp::proto::CommandType::Restart as i32,
        }),
        ..Default::default()
    }
}

/// Refuses a per-Agent press that would move nothing because the Package the Deployment holds for
/// the Agent's type is not released to it — no upgrade, no entry for its platform — and says why,
/// naming the version it compared against (SRV-deployments-and-pinned-rollouts §19). A press that moves nothing for any
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
/// naming each such Package and its platforms; nothing is released (SRV-deployments-and-pinned-rollouts §9, PKG-signed-packages-and-delivery).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_store::AgentStore;
    use crate::fleet::fixtures::*;
    use crate::fleet::views::AgentView;
    use crate::fleet::Processed;
    use crate::fleet::Transport;
    use crate::packages::PackageId;
    use crate::revocation::{CertId, Presented};
    use opamp::proto::{AgentDescription, AgentToServer, PackageStatuses, RemoteConfigStatus};
    use std::sync::Arc;

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

    /// There is no seed. A record carrying no assignment loads as **assigned nothing** — the Server
    /// never invents a rollout at startup — and what its Deployment would release shows up as
    /// waiting instead, which is the one thing an operator has to act on.
    /// Verifies: PKG-signed-packages-and-delivery, SRV-deployments-and-pinned-rollouts
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
            "what it could receive waits for an explicit act (SRV-deployments-and-pinned-rollouts)"
        );
    }

    /// SRV-deployments-and-pinned-rollouts: saving proposes, the acts assign — and an Agent that appears after the
    /// Deployment's act waits for one of its own (point 6).
    /// Verifies: SRV-deployments-and-pinned-rollouts
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
    /// Verifies: SRV-state-stores-contract-pair
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

    /// SRV-fleet-record-agents-hosts: a queued restart is operator intent and survives the Server restarting.
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
    /// Verifies: SRV-fleet-record-agents-hosts
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

    // ---- A Deployment carries Configurations beside its Packages (SRV-deployments-and-pinned-rollouts) ----

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
                    status: opamp::proto::RemoteConfigStatuses::Applied as i32,
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
    /// Verifies: SRV-deployments-and-pinned-rollouts
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
    /// Verifies: SRV-deployments-and-pinned-rollouts
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
    /// Verifies: SRV-deployments-and-pinned-rollouts
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
    /// Verifies: SRV-deployments-and-pinned-rollouts
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
    /// Verifies: SRV-deployments-and-pinned-rollouts
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
    /// Verifies: SRV-deployments-and-pinned-rollouts
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
    /// Verifies: SRV-deployments-and-pinned-rollouts
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

    /// Nothing an assignment pins can be deleted — a Configuration, a Package, a Deployment — and
    /// the refusal says how many Agents pin it. After a press without it the deletion succeeds,
    /// and a deleted Configuration or Package leaves every Deployment that held it.
    /// Verifies: SRV-deployments-and-pinned-rollouts
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
        assert!(refused.message.contains("1 Agent"), "{refused}");
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
    /// Verifies: SRV-instance-opens-chosen-planes, SRV-deployments-and-pinned-rollouts
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
    /// Verifies: SRV-instance-opens-chosen-planes, SRV-deployments-and-pinned-rollouts
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
}
