//! The Engine: n Agents over one upstream connection (ADR-0014, ADR-0017).
//!
//! This is the Server-facing seam of the hexagonal core. The transports consume it — build
//! reports, hand it every decoded `ServerToAgent`, ask for the goodbyes — and it routes each
//! reply to the owning Agent by `instance_uid` alone, never by connection. With one self-Agent
//! it behaves exactly like the single-Agent Client did; with Supervisors it multiplexes them.

use opamp::uid::InstanceUid;
use tracing::{info, warn};

use crate::supervisor::agent::{AgentState, Handled};
use crate::supervisor::ports::{ProcessCommand, ProcessEvent};

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
}

impl Engine {
    #[must_use]
    pub fn new(agents: Vec<AgentState>) -> Self {
        Engine::with_processes(
            events,
        )
    }

    #[must_use]
    pub fn with_processes(
        events: mpsc::Receiver<(usize, ProcessEvent)>,
    ) -> Self {
            events,
    }

        for agent in &mut self.agents {
        for agent in &mut self.agents {
        let Some(agent) = self.agents.get_mut(index) else {
        for agent in &mut self.agents {
    /// The identities carried, for logging.
    pub fn uids(&self) -> impl Iterator<Item = InstanceUid> + '_ {
        self.agents.iter().map(|a| a.state.uid())
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
            warn!(agent = %uid, "dropping a reply for an unknown agent");
            return Handled::default();
        };
        if handled.send_report {
            agent.owes_report = true;
        }
        // A stored configuration awaiting application goes to the process adapter; its
        if let Some(config) = agent.state.take_pending_apply() {
                                    | ProcessCommand::Shutdown
                                    | ProcessCommand::Uninstall => Vec::new(),
                    }
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

                        opamp::proto::AgentConfigObject {
    /// Retires the Agents whose `[[supervisor]]` blocks left the set (ADR-0017): each one's
    /// adapter stops the Managed Process within the stop budget, its Endpoint releases the port,
    /// the adapter's exit is awaited, and the goodbyes to send are returned. The slots stay (see
    /// [`SupervisedAgent::retired`]); unnamed Agents run on untouched.
    ///
    /// A name in `uninstalling` is leaving the set *for good* (ADR-0017): its adapter is told to
    /// uninstall — stop, undo what installing it did, answer — before the caller purges its
    /// directory (ADR-0017). A name only in `names` merely changed: it is stopped, restarts
    /// under its name, and keeps what it installed. The adapter's own stop budget bounds either
    /// path; an uninstall that cannot be delivered falls back to the plain stop.
    pub async fn retire_supervisors(
        &mut self,
        names: &[String],
        uninstalling: &[String],
    ) -> Vec<AgentToServer> {
            let uninstall = agent
                .block_name
                .as_ref()
                .is_some_and(|name| uninstalling.contains(name));
            let commands = agent.commands.take();
            let told_to_uninstall = uninstall
                && commands
                    .as_ref()
                    .is_some_and(|commands| commands.try_send(ProcessCommand::Uninstall).is_ok());
            // A plainly stopped adapter exits on its fired shutdown; one told to uninstall
            // exits on the command itself, and its Endpoint's stop follows below.
            if !told_to_uninstall {
                if let Some(stop) = agent.stop.take() {
                    let _ = stop.send(true);
                }
            if let Some(commands) = commands {
                // The channel closing is how the adapter's exit — and, for an uninstall, its
                // answered outcome — is observed.
    /// The connection's final messages: one `agent_disconnect` per Agent, as the Baseline
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
        let Some(agent) = self.agents.get_mut(index) else {
            warn!(index, "dropping an event for an unknown agent");
            return;
        };
        match event {
            ProcessEvent::Description(description) => {
                agent.state.set_process_description(description);
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
            // The adapter's last word before retirement (ADR-0017). The goodbye carries no
            // status, so the outcome is the operator's to read here — an `Err` names what the
            // kind could not undo, which nothing else will ever mention again.
            ProcessEvent::Uninstalled { result } => {
                let name = agent.block_name.as_deref().unwrap_or("?");
                match result {
                    Ok(()) => info!(supervisor = %name, "supervisor uninstalled"),
                    Err(e) => warn!(
                        supervisor = %name,
                        error = %e,
                        "the retired supervisor could not undo its installation"
                    ),
                }
            }
        }
        agent.owes_report = true;
    }

    /// Stops all Managed Processes — each adapter honours `Shutdown` within its stop budget —
    /// before the goodbyes go out.
    pub async fn shutdown_processes(&mut self) {
        for agent in &mut self.agents {
            if let Some(commands) = agent.commands.take() {
                let _ = commands.send(ProcessCommand::Shutdown).await;
            }
        }
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

    /// ADR-0017 at the Engine seam: a name in `uninstalling` is told to uninstall — its adapter
    /// answers and exits on the command itself — while a name that merely changed is only
    /// stopped, so it keeps what it installed for its restart. Both end retired with a goodbye.
    #[tokio::test]
    async fn retiring_uninstalls_the_removed_and_only_stops_the_changed() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let (event_tx, events) = mpsc::channel(8);
        let mut agents = Vec::new();
        let mut saw_uninstall = Vec::new();
        for (index, name) in ["removed", "changed"].into_iter().enumerate() {
            let storage = Storage::new(dir.path().join(name)).expect("storage");
            let state = AgentState::new(name.to_string(), storage).expect("agent");
            let (commands_tx, mut commands_rx) = mpsc::channel::<ProcessCommand>(4);
            let (stop_tx, mut stop) = crate::service::runtime::shutdown_channel();
            let saw = Arc::new(AtomicBool::new(false));
            let events = crate::supervisor::ports::EventSender::new(index, event_tx.clone());
            let flag = saw.clone();
            // A stand-in adapter with the Runner's two exits: the command itself, answered,
            // or the fired shutdown. Dropping the receiver is how either exit is observed.
            tokio::spawn(async move {
                tokio::select! {
                    command = commands_rx.recv() => {
                        if let Some(ProcessCommand::Uninstall) = command {
                            flag.store(true, Ordering::SeqCst);
                            events.send(ProcessEvent::Uninstalled { result: Ok(()) }).await;
                        }
                    }
                    _ = stop.requested() => {}
                }
            });
            saw_uninstall.push(saw);
            agents.push(EngineAgent {
                state,
                commands: Some(commands_tx),
                stop: Some(stop_tx),
                block_name: Some(name.to_string()),
            });
        }
        let mut engine = Engine::with_processes(agents, events, event_tx);

        let goodbyes = engine
            .retire_supervisors(
                &["removed".to_string(), "changed".to_string()],
                &["removed".to_string()],
            )
            .await;

        assert_eq!(goodbyes.len(), 2, "both said their goodbye");
        assert!(
            saw_uninstall[0].load(Ordering::SeqCst),
            "the removed adapter was told to uninstall"
        );
        assert!(
            !saw_uninstall[1].load(Ordering::SeqCst),
            "the changed adapter was only stopped"
        );
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
