//! Runs the Supervisor Host (skeleton).
//!
//! Today it starts, reports that it holds no plugins yet, and exits. The plugin/hexagonal
//! implementation — loading Collector and Custom supervisors and running each as an OpAMP Agent
//! against the Server — follows in a later change (ADR-0005).

use opamp_supervisor::host::SupervisorHost;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let host = SupervisorHost::new();
    info!(
        supervisors = host.supervisors().len(),
        "Supervisor Host starting (skeleton — no plugins registered yet)"
    );
    info!("nothing to run yet: the plugin/hexagonal implementation follows in a later change (ADR-0005)");
}
