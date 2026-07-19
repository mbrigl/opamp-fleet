//! The JSON REST API for fleet state and configuration (ADR-0007).
//!
//! Served under `/api` on the same listener as the HTML UI (`:4321`), so it shares the UI's firewall
//! boundary and its (currently unauthenticated) trust model ([ADR-0007](../../docs/adr/0007-rest-api-and-fleet-ui.md)).
//! It adds no distribution path of its own: `PUT /api/config` writes `config/collector.yaml`, exactly
//! as the HTML form does, and the server's poll loop distributes it.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use serde::Serialize;
use tracing::{error, info};

use crate::server::FleetPush;
use crate::ui::UiState;

/// The REST API routes, to be merged into the UI listener's router.
pub fn router(state: UiState) -> Router {
    Router::new()
        .route("/api/fleet", get(fleet))
        .route("/api/fleet/events", get(fleet_events))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/agents/{uid}/restart", post(restart_agent))
        .with_state(state)
}

/// `POST /api/agents/{uid}/restart` — asks the agent with this instance UID (hex) to restart (ADR-0011).
/// `202` once the command is fanned out to the agent's connection, `404` if no such agent is connected,
/// `400` if the UID is not hex. Writing the config is still the only *reconfiguration* path (ADR-0007);
/// this is a separate, targeted control action.
async fn restart_agent(State(state): State<UiState>, Path(uid): Path<String>) -> Response {
    let Ok(bytes) = hex::decode(&uid) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid agent uid (expected hex)".to_string(),
        );
    };
    if !state.fleet.is_connected(&bytes) {
        return api_error(
            StatusCode::NOT_FOUND,
            "no connected agent with that uid".to_string(),
        );
    }
    let _ = state.pushes.send(FleetPush::Restart(bytes));
    info!(agent = %uid, "restart requested from the API");
    StatusCode::ACCEPTED.into_response()
}

/// `GET /api/fleet` — the current fleet as JSON.
async fn fleet(State(state): State<UiState>) -> Response {
    let want = state.config.current();
    Json(state.fleet.api_snapshot(want.as_ref())).into_response()
}

/// `GET /api/fleet/events` — a Server-Sent-Events stream: the fleet on connect, then again on every
/// change (an agent connects, reports, or disconnects), with keep-alive comments between changes.
async fn fleet_events(
    State(state): State<UiState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.fleet.subscribe();
    // `first` sends the current fleet immediately; afterwards each item waits for the next change.
    let stream =
        futures_util::stream::unfold((state, rx, true), |(state, mut rx, first)| async move {
            if !first && rx.changed().await.is_err() {
                return None;
            }
            let want = state.config.current();
            let snapshot = state.fleet.api_snapshot(want.as_ref());
            let event = Event::default()
                .json_data(&snapshot)
                .unwrap_or_else(|_| Event::default());
            Some((Ok(event), (state, rx, false)))
        });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

/// `GET /api/config` — the current collector configuration as raw YAML, read from disk.
async fn get_config(State(state): State<UiState>) -> Response {
    match state.config.read() {
        Ok(body) => ([(header::CONTENT_TYPE, "text/yaml; charset=utf-8")], body).into_response(),
        Err(e) => {
            error!(error = %e, "cannot read collector configuration for the API");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot read the collector configuration: {e}"),
            )
        }
    }
}

/// `PUT /api/config` — replace the collector configuration. The body is the YAML; the same guards as
/// the HTML form apply (reject empty, normalise line endings). Writing the file is the only way to
/// reconfigure the fleet, so this distributes nothing itself.
async fn put_config(State(state): State<UiState>, body: String) -> Response {
    if body.trim().is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "the configuration is empty; refusing to push it to the fleet".to_string(),
        );
    }
    let mut body = body.replace("\r\n", "\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    if let Err(e) = state.config.write(body.as_bytes()) {
        error!(error = %e, "cannot write collector configuration from the API");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot write the collector configuration: {e}"),
        );
    }
    info!(
        bytes = body.len(),
        "collector configuration written from the API"
    );
    StatusCode::NO_CONTENT.into_response()
}

/// A JSON error body, so API clients get structured errors rather than an HTML page.
#[derive(Serialize)]
struct ApiError {
    error: String,
}

fn api_error(code: StatusCode, message: String) -> Response {
    (code, Json(ApiError { error: message })).into_response()
}
