//! The REST API v1 — the Server's integration contract (DEV-six-crates-tokio-axum, SRV-configurations-compose-one-map) — and the bundled
//! rudimentary UI. Both belong to the Operator plane and are served on its own listener
//! (SEC-tls13-bounded-input-chosen-planes); the Agent-facing artifact download is the Agent plane's, in
//! `transport`.
//!
//! The OpenAPI document is generated code-first with `utoipa`: the same annotations that register
//! a route describe it, so contract and behaviour cannot drift. Any external portal generates a
//! client from `/api/v1/openapi.json`; the UI is a client of the same routes and nothing more.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, RawQuery, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use opamp::uid::InstanceUid;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use utoipa::{IntoParams, OpenApi, ToSchema};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouterExt};
use utoipa_axum::routes;

use crate::configs::{self, Configuration, Revision};
use crate::deployments::{Deployment, DeploymentError};
use crate::fleet::{self, DeleteError, ForgetError, RestartError, RolloutError};
use crate::labels::LabelError;
use crate::operator_cases::{OperatorCases, SignIn};
use crate::packages::{PackageError, PackageId, PackageSummary, Platform, SignatureState};

mod paging;

use paging::Asked;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "OpAMP Fleet REST API",
        description = "Read fleet state; create, change, and delete Configurations, Packages \
                       and the Deployments that aim and release them. The stable contract any UI \
                       or portal builds on (SRV-configurations-compose-one-map, SRV-deployments-and-pinned-rollouts)."
    ),
    tags(
        (name = "fleet", description = "The fleet as the Server sees it"),
        (name = "configurations", description = "Configurations: content an Agent type runs, \
                                                 released only through a Deployment (SRV-deployments-and-pinned-rollouts)"),
        (name = "packages", description = "Software packages the Server delivers (PKG-signed-packages-and-delivery), \
                                            and the signed floor below which no Client takes \
                                            one (PKG-versions-high-water-mark-floor)"),
        (name = "deployments", description = "What reaches a channel of hosts — Packages and \
                                              Configurations under one Selector, and the only \
                                              thing rolled out (PKG-signed-packages-and-delivery, SRV-deployments-and-pinned-rollouts)"),
        (name = "enrolment", description = "How a host gets its first certificate: a window an \
                                            operator opens, and requests an operator decides \
                                            (SEC-admission-by-client-certificate)"),
        (name = "revocation", description = "What the client CA signed, and the certificates the \
                                             Server no longer admits (SEC-admission-by-client-certificate)")
    )
)]
struct ApiDoc;

/// Records every mutating act on the Operator plane, before it runs and once it has run; an act
/// that cannot be recorded is not run (SEC-append-only-audit-record §1 and §5).
async fn record_act(
    State(cases): State<Arc<OperatorCases>>,
    request: Request,
    next: Next,
) -> Response {
    use crate::audit::Entry;
    let method = request.method().clone();
    if matches!(
        method,
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    ) {
        return next.run(request).await;
    }
    let peer = crate::answers::peer_ip(&request);
    let operator = basic_user(request.headers());
    let path = request.uri().path().to_string();
    let entry = |event: &str, outcome: &str| {
        Entry::new(event, outcome)
            .peer(peer)
            .with("plane", "operator")
            .with("operator", operator.clone())
            .with("method", method.to_string())
            .with("path", path.clone())
    };
    if cases.record(entry("operator.act", "requested")).is_err() {
        return crate::answers::busy();
    }
    let response = next.run(request).await;
    cases.note(entry("operator.act", "completed").with("status", response.status().as_u16()));
    response
}

/// The user name of a Basic `Authorization` header, never the password.
fn basic_user(headers: &axum::http::HeaderMap) -> Option<String> {
    use base64::Engine as _;
    let encoded = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    text.split_once(':').map(|(user, _)| user.to_string())
}

/// Refuses every request that carries no configured credential, before any handler sees it, and
/// a peer address that has failed too often before its credential is compared.
async fn authenticate(
    State(cases): State<Arc<OperatorCases>>,
    request: Request,
    next: Next,
) -> Response {
    let peer = crate::answers::peer_ip(&request);
    let operator = basic_user(request.headers());
    match cases.sign_in(peer, operator, request.headers()).await {
        SignIn::Admitted => next.run(request).await,
        SignIn::Throttled(wait) => crate::answers::throttled(wait),
        SignIn::Busy => crate::answers::busy(),
        // The challenge is what turns this into a browser prompt rather than a dead end.
        SignIn::Refused(challenge) => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, challenge)],
            "the REST API and the UI require authentication",
        )
            .into_response(),
    }
}

pub fn router(state: Arc<OperatorCases>) -> Router {
    let (api, document) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(agents))
        .routes(routes!(reported_attributes))
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
        .routes(routes!(list_packages))
        .routes(routes!(
            get_package_set,
            put_package_set,
            delete_package_set
        ))
        // The one route that legitimately carries a program: the framework's 2 MiB default would
        // refuse every real agent binary, so the upload streams past it and the handler bounds it
        // by `max_package_size_bytes` instead (DEV-six-crates-tokio-axum). No other route is unbounded.
        .routes(routes!(put_package_entry, delete_package_entry).layer(DefaultBodyLimit::disable()))
        .routes(routes!(put_entry_signature, delete_entry_signature))
        .routes(routes!(list_package_floors))
        .routes(routes!(put_package_floor, delete_package_floor))
        .routes(routes!(list_deployments))
        .routes(routes!(get_deployment, put_deployment, delete_deployment))
        .routes(routes!(put_deployment_selector))
        .routes(routes!(put_deployment_package, delete_deployment_package))
        .routes(routes!(
            put_deployment_configuration,
            delete_deployment_configuration
        ))
        .routes(routes!(rollout_deployment))
        .routes(routes!(
            get_enrolment_window,
            open_enrolment_window,
            close_enrolment_window
        ))
        .routes(routes!(list_enrolments))
        .routes(routes!(approve_enrolment))
        .routes(routes!(reject_enrolment))
        .routes(routes!(
            list_bootstrap_certificates,
            issue_bootstrap_certificate
        ))
        .routes(routes!(download_bootstrap_p12))
        .routes(routes!(delete_bootstrap_certificate))
        .routes(routes!(list_certificates))
        .routes(routes!(list_revocations, revoke))
        .routes(routes!(lift_revocation))
        .routes(routes!(list_hosts))
        .routes(routes!(set_gateway))
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
        // The interactive API docs (SEC-tls13-bounded-input-chosen-planes): a Redoc page rendering /api/v1/openapi.json, with
        // Redoc vendored and served from this same origin so the docs work offline.
        .route("/api/v1/docs", get(docs))
        .route("/api/v1/docs/redoc.js", get(redoc_js))
        .route("/", get(index))
        .route("/ui.css", get(ui_css))
        .route("/ui-carbon.css", get(ui_carbon_css))
        .route("/ui-quiet.css", get(ui_quiet_css))
        .route("/shiki.js", get(shiki_js))
        .with_state(state.clone());
    // Inside the guard, so an act is recorded only once it is authenticated, and named by the
    // operator who made it (SEC-append-only-audit-record §1).
    let router = if state.audits() {
        router.layer(middleware::from_fn_with_state(state.clone(), record_act))
    } else {
        router
    };
    // Outside every route, so the guard covers every route on this listener — including the
    // UI and the API docs, which are as much of the plane as `/api/v1` is (SEC-admission-by-client-certificate).
    let router = if state.guarded() {
        router.layer(middleware::from_fn_with_state(state.clone(), authenticate))
    } else {
        router
    };
    // Outermost, so a refusal of the guard carries the headers too.
    router.layer(middleware::from_fn(security_headers))
}

/// The policy every page of the plane is held to. Scripts come from this origin, or are the
/// bundled UI's inline ones named by their hash; styles come from this origin, as the UI's
/// stylesheet does, or are inline, as the UI's `style` attributes and the API docs' rendered
/// styles are. Nothing may frame the plane.
static CONTENT_SECURITY_POLICY: std::sync::LazyLock<header::HeaderValue> =
    std::sync::LazyLock::new(|| {
        use base64::Engine as _;
        use sha2::Digest as _;
        let page = include_str!("../static/index.html");
        // Each `<script …>` without a `src`, whatever its other attributes.
        let hashes: String = page
            .split("<script")
            .skip(1)
            .filter_map(|chunk| chunk.split_once('>'))
            .filter(|(attributes, _)| !attributes.contains("src="))
            .filter_map(|(_, rest)| rest.split_once("</script>"))
            .map(|(script, _)| {
                let digest = sha2::Sha256::digest(script.as_bytes());
                format!(
                    " 'sha256-{}'",
                    base64::engine::general_purpose::STANDARD.encode(digest)
                )
            })
            .collect();
        header::HeaderValue::from_str(&format!(
            "default-src 'self'; script-src 'self'{hashes}; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; \
             form-action 'self'; frame-ancestors 'none'"
        ))
        .expect("the policy is a header value")
    });

/// The response headers of the Operator plane: its policy, no framing, no content sniffing, no
/// referrer, no cached `/api/v1` answer, and HSTS. The plane serves TLS on every address
/// (SEC-tls13-bounded-input-chosen-planes), so a browser that once reached it by name never tries `http://` for it again,
/// where an active attacker could answer and ask for the Basic credentials; browsers ignore the
/// header for an IP address. It names no subdomains: what else runs on the plane's domain is not
/// the plane's to bind.
async fn security_headers(request: Request, next: Next) -> Response {
    let api = request.uri().path().starts_with("/api/v1/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        CONTENT_SECURITY_POLICY.clone(),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        header::HeaderValue::from_static("max-age=31536000"),
    );
    if api {
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
    }
    response
}

/// The bundled UI: one embedded page, no frontend toolchain (DEV-six-crates-tokio-axum).
async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

/// The bundled UI's default stylesheet, kept apart from the page so its look can be replaced
/// without touching the markup or the scripts.
async fn ui_css() -> Response {
    stylesheet(include_str!("../static/ui.css"))
}

/// The bundled UI's second look, after IBM's Carbon Design System as Würth Phoenix's NetEye wears it.
async fn ui_carbon_css() -> Response {
    stylesheet(include_str!("../static/ui-carbon.css"))
}

/// The bundled UI's third look, the quiet one, where colour says only state and selection.
async fn ui_quiet_css() -> Response {
    stylesheet(include_str!("../static/ui-quiet.css"))
}

/// The vendored Shiki bundle the UI colours an Agent's effective configuration with (SRV-ui-colours-with-shiki),
/// served from this origin like the stylesheets, so `script-src 'self'` admits it.
async fn shiki_js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/shiki.js"),
    )
        .into_response()
}

fn stylesheet(css: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], css).into_response()
}

/// The API docs page: renders the OpenAPI document with the vendored Redoc bundle (SEC-tls13-bounded-input-chosen-planes).
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

/// The Instance UID an Agent route names, or the `400` that says it is not one.
fn parse_instance_uid(text: &str) -> Result<InstanceUid, Refusal> {
    InstanceUid::parse(text).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!("{text:?} is not an Instance UID"),
        )
    })
}

/// One Agent as the fleet view now shows it — what a write to that Agent answers with. `204` when
/// it was forgotten between the write and the read: the write stands, and the Agent is simply no
/// longer in the view to return. `500` when the view cannot be read: the write stands then too,
/// and the answer says so, so that a client does not repeat the act.
fn agent_response(state: &OperatorCases, uid: &InstanceUid) -> Response {
    match state.agent_view(uid) {
        Ok(Some(view)) => Json(AgentResponse::from(view)).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the change was written, but the Agent's view cannot be read: {e}"),
        ),
    }
}

/// A CSRF guard for the body-less `POST` routes (`restart`, the rollout acts). Those are CORS
/// "simple requests": a cross-origin page can fire them without the preflight the Server would
/// have to answer, so nothing else stops a victim operator's browser from being made to send one.
///
/// Fetch Metadata closes it. A browser sends `Sec-Fetch-Site` on every request and forbids page
/// scripts from setting it, so a value other than `same-origin` (the bundled UI) or `none` (a
/// user-initiated load) marks a cross-site caller, which is refused. A non-browser client — `curl`,
/// a portal — sends no such header and is unaffected, which is why this needs no token and no change
/// to any API client. It is not authentication (that is a separate decision, SEC-admission-by-client-certificate); it only
/// keeps a browser from being turned into a confused deputy.
///
/// A browser without Fetch Metadata still sends `Origin` on a `POST`, so without `Sec-Fetch-Site`
/// an `Origin` other than the plane's own (`null` included) is refused the same way.
struct SameOrigin;

/// Whether `origin` names the authority this request was sent to — the `Host` header, or the
/// request target's authority over HTTP/2. The scheme is not compared: the plane serves one.
fn is_own_origin(parts: &axum::http::request::Parts, origin: &str) -> bool {
    let own = parts.uri.authority().map(|a| a.as_str()).or_else(|| {
        parts
            .headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
    });
    match (own, origin.split_once("://")) {
        (Some(own), Some((_, authority))) => own.eq_ignore_ascii_case(authority),
        _ => false,
    }
}

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
            Some(_) => Ok(SameOrigin),
            None => match parts.headers.get(header::ORIGIN) {
                Some(origin) => match origin.to_str() {
                    Ok(origin) if is_own_origin(parts, origin) => Ok(SameOrigin),
                    _ => Err(error(
                        StatusCode::FORBIDDEN,
                        format!("cross-origin request refused (Origin: {origin:?})"),
                    )),
                },
                None => Ok(SameOrigin),
            },
        }
    }
}

/// One Agent as the REST API and the UI see it.
#[derive(Serialize, ToSchema)]
#[schema(as = AgentView)]
struct AgentResponse {
    instance_uid: String,
    /// The Agent *type* — the Baseline's "reverse FQDN that uniquely identifies the Agent type"
    /// (CLT-what-an-agent-reports). For a Supervisor-backed Agent this is the type its Supervisor states, never one
    /// its Managed Process reports, so every Agent of one configured type reports the same value.
    /// It answers "what is this", never "which one is this": that is
    /// [`service_instance_name`](Self::service_instance_name).
    service_name: String,
    /// The operator's name for this Agent — the `[[supervisor]]` block's `name` (CLT-what-an-agent-reports). Empty
    /// for a foreign OpAMP client that reports no `service.instance.name`, which is why the UI
    /// falls back through the type to the UID rather than showing a blank row.
    service_instance_name: String,
    /// The release the Agent reports — `MAJOR.MINOR.PATCH`, with the pre-release when it is not a
    /// release build (OPS-cargo-decides-product-version). This is what belongs in a column headed "Version"; the commit the
    /// build came from is [`service_build`](Self::service_build). A reported value that is not a
    /// version at all is passed through unchanged, since a Foreign Agent numbers itself however it
    /// likes.
    service_version: String,
    /// Exactly what the Agent reported, commit metadata and all — the answer to "which build is on
    /// that host", which is a question a fleet exists to answer (OPS-cargo-decides-product-version).
    service_build: String,
    /// The reported `os.description` (e.g. "Ubuntu 24.04.2 LTS"), falling back to `os.type`.
    os: String,
    /// The platform this Agent is offered packages for, `os.type`-`host.arch` as the offer reads
    /// and folds them (`linux-amd64`), an identifying copy standing in for a non-identifying one;
    /// empty when it reports neither, so it is offered no package. The fleet search finds the
    /// Agent by this value.
    platform: String,
    /// Every reported identifying attribute — what a Selector can match on (SRV-configurations-compose-one-map).
    identifying_attributes: BTreeMap<String, String>,
    /// Every reported non-identifying attribute — Selectors match these too.
    non_identifying_attributes: BTreeMap<String, String>,
    /// The Configurations a press of the Deployment claiming this Agent would release to it — the
    /// ones it holds that fit this Agent's type (SRV-deployments-and-pinned-rollouts), in name order. Never what it runs;
    /// that is [`assigned_configurations`](Self::assigned_configurations).
    matched_configurations: Vec<String>,
    /// The Configurations rolled out to this Agent (SRV-deployments-and-pinned-rollouts §6), in name order — what its
    /// offer is composed from.
    assigned_configurations: Vec<String>,
    /// The Deployment that claims this Agent **now** — whose Selector matches it — or empty when
    /// none does.
    ///
    /// This is not [`assigned_deployment`](Self::assigned_deployment), and the difference is what
    /// tells four states apart that would otherwise look alike (PKG-signed-packages-and-delivery §27). Empty here
    /// with no conflict means the host is in **no channel**: label it, or give it a `channel`
    /// attribute. Set here with nothing assigned and nothing pending means the channel holds
    /// nothing this Agent can take — no Package for its type, or none for its platform. The
    /// operator's next move differs in each case, which is why the Server says which one it is
    /// rather than showing an empty row three ways.
    deployment: String,
    /// The Deployment this Agent's release went **through** (SRV-deployments-and-pinned-rollouts §6), or empty when
    /// nothing has been rolled out to it. Pinned as of that act, so it may name a channel that no
    /// longer claims this Agent.
    assigned_deployment: String,
    /// The Package rolled out to this Agent (SRV-deployments-and-pinned-rollouts), as `<agent type>@<version>`, or empty.
    ///
    /// It is pinned as of the act that released it: re-aiming its Deployment afterwards, or
    /// putting a newer Package in that channel, changes what is *proposed* and never what this Agent
    /// was already given.
    assigned_package: String,
    /// The Configurations waiting for a rollout act toward this Agent (SRV-deployments-and-pinned-rollouts §8):
    /// one its Deployment holds and it is not assigned (`change: "new"`), one whose
    /// saved revision differs from the assigned one (`change: "update"`), one assigned that the
    /// next press drops from a release that keeps something else, applying the smaller map
    /// (`change: "remove"`), or one assigned while the Deployment releases nothing to this Agent
    /// any more, so the next press leaves it with no assignment and it keeps running what it runs
    /// (`change: "kept"`). The Server never acts on this by itself.
    pending_configurations: Vec<PendingConfigurationResponse>,
    /// The Package waiting for a rollout act toward this Agent (SRV-deployments-and-pinned-rollouts §8) — at most one:
    /// the candidate of the Deployment that claims it, when that is not what it is assigned, or
    /// the assigned one the next press drops (`change: "remove"`, or `"kept"` as for a
    /// Configuration).
    pending_packages: Vec<PendingPackageResponse>,
    /// The Deployment the next press releases through, when the assignment names another one or
    /// none (SRV-deployments-and-pinned-rollouts §9); empty otherwise. Set alone, it says the press moves only the
    /// assignment over to the claiming Deployment, with the same content.
    pending_deployment: String,
    /// Hex hash of the composed configuration this Agent should run; empty when it is assigned
    /// nothing.
    desired_hash: String,
    /// The Capability Set this Agent declared, as capability names from the Baseline's
    /// `AgentCapabilities` (see docs/CONFORMANCE.md).
    capabilities: Vec<String>,
    /// The Agent's available components (top-level names, sorted); empty until reported.
    available_components: Vec<String>,
    /// The Agent's package installations (PKG-signed-packages-and-delivery), in name order; empty until reported.
    packages: Vec<PackageStatusResponse>,
    /// Why this Agent is proposed nothing — more than one Deployment claims it, and an Agent
    /// belongs to at most one (PKG-signed-packages-and-delivery §28). The message names every Deployment in the way.
    /// Absent when at most one claims it.
    #[serde(skip_serializing_if = "Option::is_none")]
    package_conflict: Option<String>,
    /// What the Agent said about the *offer* rather than about a package it holds — an offer it
    /// refuses outright has no package status to carry the reason, and the Client's own Agent
    /// refusing a package it was not configured to take (CLT-self-update-signed-package) is exactly that case. Empty
    /// when the Agent has nothing to complain about.
    package_error: String,
    transport: String,
    connected: bool,
    healthy: bool,
    health_status: String,
    /// Why the Agent is unhealthy — `ComponentHealth.last_error`, which the Baseline says SHOULD
    /// be set when `healthy` is false. Empty when the Agent is healthy or gave no reason.
    health_error: String,
    effective_config: String,
    remote_config_status: String,
    remote_config_error: String,
    in_sync: bool,
    sequence_num: u64,
    last_seen_ms: u64,
    /// Nothing has been heard from this Agent for longer than its staleness budget (SRV-fleet-record-agents-hosts).
    ///
    /// Beside [`connected`](Self::connected), never instead of it: that one says a connection
    /// carrying this Agent is open — behind a Gateway, the *Gateway's* — and this one says whether
    /// the Agent itself is still talking. `connected: true, stale: true` is precisely the gatewayed
    /// case, and precisely what an operator needs to be told.
    ///
    /// Only an Agent declaring `ReportsHeartbeat` can be stale: that capability is the promise that
    /// makes silence mean something. Derived on read, never stored.
    stale: bool,
    /// The operator's labels on this Agent (SRV-fleet-record-agents-hosts) — matched by Selectors exactly like a
    /// reported attribute, but set here rather than in `supervisor.toml` on the host, so moving a host
    /// between rollout channels is an API call instead of an edit and a restart.
    labels: BTreeMap<String, String>,
    /// Labels this Agent's own reports shadow: set, matching nothing, and therefore doing nothing.
    ///
    /// Reported attributes always win (SRV-fleet-record-agents-hosts) — they decide which artifact fits this machine.
    /// A collision is refused when the label is set, so this fills only when an Agent *starts*
    /// reporting a key that was labelled earlier. Shown rather than dropped in silence.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    shadowed_labels: Vec<String>,
    /// Labels stored with a key in `fleet.` or `supervisor.`, which no label may speak in
    /// (SRV-fleet-record-agents-hosts §20): ignored by every Selector and by
    /// package matching, and inherited by no Agent. Listed rather than dropped in silence; the next
    /// label set put without them removes them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ignored_labels: Vec<String>,
    /// The labels of this Agent's Client Agent, which hold for it below its own (SRV-fleet-record-agents-hosts §19);
    /// empty for the Client Agent itself, for an Agent whose Client the Server cannot
    /// tell, and for an ambiguous one.
    inherited_labels: BTreeMap<String, String>,
    /// The Instance UID of this Agent's Client Agent — its own, for the Client Agent — or empty
    /// when the Server cannot tell.
    client_agent: String,
    /// The group this Agent's Client is recognised in holds more than one Client Agent, so it
    /// inherits nothing (SRV-fleet-record-agents-hosts §19).
    ambiguous: bool,
    /// The Host this Agent last arrived over (SRV-fleet-record-agents-hosts §16), or empty. Behind a Gateway, the
    /// Gateway's.
    host: String,
    /// On PostgreSQL, the instance id of the Server instance that holds this Agent under a live
    /// lease; empty without one, and always on the filesystem store.
    holder: String,
    /// The host name that instance's lease records; empty when `holder` is.
    holder_host: String,
    /// The `fleet.host.*` values Selectors match on this Agent, from the certificate of the Host it
    /// arrived over directly (SRV-fleet-record-agents-hosts §18); empty behind a Gateway.
    host_attributes: BTreeMap<String, String>,
    /// Reported attributes in the Server's `fleet.` space, left out of what Selectors match
    /// (SRV-fleet-record-agents-hosts §20).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ignored_attributes: Vec<String>,
}

