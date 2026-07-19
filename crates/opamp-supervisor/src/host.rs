//! The Supervisor Host: the process that runs the supervisors this project hosts (ADR-0008).
//!
//! Today it hosts exactly one — the OpAMP-native [`Supervisor`] (the Collector Supervisor). Running
//! *many* supervisors as plugins, and Custom Supervisors that bring non-OpAMP Foreign Agents into the
//! same control loop, is the next milestone (its own ADR); this is the shape that grows into it.

use tracing::info;

use crate::supervisor::Supervisor;

/// The Supervisor Host process: it owns and runs its supervisors.
pub struct SupervisorHost {
    supervisor: Supervisor,
}

impl SupervisorHost {
    /// A host wrapping the one supervisor it runs today.
    pub fn new(supervisor: Supervisor) -> Self {
        Self { supervisor }
    }

    /// Runs the hosted supervisor until the process is asked to stop (Ctrl-C / SIGTERM). On shutdown
    /// the collector the supervisor owns is torn down with it (`kill_on_drop`).
    pub async fn run(mut self) {
        tokio::select! {
            _ = self.supervisor.run() => {}
            _ = shutdown_signal() => info!("shutting down"),
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
