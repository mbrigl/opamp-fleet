//! The hexagonal **Managed-Agent port** (ADR-0009).
//!
//! The OpAMP client loop ([`crate::supervisor::Supervisor`]) is the domain; it is written against this
//! port, never against a concrete agent. Placing a new kind of agent — an OpAMP-native Collector, or a
//! non-OpAMP Foreign Agent — under management is a new **adapter** that implements [`ManagedAgent`], not
//! a change to the domain.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::Notify;

use opamp_proto::proto::{AgentDescription, AvailableComponents, ComponentHealth, EffectiveConfig};

/// What a managed agent reports about itself right now. The adapter folds its own channel (a local
/// OpAMP server, process liveness, …) into this; the domain reads the latest snapshot.
#[derive(Default, Clone)]
pub struct AgentStatus {
    pub health: ComponentHealth,
    /// The agent's effective configuration, when it reports one (the Collector does; a Foreign Agent
    /// echoes what was written).
    pub effective_config: Option<EffectiveConfig>,
    /// The agent's own description, when it reports one (the Collector does over its local server).
    pub agent_description: Option<AgentDescription>,
    /// The components the agent reports available, when it reports them.
    pub available_components: Option<AvailableComponents>,
}

/// A cloneable handle to await a managed agent's next status change, held **outside** the agent so the
/// OpAMP loop can await it without borrowing the agent mutably. An adapter with no push channel returns
/// [`ChangeSignal::never`], whose [`changed`](ChangeSignal::changed) never completes.
#[derive(Clone)]
pub struct ChangeSignal(Option<Arc<Notify>>);

impl ChangeSignal {
    /// A signal backed by a `Notify` the adapter fires on a meaningful change.
    pub fn new(notify: Arc<Notify>) -> Self {
        Self(Some(notify))
    }

    /// A signal that never fires — for adapters (e.g. a Foreign Agent) with no push channel.
    pub fn never() -> Self {
        Self(None)
    }

    /// Completes when the agent next reports a meaningful change (or never, for [`ChangeSignal::never`]).
    pub async fn changed(&self) {
        match &self.0 {
            Some(n) => n.notified().await,
            None => std::future::pending().await,
        }
    }
}

/// The Managed-Agent-facing driven port. See the module docs.
pub trait ManagedAgent: Send + 'static {
    /// Transform the remote config before it is applied (e.g. the Collector injects its `opamp`
    /// extension so it reports back). Default: unchanged.
    fn prepare_config(&self, config: Vec<u8>) -> Vec<u8> {
        config
    }

    /// Apply a prepared config and make it take effect. `Err(message)` is reported as a `FAILED`
    /// remote-config status carrying the message.
    fn apply(&mut self, config: &[u8]) -> impl Future<Output = Result<(), String>> + Send;

    /// Restart the agent on the config already applied — recovery after a crash, or a Server restart
    /// command.
    fn restart(&mut self) -> impl Future<Output = Result<(), String>> + Send;

    /// The agent's current self-reported status (health always; effective config / description /
    /// available components when the agent reports them).
    fn status(&self) -> AgentStatus;

    /// A handle to await the agent's next meaningful status change, for prompt forwarding.
    fn change_signal(&self) -> ChangeSignal;

    /// Check whether the agent exited unexpectedly since the last call; returns the exit reason if it
    /// had (so the domain reports it unhealthy and calls [`restart`](ManagedAgent::restart)), or `None`
    /// if it is still running. Detection only — recovery is the domain's job.
    fn supervise(&mut self) -> impl Future<Output = Option<String>> + Send;
}

/// A liveness-based [`ComponentHealth`], used by adapters (and the domain, for apply/crash reports) so
/// the health an agent reports is shaped consistently.
pub fn liveness_health(
    healthy: bool,
    last_error: String,
    start_time_unix_nano: u64,
) -> ComponentHealth {
    ComponentHealth {
        healthy,
        start_time_unix_nano,
        last_error,
        status: if healthy { "Running" } else { "Errored" }.to_string(),
        ..Default::default()
    }
}
