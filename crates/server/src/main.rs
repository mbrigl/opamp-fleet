//! OpAMP Fleet Server — the API-first control plane (ADR-0005).
//!
//! Serves the OpAMP HTTP endpoint (the fleet control loop), a rudimentary JSON API, and a single
//! static UI page — all on one port. Fleet state is held in memory for this first version.

mod api;
mod fleet;
mod opamp_endpoint;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;
use fleet::Fleet;
use opamp::transport::{DEFAULT_PORT, OPAMP_HTTP_PATH};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let fleet = Arc::new(Fleet::new());

    let app = Router::new()
        .route("/", get(index))
        .route(OPAMP_HTTP_PATH, post(opamp_endpoint::handle))
        .route("/api/agents", get(api::agents))
        .route("/api/config", get(api::get_config).put(api::put_config))
        .with_state(fleet);

    let port = std::env::var("OPAMP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding to {addr}"))?;
    info!(%addr, "OpAMP Fleet Server listening (UI at http://127.0.0.1:{port}/)");

    axum::serve(listener, app)
        .await
        .context("running the HTTP server")?;
    Ok(())
}

/// The rudimentary UI: a single static page embedded at compile time.
async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}
