//! The fleet UI: one page showing every connected agent's reported state, and an editor for the
//! configuration the fleet runs (ADR-0007).
//!
//! The page is rendered server-side with `askama`, whose contextual auto-escaping is what keeps
//! agent-reported strings — which the server does not control — from becoming markup. Escaping here
//! is a security property, not a convenience.
//!
//! Writing the configuration is the only way to reconfigure the fleet: the server's watcher notices
//! the change and distributes it, so the UI has no distribution path of its own and the file stays
//! the single source of truth.

use std::sync::Arc;

use askama::Template;
use axum::extract::{Form, Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::{error, info};

use crate::config::ConfigSource;
use crate::fleet::{AgentState, Fleet};
use crate::packages::PackageSource;
use crate::server::FleetPush;

/// Shared state for the UI: the fleet to display, the configuration to read and write, the push
/// channel a restart command is fanned out on (ADR-0011), and the packages served for download (ADR-0018).
#[derive(Clone)]
pub struct UiState {
    pub fleet: Arc<Fleet>,
    pub config: Arc<ConfigSource>,
    pub pushes: broadcast::Sender<FleetPush>,
    pub packages: Arc<PackageSource>,
}

/// The fleet UI router: the page at `/`, the configuration write at `POST /config`, a per-agent restart
/// at `POST /agents/{uid}/restart` (ADR-0011), and the package download at `GET /packages/{name}`
/// (ADR-0018).
pub fn router(state: UiState) -> Router {
    Router::new()
        .route("/", get(show))
        .route("/config", post(save))
        .route("/agents/{uid}/restart", post(restart))
        .route("/packages/{name}", get(download_package))
        .with_state(state)
}

/// `GET /packages/{name}` — serves a configured package's bytes for an agent to install (ADR-0018). The
/// `download_url` in the offer points here; the whole `:4321` surface is gated by the shared token
/// (ADR-0012), and the offer carries that token so the agent's download is authenticated. `404` if no
/// package by that name is configured.
async fn download_package(State(state): State<UiState>, Path(name): Path<String>) -> Response {
    match state.packages.file(&name) {
        Some(bytes) => (
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes.to_vec(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no such package").into_response(),
    }
}

#[derive(Template)]
#[template(path = "page.html")]
struct Page {
    agents: Vec<AgentState>,
    config: String,
    error: Option<String>,
    saved: bool,
}

/// The submitted configuration form.
#[derive(Deserialize)]
struct ConfigForm {
    config: String,
}

/// Renders the fleet page. The configuration shown is read from disk, not from the poll cache, so the
/// editor always shows what the file actually says.
async fn show(State(state): State<UiState>, RawQuery(query): RawQuery) -> Response {
    let saved = query
        .as_deref()
        .is_some_and(|q| q.split('&').any(|p| p == "saved"));

    let (config, error) = match state.config.read() {
        Ok(body) => (String::from_utf8_lossy(&body).into_owned(), None),
        Err(e) => {
            error!(error = %e, "cannot read collector configuration for the UI");
            (
                String::new(),
                Some(format!("Cannot read the collector configuration: {e}")),
            )
        }
    };

    let want = state.config.current();
    let page = Page {
        agents: state.fleet.snapshot(want.as_ref()),
        config,
        error,
        saved,
    };
    render(page, StatusCode::OK)
}

/// Writes the submitted configuration. It distributes nothing itself — the watcher takes it from
/// here — so on success it redirects, and a reload does not resubmit the form.
async fn save(State(state): State<UiState>, Form(form): Form<ConfigForm>) -> Response {
    // An empty configuration is never what someone meant, and the collector would reject it anyway —
    // but only after the supervisor had already torn down the running one.
    if form.config.trim().is_empty() {
        return render_error(
            &state,
            "The configuration is empty. Refusing to push it to the fleet.",
        );
    }

    // Normalise the CRLF that browsers submit; the collector reads YAML, and the file is also edited
    // by hand next to it.
    let mut body = form.config.replace("\r\n", "\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }

    if let Err(e) = state.config.write(body.as_bytes()) {
        error!(error = %e, "cannot write collector configuration from the UI");
        return render_error(
            &state,
            &format!("Cannot write the collector configuration: {e}"),
        );
    }
    info!(
        bytes = body.len(),
        "collector configuration written from the UI"
    );

    Redirect::to("/?saved").into_response()
}

/// Requests a restart of one agent (ADR-0011): if it is connected, the command is fanned out to its
/// connection; either way it redirects back to the page, keeping the plain-form, no-JavaScript UX.
async fn restart(State(state): State<UiState>, Path(uid): Path<String>) -> Response {
    match hex::decode(&uid) {
        Ok(bytes) if state.fleet.is_connected(&bytes) => {
            let _ = state.pushes.send(FleetPush::Restart(bytes));
            info!(agent = %uid, "restart requested from the UI");
        }
        _ => error!(agent = %uid, "restart requested for an unknown or disconnected agent"),
    }
    Redirect::to("/").into_response()
}

/// Re-renders the page with an error banner and the current fleet, for a rejected write.
fn render_error(state: &UiState, msg: &str) -> Response {
    let config = state
        .config
        .read()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let want = state.config.current();
    let page = Page {
        agents: state.fleet.snapshot(want.as_ref()),
        config,
        error: Some(msg.to_string()),
        saved: false,
    };
    render(page, StatusCode::BAD_REQUEST)
}

/// Renders a page template to an HTML response, or a plain 500 if templating itself fails.
fn render(page: Page, code: StatusCode) -> Response {
    match page.render() {
        Ok(html) => (code, Html(html)).into_response(),
        Err(e) => {
            error!(error = %e, "cannot render the fleet page");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot render the fleet page",
            )
                .into_response()
        }
    }
}
