//! The hexagonal **Managed-Agent port** (ADR-0009).
//!
//! The OpAMP client loop ([`crate::supervisor::Supervisor`]) is the domain; it is written against this
//! port, never against a concrete agent. Placing a new kind of agent — an OpAMP-native Collector, or a
//! non-OpAMP Foreign Agent — under management is a new **adapter** that implements [`ManagedAgent`], not
//! a change to the domain.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Child;
use tokio::sync::Notify;

use opamp_proto::proto::{AgentDescription, AvailableComponents, ComponentHealth, EffectiveConfig};

/// A destination the Server offered for the agent's own telemetry: an OTLP/HTTP endpoint and optional
/// headers (e.g. an auth token), from an OpAMP `TelemetryConnectionSettings` (ADR-0010).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryDestination {
    pub endpoint: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// The destinations the Server has offered for the Collector's own metrics, logs, and traces (ADR-0010).
/// A `None` signal is one the Server has not offered (or the supervisor is not configured to report), and
/// is left unset in the Collector's telemetry config.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnTelemetry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<TelemetryDestination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<TelemetryDestination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traces: Option<TelemetryDestination>,
}

impl OwnTelemetry {
    /// Whether the Server has offered no own-telemetry destination at all.
    pub fn is_empty(&self) -> bool {
        self.metrics.is_none() && self.logs.is_none() && self.traces.is_none()
    }
}

/// How long a graceful stop waits for a process to exit after SIGTERM before resorting to SIGKILL.
pub(crate) const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// Combine the offered remote-config files into the single raw config body to apply. `files` are the
    /// OpAMP config-map entries in sorted-key order. The default takes the file under the main ("") key,
    /// else the first entry — correct for a single-file agent. The Collector overrides this to deep-merge
    /// multiple YAML files in key order, matching the Go supervisor. `None` means no usable config.
    fn merge_config(&self, files: &[(String, Vec<u8>)]) -> Option<Vec<u8>> {
        files
            .iter()
            .find(|(key, _)| key.is_empty())
            .or_else(|| files.first())
            .map(|(_, body)| body.clone())
    }

    /// Transform the remote config before it is applied (e.g. the Collector injects its `opamp`
    /// extension so it reports back). Default: unchanged.
    fn prepare_config(&self, config: Vec<u8>) -> Vec<u8> {
        config
    }

    /// Learn the agent's real identity and available components before the first Server report, so the
    /// full-state report carries the agent-authoritative description rather than a synthesized one —
    /// mirroring the Go supervisor's bootstrap step. Default: no-op (adapters without a discovery
    /// channel, e.g. a Foreign Agent, keep the synthesized description).
    fn bootstrap(&mut self) -> impl Future<Output = ()> + Send {
        async {}
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

    /// Gracefully stop the managed agent when the Host is shutting down — SIGTERM, wait, then SIGKILL —
    /// so a collector can flush and a foreign agent can run its shutdown instead of being hard-killed.
    /// Default: no-op (an adapter that owns no process has nothing to stop).
    fn shutdown(&mut self) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// Point the agent's own-telemetry reporting at the Server-offered destinations (ADR-0010),
    /// returning whether the effective settings changed — so the domain re-applies the running config to
    /// make the change take effect. Default: ignored, `false` (an adapter with no own-telemetry pipeline
    /// does not report its own telemetry).
    fn set_own_telemetry(&mut self, _settings: OwnTelemetry) -> bool {
        false
    }
}

/// Stops a child process gracefully, so a shutdown or a restart lets the process terminate cleanly
/// rather than being hard-killed. On **Unix** (Linux and macOS): send SIGTERM, wait up to
/// [`GRACEFUL_SHUTDOWN_TIMEOUT`] for it to exit, then SIGKILL if it is still running. On **Windows**,
/// which has no SIGTERM for another process, it terminates the process immediately (`TerminateProcess`,
/// via [`Child::start_kill`]) and waits for it to exit — a clean graceful stop there would need console
/// control events (and a Windows-only dependency), which is left to its own change.
pub(crate) async fn terminate(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: kill(2) with a process id we spawned and a valid signal number has no memory-safety
        // implications; the result is ignored because a failure means the process is already gone.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        if tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, child.wait())
            .await
            .is_ok()
        {
            return;
        }
        tracing::warn!(pid, "process did not exit after SIGTERM; sending SIGKILL");
    }
    // The Windows path, and the Unix SIGKILL escalation after the SIGTERM window: hard-stop and reap.
    let _ = child.start_kill();
    let _ = child.wait().await;
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
