//! The REST API v1 — the Server's integration contract (ADR-0009, ADR-0025) — and the bundled
//! rudimentary UI. Both belong to the Operator plane and are served on its own listener
//! (ADR-0012); the one exception, the Agent-facing artifact download, is [`download_router`].
//!
//! The OpenAPI document is generated code-first with `utoipa`: the same annotations that register
//! a route describe it, so contract and behaviour cannot drift. Any external portal generates a
//! client from `/api/v1/openapi.json`; the UI is a client of the same routes and nothing more.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use utoipa::{IntoParams, OpenApi, ToSchema};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouterExt};
use utoipa_axum::routes;

use crate::config::RestAuthConfig;
use crate::configs::{self, Configuration, ConfigurationSpec, Revision};
use crate::credentials::Credentials;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "OpAMP Fleet REST API",
        description = "Read fleet state; create, change, and delete Selector-targeted \
                       Configurations. The stable contract any UI or portal builds on (ADR-0025)."
    ),
    tags(
        (name = "fleet", description = "The fleet as the Server sees it"),
        (name = "configurations", description = "Selector-targeted Configurations"),
    )
)]
struct ApiDoc;

/// The Operator plane's credential check (ADR-0022), precomputed from `[rest.auth]`. Basic only,
/// and it guards the whole plane — the API, its document, the docs page, and the UI — because a
/// browser answers a Basic challenge by itself, which is what spares the rudimentary UI a login
/// page and a session.
pub struct OperatorAuth(Credentials);

impl OperatorAuth {
    pub fn from_config(auth: &RestAuthConfig) -> Self {
        OperatorAuth(Credentials::new(auth.accepted_headers(), auth.challenge()))
    }
}

/// Refuses every request that carries no configured credential, before any handler sees it.
async fn authenticate(
    State(auth): State<Arc<OperatorAuth>>,
    request: Request,
    next: Next,
) -> Response {
    if !auth.0.permits(request.headers()) {
        // The challenge is what turns this into a browser prompt rather than a dead end.
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, auth.0.challenge().to_string())],
            "the REST API and the UI require authentication",
        )
            .into_response();
    }
    next.run(request).await
}

pub fn router(state: Arc<AppState>, auth: Option<OperatorAuth>) -> Router {
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
        // The one route that legitimately carries a program: the framework's 2 MiB default would
        // refuse every real agent binary, so the upload streams past it and the handler bounds it
        // by `max_package_size_bytes` instead (ADR-0009). No other route is unbounded.
        .split_for_parts();
    // The document is immutable once assembled — serialize it once, serve it forever.
    let document =
        serde_json::to_string_pretty(&document).expect("the OpenAPI document serializes");
    let router = api
        .route(
            "/api/v1/openapi.json",
            get(move || {
                let body = (
                    [(header::CONTENT_TYPE, "application/json")],
                    document.clone(),
                );
                std::future::ready(body.into_response())
            }),
        )
        // The interactive API docs (ADR-0009): a Redoc page rendering /api/v1/openapi.json, with
        // Redoc vendored and served from this same origin so the docs work offline.
        .route("/api/v1/docs", get(docs))
        .route("/api/v1/docs/redoc.js", get(redoc_js))
        .route("/", get(index))
        .with_state(state);
    match auth {
        // The outermost layer, so the guard covers every route on this listener — including the
        // UI and the API docs, which are as much of the plane as `/api/v1` is (ADR-0022).
        Some(auth) => router.layer(middleware::from_fn_with_state(Arc::new(auth), authenticate)),
        None => router,
    }
}

/// The one route of `/api/v1` that is not the operator's: the artifact bytes an Agent downloads.
/// It is served on the **Agent plane** (ADR-0012), because that is the audience — the
/// `download_url` in a package offer is a path the Client resolves against its own OpAMP endpoint
/// (ADR-0028), so this listener is where the offer already points. It keeps its `/api/v1` path,
/// which every published Set's `download_url` names.
///
/// Consequently it is not in the OpenAPI document: that document describes the Operator plane.
pub fn download_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            get(download_package),
        )
        .with_state(state)
}

/// The bundled UI: one embedded page, no frontend toolchain (ADR-0009).
async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

/// The API docs page: renders the OpenAPI document with the vendored Redoc bundle (ADR-0009).
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

