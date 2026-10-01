//! The REST API v1 — the Server's integration contract (ADR-0005, ADR-0012) — and the bundled
//! rudimentary UI. Both belong to the Operator plane and are served on its own listener
//! (ADR-0066); the one exception, the Agent-facing artifact download, is [`download_router`].
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
use crate::fleet::{AgentView, AppState, ForgetError, RestartError, RolloutError, RolloutTarget};
use crate::labels::LabelError;

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
        (name = "packages", description = "Software packages the Server delivers (ADR-0015)"),
        (name = "deployments", description = "What reaches a channel of hosts — the only thing \
                                              rolled out (ADR-0096)")
    )
)]
struct ApiDoc;

/// The Operator plane's credential check (ADR-0067), precomputed from `[rest.auth]`. Basic only,
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
        .routes(routes!(forget_agent))
        .routes(routes!(set_agent_labels))
        .routes(routes!(rollout_to_agent))
        .routes(routes!(list_configurations))
        .routes(routes!(
            get_configuration,
            put_configuration,
            delete_configuration
        ))
        .routes(routes!(rollout_configuration))
        .routes(routes!(list_packages))
        .routes(routes!(
            get_package_set,
            put_package_set,
            delete_package_set
        ))
        // The one route that legitimately carries a program: the framework's 2 MiB default would
        // refuse every real agent binary, so the upload streams past it and the handler bounds it
        // by `max_package_size_bytes` instead (ADR-0008). No other route is unbounded.
        .routes(routes!(put_package_entry, delete_package_entry).layer(DefaultBodyLimit::disable()))
        .routes(routes!(put_package_entry_source))
        .routes(routes!(list_deployments))
        .routes(routes!(get_deployment, put_deployment, delete_deployment))
        .routes(routes!(put_deployment_selector))
        .routes(routes!(put_deployment_package, delete_deployment_package))
        .routes(routes!(
            put_deployment_signature,
            delete_deployment_signature
        ))
        .routes(routes!(rollout_deployment))
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
        // The interactive API docs (ADR-0005): a Redoc page rendering /api/v1/openapi.json, with
        // Redoc vendored and served from this same origin so the docs work offline.
        .route("/api/v1/docs", get(docs))
        .route("/api/v1/docs/redoc.js", get(redoc_js))
        .route("/", get(index))
        .with_state(state);
    match auth {
        // The outermost layer, so the guard covers every route on this listener — including the
        // UI and the API docs, which are as much of the plane as `/api/v1` is (ADR-0067).
        Some(auth) => router.layer(middleware::from_fn_with_state(Arc::new(auth), authenticate)),
        None => router,
    }
}

/// The one route of `/api/v1` that is not the operator's: the artifact bytes an Agent downloads.
/// It is served on the **Agent plane** (ADR-0066), because that is the audience — the
/// `download_url` in a package offer is a path the Client resolves against its own OpAMP endpoint
/// (ADR-0015), so this listener is where the offer already points. It keeps its `/api/v1` path,
/// which every published Set's `download_url` names.
///
/// Consequently it is not in the OpenAPI document: that document describes the Operator plane.
pub fn download_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/api/v1/packages/{agent_type}/{version}/file",
            get(download_package),
        )
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

/// A CSRF guard for the body-less `POST` routes (`restart`, the rollout acts). Those are CORS
/// "simple requests": a cross-origin page can fire them without the preflight the Server would
/// have to answer, so nothing else stops a victim operator's browser from being made to send one.
///
/// Fetch Metadata closes it. A browser sends `Sec-Fetch-Site` on every request and forbids page
/// scripts from setting it, so a value other than `same-origin` (the bundled UI) or `none` (a
/// user-initiated load) marks a cross-site caller, which is refused. A non-browser client — `curl`,
/// a portal — sends no such header and is unaffected, which is why this needs no token and no change
/// to any API client. It is not authentication (that is a separate decision, ADR-0013); it only
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

/// The labels to put on an Agent (ADR-0042). The whole set, replacing what was there.
#[derive(Deserialize, ToSchema)]
struct LabelsBody {
    /// Equality pairs a Selector can match, exactly like a reported attribute — `rollout: canary`
    /// being the one this exists for. An empty map clears the Agent's labels.
    #[serde(default)]
    labels: std::collections::BTreeMap<String, String>,
}

