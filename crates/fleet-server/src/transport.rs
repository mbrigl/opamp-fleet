//! The OpAMP endpoint — both transports on one path (SEC-tls13-bounded-input-chosen-planes) — and the
//! rest of the Agent plane: the package download, the Gateways' revocation list and the package
//! floors.
//!
//! `/v1/opamp` serves the whole protocol: a request carrying the protobuf `Content-Type` is the
//! plain-HTTP transport, a WebSocket upgrade is the other — exactly the detection the Baseline
//! describes. The communication is `opamp::server`'s, shared with the Gateway and the Supervisor
//! Endpoint (DEV-six-crates-tokio-axum); what is the Server's is the [`Fleet`] handler, which hands
//! every decoded report to [`AgentCases`], so transport is carriage, never semantics. Every route
//! of the plane reaches state through [`AgentCases`] alone (SRV-two-use-case-layers §2).

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::server::listen::PeerCertificate;
use opamp::server::{
    Closing, Handler, Outbound, Rejection, Reply, RequestInfo, Settings, Unreadable,
};
use serde::Deserialize;

use crate::agent_cases::{
    AgentCases, ArtifactRefused, Connection, DownloadRefused, Proofs, Push, Refused, Session,
    FORGOTTEN,
};
use crate::answers::{busy, peer_ip, throttled};
use crate::packages::{PackageId, Platform};
use crate::tls::Peer;

/// The endpoint path the Baseline names as the default, and the protobuf media type it requires —
/// both from the shared crate, because the Gateway serves the same endpoint (DEV-six-crates-tokio-axum).
pub use opamp::endpoint::{OPAMP_PATH, PROTOBUF_CONTENT_TYPE};

/// What a message is refused with while the instance holds no lease it can prove.
const NO_LEASE: &str = "this instance holds no lease it can prove; connect again";

/// `401` without a challenge: the Agent plane admits by certificate, which no header can supply
/// (SEC-admission-by-client-certificate §1).
fn unauthorized(text: &'static str) -> Response {
    (StatusCode::UNAUTHORIZED, text).into_response()
}

/// Where a Gateway fetches the revoked certificates it refuses (SEC-admission-by-client-certificate §34).
pub const GATEWAY_REVOCATIONS_PATH: &str = fleet_core::revocation_list::PATH;

/// Where every member fetches the floor of each Agent type (PKG-versions-high-water-mark-floor §15).
pub const PACKAGE_FLOORS_PATH: &str = "/v1/package-floors";

/// The Agent plane's OpAMP endpoint and its two member routes, guarded by admission when
/// `cases` requires it.
pub fn router(cases: Arc<AgentCases>) -> Router {
    // The limits the Baseline requires of the Server, on both transports and in both directions.
    let settings = Settings::new(cases.max_message_size());
    let gateways = Router::new()
        .route(
            GATEWAY_REVOCATIONS_PATH,
            axum::routing::get(gateway_revocations),
        )
        .route(PACKAGE_FLOORS_PATH, axum::routing::get(package_floors))
        .with_state(cases.clone());
    let fleet = Fleet(cases.clone());
    let mut router = opamp::server::router(Arc::new(fleet), settings).merge(gateways);
    if cases.requires_admission() {
        // The outermost layer: every plain-HTTP POST and the upgrade GET — checked before the
        // WebSocket upgrade completes — answers 401 without a certificate that admits.
        router = router.layer(middleware::from_fn_with_state(cases, admit));
    }
    router
}

/// The package download route (SEC-admission-by-client-certificate §18), which the root wraps in
/// [`guard_download`]. It lives on the Agent plane rather than with the rest of `/api/v1` because
/// the split between the two planes is by *audience*, not by path.
pub fn download_router(cases: Arc<AgentCases>) -> Router {
    Router::new()
        .route(
            "/api/v1/packages/{agent_type}/{version}/file",
            axum::routing::get(download_package),
        )
        .with_state(cases)
}

/// Wraps the download route in its guard.
pub fn guard_download(router: Router, cases: Arc<AgentCases>) -> Router {
    if !cases.guards_downloads() {
        return router;
    }
    router.layer(middleware::from_fn_with_state(cases, admit_download))
}

