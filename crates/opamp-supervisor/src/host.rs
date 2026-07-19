//! The Supervisor Host: the process that runs the supervisors this project hosts (ADR-0009).
//!
//! Each supervisor is its own OpAMP Agent — its own connection, Instance UID, and storage — and runs as
//! its own task. The Host spawns them, then owns startup and shutdown for all of them. Adding a kind of
//! agent is a new adapter behind the [`ManagedAgent`](crate::agent::ManagedAgent) port, not a change
//! here.

use tracing::info;

use crate::agent::ManagedAgent;
use crate::supervisor::Supervisor;

/// The Supervisor Host process: it owns and runs its supervisors.
#[derive(Default)]
pub struct SupervisorHost {
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl SupervisorHost {
    /// A host running no supervisors yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawns a supervisor as its own task; it runs its OpAMP loop until the host shuts down.
    pub fn spawn<A: ManagedAgent>(&mut self, mut supervisor: Supervisor<A>) {
        self.handles.push(tokio::spawn(async move {
            supervisor.run().await;
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

    /// Runs until the process is asked to stop (Ctrl-C / SIGTERM), then tears every supervisor down —
    /// dropping each agent, whose `kill_on_drop` child processes are killed with it.
    pub async fn run(self) {
        shutdown_signal().await;
        info!(supervisors = self.handles.len(), "shutting down");
        for handle in &self.handles {
            handle.abort();
        }
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