/// Sets an Agent's labels, which decide what Selectors match it.
#[utoipa::path(
    put,
    path = "/api/v1/agents/{instance_uid}/labels",
    tag = "fleet",
    params(("instance_uid" = String, Path, description = "The Agent's Instance UID")),
    request_body = LabelsBody,
    description = "Replace this Agent's labels (ADR-0042). A label is an operator's key/value pair \
                   that joins what a Selector matches — for Configurations and for packages alike — \
                   so a rollout channel is a Server-side decision instead of an edit to supervisor.toml on \
                   the host. An empty map clears them. Labels never travel to the Agent, and they \
                   outlive it: forgetting an Agent does not clear them. A key the Agent already \
                   reports is refused, because reported attributes decide which artifact fits the \
                   machine and a label must never be able to overrule them.",
    responses(
        (status = 200, description = "The Agent, with its new labels", body = AgentView),
        (status = 400, description = "Malformed Instance UID, or an unusable label", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
        (status = 409, description = "A label restates an attribute the Agent reports", body = ErrorBody)
    )
)]
async fn set_agent_labels(
    State(state): State<Arc<AppState>>,
    Path(instance_uid): Path<String>,
    Json(body): Json<LabelsBody>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
        return error(
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
    match state.set_labels(&uid, body.labels) {
        Ok(()) => match state.snapshot().into_iter().find(|a| a.instance_uid == uid.to_string()) {
            Some(view) => Json(view).into_response(),
            // Forgotten between the write and the read: the labels are stored, and the Agent is
            // simply no longer in the view to return.
            None => StatusCode::NO_CONTENT.into_response(),
        },
        Err(LabelError::UnknownAgent) => error(StatusCode::NOT_FOUND, format!("no agent {uid}")),
        Err(LabelError::RestatesReported(key)) => error(
            StatusCode::CONFLICT,
            format!(
                "agent {uid} reports {key:?} itself, and a label may not restate it — a reported \
                 attribute decides which artifact fits this machine, so it wins. Change it where it \
                 comes from, in that host's supervisor.toml, or label it under another key"
            ),
        ),
        Err(LabelError::Storage(e)) => error(StatusCode::BAD_REQUEST, e),
    }
}

/// What a per-Agent rollout act releases (ADR-0061). Name at most one of the two; an empty body
/// releases everything currently waiting for the Agent.
#[derive(Deserialize, ToSchema, Default)]
#[serde(deny_unknown_fields)]
struct AgentRolloutSpec {
    /// Release this Configuration — its saved revision, pinned as of this press.
    #[serde(default)]
    configuration: Option<String>,
    /// Release this Agent's Deployment — the Package it holds for the Agent's type, pinned as of
    /// this press.
    ///
    /// It must be the Deployment that actually claims this Agent. Naming another is refused, and
    /// so is naming one while a second Deployment also claims the Agent: an operator who names one
    /// has said which they mean, but honouring that would sidestep the conflict for good instead
    /// of fixing it, and make this path the way into a state the fleet-wide act forbids
    /// (ADR-0096 point 9).
    #[serde(default)]
    deployment: Option<String>,
}

/// Rolls a Configuration or this Agent's Deployment out to **this Agent** (ADR-0061) — or, with an empty
/// body, everything the fleet view shows as waiting for it. The operator's press is the only
/// thing that distributes: saving, publishing-like states, Selector edits and label moves all
/// merely change what is *proposed* here.
#[utoipa::path(
    post,
    path = "/api/v1/agents/{instance_uid}/rollout",
    tag = "fleet",
    params(("instance_uid" = String, Path, description = "The Agent's Instance UID")),
    request_body(content = AgentRolloutSpec, description = "What to release; empty releases everything waiting"),
    responses(
        (status = 200, description = "Rolled out; the Agent with its new assignments", body = AgentView),
        (status = 400, description = "Malformed Instance UID or body", body = ErrorBody),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No such Agent, Configuration, or Deployment", body = ErrorBody),
        (status = 409, description = "The named resource does not fit or aim at this Agent", body = ErrorBody)
    )
)]
async fn rollout_to_agent(
    State(state): State<Arc<AppState>>,
    _csrf: SameOrigin,
    Path(instance_uid): Path<String>,
    body: Option<Json<AgentRolloutSpec>>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
        return error(
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
    let spec = body.map(|Json(spec)| spec).unwrap_or_default();
    let target = match (spec.configuration, spec.deployment) {
        (Some(_), Some(_)) => return error(
            StatusCode::BAD_REQUEST,
            "name a configuration or a deployment, not both — or neither for everything waiting",
        ),
        (Some(name), None) => RolloutTarget::Configuration(name),
        (None, Some(deployment)) => RolloutTarget::Deployment(deployment),
        (None, None) => RolloutTarget::Everything,
    };
    match state.rollout_to_agent(&uid, &target) {
        Ok(()) => match state
            .snapshot()
            .into_iter()
            .find(|a| a.instance_uid == uid.to_string())
        {
            Some(view) => Json(view).into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        },
        Err(e) => rollout_error(e),
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

/// One Configuration as the API shows it (ADR-0061): the **saved** revision — what editing
/// operates on and what a rollout act releases. Which Agents run which pinned revision is a fact
/// about the Agents, answered per Agent by `GET /api/v1/agents`.
#[derive(Serialize, ToSchema)]
struct ConfigurationView {
    name: String,
    selector: std::collections::BTreeMap<String, String>,
    body: String,
    /// The Baseline's `AgentConfigObject.role` (ADR-0016); absent means top-level configuration.
    #[serde(skip_serializing_if = "String::is_empty")]
    role: String,
    /// The Agent type this Configuration is for (ADR-0054); absent means every type.
    #[serde(skip_serializing_if = "String::is_empty")]
    service_name: String,
}

impl From<Configuration> for ConfigurationView {
    fn from(config: Configuration) -> Self {
        ConfigurationView {
            name: config.name,
            selector: config.saved.selector,
            body: config.saved.body,
            role: config.saved.role,
            service_name: config.saved.service_name,
        }
    }
}

/// What a resource-level rollout act did (ADR-0061 point 5).
#[derive(Serialize, ToSchema)]
struct RolloutOutcome {
    /// How many Agents the act assigned the resource to — every Agent it currently fits and
    /// aims at. An Agent that appears later waits for its own act.
    assigned_agents: usize,
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

/// Creates a Configuration or replaces its saved revision. **Saving only saves** (ADR-0061):
/// nothing reaches any Agent — every Agent keeps the revision its assignment pins — until a
/// rollout act (`POST …/rollout`, or per Agent) releases the saved revision as one snapshot.
#[utoipa::path(
    put,
    path = "/api/v1/configurations/{name}",
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name (ADR-0010 grammar)")),
    request_body = ConfigurationSpec,
    responses(
        (status = 200, description = "The stored Configuration — distributed to nobody until rolled out", body = ConfigurationView),
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
        // Carried verbatim (ADR-0016): the values are Agent-type-specific, so the Server never
        // validates one against a vocabulary of its own. Empty is top-level configuration.
        role: spec.role,
        // Compared raw against the reported `service.name` (ADR-0054); empty is every type. Not
        // validated against the fleet, because a Configuration may precede its first Agent.
        service_name: spec.service_name,
    };
    match state.save_configuration(&name, revision) {
        Ok(config) => {
            info!(configuration = %config.name, role = %config.saved.role, service_name = %config.saved.service_name, bytes = config.saved.body.len(), "configuration saved from the API");
            Json(ConfigurationView::from(config)).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Maps a rollout refusal onto the REST contract (ADR-0061).
fn rollout_error(e: RolloutError) -> Response {
    match e {
        RolloutError::UnknownAgent => error(StatusCode::NOT_FOUND, "no such agent"),
        RolloutError::UnknownResource(e) => error(StatusCode::NOT_FOUND, e),
        RolloutError::NotApplicable(e) => error(StatusCode::CONFLICT, e),
        RolloutError::Storage(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Rolls a Configuration out to **every Agent it currently fits and aims at** (ADR-0061).
///
/// **This is the moment the fleet changes.** The saved revision is pinned as one snapshot and
/// written into each matching Agent's assignment; a later edit changes nothing anywhere until
/// the next rollout act. An Agent that enrols — or starts matching — later is *not* included: it
/// surfaces in the fleet view as waiting, for its own act.
#[utoipa::path(
    post,
    path = "/api/v1/configurations/{name}/rollout",
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name")),
    responses(
        (status = 200, description = "Rolled out; how many Agents were assigned", body = RolloutOutcome),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No Configuration of that name", body = ErrorBody),
        (status = 500, description = "The rollout could not be persisted", body = ErrorBody)
    )
)]
async fn rollout_configuration(
    State(state): State<Arc<AppState>>,
    _csrf: SameOrigin,
    Path(name): Path<String>,
) -> Response {
    match state.rollout_configuration(&name) {
        Ok(assigned_agents) => {
            info!(configuration = %name, agents = assigned_agents, "configuration rolled out from the API");
            Json(RolloutOutcome { assigned_agents }).into_response()
        }
        Err(e) => rollout_error(e),
    }
}

/// Deletes a Configuration and removes every per-Agent assignment that referenced it
/// (ADR-0061). That is **not inert** for an Agent that had it assigned: its composed map
/// shrinks, and it applies the map without the entry; only an Agent left assigned nothing keeps
/// running what it runs.
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

/// One stored package **Set** as the API shows it (ADR-0052) — never its artifact bytes.
///
/// A Set is identified by *(name, agent type, version)*, stated at creation and never edited: a
/// new version is a new Set. It may define a Selector and holds one entry per platform. **Saving
/// never distributes anything** (ADR-0061): a Set reaches an Agent only through a rollout act,
/// and which Agents run it is answered per Agent by `GET /api/v1/agents`.
#[derive(Serialize, ToSchema)]
struct PackageSetView {
    /// The Agent type this Package is built for, matched raw against the `service.name` an Agent
    /// reports before anything else is considered (ADR-0034). Half its identity — and the name it
    /// carries on the wire, which is why it never holds the version.
    agent_type: String,
    /// The version every entry of this Package shares. The other half of its identity.
    version: String,
    /// What an operator reads: the Agent type and the version together. Derived, never stored.
    display_name: String,
    /// One entry per platform. An Agent is offered the one built for the machine it reported, and
    /// never another (ADR-0031).
    entries: Vec<PackageEntryView>,
    /// The Deployments that hold this Package, in name order.
    ///
    /// This is where "whom does it reach" is answered now, and it is a different question than it
    /// used to be: a Package aims at nobody by itself (ADR-0095), so an empty list means it is
    /// stored and unreachable — the state that used to be a Selector matching no one.
    deployments: Vec<String>,
}

/// One platform's entry of a Set: an uploaded artifact or a source reference (ADR-0018).
#[derive(Serialize, ToSchema)]
struct PackageEntryView {
    /// The operating system, as `os.type` reports it: `linux`, `darwin`, `windows`.
    os: String,
    /// The architecture, as `host.arch` reports it: `amd64`, `arm64`.
    arch: String,
    /// The artifact's size in bytes; `0` for a referenced one, whose bytes this Server never holds.
    size: u64,
    /// The artifact's SHA-256, hex — **the exact value the Agent verifies what it downloaded
    /// against** (ADR-0095). Reading it is how an operator answers "did this host take my bytes"
    /// without trusting a status field: it is the same string the Agent reports back in its
    /// `PackageStatuses`, and the same one `opamp-package-sign sha256` prints locally.
    content_hash: String,
    /// The per-package hash this entry is offered under, hex. An Agent echoes it once it is in
    /// sync, and the Server stops re-offering while the two agree — so a rollout that seems not to
    /// travel is answered here rather than by reading logs.
    package_hash: String,
    /// Where Agents fetch the artifact when this Server does not hold it (ADR-0018). Absent for an
    /// uploaded one, which is served from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
}

impl PackageSetView {
    fn of(summary: crate::packages::PackageSummary, deployments: Vec<String>) -> Self {
        PackageSetView {
            display_name: format!("{} {}", summary.agent_type, summary.version),
            agent_type: summary.agent_type,
            version: summary.version,
            deployments,
            entries: summary
                .entries
                .into_iter()
                .map(|entry| PackageEntryView {
                    os: entry.os,
                    arch: entry.arch,
                    size: entry.size,
                    content_hash: entry.content_hash,
                    package_hash: entry.package_hash,
                    source_url: entry.source_url,
                })
                .collect(),
        }
    }
}

/// The Platform the download route names (ADR-0031): the artifact endpoint serves bytes, and a
/// request naming bytes names the Platform they are for.
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

/// The query parameters of an entry upload: everything but the artifact, which is the body — and
/// the platform, which is the path.
#[derive(Deserialize, IntoParams)]
struct EntryUpload {
    /// Hex-encoded Ed25519 signature over the artifact; verified by the Agent before it installs.
    #[serde(default)]
    signature: Option<String>,
}

/// The identity triple as every Set route carries it in its path.
fn package_id(agent_type: &str, version: &str) -> Result<crate::packages::PackageId, String> {
    crate::packages::PackageId::new(agent_type, version)
}

/// A stored Set as the API answers with it, read back from the store rather than assembled from
/// whatever the handler happened to be given — so every response describes the Set as it now is.
fn set_response(state: &AppState, id: &crate::packages::PackageId) -> Response {
    match state.packages().and_then(|store| store.summary(id)) {
        Some(summary) => {
            let deployments = state
                .deployments_holding()
                .remove(&id.to_string())
                .unwrap_or_default();
            Json(PackageSetView::of(summary, deployments)).into_response()
        }
        None => error(StatusCode::NOT_FOUND, format!("no package set {id}")),
    }
}

/// Maps a store refusal onto the status the REST contract names: immutability and emptiness are
/// conflicts with the Set's current state (`409`), absence is `404`, bad input `400`.
fn package_error(e: String) -> Response {
    if e.contains("not configured") || e.starts_with("no package set") {
        error(StatusCode::NOT_FOUND, e)
    } else if e.contains("immutable") || e.contains("holds no entries") {
        error(StatusCode::CONFLICT, e)
    } else if e.starts_with("invalid") || e.starts_with("the ") || e.contains("empty") {
        error(StatusCode::BAD_REQUEST, e)
    } else {
        error(StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}

/// All stored Sets, in identity order (never the artifact bytes). A UI groups them by name; the
/// list itself is flat, sorted by name, then version, then type.
#[utoipa::path(
    get,
    path = "/api/v1/packages",
    tag = "packages",
    responses(
        (status = 200, description = "Every stored package Set", body = [PackageSetView]),
        (status = 404, description = "Package delivery is not configured", body = ErrorBody)
    )
)]
async fn list_packages(State(state): State<Arc<AppState>>) -> Response {
    match state.packages() {
        Some(store) => {
            let summaries = store.list();
            // One pass over the fleet for the whole list, rather than one per Set.
            let holding = state.deployments_holding();
            Json(
                summaries
                    .into_iter()
                    .map(|summary| {
                        let key = format!("{}@{}", summary.agent_type, summary.version);
                        let deployments = holding.get(&key).cloned().unwrap_or_default();
                        PackageSetView::of(summary, deployments)
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

/// One stored Set.
#[utoipa::path(
    get,
    path = "/api/v1/packages/{agent_type}/{version}",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name (ADR-0010 grammar)"),
        ("agent_type" = String, Path, description = "The Agent type the Set is built for (ADR-0034)"),
        ("version" = String, Path, description = "The Set's version")
    ),
    responses(
        (status = 200, description = "The stored Set", body = PackageSetView),
        (status = 400, description = "Invalid identity", body = ErrorBody),
        (status = 404, description = "No such Set, or package delivery is not configured", body = ErrorBody)
    )
)]
async fn get_package_set(
    State(state): State<Arc<AppState>>,
    Path((agent_type, version)): Path<(String, String)>,
) -> Response {
    match package_id(&agent_type, &version) {
        Ok(id) => set_response(&state, &id),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

/// Creates a Set, or updates an existing one's Selector and kind. **Saving never distributes**
/// (ADR-0061): the Set reaches an Agent only through a rollout act. The identity in the path is
/// the whole identity: a new version is a new Set, never a mutation of an old one.
#[utoipa::path(
    put,
    path = "/api/v1/packages/{agent_type}/{version}",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name (ADR-0010 grammar)"),
        ("agent_type" = String, Path, description = "The Agent type the Set is built for, compared raw against the `service.name` Agents report"),
        ("version" = String, Path, description = "The Set's version — every entry shares it")
    ),
    responses(
        (status = 200, description = "The stored Set", body = PackageSetView),
        (status = 400, description = "Invalid identity or body", body = ErrorBody),
        (status = 404, description = "Package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Set is assigned to an Agent and its kind is frozen", body = ErrorBody),
        (status = 500, description = "The Set could not be persisted", body = ErrorBody)
    )
)]
async fn put_package_set(
    State(state): State<Arc<AppState>>,
    Path((agent_type, version)): Path<(String, String)>,
) -> Response {
    let id = match package_id(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match state.create_package_set(&id) {
        Ok(()) => set_response(&state, &id),
        Err(e) => package_error(e),
    }
}

/// Deletes a Set — entries, artifacts, metadata, and every per-Agent assignment that referenced
/// it (ADR-0061): the offer is withdrawn, and Agents that installed it keep running it
/// (ADR-0017).
#[utoipa::path(
    delete,
    path = "/api/v1/packages/{agent_type}/{version}",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name"),
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version")
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 400, description = "Invalid identity", body = ErrorBody),
        (status = 404, description = "No such Set, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The Set could not be deleted", body = ErrorBody)
    )
)]
async fn delete_package_set(
    State(state): State<Arc<AppState>>,
    Path((agent_type, version)): Path<(String, String)>,
) -> Response {
    let id = match package_id(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match state.delete_package_set(&id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no package set {id}")),
        Err(e) => package_error(e),
    }
}

/// Stores one platform's artifact as an entry of a Set (ADR-0052). The artifact is the raw
/// request body; the Set and the platform are the path. Nothing is distributed: the Set reaches
/// nobody until a rollout act releases it (ADR-0061). Refused while the Set is assigned to an
/// Agent — an assigned Set's bytes are immutable.
#[utoipa::path(
    put,
    path = "/api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name"),
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version"),
        ("os" = String, Path, description = "The operating system this artifact is built for, as `os.type`: `linux`, `darwin`, `windows`"),
        ("arch" = String, Path, description = "The architecture, as `host.arch`: `amd64`, `arm64`"),
        EntryUpload
    ),
    request_body(content = Vec<u8>, description = "The artifact bytes", content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "The Set, with the stored entry", body = PackageSetView),
        (status = 400, description = "Invalid identity, platform, empty artifact, or bad signature", body = ErrorBody),
        (status = 404, description = "No such Set, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Set is assigned to an Agent and its entries are immutable", body = ErrorBody),
        (status = 413, description = "The artifact exceeds max_package_size_bytes", body = ErrorBody),
        (status = 500, description = "The entry could not be persisted", body = ErrorBody),
        (status = 507, description = "Storing it would exceed max_total_package_bytes", body = ErrorBody)
    )
)]
async fn put_package_entry(
    State(state): State<Arc<AppState>>,
    Path((agent_type, version, os, arch)): Path<(String, String, String, String)>,
    Query(upload): Query<EntryUpload>,
    body: Body,
) -> Response {
    let id = match package_id(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    if upload.signature.is_some() {
        // Named rather than ignored: a signature dropped on the floor here is an unsigned rollout
        // nobody notices, which is the one failure mode this whole gate exists to prevent.
        return error(
            StatusCode::BAD_REQUEST,
            format!(
                "a signature belongs to the deployment that offers these bytes, not to the \
                 artifact (ADR-0096) — upload the artifact without it, then \
                 PUT /api/v1/deployments/<name>/signatures/{agent_type}/{version}/{os}/{arch}"
            ),
        );
    }
    let platform = match crate::packages::Platform::new(&os, &arch) {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    let staged = match state.package_staging_path(&id, &platform) {
        Ok(path) => path,
        Err(e) => return package_error(e),
    };
    // Refuse before streaming a gibibyte we would only reject: a store already at its ceiling takes
    // nothing more. This — with the whole-store check after the stream — is what stops a caller
    // filling the disk by uploading artifact after artifact under distinct names (ADR-0015).
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
    match state.put_package_entry(&id, &platform, &staged) {
        Ok(()) => {
            info!(set = %id, bytes = written, "package entry stored from the API");
            set_response(&state, &id)
        }
        Err(e) => package_error(e),
    }
}

/// Deletes one entry of a Set. Refused while the Set is assigned to an Agent — its bytes are
/// immutable (ADR-0061). The last entry taken away leaves an empty Set: a Set being reassembled
/// is a normal state, and deleting the Set is its own act.
#[utoipa::path(
    delete,
    path = "/api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name"),
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version"),
        ("os" = String, Path, description = "The entry's operating system"),
        ("arch" = String, Path, description = "The entry's architecture")
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 400, description = "Invalid identity or platform", body = ErrorBody),
        (status = 404, description = "No such Set or entry, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Set is assigned to an Agent and its entries are immutable", body = ErrorBody),
        (status = 500, description = "The entry could not be deleted", body = ErrorBody)
    )
)]
async fn delete_package_entry(
    State(state): State<Arc<AppState>>,
    Path((agent_type, version, os, arch)): Path<(String, String, String, String)>,
) -> Response {
    let id = match package_id(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let platform = match crate::packages::Platform::new(&os, &arch) {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    match state.delete_package_entry(&id, &platform) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            format!("no entry {}-{} in set {id}", platform.os, platform.arch),
        ),
        Err(e) => package_error(e),
    }
}

