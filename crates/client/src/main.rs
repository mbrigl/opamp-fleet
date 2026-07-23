//! Entry point: restore the Agent's identity and state. This commit carries the
//! transport-agnostic Agent core (ADR-0005); the configuration file (ADR-0008) and the transports
//! that put the Agent on the wire (ADR-0007) arrive with their ADRs.

mod agent;
mod storage;

use std::path::PathBuf;

use agent::Agent;
use storage::Storage;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let state_dir = PathBuf::from("client-state");
    let result = Storage::new(state_dir.clone())
        .map_err(|e| format!("cannot prepare {}: {e}", state_dir.display()))
        .and_then(|storage| {
            Agent::new("opamp-fleet-client".to_string(), storage)
                .map_err(|e| format!("cannot restore the agent state: {e}"))
        });
    match result {
        Ok(agent) => {
            tracing::info!(agent = %agent.uid(), "agent identity ready; transports arrive with ADR-0007");
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