///
/// Fetch Metadata closes it. A browser sends `Sec-Fetch-Site` on every request and forbids page
/// scripts from setting it, so a value other than `same-origin` (the bundled UI) or `none` (a
/// user-initiated load) marks a cross-site caller, which is refused. A non-browser client — `curl`,
/// a portal — sends no such header and is unaffected, which is why this needs no token and no change
/// to any API client. It is not authentication (that is a separate decision, ADR-0022); it only
/// keeps a browser from being turned into a confused deputy.
struct SameOrigin;

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for SameOrigin {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        match parts
            .headers
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
        {
            Some(site) if site != "same-origin" && site != "none" => Err(error(
                StatusCode::FORBIDDEN,
                format!("cross-site request refused (Sec-Fetch-Site: {site})"),
            )),
            _ => Ok(SameOrigin),
        }
    }
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
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
        (status = 409, description = "The Agent does not declare AcceptsRestartCommand", body = ErrorBody)
    )
)]
async fn restart_agent(
    State(state): State<Arc<AppState>>,
    _csrf: SameOrigin,
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

#[derive(Deserialize, ToSchema)]
        (status = 404, description = "No such Agent", body = ErrorBody),
    State(state): State<Arc<AppState>>,
    Path(instance_uid): Path<String>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
        return error(
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
#[serde(deny_unknown_fields)]
    tag = "fleet",
    params(("instance_uid" = String, Path, description = "The Agent's Instance UID")),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
    State(state): State<Arc<AppState>>,
    _csrf: SameOrigin,
    Path(instance_uid): Path<String>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
        return error(
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
            StatusCode::BAD_REQUEST,
        (status = 400, description = "Malformed Instance UID", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
    State(state): State<Arc<AppState>>,
    Path(instance_uid): Path<String>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
        return error(
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
#[derive(Serialize, ToSchema)]
struct ConfigurationView {
    name: String,
    selector: std::collections::BTreeMap<String, String>,
    body: String,
    /// The Baseline's `AgentConfigObject.role` (ADR-0025); absent means top-level configuration.
    #[serde(skip_serializing_if = "String::is_empty")]
    role: String,
    /// The Agent type this Configuration is for (ADR-0025); absent means every type.
    #[serde(skip_serializing_if = "String::is_empty")]
    service_name: String,
}

impl From<Configuration> for ConfigurationView {
    fn from(config: Configuration) -> Self {
        ConfigurationView {
            name: config.name,
        }
    }
}

#[derive(Serialize, ToSchema)]
}

/// All Configurations, in name order.
#[utoipa::path(
    get,
    path = "/api/v1/configurations",
    tag = "configurations",
    responses((status = 200, description = "Every stored Configuration", body = [ConfigurationView]))
)]
async fn list_configurations(State(state): State<Arc<AppState>>) -> Json<Vec<ConfigurationView>> {
    Json(
        state
            .configurations()
            .list()
            .into_iter()
            .map(ConfigurationView::from)
            .collect(),
    )
}

/// One Configuration by name.
#[utoipa::path(
    get,
    path = "/api/v1/configurations/{name}",
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name")),
    responses(
        (status = 200, description = "The Configuration", body = ConfigurationView),
        (status = 404, description = "No Configuration of that name", body = ErrorBody)
    )
)]
async fn get_configuration(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match state.configurations().get(&name) {
        Some(config) => Json(ConfigurationView::from(config)).into_response(),
        None => error(StatusCode::NOT_FOUND, format!("no configuration {name:?}")),
    }
}

#[utoipa::path(
    put,
    path = "/api/v1/configurations/{name}",
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name (ADR-0021 grammar)")),
    request_body = ConfigurationSpec,
    responses(
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
            "the configuration body is empty; refusing to store it",
        );
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let revision = Revision {
        selector: spec.selector,
        body,
        // Carried verbatim (ADR-0025): the values are Agent-type-specific, so the Server never
        // validates one against a vocabulary of its own. Empty is top-level configuration.
        role: spec.role,
        // Compared raw against the reported `service.name` (ADR-0025); empty is every type. Not
        // validated against the fleet, because a Configuration may precede its first Agent.
        service_name: spec.service_name,
    };
    match state.save_configuration(&name, revision) {
        Ok(config) => {
            Json(ConfigurationView::from(config)).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

///
#[utoipa::path(
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name")),
    responses(
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No Configuration of that name", body = ErrorBody),
    )
)]
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
        }
    }
}

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

#[derive(Serialize, ToSchema)]
}