/// The body of `PUT …/entries/{os}/{arch}/source` (ADR-0018, per ADR-0052): an entry that is a
/// reference instead of an upload.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct EntrySourceSpec {
    /// Where the artifact lives — `http://` or `https://`. Agents fetch it from here; this Server
    /// never downloads it.
    url: String,
    /// The artifact's SHA-256, hex, as published in the release's checksums file. Required: for a
    /// referenced entry nothing here ever sees the bytes, so this is what protects every Agent.
    sha256: String,
    /// Retired (ADR-0096): the signature belongs to the Deployment that offers these bytes.
    /// Supplying it here is refused by name rather than ignored.
    #[serde(default)]
    signature: Option<String>,
    /// Headers the Agents send with the download — a token for a private source. Two things to know
    /// before using one: it is stored in cleartext in the package store (owner-only on disk, not
    /// encrypted), and it is delivered to **every** Agent the Set targets. Prefer a
    /// narrowly-scoped, rotatable token over a long-lived credential.
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
}

/// Points one entry of a Set at an artifact hosted elsewhere (ADR-0018), instead of uploading
/// it. Refused while the Set is assigned to an Agent (ADR-0061). The Server stores the reference
/// and offers it verbatim; it never downloads the artifact, so the `sha256` — and the signature,
/// when one is configured — is what protects every Agent.
///
/// The URL is probed once, to catch a typo while the operator is still looking at the screen. A
/// definitive refusal from the source (a 4xx) fails the request; a source this Server simply
/// cannot reach does not, because the Server is not in the download path and its reachability says
/// nothing about the Agents'.
#[utoipa::path(
    put,
    path = "/api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}/source",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name"),
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version"),
        ("os" = String, Path, description = "The entry's operating system"),
        ("arch" = String, Path, description = "The entry's architecture")
    ),
    request_body = EntrySourceSpec,
    responses(
        (status = 200, description = "The Set, with the referenced entry", body = PackageSetView),
        (status = 400, description = "Invalid identity, url, hash or signature — or the source refused the probe", body = ErrorBody),
        (status = 404, description = "No such Set, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Set is assigned to an Agent and its entries are immutable", body = ErrorBody),
        (status = 500, description = "The reference could not be persisted", body = ErrorBody)
    )
)]
async fn put_package_entry_source(
    State(state): State<Arc<AppState>>,
    Path((agent_type, version, os, arch)): Path<(String, String, String, String)>,
    Json(spec): Json<EntrySourceSpec>,
) -> Response {
    let id = match package_id(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let platform = match crate::packages::Platform::new(&os, &arch) {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    let content_hash = match hex::decode(spec.sha256.trim()) {
        Ok(bytes) => bytes,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid sha256: {e}")),
    };
    if spec.signature.is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "a signature belongs to the deployment that offers these bytes, not to the source \
             record (ADR-0096) — put it on the deployment instead"
                .to_string(),
        );
    }
    if let Err(e) = probe(&spec.url, &spec.headers).await {
        return error(StatusCode::BAD_REQUEST, e);
    }
    let source = crate::packages::Source {
        url: spec.url.clone(),
        headers: spec.headers.clone(),
    };
    match state.set_package_entry_source(&id, &platform, content_hash, source) {
        Ok(()) => {
            info!(set = %id, url = %spec.url, "package entry source stored from the API");
            set_response(&state, &id)
        }
        Err(e) => package_error(e),
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
/// operator's mirror (ADR-0018) legitimately lives on an internal network, and the URL here is the
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
    Path((agent_type, version)): Path<(String, String)>,
    Query(query): Query<PlatformQuery>,
) -> Response {
    let id = match package_id(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let platform = match query.platform() {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    let Some(path) = state
        .packages()
        .and_then(|store| store.artifact_path(&id, &platform))
    else {
        return error(
            StatusCode::NOT_FOUND,
            format!(
                "no set {id} with an artifact for {}-{}",
                platform.os, platform.arch
            ),
        );
    };
    // Streamed from disk, never buffered: a fleet updating at once means many concurrent
    // downloads of the same artifact, and each one holding a copy of a program in memory is how a
    // rollout takes the Server down with it.
    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(e) => {
            warn!(package = %id, error = %e, "cannot open the stored artifact");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the artifact of {id} cannot be read"),
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
                format!("cannot serve the artifact of {id}: {e}"),
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

// -------------------------------------------------------------------------------------------
// Deployments (ADR-0096): the aim, the signature, and the act — everything a Package gave up.
// -------------------------------------------------------------------------------------------

/// One Deployment as the REST API shows it.
#[derive(Serialize, ToSchema)]
struct DeploymentView {
    /// The operator's name for this channel, and the path segment that addresses it.
    name: String,
    /// Equality pairs that must all match an attribute the Agent reported, labels included
    /// (ADR-0012). **Never empty**: there is no fleet-wide default, because an Agent belongs to at
    /// most one Deployment and an empty Selector would match every Agent in every other channel.
    selector: std::collections::BTreeMap<String, String>,
    /// At most one Package per Agent type — an Agent has one binary to replace.
    packages: Vec<DeploymentPackageView>,
    /// Agents this channel claims, and no other does.
    ///
    /// **Zero is the value worth looking at**: a channel aims at nobody when its Selector names an
    /// attribute no Agent reports, or a value none of them carries — and neither is a rejected
    /// write, so nothing else would say so.
    claiming_agents: usize,
    /// Of those, the Agents a rollout act would actually move — this channel holds a Package for what
    /// they report and it is an upgrade (ADR-0083). Zero here with a non-zero `claiming_agents`
    /// means everyone in the channel already runs it: nothing to do, and nothing wrong.
    targeted_agents: usize,
    /// Agents this channel matches that **another Deployment matches too**. They are offered nothing
    /// new until an operator narrows a Selector, and they are not in `claiming_agents`.
    conflicting_agents: usize,
}

/// One Package a Deployment holds, and how much of it is signed.
#[derive(Serialize, ToSchema)]
struct DeploymentPackageView {
    /// The Agent type this Package is built for — also the key under which this Deployment holds
    /// it, and the name it carries on the wire.
    agent_type: String,
    version: String,
    /// What an operator reads: the Agent type and the version together.
    display_name: String,
    /// The platforms whose artifact this Deployment holds a signature for, as `os/arch`.
    ///
    /// Read it against the Package's own entries: a platform listed there and missing here is one
    /// an Agent will be offered **unsigned**. The Server does not refuse that — an unsigned fleet
    /// is a legitimate policy (ADR-0015) — so it reports it, which is the only thing left to do.
    signed_platforms: Vec<String>,
}

impl DeploymentView {
    fn of(
        deployment: crate::deployments::Deployment,
        reach: crate::fleet::DeploymentReach,
    ) -> Self {
        let mut packages: Vec<DeploymentPackageView> = deployment
            .packages
            .values()
            .map(|id| DeploymentPackageView {
                agent_type: id.agent_type.clone(),
                version: id.version.clone(),
                display_name: id.display_name(),
                signed_platforms: deployment
                    .signatures
                    .keys()
                    .filter(|(held, _)| held == id)
                    .map(|(_, platform)| format!("{}/{}", platform.os, platform.arch))
                    .collect(),
            })
            .collect();
        packages.sort_by(|a, b| a.agent_type.cmp(&b.agent_type));
        DeploymentView {
            name: deployment.name,
            selector: deployment.selector,
            packages,
            claiming_agents: reach.claiming,
            targeted_agents: reach.targeted,
            conflicting_agents: reach.conflicting,
        }
    }
}

/// The writable part of a Deployment — the body of `PUT /api/v1/deployments/{name}`.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct DeploymentSpec {
    /// The channel this Deployment aims at. Required and never empty.
    selector: std::collections::BTreeMap<String, String>,
}

/// One artifact's Ed25519 signature, as the operator supplies it.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct SignatureSpec {
    /// The signature over the artifact's bytes, hex-encoded — what `opamp-package-sign` prints.
    signature: String,
}

/// Maps a store refusal onto the status code that says the same thing.
fn deployment_error(refusal: crate::deployments::DeploymentError) -> Response {
    use crate::deployments::DeploymentError as E;
    let text = refusal.to_string();
    match refusal {
        E::Invalid(_) => error(StatusCode::BAD_REQUEST, text),
        E::NotFound => error(StatusCode::NOT_FOUND, text),
        E::TypeTaken { .. } | E::Conflict(_) => error(StatusCode::CONFLICT, text),
        E::Storage(_) => error(StatusCode::INTERNAL_SERVER_ERROR, text),
    }
}

/// The Deployment store, or the `404` that says package delivery is not armed on this Server.
fn deployments(state: &AppState) -> Result<&crate::deployments::DeploymentStore, Refusal> {
    state.deployment_store().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "package delivery is not configured on this Server (packages_dir)".to_string(),
        )
    })
}