impl From<fleet::AgentView> for AgentResponse {
    fn from(view: fleet::AgentView) -> Self {
        AgentResponse {
            instance_uid: view.instance_uid,
            service_name: view.service_name,
            service_instance_name: view.service_instance_name,
            service_version: view.service_version,
            service_build: view.service_build,
            os: view.os,
            platform: view.platform,
            identifying_attributes: view.identifying_attributes,
            non_identifying_attributes: view.non_identifying_attributes,
            matched_configurations: view.matched_configurations,
            assigned_configurations: view.assigned_configurations,
            deployment: view.deployment,
            assigned_deployment: view.assigned_deployment,
            assigned_package: view.assigned_package,
            pending_configurations: view
                .pending_configurations
                .into_iter()
                .map(Into::into)
                .collect(),
            pending_packages: view.pending_packages.into_iter().map(Into::into).collect(),
            pending_deployment: view.pending_deployment,
            desired_hash: view.desired_hash,
            capabilities: view.capabilities,
            available_components: view.available_components,
            packages: view.packages.into_iter().map(Into::into).collect(),
            package_conflict: view.package_conflict,
            package_error: view.package_error,
            transport: view.transport,
            connected: view.connected,
            healthy: view.healthy,
            health_status: view.health_status,
            health_error: view.health_error,
            effective_config: view.effective_config,
            remote_config_status: view.remote_config_status,
            remote_config_error: view.remote_config_error,
            in_sync: view.in_sync,
            sequence_num: view.sequence_num,
            last_seen_ms: view.last_seen_ms,
            stale: view.stale,
            labels: view.labels,
            shadowed_labels: view.shadowed_labels,
            ignored_labels: view.ignored_labels,
            inherited_labels: view.inherited_labels,
            client_agent: view.client_agent,
            ambiguous: view.ambiguous,
            host: view.host,
            holder: view.holder,
            holder_host: view.holder_host,
            host_attributes: view.host_attributes,
            ignored_attributes: view.ignored_attributes,
        }
    }
}

/// One Configuration waiting for a rollout act toward one Agent (SRV-deployments-and-pinned-rollouts).
#[derive(Serialize, ToSchema)]
#[schema(as = PendingConfigurationView)]
struct PendingConfigurationResponse {
    name: String,
    /// `new` — held by the claiming Deployment and not yet assigned; `update` — the saved
    /// revision differs from the assigned one; `remove` — assigned, and dropped by the next press,
    /// which applies the smaller map; `kept` — assigned, and the next press leaves the Agent with
    /// no assignment, so it keeps running it.
    change: String,
}

impl From<fleet::PendingConfigurationView> for PendingConfigurationResponse {
    fn from(view: fleet::PendingConfigurationView) -> Self {
        PendingConfigurationResponse {
            name: view.name,
            change: view.change,
        }
    }
}

/// The Package waiting for a rollout act toward one Agent (SRV-deployments-and-pinned-rollouts).
#[derive(Serialize, ToSchema)]
#[schema(as = PendingPackageView)]
struct PendingPackageResponse {
    /// The Deployment that would release it — the channel this Agent belongs to.
    deployment: String,
    /// The Agent type this Package is built for — its identity, and its wire name.
    agent_type: String,
    version: String,
    /// What an operator reads: the Agent type and the version together.
    display_name: String,
    /// `new` — nothing is assigned for this Agent type; `update` — another version is; `remove` —
    /// the assigned one, which the next press drops; `kept` — the assigned one, while the next
    /// press leaves the Agent with no assignment.
    change: String,
}

impl From<fleet::PendingPackageView> for PendingPackageResponse {
    fn from(view: fleet::PendingPackageView) -> Self {
        PendingPackageResponse {
            deployment: view.deployment,
            agent_type: view.agent_type,
            version: view.version,
            display_name: view.display_name,
            change: view.change,
        }
    }
}

/// One package's installation state as the REST API and UI see it (PKG-signed-packages-and-delivery).
#[derive(Serialize, ToSchema)]
#[schema(as = PackageStatusView)]
struct PackageStatusResponse {
    name: String,
    /// The version the Agent has installed; empty if it has none.
    version: String,
    /// `Downloading`, `Installing`, `Installed`, `InstallPending`, or `InstallFailed`.
    status: String,
    /// The failure reason when `status` is `InstallFailed`.
    error: String,
    /// How far the artifact download has got, as a percentage. Present only while `Downloading`,
    /// and only when the download source stated a size.
    #[serde(skip_serializing_if = "Option::is_none")]
    download_percent: Option<f64>,
    /// The download's current rate in bytes per second. Present only while `Downloading`.
    #[serde(skip_serializing_if = "Option::is_none")]
    download_bytes_per_second: Option<f64>,
}

impl From<fleet::PackageStatusView> for PackageStatusResponse {
    fn from(view: fleet::PackageStatusView) -> Self {
        PackageStatusResponse {
            name: view.name,
            version: view.version,
            status: view.status,
            error: view.error,
            download_percent: view.download_percent,
            download_bytes_per_second: view.download_bytes_per_second,
        }
    }
}

/// The parameters a list that pages takes, besides its filters
/// (API-lists-paged-by-cursor §3 and §4).
const LIMIT_DOC: &str = "How many entries a page holds, 1 to 1000; a larger value is answered as \
                         1000. Without limit and cursor the whole list is answered while it holds \
                         at most 10000 entries after its filters, and a longer one is a 400 \
                         naming limit.";

