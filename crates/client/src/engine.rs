//! The Engine: n Agents over one upstream connection (ADR-0003, ADR-0011).
//!
//! This is the Server-facing seam of the hexagonal core. The transports consume it — build
//! reports, hand it every decoded `ServerToAgent`, ask for the goodbyes — and it routes each
//! reply to the owning Agent by `instance_uid` alone, never by connection. With one self-Agent
//! it behaves exactly like the single-Agent Client did; with Supervisors it multiplexes them.

use std::sync::{Arc, Mutex};

use opamp::proto::{AgentToServer, ConnectionSettingsOffers, ServerToAgent};
use opamp::uid::InstanceUid;
use tokio::sync::mpsc;
use tracing::warn;

use crate::packages::PackageDownload;
use crate::supervisor::agent::{AgentState, Handled};
use crate::supervisor::ports::{ProcessCommand, ProcessEvent};

/// One Agent as the Engine carries it: its protocol state machine, the command side of its
/// Managed-Process Port (absent for the self-Agent), and the bookkeeping of whether it owes the
/// Server a report right now.
struct SupervisedAgent {
    state: AgentState,
    commands: Option<mpsc::Sender<ProcessCommand>>,
    /// A handled reply asked for an immediate report (config outcome, demanded full state).
    owes_report: bool,
}

pub struct Engine {
    agents: Vec<SupervisedAgent>,
    /// The shared event channel every adapter reports into, tagged with the Agent's index.
    events: mpsc::Receiver<(usize, ProcessEvent)>,
    /// A connection-settings offer awaiting the transport's verification (ADR-0014). The offer
    /// arrives per Agent but the settings are connection-scoped, so the Engine keeps exactly one
    /// pending offer — n Agents receiving the same offer verify and switch once.
    pending_connection_offer: Option<ConnectionSettingsOffers>,
    /// Packages awaiting the transport's download and verification (ADR-0015), each tagged with
    /// the owning Agent's index so the verified artifact routes back to the right Supervisor.
    pending_package_downloads: Vec<(usize, PackageDownload)>,
    /// How the Client updates *itself* (ADR-0020): where its state lives, the archive key, and —
    /// while this process is a freshly installed version — the marker it must commit. `None` when
    /// `[self_update]` is absent, in which case the self-Agent accepts no packages anyway.
    self_update: Option<SelfUpdateState>,
    /// Set once the Server has answered at all. Reaching the Server is what a new version has to
    /// do to prove itself: a binary that starts, connects, and is spoken to is running.
    seen_server: bool,
    /// Set once a self-update has moved the `current` pointer: the run must end for the service
    /// manager to start the new version (ADR-0020).
    restart_for_update: bool,
    /// The sampling targets, shared with the own-telemetry sampler (ADR-0036), which runs beside
    /// a transport that holds this Engine mutably for the whole of a connection.
    sampling: Arc<Mutex<Vec<(String, u32)>>>,
}

/// What the Engine needs to install a new version of the Client and to close out one that is on
/// probation (ADR-0020).
struct SelfUpdateState {
    state_dir: std::path::PathBuf,
    archive_key: Option<String>,
    /// Present while this process is the new version and has not yet committed itself.
    probation: Option<Box<crate::selfupdate::UpdateMarker>>,
}

impl Engine {
    /// An Engine over Agents without Managed Processes. Since ADR-0020 the Client always builds
    /// its self-Agent *and* its Supervisors through [`with_processes`](Self::with_processes), so
    /// this is the tests' constructor — the shape it stands for no longer occurs in production.
    #[cfg(test)]
    #[must_use]
    pub fn new(agents: Vec<AgentState>) -> Self {
        let (_, events) = mpsc::channel(1);
        Engine::with_processes(
            agents.into_iter().map(|state| (state, None)).collect(),
            events,
        )
    }

    /// An Engine over Supervisor-backed Agents: each with the command side of its Port, all
    /// sharing one event channel (senders tagged by the Agent's index here).
    #[must_use]
    pub fn with_processes(
        agents: Vec<(AgentState, Option<mpsc::Sender<ProcessCommand>>)>,
        events: mpsc::Receiver<(usize, ProcessEvent)>,
    ) -> Self {
        let engine = Engine {
            agents: agents
                .into_iter()
                .map(|(state, commands)| SupervisedAgent {
                    state,
                    commands,
                    owes_report: false,
                })
                .collect(),
            events,
            pending_connection_offer: None,
            pending_package_downloads: Vec::new(),
            self_update: None,
            seen_server: false,
            restart_for_update: false,
            sampling: Arc::new(Mutex::new(Vec::new())),
        };
        // The Client's own Agent samples this process, and that is true from the start — only a
        // Managed Process's pid has to wait for the process to exist.
        engine.refresh_sampling();
        engine
    }