#[derive(Serialize, ToSchema)]
    /// The operating system, as `os.type` reports it: `linux`, `darwin`, `windows`.
    os: String,
    /// The architecture, as `host.arch` reports it: `amd64`, `arm64`.
    arch: String,
    /// The artifact's size in bytes; `0` for a referenced one, whose bytes this Server never holds.
    size: u64,
    /// Where Agents fetch the artifact when this Server does not hold it (ADR-0028). Absent for an
    /// uploaded one, which is served from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
}

                .into_iter()
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct PlatformQuery {
    /// The operating system, as `os.type`: `linux`, `darwin`, `windows`. Other spellings — `macos`
    /// off a release file name — are accepted and answered canonically.
    os: String,
    /// The architecture, as `host.arch`: `amd64`, `arm64`. Other spellings — `x86_64`, `aarch64` —
    /// are accepted and answered canonically.
    arch: String,
}

impl PlatformQuery {
    fn platform(&self) -> Result<crate::packages::Platform, String> {
        crate::packages::Platform::new(&self.os, &self.arch)
    }
}

#[derive(Deserialize, IntoParams)]
    /// Hex-encoded Ed25519 signature over the artifact; verified by the Agent before it installs.
    #[serde(default)]
    signature: Option<String>,
}

    }
}

#[utoipa::path(
    get,
    path = "/api/v1/packages",
    tag = "packages",
    responses(
        (status = 404, description = "Package delivery is not configured", body = ErrorBody)
    )
)]
async fn list_packages(State(state): State<Arc<AppState>>) -> Response {
    match state.packages() {
        None => error(
            StatusCode::NOT_FOUND,
            "package delivery is not configured on this Server",
        ),
    }
}

    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name (ADR-0021 grammar)"),
    State(state): State<Arc<AppState>>,
#[utoipa::path(
    put,
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name (ADR-0021 grammar)"),
    ),
    responses(
        (status = 404, description = "Package delivery is not configured", body = ErrorBody),
    State(state): State<Arc<AppState>>,
    responses(
        (status = 204, description = "Deleted"),
    State(state): State<Arc<AppState>>,
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
    ),
    request_body(content = Vec<u8>, description = "The artifact bytes", content_type = "application/octet-stream"),
    responses(
        (status = 413, description = "The artifact exceeds max_package_size_bytes", body = ErrorBody),
        (status = 507, description = "Storing it would exceed max_total_package_bytes", body = ErrorBody)
    )
)]
    State(state): State<Arc<AppState>>,
    body: Body,
) -> Response {
        return error(
            StatusCode::BAD_REQUEST,
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
        Ok(path) => path,
    };
    // Refuse before streaming a gibibyte we would only reject: a store already at its ceiling takes
    // nothing more. This — with the whole-store check after the stream — is what stops a caller
    // filling the disk by uploading artifact after artifact under distinct names (ADR-0028).
    let quota = state.max_total_package_bytes();
    let stored = state.stored_package_bytes();
    if stored >= quota {
        return error(
            StatusCode::INSUFFICIENT_STORAGE,
            format!("the package store is at its {quota}-byte limit (max_total_package_bytes)"),
        );
    }
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
    // Now the size is known: refuse if committing it would take the store past its ceiling. The
    // staging file is not itself an artifact yet, so `stored_package_bytes` does not count it.
    if state.stored_package_bytes() + written > quota {
        let _ = tokio::fs::remove_file(&staged).await;
        return error(
            StatusCode::INSUFFICIENT_STORAGE,
            format!(
                "storing this {written}-byte artifact would take the package store past its \
                 {quota}-byte limit (max_total_package_bytes)"
            ),
        );
    }
        Ok(()) => {
        }
    }
}

#[utoipa::path(
    delete,
    tag = "packages",
    responses(
        (status = 204, description = "Deleted"),
    )
)]
    State(state): State<Arc<AppState>>,
) -> Response {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
    /// Where the artifact lives — `http://` or `https://`. Agents fetch it from here; this Server
    /// never downloads it.
    url: String,
    /// The artifact's SHA-256, hex, as published in the release's checksums file. Required: for a
    sha256: String,
    #[serde(default)]
    signature: Option<String>,
    /// Headers the Agents send with the download — a token for a private source. Two things to know
    /// before using one: it is stored in cleartext in the package store (owner-only on disk, not
    /// narrowly-scoped, rotatable token over a long-lived credential.
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
}

///
/// The URL is probed once, to catch a typo while the operator is still looking at the screen. A
/// definitive refusal from the source (a 4xx) fails the request; a source this Server simply
/// cannot reach does not, because the Server is not in the download path and its reachability says
/// nothing about the Agents'.
#[utoipa::path(
    put,
    tag = "packages",
    responses(
        (status = 500, description = "The reference could not be persisted", body = ErrorBody)
    )
)]
    State(state): State<Arc<AppState>>,
) -> Response {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    let content_hash = match hex::decode(spec.sha256.trim()) {
        Ok(bytes) => bytes,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid sha256: {e}")),
    };
        return error(
            StatusCode::BAD_REQUEST,
    if let Err(e) = probe(&spec.url, &spec.headers).await {
        return error(StatusCode::BAD_REQUEST, e);
    }
    let source = crate::packages::Source {
        url: spec.url.clone(),
        headers: spec.headers.clone(),
    };
        Ok(()) => {
        }
    }
}

