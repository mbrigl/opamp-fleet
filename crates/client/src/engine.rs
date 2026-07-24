//! The Engine: n Agents over one upstream connection (ADR-0003, ADR-0011).
//!
//! This is the Server-facing seam of the hexagonal core. The transports consume it — build
//! reports, hand it every decoded `ServerToAgent`, ask for the goodbyes — and it routes each
//! reply to the owning Agent by `instance_uid` alone, never by connection. With one self-Agent
//! it behaves exactly like the single-Agent Client did; with Supervisors it multiplexes them.

use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::uid::InstanceUid;
use tokio::sync::mpsc;
use tracing::warn;

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
}

impl Engine {
    /// An Engine over Agents without Managed Processes (the self-Agent case, and tests).
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
        Engine {
            agents: agents
                .into_iter()
                .map(|(state, commands)| SupervisedAgent {
                    state,
                    commands,
                    owes_report: false,
                })
                .collect(),
            events,
        }
    }

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
        let Some(agent) = self.agents.iter_mut().find(|a| a.state.uid() == uid) else {
            warn!(agent = %uid, "dropping a reply for an unknown agent");
            return Handled::default();
        };
        let handled = agent.state.handle(reply);
        if handled.send_report {
            agent.owes_report = true;
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
                                ProcessCommand::Restart | ProcessCommand::Shutdown => Vec::new(),
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
            ProcessEvent::ConfigApplied { hash, result } => {
                agent.state.config_applied(hash, result);
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