    /// Arms self-update (ADR-0020): where to write the marker, how to open an encrypted archive,
    /// and the marker this process must commit if it is itself a freshly installed version.
    pub fn arm_self_update(
        &mut self,
        state_dir: std::path::PathBuf,
        archive_key: Option<String>,
        probation: Option<crate::selfupdate::UpdateMarker>,
    ) {
        self.self_update = Some(SelfUpdateState {
            state_dir,
            archive_key,
            probation: probation.map(Box::new),
        });
    }

    /// Reports a self-update that finished in a previous process (ADR-0020): the install
    /// necessarily completes across a restart, so the terminal status is owed by whichever
    /// version came up — the new one saying `Installed`, or the old one saying why it is back.
    pub fn report_self_update_outcome(&mut self, outcome: &crate::selfupdate::UpdateOutcome) {
        let Some(agent) = self.agents.get_mut(crate::supervisor::SELF_AGENT_INDEX) else {
            return;
        };
        let hash = hex::decode(&outcome.package_hash_hex).unwrap_or_default();
        agent.state.package_applied(
            hash,
            match &outcome.error {
                None => Ok(outcome.version.clone()),
                Some(error) => Err(error.clone()),
            },
        );
        agent.owes_report = true;
    }

    /// Restores previously applied connection settings on every Agent (ADR-0014), so a restarted
    /// Client reports `APPLIED` and is not re-offered what it already runs.
    pub fn adopt_connection_settings(&mut self, hash: &[u8]) {
        for agent in &mut self.agents {
            agent.state.adopt_connection_settings(hash);
        }
    }

    /// Declares one more capability on every Agent — e.g. `ReportsHeartbeat` once an offered
    /// interval enables what the configuration had disabled.
    pub fn declare_capability_all(&mut self, capability: opamp::proto::AgentCapabilities) {
        for agent in &mut self.agents {
            agent.state.declare_capability(capability);
        }
    }

    /// What own metrics are sampled from (ADR-0036): every Agent's `service.instance.id` paired
    /// with the pid to sample for it — this process for the Client's own Agent, the Managed
    /// Process for a Supervisor-backed one, and nothing while that process is not running.
    pub fn sampling_targets(&self) -> Vec<(String, u32)> {
        self.agents
            .iter()
            .filter_map(|agent| {
                let pid = match agent.state.is_managed() {
                    false => std::process::id(),
                    true => agent.state.process_pid()?,
                };
                Some((agent.state.uid().to_string(), pid))
            })
            .collect()
    }

    /// A handle on [`sampling_targets`](Self::sampling_targets) the metrics sampler can read while
    /// the transport holds the Engine mutably — which it does for the whole of a connection.
    /// Refreshed whenever a pid changes, so a Managed Process that restarts is followed.
    pub fn sampling_handle(&self) -> Arc<Mutex<Vec<(String, u32)>>> {
        self.sampling.clone()
    }

    fn refresh_sampling(&self) {
        if let Ok(mut shared) = self.sampling.lock() {
            *shared = self.sampling_targets();
        }
    }

    /// The Client's own Agent's description, for the Resource its telemetry carries (ADR-0036).
    pub fn self_description(&self) -> opamp::proto::AgentDescription {
        self.agents
            .iter()
            .find(|agent| !agent.state.is_managed())
            .map(|agent| agent.state.description())
            .unwrap_or_default()
    }

    /// Asks the Server for a client certificate when it signs them and this Client needs one
    /// (ADR-0035). Driven by capability rather than configuration: a Server that declares nothing
    /// is never asked, and one that does hands this host an identity before mutual TLS is switched
    /// on, which is what makes switching it on uneventful.
    ///
    /// The request rides the Client's **own** Agent. The identity belongs to the connection, not to
    /// any one Agent (n Agents share it, ADR-0003), and the self-Agent is the one every Client has.
    pub fn request_certificate(&mut self, config: &crate::config::ClientConfig) {
        let Some(agent) = self
            .agents
            .iter_mut()
            .find(|agent| agent.state.server_signs_certificates() && !agent.state.is_managed())
        else {
            return;
        };
        if let Some(csr) = crate::csr::request(config) {
            agent.state.request_certificate(csr);
        }
    }

