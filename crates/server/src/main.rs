//! Entry point: bind one listener, serve until interrupted. Runs on the protocol's default port;
//! the configuration file (ADR-0008) and the TLS listener (ADR-0007) arrive with their ADRs.

use std::path::PathBuf;
use std::sync::Arc;

use server::fleet::AppState;
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let state = Arc::new(AppState::new(PathBuf::from("fleet-config.yaml")));
    let app = server::app(state);

    let listen = "0.0.0.0:4320";
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .expect("bind the listener");
    info!(listen, "serving REST API and UI");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutting down");
        })
        .await
        .expect("serve");
}
