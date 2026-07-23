//! Entry point: load `server.toml`, bind one listener, serve until interrupted. The TLS listener
//! (ADR-0007) arrives with its ADR.

use std::path::PathBuf;
use std::sync::Arc;

use server::config::ServerConfig;
use server::fleet::AppState;
use tracing::info;

fn usage() -> ! {
    eprintln!("Usage: server [--config <server.toml>] [--version]");
    std::process::exit(2);
}

fn parse_args() -> PathBuf {
    let mut config = PathBuf::from("server.toml");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => match args.next() {
                Some(path) => config = PathBuf::from(path),
                None => usage(),
            },
            "--version" => {
                println!("server {}", env!("CARGO_PKG_VERSION"));
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
    let config = match ServerConfig::load(&config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(AppState::new(config.fleet_config_file.clone()));
    let app = server::app(state);

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .expect("bind the listener");
    info!(listen = %config.listen, "serving REST API and UI");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutting down");
        })
        .await
        .expect("serve");
}
