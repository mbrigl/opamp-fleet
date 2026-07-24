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
    if config.supervisors.is_empty() {
        let storage = Storage::new(config.state_dir.clone())
            .map_err(|e| format!("cannot prepare {}: {e}", config.state_dir.display()))?;
        let state = AgentState::new(config.name.clone(), storage)
            .map_err(|e| format!("cannot restore the agent state: {e}"))?
            .with_attributes(config.agent_attributes(None));
        return Ok(Engine::new(vec![declare_heartbeat(state)]));
    }

    let plugins = registry();
    let (event_tx, events) = mpsc::channel(64);
    let mut agents = Vec::with_capacity(config.supervisors.len());
    for (index, block) in config.supervisors.iter().enumerate() {
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
        let state = declare_heartbeat(
            AgentState::supervised(block.name.clone(), storage)
                .map_err(|e| format!("cannot restore the state of {:?}: {e}", block.name))?
                .with_attributes(config.agent_attributes(Some(block))),
        );

        // The Supervisor Endpoint is intrinsic to every Supervisor (ADR-0003): bound
        // unconditionally, before the process starts — a taken port fails startup, not later.
        endpoint::start(
            block.name.clone(),
            block.endpoint_port,
            EventSender::new(index, event_tx.clone()),
            shutdown.clone(),
        )?;

        let commands = plugin.start(SupervisorContext {
            name: block.name.clone(),
            config_dir,
            stop_timeout: Duration::from_secs(block.stop_timeout_secs),
            apply_grace: Duration::from_secs(block.apply_grace_secs),
            settings: block.settings.clone(),
            events: EventSender::new(index, event_tx.clone()),
            shutdown: shutdown.clone(),
        })?;
        agents.push((state, Some(commands)));
    }
    Ok(Engine::with_processes(agents, events))
}
