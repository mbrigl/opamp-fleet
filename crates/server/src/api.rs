//! The REST API v1 — the Server's integration contract (ADR-0005, ADR-0012) — and the bundled
//! rudimentary UI.
//!
//! The OpenAPI document is generated code-first with `utoipa`: the same annotations that register
//! a route describe it, so contract and behaviour cannot drift. Any external portal generates a
//! client from `/api/v1/openapi.json`; the UI is a client of the same routes and nothing more.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use utoipa::{IntoParams, OpenApi, ToSchema};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouterExt};
use utoipa_axum::routes;

use crate::configs::{self, Configuration, ConfigurationSpec};
use crate::fleet::{AgentView, AppState, ForgetError, RestartError};

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
        .routes(routes!(forget_agent))
        .routes(routes!(list_configurations))
        .routes(routes!(
            get_configuration,
            put_configuration,
            delete_configuration
        ))
        .routes(routes!(list_packages))
        // The one route that legitimately carries a program: the framework's 2 MiB default would
        // refuse every real agent binary, so the upload streams past it and the handler bounds it
        // by `max_package_size_bytes` instead (ADR-0008). No other route is unbounded.
        .routes(routes!(put_package, delete_package).layer(DefaultBodyLimit::disable()))
        .routes(routes!(put_package_selector))
        .routes(routes!(put_package_type))
        .routes(routes!(rollback_package))
        .routes(routes!(put_package_source))
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

