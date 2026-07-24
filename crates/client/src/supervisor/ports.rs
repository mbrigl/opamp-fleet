//! The Managed-Process-facing Port (ADR-0011): the boundary the supervision domain defines and
//! depends on. A Plugin is an adapter behind it — a factory that validates its block's settings
//! and starts a task driving one Managed Process.
//!
//! The Port is a message pair, not a trait with async methods: commands flow to the adapter,
//! events flow back. That keeps the [`Plugin`] trait object-safe without an `async-trait`
//! dependency, makes every adapter a plain tokio task, and keeps the domain core free of
//! process handles.

use std::path::PathBuf;
use std::time::Duration;

use opamp::proto::{
    AgentDescription, AgentRemoteConfig, AvailableComponents, ComponentHealth, EffectiveConfig,
};
use tokio::sync::mpsc;

use crate::service::runtime::Shutdown;

/// What the supervision core asks of a Managed Process.
#[derive(Debug)]
pub enum ProcessCommand {
    /// A remote configuration was received and persisted — the entry files are already written
    /// to the adapter's [`config_dir`](SupervisorContext::config_dir). Apply it, which for a
    /// process means restarting on the new files, and answer with
    /// [`ProcessEvent::ConfigApplied`].
    ApplyConfig { config: AgentRemoteConfig },
    /// The Server commanded a restart (`AcceptsRestartCommand`): stop and respawn on the
    /// *current* files. No configuration changed, so no [`ProcessEvent::ConfigApplied`] follows —
    /// the health events of the stop/spawn cycle are the visible outcome.
    Restart,
    /// Stop the Managed Process gracefully.
    Shutdown,
}

/// What a Managed-Process adapter reports back to the core.
#[derive(Debug)]
pub enum ProcessEvent {
    /// The process's own description (reported through the Supervisor Endpoint), folded into
    /// the Agent's — its identity (`service.instance.id`) stays the Supervisor's.
    Description(AgentDescription),
    /// Health — derived from the outside (spawned, exited, spawn failed) or self-reported.
    Health(ComponentHealth),
    /// The process's self-reported effective configuration; replaces the written-files echo.
    EffectiveConfig(EffectiveConfig),
    /// The process's available components (reported through the Supervisor Endpoint by the
    /// Collector's `opampextension`), relayed upstream under the owning Agent.
    AvailableComponents(AvailableComponents),
    /// Outcome of an [`ProcessCommand::ApplyConfig`]: `Ok` acknowledges `APPLIED`, `Err`
    /// reports `FAILED` with the error — a rejected configuration is a report, not a silence.
    ConfigApplied {
        hash: Vec<u8>,
        result: Result<(), String>,
    },
}

/// The adapter's way back into the core: events tagged with the owning Agent's index on the
/// shared channel the [`Engine`](crate::engine::Engine) drains.
#[derive(Debug, Clone)]
pub struct EventSender {
    index: usize,
    tx: mpsc::Sender<(usize, ProcessEvent)>,
}

impl EventSender {
    #[must_use]
    pub fn new(index: usize, tx: mpsc::Sender<(usize, ProcessEvent)>) -> Self {
        EventSender { index, tx }
    }

    /// Sends one event; a closed channel means the Engine is gone and the event is moot.
    pub async fn send(&self, event: ProcessEvent) {
        let _ = self.tx.send((self.index, event)).await;
    }
}

/// Everything a plugin needs to start its adapter task.
pub struct SupervisorContext {
    /// The Supervisor's name (the TOML `name`; the Agent's `service.name`).
    pub name: String,
    /// Where the received remote configuration's entry files are written — what the Managed
    /// Process is pointed at.
    pub config_dir: PathBuf,
    /// Graceful-stop budget before the Managed Process is killed.
    pub stop_timeout: Duration,
    /// How long a freshly (re)started process must survive before `ApplyConfig` is acknowledged
    /// `Ok` — the health-gated acknowledgement (ADR-0011). Zero acknowledges on start.
    pub apply_grace: Duration,
    /// The plugin-specific keys of the block, for the strict second-stage parse.
    pub settings: toml::Table,
    /// Where the adapter reports events.
    pub events: EventSender,
    /// The Client's shutdown signal; the adapter stops its process and exits when it fires.
    pub shutdown: Shutdown,
}

/// A compiled-in Supervisor Plugin (ADR-0011): the adapter factory on the Managed-Process side.
/// A new process kind is a new implementation and one line in
/// [`registry`](crate::supervisor::registry).
pub trait Plugin {
    /// The TOML `type` value this plugin serves.
    fn kind(&self) -> &'static str;

    /// Validate the settings and start the adapter task, returning the command side of the Port.
    ///
    /// # Errors
    /// Returns an error when the settings do not parse — startup fails loudly, nothing spawns.
    fn start(&self, ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String>;
}