async fn admit_download(
    State(cases): State<Arc<AgentCases>>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = peer_ip(&request);
    let path = request.uri().path().to_string();
    let certificate = request
        .extensions()
        .get::<PeerCertificate>()
        .and_then(|peer| peer.0.clone());
    match cases.admit_download(peer, &path, certificate.as_ref().map(|cert| cert.as_ref())) {
        Err(DownloadRefused::Throttled(wait)) => throttled(wait),
        Err(DownloadRefused::NotOfTheFleet) => {
            unauthorized("the package download requires a certificate of the fleet")
        }
        Err(DownloadRefused::OverRate) => over_rate(),
        Err(DownloadRefused::Busy) => busy(),
        Ok(proofs) => {
            if let Some(proofs) = proofs {
                request.extensions_mut().insert(proofs);
            }
            next.run(request).await
        }
    }
}

/// The Platform the download route names (PKG-signed-packages-and-delivery): the artifact endpoint serves bytes, and a
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

/// The JSON error body the download answers with, as the REST API's.
fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}

/// Serves an entry's artifact bytes — the `download_url` the Agent is offered points here.
///
/// `200` with the bytes, `400` for a missing or invalid platform or identity, `404` for a Package
/// without an uploaded artifact for that platform.
async fn download_package(
    State(cases): State<Arc<AgentCases>>,
    Path((agent_type, version)): Path<(String, String)>,
    Query(query): Query<PlatformQuery>,
    request: Request,
) -> Response {
    // A read: a pinned Package an older Server stored under a non-SemVer version is still offered
    // (PKG-versions-high-water-mark-floor §3), so its bytes are still served.
    let id = match PackageId::load(&agent_type, &version) {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let platform = match Platform::new(&query.os, &query.arch) {
        Ok(platform) => platform,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid platform: {e}")),
    };
    let proofs = request.extensions().get::<Proofs>();
    match cases.artifact(proofs, peer_ip(&request), &id, &platform) {
        // One answer for every artifact this requester may not fetch, whether or not the store holds
        // it, so the store cannot be listed by probing (PKG-signed-packages-and-delivery §37).
        Err(ArtifactRefused::NotFound) => error(
            StatusCode::NOT_FOUND,
            format!(
                "no set {id} with an artifact for {}-{}",
                platform.os, platform.arch
            ),
        ),
        Err(ArtifactRefused::Unreadable) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the artifact of {id} cannot be read"),
        ),
        // Streamed from the blob store, never buffered: a fleet updating at once means many concurrent
        // downloads of the same artifact, and each one holding a copy of a program in memory is how a
        // rollout takes the Server down with it.
        Ok((size, bytes)) => Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            // So the Agent can size the download, and a truncated transfer is detectable as one.
            .header(header::CONTENT_LENGTH, size)
            .body(Body::from_stream(read_chunks(bytes)))
            .unwrap_or_else(|e| {
                error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("cannot serve the artifact of {id}: {e}"),
                )
            }),
    }
}

/// A stream of an artifact's chunks, read on a thread of its own, so a response body never
/// materialises whole and a slow disk never holds up the runtime.
fn read_chunks(
    mut bytes: Box<dyn std::io::Read + Send>,
) -> impl futures_util::Stream<Item = std::io::Result<Vec<u8>>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    tokio::task::spawn_blocking(move || loop {
        let mut buffer = vec![0u8; 64 * 1024];
        let next = match bytes.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => {
                buffer.truncate(read);
                Ok(buffer)
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => Err(e),
        };
        let failed = next.is_err();
        if sender.blocking_send(next).is_err() || failed {
            return;
        }
    });
    futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|chunk| (chunk, receiver))
    })
}

/// The revoked certificates of the client CA, chains resolved, for a host marked as a Gateway
/// (SEC-admission-by-client-certificate §34). Admitted as `/v1/opamp` is, by the layer around both; what is checked
/// here is that the member is a Gateway.
async fn gateway_revocations(State(cases): State<Arc<AgentCases>>, request: Request) -> Response {
    let host = match request.extensions().get::<Peer>() {
        Some(Peer::Member) => request
            .extensions()
            .get::<Proofs>()
            .and_then(|proofs| proofs.host.clone()),
        _ => None,
    };
    let Some((body, etag)) = cases.gateway_revocations(host, peer_ip(&request)) else {
        return (
            StatusCode::FORBIDDEN,
            "the revocation list is for hosts marked as Gateways",
        )
            .into_response();
    };
    if request
        .headers()
        .get(header::IF_NONE_MATCH)
        .is_some_and(|value| value.as_bytes() == etag.as_bytes())
    {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::ETAG, etag),
        ],
        body,
    )
        .into_response()
}

