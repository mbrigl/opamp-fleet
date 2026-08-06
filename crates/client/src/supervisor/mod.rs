//! The supervision domain (ADR-0011): builds the Agents the [`Engine`](crate::engine) carries.
//!
//! With `[[supervisor]]` blocks configured, each becomes one Supervisor-backed Agent — its state
//! under `<state_dir>/supervisors/<name>/`, its Managed Process driven by the plugin the block's
//! `type` selects. Without any, the Client presents itself as the single self-Agent — the same
//! state machine with no Managed Process behind it.

pub mod agent;
pub mod collector;
pub mod command;
pub mod endpoint;
pub mod ports;
pub mod process;

use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::ClientConfig;
use crate::engine::Engine;
use crate::service::runtime::Shutdown;
use crate::storage::Storage;

use agent::AgentState;
use ports::{EventSender, Plugin, SupervisorContext};

/// The Engine index of the Client's own Agent (ADR-0020). It is built first, so a Supervisor's
/// index is its block's position plus this.
pub const SELF_AGENT_INDEX: usize = 0;

/// What a Supervisor's block position must be shifted by to reach its Engine index.
const SELF_AGENT_OFFSET: usize = SELF_AGENT_INDEX + 1;

/// The compiled-in plugin registry (ADR-0011). A new process kind is a new module and one line
/// here — the supervision core stays untouched (goal 8).
fn registry() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(collector::CollectorPlugin),
        Box::new(command::CommandPlugin),
    ]
}

/// Build the Engine from the configuration, starting one adapter task per Supervisor.
///
/// # Errors
/// Returns an error when an Agent's state cannot be restored, a `[[supervisor]]` block names an
/// unknown plugin, or a plugin rejects its settings — startup fails loudly, nothing runs half.
pub fn build_engine(config: &ClientConfig, shutdown: &Shutdown) -> Result<Engine, String> {
    // Heartbeats are a Client-wide choice: enabled (interval > 0) every Agent declares the
    // capability; disabled none does — an undeclared capability must never be exercised.
    let declare_heartbeat = |mut state: AgentState| {
        if config.heartbeat_interval_secs > 0 {
            state.declare_capability(opamp::proto::AgentCapabilities::ReportsHeartbeat);
        }
        state
    };
    let plugins = registry();
    let (event_tx, events) = mpsc::channel(64);
    let mut agents = Vec::with_capacity(config.supervisors.len() + 1);

    // The Client is always its own Agent (ADR-0020), whether or not it supervises anything. It
    // used to exist only when nothing else did, which left the Client invisible on exactly the
    // hosts that manage something — and left the Server with nobody to offer the Client's own
    // package to. It is index 0 so the Supervisors that follow keep a stable, obvious offset.
    let storage = Storage::new(config.state_dir.clone())
        .map_err(|e| format!("cannot prepare {}: {e}", config.state_dir.display()))?;
    let mut self_state = declare_heartbeat(
        AgentState::new(config.name.clone(), storage)
            .map_err(|e| format!("cannot restore the agent state: {e}"))?
            .with_attributes(config.agent_attributes(None)),
    );
    // Consenting to be updated is its own decision, made per Client, and it names the package it
    // will take — anything else is refused rather than written over this binary (ADR-0020).
    if let Some(self_update) = &config.self_update {
        self_state.accept_packages_named(self_update.package.clone());
    }
    agents.push((self_state, None));

    for (block_index, block) in config.supervisors.iter().enumerate() {
        // The event channel is keyed by position in `agents`, and the self-Agent holds 0.
        let index = block_index + SELF_AGENT_OFFSET;
        let plugin = plugins
            .iter()
            .find(|p| p.kind() == block.kind)
            .ok_or_else(|| {
                let known: Vec<&str> = plugins.iter().map(|p| p.kind()).collect();
                format!(
                    "supervisor {:?}: unknown type {:?} (known: {})",
                    block.name,
                    block.kind,
                    known.join(", ")
                )
            })?;

        let state_dir = config.state_dir.join("supervisors").join(&block.name);
        let storage = Storage::new(state_dir.clone())
            .map_err(|e| format!("cannot prepare {}: {e}", state_dir.display()))?;
        let config_dir = storage.config_dir();
        let mut state = declare_heartbeat(
            AgentState::supervised(block.name.clone(), storage)
                .map_err(|e| format!("cannot restore the state of {:?}: {e}", block.name))?
                .with_attributes(config.agent_attributes(Some(block))),
        );
        // A Supervisor that consents takes whichever top-level package the Server selects for it
        // (ADR-0015, ADR-0017).
        if block.accepts_packages {
            state.accept_packages();
        }

        // The Supervisor Endpoint is intrinsic to every Supervisor (ADR-0003): bound
        // unconditionally, before the process starts — a taken port fails startup, not later.
        endpoint::start(
            block.name.clone(),
            block.endpoint_port,
            EventSender::new(index, event_tx.clone()),
            shutdown.clone(),
            config.max_message_size_bytes,
        )?;

        let commands = plugin.start(SupervisorContext {
            name: block.name.clone(),
            config_dir,
            stop_timeout: Duration::from_secs(block.stop_timeout_secs),
            apply_grace: Duration::from_secs(block.apply_grace_secs),
            archive_key: config.packages.as_ref().and_then(|p| p.archive_key.clone()),
            settings: block.settings.clone(),
            events: EventSender::new(index, event_tx.clone()),
            shutdown: shutdown.clone(),
        })?;
        agents.push((state, Some(commands)));
    }
    Ok(Engine::with_processes(agents, events))
}
