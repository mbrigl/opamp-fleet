//! The REST API v1 — the Server's integration contract (ADR-0005, ADR-0012) — and the bundled
//! rudimentary UI.
//!
//! The OpenAPI document is generated code-first with `utoipa`: the same annotations that register
//! a route describe it, so contract and behaviour cannot drift. Any external portal generates a
//! client from `/api/v1/openapi.json`; the UI is a client of the same routes and nothing more.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::info;
use utoipa::{IntoParams, OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::configs::{self, Configuration, ConfigurationSpec};
use crate::fleet::{AgentView, AppState, RestartError};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "OpAMP Fleet REST API",
        description = "Read fleet state; create, change, and delete Selector-targeted \
                       Configurations. The stable contract any UI or portal builds on (ADR-0012)."
    ),
    tags(
        (name = "fleet", description = "The fleet as the Server sees it"),
        (name = "configurations", description = "Selector-targeted Configurations"),
        (name = "packages", description = "Software packages the Server delivers (ADR-0015)")
    )
)]
struct ApiDoc;

pub fn router(state: Arc<AppState>) -> Router {
    let (api, document) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(agents))
        .routes(routes!(restart_agent))
        .routes(routes!(list_configurations))
        .routes(routes!(
            get_configuration,
            put_configuration,
            delete_configuration
        ))
        .routes(routes!(list_packages))
        .routes(routes!(put_package, delete_package))
        .routes(routes!(download_package))
        .split_for_parts();
    // The document is immutable once assembled — serialize it once, serve it forever.
    let document =
        serde_json::to_string_pretty(&document).expect("the OpenAPI document serializes");
    api.route(
        "/api/v1/openapi.json",
        get(move || {
            let body = (
                [(header::CONTENT_TYPE, "application/json")],
                document.clone(),
            );
            std::future::ready(body.into_response())
        }),
    )
    // The interactive API docs (ADR-0005): a Redoc page rendering /api/v1/openapi.json, with
    // Redoc vendored and served from this same origin so the docs work offline.
    .route("/api/v1/docs", get(docs))
    .route("/api/v1/docs/redoc.js", get(redoc_js))
    .route("/", get(index))
    .with_state(state)
}

/// The bundled UI: one embedded page, no frontend toolchain (ADR-0005).
async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

/// The API docs page: renders the OpenAPI document with the vendored Redoc bundle (ADR-0005).
async fn docs() -> Html<&'static str> {
    Html(include_str!("../static/docs.html"))
}

/// The vendored Redoc standalone bundle, served same-origin so the docs page needs no CDN.
async fn redoc_js() -> Response {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        include_str!("../static/redoc.standalone.js"),
    )
        .into_response()
}

/// A machine-readable error, so generated clients get a body they can show.
#[derive(Serialize, ToSchema)]
struct ErrorBody {
    error: String,
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorBody {
            error: message.into(),
        }),
    )
        .into_response()
}

/// The fleet: every Agent the Server knows, its reported attributes, and the Configurations
/// currently matching it.
#[utoipa::path(
    get,
    path = "/api/v1/agents",
    tag = "fleet",
    responses((status = 200, description = "Every known Agent", body = [AgentView]))
)]
async fn agents(State(state): State<Arc<AppState>>) -> Json<Vec<AgentView>> {
    Json(state.snapshot())
}

/// Queues a restart of the Agent's Managed Process, delivered as the protocol's restart command
/// on the Agent's next exchange — immediately over WebSocket, on the next poll over plain HTTP.
#[utoipa::path(
    post,
    path = "/api/v1/agents/{instance_uid}/restart",
    tag = "fleet",
    params(("instance_uid" = String, Path, description = "The Agent's Instance UID")),
    responses(
        (status = 202, description = "Restart queued"),
        (status = 400, description = "Malformed Instance UID", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
        (status = 409, description = "The Agent does not declare AcceptsRestartCommand", body = ErrorBody)
    )
)]
async fn restart_agent(
    State(state): State<Arc<AppState>>,
    Path(instance_uid): Path<String>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
        return error(
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
    match state.request_restart(&uid) {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(RestartError::UnknownAgent) => error(StatusCode::NOT_FOUND, format!("no agent {uid}")),
        Err(RestartError::NoCapability) => error(
            StatusCode::CONFLICT,
            format!("agent {uid} does not declare AcceptsRestartCommand"),
        ),
    }
}

/// All Configurations, in name order.
#[utoipa::path(
    get,
    path = "/api/v1/configurations",
    tag = "configurations",
    responses((status = 200, description = "Every stored Configuration", body = [Configuration]))
)]
async fn list_configurations(State(state): State<Arc<AppState>>) -> Json<Vec<Configuration>> {
    Json(state.configurations().list())
}

/// One Configuration by name.
#[utoipa::path(
    get,
    path = "/api/v1/configurations/{name}",
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name")),
    responses(
        (status = 200, description = "The Configuration", body = Configuration),
        (status = 404, description = "No Configuration of that name", body = ErrorBody)
    )
)]
async fn get_configuration(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match state.configurations().get(&name) {
        Some(config) => Json(config).into_response(),
        None => error(StatusCode::NOT_FOUND, format!("no configuration {name:?}")),
    }
}

