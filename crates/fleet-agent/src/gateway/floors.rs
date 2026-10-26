//! The floors a Gateway hands the hosts behind it (ADR-0028 clause 16): fetched from its Server when
//! it starts and once an hour, and answered on the downstream `GET /v1/package-floors` as they were
//! last fetched, behind the handshake and the revocation verdict of its `/v1/opamp`. The Gateway
//! checks no signature on their behalf: each host checks its own and keeps what verifies.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use opamp::server::listen::PeerCertificate;
use tokio::time::Instant;
use tracing::{info, warn};

use super::revocations::{RevocationList, Verdict};
use super::Members;
use crate::config::ClientConfig;
use crate::shutdown::Shutdown;

/// The body of floors the Gateway last fetched, shared by every downstream request.
#[derive(Clone, Default)]
pub struct Relayed {
    body: Arc<RwLock<Option<Vec<u8>>>>,
}

impl Relayed {
    /// One fetch from the Server: a body that reads as floors replaces the one held; a failed
    /// fetch keeps it, and is logged.
    pub async fn update(&self, config: &ClientConfig) {
        match crate::floors::fetch(config).await.and_then(|body| {
            crate::floors::parse(&body)
                .map(|floors| (floors.len(), body))
                .map_err(|e| format!("an unreadable body: {e}"))
        }) {
            Ok((count, body)) => {
                info!(
                    floors = count,
                    "package floors fetched for the hosts behind this Gateway"
                );
                *self.body.write().expect("floors lock") = Some(body);
            }
            Err(e) => warn!(
                error = %e,
                "cannot fetch the package floors; the hosts behind this Gateway are handed the \
                 ones last fetched"
            ),
        }
    }

    fn body(&self) -> Option<Vec<u8>> {
        self.body.read().expect("floors lock").clone()
    }
}

/// Fetches the floors at once, and then every `every`, until `shutdown` fires.
pub async fn refresh(
    relayed: Relayed,
    config: Arc<ClientConfig>,
    mut shutdown: Shutdown,
    every: Duration,
) {
    let mut tick = tokio::time::interval_at(Instant::now(), every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            () = shutdown.requested() => return,
        }
        relayed.update(&config).await;
    }
}

/// What the downstream floors route needs.
pub struct Downstream {
    pub relayed: Relayed,
    pub revocations: RevocationList,
    /// Tells an enrolling peer, which is no member yet and is handed nothing, from a member.
    pub members: Members,
}

/// The floors route on the downstream listener.
pub fn router(downstream: Arc<Downstream>) -> axum::Router {
    axum::Router::new()
        .route(crate::floors::PATH, axum::routing::get(serve))
        .with_state(downstream)
}

async fn serve(
    axum::extract::State(downstream): axum::extract::State<Arc<Downstream>>,
    request: Request,
) -> Response {
    let certificate = request
        .extensions()
        .get::<PeerCertificate>()
        .and_then(|peer| peer.0.as_ref())
        .map(|cert| cert.as_ref().to_vec())
        .unwrap_or_default();
    // The admission of `/v1/opamp`, as the download route holds to it (ADR-0015 clause 15).
    match downstream.revocations.verdict(&certificate) {
        Verdict::Admit => {}
        Verdict::Revoked => {
            return (StatusCode::UNAUTHORIZED, "this certificate is revoked").into_response()
        }
        Verdict::Stale => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, HeaderValue::from_static("30"))],
                "the Gateway holds no current revocation list",
            )
                .into_response()
        }
    }
    if downstream.members.enrolling(&certificate) {
        return (
            StatusCode::FORBIDDEN,
            "the package floors are for members of the fleet",
        )
            .into_response();
    }
    match downstream.relayed.body() {
        Some(body) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, HeaderValue::from_static("30"))],
            "the Gateway has fetched no package floors yet",
        )
            .into_response(),
    }
}
