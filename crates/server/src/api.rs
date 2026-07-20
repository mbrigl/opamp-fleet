//! The rudimentary JSON API (ADR-0005).
//!
//! These two endpoints are the seed of the future OpenAPI-described REST API: the UI reads fleet
//! state from `GET /api/agents` and sets the desired remote configuration with `PUT /api/config`.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use tracing::info;

use crate::fleet::Fleet;

/// `GET /api/agents` — a snapshot of every connected Agent.
pub async fn agents(State(fleet): State<Arc<Fleet>>) -> impl IntoResponse {
    Json(fleet.snapshot())
}

/// `GET /api/config` — the desired remote configuration and its Config hash.
pub async fn get_config(State(fleet): State<Arc<Fleet>>) -> impl IntoResponse {
    Json(fleet.desired_config())
}

/// `PUT /api/config` — set the desired remote configuration (raw text body).
pub async fn put_config(State(fleet): State<Arc<Fleet>>, body: String) -> impl IntoResponse {
    let hash = fleet.set_desired_config(body.into_bytes());
    info!(config_hash = %hash, "desired remote configuration updated");
    Json(fleet.desired_config())
}