    /// The connection-settings offer the transport must verify by actually connecting, taken
    /// exactly once (ADR-0014).
    pub fn take_connection_offer(&mut self) -> Option<ConnectionSettingsOffers> {
        self.pending_connection_offer.take()
    }

    /// The packages the transport must download and verify (ADR-0015), each with its Agent's
    /// index — drained so each is dispatched once.
    pub fn take_package_downloads(&mut self) -> Vec<(usize, PackageDownload)> {
        std::mem::take(&mut self.pending_package_downloads)
    }

    /// Records download progress for the Agent whose package is being fetched (ADR-0015), so the
    /// next report carries `Downloading` with its details instead of a silent `Installing`.
    pub fn package_downloading(
        &mut self,
        index: usize,
        details: opamp::proto::PackageDownloadDetails,
    ) {
        if let Some(agent) = self.agents.get_mut(index) {
            agent.state.package_downloading(details);
            agent.owes_report = true;
        }
    }

    /// Hands a downloaded, verified artifact to the owning Agent's Supervisor to apply (ADR-0015),
    /// or — for the Client's own Agent — installs it as a new version of the Client (ADR-0020).
    /// The Supervisor's `PackageApplied` event closes the lifecycle. A missing adapter, or one not
    /// accepting commands, fails the install (reported, not silent).
    ///
    pub fn apply_package(
        &mut self,
        index: usize,
        staged: std::path::PathBuf,
        version: String,
        hash: Vec<u8>,
    ) {
        if index == crate::supervisor::SELF_AGENT_INDEX {
            self.apply_self_update(&staged, version, hash);
            return;
        }
        let Some(agent) = self.agents.get_mut(index) else {
            return;
        };
        // The bytes are in: the status moves from Downloading to Installing.
        agent.state.package_downloaded();
        match &agent.commands {
            Some(commands) => {
                if let Err(e) = commands.try_send(ProcessCommand::ApplyPackage {
                    staged,
                    version,
                    hash: hash.clone(),
                }) {
                    warn!(error = %e, "cannot hand the package to the supervisor");
                    agent.state.package_applied(
                        hash,
                        Err("the supervisor is not accepting commands".to_string()),
                    );
                    agent.owes_report = true;
                }
            }
            None => {
                agent.state.package_applied(
                    hash,
                    Err("this agent has no process to install a package into".to_string()),
                );
                agent.owes_report = true;
            }
        }
    }

    /// Whether a self-update has moved the `current` pointer and the run must therefore end, so
    /// the service manager restarts into the new version (ADR-0020).
    #[must_use]
    pub fn restart_for_update(&self) -> bool {
        self.restart_for_update
    }

    /// Installs a verified artifact as a new version of *this Client* (ADR-0020).
    ///
    /// Unlike a Supervisor's install, the outcome cannot be reported from here on success: this
    /// process is about to stop being the one that runs. Only the failure is terminal now — and it
    /// is terminal with the previous version still current and still running.
    fn apply_self_update(&mut self, staged: &std::path::Path, version: String, hash: Vec<u8>) {
        let Some(agent) = self.agents.get_mut(crate::supervisor::SELF_AGENT_INDEX) else {
            return;
        };
        agent.state.package_downloaded();
        let Some(update) = &self.self_update else {
            // Unreachable while the capability is only declared with `[self_update]`, but a
            // refusal that says so beats an install that should not have been offered.
            let agent = &mut self.agents[crate::supervisor::SELF_AGENT_INDEX];
            agent
                .state
                .package_applied(hash, Err("self-update is not enabled".to_string()));
            agent.owes_report = true;
            return;
        };

        match crate::selfupdate::install(
            &update.state_dir,
            staged,
            &version,
            &hash,
            update.archive_key.as_deref(),
        ) {
            Ok(crate::selfupdate::Install::Staged) => {
                // `Installing` is already the reported status and the caller flushes it before the
                // run ends. What comes after the restart reports the outcome.
                let _ = std::fs::remove_file(staged);
                self.restart_for_update = true;
            }
            // The version offered is the one running — which is what a freshly updated Client is
            // told every time, since the Server keeps offering until an Agent reports a terminal
            // status for that package. Saying `Installed` is both true and what closes the loop:
            // reporting a failure here left the Server offering and this Client downloading, over
            // and over, for as long as both were up.
            Ok(crate::selfupdate::Install::AlreadyRunning) => {
                let _ = std::fs::remove_file(staged);
                let agent = &mut self.agents[crate::supervisor::SELF_AGENT_INDEX];
                agent.state.package_applied(hash, Ok(version));
                agent.owes_report = true;
            }
            Err(e) => {
                warn!(error = %e, "the Client's self-update failed; staying on this version");
                let _ = std::fs::remove_file(staged);
                let agent = &mut self.agents[crate::supervisor::SELF_AGENT_INDEX];
                agent.state.package_applied(hash, Err(e));
                agent.owes_report = true;
            }
        }
    }