/// A status and what to say with it — carried instead of a whole `Response` so a helper's error
/// path stays small (the response is built once, at the handler that returns it).
type Refusal = (StatusCode, String);

/// Every Deployment, in name order.
#[utoipa::path(
    get,
    path = "/api/v1/deployments",
    tag = "deployments",
    responses((status = 200, body = [DeploymentView]))
)]
async fn list_deployments(State(state): State<Arc<AppState>>) -> Response {
    match deployments(&state) {
        Ok(store) => {
            let reach = state.deployment_reach();
            Json(
                store
                    .list()
                    .into_iter()
                    .map(|deployment| {
                        let counts = reach.get(&deployment.name).copied().unwrap_or_default();
                        DeploymentView::of(deployment, counts)
                    })
                    .collect::<Vec<_>>(),
            )
            .into_response()
        }
        Err((status, why)) => error(status, why),
    }
}

/// One Deployment.
#[utoipa::path(
    get,
    path = "/api/v1/deployments/{name}",
    tag = "deployments",
    params(("name" = String, Path, description = "the Deployment's name")),
    responses((status = 200, body = DeploymentView), (status = 404))
)]
async fn get_deployment(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let store = match deployments(&state) {
        Ok(store) => store,
        Err((status, why)) => return error(status, why),
    };
    match store.get(&name) {
        Some(deployment) => deployment_response(&state, deployment),
        None => error(StatusCode::NOT_FOUND, format!("no deployment {name:?}")),
    }
}

