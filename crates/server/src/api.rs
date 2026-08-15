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

use crate::configs::{self, Configuration, ConfigurationSpec, Revision};
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
        (name = "packages", description = "Software packages the Server delivers (ADR-0015)")
    )
)]
struct ApiDoc;

pub fn router(state: Arc<AppState>) -> Router {
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
        .routes(routes!(put_package_set_selector))
        .routes(routes!(rollout_package_set))
        .routes(routes!(put_package_entry_source))
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
                   so a rollout ring is a Server-side decision instead of an edit to client.toml on \
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
                 comes from, in that host's client.toml, or label it under another key"
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
    /// Release this Set. Any Set that fits and aims at the Agent may be named — an older version
    /// too, which is the rollback.
    #[serde(default)]
    package: Option<PackageRef>,
}

/// A Set's identity, as a rollout body names it.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct PackageRef {
    name: String,
    agent_type: String,
    version: String,
}

/// Rolls a Configuration or a package Set out to **this Agent** (ADR-0061) — or, with an empty
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
        (status = 404, description = "No such Agent, Configuration, or Set", body = ErrorBody),
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
    let target =
        match (spec.configuration, spec.package) {
            (Some(_), Some(_)) => return error(
                StatusCode::BAD_REQUEST,
                "name a configuration or a package, not both — or neither for everything waiting",
            ),
            (Some(name), None) => RolloutTarget::Configuration(name),
            (None, Some(package)) => {
                match set_id(&package.name, &package.agent_type, &package.version) {
                    Ok(id) => RolloutTarget::Package(id),
                    Err(e) => return error(StatusCode::BAD_REQUEST, e),
                }
            }
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
    name: String,
    /// The Agent type this Set is built for, matched raw against the `service.name` an Agent
    /// reports before any Selector is considered (ADR-0034). Part of the Set's identity.
    service_name: String,
    /// The version every entry of this Set shares. Part of the Set's identity.
    version: String,
    /// Whom a rollout act would release this Set to (ADR-0017): equality pairs that must all
    /// match an attribute the Agent reported. Empty targets every Agent of this Set's type.
    /// Always editable — it steers the next act, never a running offer.
    #[serde(default)]
    selector: std::collections::BTreeMap<String, String>,
    /// `true` for an addon, `false` for a top-level package (a Managed Process's binary).
    #[serde(default)]
    addon: bool,
    /// One entry per platform. An Agent is offered the one built for the machine it reported,
    /// and never another (ADR-0031).
    entries: Vec<PackageEntryView>,
    /// How many Agents in the fleet this Set **would** reach — fitted by type and platform, then
    /// aimed by Selector and resolved against its sibling versions, exactly as the offer resolves.
    ///
    /// **`0` is the value worth looking at.** A Set targets nobody when its type is misspelled,
    /// when no entry matches any reported platform, or when its Selector matches no Agent — and
    /// none of those is a rejected upload, so nothing else would say so. A **draft** is counted as
    /// if it were published, because staging a rollout is how its aim is checked before it starts.
    targeted_agents: usize,
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
    /// Where Agents fetch the artifact when this Server does not hold it (ADR-0018). Absent for an
    /// uploaded one, which is served from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    /// Whether the operator supplied an Ed25519 signature for this entry.
    signed: bool,
}

impl PackageSetView {
    fn of(summary: crate::packages::SetSummary, targeted_agents: usize) -> Self {
        PackageSetView {
            targeted_agents,
            name: summary.name,
            service_name: summary.service_name,
            version: summary.version,
            selector: summary.selector,
            addon: summary.addon,
            entries: summary
                .entries
                .into_iter()
                .map(|entry| PackageEntryView {
                    os: entry.os,
                    arch: entry.arch,
                    size: entry.size,
                    source_url: entry.source_url,
                    signed: entry.signed,
                })
                .collect(),
        }
    }
}

/// The Platform the download route names (ADR-0031): the artifact endpoint serves bytes, and a
/// request naming bytes names the Platform they are for.
#[derive(Deserialize, IntoParams)]
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

/// The writable part of a Set — the body of `PUT /api/v1/packages/{name}/{agent_type}/{version}`.
/// Everything else about a Set is either its identity (the path) or set through its own
/// sub-resource (`…/publication`), or belongs to an entry.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct PackageSetSpec {
    /// Equality pairs an Agent's reported attributes must all match; empty targets every Agent of
    /// this Set's type.
    #[serde(default)]
    selector: std::collections::BTreeMap<String, String>,
    /// `true` marks an addon; the default is a top-level package (a Managed Process's binary).
    /// Frozen with the bytes while the Set is published.
    #[serde(default)]
    addon: bool,
}