/// Forgets what the Server knows about an Agent, dropping its row from the fleet view.
///
/// Reaches no host: nothing is stopped, nothing is uninstalled, and no credential is revoked —
/// there is none per Agent to revoke. A Client that is still running reappears on its next report.
#[utoipa::path(
    delete,
    path = "/api/v1/agents/{instance_uid}",
    tag = "fleet",
    params(("instance_uid" = String, Path, description = "The Agent's Instance UID")),
    description = "Forget this Agent: the Server drops what it knows and the row leaves the fleet \
                   view (ADR-0039). Nothing happens on the host — no process is stopped, nothing \
                   is uninstalled, and no credential is revoked, because a credential here proves \
                   fleet membership rather than one Agent's identity. A Client still configured \
                   for this Server therefore comes back on its next report. Refused while the \
                   Agent is still reporting, since forgetting it would have its configuration \
                   offered again and a managed process restarted with it.",
    responses(
        (status = 204, description = "The Agent is forgotten"),
        (status = 400, description = "Malformed Instance UID", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
        (status = 409, description = "The Agent is still reporting", body = ErrorBody)
    )
)]
async fn forget_agent(
    State(state): State<Arc<AppState>>,
    Path(instance_uid): Path<String>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
        return error(
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
    match state.forget_agent(&uid) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(ForgetError::UnknownAgent) => error(StatusCode::NOT_FOUND, format!("no agent {uid}")),
        Err(ForgetError::StillReporting) => error(
            StatusCode::CONFLICT,
            format!("agent {uid} is still reporting; stop it or wait for it to go stale"),
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
        // Carried verbatim (ADR-0016): the values are Agent-type-specific, so the Server never
        // validates one against a vocabulary of its own. Empty is top-level configuration.
        role: spec.role,
    };
    match state.put_configuration(config.clone()) {
        Ok(()) => {
            info!(configuration = %config.name, role = %config.role, bytes = config.body.len(), "configuration stored from the API");
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

/// One stored package as the API shows it — never its artifact bytes.
#[derive(Serialize, ToSchema)]
struct PackageView {
    name: String,
    /// Whom this package is offered to (ADR-0017): equality pairs that must all match an
    /// attribute the Agent reported. Empty targets the whole fleet. It belongs to the package, not
    /// to one of its artifacts — every platform of a name is aimed at the same Agents (ADR-0031).
    #[serde(default)]
    selector: std::collections::BTreeMap<String, String>,
    /// The Agent type this package is built for, matched against the `service.name` an Agent
    /// reports before any Selector is considered (ADR-0034). **Empty means offered to nobody** —
    /// not "every type" — so a package is inert until this is set.
    #[serde(default)]
    service_name: String,
    /// One artifact per platform. An Agent is offered the one built for the machine it reported,
    /// and never another (ADR-0031).
    variants: Vec<PackageVariantView>,
    /// How many Agents in the fleet this package reaches as things stand — fitted by type and
    /// platform, then aimed by Selector, exactly as the offer resolves it.
    ///
    /// **`0` is the value worth looking at.** A package targets nobody when its `service_name` is
    /// unset or misspelled, when no artifact matches any reported platform, or when its Selector
    /// matches no Agent — and none of those is an upload error, so nothing else would say so. It
    /// counts the fleet *as reported so far*: a package staged for hosts that have not connected
    /// yet is legitimately at `0`, which is why this is a number to read rather than a rejection.
    targeted_agents: usize,
}

/// One platform's artifact of a package.
#[derive(Serialize, ToSchema)]
struct PackageVariantView {
    /// The operating system, as `os.type` reports it: `linux`, `darwin`, `windows`.
    os: String,
    /// The architecture, as `host.arch` reports it: `amd64`, `arm64`.
    arch: String,
    version: String,
    /// `true` for an addon, `false` for a top-level package (a Managed Process's binary).
    #[serde(default)]
    addon: bool,
    /// The artifact's size in bytes; `0` for a referenced one, whose bytes this Server never holds.
    size: u64,
    /// Where Agents fetch the artifact when this Server does not hold it (ADR-0018). Absent for an
    /// uploaded one, which is served from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    /// The version `POST /api/v1/packages/{name}/rollback?os=…&arch=…` would put back (ADR-0019).
    /// Absent when this artifact has never replaced another — in which case a rollback answers
    /// `409`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_version: Option<String>,
    /// Where that previous version is fetched from, when it is a referenced one (ADR-0018).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_source_url: Option<String>,
}

impl PackageView {
    fn of(summary: crate::packages::PackageSummary, targeted_agents: usize) -> Self {
        PackageView {
            targeted_agents,
            name: summary.name,
            selector: summary.selector,
            service_name: summary.service_name,
            variants: summary
                .variants
                .into_iter()
                .map(|variant| PackageVariantView {
                    os: variant.os,
                    arch: variant.arch,
                    version: variant.version,
                    addon: variant.addon,
                    size: variant.size,
                    source_url: variant.source_url,
                    previous_version: variant.previous_version,
                    previous_source_url: variant.previous_source_url,
                })
                .collect(),
        }
    }
}

/// The Platform an artifact route names (ADR-0031). Required wherever bytes are written or served,
/// because a package name alone no longer names one file.
#[derive(Deserialize, IntoParams)]
struct PlatformQuery {
    /// The operating system, as `os.type`: `linux`, `darwin`, `windows`. Other spellings — `macos`
    /// off a release file name — are accepted and answered canonically.
    os: String,
    /// The architecture, as `host.arch`: `amd64`, `arm64`. Other spellings — `x86_64`, `aarch64` —
    /// are accepted and answered canonically.
    arch: String,
}

/// The Platform on a route where naming none means "the whole package".
#[derive(Deserialize, IntoParams)]
struct OptionalPlatformQuery {
    os: Option<String>,
    arch: Option<String>,
}

impl PlatformQuery {
    fn platform(&self) -> Result<crate::packages::Platform, String> {
        crate::packages::Platform::new(&self.os, &self.arch)
    }
}

/// The writable Selector of a package — the body of `PUT /api/v1/packages/{name}/selector`.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct PackageSelectorSpec {
    /// Equality pairs an Agent's reported attributes must all match; empty targets every Agent.
    #[serde(default)]
    selector: std::collections::BTreeMap<String, String>,
}

/// The Agent type a package is built for (ADR-0034).
#[derive(Deserialize, ToSchema)]
struct PackageTypeSpec {
    /// Matched for equality against the `service.name` an Agent reports — `otelcol-contrib`,
    /// `opamp-fleet-client`. Compared raw: there is no canonical set of Agent types to normalise
    /// against, so this must be spelled exactly as the Agent reports it.
    service_name: String,
}

/// The query parameters of a package upload: everything but the artifact, which is the body.
#[derive(Deserialize, IntoParams)]
struct PackageUpload {
    /// The package version (free-form, e.g. a SemVer the Agent may compare).
    version: String,
    /// The operating system this artifact is built for, as `os.type`: `linux`, `darwin`,
    /// `windows`. **Required** — an artifact the Server cannot fit to a machine is one it will not
    /// offer (ADR-0031).
    os: String,
    /// The architecture this artifact is built for, as `host.arch`: `amd64`, `arm64`. **Required**.
    arch: String,
    /// `true` marks an addon; the default is a top-level package (a Managed Process's binary).
    #[serde(default)]
    addon: bool,
    /// Hex-encoded Ed25519 signature over the artifact; verified by the Agent before it installs.
    #[serde(default)]
    signature: Option<String>,
}

/// A stored package as the API answers with it, read back from the store rather than assembled
/// from whatever the handler happened to be given — so every response describes the package as it
/// now is, including the version a rollback would restore.
fn package_response(state: &AppState, name: &str) -> Response {
    match state.packages().and_then(|store| store.summary(name)) {
        Some(summary) => {
            let reach = state.package_reach().get(name).copied().unwrap_or(0);
            Json(PackageView::of(summary, reach)).into_response()
        }
        None => error(StatusCode::NOT_FOUND, format!("no package {name:?}")),
    }
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
        Some(store) => {
            let summaries = store.list();
            // One pass over the fleet for the whole list, rather than one per package.
            let reach = state.package_reach();
            Json(
                summaries
                    .into_iter()
                    .map(|summary| {
                        let targeted = reach.get(&summary.name).copied().unwrap_or(0);
                        PackageView::of(summary, targeted)
                    })
                    .collect::<Vec<_>>(),
            )
            .into_response()
        }
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
    body: Body,
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
    let platform = match crate::packages::Platform::new(&upload.os, &upload.arch) {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    let staged = match state.package_staging_path(&name, &platform) {
        Ok(path) => path,
        Err(e) => return error(StatusCode::NOT_FOUND, e),
    };
    // The artifact is streamed to the store's own directory and bounded as it arrives: taking it
    // as `Bytes` would mean holding a whole program in memory — twice — before writing it out.
    let written = match stream_to_file(body, &staged, state.max_package_size()).await {
        Ok(written) => written,
        Err(UploadError::TooLarge(limit)) => {
            let _ = tokio::fs::remove_file(&staged).await;
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("the artifact exceeds the {limit}-byte package size limit"),
            );
        }
        Err(UploadError::Io(e)) => {
            let _ = tokio::fs::remove_file(&staged).await;
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the upload could not be stored: {e}"),
            );
        }
    };
    match state.put_package(
        name.clone(),
        platform,
        upload.version.clone(),
        upload.addon,
        signature,
        &staged,
    ) {
        Ok(()) => {
            info!(package = %name, bytes = written, "package stored from the API");
            package_response(&state, &name)
        }
        Err(e) if e.contains("not configured") => error(StatusCode::NOT_FOUND, e),
        Err(e) if e.starts_with("invalid") || e.contains("empty") => {
            error(StatusCode::BAD_REQUEST, e)
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Puts a package back to the version it replaced (ADR-0019) — the undo for a rollout that
/// installed cleanly and then behaved badly, which the Agent-side rollback (ADR-0015) does not
/// cover because that one only catches a binary that will not start.
///
/// Exactly one step is remembered, so the version rolled back *from* becomes the next one to go
/// back to and pressing this twice returns to where it started. The Selector is untouched: which
/// Agents a package reaches is a separate decision from which bytes they get. Distribution follows
/// from state, like every package change — matching Agents are offered the restored version on
/// their next exchange, and one that is offline stays on the new version until it returns.
#[utoipa::path(
    post,
    path = "/api/v1/packages/{name}/rollback",
    tag = "packages",
    params(("name" = String, Path, description = "The package name"), PlatformQuery),
    responses(
        (status = 200, description = "The package, now back at its previous version", body = PackageView),
        (status = 400, description = "Missing or invalid platform", body = ErrorBody),
        (status = 404, description = "No such package or platform, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "That platform's artifact has no previous version to go back to", body = ErrorBody),
        (status = 500, description = "The rollback could not be persisted", body = ErrorBody)
    )
)]
async fn rollback_package(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<PlatformQuery>,
) -> Response {
    let platform = match query.platform() {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    match state.rollback_package(&name, &platform) {
        Ok(()) => {
            info!(package = %name, "package rolled back to its previous version");
            package_response(&state, &name)
        }
        Err(e) if e.contains("not configured") => error(StatusCode::NOT_FOUND, e),
        Err(e) if e.contains("holds no artifact") => error(StatusCode::NOT_FOUND, e),
        // A package at its first upload has nothing to go back to; the API says so rather than
        // silently doing nothing.
        Err(e) if e.contains("no previous version") => error(StatusCode::CONFLICT, e),
        Err(e) if e.starts_with("no package") => error(StatusCode::NOT_FOUND, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Deletes a package, or — when a platform is named — only that platform's artifact. Agents that
/// installed it keep running it; they simply receive no further offers of it. Taking the last
/// artifact away takes the package with it: a name with nothing to offer is not a package.
#[utoipa::path(
    delete,
    path = "/api/v1/packages/{name}",
    tag = "packages",
    params(("name" = String, Path, description = "The package name"), OptionalPlatformQuery),
    responses(
        (status = 204, description = "Deleted"),
        (status = 400, description = "A platform was half-named — `os` without `arch`, or the reverse", body = ErrorBody),
        (status = 404, description = "No such package or platform, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The package could not be deleted", body = ErrorBody)
    )
)]
async fn delete_package(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<OptionalPlatformQuery>,
) -> Response {
    let deleted = match (&query.os, &query.arch) {
        (None, None) => state.delete_package(&name),
        (Some(os), Some(arch)) => match crate::packages::Platform::new(os, arch) {
            Ok(platform) => state.delete_package_variant(&name, &platform),
            Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
        },
        // Half a platform is not a narrower delete, it is an ambiguous one: the whole package or
        // one artifact of it are very different things to lose.
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "name both `os` and `arch` to delete one platform's artifact, or neither to \
                 delete the whole package",
            )
        }
    };
    match deleted {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no package {name:?}")),
        Err(e) if e.contains("not configured") => error(StatusCode::NOT_FOUND, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// The body of `PUT /api/v1/packages/{name}/source` (ADR-0018).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct PackageSourceSpec {
    /// Where the artifact lives — `http://` or `https://`. Agents fetch it from here; this Server
    /// never downloads it.
    url: String,
    /// The artifact's SHA-256, hex, as published in the release's checksums file. Required: for a
    /// referenced package nothing here ever sees the bytes, so this is what protects every Agent.
    sha256: String,
    /// The version Agents report having installed.
    version: String,
    /// The operating system this artifact is built for, as `os.type`: `linux`, `darwin`,
    /// `windows`. **Required** (ADR-0031).
    os: String,
    /// The architecture this artifact is built for, as `host.arch`: `amd64`, `arm64`. **Required**.
    arch: String,
    /// `true` marks an addon; the default is a top-level package.
    #[serde(default)]
    addon: bool,
    /// Hex Ed25519 signature over the artifact, checked by the Agent against its configured key.
    #[serde(default)]
    signature: Option<String>,
    /// Headers the Agents send with the download — a token for a private source. Every targeted
    /// Agent receives them.
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
}

/// Points a package at an artifact hosted elsewhere (ADR-0018), instead of uploading it. The
/// Server stores the reference and offers it verbatim; it never downloads the artifact, so the
/// `sha256` — and the signature, when one is configured — is what protects every Agent.
///
/// The URL is probed once, to catch a typo while the operator is still looking at the screen. A
/// definitive refusal from the source (a 4xx) fails the request; a source this Server simply
/// cannot reach does not, because the Server is not in the download path and its reachability says
/// nothing about the Agents'.
#[utoipa::path(
    put,
    path = "/api/v1/packages/{name}/source",
    tag = "packages",
    params(("name" = String, Path, description = "The package name (ADR-0010 grammar)")),
    request_body = PackageSourceSpec,
    responses(
        (status = 200, description = "The package, now referenced", body = PackageView),
        (status = 400, description = "Invalid name, url, hash or signature — or the source refused the probe", body = ErrorBody),
        (status = 404, description = "Package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The reference could not be persisted", body = ErrorBody)
    )
)]
async fn put_package_source(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(spec): Json<PackageSourceSpec>,
) -> Response {
    if let Err(e) = configs::validate_name(&name) {
        return error(
            StatusCode::BAD_REQUEST,
            format!("invalid name {name:?}: {e}"),
        );
    }
    let platform = match crate::packages::Platform::new(&spec.os, &spec.arch) {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    let content_hash = match hex::decode(spec.sha256.trim()) {
        Ok(bytes) => bytes,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid sha256: {e}")),
    };
    let signature = match spec.signature.as_deref() {
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
    if let Err(e) = probe(&spec.url, &spec.headers).await {
        return error(StatusCode::BAD_REQUEST, e);
    }
    let source = crate::packages::Source {
        url: spec.url.clone(),
        headers: spec.headers.clone(),
    };
    match state.set_package_source(
        &name,
        &platform,
        &spec.version,
        spec.addon,
        content_hash,
        signature,
        source,
    ) {
        Ok(()) => {
            info!(package = %name, url = %spec.url, "package source stored from the API");
            package_response(&state, &name)
        }
        Err(e) if e.contains("not configured") => error(StatusCode::NOT_FOUND, e),
        Err(e) if e.starts_with("invalid") || e.contains("must ") => {
            error(StatusCode::BAD_REQUEST, e)
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Asks the source whether it has the artifact. A refusal is reported; being unable to ask is not,
/// because this Server never downloads it and the Agents may well reach what it cannot.
async fn probe(
    url: &str,
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            warn!(error = %e, "cannot build the probe client; storing the source unprobed");
            return Ok(());
        }
    };
    let mut request = client.head(url);
    for (key, value) in headers {
        request = request.header(key, value);
    }
    match request.send().await {
        Ok(response) if response.status().is_client_error() => Err(format!(
            "the source answered {} — check the url{}",
            response.status(),
            if response.status() == reqwest::StatusCode::UNAUTHORIZED
                || response.status() == reqwest::StatusCode::FORBIDDEN
            {
                " and whether it needs headers"
            } else {
                ""
            }
        )),
        Ok(_) => Ok(()),
        Err(e) => {
            // Not an error: a fleet may reach an address its Server cannot.
            warn!(url = %url, error = %e, "cannot reach the source from here; storing it anyway");
            Ok(())
        }
    }
}

/// Sets which Agents a package is offered to (ADR-0017). An empty Selector targets the whole
/// fleet; every pair must equal an attribute the Agent reported, exactly as for a Configuration.
///
/// Where several top-level packages match one Agent, the most specific Selector wins — so a
/// fleet-wide package plus a narrower one is how a rollout starts on part of the fleet. Two
/// equally specific Selectors reaching the same Agent leave it with no offer, and the fleet view
/// says so on that Agent (`package_conflict`).
#[utoipa::path(
    put,
    path = "/api/v1/packages/{name}/selector",
    tag = "packages",
    params(("name" = String, Path, description = "The package name")),
    request_body = PackageSelectorSpec,
    responses(
        (status = 200, description = "The package, with its Selector", body = PackageView),
        (status = 404, description = "No such package, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The Selector could not be persisted", body = ErrorBody)
    )
)]
async fn put_package_selector(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(spec): Json<PackageSelectorSpec>,
) -> Response {
    match state.set_package_selector(&name, spec.selector.clone()) {
        Ok(_) => {
            info!(package = %name, pairs = spec.selector.len(), "package selector set");
            package_response(&state, &name)
        }
        Err(e) if e.contains("not configured") || e.starts_with("no package") => {
            error(StatusCode::NOT_FOUND, e)
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Sets the Agent type a package is built for (ADR-0034) — for every platform of it at once,
/// because a type is platform-independent, exactly as the Selector is aim-independent of bytes.
///
/// **This is what arms a package.** Until a type is set the package is offered to no Agent, so an
/// artifact uploaded and then forgotten reaches nobody rather than everybody. The value is compared
/// raw against the `service.name` an Agent reports, with no normalisation — there is no canonical
/// set of Agent types — so a typo here is a rollout that never starts, not an error.
#[utoipa::path(
    put,
    path = "/api/v1/packages/{name}/type",
    tag = "packages",
    params(("name" = String, Path, description = "The package name")),
    request_body = PackageTypeSpec,
    responses(
        (status = 200, description = "The package, with its Agent type", body = PackageView),
        (status = 400, description = "The agent type is empty", body = ErrorBody),
        (status = 404, description = "No such package, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The agent type could not be persisted", body = ErrorBody)
    )
)]
async fn put_package_type(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(spec): Json<PackageTypeSpec>,
) -> Response {
    match state.set_package_service_name(&name, spec.service_name.clone()) {
        Ok(_) => {
            info!(package = %name, service_name = %spec.service_name, "package agent type set");
            package_response(&state, &name)
        }
        Err(e) if e.contains("not configured") || e.starts_with("no package") => {
            error(StatusCode::NOT_FOUND, e)
        }
        Err(e) if e.starts_with("an agent type") => error(StatusCode::BAD_REQUEST, e),
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
    params(("name" = String, Path, description = "The package name"), PlatformQuery),
    responses(
        (status = 200, description = "The artifact bytes", content_type = "application/octet-stream"),
        (status = 400, description = "Missing or invalid platform", body = ErrorBody),
        (status = 404, description = "No such package, or none for that platform", body = ErrorBody)
    )
)]
async fn download_package(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<PlatformQuery>,
) -> Response {
    let platform = match query.platform() {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    let Some(path) = state
        .packages()
        .and_then(|store| store.artifact_path(&name, &platform))
    else {
        return error(
            StatusCode::NOT_FOUND,
            format!("no package {name:?} for {}-{}", platform.os, platform.arch),
        );
    };
    // Streamed from disk, never buffered: a fleet updating at once means many concurrent
    // downloads of the same artifact, and each one holding a copy of a program in memory is how a
    // rollout takes the Server down with it.
    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(e) => {
            warn!(package = %name, error = %e, "cannot open the stored artifact");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the artifact of {name:?} cannot be read"),
            );
        }
    };
    let mut response = Response::builder().header(header::CONTENT_TYPE, "application/octet-stream");
    if let Ok(metadata) = file.metadata().await {
        // So the Agent can size the download, and a truncated transfer is detectable as one.
        response = response.header(header::CONTENT_LENGTH, metadata.len());
    }
    response
        .body(Body::from_stream(read_chunks(file)))
        .unwrap_or_else(|e| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot serve the artifact of {name:?}: {e}"),
            )
        })
}

/// Why a streamed upload did not become a file.
enum UploadError {
    /// The body grew past the configured package limit; the number is that limit.
    TooLarge(usize),
    Io(std::io::Error),
}

/// Streams a request body into `path`, refusing it the moment it grows past `limit`. Returns how
/// many bytes were written.
async fn stream_to_file(
    body: Body,
    path: &std::path::Path,
    limit: usize,
) -> Result<u64, UploadError> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(UploadError::Io)?;
    let mut stream = body.into_data_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| UploadError::Io(std::io::Error::other(e)))?;
        written += chunk.len() as u64;
        if written > limit as u64 {
            return Err(UploadError::TooLarge(limit));
        }
        file.write_all(&chunk).await.map_err(UploadError::Io)?;
    }
    file.flush().await.map_err(UploadError::Io)?;
    Ok(written)
}

/// A stream of the file's chunks, so a response body never materialises whole.
fn read_chunks(
    file: tokio::fs::File,
) -> impl futures_util::Stream<Item = std::io::Result<Vec<u8>>> {
    futures_util::stream::unfold(file, |mut file| async move {
        let mut buffer = vec![0u8; 64 * 1024];
        match tokio::io::AsyncReadExt::read(&mut file, &mut buffer).await {
            Ok(0) => None,
            Ok(read) => {
                buffer.truncate(read);
                Some((Ok(buffer), file))
            }
            Err(e) => Some((Err(e), file)),
        }
    })
}