/// Creates a Deployment, or replaces the channel it aims at.
///
/// **This distributes nothing** (ADR-0061). Saving is saving; the rollout act is its own press,
/// and until it happens no Agent is offered anything new.
///
/// The Selector must name at least one pair. There is deliberately no fleet-wide default: an Agent
/// belongs to at most one Deployment, so an empty Selector would collide with every other channel the
/// moment a second one exists — and it is what a forgotten field looks like. Channels are a partition
/// over an attribute every Agent carries (`channel = "stable"`), set at provisioning or as a Server
/// label (ADR-0042), because a Selector is equality and cannot express "not".
#[utoipa::path(
    put,
    path = "/api/v1/deployments/{name}",
    tag = "deployments",
    params(("name" = String, Path, description = "the Deployment's name")),
    request_body = DeploymentSpec,
    responses((status = 200, body = DeploymentView), (status = 400), (status = 404))
)]
async fn put_deployment(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(spec): Json<DeploymentSpec>,
) -> Response {
    let store = match deployments(&state) {
        Ok(store) => store,
        Err((status, why)) => return error(status, why),
    };
    match store.put(&name, spec.selector) {
        Ok(deployment) => deployment_response(&state, deployment),
        Err(e) => deployment_error(e),
    }
}

/// Deletes a Deployment. What was rolled out through it is withdrawn as an offer; nothing is
/// uninstalled (ADR-0061 point 7's rule, unchanged).
#[utoipa::path(
    delete,
    path = "/api/v1/deployments/{name}",
    tag = "deployments",
    params(("name" = String, Path, description = "the Deployment's name")),
    responses((status = 204), (status = 404))
)]
async fn delete_deployment(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let store = match deployments(&state) {
        Ok(store) => store,
        Err((status, why)) => return error(status, why),
    };
    match store.delete(&name) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no deployment {name:?}")),
        Err(e) => deployment_error(e),
    }
}

