//! The Supervisor Host: the process that runs the supervisors this project hosts (ADR-0009).
//!
//! Each supervisor is its own OpAMP Agent — its own connection, Instance UID, and storage — and runs as
//! its own task. The Host spawns them, then owns startup and shutdown for all of them. Adding a kind of
//! agent is a new adapter behind the [`ManagedAgent`](crate::agent::ManagedAgent) port, not a change
//! here.

use std::sync::Arc;

use tokio::sync::Notify;
use tracing::info;

use crate::agent::{ManagedAgent, GRACEFUL_SHUTDOWN_TIMEOUT};
use crate::supervisor::Supervisor;

/// A margin added to the per-process graceful-stop timeout, so the Host waits a little longer than any
/// single agent's SIGTERM window before giving up on a clean shutdown and exiting.
const SHUTDOWN_JOIN_MARGIN: std::time::Duration = std::time::Duration::from_secs(2);

/// The Supervisor Host process: it owns and runs its supervisors.
pub struct SupervisorHost {
    handles: Vec<tokio::task::JoinHandle<()>>,
    /// Fired on process shutdown to cancel every supervisor's run loop so it can stop its agent
    /// gracefully, rather than being aborted and hard-killed on drop.
    shutdown: Arc<Notify>,
}

impl SupervisorHost {
    /// A host running no supervisors yet.
    pub fn new() -> Self {
        Self {
            handles: Vec::new(),
            shutdown: Arc::new(Notify::new()),
        }
    }

    /// Spawns a supervisor as its own task; it runs its OpAMP loop until the host signals shutdown, then
    /// stops its managed agent gracefully.
    pub fn spawn<A: ManagedAgent>(&mut self, mut supervisor: Supervisor<A>) {
        let shutdown = self.shutdown.clone();
        self.handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = supervisor.run() => {}
                _ = shutdown.notified() => {}
            }
            supervisor.shutdown().await;
        }));
    }

    /// How many supervisors the host is running.
    pub fn len(&self) -> usize {
        self.handles.len()
    }

    /// Whether the host is running no supervisors.
    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// Runs until the process is asked to stop (Ctrl-C / SIGTERM), then signals every supervisor to stop
    /// its agent gracefully and waits, bounded, for them to finish.
    pub async fn run(mut self) {
        shutdown_signal().await;
        info!(supervisors = self.handles.len(), "shutting down");
        self.shutdown.notify_waiters();

        let deadline = GRACEFUL_SHUTDOWN_TIMEOUT + SHUTDOWN_JOIN_MARGIN;
        for handle in self.handles.drain(..) {
            let _ = tokio::time::timeout(deadline, handle).await;
        }
    }
}

impl Default for SupervisorHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Completes when the process is asked to stop — Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