/// The fleet, a page at a time where `limit` asks for one: every Agent the Server knows that
/// matches the filters, in the order of `sort`, its reported attributes, and the Configurations
/// currently matching it.
#[utoipa::path(
    get,
    path = "/api/v1/agents",
    tag = "fleet",
    params(
        ("limit" = Option<u32>, Query, description = LIMIT_DOC),
        ("cursor" = Option<String>, Query, description = "The position the page starts after, from the Link of the page before; opaque, passed back unchanged with the same sort and filters"),
        ("q" = Option<String>, Query, description = "A case-insensitive substring of what the UI's filter searches: the names, the status, the OS, the platform, the versions, the attributes and labels, the capabilities and the package statuses"),
        ("selector" = Option<String>, Query, description = "key=value pairs separated by commas, matched as a Deployment's Selector matches an Agent: the Host's values, what it reported, its labels and its Client Agent's"),
        ("connected" = Option<bool>, Query, description = "Only the Agents a connection carries, or only those it does not"),
        ("stale" = Option<bool>, Query, description = "Only the stale Agents, or only those that are not"),
        ("service_name" = Option<String>, Query, description = "Only the Agents of this type"),
        ("holder" = Option<String>, Query, description = "On PostgreSQL: only the Agents the instance of this id holds, or with none those no instance with a live lease holds; on the filesystem store a 400"),
        ("sort" = Option<String>, Query, description = "name (the default), connected, health, config, version, os, seq or seen; ties are broken by Instance UID"),
        ("order" = Option<String>, Query, description = "asc (the default) or desc")
    ),
    responses(
        (status = 200, description = "The Agents that match, a page of them where limit asks for one", body = [AgentResponse],
            headers(
                ("X-Total-Count" = u64, description = "How many Agents match the filters"),
                ("Link" = String, description = "The next page, rel=\"next\"; absent on the last")
            )),
        (status = 400, description = "A parameter this route does not take, one that is not valid, a cursor that does not parse or was made for another sort or filters, holder on the filesystem store, or more than 10000 Agents without limit", body = ErrorBody),
        (status = 500, description = "The fleet cannot be read from the store", body = ErrorBody)
    )
)]
async fn agents(State(state): State<Arc<OperatorCases>>, RawQuery(raw): RawQuery) -> Response {
    let asked = match Asked::parse(
        raw.as_deref(),
        &[
            "q",
            "selector",
            "connected",
            "stale",
            "service_name",
            "holder",
            "sort",
            "order",
        ],
    ) {
        Ok(asked) => asked,
        Err((status, message)) => return error(status, message),
    };
    let query = match agent_query(&state, &asked) {
        Ok(query) => query,
        Err((status, message)) => return error(status, message),
    };
    // A whole answer is counted first, so a list past the bound is refused without being read.
    if asked.limit.is_none() {
        match state.list_agents(&fleet::AgentQuery {
            limit: 0,
            ..query.clone()
        }) {
            Ok(counted) if counted.total > crate::listing::WHOLE_BOUND => {
                return asked.answer(
                    "/api/v1/agents",
                    Vec::<AgentResponse>::new(),
                    counted.total,
                    None,
                )
            }
            Ok(_) => {}
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
        }
    }
    match state.list_agents(&query) {
        Ok(listing) => asked.answer(
            "/api/v1/agents",
            listing
                .agents
                .into_iter()
                .map(AgentResponse::from)
                .collect(),
            listing.total,
            listing.next,
        ),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// What a request of the list of Agents asks, or the `400` that says what is wrong with it.
fn agent_query(state: &OperatorCases, asked: &Asked) -> Result<fleet::AgentQuery, Refusal> {
    use crate::listing::{AgentFilter, AgentSort, HolderFilter};
    let sort = match asked.params.get("sort") {
        None => AgentSort::Name,
        Some(name) => AgentSort::parse(name).ok_or_else(|| {
            paging::refused(format!(
                "sort: {name:?} is none of name, connected, health, config, version, os, seq, seen"
            ))
        })?,
    };
    let descending = match asked.params.get("order").map(String::as_str) {
        None | Some("asc") => false,
        Some("desc") => true,
        Some(other) => {
            return Err(paging::refused(format!(
                "order: {other:?} is neither asc nor desc"
            )))
        }
    };
    let selector = match asked.params.get("selector") {
        None => BTreeMap::new(),
        Some(text) => crate::listing::parse_selector(text).ok_or_else(|| {
            paging::refused(format!(
                "selector: {text:?} is not key=value pairs separated by commas"
            ))
        })?,
    };
    let holder = match asked.params.get("holder") {
        None => None,
        Some(_) if !state.keeps_holders() => {
            return Err(paging::refused(
                "holder: on the filesystem store there is one Server and no holder",
            ))
        }
        Some(id) if id == "none" => Some(HolderFilter::None),
        Some(id) if id.is_empty() => {
            return Err(paging::refused("holder: an instance id, or none"))
        }
        Some(id) => Some(HolderFilter::Instance(id.clone())),
    };
    let after = asked.after_keyed(matches!(
        sort,
        AgentSort::Name | AgentSort::Config | AgentSort::Version | AgentSort::Os
    ))?;
    Ok(fleet::AgentQuery {
        filter: AgentFilter {
            q: asked.q(),
            selector,
            connected: asked.flag("connected")?,
            stale: asked.flag("stale")?,
            service_name: asked.params.get("service_name").cloned(),
            holder,
            arrived_over: None,
        },
        sort,
        descending,
        after,
        limit: asked.reading(),
    })
}

/// What the fleet reports, for suggestions
/// (API-lists-paged-by-cursor §7).
#[derive(Serialize, ToSchema)]
struct ReportedValuesResponse {
    /// The distinct `key=value` pairs the Agents report that a Selector can match, sorted, at most
    /// 1000.
    attributes: Vec<String>,
    /// The distinct `service_name` values the Agents report, sorted, at most 1000.
    service_names: Vec<String>,
}

/// The distinct `key=value` pairs and Agent types the fleet reports: what the Deployment form
/// suggests for a Selector and the Configuration form for a type.
#[utoipa::path(
    get,
    path = "/api/v1/agents/attributes",
    tag = "fleet",
    responses(
        (status = 200, description = "The distinct pairs and types, each sorted, at most 1000 of each", body = ReportedValuesResponse),
        (status = 500, description = "The fleet cannot be read from the store", body = ErrorBody)
    )
)]
async fn reported_attributes(State(state): State<Arc<OperatorCases>>) -> Response {
    match state.reported_values() {
        Ok(values) => Json(ReportedValuesResponse {
            attributes: values.pairs,
            service_names: values.service_names,
        })
        .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
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
        (status = 409, description = "The Agent does not declare AcceptsRestartCommand", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn restart_agent(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Path(instance_uid): Path<String>,
) -> Response {
    let uid = match parse_instance_uid(&instance_uid) {
        Ok(uid) => uid,
        Err((status, why)) => return error(status, why),
    };
    match state.request_restart(&uid) {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(RestartError::UnknownAgent) => error(StatusCode::NOT_FOUND, format!("no agent {uid}")),
        Err(RestartError::NoCapability) => error(
            StatusCode::CONFLICT,
            format!("agent {uid} does not declare AcceptsRestartCommand"),
        ),
        Err(RestartError::Storage(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// The labels to put on an Agent (SRV-fleet-record-agents-hosts). The whole set, replacing what was there.
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
    description = "Replace this Agent's labels (SRV-fleet-record-agents-hosts). A label is an operator's \
                   key/value pair that joins what a Deployment's Selector matches, so a rollout \
                   channel is a Server-side decision instead of an edit to supervisor.toml on the \
                   host. An empty map clears them. The labels of a Client's own Agent hold for the \
                   Client's other Agents, below their own. Labels are part of the Agent's record: \
                   they follow a re-key, forgetting the Agent removes them, and they never travel \
                   to the Agent. A key the Agent already reports is refused, because reported \
                   attributes decide which artifact fits the machine and a label must never be \
                   able to overrule them; a key starting `fleet.` is refused, because the Server \
                   fills those from the Host's certificate.",
    responses(
        (status = 200, description = "The Agent, with its new labels", body = AgentResponse),
        (status = 400, description = "Malformed Instance UID, an unusable label, or a key starting `fleet.`", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
        (status = 409, description = "A label restates an attribute the Agent reports", body = ErrorBody),
        (status = 500, description = "The labels could not be written, or they were written and the Agent's view cannot be read from the store; the answer says which", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn set_agent_labels(
    State(state): State<Arc<OperatorCases>>,
    Path(instance_uid): Path<String>,
    Json(body): Json<LabelsBody>,
) -> Response {
    let uid = match parse_instance_uid(&instance_uid) {
        Ok(uid) => uid,
        Err((status, why)) => return error(status, why),
    };
    match state.set_labels(&uid, body.labels) {
        Ok(()) => agent_response(&state, &uid),
        Err(LabelError::UnknownAgent) => error(StatusCode::NOT_FOUND, format!("no agent {uid}")),
        Err(LabelError::RestatesReported(key)) => error(
            StatusCode::CONFLICT,
            format!(
                "agent {uid} reports {key:?} itself, and a label may not restate it — a reported \
                 attribute decides which artifact fits this machine, so it wins. Change it where it \
                 comes from, in that host's supervisor.toml, or label it under another key"
            ),
        ),
        Err(LabelError::Invalid(e)) => error(StatusCode::BAD_REQUEST, e),
        Err(LabelError::Storage(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// What a per-Agent rollout act names (SRV-deployments-and-pinned-rollouts §9): nothing, or the Deployment that claims
/// the Agent. An empty body releases that Deployment just the same.
#[derive(Deserialize, ToSchema, Default)]
#[serde(deny_unknown_fields)]
struct AgentRolloutSpec {
    /// The Deployment the operator means — the one that claims this Agent; its release replaces
    /// the Agent's assignment, pinned as of this press.
    ///
    /// Naming another is refused, and so is any act while no Deployment or a second one claims the
    /// Agent: honouring a name over a conflict would sidestep the conflict for good instead of
    /// fixing it, and make this path the way into a state the fleet-wide act forbids.
    #[serde(default)]
    deployment: Option<String>,
}

/// Rolls the Deployment that claims **this Agent** out to it (SRV-deployments-and-pinned-rollouts §9): its Package for
/// the Agent's type, if that is a candidate, and every Configuration it holds that fits the
/// Agent's type, at its saved revision — replacing the Agent's assignment. The operator's press is
/// the only thing that distributes: saving, Selector edits and label moves all merely change what
/// is *proposed* here.
#[utoipa::path(
    post,
    path = "/api/v1/agents/{instance_uid}/rollout",
    tag = "fleet",
    params(("instance_uid" = String, Path, description = "The Agent's Instance UID")),
    request_body(content = AgentRolloutSpec, description = "Empty, or the claiming Deployment's name"),
    responses(
        (status = 200, description = "Rolled out; the Agent with its new assignment", body = AgentResponse),
        (status = 400, description = "Malformed Instance UID or body", body = ErrorBody),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
        (status = 409, description = "The Agent is in no Deployment, in a conflict, or in another one than named; or the Deployment holds nothing, or a Package with an unsigned entry", body = ErrorBody),
        (status = 500, description = "Nothing was released, as the Agent or the fleet could not be read or the release could not be written; or the release was written and the Agent's view cannot be read, which the answer says", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn rollout_to_agent(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Path(instance_uid): Path<String>,
    body: Option<Json<AgentRolloutSpec>>,
) -> Response {
    let uid = match parse_instance_uid(&instance_uid) {
        Ok(uid) => uid,
        Err((status, why)) => return error(status, why),
    };
    let spec = body.map(|Json(spec)| spec).unwrap_or_default();
    match state.rollout_to_agent(&uid, spec.deployment.as_deref()) {
        Ok(()) => agent_response(&state, &uid),
        Err(e) => rollout_error(e),
    }
}

/// Forgets what the Server knows about an Agent, dropping its row from the fleet view.
///
/// Reaches no host: nothing is stopped, nothing is uninstalled, and no certificate is revoked —
/// a certificate is the host's, not the Agent's. Its `instance_uid` is released from the Host it
/// was bound to (SRV-fleet-record-agents-hosts §10). A Client that is still running reappears on its next report.
#[utoipa::path(
    delete,
    path = "/api/v1/agents/{instance_uid}",
    tag = "fleet",
    params(("instance_uid" = String, Path, description = "The Agent's Instance UID")),
    description = "Forget this Agent: the Server drops what it knows and the row leaves the fleet \
                   view (SRV-fleet-record-agents-hosts). Nothing happens on the host — no process is stopped, nothing \
                   is uninstalled, and no certificate is revoked, because a certificate here \
                   proves fleet membership and its host rather than one Agent's identity. The \
                   Instance UID is released from the Host it was bound to (SRV-fleet-record-agents-hosts), so its \
                   next report binds it to the host whose certificate carries that report. A \
                   Client still configured for this Server therefore comes back on its next \
                   report. Refused while the \
                   Agent is still reporting, since forgetting it would have its configuration \
                   offered again and a managed process restarted with it. A session that still \
                   carries an Agent that went quiet is ended, on whichever instance carries it.",
    responses(
        (status = 204, description = "The Agent is forgotten"),
        (status = 400, description = "Malformed Instance UID", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
        (status = 409, description = "The Agent is still reporting", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn forget_agent(
    State(state): State<Arc<OperatorCases>>,
    Path(instance_uid): Path<String>,
) -> Response {
    let uid = match parse_instance_uid(&instance_uid) {
        Ok(uid) => uid,
        Err((status, why)) => return error(status, why),
    };
    match state.forget_agent(&uid) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(ForgetError::UnknownAgent) => error(StatusCode::NOT_FOUND, format!("no agent {uid}")),
        Err(ForgetError::StillReporting) => error(
            StatusCode::CONFLICT,
            format!("agent {uid} is still reporting; stop it or wait for it to go stale"),
        ),
        Err(ForgetError::Storage(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// The writable part of a [`Configuration`] — the `PUT` request body; the name comes from the
/// URL. Writes the saved revision (SRV-deployments-and-pinned-rollouts): saving only saves. A Configuration is content only
/// (SRV-deployments-and-pinned-rollouts): what aims it is the Selector of a Deployment that holds it.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ConfigurationSpec {
    body: String,
    /// See [`Revision::role`]. Absent means top-level configuration.
    #[serde(default)]
    role: String,
    /// The Agent type this Configuration is written for, compared raw against the `service.name`
    /// an Agent reports (SRV-deployments-and-pinned-rollouts §2 and §4). Required: absent or empty is refused `400`.
    /// Read as empty when absent, so the refusal names it.
    #[serde(default)]
    #[schema(required = true)]
    service_name: String,
    /// Read only to refuse it by name: a Configuration has no Selector (SRV-deployments-and-pinned-rollouts §2 and §23).
    #[serde(default)]
    #[schema(ignore)]
    selector: Option<serde_json::Value>,
}

/// One Configuration as the API shows it (SRV-deployments-and-pinned-rollouts): the **saved** revision — what
/// editing operates on and what a rollout act releases — and the Deployments holding it. Which
/// Agents run which pinned revision is a fact about the Agents, answered per Agent by
/// `GET /api/v1/agents`.
#[derive(Serialize, ToSchema)]
struct ConfigurationView {
    name: String,
    body: String,
    /// The Baseline's `AgentConfigObject.role` (SRV-configurations-compose-one-map); absent means top-level configuration.
    #[serde(skip_serializing_if = "String::is_empty")]
    role: String,
    /// The Agent type this Configuration is written for (SRV-deployments-and-pinned-rollouts §2 and §4).
    service_name: String,
    /// The Deployments that hold this Configuration, in name order — where "whom does it reach"
    /// is answered. Empty means it is saved and no rollout act can release it.
    deployments: Vec<String>,
}

impl ConfigurationView {
    fn of(config: Configuration, deployments: Vec<String>) -> Self {
        ConfigurationView {
            name: config.name,
            body: config.saved.body,
            role: config.saved.role,
            service_name: config.saved.service_name,
            deployments,
        }
    }
}

/// One Configuration as the API answers with it, with the Deployments that hold it read now.
fn configuration_view(state: &OperatorCases, config: Configuration) -> ConfigurationView {
    let deployments = state
        .deployments_holding_configurations()
        .remove(&config.name)
        .unwrap_or_default();
    ConfigurationView::of(config, deployments)
}

/// What a Deployment's rollout act did (SRV-deployments-and-pinned-rollouts §9).
#[derive(Serialize, ToSchema)]
struct RolloutOutcome {
    /// How many Agents the act assigned — every Agent the Deployment claims and the press moved.
    /// An Agent that appears later waits for its own act.
    assigned_agents: usize,
}

/// All Configurations, in name order, each with the Deployments that hold it.
#[utoipa::path(
    get,
    path = "/api/v1/configurations",
    tag = "configurations",
    responses((status = 200, description = "Every stored Configuration", body = [ConfigurationView]))
)]
async fn list_configurations(
    State(state): State<Arc<OperatorCases>>,
) -> Json<Vec<ConfigurationView>> {
    let mut holding = state.deployments_holding_configurations();
    Json(
        state
            .configurations()
            .into_iter()
            .map(|config| {
                let deployments = holding.remove(&config.name).unwrap_or_default();
                ConfigurationView::of(config, deployments)
            })
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
    State(state): State<Arc<OperatorCases>>,
    Path(name): Path<String>,
) -> Response {
    match state.configuration(&name) {
        Some(config) => Json(configuration_view(&state, config)).into_response(),
        None => error(StatusCode::NOT_FOUND, format!("no configuration {name:?}")),
    }
}

/// Creates a Configuration or replaces its saved revision. **Saving only saves** (SRV-deployments-and-pinned-rollouts):
/// nothing reaches any Agent — every Agent keeps the revision its assignment pins — until a
/// rollout act of a Deployment holding it releases the saved revision as one snapshot.
///
/// A Configuration has no Selector (SRV-deployments-and-pinned-rollouts): a body naming one is refused, and the Deployment
/// that holds the Configuration is where it is aimed.
#[utoipa::path(
    put,
    path = "/api/v1/configurations/{name}",
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name (CLT-installed-service-and-state grammar)")),
    request_body = ConfigurationSpec,
    responses(
        (status = 200, description = "The stored Configuration — distributed to nobody until a Deployment holding it is rolled out", body = ConfigurationView),
        (status = 400, description = "Invalid name, an empty body, no service_name, or a selector", body = ErrorBody),
        (status = 500, description = "The Configuration could not be persisted", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn put_configuration(
    State(state): State<Arc<OperatorCases>>,
    Path(name): Path<String>,
    Json(spec): Json<ConfigurationSpec>,
) -> Response {
    if spec.selector.is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "a Configuration has no selector — put it into a Deployment and aim the Deployment \
             (PUT /api/v1/deployments/{name}/configurations/{configuration})",
        );
    }
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
        body,
        // Carried verbatim (SRV-configurations-compose-one-map): the values are Agent-type-specific, so the Server never
        // validates one against a vocabulary of its own. Empty is top-level configuration.
        role: spec.role,
        // Compared raw against the reported `service.name` (SRV-deployments-and-pinned-rollouts §4). Required, and not
        // validated against the fleet, because a Configuration may precede its first Agent.
        service_name: spec.service_name,
    };
    if let Err(e) = configs::check_type(&revision) {
        return error(StatusCode::BAD_REQUEST, e);
    }
    match state.save_configuration(&name, revision) {
        Ok(config) => {
            info!(configuration = %config.name, role = %config.saved.role, service_name = %config.saved.service_name, bytes = config.saved.body.len(), "configuration saved from the API");
            Json(configuration_view(&state, config)).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Maps a rollout refusal onto the REST contract (SRV-deployments-and-pinned-rollouts §9).
fn rollout_error(e: RolloutError) -> Response {
    match e {
        RolloutError::UnknownAgent => error(StatusCode::NOT_FOUND, "no such agent"),
        RolloutError::UnknownResource(e) => error(StatusCode::NOT_FOUND, e),
        RolloutError::NotApplicable(e) => error(StatusCode::CONFLICT, e),
        RolloutError::Storage(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Deletes a Configuration — refused with `409` while an Agent's assignment pins it, saying how
/// many (SRV-deployments-and-pinned-rollouts §12). Rolling those Agents out through a Deployment without it, or
/// forgetting them, comes first. A deleted Configuration leaves every Deployment that held it.
#[utoipa::path(
    delete,
    path = "/api/v1/configurations/{name}",
    tag = "configurations",
    params(("name" = String, Path, description = "The Configuration's name")),
    responses(
        (status = 204, description = "Deleted, and taken out of every Deployment that held it"),
        (status = 404, description = "No Configuration of that name", body = ErrorBody),
        (status = 409, description = "An Agent's assignment pins it", body = ErrorBody),
        (status = 500, description = "The Configuration could not be deleted", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn delete_configuration(
    State(state): State<Arc<OperatorCases>>,
    Path(name): Path<String>,
) -> Response {
    match state.delete_configuration(&name) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no configuration {name:?}")),
        Err(DeleteError::Pinned(e)) => error(StatusCode::CONFLICT, e),
        Err(DeleteError::Storage(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// One stored **Package** as the API shows it (PKG-signed-packages-and-delivery) — never its artifact bytes.
///
/// A Package is identified by *(Agent type, version)*, stated at creation and never edited: a new
/// version is a new Package. It holds one entry per platform and aims at nobody — the Deployments
/// that hold it do (PKG-signed-packages-and-delivery). **Saving never distributes anything** (SRV-deployments-and-pinned-rollouts): a Package reaches
/// an Agent only through a rollout act, and which Agents run it is answered per Agent by
/// `GET /api/v1/agents`.
#[derive(Serialize, ToSchema)]
struct PackageSetView {
    /// The Agent type this Package is built for, matched raw against the `service.name` an Agent
    /// reports before anything else is considered (PKG-signed-packages-and-delivery). Half its identity — and the name it
    /// carries on the wire, which is why it never holds the version.
    agent_type: String,
    /// The version every entry of this Package shares. The other half of its identity.
    version: String,
    /// What an operator reads: the Agent type and the version together. Derived, never stored.
    display_name: String,
    /// One entry per platform. An Agent is offered the one built for the machine it reported, and
    /// never another (PKG-signed-packages-and-delivery).
    entries: Vec<PackageEntryView>,
    /// The Deployments that hold this Package, in name order.
    ///
    /// This is where "whom does it reach" is answered: a Package aims at nobody by itself
    /// (PKG-signed-packages-and-delivery), so an empty list means it is stored and no rollout act can release it.
    deployments: Vec<String>,
}

/// One platform's entry of a Package: an uploaded artifact and its signature (PKG-signed-packages-and-delivery).
#[derive(Serialize, ToSchema)]
struct PackageEntryView {
    /// The operating system, as `os.type` reports it: `linux`, `darwin`, `windows`.
    os: String,
    /// The architecture, as `host.arch` reports it: `amd64`, `arm64`.
    arch: String,
    /// The artifact's size in bytes.
    size: u64,
    /// The artifact's SHA-256, hex — **the exact value the Agent verifies what it downloaded
    /// against** (PKG-signed-packages-and-delivery). Reading it is how an operator answers "did this host take my bytes"
    /// without trusting a status field: it is the same string the Agent reports back in its
    /// `PackageStatuses`, and the same one `opamp-fleetctl package sha256` prints locally.
    content_hash: String,
    /// The per-package hash this entry is offered under, hex. An Agent echoes it once it is in
    /// sync, and the Server stops re-offering while the two agree — so a rollout that seems not to
    /// travel is answered here rather than by reading logs.
    package_hash: String,
    /// The operator's Ed25519 signature over this entry, hex, as `opamp-fleetctl package sign`
    /// prints it (PKG-signed-packages-and-delivery); absent while the entry is unsigned. An unsigned entry is offered to
    /// nobody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    /// What the Server found when it checked that signature against `[packages] verification_key`
    /// (PKG-signed-packages-and-delivery). Feedback only: an `invalid` one is stored and offered, and the Agent's own check
    /// decides.
    signature_state: SignatureStateView,
}

/// What the Server found when it checked an entry's signature (PKG-signed-packages-and-delivery).
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
enum SignatureStateView {
    /// The entry carries no signature; it is offered to nobody.
    Missing,
    /// The signature verifies against the Server's `[packages] verification_key`.
    Valid,
    /// The signature does not verify against that key. Stored, shown as a warning, and never
    /// refused: the Agent's check decides.
    Invalid,
    /// The entry carries a signature and the Server holds no key to check it with.
    Unchecked,
}

impl From<SignatureState> for SignatureStateView {
    fn from(state: SignatureState) -> Self {
        match state {
            SignatureState::Missing => SignatureStateView::Missing,
            SignatureState::Valid => SignatureStateView::Valid,
            SignatureState::Invalid => SignatureStateView::Invalid,
            SignatureState::Unchecked => SignatureStateView::Unchecked,
        }
    }
}

impl PackageSetView {
    fn of(summary: PackageSummary, deployments: Vec<String>) -> Self {
        PackageSetView {
            display_name: summary.id.display_name(),
            agent_type: summary.id.agent_type,
            version: summary.id.version,
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
                    signature: entry.signature,
                    signature_state: entry.signature_state.into(),
                })
                .collect(),
        }
    }
}

/// The `(Package, Platform)` a route names in its path, or the `400` that says which half of it
/// does not parse. `platform_prefix` leads the platform half's message.
fn artifact_of(
    agent_type: &str,
    version: &str,
    os: &str,
    arch: &str,
    platform_prefix: &str,
) -> Result<(PackageId, Platform), Refusal> {
    let id = PackageId::new(agent_type, version).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let platform = Platform::new(os, arch)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{platform_prefix}{e}")))?;
    Ok((id, platform))
}

/// A stored Package as the API answers with it, read back from the store rather than assembled
/// from whatever the handler happened to be given — so every response describes the Package as it
/// now is.
fn set_response(state: &OperatorCases, id: &PackageId) -> Response {
    match state.package(id) {
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
/// conflicts with the Package's current state (`409`), absence is `404`, bad input `400`.
fn package_error(e: PackageError) -> Response {
    let status = match e.refusal {
        Some(crate::packages::Refusal::NotFound) => StatusCode::NOT_FOUND,
        Some(crate::packages::Refusal::Conflict) => StatusCode::CONFLICT,
        Some(crate::packages::Refusal::Invalid) => StatusCode::BAD_REQUEST,
        None => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, e.message)
}

/// All stored Packages, in identity order — Agent type, then version — and never the artifact
/// bytes.
#[utoipa::path(
    get,
    path = "/api/v1/packages",
    tag = "packages",
    responses(
        (status = 200, description = "Every stored package Set", body = [PackageSetView]),
        (status = 404, description = "Package delivery is not configured", body = ErrorBody)
    )
)]
async fn list_packages(State(state): State<Arc<OperatorCases>>) -> Response {
    match state.packages() {
        Some(summaries) => {
            // One pass over the fleet for the whole list, rather than one per Package.
            let holding = state.deployments_holding();
            Json(
                summaries
                    .into_iter()
                    .map(|summary| {
                        let deployments = holding
                            .get(&summary.id.to_string())
                            .cloned()
                            .unwrap_or_default();
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

/// One stored Package.
#[utoipa::path(
    get,
    path = "/api/v1/packages/{agent_type}/{version}",
    tag = "packages",
    params(
        ("agent_type" = String, Path, description = "The Agent type the Set is built for (PKG-signed-packages-and-delivery)"),
        ("version" = String, Path, description = "The Set's version")
    ),
    responses(
        (status = 200, description = "The stored Set", body = PackageSetView),
        (status = 400, description = "Invalid identity", body = ErrorBody),
        (status = 404, description = "No such Set, or package delivery is not configured", body = ErrorBody)
    )
)]
async fn get_package_set(
    State(state): State<Arc<OperatorCases>>,
    Path((agent_type, version)): Path<(String, String)>,
) -> Response {
    match PackageId::load(&agent_type, &version) {
        Ok(id) => set_response(&state, &id),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

/// Creates a Package (PKG-signed-packages-and-delivery). **Saving never distributes** (SRV-deployments-and-pinned-rollouts): the Package reaches an
/// Agent only through a rollout act of a Deployment that holds it. The identity in the path is the
/// whole identity and there is nothing else to write, so creating one that exists answers it
/// unchanged: a new version is a new Package, never a mutation of an old one.
#[utoipa::path(
    put,
    path = "/api/v1/packages/{agent_type}/{version}",
    tag = "packages",
    params(
        ("agent_type" = String, Path, description = "The Agent type the Set is built for, compared raw against the `service.name` Agents report"),
        ("version" = String, Path, description = "The Set's version — every entry shares it. A SemVer 2.0.0 version, such as `1.19.0` for upstream's `1.19` (PKG-versions-high-water-mark-floor)")
    ),
    responses(
        (status = 200, description = "The stored Set", body = PackageSetView),
        (status = 400, description = "Invalid identity, or a version that is not SemVer", body = ErrorBody),
        (status = 404, description = "Package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The Set could not be persisted", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn put_package_set(
    State(state): State<Arc<OperatorCases>>,
    Path((agent_type, version)): Path<(String, String)>,
) -> Response {
    let id = match PackageId::new(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match state.create_package_set(&id) {
        Ok(()) => set_response(&state, &id),
        Err(e) => package_error(e),
    }
}

/// Deletes a Package — entries, artifacts and metadata — refused with `409` while an Agent's
/// assignment pins it, saying how many (SRV-deployments-and-pinned-rollouts §12). A deleted Package leaves every
/// Deployment that held it; nothing is ever uninstalled (PKG-signed-packages-and-delivery).
#[utoipa::path(
    delete,
    path = "/api/v1/packages/{agent_type}/{version}",
    tag = "packages",
    params(
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version")
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 400, description = "Invalid identity", body = ErrorBody),
        (status = 404, description = "No such Set, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "An Agent's assignment pins the Package", body = ErrorBody),
        (status = 500, description = "The Set could not be deleted", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn delete_package_set(
    State(state): State<Arc<OperatorCases>>,
    Path((agent_type, version)): Path<(String, String)>,
) -> Response {
    // A Package stored under a non-SemVer version is deleted like any other (PKG-versions-high-water-mark-floor §3).
    let id = match PackageId::load(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    // Off the async threads: the deletion's sweep may wait up to a minute for the blob store's lock
    // (SRV-state-stores-contract-pair §14).
    let deleted = tokio::task::spawn_blocking({
        let id = id.clone();
        move || state.delete_package_set(&id)
    })
    .await
    .unwrap_or_else(|e| Err(PackageError::from(format!("the deletion ended: {e}"))));
    match deleted {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no package set {id}")),
        Err(e) => package_error(e),
    }
}

/// Stores one platform's artifact as an entry of a Package (PKG-signed-packages-and-delivery). The artifact is
/// the raw request body; the Package and the platform are the path. New bytes remove the entry's
/// signature, which covered the old hash. Nothing is distributed: the Package reaches nobody until
/// a rollout act releases it (SRV-deployments-and-pinned-rollouts). Refused while the Package is assigned to an Agent — an
/// assigned Package's bytes are immutable.
#[utoipa::path(
    put,
    path = "/api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}",
    tag = "packages",
    params(
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version"),
        ("os" = String, Path, description = "The operating system this artifact is built for, as `os.type`: `linux`, `darwin`, `windows`"),
        ("arch" = String, Path, description = "The architecture, as `host.arch`: `amd64`, `arm64`")
    ),
    request_body(content = Vec<u8>, description = "The artifact bytes", content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "The Set, with the stored entry, unsigned", body = PackageSetView),
        (status = 400, description = "Invalid identity, platform, or an empty artifact", body = ErrorBody),
        (status = 404, description = "No such Set, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Set is assigned to an Agent and its entries are immutable", body = ErrorBody),
        (status = 413, description = "The artifact exceeds max_package_size_bytes", body = ErrorBody),
        (status = 500, description = "The entry could not be persisted", body = ErrorBody),
        (status = 507, description = "Storing it would exceed max_total_package_bytes", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn put_package_entry(
    State(state): State<Arc<OperatorCases>>,
    Path((agent_type, version, os, arch)): Path<(String, String, String, String)>,
    body: Body,
) -> Response {
    let id = match PackageId::new(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let platform = match Platform::new(&os, &arch) {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    if let Err(e) = state.check_package_upload(&id) {
        return package_error(e);
    }
    let mut reservation = match state.admit_upload() {
        Ok(reservation) => reservation,
        Err(full) => return error(StatusCode::INSUFFICIENT_STORAGE, full.to_string()),
    };
    // The artifact is streamed into the blob store as it arrives and bounded on the way: taking it
    // as `Bytes` would mean holding a whole program in memory — twice — before writing it out.
    // The store reads on a thread of its own; this side hands it each chunk, and the end only once
    // every limit has passed, so a refused or cancelled upload stores nothing.
    let (chunks, receiver) = tokio::sync::mpsc::channel(8);
    let store = tokio::task::spawn_blocking({
        let state = state.clone();
        let (id, platform) = (id.clone(), platform.clone());
        move || state.put_package_entry(&id, &platform, &mut ChunkReader::new(receiver))
    });
    let streamed = stream_upload(body, &chunks, state.max_package_size(), &mut reservation).await;
    let streamed = streamed.and_then(|written| {
        reservation
            .admit_artifact()
            .map(|()| written)
            .map_err(UploadError::StoreFull)
    });
    let written = match streamed {
        Ok(written) => {
            // The end of the artifact: the store may commit it now. A store that has already
            // given up answers below.
            let _ = chunks.send(Chunk::End).await;
            written
        }
        Err(e) => {
            // Without the end, the store's stream fails and nothing is stored.
            drop(chunks);
            let _ = store.await;
            return match e {
                UploadError::TooLarge(limit) => error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("the artifact exceeds the {limit}-byte package size limit"),
                ),
                UploadError::StoreFull(full) => {
                    error(StatusCode::INSUFFICIENT_STORAGE, full.to_string())
                }
                UploadError::Io(e) => error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("the upload could not be stored: {e}"),
                ),
            };
        }
    };
    drop(chunks);
    let committed = store.await.unwrap_or_else(|e| {
        Err(PackageError::from(format!(
            "the upload could not be stored: {e}"
        )))
    });
    // Stored now, or gone: either way the store counts it from here, and the upload no longer.
    drop(reservation);
    match committed {
        Ok(()) => {
            info!(set = %id, bytes = written, "package entry stored from the API");
            set_response(&state, &id)
        }
        Err(e) => package_error(e),
    }
}

/// Deletes one entry of a Package. Refused while the Package is assigned to an Agent — its bytes
/// are immutable (SRV-deployments-and-pinned-rollouts). The last entry taken away leaves an empty Package: a Package being
/// reassembled is a normal state, and deleting the Package is its own act.
#[utoipa::path(
    delete,
    path = "/api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}",
    tag = "packages",
    params(
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
        (status = 500, description = "The entry could not be deleted", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn delete_package_entry(
    State(state): State<Arc<OperatorCases>>,
    Path((agent_type, version, os, arch)): Path<(String, String, String, String)>,
) -> Response {
    let (id, platform) = match artifact_of(&agent_type, &version, &os, &arch, "invalid platform: ")
    {
        Ok(pair) => pair,
        Err((status, why)) => return error(status, why),
    };
    // Off the async threads: the deletion's sweep may wait up to a minute for the blob store's lock
    // (SRV-state-stores-contract-pair §14).
    let deleted = tokio::task::spawn_blocking({
        let (id, platform) = (id.clone(), platform.clone());
        move || state.delete_package_entry(&id, &platform)
    })
    .await
    .unwrap_or_else(|e| Err(PackageError::from(format!("the deletion ended: {e}"))));
    match deleted {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            format!("no entry {}-{} in set {id}", platform.os, platform.arch),
        ),
        Err(e) => package_error(e),
    }
}

/// Writes the operator's Ed25519 signature onto one entry of a Package (PKG-signed-packages-and-delivery), as
/// `opamp-fleetctl package sign` prints it. One signature serves every Deployment that holds the
/// Package; an entry without one is offered to nobody.
///
/// With `[packages] verification_key` set the Server checks it and answers the entry's
/// `signature_state`. An `invalid` signature is stored all the same: the Server only reports, and
/// the Agent's own verification decides. Refused while the Package is assigned to an Agent, like
/// any other write to its entries (SRV-deployments-and-pinned-rollouts).
#[utoipa::path(
    put,
    path = "/api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}/signature",
    tag = "packages",
    params(
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version"),
        ("os" = String, Path, description = "The entry's operating system"),
        ("arch" = String, Path, description = "The entry's architecture")
    ),
    request_body = SignatureSpec,
    responses(
        (status = 200, description = "The Set, with the signed entry and its signature_state", body = PackageSetView),
        (status = 400, description = "Invalid identity or platform, or a signature that is empty, not hex, or not 64 bytes", body = ErrorBody),
        (status = 404, description = "No such Set or entry, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Set is assigned to an Agent and its entries are immutable", body = ErrorBody),
        (status = 500, description = "The signature could not be persisted", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn put_entry_signature(
    State(state): State<Arc<OperatorCases>>,
    Path((agent_type, version, os, arch)): Path<(String, String, String, String)>,
    Json(spec): Json<SignatureSpec>,
) -> Response {
    let (id, platform) = match artifact_of(&agent_type, &version, &os, &arch, "invalid platform: ")
    {
        Ok(pair) => pair,
        Err((status, why)) => return error(status, why),
    };
    let signature = match hex::decode(spec.signature.trim()) {
        Ok(bytes) => bytes,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!(
                    "the signature must be hex, as `opamp-fleetctl package sign` prints it: {e}"
                ),
            );
        }
    };
    match state.put_package_entry_signature(&id, &platform, signature) {
        Ok(_) => set_response(&state, &id),
        Err(e) => package_error(e),
    }
}

/// Takes one entry's signature away (PKG-signed-packages-and-delivery): the entry is offered to nobody until it is signed
/// again. Refused while the Package is assigned to an Agent (SRV-deployments-and-pinned-rollouts).
#[utoipa::path(
    delete,
    path = "/api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}/signature",
    tag = "packages",
    params(
        ("agent_type" = String, Path, description = "The Agent type"),
        ("version" = String, Path, description = "The version"),
        ("os" = String, Path, description = "The entry's operating system"),
        ("arch" = String, Path, description = "The entry's architecture")
    ),
    responses(
        (status = 200, description = "The Set, with the entry unsigned", body = PackageSetView),
        (status = 400, description = "Invalid identity or platform", body = ErrorBody),
        (status = 404, description = "No such Set, entry or signature, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Set is assigned to an Agent and its entries are immutable", body = ErrorBody),
        (status = 500, description = "The change could not be persisted", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn delete_entry_signature(
    State(state): State<Arc<OperatorCases>>,
    Path((agent_type, version, os, arch)): Path<(String, String, String, String)>,
) -> Response {
    let (id, platform) = match artifact_of(&agent_type, &version, &os, &arch, "invalid platform: ")
    {
        Ok(pair) => pair,
        Err((status, why)) => return error(status, why),
    };
    match state.remove_package_entry_signature(&id, &platform) {
        Ok(true) => set_response(&state, &id),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            format!(
                "entry {}-{} of set {id} carries no signature",
                platform.os, platform.arch
            ),
        ),
        Err(e) => package_error(e),
    }
}

/// One floor as the operator sets it (PKG-versions-high-water-mark-floor §15).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct FloorSpec {
    /// The lowest version still taken, a SemVer 2.0.0 version.
    version: String,
    /// The operator's signature over the floor's statement — the Agent type and the version —
    /// hex-encoded, as `opamp-fleetctl package floor` prints it.
    signature: String,
}

/// One floor as the Server stores it: below `version`, no Client takes a package of `agent_type`
/// (PKG-versions-high-water-mark-floor).
#[derive(Serialize, ToSchema)]
struct FloorView {
    /// The Agent type the floor holds for.
    agent_type: String,
    /// The lowest version still taken.
    version: String,
    /// The operator's Ed25519 signature over the floor, hex.
    signature: String,
    /// What the Server found when it checked that signature against `[packages]
    /// verification_key`. Feedback only: an `invalid` floor is stored and served, and each
    /// Client's own check decides. Never `missing`: a floor carries its signature.
    signature_state: SignatureStateView,
}

impl FloorView {
    fn of(agent_type: String, floor: crate::floors::Floor, state: SignatureState) -> Self {
        FloorView {
            agent_type,
            version: floor.version,
            signature: hex::encode(floor.signature),
            signature_state: state.into(),
        }
    }
}

fn floors_off() -> Response {
    error(StatusCode::NOT_FOUND, "package floors are not configured")
}

/// Every floor the Server stores and serves to the fleet, by Agent type (PKG-versions-high-water-mark-floor).
#[utoipa::path(
    get,
    path = "/api/v1/package-floors",
    tag = "packages",
    responses(
        (status = 200, description = "Every floor, by Agent type, with its signature_state", body = Vec<FloorView>),
        (status = 404, description = "Package floors are not configured", body = ErrorBody)
    )
)]
async fn list_package_floors(State(state): State<Arc<OperatorCases>>) -> Response {
    let Some(floors) = state.floors() else {
        return floors_off();
    };
    let list: Vec<FloorView> = floors
        .into_iter()
        .map(|(agent_type, floor, state)| FloorView::of(agent_type, floor, state))
        .collect();
    Json(list).into_response()
}

/// Stores the operator's signed floor of one Agent type (PKG-versions-high-water-mark-floor §15): below its version no
/// Client takes a package of the type, its own update included, once it has fetched the floor.
/// A floor is raised, or removed and set anew; a version below the one stored is `409`.
///
/// With `[packages] verification_key` set the Server checks the signature and answers the floor's
/// `signature_state`. An `invalid` floor is stored all the same: the Server only reports, and
/// each Client's own verification decides.
#[utoipa::path(
    put,
    path = "/api/v1/package-floors/{agent_type}",
    tag = "packages",
    params(("agent_type" = String, Path, description = "The Agent type")),
    request_body = FloorSpec,
    responses(
        (status = 200, description = "The stored floor, with its signature_state", body = FloorView),
        (status = 400, description = "An Agent type that is no token, a version that is not SemVer, or a signature that is not hex or not 64 bytes", body = ErrorBody),
        (status = 404, description = "Package floors are not configured", body = ErrorBody),
        (status = 409, description = "The version orders below the floor stored for the type", body = ErrorBody),
        (status = 500, description = "The floor could not be persisted", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn put_package_floor(
    State(state): State<Arc<OperatorCases>>,
    Path(agent_type): Path<String>,
    Json(spec): Json<FloorSpec>,
) -> Response {
    use crate::floors::FloorError;
    if !state.has_floors() {
        return floors_off();
    }
    let signature = match hex::decode(spec.signature.trim()) {
        Ok(bytes) => bytes,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!(
                    "the signature must be hex, as `opamp-fleetctl package floor` prints it: {e}"
                ),
            );
        }
    };
    // A floor is set only with its entry's place held, so a restore finds every floor raised;
    // only one known to have failed gives the place back unwritten.
    let Ok(place) = state.hold_place() else {
        return crate::answers::busy();
    };
    let floor_entry = |outcome: &str| {
        crate::audit::Entry::new("package_floor.set", outcome)
            .with("agent_type", agent_type.clone())
            .with("version", spec.version.clone())
    };
    let Some(outcome) = state.put_floor(&agent_type, &spec.version, signature.clone()) else {
        return floors_off();
    };
    match outcome {
        Ok(signature_state) => {
            info!(
                agent_type = %agent_type, version = %spec.version,
                state = signature_state.as_str(), "package floor set"
            );
            fill_place(
                place,
                floor_entry("set").with("signature_state", signature_state.as_str()),
            );
            let floor = crate::floors::Floor {
                version: spec.version,
                signature,
            };
            Json(FloorView::of(agent_type, floor, signature_state)).into_response()
        }
        Err(FloorError::Invalid(e)) => error(StatusCode::BAD_REQUEST, e),
        Err(FloorError::Lower(stored)) => error(
            StatusCode::CONFLICT,
            format!(
                "the floor of {agent_type:?} is {stored}, and {} orders below it — a floor is \
                 raised, or removed and set anew",
                spec.version
            ),
        ),
        Err(FloorError::Store(e)) => {
            // The store may hold it all the same, so the record names it, toward setting it
            // again at a restore.
            fill_place(place, floor_entry("unknown"));
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the floor could not be persisted: {e}"),
            )
        }
    }
}

/// Removes the floor of one Agent type: the Server stops serving it (PKG-versions-high-water-mark-floor §20). A Client
/// that has kept it keeps it; only its host lowers what it holds.
#[utoipa::path(
    delete,
    path = "/api/v1/package-floors/{agent_type}",
    tag = "packages",
    params(("agent_type" = String, Path, description = "The Agent type")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No floor for the type, or package floors are not configured", body = ErrorBody),
        (status = 500, description = "The removal could not be persisted; the floor is kept", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn delete_package_floor(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Path(agent_type): Path<String>,
) -> Response {
    let Some(outcome) = state.remove_floor(&agent_type) else {
        return floors_off();
    };
    match outcome {
        Ok(true) => {
            info!(agent_type = %agent_type, "package floor removed");
            state.note(
                crate::audit::Entry::new("package_floor.removed", "removed")
                    .with("agent_type", agent_type.clone()),
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            format!("no floor for {agent_type:?}"),
        ),
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the floor could not be removed: {e}"),
        ),
    }
}

/// Why a streamed upload did not become an artifact.
enum UploadError {
    /// The body grew past the configured package limit; the number is that limit.
    TooLarge(usize),
    /// The body, with the uploads in progress beside it, would take the store past its ceiling.
    StoreFull(crate::fleet::StoreFull),
    Io(std::io::Error),
}

/// What the request side hands the store's reader: the next chunk of the artifact, or its end.
enum Chunk {
    Data(axum::body::Bytes),
    End,
}

/// The upload as the blob store reads it (SRV-state-stores-contract-pair §13): the request's chunks, handed over a
/// channel to the thread the store reads on. A channel that closes before [`Chunk::End`] is a
/// failed stream — a refused, cut-off or cancelled upload — so the store keeps nothing of it.
struct ChunkReader {
    receiver: tokio::sync::mpsc::Receiver<Chunk>,
    current: axum::body::Bytes,
    ended: bool,
}

impl ChunkReader {
    fn new(receiver: tokio::sync::mpsc::Receiver<Chunk>) -> Self {
        ChunkReader {
            receiver,
            current: axum::body::Bytes::new(),
            ended: false,
        }
    }
}

impl std::io::Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.current.is_empty() {
            if self.ended {
                return Ok(0);
            }
            match self.receiver.blocking_recv() {
                Some(Chunk::Data(bytes)) => self.current = bytes,
                Some(Chunk::End) => self.ended = true,
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the upload ended before its last byte",
                    ))
                }
            }
        }
        let n = buf.len().min(self.current.len());
        buf[..n].copy_from_slice(&self.current.split_to(n));
        Ok(n)
    }
}

/// Streams a request body to the store's reader, refusing it the moment it grows past `limit` or
/// takes the store past its ceiling. Returns how many bytes were handed over.
async fn stream_upload(
    body: Body,
    chunks: &tokio::sync::mpsc::Sender<Chunk>,
    limit: usize,
    reservation: &mut crate::fleet::UploadReservation<'_>,
) -> Result<u64, UploadError> {
    use futures_util::StreamExt;

    let mut stream = body.into_data_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| UploadError::Io(std::io::Error::other(e)))?;
        written += chunk.len() as u64;
        if written > limit as u64 {
            return Err(UploadError::TooLarge(limit));
        }
        reservation
            .grow(chunk.len() as u64)
            .map_err(UploadError::StoreFull)?;
        if chunks.send(Chunk::Data(chunk)).await.is_err() {
            // The store stopped reading: it failed, and says why when it is awaited.
            break;
        }
    }
    Ok(written)
}

// -------------------------------------------------------------------------------------------
// Deployments (PKG-signed-packages-and-delivery, SRV-deployments-and-pinned-rollouts): the aim and the act — what a Package and a Configuration do
// not have.
// -------------------------------------------------------------------------------------------

/// One Deployment as the REST API shows it.
#[derive(Serialize, ToSchema)]
struct DeploymentView {
    /// The operator's name for this channel, and the path segment that addresses it.
    name: String,
    /// Equality pairs that must all match an attribute the Agent reported, labels included
    /// (SRV-configurations-compose-one-map). **Never empty**: there is no fleet-wide default, because an Agent belongs to at
    /// most one Deployment and an empty Selector would match every Agent in every other channel.
    selector: std::collections::BTreeMap<String, String>,
    /// At most one Package per Agent type — an Agent has one binary to replace.
    packages: Vec<DeploymentPackageView>,
    /// The Configurations it holds, by name, in name order (SRV-deployments-and-pinned-rollouts). Each reaches the Agents of
    /// this channel whose type it fits.
    configurations: Vec<String>,
    /// Agents this channel claims, and no other does.
    ///
    /// **Zero is the value worth looking at**: a channel aims at nobody when its Selector names an
    /// attribute no Agent reports, or a value none of them carries — and neither is a rejected
    /// write, so nothing else would say so.
    claiming_agents: usize,
    /// Of those, the Agents a rollout act would actually move — what it would release to them
    /// differs from what their assignment pins (SRV-deployments-and-pinned-rollouts §9). Zero here with a non-zero
    /// `claiming_agents` means everyone in the channel already holds it: nothing to do, and nothing
    /// wrong.
    targeted_agents: usize,
    /// Agents this channel matches that **another Deployment matches too**. They are offered nothing
    /// new until an operator narrows a Selector, and they are not in `claiming_agents`.
    conflicting_agents: usize,
}

/// One Package a Deployment holds. Its entries and their signatures are the Package's own
/// (`GET /api/v1/packages`, PKG-signed-packages-and-delivery).
#[derive(Serialize, ToSchema)]
struct DeploymentPackageView {
    /// The Agent type this Package is built for — also the key under which this Deployment holds
    /// it, and the name it carries on the wire.
    agent_type: String,
    version: String,
    /// What an operator reads: the Agent type and the version together.
    display_name: String,
}

impl DeploymentView {
    fn of(deployment: Deployment, reach: crate::fleet::DeploymentReach) -> Self {
        let mut packages: Vec<DeploymentPackageView> = deployment
            .packages
            .values()
            .map(|id| DeploymentPackageView {
                agent_type: id.agent_type.clone(),
                version: id.version.clone(),
                display_name: id.display_name(),
            })
            .collect();
        packages.sort_by(|a, b| a.agent_type.cmp(&b.agent_type));
        DeploymentView {
            name: deployment.name,
            selector: deployment.selector,
            packages,
            configurations: deployment.configurations.into_iter().collect(),
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

/// One artifact's Ed25519 signature, as the operator supplies it (PKG-signed-packages-and-delivery).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct SignatureSpec {
    /// The signature over the entry's statement — the Agent type, the version and the artifact's
    /// SHA-256 — hex-encoded, as `opamp-fleetctl package sign` prints it.
    signature: String,
}

/// Maps a store refusal onto the status code that says the same thing.
fn deployment_error(refusal: DeploymentError) -> Response {
    use DeploymentError as E;
    let text = refusal.to_string();
    match refusal {
        E::Invalid(_) => error(StatusCode::BAD_REQUEST, text),
        E::NotFound | E::Missing(_) => error(StatusCode::NOT_FOUND, text),
        E::TypeTaken { .. } | E::Conflict(_) => error(StatusCode::CONFLICT, text),
        E::Storage(_) => error(StatusCode::INTERNAL_SERVER_ERROR, text),
    }
}

/// The `404` that says package delivery is not armed on this Server.
fn deployments_off() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "package delivery is not configured on this Server",
    )
}

/// A status and what to say with it — carried instead of a whole `Response` so a helper's error
/// path stays small (the response is built once, at the handler that returns it).
type Refusal = (StatusCode, String);

/// Every Deployment, in name order.
#[utoipa::path(
    get,
    path = "/api/v1/deployments",
    tag = "deployments",
    responses(
        (status = 200, description = "Every Deployment, in name order", body = [DeploymentView]),
        (status = 404, description = "Package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The fleet cannot be read from the store", body = ErrorBody)
    )
)]
async fn list_deployments(State(state): State<Arc<OperatorCases>>) -> Response {
    let Some(all) = state.deployments() else {
        return deployments_off();
    };
    let reach = match state.deployment_reach() {
        Ok(reach) => reach,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    Json(
        all.into_iter()
            .map(|deployment| {
                let counts = reach.get(&deployment.name).copied().unwrap_or_default();
                DeploymentView::of(deployment, counts)
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}

/// One Deployment.
#[utoipa::path(
    get,
    path = "/api/v1/deployments/{name}",
    tag = "deployments",
    params(("name" = String, Path, description = "the Deployment's name")),
    responses(
        (status = 200, description = "The Deployment", body = DeploymentView),
        (status = 404, description = "No such Deployment, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The fleet cannot be read from the store", body = ErrorBody)
    )
)]
async fn get_deployment(
    State(state): State<Arc<OperatorCases>>,
    Path(name): Path<String>,
) -> Response {
    if !state.has_deployments() {
        return deployments_off();
    }
    match state.deployment(&name) {
        Some(deployment) => deployment_response(&state, deployment),
        None => error(StatusCode::NOT_FOUND, format!("no deployment {name:?}")),
    }
}

/// Creates a Deployment, or replaces the channel it aims at.
///
/// **This distributes nothing** (SRV-deployments-and-pinned-rollouts). Saving is saving; the rollout act is its own press,
/// and until it happens no Agent is offered anything new.
///
/// The Selector must name at least one pair. There is deliberately no fleet-wide default: an Agent
/// belongs to at most one Deployment, so an empty Selector would collide with every other channel the
/// moment a second one exists — and it is what a forgotten field looks like. Channels are a partition
/// over an attribute every Agent carries (`channel = "stable"`), set at provisioning or as a Server
/// label (SRV-fleet-record-agents-hosts), because a Selector is equality and cannot express "not".
#[utoipa::path(
    put,
    path = "/api/v1/deployments/{name}",
    tag = "deployments",
    params(("name" = String, Path, description = "the Deployment's name")),
    request_body = DeploymentSpec,
    responses(
        (status = 200, description = "The stored Deployment — distributed to nobody until rolled out", body = DeploymentView),
        (status = 400, description = "Invalid name, or a Selector that names no pair", body = ErrorBody),
        (status = 404, description = "Package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The change could not be written, or it was written and whom the Deployment reaches cannot be read from the store; the answer says which", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn put_deployment(
    State(state): State<Arc<OperatorCases>>,
    Path(name): Path<String>,
    Json(spec): Json<DeploymentSpec>,
) -> Response {
    match state.put_deployment(&name, spec.selector) {
        Some(Ok(deployment)) => deployment_response(&state, deployment),
        Some(Err(e)) => deployment_error(e),
        None => deployments_off(),
    }
}

/// Deletes a Deployment — refused with `409` while an Agent's assignment names it, saying how many
/// (SRV-deployments-and-pinned-rollouts §12). Rolling those Agents out through another Deployment, or forgetting them,
/// comes first. Nothing is uninstalled either way (SRV-deployments-and-pinned-rollouts).
#[utoipa::path(
    delete,
    path = "/api/v1/deployments/{name}",
    tag = "deployments",
    params(("name" = String, Path, description = "the Deployment's name")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such Deployment, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "An Agent's assignment still names this Deployment"),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn delete_deployment(
    State(state): State<Arc<OperatorCases>>,
    Path(name): Path<String>,
) -> Response {
    if !state.has_deployments() {
        return deployments_off();
    }
    match state.delete_deployment(&name) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no deployment {name:?}")),
        Err(e) => deployment_error(e),
    }
}

/// Re-aims a Deployment. Editable in every state (SRV-deployments-and-pinned-rollouts §11) — moving a channel is how a
/// rollout proceeds. It changes whom the *next* act would reach, never a running offer.
#[utoipa::path(
    put,
    path = "/api/v1/deployments/{name}/selector",
    tag = "deployments",
    params(("name" = String, Path, description = "the Deployment's name")),
    request_body = DeploymentSpec,
    responses(
        (status = 200, description = "The re-aimed Deployment", body = DeploymentView),
        (status = 400, description = "A Selector that names no pair", body = ErrorBody),
        (status = 404, description = "No such Deployment, or package delivery is not configured", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn put_deployment_selector(
    State(state): State<Arc<OperatorCases>>,
    Path(name): Path<String>,
    Json(spec): Json<DeploymentSpec>,
) -> Response {
    // The same write as `PUT /api/v1/deployments/{name}`, except that this route only re-aims:
    // it never creates.
    if state.has_deployments() && state.deployment(&name).is_none() {
        return error(StatusCode::NOT_FOUND, format!("no deployment {name:?}"));
    }
    put_deployment(State(state), Path(name), Json(spec)).await
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
    responses(
        (status = 200, description = "The Deployment, with the Package", body = DeploymentView),
        (status = 400, description = "Invalid Agent type or version", body = ErrorBody),
        (status = 404, description = "No such Deployment or Package, or package delivery is not configured", body = ErrorBody),
        (status = 409, description = "The Deployment already holds a Package for this Agent type", body = ErrorBody),
        (status = 500, description = "The change could not be written, or it was written and whom the Deployment reaches cannot be read from the store; the answer says which", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn put_deployment_package(
    State(state): State<Arc<OperatorCases>>,
    Path((name, agent_type, version)): Path<(String, String, String)>,
    Query(replace): Query<ReplaceQuery>,
) -> Response {
    let id = match PackageId::new(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    // A channel cannot offer what the store does not hold, so a reference to a Package that is not
    // there is a mistake, not a reservation for one uploaded later.
    match state.package(&id) {
        Some(_) => {}
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

/// Takes a Package out of a Deployment. Editable in every state (SRV-deployments-and-pinned-rollouts §11): an Agent
/// already released keeps the Package its assignment pins until the next press, which drops it.
#[utoipa::path(
    delete,
    path = "/api/v1/deployments/{name}/packages/{agent_type}/{version}",
    tag = "deployments",
    params(
        ("name" = String, Path, description = "the Deployment's name"),
        ("agent_type" = String, Path, description = "the Package's Agent type"),
        ("version" = String, Path, description = "the Package's version")
    ),
    responses(
        (status = 200, description = "The Deployment, without the Package", body = DeploymentView),
        (status = 400, description = "Invalid Agent type or version", body = ErrorBody),
        (status = 404, description = "No such Deployment or Package in it, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The change could not be written, or it was written and whom the Deployment reaches cannot be read from the store; the answer says which", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn delete_deployment_package(
    State(state): State<Arc<OperatorCases>>,
    Path((name, agent_type, version)): Path<(String, String, String)>,
) -> Response {
    // Taking a Package out writes nothing to it, so an older non-SemVer one may leave too.
    let id = match PackageId::load(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match state.remove_deployment_package(&name, &id) {
        Ok(deployment) => deployment_response(&state, deployment),
        Err(e) => deployment_error(e),
    }
}

/// Puts a Configuration into a Deployment (SRV-deployments-and-pinned-rollouts §5). A Configuration may sit in several
/// Deployments; holding it already is the same request arriving twice. **This distributes
/// nothing**: the Agents of the channel see it waiting until the next press.
#[utoipa::path(
    put,
    path = "/api/v1/deployments/{name}/configurations/{configuration}",
    tag = "deployments",
    params(
        ("name" = String, Path, description = "the Deployment's name"),
        ("configuration" = String, Path, description = "the Configuration's name")
    ),
    responses(
        (status = 200, description = "The Deployment, with the Configuration", body = DeploymentView),
        (status = 404, description = "No such Deployment or Configuration, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The change could not be written, or it was written and whom the Deployment reaches cannot be read from the store; the answer says which", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn put_deployment_configuration(
    State(state): State<Arc<OperatorCases>>,
    Path((name, configuration)): Path<(String, String)>,
) -> Response {
    match state.put_deployment_configuration(&name, &configuration) {
        Ok(deployment) => deployment_response(&state, deployment),
        Err(e) => deployment_error(e),
    }
}

/// Takes a Configuration out of a Deployment. Editable in every state (SRV-deployments-and-pinned-rollouts §11): an
/// Agent already released keeps the revision its assignment pins until the next press, which drops
/// it from the Agent's map.
#[utoipa::path(
    delete,
    path = "/api/v1/deployments/{name}/configurations/{configuration}",
    tag = "deployments",
    params(
        ("name" = String, Path, description = "the Deployment's name"),
        ("configuration" = String, Path, description = "the Configuration's name")
    ),
    responses(
        (status = 200, description = "The Deployment, without the Configuration", body = DeploymentView),
        (status = 404, description = "No such Deployment, or it does not hold the Configuration, or package delivery is not configured", body = ErrorBody),
        (status = 500, description = "The change could not be written, or it was written and whom the Deployment reaches cannot be read from the store; the answer says which", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn delete_deployment_configuration(
    State(state): State<Arc<OperatorCases>>,
    Path((name, configuration)): Path<(String, String)>,
) -> Response {
    match state.remove_deployment_configuration(&name, &configuration) {
        Ok(deployment) => deployment_response(&state, deployment),
        Err(e) => deployment_error(e),
    }
}

/// `?replace=true` on putting a Package into a Deployment.
#[derive(Deserialize, IntoParams)]
struct ReplaceQuery {
    replace: Option<bool>,
}

/// Rolls a Deployment out to **every Agent it claims and would move** (SRV-deployments-and-pinned-rollouts §9).
///
/// **This is the moment the fleet changes.** Each such Agent's assignment is replaced with this
/// channel's release, pinned as of this press: the Package it holds for the Agent's type, if that
/// is a candidate, and every Configuration it holds that fits the Agent's type, at its saved
/// revision. The hash gates send only what changed. An Agent that enrols — or is labelled into the
/// channel — later is *not* included: it surfaces in the fleet view as waiting, for its own act.
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
        (status = 409, description = "The Deployment holds no Package and no Configuration, or a Package with an unsigned entry", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn rollout_deployment(
    State(state): State<Arc<OperatorCases>>,
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
/// `500` when the reach cannot be read; a change the request made stands, and the answer says so.
fn deployment_response(state: &OperatorCases, deployment: Deployment) -> Response {
    let reach = match state.deployment_reach() {
        Ok(reach) => reach.get(&deployment.name).copied().unwrap_or_default(),
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "whom the Deployment reaches cannot be read; a change this request made was \
                     written: {e}"
                ),
            )
        }
    };
    Json(DeploymentView::of(deployment, reach)).into_response()
}

/// The enrolment window as the API answers with it (SRV-state-stores-contract-pair §8).
#[derive(Serialize, ToSchema)]
struct EnrolmentWindow {
    /// Whether a host with a bootstrap certificate may enrol now.
    open: bool,
    /// When the window closes, in milliseconds since the Unix epoch; absent while it is closed.
    #[serde(skip_serializing_if = "Option::is_none")]
    until_ms: Option<u64>,
}

/// How long to open the enrolment window for.
#[derive(Deserialize, ToSchema)]
struct OpenWindow {
    /// From 1 to 86400 seconds.
    open_for_secs: u64,
}

/// One enrolment request waiting for an operator (SEC-bootstrap-certificates-and-enrolment §10).
#[derive(Serialize, ToSchema)]
struct PendingEnrolment {
    /// What `approve` and `reject` name: the SHA-256 fingerprint of the requested public key.
    id: String,
    arrived_ms: u64,
    /// The address the request came from, on a direct connection.
    #[serde(skip_serializing_if = "Option::is_none")]
    peer: Option<String>,
    /// The host of the Gateway the request came through, in place of a peer address (CLT-modes-and-gateway §11).
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway: Option<String>,
    /// The SHA-256 fingerprint of the requested public key, hex — the Client logs the same value,
    /// so an operator can match a request to a host.
    key_fingerprint: String,
    /// The six subject values of the bootstrap certificate the request proves: what an
    /// approval carries into the host certificate (SEC-bootstrap-certificates-and-enrolment §10). The request's own subject
    /// is not listed, because it is neither trusted nor carried. Absent when that certificate's
    /// subject cannot be carried — `subject_refusal` then says why.
    #[serde(skip_serializing_if = "Option::is_none")]
    bootstrap_subject: Option<HostSubject>,
    /// Why the bootstrap certificate's subject breaks the rules of SEC-bootstrap-certificates-and-enrolment §1, naming the
    /// attribute: an approval is refused `409` with this reason, and nothing is signed. Absent
    /// when the values can be carried.
    #[serde(skip_serializing_if = "Option::is_none")]
    subject_refusal: Option<String>,
    /// The SHA-256 fingerprint of that bootstrap certificate, hex.
    bootstrap_fingerprint: String,
    /// Its serial, lowercase hex: the `serial` of `GET /api/v1/bootstrap-certificates` when this
    /// Server issued it.
    bootstrap_serial: String,
}

/// What an operator asks a bootstrap certificate for (SEC-bootstrap-certificates-and-enrolment §1 and §3 and
/// OPS-operator-program-fleetctl §14).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct IssueBootstrapRequest {
    /// Required, with `organization` and `organizational_unit`.
    #[serde(default)]
    #[schema(required = true)]
    subject: Option<BootstrapSubject>,
    /// 1 to 90; 10 when absent. Cut to the bootstrap CA's end.
    #[serde(default)]
    validity_days: Option<u32>,
    /// 8 to 128 characters without a control character (SEC-bootstrap-certificates-and-enrolment). Required: the Server keeps a PKCS#12
    /// file of the certificate and its key under this password, to be downloaded until the
    /// certificate ends or is revoked; the key is never answered in PEM. The password is kept
    /// nowhere. Read as absent when it is missing, so the refusal names it.
    // Wiped when it is dropped, from the moment it is read (SEC-secrets-wiped-on-drop).
    #[serde(default)]
    #[schema(value_type = Option<String>, format = Password, required = true)]
    password: Option<crate::bootstrap::Password>,
}

/// The subject of a bootstrap certificate: these attributes only, `organization` and
/// `organizational_unit` required and the others optional; the Server sets the common name. Every
/// host certificate issued from it carries them, and they describe and select the Host through
/// `fleet.host.*`; they authorize nothing (SEC-bootstrap-certificates-and-enrolment §1).
#[derive(Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct BootstrapSubject {
    /// C — exactly two upper-case letters, an ISO 3166-1 alpha-2 code.
    country: Option<String>,
    /// ST — 1 to 64 characters without a control character, as are the others.
    state: Option<String>,
    /// L.
    locality: Option<String>,
    /// O — the customer, typically. Required; absent is refused `400` naming it.
    #[schema(required = true)]
    organization: Option<String>,
    /// OU — the site, typically. Required; absent is refused `400` naming it.
    #[schema(required = true)]
    organizational_unit: Option<String>,
    /// organizationIdentifier (2.5.4.97) — a customer or contract number, a UUID for instance.
    organization_identifier: Option<String>,
}

/// An issued bootstrap certificate, never with its private key: the key is in the PKCS#12 file of
/// `GET /api/v1/bootstrap-certificates/{serial}/p12` (SEC-bootstrap-certificates-and-enrolment §3).
#[derive(Serialize, ToSchema)]
struct IssuedBootstrap {
    /// What a revocation names, with the authority `bootstrap`.
    serial: String,
    subject: String,
    not_before_ms: u64,
    not_after_ms: u64,
    certificate_pem: String,
}

/// A bootstrap certificate on record (SRV-state-stores-contract-pair §11).
#[derive(Serialize, ToSchema)]
struct BootstrapView {
    serial: String,
    subject: String,
    /// The SHA-256 fingerprint of its public key, hex.
    key_fingerprint: String,
    issued_ms: u64,
    not_after_ms: u64,
    revoked: bool,
    /// Whether its PKCS#12 file can be downloaded (SRV-state-stores-contract-pair §11).
    p12: bool,
}

/// The valid bootstrap certificates this Server issued, oldest first.
#[utoipa::path(
    get,
    path = "/api/v1/bootstrap-certificates",
    tag = "enrolment",
    responses(
        (status = 200, description = "The valid bootstrap certificates, oldest first", body = Vec<BootstrapView>),
        (status = 404, description = "Enrolment is not configured", body = ErrorBody)
    )
)]
async fn list_bootstrap_certificates(State(state): State<Arc<OperatorCases>>) -> Response {
    let Some(certificates) = state.bootstrap_certificates() else {
        return enrolment_off();
    };
    let revoked = |id: &crate::revocation::CertId| state.is_certificate_revoked(id);
    let list: Vec<BootstrapView> = certificates
        .into_iter()
        .map(|entry| BootstrapView {
            revoked: revoked(&entry.id),
            p12: entry.p12 && !revoked(&entry.id),
            serial: entry.serial,
            subject: entry.subject,
            key_fingerprint: entry.key_fingerprint,
            issued_ms: entry.issued_ms,
            not_after_ms: entry.not_after_ms,
        })
        .collect();
    Json(list).into_response()
}

/// Issues a bootstrap certificate with the subject an operator names, which holds at least an
/// organization and an organizational unit, under the password the request names. The key is in no
/// answer: the Server keeps it, encrypted under that password in a PKCS#12 file, for download
/// (SEC-bootstrap-certificates-and-enrolment §3). A host with it may only ask to enrol, inside an open window, and
/// an operator approves (SEC-bootstrap-certificates-and-enrolment §9 and §10).
#[utoipa::path(
    post,
    path = "/api/v1/bootstrap-certificates",
    tag = "enrolment",
    request_body = IssueBootstrapRequest,
    responses(
        (status = 201, description = "Issued; the certificate, its key kept in the PKCS#12 file only", body = IssuedBootstrap),
        (status = 400, description = "No organization, organizational_unit or password, a subject attribute this route does not know, a country that is not two upper-case letters, another value out of bounds, validity_days outside 1 to 90, or a password outside 8 to 128 characters", body = ErrorBody),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "Enrolment is not configured", body = ErrorBody),
        (status = 409, description = "No bootstrap_ca_key_file is configured, the bootstrap CA has ended, or the record is full", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was handed out — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn issue_bootstrap_certificate(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Json(body): Json<serde_json::Value>,
) -> Response {
    use crate::bootstrap::IssueError;
    if !state.has_bootstrap() {
        return enrolment_off();
    }
    let request = match serde_json::from_value::<IssueBootstrapRequest>(body) {
        Ok(request) => {
            let subject = request.subject.unwrap_or_default();
            crate::bootstrap::IssueRequest {
                subject: crate::bootstrap::Subject {
                    country: subject.country,
                    state: subject.state,
                    locality: subject.locality,
                    organization: subject.organization,
                    organizational_unit: subject.organizational_unit,
                    organization_identifier: subject.organization_identifier,
                },
                validity_days: request.validity_days,
                password: request.password,
            }
        }
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    // Off the async workers: sealing a PKCS#12 file takes the better part of a second, as
    // password hashing does (SEC-bootstrap-certificates-and-enrolment).
    let issuer = state.clone();
    let issued = match tokio::task::spawn_blocking(move || issuer.issue_bootstrap(&request)).await {
        Ok(Some(issued)) => issued,
        Ok(None) => return enrolment_off(),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match issued {
        Ok(pair) => {
            // The key leaves only once the issuance is on the record (SEC-append-only-audit-record): a refused record
            // leaves a listed, revocable entry and no key in anyone's hands.
            let recorded = state.record(
                crate::audit::Entry::new("enrolment.bootstrap_issued", "issued")
                    .with("serial", pair.serial.clone())
                    .with("subject", pair.subject.clone())
                    .with("not_after_ms", pair.not_after_ms)
                    .with("p12", "true"),
            );
            if recorded.is_err() {
                // Nor later: an issuance that is not on the record keeps no file to download.
                let _ = state.forget_bootstrap_p12(&pair.serial);
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the audit record cannot be written now; nothing was handed out",
                );
            }
            info!(serial = %pair.serial, subject = %pair.subject, "bootstrap certificate issued");
            (
                StatusCode::CREATED,
                [(header::CACHE_CONTROL, "no-store")],
                Json(IssuedBootstrap {
                    serial: pair.serial,
                    subject: pair.subject,
                    not_before_ms: pair.not_before_ms,
                    not_after_ms: pair.not_after_ms,
                    certificate_pem: pair.certificate_pem,
                }),
            )
                .into_response()
        }
        Err(IssueError::NoKey) => error(
            StatusCode::CONFLICT,
            "this Server does not hold the bootstrap CA's key — set [enrolment] \
             bootstrap_ca_key_file",
        ),
        Err(IssueError::Invalid(e)) => error(StatusCode::BAD_REQUEST, e),
        Err(IssueError::Full) => error(
            StatusCode::CONFLICT,
            format!(
                "{} valid bootstrap certificates are on record already",
                crate::bootstrap::MAX_VALID
            ),
        ),
        Err(IssueError::CaEnded) => error(
            StatusCode::CONFLICT,
            "the bootstrap CA has ended — replace it with `opamp-fleetctl pki bootstrap-ca`",
        ),
        Err(IssueError::Failed(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// The PKCS#12 file of a bootstrap certificate, as often as it is asked,
/// until the certificate ends or is revoked (SRV-state-stores-contract-pair §11). Each download is on the record.
#[utoipa::path(
    get,
    path = "/api/v1/bootstrap-certificates/{serial}/p12",
    tag = "enrolment",
    params(("serial" = String, Path, description = "the serial, as the list names it")),
    responses(
        (status = 200, description = "The PKCS#12 file, `bootstrap.p12`", content_type = "application/x-pkcs12", body = Vec<u8>),
        (status = 404, description = "No such certificate, no file kept for it, revoked or ended — or enrolment is not configured", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was handed out — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn download_bootstrap_p12(
    State(state): State<Arc<OperatorCases>>,
    Path(serial): Path<String>,
) -> Response {
    let none = || {
        error(
            StatusCode::NOT_FOUND,
            format!("no PKCS#12 file for the bootstrap certificate {serial:?}"),
        )
    };
    let (entry, file) = match state.bootstrap_p12(&serial) {
        Some(Ok(Some(found))) => found,
        Some(Ok(None)) => return none(),
        Some(Err(e)) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
        None => return enrolment_off(),
    };
    // A revocation removes the file before it is answered; one that could not is still no
    // licence to hand it out.
    if state.is_certificate_revoked(&entry.id) {
        let _ = state.forget_bootstrap_p12(&entry.serial);
        return none();
    }
    let recorded = state.record(
        crate::audit::Entry::new("enrolment.bootstrap_p12_downloaded", "downloaded")
            .with("serial", entry.serial.clone()),
    );
    if recorded.is_err() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "the audit record cannot be written now; nothing was handed out",
        );
    }
    info!(serial = %entry.serial, "bootstrap PKCS#12 file downloaded");
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/x-pkcs12".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", fleet_core::pkcs12::FILE_NAME),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        file,
    )
        .into_response()
}

/// Deletes a revoked bootstrap certificate's entry, and any PKCS#12 file of it, from the record.
/// Its revocation stays on the list, so it stays refused for as long as it is valid (SEC-bootstrap-certificates-and-enrolment §7).
#[utoipa::path(
    delete,
    path = "/api/v1/bootstrap-certificates/{serial}",
    tag = "enrolment",
    params(("serial" = String, Path, description = "the serial, as the list names it")),
    responses(
        (status = 204, description = "Deleted from the record; the revocation stays"),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No such certificate on record, or enrolment is not configured", body = ErrorBody),
        (status = 409, description = "The certificate is not revoked — revoke it first", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was deleted — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn delete_bootstrap_certificate(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Path(serial): Path<String>,
) -> Response {
    use crate::bootstrap::DeleteError;
    let revoked = |entry: &crate::bootstrap::Entry| state.is_certificate_revoked(&entry.id);
    let recorded = |entry: &crate::bootstrap::Entry| {
        state
            .record(
                crate::audit::Entry::new("enrolment.bootstrap_deleted", "deleted")
                    .with("serial", entry.serial.clone())
                    .with("subject", entry.subject.clone()),
            )
            .is_ok()
    };
    let Some(outcome) = state.delete_bootstrap(&serial, revoked, recorded) else {
        return enrolment_off();
    };
    match outcome {
        Ok(entry) => {
            info!(serial = %entry.serial, "revoked bootstrap certificate deleted from the record");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(DeleteError::NotFound) => error(
            StatusCode::NOT_FOUND,
            format!("no bootstrap certificate {serial:?} on record"),
        ),
        Err(DeleteError::NotRevoked) => error(
            StatusCode::CONFLICT,
            format!("the bootstrap certificate {serial:?} is not revoked — revoke it first"),
        ),
        Err(DeleteError::Unrecorded) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "the audit record cannot be written now; nothing was deleted",
        ),
        Err(DeleteError::Failed(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

fn enrolment_off() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "enrolment is not configured — set [enrolment] bootstrap_ca_file",
    )
}

fn window_view(until_ms: Option<u64>) -> Response {
    Json(EnrolmentWindow {
        open: until_ms.is_some(),
        until_ms,
    })
    .into_response()
}

/// Whether hosts may enrol now, and until when.
#[utoipa::path(
    get,
    path = "/api/v1/enrolment/window",
    tag = "enrolment",
    responses(
        (status = 200, description = "Whether the window is open, and until when", body = EnrolmentWindow),
        (status = 404, description = "Enrolment is not configured", body = ErrorBody)
    )
)]
async fn get_enrolment_window(State(state): State<Arc<OperatorCases>>) -> Response {
    match state.enrolment_window() {
        Some(until) => window_view(until),
        None => enrolment_off(),
    }
}

/// Opens the enrolment window for a bounded time, or moves its end. It is closed by default, and
/// persisted before this answers, so a Server restart keeps it open until its end (SRV-state-stores-contract-pair).
#[utoipa::path(
    post,
    path = "/api/v1/enrolment/window",
    tag = "enrolment",
    request_body = OpenWindow,
    responses(
        (status = 200, description = "The window, open until its new end", body = EnrolmentWindow),
        (status = 400, description = "open_for_secs is out of range", body = ErrorBody),
        (status = 404, description = "Enrolment is not configured", body = ErrorBody),
        (status = 500, description = "The window could not be persisted; it is as it was", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn open_enrolment_window(
    State(state): State<Arc<OperatorCases>>,
    Json(spec): Json<OpenWindow>,
) -> Response {
    let Some(opened) = state.open_enrolment_window(spec.open_for_secs) else {
        return enrolment_off();
    };
    match opened {
        Ok(until) => {
            info!(secs = spec.open_for_secs, "enrolment window opened");
            state.note(
                crate::audit::Entry::new("enrolment.window", "opened").with("until_ms", until),
            );
            window_view(Some(until))
        }
        Err(crate::enrolment::WindowError::Invalid(e)) => error(StatusCode::BAD_REQUEST, e),
        Err(crate::enrolment::WindowError::Store(e)) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the enrolment window could not be persisted: {e}"),
        ),
    }
}

/// Closes the enrolment window now: every enrolling connection ends, and every pending request
/// expires.
#[utoipa::path(
    delete,
    path = "/api/v1/enrolment/window",
    tag = "enrolment",
    responses(
        (status = 204, description = "Closed"),
        (status = 404, description = "Enrolment is not configured", body = ErrorBody),
        (status = 500, description = "Closed, but not persisted: a restart may find the window open until its end", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn close_enrolment_window(State(state): State<Arc<OperatorCases>>) -> Response {
    if !state.has_enrolment() {
        return enrolment_off();
    }
    // The window is closed only with its entry's place held. It is closed here whatever the store
    // answers, so the entry says `closed` either way.
    let Ok(place) = state.hold_place() else {
        return crate::answers::busy();
    };
    let Some((expired, saved)) = state.close_enrolment_window() else {
        return enrolment_off();
    };
    info!("enrolment window closed");
    fill_place(
        place,
        crate::audit::Entry::new("enrolment.window", "closed").with("expired_requests", expired),
    );
    match saved {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "the enrolment window is closed, but that could not be persisted — a restart may \
                 find it open until its end: {e}"
            ),
        ),
    }
}

/// The enrolment requests waiting for an operator, oldest first.
#[utoipa::path(
    get,
    path = "/api/v1/enrolments",
    tag = "enrolment",
    responses(
        (status = 200, description = "The waiting enrolment requests, oldest first", body = Vec<PendingEnrolment>),
        (status = 404, description = "Enrolment is not configured", body = ErrorBody)
    )
)]
async fn list_enrolments(State(state): State<Arc<OperatorCases>>) -> Response {
    let Some(pending) = state.enrolments() else {
        return enrolment_off();
    };
    let pending: Vec<PendingEnrolment> = pending
        .into_iter()
        .map(|p| PendingEnrolment {
            id: p.id,
            arrived_ms: p.arrived_ms,
            peer: p.requester.peer.map(|ip| ip.to_string()),
            gateway: p.requester.gateway.clone(),
            key_fingerprint: p.key_fingerprint,
            bootstrap_subject: p.requester.carried.clone().ok().map(Into::into),
            subject_refusal: p.requester.carried.err(),
            bootstrap_fingerprint: p.requester.bootstrap_fingerprint,
            bootstrap_serial: p.requester.bootstrap.serial,
        })
        .collect();
    Json(pending).into_response()
}

/// Approves one enrolment request: the client CA signs it, and the host is handed its certificate.
#[utoipa::path(
    post,
    path = "/api/v1/enrolments/{id}/approve",
    tag = "enrolment",
    params(("id" = String, Path, description = "the request's id")),
    responses(
        (status = 204, description = "Approved and signed"),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No such pending request, or enrolment is not configured", body = ErrorBody),
        (status = 409, description = "The CA could not sign the request, its bootstrap certificate is revoked, that certificate's subject breaks the rules of SEC-bootstrap-certificates-and-enrolment §1 — the message names the attribute —, or another Server instance is deciding the request at this moment; nothing is signed", body = ErrorBody),
        (status = 500, description = "The approval could not be persisted; the request still waits", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn approve_enrolment(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Path(id): Path<String>,
) -> Response {
    match state.approve_enrolment(&id) {
        Ok(serial) => {
            info!(request = %id, serial = %serial, "enrolment request approved");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(crate::enrolment::DecisionError::NotFound) => error(
            StatusCode::NOT_FOUND,
            format!("no pending enrolment request {id:?}"),
        ),
        Err(crate::enrolment::DecisionError::Revoked) => error(
            StatusCode::CONFLICT,
            format!(
                "enrolment request {id:?} came with a bootstrap certificate that is revoked; it is \
                 withdrawn, and nothing was signed"
            ),
        ),
        Err(
            crate::enrolment::DecisionError::Subject(e) | crate::enrolment::DecisionError::Sign(e),
        ) => error(StatusCode::CONFLICT, e),
        Err(crate::enrolment::DecisionError::Deciding) => deciding(&id),
        Err(crate::enrolment::DecisionError::Store(e)) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the approval could not be persisted, and the request still waits: {e}"),
        ),
    }
}

/// The answer to a decision on a request another Server instance is deciding at this moment.
fn deciding(id: &str) -> Response {
    error(
        StatusCode::CONFLICT,
        format!(
            "another Server instance is deciding enrolment request {id:?} at this moment, and \
             nothing was decided here; asked again once it is decided, this answers 404"
        ),
    )
}

/// Rejects the key of one enrolment request, beyond its window: a request waiting for it is
/// decided and its connection closed, and a request for it is refused whenever it arrives. An id
/// the queue does not hold is rejected before its request arrives, with the window open or closed.
#[utoipa::path(
    post,
    path = "/api/v1/enrolments/{id}/reject",
    tag = "enrolment",
    params(("id" = String, Path, description = "the request's id, the SHA-256 fingerprint of its key: 64 lowercase hexadecimal digits")),
    responses(
        (status = 204, description = "Rejected, whether the queue held the request or not; a request for the key is refused from now on"),
        (status = 400, description = "The id is not 64 lowercase hexadecimal digits", body = ErrorBody),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "The queue holds the request decided already, or enrolment is not configured", body = ErrorBody),
        (status = 409, description = "Another Server instance is deciding the request at this moment; nothing is decided", body = ErrorBody),
        (status = 500, description = "The rejection could not be persisted; the request still waits", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn reject_enrolment(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Path(id): Path<String>,
) -> Response {
    if !state.has_enrolment() {
        return enrolment_off();
    }
    if !crate::enrolment::is_key_id(&id) {
        return error(
            StatusCode::BAD_REQUEST,
            format!(
                "{id:?} is not an enrolment request's id: the SHA-256 fingerprint of its key, 64 \
                 lowercase hexadecimal digits"
            ),
        );
    }
    let gateway = state.enrolment_gateway(&id);
    // A request is rejected only with its entry's place held, so a restore rejects it again; only
    // a rejection known to have failed gives the place back unwritten.
    let Ok(place) = state.hold_place() else {
        return crate::answers::busy();
    };
    let rejection = |outcome: &str| {
        crate::audit::Entry::new("enrolment.rejected", outcome)
            .with("id", id.clone())
            .with("gateway", gateway.clone())
    };
    let Some(outcome) = state.reject_enrolment(&id) else {
        return enrolment_off();
    };
    match outcome {
        Ok(rejected) => {
            let queued = rejected == crate::enrolment::Rejection::Queued;
            info!(request = %id, queued, "enrolment request rejected");
            fill_place(place, rejection("rejected").with("queued", queued));
            StatusCode::NO_CONTENT.into_response()
        }
        Err(crate::enrolment::DecisionError::Deciding) => deciding(&id),
        Err(crate::enrolment::DecisionError::Store(e)) => {
            // The store may hold it all the same, so the record names it, toward rejecting it
            // again at a restore.
            fill_place(place, rejection("unknown"));
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the rejection could not be persisted, and the request still waits: {e}"),
            )
        }
        Err(_) => error(
            StatusCode::NOT_FOUND,
            format!("enrolment request {id:?} is decided already"),
        ),
    }
}

/// One certificate the client CA signed (SRV-state-stores-contract-pair §9).
#[derive(Serialize, ToSchema)]
struct CertificateView {
    /// `client` — the CA that signed it, as a revocation names it.
    authority: String,
    /// The issuer's name, for reading.
    issuer: String,
    /// The serial, lowercase hex.
    serial: String,
    subject: String,
    /// The SHA-256 fingerprint of the certified public key, hex.
    key_fingerprint: String,
    not_after_ms: u64,
    /// The `instance_uid` of the message that carried the CSR, hex.
    instance_uid: String,
    issued_ms: u64,
    /// On a renewal, the certificate it renewed.
    #[serde(skip_serializing_if = "Option::is_none")]
    predecessor: Option<CertificateRef>,
    /// The host it was issued to (SEC-bootstrap-certificates-and-enrolment §11); absent on one an operator provisioned.
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
}

/// A Host: what a client certificate's host id names (SEC-bootstrap-certificates-and-enrolment §11, SRV-fleet-record-agents-hosts).
#[derive(Serialize, ToSchema)]
struct HostView {
    host: String,
    /// What it is shown by: the `host.name` its Client Agent reports, else the host id. Shown
    /// only; it decides nothing (SRV-fleet-record-agents-hosts §24).
    name: String,
    /// A Gateway speaks for any Agent; any other host only for those it reported first.
    gateway: bool,
    /// The `instance_uid`s it speaks for, hex.
    instance_uids: Vec<String>,
    /// How many valid certificates it holds — at most three.
    certificates: usize,
    /// The subject values of the client certificate it was issued (SRV-fleet-record-agents-hosts §15), which the
    /// Agents arriving over it directly carry as `fleet.host.*`.
    subject: HostSubject,
    /// When its enrolment was approved, in milliseconds since the Unix epoch; absent for a Host a
    /// provisioned certificate made at its first report.
    #[serde(skip_serializing_if = "Option::is_none")]
    approved_ms: Option<u64>,
    /// The Agents whose record names it as the Host they last arrived over (SRV-fleet-record-agents-hosts §16) —
    /// for a Gateway, the Agents behind it.
    agents: Vec<HostAgent>,
}

/// The subject values a Host carries (SRV-fleet-record-agents-hosts §15), or a bootstrap certificate carries into
/// the host certificate an approval issues (CLT-modes-and-gateway §23); an empty one is absent from the
/// certificate.
#[derive(Serialize, ToSchema)]
struct HostSubject {
    /// `C`, as `fleet.host.country`.
    country: String,
    /// `ST`, as `fleet.host.state`.
    state: String,
    /// `L`, as `fleet.host.locality`.
    locality: String,
    /// `O`, as `fleet.host.organization`.
    organization: String,
    /// `OU`, as `fleet.host.site`.
    organizational_unit: String,
    /// `organizationIdentifier`, as `fleet.host.organization_id`.
    organization_identifier: String,
}

impl From<crate::revocation::SubjectValues> for HostSubject {
    fn from(values: crate::revocation::SubjectValues) -> Self {
        HostSubject {
            country: values.country,
            state: values.state,
            locality: values.locality,
            organization: values.organization,
            organizational_unit: values.organizational_unit,
            organization_identifier: values.organization_identifier,
        }
    }
}

/// One Agent under the Host it arrived over (SRV-fleet-record-agents-hosts §24).
#[derive(Serialize, ToSchema)]
struct HostAgent {
    instance_uid: String,
    service_name: String,
    service_instance_name: String,
    /// The `host.name` it reports, or empty.
    host_name: String,
    /// Whether it is a Client Agent: it reports `supervisor.kind.*`.
    client: bool,
    /// The Instance UID of its Client Agent — its own, for the Client Agent — or empty when the
    /// Server cannot tell.
    client_agent: String,
    /// Behind a Gateway, the machine it is grouped by (`host.id=…`, else `host.name=…`); empty on a
    /// direct connection.
    machine: String,
    /// Its group holds more than one Client Agent, so it inherits nothing.
    ambiguous: bool,
    connected: bool,
}

/// Whether a host is a Gateway.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct GatewayRequest {
    gateway: bool,
}

/// A certificate by the CA that issued it and its serial.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct CertificateRef {
    /// `client` for the client CA, `bootstrap` for the bootstrap CA of `[enrolment]`.
    authority: String,
    /// The serial in hex, as `openssl x509 -noout -serial` prints it; colons, spaces, case and
    /// leading zeros are ignored.
    serial: String,
}

/// What to revoke: a certificate (SEC-admission-by-client-certificate §27).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct RevokeRequest {
    certificate: CertificateRef,
}

/// One revocation (SEC-admission-by-client-certificate §30).
#[derive(Serialize, ToSchema)]
struct RevocationView {
    /// What `DELETE` names.
    id: String,
    /// `certificate`.
    kind: &'static str,
    revoked_ms: u64,
    certificate: CertificateRef,
}

impl RevocationView {
    fn of(entry: crate::revocation::Revocation) -> Self {
        let crate::revocation::Revoked::Certificate {
            authority, serial, ..
        } = entry.revoked;
        RevocationView {
            id: entry.id,
            kind: "certificate",
            revoked_ms: entry.revoked_ms,
            certificate: CertificateRef { authority, serial },
        }
    }
}

/// Writes what an act did into the place [`OperatorCases::hold_place`] held for it.
fn fill_place(place: Option<crate::audit::Permit>, entry: crate::audit::Entry) {
    if let Some(place) = place {
        place.write(entry);
    }
}

fn revocation_entry(
    event: &str,
    outcome: &str,
    entry: &crate::revocation::Revocation,
) -> crate::audit::Entry {
    let crate::revocation::Revoked::Certificate {
        authority, serial, ..
    } = &entry.revoked;
    crate::audit::Entry::new(event, outcome)
        .with("id", entry.id.clone())
        .with("kind", "certificate")
        .with("authority", authority.clone())
        .with("serial", serial.clone())
}

fn revocation_off() -> Response {
    error(StatusCode::NOT_FOUND, "revocation is not configured")
}

/// Every certificate the client CA signed that has not yet expired, oldest first, a page at a
/// time where `limit` asks for one.
#[utoipa::path(
    get,
    path = "/api/v1/certificates",
    tag = "revocation",
    params(
        ("limit" = Option<u32>, Query, description = LIMIT_DOC),
        ("cursor" = Option<String>, Query, description = "The position the page starts after, from the Link of the page before"),
        ("q" = Option<String>, Query, description = "A case-insensitive substring of the subject, the serial or the host")
    ),
    responses(
        (status = 200, description = "Every unexpired certificate the client CA signed that matches, by the time it was issued, then by issuer and serial", body = Vec<CertificateView>,
            headers(
                ("X-Total-Count" = u64, description = "How many certificates match the filters"),
                ("Link" = String, description = "The next page, rel=\"next\"; absent on the last")
            )),
        (status = 400, description = "A parameter this route does not take, one that is not valid, a cursor that names no position of this list, or more than 10000 without limit", body = ErrorBody),
        (status = 404, description = "Revocation is not configured", body = ErrorBody)
    )
)]
async fn list_certificates(
    State(state): State<Arc<OperatorCases>>,
    RawQuery(raw): RawQuery,
) -> Response {
    if !state.has_revocations() {
        return revocation_off();
    }
    let asked = match Asked::parse(raw.as_deref(), &["q"]) {
        Ok(asked) => asked,
        Err((status, message)) => return error(status, message),
    };
    let after = match asked.after_keyed(false) {
        Ok(after) => after,
        Err((status, message)) => return error(status, message),
    };
    let q = asked.q();
    let authority = |id: &crate::revocation::CertId| {
        state
            .authority_role(&id.issuer)
            .unwrap_or_else(|| "unknown".to_string())
    };
    let issued: Vec<(crate::listing::Position, CertificateView)> = state
        .issued_certificates()
        .unwrap_or_default()
        .into_iter()
        .filter(|issued| {
            q.as_ref().is_none_or(|q| {
                [
                    issued.facts.subject.as_str(),
                    issued.facts.id.serial.as_str(),
                    issued.facts.host.as_deref().unwrap_or_default(),
                ]
                .iter()
                .any(|field| field.to_lowercase().contains(q.as_str()))
            })
        })
        .map(|issued| {
            let position = crate::listing::Position {
                key: crate::listing::Key::Int(issued.issued_ms as i64),
                tiebreak: format!("{}\n{}", issued.facts.id.issuer, issued.facts.id.serial),
            };
            let view = CertificateView {
                authority: authority(&issued.facts.id),
                issuer: issued.facts.issuer_name,
                serial: issued.facts.id.serial,
                subject: issued.facts.subject,
                key_fingerprint: issued.facts.key_fingerprint,
                not_after_ms: issued.facts.not_after_ms,
                instance_uid: issued.instance_uid,
                issued_ms: issued.issued_ms,
                predecessor: issued.predecessor.map(|id| CertificateRef {
                    authority: authority(&id),
                    serial: id.serial,
                }),
                host: issued.facts.host,
            };
            (position, view)
        })
        .collect();
    let page = paging::cut(&asked, after.as_ref(), issued);
    asked.answer("/api/v1/certificates", page.entries, page.total, page.next)
}

/// Every Host, by host id, with its subject values and the Agents that arrived over it, a page at
/// a time where `limit` asks for one.
#[utoipa::path(
    get,
    path = "/api/v1/hosts",
    tag = "revocation",
    params(
        ("limit" = Option<u32>, Query, description = LIMIT_DOC),
        ("cursor" = Option<String>, Query, description = "The position the page starts after, from the Link of the page before"),
        ("q" = Option<String>, Query, description = "A case-insensitive substring of the host id or one of its subject values"),
        ("gateway" = Option<bool>, Query, description = "Only the Gateways, or only the Hosts that are none")
    ),
    responses(
        (status = 200, description = "Every Host that matches, by host id, with its subject values and the Agents that arrived over it (SRV-fleet-record-agents-hosts)", body = Vec<HostView>,
            headers(
                ("X-Total-Count" = u64, description = "How many Hosts match the filters"),
                ("Link" = String, description = "The next page, rel=\"next\"; absent on the last")
            )),
        (status = 400, description = "A parameter this route does not take, one that is not valid, a cursor that names no position of this list, or more than 10000 without limit", body = ErrorBody),
        (status = 404, description = "Revocation is not configured", body = ErrorBody),
        (status = 500, description = "The fleet cannot be read from the store", body = ErrorBody)
    )
)]
async fn list_hosts(State(state): State<Arc<OperatorCases>>, RawQuery(raw): RawQuery) -> Response {
    if !state.has_revocations() {
        return revocation_off();
    }
    let asked = match Asked::parse(raw.as_deref(), &["q", "gateway"]) {
        Ok(asked) => asked,
        Err((status, message)) => return error(status, message),
    };
    let (after, gateway) = match (asked.after_keyed(true), asked.flag("gateway")) {
        (Ok(after), Ok(gateway)) => (after, gateway),
        (Err((status, message)), _) | (_, Err((status, message))) => return error(status, message),
    };
    let q = asked.q();
    let matching: Vec<_> = state
        .hosts()
        .unwrap_or_default()
        .into_iter()
        .filter(|(host, entry, _)| {
            gateway.is_none_or(|gateway| gateway == entry.gateway)
                && q.as_ref().is_none_or(|q| {
                    std::iter::once(host.as_str())
                        .chain(entry.subject.attributes().map(|(_, value)| value))
                        .any(|field| field.to_lowercase().contains(q.as_str()))
                })
        })
        .map(|(host, entry, certificates)| {
            let position = crate::listing::Position {
                key: crate::listing::Key::Text(host.clone()),
                tiebreak: host.clone(),
            };
            (position, (host, entry, certificates))
        })
        .collect();
    let page = paging::cut(&asked, after.as_ref(), matching);
    let mut carried = match state.host_agents() {
        Ok(carried) => carried,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let hosts: Vec<HostView> = page
        .entries
        .into_iter()
        .map(|(host, entry, certificates)| {
            let agents = carried.remove(&host).unwrap_or_default();
            HostView {
                name: agents.name.unwrap_or_else(|| host.clone()),
                host,
                gateway: entry.gateway,
                instance_uids: entry.instance_uids.into_iter().collect(),
                certificates,
                subject: entry.subject.into(),
                approved_ms: entry.approved_ms,
                agents: agents
                    .agents
                    .into_iter()
                    .map(|agent| HostAgent {
                        instance_uid: agent.instance_uid,
                        service_name: agent.service_name,
                        service_instance_name: agent.service_instance_name,
                        host_name: agent.host_name,
                        client: agent.client,
                        client_agent: agent.client_agent,
                        machine: agent.machine,
                        ambiguous: agent.ambiguous,
                        connected: agent.connected,
                    })
                    .collect(),
            }
        })
        .collect();
    asked.answer("/api/v1/hosts", hosts, page.total, page.next)
}

/// Marks a host as a Gateway, which speaks for the Agents behind it, or takes the mark away.
#[utoipa::path(
    put,
    path = "/api/v1/hosts/{host}/gateway",
    tag = "revocation",
    params(("host" = String, Path, description = "the host, as its certificates name it")),
    request_body = GatewayRequest,
    responses(
        (status = 204, description = "Set"),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No such host, or revocation is not configured", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn set_gateway(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Path(host): Path<String>,
    Json(request): Json<GatewayRequest>,
) -> Response {
    if !state.has_revocations() {
        return revocation_off();
    }
    // A mark is set or cleared only with its entry's place held, so a restore finds every mark
    // cleared; only a change known to have failed gives the place back unwritten.
    let Ok(place) = state.hold_place() else {
        return crate::answers::busy();
    };
    let mark = |outcome: &str| {
        crate::audit::Entry::new("host.gateway", outcome)
            .with("host", host.clone())
            .with("gateway", request.gateway)
    };
    // Off the async workers: a mark of a Host this instance held no copy of reads the ledger
    // again before it answers.
    let marked = tokio::task::spawn_blocking({
        let (state, host, gateway) = (state.clone(), host.clone(), request.gateway);
        move || state.set_gateway(&host, gateway)
    })
    .await
    .unwrap_or_else(|e| Err(format!("the mark ended: {e}")));
    match marked {
        Ok(true) => {
            info!(%host, gateway = request.gateway, "gateway mark set");
            fill_place(place, mark(if request.gateway { "set" } else { "cleared" }));
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no host {host:?}")),
        Err(e) => {
            // The store may hold it all the same, so the record names what was asked; a restore
            // clears the mark, whichever it was (toward withdrawing trust).
            fill_place(place, mark("unknown"));
            error(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    }
}

/// The revocation list, oldest first, a page at a time where `limit` asks for one.
#[utoipa::path(
    get,
    path = "/api/v1/revocations",
    tag = "revocation",
    params(
        ("limit" = Option<u32>, Query, description = LIMIT_DOC),
        ("cursor" = Option<String>, Query, description = "The position the page starts after, from the Link of the page before"),
        ("q" = Option<String>, Query, description = "A case-insensitive substring of the serial, or of the subject or the host of the certificate revoked while the register holds it")
    ),
    responses(
        (status = 200, description = "The revocations that match, by the time they were made, then by id", body = Vec<RevocationView>,
            headers(
                ("X-Total-Count" = u64, description = "How many revocations match the filters"),
                ("Link" = String, description = "The next page, rel=\"next\"; absent on the last")
            )),
        (status = 400, description = "A parameter this route does not take, one that is not valid, a cursor that names no position of this list, or more than 10000 without limit", body = ErrorBody),
        (status = 404, description = "Revocation is not configured", body = ErrorBody)
    )
)]
async fn list_revocations(
    State(state): State<Arc<OperatorCases>>,
    RawQuery(raw): RawQuery,
) -> Response {
    if !state.has_revocations() {
        return revocation_off();
    }
    let asked = match Asked::parse(raw.as_deref(), &["q"]) {
        Ok(asked) => asked,
        Err((status, message)) => return error(status, message),
    };
    let after = match asked.after_keyed(false) {
        Ok(after) => after,
        Err((status, message)) => return error(status, message),
    };
    let q = asked.q();
    // What the register says of a revoked certificate, while it holds it.
    let issued: BTreeMap<(String, String), (String, String)> = match &q {
        None => BTreeMap::new(),
        Some(_) => state
            .issued_certificates()
            .unwrap_or_default()
            .into_iter()
            .map(|issued| {
                (
                    (issued.facts.id.issuer, issued.facts.id.serial),
                    (issued.facts.subject, issued.facts.host.unwrap_or_default()),
                )
            })
            .collect(),
    };
    let matching: Vec<(crate::listing::Position, RevocationView)> = state
        .revocation_list()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| {
            let Some(q) = &q else { return true };
            let crate::revocation::Revoked::Certificate {
                issuers, serial, ..
            } = &entry.revoked;
            serial.to_lowercase().contains(q.as_str())
                || issuers.iter().any(|issuer| {
                    issued
                        .get(&(issuer.clone(), serial.clone()))
                        .is_some_and(|(subject, host)| {
                            subject.to_lowercase().contains(q.as_str())
                                || host.to_lowercase().contains(q.as_str())
                        })
                })
        })
        .map(|entry| {
            let position = crate::listing::Position {
                key: crate::listing::Key::Int(entry.revoked_ms as i64),
                tiebreak: entry.id.clone(),
            };
            (position, RevocationView::of(entry))
        })
        .collect();
    let page = paging::cut(&asked, after.as_ref(), matching);
    asked.answer("/api/v1/revocations", page.entries, page.total, page.next)
}

/// Revokes a certificate, and every renewal of it. Every session it admitted ends at once, and no
/// new one is admitted. There is no credential to revoke: the Agent plane admits by client
/// certificate alone (SEC-admission-by-client-certificate §29).
#[utoipa::path(
    post,
    path = "/api/v1/revocations",
    tag = "revocation",
    request_body = RevokeRequest,
    responses(
        (status = 201, description = "Revoked; the new entry of the list", body = RevocationView),
        (status = 400, description = "No certificate named, a field this route does not know, an authority this Server does not have, a serial that is not hex, or a credential named, which the Agent plane does not have", body = ErrorBody),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "Revocation is not configured", body = ErrorBody),
        (status = 507, description = "The list is full", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn revoke(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Json(body): Json<serde_json::Value>,
) -> Response {
    use crate::revocation::RevokeError;
    if !state.has_revocations() {
        return revocation_off();
    }
    // Named before the body is read as a request, so the answer says why rather than that the
    // field is unknown.
    if body.get("credential").is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "credential is refused — the Agent plane admits by client certificate alone, so \
             there is no credential to revoke; name a certificate",
        );
    }
    let request = match serde_json::from_value::<RevokeRequest>(body) {
        Ok(request) => request,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    // A revocation is made only with its entry's place held, so the record names every certificate
    // revoked; only one known to have failed gives the place back unwritten.
    let Ok(place) = state.hold_place() else {
        return crate::answers::busy();
    };
    let Some(outcome) =
        state.revoke_certificate(&request.certificate.authority, &request.certificate.serial)
    else {
        return revocation_off();
    };
    match outcome {
        Ok(entry) => {
            info!(revocation = %entry.id, "revoked");
            // A revoked bootstrap certificate's PKCS#12 file goes before the answer (SRV-state-stores-contract-pair §11);
            // the download refuses it either way.
            let crate::revocation::Revoked::Certificate {
                authority,
                issuers,
                serial,
            } = &entry.revoked;
            let mut withdrawn = 0;
            if authority == "bootstrap" {
                // The requests it carried wait no longer, on every instance (SEC-admission-by-client-certificate §28);
                // approving one is refused either way.
                withdrawn = state.withdraw_revoked(issuers, serial);
            }
            // Written before anything after the revocation can fail: the revocation is made.
            fill_place(
                place,
                revocation_entry("revocation.revoked", "revoked", &entry)
                    .with("withdrawn_requests", withdrawn),
            );
            if authority == "bootstrap" {
                if let Some(Err(e)) = state.forget_bootstrap_p12(serial) {
                    // Said, not swallowed: lifting this revocation would make the file
                    // downloadable again.
                    warn!(serial = %serial, "cannot remove the revoked PKCS#12 file: {e}");
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "revoked as {}, but its PKCS#12 file could not be removed: {e}",
                            entry.id
                        ),
                    );
                }
            }
            (StatusCode::CREATED, Json(RevocationView::of(entry))).into_response()
        }
        Err(RevokeError::Invalid(e)) => error(StatusCode::BAD_REQUEST, e),
        Err(RevokeError::Full) => error(
            StatusCode::INSUFFICIENT_STORAGE,
            format!(
                "the revocation list holds {} entries already",
                crate::revocation::MAX_REVOCATIONS
            ),
        ),
        Err(RevokeError::Store {
            error: e,
            attempted,
        }) => {
            // The store may hold it all the same, so the record names it, toward revoking it
            // again at a restore.
            fill_place(
                place,
                revocation_entry("revocation.revoked", "unknown", &attempted),
            );
            error(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    }
}

/// Lifts one revocation. It takes effect at the next connection.
#[utoipa::path(
    delete,
    path = "/api/v1/revocations/{id}",
    tag = "revocation",
    params(("id" = String, Path, description = "the revocation's id")),
    responses(
        (status = 204, description = "Lifted"),
        (status = 403, description = "Refused as a cross-site request (Sec-Fetch-Site)", body = ErrorBody),
        (status = 404, description = "No such revocation, or revocation is not configured", body = ErrorBody),
        (status = 503, description = "The audit record cannot be written; nothing was changed — retry after Retry-After", content_type = "text/plain", body = String, headers(("Retry-After" = u64, description = "Seconds to wait before asking again")))
    )
)]
async fn lift_revocation(
    State(state): State<Arc<OperatorCases>>,
    _csrf: SameOrigin,
    Path(id): Path<String>,
) -> Response {
    let Some(outcome) = state.lift_revocation(&id) else {
        return revocation_off();
    };
    match outcome {
        Ok(true) => {
            info!(revocation = %id, "revocation lifted");
            state.note(
                crate::audit::Entry::new("revocation.lifted", "lifted").with("id", id.clone()),
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error(StatusCode::NOT_FOUND, format!("no revocation {id:?}")),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::AppState;

    /// A register whose revocation is written and whose answer is then lost, as a commit whose
    /// answer never arrived: the store holds what it was answered `Err` for.
    #[derive(Default)]
    struct LostAnswer(std::sync::Mutex<Vec<crate::revocation::Revocation>>);

    impl crate::revocation::LedgerStore for LostAnswer {
        fn load(&self) -> Result<crate::revocation::Ledger, String> {
            Ok(crate::revocation::Ledger::default())
        }
        fn put_issued(&self, _: &crate::revocation::Issued) -> Result<(), String> {
            Ok(())
        }
        fn remove_issued(&self, _: &crate::revocation::CertId, _: u64) -> Result<bool, String> {
            Ok(false)
        }
        fn add_revocation(
            &self,
            entry: &crate::revocation::Revocation,
            _: usize,
        ) -> Result<crate::revocation::Added, String> {
            self.0.lock().expect("lock").push(entry.clone());
            Err("the connection closed before the commit was answered".into())
        }
        fn remove_revocation(&self, _: &str) -> Result<bool, String> {
            Ok(false)
        }
        fn save_hosts(&self, _: &BTreeMap<String, crate::revocation::Host>) -> Result<(), String> {
            Ok(())
        }
    }

    /// The entries recorded, places written into included.
    #[derive(Default)]
    struct Recorded(Arc<std::sync::Mutex<Vec<crate::audit::Entry>>>);

    impl crate::audit::Audit for Recorded {
        fn record(&self, entry: crate::audit::Entry) -> Result<(), crate::audit::Unavailable> {
            self.0.lock().expect("lock").push(entry);
            Ok(())
        }

        fn reserve(&self) -> Result<crate::audit::Permit, crate::audit::Unavailable> {
            let entries = self.0.clone();
            Ok(crate::audit::Permit::new(move |entry| {
                entries.lock().expect("lock").push(entry);
            }))
        }
    }

    /// A revocation whose store answers an error may have been made all the same, so its place is
    /// written with the outcome `unknown`, naming the certificate as a revocation that was made
    /// would, and a restore sets it again.
    /// Verifies: SEC-append-only-audit-record
    #[tokio::test]
    async fn a_revocation_whose_store_answer_was_lost_is_recorded_as_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let revocations = crate::revocation::Revocations::open(
            Box::new(LostAnswer::default()),
            Arc::new(crate::clock::SystemClock),
            vec![crate::revocation::Authority {
                role: "client".to_string(),
                subject: "0".repeat(64),
                name: "client CA".to_string(),
            }],
        )
        .expect("revocations");
        let audit = Recorded::default();
        let entries = audit.0.clone();
        let state = AppState::new(dir.path().to_path_buf())
            .expect("state")
            .with_revocations(Some(Arc::new(revocations)))
            .with_audit(Some(Arc::new(audit) as Arc<dyn crate::audit::Audit>));
        let response = revoke(
            State(Arc::new(OperatorCases::new(Arc::new(state), None))),
            SameOrigin,
            Json(serde_json::json!({
                "certificate": { "authority": "client", "serial": "0ABC123" }
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let entries = entries.lock().expect("lock");
        let revoked: Vec<&crate::audit::Entry> = entries
            .iter()
            .filter(|entry| entry.event == "revocation.revoked")
            .collect();
        assert_eq!(revoked.len(), 1, "{entries:#?}");
        assert_eq!(revoked[0].outcome, "unknown");
        let field = |key: &str| revoked[0].fields.get(key).cloned();
        let text = |value: &str| Some(crate::audit::Field::Text(value.to_string()));
        assert_eq!(field("authority"), text("client"));
        assert_eq!(field("serial"), text("abc123"));
        assert!(field("id").is_some(), "{:?}", revoked[0]);
    }

    /// A register that knows the Gateway `h-gw`, and whose write of the hosts is answered `Err`
    /// after it may have landed.
    struct LostHosts;

    impl crate::revocation::LedgerStore for LostHosts {
        fn load(&self) -> Result<crate::revocation::Ledger, String> {
            let mut ledger = crate::revocation::Ledger::default();
            ledger.hosts.insert(
                "h-gw".to_string(),
                crate::revocation::Host {
                    gateway: true,
                    ..crate::revocation::Host::default()
                },
            );
            Ok(ledger)
        }
        fn put_issued(&self, _: &crate::revocation::Issued) -> Result<(), String> {
            Ok(())
        }
        fn remove_issued(&self, _: &crate::revocation::CertId, _: u64) -> Result<bool, String> {
            Ok(false)
        }
        fn add_revocation(
            &self,
            _: &crate::revocation::Revocation,
            _: usize,
        ) -> Result<crate::revocation::Added, String> {
            Err("not in this test".into())
        }
        fn remove_revocation(&self, _: &str) -> Result<bool, String> {
            Ok(false)
        }
        fn save_hosts(&self, _: &BTreeMap<String, crate::revocation::Host>) -> Result<(), String> {
            Err("the connection closed before the commit was answered".into())
        }
    }

    /// Floors whose write is answered `Err` after it may have landed.
    struct LostFloor;

    impl crate::floors::FloorStore for LostFloor {
        fn load(&self) -> Result<BTreeMap<String, crate::floors::Floor>, String> {
            Ok(BTreeMap::new())
        }
        fn put(&self, _: &str, _: &crate::floors::Floor) -> Result<crate::floors::Put, String> {
            Err("the connection closed before the commit was answered".into())
        }
        fn remove(&self, _: &str) -> Result<bool, String> {
            Ok(false)
        }
    }

    /// An enrolment store in memory whose writes, once `failing` is set, are answered `Err`.
    #[derive(Clone, Default)]
    struct Failing(
        Arc<std::sync::Mutex<Option<crate::enrolment::Persisted>>>,
        Arc<std::sync::atomic::AtomicBool>,
    );

    impl crate::enrolment::EnrolmentStore for Failing {
        fn load(&self) -> Result<Option<crate::enrolment::Persisted>, String> {
            Ok(self.0.lock().expect("lock").clone())
        }
        fn save(&self, persisted: &crate::enrolment::Persisted) -> Result<(), String> {
            if self.1.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("the connection closed before the commit was answered".into());
            }
            *self.0.lock().expect("lock") = Some(persisted.clone());
            Ok(())
        }
    }

    /// An act that withdraws trust whose store answers an error may have been made all the same,
    /// so its place is written with the outcome `unknown` and what it was asked to do, and a
    /// restore repeats it; a window closed here is closed whatever its store answers, so its entry
    /// says `closed`.
    /// Verifies: SEC-append-only-audit-record
    #[tokio::test]
    async fn an_act_withdrawing_trust_whose_store_answered_an_error_is_recorded_all_the_same() {
        let dir = tempfile::tempdir().expect("tempdir");
        let revocations = crate::revocation::Revocations::open(
            Box::new(LostHosts),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations");
        let floors = crate::floors::PackageFloors::open(Box::new(LostFloor)).expect("floors");
        let store = Failing::default();
        let enrolment = crate::enrolment::Enrolment::open_on(
            Arc::new(crate::clock::SystemClock),
            Box::new(store.clone()),
        )
        .expect("enrolment");
        enrolment.open(600).expect("open the window");
        // A request's id is its key's fingerprint, which the rejection checks.
        enrolment.submit(
            crate::enrolment::tests::request(&crate::enrolment::tests::key(1)),
            crate::enrolment::tests::requester(),
        );
        let id = enrolment.pending()[0].id.clone();
        store.1.store(true, std::sync::atomic::Ordering::SeqCst);
        let audit = Recorded::default();
        let entries = audit.0.clone();
        let state = Arc::new(OperatorCases::new(
            Arc::new(
                AppState::new(dir.path().to_path_buf())
                    .expect("state")
                    .with_revocations(Some(Arc::new(revocations)))
                    .with_floors(Some(Arc::new(floors)))
                    .with_enrolment(Some(Arc::new(enrolment)))
                    .with_audit(Some(Arc::new(audit) as Arc<dyn crate::audit::Audit>)),
            ),
            None,
        ));

        let cleared = set_gateway(
            State(state.clone()),
            SameOrigin,
            Path("h-gw".to_string()),
            Json(GatewayRequest { gateway: false }),
        )
        .await;
        let floor = put_package_floor(
            State(state.clone()),
            Path("otelcol".to_string()),
            Json(FloorSpec {
                version: "2.0.0".to_string(),
                signature: hex::encode([7u8; 64]),
            }),
        )
        .await;
        let rejected = reject_enrolment(State(state.clone()), SameOrigin, Path(id.clone())).await;
        let closed = close_enrolment_window(State(state.clone())).await;
        for response in [&cleared, &floor, &rejected, &closed] {
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }

        let entries = entries.lock().expect("lock");
        let only = |event: &str| -> crate::audit::Entry {
            let named: Vec<&crate::audit::Entry> = entries
                .iter()
                .filter(|entry| entry.event == event)
                .collect();
            assert_eq!(named.len(), 1, "{event}: {entries:#?}");
            named[0].clone()
        };
        let text = |value: &str| Some(crate::audit::Field::Text(value.to_string()));
        let gateway = only("host.gateway");
        assert_eq!(gateway.outcome, "unknown");
        assert_eq!(gateway.fields.get("host").cloned(), text("h-gw"));
        assert_eq!(
            gateway.fields.get("gateway").cloned(),
            Some(crate::audit::Field::Flag(false))
        );
        let floor = only("package_floor.set");
        assert_eq!(floor.outcome, "unknown");
        assert_eq!(floor.fields.get("agent_type").cloned(), text("otelcol"));
        assert_eq!(floor.fields.get("version").cloned(), text("2.0.0"));
        let rejected = only("enrolment.rejected");
        assert_eq!(rejected.outcome, "unknown");
        assert_eq!(rejected.fields.get("id").cloned(), text(&id));
        assert_eq!(only("enrolment.window").outcome, "closed");
    }

    /// A rejection of an id the queue does not hold, with the window closed, is answered `204`,
    /// recorded as `enrolment.rejected` with `queued` false, and refuses that request when it
    /// arrives in a window; one queued is recorded with `queued` true. An id that is not 64
    /// lowercase hexadecimal digits is answered `400` and recorded nowhere.
    /// Verifies: SEC-bootstrap-certificates-and-enrolment
    #[tokio::test]
    async fn an_id_not_queued_is_rejected_and_a_malformed_one_is_refused() {
        use crate::enrolment::tests::{key, request, requester};
        use crate::enrolment::Submitted;
        let dir = tempfile::tempdir().expect("tempdir");
        let enrolment = Arc::new(crate::enrolment::Enrolment::new(Arc::new(
            crate::clock::SystemClock,
        )));
        let audit = Recorded::default();
        let entries = audit.0.clone();
        let state = Arc::new(OperatorCases::new(
            Arc::new(
                AppState::new(dir.path().to_path_buf())
                    .expect("state")
                    .with_enrolment(Some(enrolment.clone()))
                    .with_audit(Some(Arc::new(audit) as Arc<dyn crate::audit::Audit>)),
            ),
            None,
        ));
        let reject = |id: String| reject_enrolment(State(state.clone()), SameOrigin, Path(id));

        for malformed in [
            "no-such-request".to_string(),
            key(0xab).to_uppercase(),
            key(1)[1..].to_string(),
            format!("{}0", key(1)),
        ] {
            let response = reject(malformed.clone()).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{malformed}");
        }
        assert!(entries.lock().expect("lock").is_empty(), "nothing recorded");

        assert!(!enrolment.is_open());
        let response = reject(key(1)).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        enrolment.open(600).expect("open the window");
        assert_eq!(
            enrolment.submit(request(&key(1)), requester()),
            Submitted::Rejected
        );
        assert!(enrolment.pending().is_empty());
        assert_eq!(
            enrolment.submit(request(&key(2)), requester()),
            Submitted::Waiting
        );
        let response = reject(key(2)).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let entries = entries.lock().expect("lock");
        let rejected: Vec<_> = entries
            .iter()
            .filter(|entry| entry.event == "enrolment.rejected")
            .map(|entry| {
                (
                    entry.outcome.clone(),
                    entry.fields.get("id").cloned(),
                    entry.fields.get("queued").cloned(),
                )
            })
            .collect();
        let text = |value: String| Some(crate::audit::Field::Text(value));
        let flag = |value: bool| Some(crate::audit::Field::Flag(value));
        assert_eq!(
            rejected,
            [
                ("rejected".to_string(), text(key(1)), flag(false)),
                ("rejected".to_string(), text(key(2)), flag(true)),
            ]
        );
    }

    /// A store refusal of the Package routes is answered with the status its kind names, whatever
    /// its text: a refusal whose message a database prefixed keeps its status, and a failure whose
    /// message begins as a refusal's does is a `500`.
    /// Verifies: PKG-signed-packages-and-delivery
    #[test]
    fn a_package_refusal_is_answered_with_its_status() {
        use crate::packages::Refusal;
        for (refusal, status) in [
            (
                PackageError::refused(
                    Refusal::NotFound,
                    "database fleet on db:5432: no package set otelcol@1.0.0",
                ),
                StatusCode::NOT_FOUND,
            ),
            (
                PackageError::refused(Refusal::Conflict, "set otelcol@1.0.0 is assigned"),
                StatusCode::CONFLICT,
            ),
            (
                PackageError::refused(
                    Refusal::Invalid,
                    "database fleet on db:5432: the artifact was removed while it was uploaded",
                ),
                StatusCode::BAD_REQUEST,
            ),
            (
                PackageError::from("the deletion ended: panicked".to_string()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            let text = refusal.message.clone();
            assert_eq!(package_error(refusal).status(), status, "{text}");
        }
    }
}