/// Asks the source whether it has the artifact. A refusal is reported; being unable to ask is not,
/// because this Server never downloads it and the Agents may well reach what it cannot.
async fn probe(
    url: &str,
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    // Refuse to aim the probe at an internal address on the caller's behalf (SSRF): the source URL
    // and its headers are entirely client-supplied, so without this a caller could read the cloud
    // metadata endpoint or map internal services by the answers this probe reflects back.
    if let Some(reason) = ssrf_blocked(url).await {
        return Err(reason);
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        // Never chase a redirect: a public URL that 3xx-bounces to `169.254.169.254` or an internal
        // host would otherwise walk the probe straight past the check above.
        .redirect(reqwest::redirect::Policy::none())
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

/// Whether probing `url` would make the Server reach a non-routable address on the caller's behalf,
/// and the reason to refuse if so. `None` clears the probe to proceed: a public host, or one this
/// Server cannot resolve (left to the probe, which treats unreachable as "not an error" — the
/// Server is not in the download path).
///
/// A resolve-then-probe still leaves a DNS-rebinding window in theory; it closes the URLs that
/// matter (literal internal IPs, the metadata address, internal hostnames) without a custom
/// resolver, which is proportionate for a probe the Server itself never downloads through.
async fn ssrf_blocked(url: &str) -> Option<String> {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        // Unparsable here is not this check's error to raise — `set_package_source` validates the
        // scheme and the store rejects a bad URL with its own message.
        return None;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return Some(format!(
            "the source url must be http:// or https://, not {}://",
            parsed.scheme()
        ));
    }
    let host = parsed.host_str()?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    let resolved = tokio::net::lookup_host((host, port)).await.ok()?;
    for addr in resolved {
        if is_internal(addr.ip()) {
            return Some(format!(
                "the source url resolves to the non-routable address {} — refusing to probe an \
                 internal endpoint",
                addr.ip()
            ));
        }
    }
    None
}

/// Whether an address is one a client-supplied URL must never steer the Server at.
///
/// The line is deliberate. This blocks the cloud-metadata address and the ranges that are never a
/// legitimate artifact source — link-local (where `169.254.169.254` lives), the shared/CGNAT range
/// (Alibaba's `100.100.100.200` among it), the unspecified address, broadcast, documentation, and
/// `0.0.0.0/8`. It does **not** block loopback or the RFC 1918 / unique-local private ranges: an
/// operator's mirror (ADR-0028) legitimately lives on an internal network, and the URL here is the
/// operator's, not a stranger's. Redirects are disabled separately, so a public URL cannot bounce
/// the probe onto a blocked address behind this check.
fn is_internal(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => is_v4_internal(v4),
        std::net::IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_v4_internal(mapped);
            }
            // link-local fe80::/10, and the unspecified address.
            v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn is_v4_internal(v4: std::net::Ipv4Addr) -> bool {
    v4.is_link_local() // 169.254.0.0/16 — the cloud metadata endpoint
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_documentation()
        // 0.0.0.0/8 "this network"
        || v4.octets()[0] == 0
        // 100.64.0.0/10 shared / carrier-grade NAT — Alibaba's metadata address among it
        || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
}

/// Serves an entry's artifact bytes — the `download_url` the Agent is offered points here.
///
/// `200` with the bytes, `400` for a missing or invalid platform or identity, `404` for a Set
/// without an uploaded artifact for that platform.
async fn download_package(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PlatformQuery>,
) -> Response {
    let platform = match query.platform() {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    let Some(path) = state
        .packages()
    else {
        return error(
            StatusCode::NOT_FOUND,
        );
    };
    // Streamed from disk, never buffered: a fleet updating at once means many concurrent
    // downloads of the same artifact, and each one holding a copy of a program in memory is how a
    // rollout takes the Server down with it.
    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
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
#[derive(Serialize, ToSchema)]
    selector: std::collections::BTreeMap<String, String>,
#[derive(Serialize, ToSchema)]
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
    selector: std::collections::BTreeMap<String, String>,
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
    State(state): State<Arc<AppState>>,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
    State(state): State<Arc<AppState>>,
    State(state): State<Arc<AppState>>,
    State(state): State<Arc<AppState>>,
#[derive(Deserialize, IntoParams)]
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
