//! The Managed-Process-facing Port (ADR-0017): the boundary the supervision domain defines and
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
    /// The Server commanded a restart (`AcceptsRestartCommand`): stop and respawn on the
    /// *current* files. No configuration changed, so no [`ProcessEvent::ConfigApplied`] follows —
    /// the health events of the stop/spawn cycle are the visible outcome.
    Restart,
    /// Stop the Managed Process gracefully.
    Shutdown,
    /// The Supervisor is retired for good (ADR-0017): stop the Managed Process, undo whatever
    /// installing it left *outside* the Supervisor's directory — the generic implementation has
    /// nothing there, so its uninstall is exactly the graceful stop — answer with
    /// [`ProcessEvent::Uninstalled`], and exit. The directory itself is the core's to purge
    /// (ADR-0017), after the answer.
    Uninstall,
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
    /// Outcome of a [`ProcessCommand::Uninstall`] (ADR-0017), the adapter's last event. The
    /// Agent's goodbye carries no status, so the outcome is a log line — but an `Err` names what
    /// the retired kind could not undo, which the operator otherwise learns from nothing.
    Uninstalled { result: Result<(), String> },
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
    /// `Ok` — the health-gated acknowledgement (ADR-0017). Zero acknowledges on start.
    pub apply_grace: Duration,
    /// The plugin-specific keys of the block, for the strict second-stage parse.
    pub settings: toml::Table,
    /// Where the adapter reports events.
    pub events: EventSender,
    /// The Client's shutdown signal; the adapter stops its process and exits when it fires.
    pub shutdown: Shutdown,
}

/// What a kind knows about its own agent, so a block does not have to say it (ADR-0017).
///
/// Every field is a `&'static str` resolved **per platform** at compile time, which is the point:
/// a constant can be asserted against the artifact this project packs, where a path written in a
/// manual can only be believed. A kind that knows nothing — `collector`, `command` — returns
/// [`KindDefaults::none`] and its blocks keep saying what they always said.
///
/// A value supplied here is not a default the block may override. It is the answer, and a block
/// naming the key is refused: two sources of truth for a value this Client computes is how a host
/// quietly differs from what the fleet believes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindDefaults {
    /// The program's file name in this Supervisor's own `program/` directory (ADR-0017).
    /// `None` leaves it to the block's program key.
    pub program: Option<&'static str>,
    /// Where the program sits inside an unpacked package tree (ADR-0028); `None` is a single-file
    /// package, or a kind that leaves the question to the block.
    pub program_path: Option<&'static str>,
    /// The Agent *type* this kind presents (ADR-0015). `None` falls back to the program's file
    /// name, which is what the block already said.
    pub service_name: Option<&'static str>,
    /// What this kind corrects about the fleet's timing policy, and whether its block may state
    /// any of it at all (ADR-0017). `None` — an unwrapped kind — leaves the three keys in the
    /// block, because no kind exists there to hold a value; `Some` takes them out of the block and
    /// states the kind's own corrections, each `None` meaning "the fleet's number is right".
    pub timing: Option<KindTiming>,
    /// Whether a block of this kind may pin the Supervisor Endpoint's port. The Endpoint is bound
    /// for every Supervisor (ADR-0014); pinning it only means something where a Managed Process
    /// connects to it, which in practice is a Collector carrying the `opampextension`.
    pub endpoint_port: bool,
}

/// What a wrapped kind says about timing, over the fleet's own policy (ADR-0017).
///
/// Every field is an *agent's* property rather than a host's: how long it needs to shut down, how
/// long a restart of it has to hold before the fleet may believe it, how long its superseded
/// version is worth keeping. A kind that has nothing to correct states three `None`s and still
/// takes the keys out of its block — the value is the fleet's, and there is no host-level answer
/// to a question about an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KindTiming {
    /// Overrides `[supervisors] stop_timeout_secs`.
    pub stop_timeout: Option<Duration>,
    /// Overrides `[supervisors] apply_grace_secs`.
    pub apply_grace: Option<Duration>,
    /// Overrides `[updates] retain_previous_secs` (ADR-0028).
    pub retain_previous: Option<Duration>,
}

impl KindDefaults {
    /// A kind that knows nothing about its agent: the block says everything, as it always has.
    #[must_use]
    pub const fn none() -> Self {
        KindDefaults {
            program: None,
            program_path: None,
            service_name: None,
            timing: None,
            endpoint_port: false,
        }
    }
}

/// A compiled-in Supervisor Plugin (ADR-0017): the adapter factory on the Managed-Process side.
/// A new process kind is a new implementation and one line in
/// [`registry`](crate::supervisor::registry).
pub trait Plugin {
    /// The TOML `type` value this plugin serves.
    fn kind(&self) -> &'static str;

    /// What this kind knows about its agent, so a block need not repeat it (ADR-0017). Stated by
    /// every plugin rather than defaulted, because "this kind knows nothing" is an answer worth
    /// writing down where a reader of the plugin will see it.
    fn defaults(&self) -> KindDefaults;

    /// Validate the settings and start the adapter task, returning the command side of the Port.
    ///
    /// # Errors
    /// Returns an error when the settings do not parse — startup fails loudly, nothing spawns.
    fn start(&self, ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String>;
}
mod tests {
    use super::*;
    use crate::service::runtime::shutdown_channel;