/// Creates or replaces a Configuration. Distribution follows from state: polling Agents pick the
/// change up on their next exchange, WebSocket Agents whose attributes match are pushed
/// immediately.
#[utoipa::path(
    put,
    path = "/api/v1/configurations/{name}",
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name (ADR-0010 grammar)")),
    request_body = ConfigurationSpec,
    responses(
        (status = 200, description = "The stored Configuration", body = Configuration),
        (status = 400, description = "Invalid name or empty body", body = ErrorBody),
        (status = 500, description = "The Configuration could not be persisted", body = ErrorBody)
    )
)]
async fn put_configuration(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(spec): Json<ConfigurationSpec>,
) -> Response {
    if let Err(e) = configs::validate_name(&name) {
        return error(
            StatusCode::BAD_REQUEST,
            format!("invalid name {name:?}: {e}"),
        );
    }
    let mut body = spec.body.replace("\r\n", "\n");
    if body.trim().is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "the configuration body is empty; refusing to distribute it",
        );
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let config = Configuration {
        name,
        selector: spec.selector,
        body,
    };
    match state.put_configuration(config.clone()) {
        Ok(()) => {
            info!(configuration = %config.name, bytes = config.body.len(), "configuration stored from the API");
            Json(config).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Deletes a Configuration. Agents that applied it keep running it — narrowing never revokes
/// (ADR-0012); they simply receive no further offers from it.
#[utoipa::path(
    delete,
    path = "/api/v1/configurations/{name}",
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No Configuration of that name", body = ErrorBody),
        (status = 500, description = "The Configuration could not be deleted", body = ErrorBody)
    )
)]
async fn delete_configuration(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match state.delete_configuration(&name) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no configuration {name:?}")),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// One stored package's name and version (never its artifact bytes).
#[derive(Serialize, ToSchema)]
struct PackageView {
    name: String,
    version: String,
}

/// The query parameters of a package upload: everything but the artifact, which is the body.
#[derive(Deserialize, IntoParams)]
struct PackageUpload {
    /// The package version (free-form, e.g. a SemVer the Agent may compare).
    version: String,
    /// `true` marks an addon; the default is a top-level package (a Managed Process's binary).
    #[serde(default)]
    addon: bool,
    /// Hex-encoded Ed25519 signature over the artifact; verified by the Agent before it installs.
    #[serde(default)]
    signature: Option<String>,
}

/// All stored packages, in name order (never the artifact bytes).
#[utoipa::path(
    get,
    path = "/api/v1/packages",
    tag = "packages",
    responses(
        (status = 200, description = "Every stored package", body = [PackageView]),
        (status = 404, description = "Package delivery is not configured", body = ErrorBody)
    )
)]
async fn list_packages(State(state): State<Arc<AppState>>) -> Response {
    match state.packages() {
        Some(store) => Json(
            store
                .list()
                .into_iter()
                .map(|(name, version)| PackageView { name, version })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            "package delivery is not configured on this Server",
        ),
    }
}

/// Creates or replaces a package. The artifact is the raw request body; its metadata rides the
/// query. Distribution follows from state: matching Agents are offered it on their next exchange.
#[utoipa::path(
    put,
    path = "/api/v1/packages/{name}",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name (ADR-0010 grammar)"),
        PackageUpload
    ),
    request_body(content = Vec<u8>, description = "The artifact bytes", content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "The stored package", body = PackageView),
        (status = 400, description = "Invalid name, empty artifact, or bad signature", body = ErrorBody),
        (status = 404, description = "Package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The package could not be persisted", body = ErrorBody)
    )
)]
async fn put_package(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(upload): Query<PackageUpload>,
    body: Bytes,
) -> Response {
    if let Err(e) = configs::validate_name(&name) {
        return error(
            StatusCode::BAD_REQUEST,
            format!("invalid name {name:?}: {e}"),
        );
    }
    let signature = match upload.signature.as_deref() {
        Some(hex) => match hex::decode(hex) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    format!("invalid signature hex: {e}"),
                )
            }
        },
        None => None,
    };
    match state.put_package(
        name.clone(),
        upload.version.clone(),
        upload.addon,
        signature,
        body.to_vec(),
    ) {
        Ok(()) => {
            info!(package = %name, bytes = body.len(), "package stored from the API");
            Json(PackageView {
                name,
                version: upload.version,
            })
            .into_response()
        }
        Err(e) if e.contains("not configured") => error(StatusCode::NOT_FOUND, e),
        Err(e) if e.starts_with("invalid") || e.contains("empty") => {
            error(StatusCode::BAD_REQUEST, e)
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Deletes a package. Agents that installed it keep running it; they simply receive no further
/// offers of it.
#[utoipa::path(
    delete,
    path = "/api/v1/packages/{name}",
    tag = "packages",
    params(("name" = String, Path, description = "The package name")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such package, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The package could not be deleted", body = ErrorBody)
    )
)]
async fn delete_package(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    match state.delete_package(&name) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no package {name:?}")),
        Err(e) if e.contains("not configured") => error(StatusCode::NOT_FOUND, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Serves a package's artifact bytes — the `download_url` the Agent is offered points here. On the
/// unauthenticated REST plane (ADR-0013); the artifact's content hash and Ed25519 signature are
/// what the Agent verifies before it installs (ADR-0015).
#[utoipa::path(
    get,
    path = "/api/v1/packages/{name}/file",
    tag = "packages",
    params(("name" = String, Path, description = "The package name")),
    responses(
        (status = 200, description = "The artifact bytes", content_type = "application/octet-stream"),
        (status = 404, description = "No such package", body = ErrorBody)
    )
)]
async fn download_package(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match state.packages().and_then(|store| store.artifact(&name)) {
        Some(bytes) => {
            ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
        }
        None => error(StatusCode::NOT_FOUND, format!("no package {name:?}")),
    }
}
