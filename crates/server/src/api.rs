//! The REST API — the Server's integration contract — and the bundled rudimentary UI (ADR-0005).
//!
//! The UI is a client of this API and nothing more; an external portal drives the fleet through
//! exactly the same routes.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tracing::info;

use crate::fleet::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/agents", get(agents))
        .route("/api/config", get(get_config).put(put_config))
        .with_state(state)
}

/// The bundled UI: one embedded page, no frontend toolchain (ADR-0005).
async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

/// `GET /api/agents` — the fleet as JSON.
async fn agents(State(state): State<Arc<AppState>>) -> Response {
    Json(state.snapshot()).into_response()
}

#[derive(Serialize)]
struct ConfigView {
    config: String,
    /// Hex SHA-256 of the body — the identity the control loop compares.
    hash: String,
}

/// `GET /api/config` — the configuration the Server currently wants the fleet to run.
async fn get_config(State(state): State<Arc<AppState>>) -> Response {
    let view = match state.desired_config() {
        Some(desired) => ConfigView {
            config: desired.body,
            hash: hex::encode(desired.hash),
        },
        None => ConfigView {
            config: String::new(),
            hash: String::new(),
        },
    };
    Json(view).into_response()
}

/// `PUT /api/config` — replace the desired configuration; the body is the raw configuration text.
/// Distribution follows from state: polling Agents pick it up on their next exchange, WebSocket
/// Agents are pushed immediately.
async fn put_config(State(state): State<Arc<AppState>>, body: String) -> Response {
    if body.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"the configuration is empty; refusing to push it to the fleet"}"#,
        )
            .into_response();
    }
    let mut body = body.replace("\r\n", "\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    match state.set_desired_config(body) {
        Ok(config) => {
            info!(
                bytes = config.body.len(),
                "configuration distributed from the API"
            );
            Json(ConfigView {
                config: config.body,
                hash: hex::encode(config.hash),
            })
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "application/json")],
            format!(r#"{{"error":"{e}"}}"#),
        )
            .into_response(),
    }
}