/// Every floor the Server stores, for every admitted member (PKG-versions-high-water-mark-floor §15). Admitted as
/// `/v1/opamp` is, by the layer around both; an enrolling host is no member yet, and is refused.
async fn package_floors(State(cases): State<Arc<AgentCases>>, request: Request) -> Response {
    let enrolling = matches!(
        request.extensions().get::<Peer>(),
        Some(Peer::Enrolling { .. })
    );
    match cases.package_floors(enrolling, peer_ip(&request)) {
        None => (
            StatusCode::FORBIDDEN,
            "the package floors are for members of the fleet",
        )
            .into_response(),
        Some(body) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
    }
}

/// The answer to a download past the sender's rate (SEC-tls13-bounded-input-chosen-planes §26): `429`, retry in 30 s.
fn over_rate() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(
            header::RETRY_AFTER,
            crate::fleet::RETRY_AFTER.as_secs().to_string(),
        )],
        "too many requests from this sender — retry later",
    )
        .into_response()
}

async fn admit(State(cases): State<Arc<AgentCases>>, mut request: Request, next: Next) -> Response {
    // An `Authorization` header is not read here or anywhere behind it (SEC-admission-by-client-certificate §1).
    let peer = peer_ip(&request);
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let certificate = request
        .extensions()
        .get::<PeerCertificate>()
        .and_then(|peer| peer.0.clone());
    let not_of_the_fleet =
        || unauthorized("the OpAMP endpoint requires a certificate of the fleet");
    match cases.admit(
        peer,
        websocket,
        certificate.as_ref().map(|cert| cert.as_ref()),
    ) {
        Err(Refused::Throttled(wait)) => throttled(wait),
        Err(Refused::NoCertificate) => (
            StatusCode::UNAUTHORIZED,
            "the OpAMP endpoint requires a client certificate",
        )
            .into_response(),
        Err(Refused::NotOfTheFleet) => not_of_the_fleet(),
        Err(Refused::NoEnrolmentWindow) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no enrolment window is open",
        )
            .into_response(),
        Err(Refused::Busy) => busy(),
        Ok((classified, proofs)) => {
            request.extensions_mut().insert(classified);
            request.extensions_mut().insert(proofs);
            next.run(request).await
        }
    }
}

/// The Server's handler behind the endpoint: every report goes to the fleet through
/// [`AgentCases`], and a WebSocket is also sent what the fleet wants its Agents to have when that
/// changes.
struct Fleet(Arc<AgentCases>);

impl Outbound for Session {
    type Item = Push;

    async fn next(&mut self) -> Option<Push> {
        self.next_push().await
    }
}

impl Handler for Fleet {
    type Connection = Connection;
    type Outbound = Session;

    fn on_connecting(
        &self,
        request: &RequestInfo<'_>,
    ) -> Result<(Connection, Option<Session>), Rejection> {
        let proofs = request
            .extensions
            .get::<Proofs>()
            .cloned()
            .unwrap_or_default();
        Ok(self.0.connect(
            proofs,
            request.extensions.get::<Peer>(),
            request.transport == opamp::server::Transport::WebSocket,
            request.peer,
        ))
    }

    async fn on_message(&self, carrier: &mut Connection, report: AgentToServer) -> Reply {
        match self.0.message(carrier, report).await {
            Ok(reply) => Reply::Send(reply),
            Err(_) => Reply::Refuse(StatusCode::SERVICE_UNAVAILABLE, NO_LEASE.to_string()),
        }
    }

    fn on_outbound(&self, carrier: &mut Connection, push: Push) -> Vec<ServerToAgent> {
        self.0.outbound(carrier, push)
    }

    fn on_unreadable(&self, carrier: &mut Connection, _error: &Unreadable) -> Reply {
        Reply::Send(self.0.unreadable(carrier))
    }

    fn closing(&self, carrier: &Connection) -> Option<Closing> {
        let reason = self.0.ended(carrier)?;
        if reason == FORGOTTEN {
            // Nothing the peer did: it may connect again, and comes back as a stranger.
            return Some(Closing {
                code: 1000,
                reason: reason.to_string(),
            });
        }
        Some(Closing::policy(reason))
    }

    /// A restart command its connection did not take stays queued
    /// (SRV-instance-opens-chosen-planes §8).
    fn on_unsent(&self, message: &ServerToAgent) {
        self.0.unsent(message);
    }

    /// The connection is gone; every Agent it carried is unreachable until it reports again.
    /// An enrolling host was never one of the fleet's Agents, so there is nothing to mark.
    fn on_closed(&self, carrier: Connection) {
        self.0.closed(carrier);
    }
}