/// Re-aims a Deployment. Editable in every state — aim is not bytes, and moving a channel is how a
/// rollout proceeds. It changes whom the *next* act would reach, never a running offer.
#[utoipa::path(
    put,
    path = "/api/v1/deployments/{name}/selector",
    tag = "deployments",
    params(("name" = String, Path, description = "the Deployment's name")),
    request_body = DeploymentSpec,
    responses((status = 200, body = DeploymentView), (status = 400), (status = 404))
)]
async fn put_deployment_selector(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(spec): Json<DeploymentSpec>,
) -> Response {
    let store = match deployments(&state) {
        Ok(store) => store,
        Err((status, why)) => return error(status, why),
    };
    if store.get(&name).is_none() {
        return error(StatusCode::NOT_FOUND, format!("no deployment {name:?}"));
    }
    match store.put(&name, spec.selector) {
        Ok(deployment) => deployment_response(&state, deployment),
        Err(e) => deployment_error(e),
    }
}

/// Puts a Package into a Deployment.
///
/// At most one per Agent type: a second is refused `409` naming what is already held, because two
/// would collide on the wire map key *and* fit the same Agent. Writing the same one again is the
/// same request arriving twice and succeeds; `?replace=true` is how an operator says they mean to
/// swap the version this channel runs.
#[utoipa::path(
    put,
    path = "/api/v1/deployments/{name}/packages/{agent_type}/{version}",
    tag = "deployments",
    params(
        ("name" = String, Path, description = "the Deployment's name"),
        ("agent_type" = String, Path, description = "the Package's Agent type"),
        ("version" = String, Path, description = "the Package's version"),
        ("replace" = Option<bool>, Query, description = "swap the Package held for this Agent type")
    ),
    responses((status = 200, body = DeploymentView), (status = 404), (status = 409))
)]
async fn put_deployment_package(
    State(state): State<Arc<AppState>>,
    Path((name, agent_type, version)): Path<(String, String, String)>,
    Query(replace): Query<ReplaceQuery>,
) -> Response {
    let id = match package_id(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    // A channel cannot offer what the store does not hold: the artifacts are what the Deployment
    // signs and hands out, so a reference to a Package that is not there is a mistake, not a
    // reservation for one uploaded later.
    match state.packages() {
        Some(packages) if packages.summary(&id).is_some() => {}
        _ => {
            return error(
                StatusCode::NOT_FOUND,
                format!("no package {id} — upload it before a deployment can offer it"),
            )
        }
    }
    match state.put_deployment_package(&name, &id, replace.replace.unwrap_or(false)) {
        Ok(deployment) => deployment_response(&state, deployment),
        Err(e) => deployment_error(e),
    }
}

/// Takes a Package out of a Deployment, and every signature that named it.
#[utoipa::path(
    delete,
    path = "/api/v1/deployments/{name}/packages/{agent_type}/{version}",
    tag = "deployments",
    params(
        ("name" = String, Path, description = "the Deployment's name"),
        ("agent_type" = String, Path, description = "the Package's Agent type"),
        ("version" = String, Path, description = "the Package's version")
    ),
    responses((status = 200, body = DeploymentView), (status = 404))
)]
async fn delete_deployment_package(
    State(state): State<Arc<AppState>>,
    Path((name, agent_type, version)): Path<(String, String, String)>,
) -> Response {
    let id = match package_id(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match state.remove_deployment_package(&name, &id) {
        Ok(deployment) => deployment_response(&state, deployment),
        Err(e) => deployment_error(e),
    }
}

/// Records the Ed25519 signature of one artifact this Deployment offers (ADR-0096 point 7).
///
/// The signature lives here rather than on the artifact because what an operator signs off on is a
/// release to a set of machines. The same Package in two Deployments is therefore signed in each.
/// Generate it with `opamp-package-sign`.
#[utoipa::path(
    put,
    path = "/api/v1/deployments/{name}/signatures/{agent_type}/{version}/{os}/{arch}",
    tag = "deployments",
    params(
        ("name" = String, Path, description = "the Deployment's name"),
        ("agent_type" = String, Path, description = "the Package's Agent type"),
        ("version" = String, Path, description = "the Package's version"),
        ("os" = String, Path, description = "the artifact's operating system"),
        ("arch" = String, Path, description = "the artifact's architecture")
    ),
    request_body = SignatureSpec,
    responses((status = 200, body = DeploymentView), (status = 400), (status = 404))
)]
async fn put_deployment_signature(
    State(state): State<Arc<AppState>>,
    Path((name, agent_type, version, os, arch)): Path<(String, String, String, String, String)>,
    Json(spec): Json<SignatureSpec>,
) -> Response {
    let (id, platform) = match deployment_artifact(&agent_type, &version, &os, &arch) {
        Ok(pair) => pair,
        Err((status, why)) => return error(status, why),
    };
    let signature = match hex::decode(spec.signature.trim()) {
        Ok(bytes) => bytes,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("the signature must be hex, as `opamp-package-sign` prints it: {e}"),
            )
        }
    };
    match state.put_deployment_signature(&name, &id, &platform, signature) {
        Ok(deployment) => deployment_response(&state, deployment),
        Err(e) => deployment_error(e),
    }
}