/// The writable Selector of a Set — the body of `PUT …/selector`.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct PackageSelectorSpec {
    /// Equality pairs an Agent's reported attributes must all match; empty targets every Agent of
    /// this Set's type.
    #[serde(default)]
    selector: std::collections::BTreeMap<String, String>,
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
fn set_id(name: &str, agent_type: &str, version: &str) -> Result<crate::packages::SetId, String> {
    crate::packages::SetId::new(name, agent_type, version)
}

/// A stored Set as the API answers with it, read back from the store rather than assembled from
/// whatever the handler happened to be given — so every response describes the Set as it now is.
fn set_response(state: &AppState, id: &crate::packages::SetId) -> Response {
    match state.packages().and_then(|store| store.summary(id)) {
        Some(summary) => {
            let reach = state
                .package_reach()
                .get(&id.to_string())
                .copied()
                .unwrap_or(0);
            Json(PackageSetView::of(summary, reach)).into_response()
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
            let reach = state.package_reach();
            Json(
                summaries
                    .into_iter()
                    .map(|summary| {
                        let key = format!(
                            "{}@{}@{}",
                            summary.name, summary.version, summary.service_name
                        );
                        let targeted = reach.get(&key).copied().unwrap_or(0);
                        PackageSetView::of(summary, targeted)
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
    path = "/api/v1/packages/{name}/{agent_type}/{version}",
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
    Path((name, agent_type, version)): Path<(String, String, String)>,
) -> Response {
    match set_id(&name, &agent_type, &version) {
        Ok(id) => set_response(&state, &id),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

/// Creates a Set, or updates an existing one's Selector and kind. **Saving never distributes**
/// (ADR-0061): the Set reaches an Agent only through a rollout act. The identity in the path is
/// the whole identity: a new version is a new Set, never a mutation of an old one.
#[utoipa::path(
    put,
    path = "/api/v1/packages/{name}/{agent_type}/{version}",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name (ADR-0010 grammar)"),
        ("agent_type" = String, Path, description = "The Agent type the Set is built for, compared raw against the `service.name` Agents report"),
        ("version" = String, Path, description = "The Set's version — every entry shares it")
    ),
    request_body = PackageSetSpec,
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
    Path((name, agent_type, version)): Path<(String, String, String)>,
    Json(spec): Json<PackageSetSpec>,
) -> Response {
    let id = match set_id(&name, &agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match state.create_package_set(&id, spec.selector, spec.addon) {
        Ok(()) => set_response(&state, &id),
        Err(e) => package_error(e),
    }
}

/// Deletes a Set — entries, artifacts, metadata, and every per-Agent assignment that referenced
/// it (ADR-0061): the offer is withdrawn, and Agents that installed it keep running it
/// (ADR-0017).
#[utoipa::path(
    delete,
    path = "/api/v1/packages/{name}/{agent_type}/{version}",
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
    Path((name, agent_type, version)): Path<(String, String, String)>,
) -> Response {
    let id = match set_id(&name, &agent_type, &version) {
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
    path = "/api/v1/packages/{name}/{agent_type}/{version}/entries/{os}/{arch}",
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
    Path((name, agent_type, version, os, arch)): Path<(String, String, String, String, String)>,
    Query(upload): Query<EntryUpload>,
    body: Body,
) -> Response {
    let id = match set_id(&name, &agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
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
    match state.put_package_entry(&id, &platform, signature, &staged) {
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
    path = "/api/v1/packages/{name}/{agent_type}/{version}/entries/{os}/{arch}",
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
    Path((name, agent_type, version, os, arch)): Path<(String, String, String, String, String)>,
) -> Response {
    let id = match set_id(&name, &agent_type, &version) {
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
    /// Hex Ed25519 signature over the artifact, checked by the Agent against its configured key.
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
    path = "/api/v1/packages/{name}/{agent_type}/{version}/entries/{os}/{arch}/source",
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
    Path((name, agent_type, version, os, arch)): Path<(String, String, String, String, String)>,
    Json(spec): Json<EntrySourceSpec>,
) -> Response {
    let id = match set_id(&name, &agent_type, &version) {
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
    match state.set_package_entry_source(&id, &platform, content_hash, signature, source) {
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

/// Sets which Agents a rollout act would release this Set to (ADR-0017). An empty Selector
/// targets every Agent of the Set's type; every pair must equal an attribute the Agent reported,
/// exactly as for a Configuration. **Always editable, and never distributing** (ADR-0061):
/// widening a Selector only widens what the fleet view proposes and whom the next act reaches.
///
/// Where several Sets of one name match an Agent, the most specific Selector wins the candidate,
/// and among equally specific ones the greater version (ADR-0052). A tie the version comparison
/// cannot break leaves that Agent with no proposal, and the fleet view says so
/// (`package_conflict`).
#[utoipa::path(
    put,
    path = "/api/v1/packages/{name}/{agent_type}/{version}/selector",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name"),
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version")
    ),
    request_body = PackageSelectorSpec,
    responses(
        (status = 200, description = "The Set, with its Selector", body = PackageSetView),
        (status = 400, description = "Invalid identity", body = ErrorBody),
        (status = 404, description = "No such Set, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The Selector could not be persisted", body = ErrorBody)
    )
)]
async fn put_package_set_selector(
    State(state): State<Arc<AppState>>,
    Path((name, agent_type, version)): Path<(String, String, String)>,
    Json(spec): Json<PackageSelectorSpec>,
) -> Response {
    let id = match set_id(&name, &agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match state.set_package_selector(&id, spec.selector.clone()) {
        Ok(_) => {
            info!(set = %id, pairs = spec.selector.len(), "package selector set");
            set_response(&state, &id)
        }
        Err(e) => package_error(e),
    }
}

/// Rolls a Set out to **every Agent it currently fits and its Selector aims at** (ADR-0061).
///
/// **This is the moment the fleet changes.** The Set is written into each matching Agent's
/// assignment, replacing any other version of the same name — and, for a top-level Set, any
/// other top-level assignment: an Agent has one binary to replace. An Agent that enrols — or
/// starts matching — later is *not* included: it surfaces in the fleet view as waiting, for its
/// own act. Rolling out a Set with **no entries** is refused: a Set contains one or more entries.
///
/// Rollback is the same act pointed at the older version. Nothing is ever uninstalled by an
/// assignment change; an Agent keeps running what it installed (ADR-0017).
#[utoipa::path(
    post,
    path = "/api/v1/packages/{name}/{agent_type}/{version}/rollout",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name"),
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version")
    ),
    responses(
        (status = 200, description = "Rolled out; how many Agents were assigned", body = RolloutOutcome),
        (status = 400, description = "Invalid identity", body = ErrorBody),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No such Set, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Set holds no entries and cannot be rolled out", body = ErrorBody)
    )
)]
async fn rollout_package_set(
    State(state): State<Arc<AppState>>,
    _csrf: SameOrigin,
    Path((name, agent_type, version)): Path<(String, String, String)>,
) -> Response {
    let id = match set_id(&name, &agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match state.rollout_package(&id) {
        Ok(assigned_agents) => {
            info!(set = %id, agents = assigned_agents, "package rolled out from the API");
            Json(RolloutOutcome { assigned_agents }).into_response()
        }
        Err(e) => rollout_error(e),
    }
}

/// Serves an entry's artifact bytes — the `download_url` the Agent is offered points here. On the
/// unauthenticated REST plane (ADR-0013); the artifact's content hash and Ed25519 signature are
/// what the Agent verifies before it installs (ADR-0015).
#[utoipa::path(
    get,
    path = "/api/v1/packages/{name}/{agent_type}/{version}/file",
    tag = "packages",
    params(
        ("name" = String, Path, description = "The package name"),
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version"),
        PlatformQuery
    ),
    responses(
        (status = 200, description = "The artifact bytes", content_type = "application/octet-stream"),
        (status = 400, description = "Missing or invalid platform, or invalid identity", body = ErrorBody),
        (status = 404, description = "No such Set, or no uploaded artifact for that platform", body = ErrorBody)
    )
)]
async fn download_package(
    State(state): State<Arc<AppState>>,
    Path((name, agent_type, version)): Path<(String, String, String)>,
    Query(query): Query<PlatformQuery>,
) -> Response {
    let id = match set_id(&name, &agent_type, &version) {
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
