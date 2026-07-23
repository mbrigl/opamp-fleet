//! Entry point: load `client.toml`, restore the Agent's identity, run the transport the endpoint
//! selects (ADR-0007) until interrupted.

mod agent;
mod config;
mod storage;
mod tls;
mod transport;
mod version;

use std::path::PathBuf;

use agent::Agent;
use config::{ClientConfig, TransportKind};
use storage::Storage;

fn usage() -> ! {
    eprintln!("Usage: client [--config <client.toml>] [--version]");
    std::process::exit(2);
}

fn parse_args() -> PathBuf {
    let mut config = PathBuf::from("client.toml");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => match args.next() {
                Some(path) => config = PathBuf::from(path),
                None => usage(),
            },
            "--version" => {
                // The git-derived build version (ADR-0009), never the static crate version.
                println!("client {}", version::version());
                std::process::exit(0);
            }
            _ => usage(),
        }
    }
    config
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // One TLS provider for the whole process (ADR-0007): ring, never a system library.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install the rustls ring provider");

    let config_path = parse_args();
    let result = run(&config_path).await;
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn run(config_path: &std::path::Path) -> Result<(), String> {
    let config = ClientConfig::load(config_path)?;
    let transport = config.transport()?;

    let storage = Storage::new(config.state_dir.clone())
        .map_err(|e| format!("cannot prepare {}: {e}", config.state_dir.display()))?;
    let agent = Agent::new(config.name.clone(), storage)
        .map_err(|e| format!("cannot restore the agent state: {e}"))?;
    tracing::info!(agent = %agent.uid(), name = %config.name, "starting");

    match transport {
        TransportKind::WebSocket => transport::ws::run(agent, &config).await,
        TransportKind::Http => transport::http::run(agent, &config).await,
    }
}