/// Takes one artifact's signature away. The Package stays; what it is offered with changes.
#[utoipa::path(
    delete,
    path = "/api/v1/deployments/{name}/signatures/{agent_type}/{version}/{os}/{arch}",
    tag = "deployments",
    params(
        ("name" = String, Path, description = "the Deployment's name"),
        ("agent_type" = String, Path, description = "the Package's Agent type"),
        ("version" = String, Path, description = "the Package's version"),
        ("os" = String, Path, description = "the artifact's operating system"),
        ("arch" = String, Path, description = "the artifact's architecture")
    ),
    responses((status = 200, body = DeploymentView), (status = 404))
)]
async fn delete_deployment_signature(
    State(state): State<Arc<AppState>>,
    Path((name, agent_type, version, os, arch)): Path<(String, String, String, String, String)>,
) -> Response {
    let (id, platform) = match deployment_artifact(&agent_type, &version, &os, &arch) {
        Ok(pair) => pair,
        Err((status, why)) => return error(status, why),
    };
    match state.remove_deployment_signature(&name, &id, &platform) {
        Ok(deployment) => deployment_response(&state, deployment),
        Err(e) => deployment_error(e),
    }
}

/// The `(Package, Platform)` a signature route addresses, or the `400` that says which half of it
/// does not parse.
fn deployment_artifact(
    agent_type: &str,
    version: &str,
    os: &str,
    arch: &str,
) -> Result<(crate::packages::PackageId, crate::packages::Platform), Refusal> {
    let id = package_id(agent_type, version).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let platform =
        crate::packages::Platform::new(os, arch).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok((id, platform))
}