    /// A package download or verification failed (ADR-0015): the owning Agent reports
    /// `InstallFailed` — a rejected package is a report, not a silence.
    pub fn package_download_failed(&mut self, index: usize, hash: Vec<u8>, error: String) {
        if let Some(agent) = self.agents.get_mut(index) {
            agent.state.package_applied(hash, Err(error));
            agent.owes_report = true;
        }
    }

    /// Closes a verified offer's lifecycle on every Agent: `APPLIED` (the transport switches
    /// next) or `FAILED` with the error; either way every Agent owes the Server the outcome.
    pub fn connection_settings_outcome(&mut self, hash: &[u8], result: Result<(), &str>) {
        for agent in &mut self.agents {
            agent.state.connection_settings_outcome(hash, result);
            agent.owes_report = true;
        }
    }

    /// One Agent's next report, for the plain-HTTP verification probe (ADR-0014): a real
    /// exchange needs a real report. Delivered on success; a failed probe leaves a sequence gap
    /// the Baseline's `ReportFullState` recovery heals on the next exchange.
    pub fn probe_report(&mut self) -> Option<AgentToServer> {
        self.agents
            .first_mut()
            .map(|agent| agent.state.next_report())
    }

    /// The identities carried, for logging.
    pub fn uids(&self) -> impl Iterator<Item = InstanceUid> + '_ {
        self.agents.iter().map(|a| a.state.uid())
    }

    /// Whether any Agent this Engine runs takes Server-offered packages — a self-update or a managed
    /// process's package. What the startup check uses to decide whether an unconfigured verification
    /// key is worth warning about (ADR-0015).
    pub fn installs_packages(&self) -> bool {
        self.agents.iter().any(|a| a.state.accepts_packages())
    }

    /// Every Agent starts over with a full snapshot — after (re)connecting, or when an exchange
    /// was lost and the Server may be missing state.
    pub fn force_full_all(&mut self) {
        for agent in &mut self.agents {
            agent.state.force_full();
        }
    }

    /// One report per Agent — the routine poll, and the after-connect snapshot when
    /// [`force_full_all`](Self::force_full_all) was called first.
    pub fn poll_reports(&mut self) -> Vec<AgentToServer> {
        self.agents
            .iter_mut()
            .map(|agent| {
                agent.owes_report = false;
                agent.state.next_report()
            })
            .collect()
    }

    /// Reports from exactly the Agents that owe one — after a handled reply asked for an
    /// immediate report. Empty when nothing changed.
    pub fn owed_reports(&mut self) -> Vec<AgentToServer> {
        self.agents
            .iter_mut()
            .filter(|agent| agent.owes_report)
            .map(|agent| {
                agent.owes_report = false;
                agent.state.next_report()
            })
            .collect()
    }

    /// Routes one `ServerToAgent` to the Agent its `instance_uid` names. A reply for an unknown
    /// Agent is dropped with a warning — the protocol's multiplexing provision makes the uid the
    /// sole routing key, so there is nothing else to fall back to.
    pub fn handle(&mut self, reply: &ServerToAgent) -> Handled {
        let Some(uid) = InstanceUid::from_wire(&reply.instance_uid) else {
            warn!("dropping a reply without a valid instance_uid");
            return Handled::default();
        };
        // n is the number of local Supervisors — small; a linear scan beats a map to maintain.
        let Some(index) = self.agents.iter().position(|a| a.state.uid() == uid) else {
            warn!(agent = %uid, "dropping a reply for an unknown agent");
            return Handled::default();
        };
        // The Server answered, so this version connected and is being spoken to — which is what a
        // freshly installed one has to manage to stop being on probation (ADR-0020). Committing
        // here rather than on a timer means the bar is "it works", not "it survived a clock".
        if !self.seen_server {
            self.seen_server = true;
            if let Some(update) = &mut self.self_update {
                if let Some(marker) = update.probation.take() {
                    crate::selfupdate::commit(&update.state_dir, &marker);
                }
            }
        }
        let agent = &mut self.agents[index];
        let mut handled = agent.state.handle(reply);
        if handled.send_report {
            agent.owes_report = true;
        }
        // The connection-scoped part of the reply moves to the Engine's single pending slot;
        // whichever Agent's reply carried it last wins — they are all the same offer.
        if let Some(offer) = handled.connection_offer.take() {
            self.pending_connection_offer = Some(offer);
        }
        // A package offer is per Agent (each Supervisor has its own binary): queue it with the
        // owning Agent's index so the transport can route the verified artifact back.
        if let Some(download) = handled.package_download.take() {
            self.pending_package_downloads.push((index, download));
        }
        // A stored configuration awaiting application goes to the process adapter; its
        // ConfigApplied event closes the APPLYING → APPLIED/FAILED lifecycle.
        if let Some(config) = agent.state.take_pending_apply() {
            match &agent.commands {
                Some(commands) => {
                    if let Err(e) = commands.try_send(ProcessCommand::ApplyConfig { config }) {
                        warn!(agent = %uid, error = %e, "cannot hand the configuration to the supervisor");
                        agent.state.config_applied(
                            match e.into_inner() {
                                ProcessCommand::ApplyConfig { config } => config.config_hash,
                                ProcessCommand::ApplyPackage { .. }
                                | ProcessCommand::Restart
                                | ProcessCommand::Shutdown => Vec::new(),
                            },
                            Err("the supervisor is not accepting commands".to_string()),
                        );
                        agent.owes_report = true;
                    }
                }
                None => {
                    warn!(agent = %uid, "a configuration is pending but no process adapter exists")
                }
            }
        }
        // A Server-commanded restart goes the same way; its outcome is the health cycle the
        // stop/spawn emits, so a dropped command only needs the warning.
        if agent.state.take_pending_restart() {
            match &agent.commands {
                Some(commands) => {
                    if let Err(e) = commands.try_send(ProcessCommand::Restart) {
                        warn!(agent = %uid, error = %e, "cannot hand the restart to the supervisor");
                    }
                }
                None => warn!(agent = %uid, "a restart is pending but no process adapter exists"),
            }
        }
        handled
    }

    /// The connection's final messages: one `agent_disconnect` per Agent, as the Baseline
    /// requires of the last message each Agent sends.
    pub fn disconnect_messages(&mut self) -> Vec<AgentToServer> {
        self.agents
            .iter_mut()
            .map(|agent| agent.state.disconnect_message())
            .collect()
    }

    /// Resolves when a Managed Process changed some Agent's state, so the transport can push a
    /// report without waiting for a poll. With no adapters (the self-Agent) it never resolves.
    pub async fn changed(&mut self) {
        match self.events.recv().await {
            Some((index, event)) => self.absorb(index, event),
            // Every sender is gone — nothing will ever change again; don't spin.
            None => std::future::pending().await,
        }
    }

    /// Folds one process event into the owning Agent and marks it as owing a report.
    fn absorb(&mut self, index: usize, event: ProcessEvent) {
        // Set when the event moved a pid: the shared sampling view is refreshed after the
        // borrow of `agent` ends, since refreshing reads every Agent.
        let mut refresh = false;
        let Some(agent) = self.agents.get_mut(index) else {
            warn!(index, "dropping an event for an unknown agent");
            return;
        };
        match event {
            ProcessEvent::Description(description) => {
                agent.state.set_process_description(description);
            }
            ProcessEvent::Pid(pid) => {
                agent.state.set_process_pid(pid);
                refresh = true;
            }
            ProcessEvent::Health(health) => agent.state.set_process_health(health),
            ProcessEvent::EffectiveConfig(config) => {
                agent.state.set_process_effective_config(config);
            }
            ProcessEvent::AvailableComponents(components) => {
                agent.state.set_available_components(components);
            }
            ProcessEvent::ConfigApplied { hash, result } => {
                agent.state.config_applied(hash, result);
            }
            ProcessEvent::PackageApplied { hash, result } => {
                agent.state.package_applied(hash, result);
            }
        }
        agent.owes_report = true;
        if refresh {
            self.refresh_sampling();
        }
    }

    /// Stops all Managed Processes — each adapter honours `Shutdown` within its stop budget —
    /// before the goodbyes go out.
    pub async fn shutdown_processes(&mut self) {
        for agent in &mut self.agents {
            if let Some(commands) = agent.commands.take() {
                let _ = commands.send(ProcessCommand::Shutdown).await;
            }
        }
        // The adapters drop their event senders once stopped; drain until they are all gone so
        // the goodbyes go out after the processes are down, not concurrently.
        while let Some((index, event)) = self.events.recv().await {
            self.absorb(index, event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use opamp::proto::ServerToAgentFlags;

    fn engine_of_two(dir: &std::path::Path) -> Engine {
        let agents = ["left", "right"]
            .into_iter()
            .map(|name| {
                let storage = Storage::new(dir.join(name)).expect("storage");
                AgentState::new(name.to_string(), storage).expect("agent")
            })
            .collect();
        Engine::new(agents)
    }

    #[test]
    fn poll_reports_carries_every_agent_with_distinct_identities() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = engine_of_two(dir.path());
        let reports = engine.poll_reports();
        assert_eq!(reports.len(), 2);
        assert_ne!(reports[0].instance_uid, reports[1].instance_uid);
        // Sequence numbers are per Agent, not shared.
        let again = engine.poll_reports();
        assert!(again.iter().all(|r| r.sequence_num == 2));
    }

    #[test]
    fn a_reply_reaches_only_the_agent_its_uid_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = engine_of_two(dir.path());
        let reports = engine.poll_reports();

        let handled = engine.handle(&ServerToAgent {
            instance_uid: reports[0].instance_uid.clone(),
            flags: ServerToAgentFlags::ReportFullState as u64,
            ..Default::default()
        });
        assert!(handled.send_report);

        // Only the addressed agent owes a report, and it is a full one.
        let owed = engine.owed_reports();
        assert_eq!(owed.len(), 1);
        assert_eq!(owed[0].instance_uid, reports[0].instance_uid);
        assert!(owed[0].agent_description.is_some());
        assert!(engine.owed_reports().is_empty());
    }

    #[test]
    fn replies_for_unknown_or_malformed_uids_are_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = engine_of_two(dir.path());
        let _ = engine.poll_reports();

        let unknown = engine.handle(&ServerToAgent {
            instance_uid: InstanceUid::default().as_bytes().to_vec(),
            flags: ServerToAgentFlags::ReportFullState as u64,
            ..Default::default()
        });
        assert!(!unknown.send_report);
        let malformed = engine.handle(&ServerToAgent {
            instance_uid: vec![1, 2, 3],
            ..Default::default()
        });
        assert!(!malformed.send_report);
        assert!(engine.owed_reports().is_empty());
    }

    #[test]
    fn disconnects_cover_every_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = engine_of_two(dir.path());
        let goodbyes = engine.disconnect_messages();
        assert_eq!(goodbyes.len(), 2);
        assert!(goodbyes.iter().all(|g| g.agent_disconnect.is_some()));
    }

    #[test]
    fn a_rekeyed_agent_stays_routable_under_its_new_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = engine_of_two(dir.path());
        let reports = engine.poll_reports();
        let new_uid = InstanceUid::default();

        engine.handle(&ServerToAgent {
            instance_uid: reports[0].instance_uid.clone(),
            agent_identification: Some(opamp::proto::AgentIdentification {
                new_instance_uid: new_uid.as_bytes().to_vec(),
            }),
            ..Default::default()
        });

        let handled = engine.handle(&ServerToAgent {
            instance_uid: new_uid.as_bytes().to_vec(),
            flags: ServerToAgentFlags::ReportFullState as u64,
            ..Default::default()
        });
        assert!(handled.send_report);
    }
}
