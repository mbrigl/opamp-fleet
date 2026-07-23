//! Entry point: load `client.toml`, restore the Agent's identity and state. The transports that
//! put the Agent on the wire — selected by the endpoint's URL scheme — arrive with ADR-0007.

mod agent;
mod config;
mod storage;

use std::path::PathBuf;

use agent::Agent;
use config::ClientConfig;
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
                println!("client {}", env!("CARGO_PKG_VERSION"));
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

    let config_path = parse_args();
    if let Err(e) = run(&config_path).await {
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
    tracing::info!(
        agent = %agent.uid(),
        name = %config.name,
        endpoint = %config.endpoint,
        transport = ?transport,
        poll_interval_secs = config.poll_interval_secs,
        "agent identity ready; the transports arrive with ADR-0007"
    );
    Ok(())
}