/// `?replace=true` on putting a Package into a Deployment.
#[derive(Deserialize, IntoParams)]
struct ReplaceQuery {
    replace: Option<bool>,
}

/// Rolls a Deployment out to **every Agent it claims** (ADR-0061 point 5, ADR-0096 point 8).
///
/// **This is the moment the fleet changes.** Each claimed Agent's assignment is written with this
/// channel and the Package it holds for what that Agent reports, pinned as of this press. An Agent
/// that enrols — or is labelled into the channel — later is *not* included: it surfaces in the fleet
/// view as waiting, for its own act.
///
/// An Agent some **other** Deployment also claims is skipped rather than counted. Its conflict is
/// reported on the Agent, and a press that quietly resolved it here would be the ranking this
/// model removed wearing a different hat. Nothing is ever uninstalled by an assignment change; an
/// Agent keeps running what it installed.
#[utoipa::path(
    post,
    path = "/api/v1/deployments/{name}/rollout",
    tag = "deployments",
    params(("name" = String, Path, description = "the Deployment's name")),
    responses(
        (status = 200, description = "Rolled out; how many Agents were assigned", body = RolloutOutcome),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No such Deployment, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Deployment holds no packages", body = ErrorBody)
    )
)]
async fn rollout_deployment(
    State(state): State<Arc<AppState>>,
    _csrf: SameOrigin,
    Path(name): Path<String>,
) -> Response {
    match state.rollout_deployment(&name) {
        Ok(assigned_agents) => {
            info!(deployment = %name, agents = assigned_agents, "deployment rolled out from the API");
            Json(RolloutOutcome { assigned_agents }).into_response()
        }
        Err(e) => rollout_error(e),
    }
}

/// One Deployment as the API answers with it, with the reach counts read from the fleet — so every
/// response says whom this channel reaches as of now, and not as of whenever it was written.
fn deployment_response(state: &AppState, deployment: crate::deployments::Deployment) -> Response {
    let reach = state
        .deployment_reach()
        .get(&deployment.name)
        .copied()
        .unwrap_or_default();
    Json(DeploymentView::of(deployment, reach)).into_response()
}
