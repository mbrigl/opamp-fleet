//! The OpAMP endpoint — both transports on one path (ADR-0012).
//!
//! `/v1/opamp` serves the whole protocol: a request carrying the protobuf `Content-Type` is the
//! plain-HTTP transport, a WebSocket upgrade is the other — exactly the detection the Baseline
//! describes. The communication is `opamp::server`'s, shared with the Gateway and the Supervisor
//! Endpoint (ADR-0032); what is the Server's is the [`Fleet`] handler, which hands every decoded
//! report to the same [`AppState::process`], so transport is carriage, never semantics.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::server::listen::PeerCertificate;
use opamp::server::{
    Closing, Handler, Outbound, Rejection, Reply, RequestInfo, Settings, Unreadable,
};
use opamp::uid::InstanceUid;
use tokio::sync::watch;
use tracing::{debug, info};

use crate::audit::{Audit, Entry};
use crate::enrolment::{Enrolment, Requester, Submitted};
use crate::fleet::{bad_request, unavailable, AppState, ConnId, Transport};
use crate::revocation::{CertId, Presented, Revocations};
use crate::throttle::Throttle;
use crate::tls::{Issuers, Peer};

/// The endpoint path the Baseline names as the default, and the protobuf media type it requires —
/// both from the shared crate, because the Gateway serves the same endpoint (ADR-0011).
pub use opamp::endpoint::{OPAMP_PATH, PROTOBUF_CONTENT_TYPE};

/// What a peer must prove to reach `/v1/opamp` (ADR-0059 clauses 1, 6): a client certificate the
/// handshake verified, not revoked — the whole of admission. No `Authorization` header is read; one
/// that is sent is ignored, never refused, and no refusal carries a `WWW-Authenticate` challenge. A
/// certificate from the bootstrap CA admits an enrolling host, and only while an operator holds the
/// enrolment window open. Repeated failures from one peer address are throttled.
#[derive(Default)]
pub struct Admission {
    /// The connection must have carried a certificate. The certificate itself is already verified
    /// — rustls refuses one it cannot chain — so this is a presence check, never a second
    /// verification.
    require_client_certificate: bool,
    issuers: Issuers,
    enrolment: Option<Arc<Enrolment>>,
    throttle: Option<Arc<Throttle>>,
    revocations: Option<Arc<Revocations>>,
    audit: Option<Arc<dyn Audit>>,
    /// When a plain-HTTP peer presenting a certificate was last recorded as admitted.
    admitted: Mutex<HashMap<(IpAddr, Option<String>), Instant>>,
}

/// How often a plain-HTTP peer's admission is recorded: once per address and certificate per
/// hour, so a poll every few seconds does not crowd the record (ADR-0063 clause 1).
const ADMITTED_EVERY: Duration = Duration::from_secs(3600);

/// The plain-HTTP peers remembered as recorded at most.
const ADMITTED_MAX: usize = 100_000;

/// What admitted a connection (ADR-0065 clause 9): the certificate, with when it expires. A
/// WebSocket session ends when it is revoked, or when it expires.
#[derive(Clone, Debug, Default)]
pub struct Proofs {
    pub certificate: Option<(CertId, u64)>,
    /// The host the certificate was issued to, when it names one (ADR-0059 clause 7).
    pub host: Option<String>,
}

impl Proofs {
    fn revoked(&self, revocations: &Revocations) -> bool {
        self.certificate
            .as_ref()
            .is_some_and(|(id, _)| revocations.is_certificate_revoked(id))
    }
}

impl Admission {
    /// No proof required — what a test that is not about admission serves with. A Server never
    /// does: its configuration requires the client CA (ADR-0059).
    pub fn open() -> Self {
        Admission::default()
    }

    pub fn new(require_client_certificate: bool) -> Self {
        Admission {
            require_client_certificate,
            ..Admission::default()
        }
    }

    /// Tells a member's certificate from an enrolling host's, and admits the latter only while
    /// `enrolment`'s window is open.
    #[must_use]
    pub fn with_enrolment(mut self, issuers: Issuers, enrolment: Option<Arc<Enrolment>>) -> Self {
        self.issuers = issuers;
        self.enrolment = enrolment;
        self
    }

    /// Throttles repeated failures from one peer address (ADR-0059 clause 24).
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<Throttle>) -> Self {
        self.throttle = Some(throttle);
        self
    }

    /// Records every decision in `audit` (ADR-0063).
    #[must_use]
    pub fn with_audit(mut self, audit: Option<Arc<dyn Audit>>) -> Self {
        self.audit = audit;
        self
    }

    /// `client` or `bootstrap` for a certificate one of this Server's CAs issued.
    fn issuers_role(&self, issuer: &str) -> Option<String> {
        self.revocations
            .as_ref()
            .and_then(|revocations| revocations.authority_of(issuer))
            .map(|authority| authority.role.clone())
    }

    /// Whether this admission is recorded: every WebSocket session, and a plain-HTTP peer once per
    /// address — an IPv6 one by its /64 — and certificate per [`ADMITTED_EVERY`].
    fn worth_recording(&self, websocket: bool, peer: Option<IpAddr>, serial: Option<&str>) -> bool {
        let (false, Some(peer)) = (websocket, peer) else {
            return true;
        };
        let key = (crate::throttle::peer_key(peer), serial.map(str::to_string));
        self.admitted
            .lock()
            .expect("admitted lock")
            .get(&key)
            .is_none_or(|at| at.elapsed() >= ADMITTED_EVERY)
    }

    /// Notes that a plain-HTTP peer's admission was recorded — only once it was, so a peer
    /// refused for want of a record is recorded when it is admitted.
    fn recorded(&self, peer: Option<IpAddr>, serial: Option<&str>) {
        let Some(peer) = peer else {
            return;
        };
        let mut admitted = self.admitted.lock().expect("admitted lock");
        if admitted.len() >= ADMITTED_MAX {
            admitted.retain(|_, at| at.elapsed() < ADMITTED_EVERY);
            if admitted.len() >= ADMITTED_MAX {
                // Forgetting only means recording again; it never means admitting unrecorded.
                admitted.clear();
            }
        }
        admitted.insert(
            (crate::throttle::peer_key(peer), serial.map(str::to_string)),
            Instant::now(),
        );
    }

    /// Refuses a revoked certificate (ADR-0065 clause 8).
    #[must_use]
    pub fn with_revocations(mut self, revocations: Option<Arc<Revocations>>) -> Self {
        self.revocations = revocations;
        self
    }

    fn required(&self) -> bool {
        self.require_client_certificate || self.throttle.is_some() || self.revocations.is_some()
    }
}

/// `401` without a challenge: the Agent plane admits by certificate, which no header can supply
/// (ADR-0059 clause 1).
fn unauthorized(text: &'static str) -> Response {
    (StatusCode::UNAUTHORIZED, text).into_response()
}

/// What the package download route asks of a peer (ADR-0059 clause 23): the same handshake as
/// `/v1/opamp`, and a certificate from the client CA — a bootstrap certificate is not enough.
#[derive(Clone, Default)]
pub struct DownloadGuard {
    require_client_certificate: bool,
    issuers: Issuers,
    throttle: Option<Arc<Throttle>>,
    revocations: Option<Arc<Revocations>>,
    audit: Option<Arc<dyn Audit>>,
}

impl Admission {
    /// The download route's share of these rules.
    #[must_use]
    pub fn download_guard(&self) -> DownloadGuard {
        DownloadGuard {
            require_client_certificate: self.require_client_certificate,
            issuers: self.issuers.clone(),
            throttle: self.throttle.clone(),
            revocations: self.revocations.clone(),
            audit: self.audit.clone(),
        }
    }
}

/// Wraps the download route in its guard.
pub fn guard_download(router: Router, guard: DownloadGuard) -> Router {
    if !guard.require_client_certificate && guard.throttle.is_none() {
        return router;
    }
    router.layer(middleware::from_fn_with_state(
        Arc::new(guard),
        admit_download,
    ))
}

async fn admit_download(
    State(guard): State<Arc<DownloadGuard>>,
    request: Request,
    next: Next,
) -> Response {
    let peer = peer_ip(&request);
    let path = request.uri().path().to_string();
    let record = |event: &str, outcome: &str| {
        guard.audit.as_ref().map(|audit| {
            audit.record(
                Entry::new(event, outcome)
                    .peer(peer)
                    .with("plane", "agent")
                    .with("path", path.clone()),
            )
        })
    };
    if let (Some(throttle), Some(peer)) = (&guard.throttle, peer) {
        if let Some(wait) = throttle.retry_after(peer) {
            let _ = record("download.throttled", "throttled");
            return throttled(wait);
        }
    }
    let member = match request
        .extensions()
        .get::<PeerCertificate>()
        .and_then(|peer| peer.0.as_ref())
    {
        Some(cert) => {
            guard.issuers.classify(cert.as_ref()) == Peer::Member
                && !guard.revocations.as_ref().is_some_and(|revocations| {
                    crate::ca::facts(cert.as_ref())
                        .map_or(true, |facts| revocations.is_certificate_revoked(&facts.id))
                })
        }
        None => !guard.require_client_certificate,
    };
    if !member {
        if let (Some(throttle), Some(peer)) = (&guard.throttle, peer) {
            throttle.failed(peer);
        }
        let _ = record("download.refused", "refused");
        return unauthorized("the package download requires a certificate of the fleet");
    }
    if record("download.admitted", "admitted") == Some(Err(crate::audit::Unavailable)) {
        return busy();
    }
    next.run(request).await
}

/// Where a Gateway fetches the revoked certificates it refuses (ADR-0065 clause 12).
pub const GATEWAY_REVOCATIONS_PATH: &str = "/v1/gateway/revocations";

pub fn router(state: Arc<AppState>, admission: Admission) -> Router {
    // The limits the Baseline requires of the Server, on both transports and in both directions.
    let settings = Settings::new(state.max_message_size());
    let gateways = Router::new()
        .route(
            GATEWAY_REVOCATIONS_PATH,
            axum::routing::get(gateway_revocations),
        )
        .with_state(state.clone());
    let mut router = opamp::server::router(Arc::new(Fleet(state)), settings).merge(gateways);
    if admission.required() {
        // The outermost layer: every plain-HTTP POST and the upgrade GET — checked before the
        // WebSocket upgrade completes — answers 401 without a certificate that admits.
        router = router.layer(middleware::from_fn_with_state(Arc::new(admission), admit));
    }
    router
}

/// The revoked certificates of the client CA, chains resolved, for a host marked as a Gateway
/// (ADR-0065 clause 12). Admitted as `/v1/opamp` is, by the layer around both; what is checked
/// here is that the member is a Gateway. The `ETag` is the SHA-256 of the body, so it changes with
/// the list and survives a restart.
async fn gateway_revocations(State(state): State<Arc<AppState>>, request: Request) -> Response {
    use sha2::Digest as _;

    let forbidden = || {
        (
            StatusCode::FORBIDDEN,
            "the revocation list is for hosts marked as Gateways",
        )
            .into_response()
    };
    // Without a register nothing is revoked, so the empty list reveals nothing to anyone. With one,
    // the list is a member's whose host is marked as a Gateway, and nobody else's.
    let certificates: Vec<serde_json::Value> = match state.revocations() {
        None => Vec::new(),
        Some(revocations) => {
            let host = match request.extensions().get::<Peer>() {
                Some(Peer::Member) => request
                    .extensions()
                    .get::<Proofs>()
                    .and_then(|proofs| proofs.host.clone()),
                _ => None,
            };
            if !host
                .as_ref()
                .is_some_and(|host| revocations.is_gateway(host))
            {
                // A member asking for what only a Gateway is handed is a refusal like any other
                // (ADR-0063 clause 1).
                state.audit_refusal(
                    Entry::new("gateway_list.refused", "refused")
                        .peer(peer_ip(&request))
                        .with("plane", "agent")
                        .with("host", host)
                        .with("check", "not a gateway"),
                );
                return forbidden();
            }
            revocations
                .revoked_certificates("client")
                .into_iter()
                .map(|id| serde_json::json!({ "issuer": id.issuer, "serial": id.serial }))
                .collect()
        }
    };
    let body = serde_json::json!({ "certificates": certificates }).to_string();
    let etag = format!("\"{}\"", hex::encode(sha2::Sha256::digest(body.as_bytes())));
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

/// The peer's address, as the listener put it into the request.
pub fn peer_ip(request: &Request) -> Option<std::net::IpAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

/// The answer while the Server cannot take a decision now — too many password hashes under way on
/// the Operator plane, or the audit record unavailable: `503`, try again in a second.
pub fn busy() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "1".to_string())],
        "the Server cannot take this now — retry in a moment",
    )
        .into_response()
}

/// The answer to a peer in back-off: `429` with the seconds it still has to wait.
pub fn throttled(retry_after: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after.to_string())],
        "too many failed attempts from this address — retry later",
    )
        .into_response()
}

async fn admit(
    State(admission): State<Arc<Admission>>,
    mut request: Request,
    next: Next,
) -> Response {
    // What this gate proves is *fleet membership* and the host, not which Agent is speaking:
    // `instance_uid` stays self-asserted behind the certificate (ADR-0059 clauses 7, 14). Admission
    // is the trust boundary; the host is the only bound between admitted Agents. An
    // `Authorization` header is not read here or anywhere behind it (ADR-0059 clause 1).
    let peer = peer_ip(&request);
    let record_refusal = |event: &str, outcome: &str, check: &str| {
        if let Some(audit) = &admission.audit {
            audit.refusal(
                Entry::new(event, outcome)
                    .peer(peer)
                    .with("plane", "agent")
                    .with("check", check),
            );
        }
    };
    if let (Some(throttle), Some(peer)) = (&admission.throttle, peer) {
        if let Some(wait) = throttle.retry_after(peer) {
            record_refusal("admission.throttled", "throttled", "throttle");
            return throttled(wait);
        }
    }
    let refuse = |check: &str, response: Response| {
        if let (Some(throttle), Some(peer)) = (&admission.throttle, peer) {
            throttle.failed(peer);
        }
        record_refusal("admission.refused", "refused", check);
        response
    };
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let certificate = request
        .extensions()
        .get::<PeerCertificate>()
        .and_then(|peer| peer.0.clone());
    if admission.require_client_certificate && certificate.is_none() {
        debug!("refused: the OpAMP endpoint requires a client certificate");
        return refuse(
            "certificate",
            (
                StatusCode::UNAUTHORIZED,
                "the OpAMP endpoint requires a client certificate",
            )
                .into_response(),
        );
    }
    let presented = match certificate
        .as_ref()
        .map(|cert| crate::ca::facts(cert.as_ref()))
    {
        None => None,
        Some(Ok(facts)) => Some(facts),
        // The handshake verified it, so it parses; one that does not is not admitted.
        Some(Err(_)) => {
            return refuse(
                "certificate",
                unauthorized("the OpAMP endpoint requires a certificate of the fleet"),
            )
        }
    };
    if let Some(revocations) = &admission.revocations {
        let proofs = Proofs {
            certificate: presented
                .as_ref()
                .map(|facts| (facts.id.clone(), facts.not_after_ms)),
            host: None,
        };
        // That the certificate was revoked is not said (ADR-0065 clause 8).
        if proofs.revoked(revocations) {
            debug!("refused: a revoked certificate");
            if let Some(audit) = &admission.audit {
                audit.refusal(
                    Entry::new("admission.revoked_proof", "refused")
                        .peer(peer)
                        .with(
                            "serial",
                            presented.as_ref().map(|facts| facts.id.serial.clone()),
                        ),
                );
            }
            return refuse(
                "revoked",
                unauthorized("the OpAMP endpoint requires a certificate of the fleet"),
            );
        }
    }
    let classified = certificate
        .map(|cert| admission.issuers.classify(cert.as_ref()))
        .unwrap_or(Peer::Member);
    if let Peer::Enrolling { .. } = &classified {
        // A bootstrap certificate opens nothing outside an enrolment window (ADR-0059 clause 21).
        // The handshake proved the certificate, so this is no guess and counts as no failure:
        // hosts that wait for an operator must not throttle the members behind the same address.
        if !admission.enrolment.as_ref().is_some_and(|e| e.is_open()) {
            debug!("refused: a bootstrap certificate outside an enrolment window");
            record_refusal("admission.refused", "refused", "enrolment_window");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "no enrolment window is open",
            )
                .into_response();
        }
    }
    if let Some(audit) = &admission.audit {
        let serial = presented.as_ref().map(|facts| facts.id.serial.as_str());
        if admission.worth_recording(websocket, peer, serial) {
            let entry = Entry::new("admission.admitted", "admitted")
                .peer(peer)
                .with("plane", "agent")
                .with("transport", if websocket { "websocket" } else { "http" })
                .with(
                    "as",
                    match &classified {
                        Peer::Member => "member",
                        Peer::Enrolling { .. } => "enrolling",
                    },
                )
                .with(
                    "authority",
                    presented
                        .as_ref()
                        .and_then(|facts| admission.issuers_role(&facts.id.issuer)),
                )
                .with("serial", serial.map(str::to_string));
            // Not admitted without its record (ADR-0063 clause 6).
            if audit.record(entry).is_err() {
                return busy();
            }
            if !websocket {
                admission.recorded(peer, serial);
            }
        }
    }
    request.extensions_mut().insert(classified);
    request.extensions_mut().insert(Proofs {
        host: presented.as_ref().and_then(|facts| facts.host.clone()),
        certificate: presented.map(|facts| (facts.id, facts.not_after_ms)),
    });
    next.run(request).await
}

/// The Server's handler behind the endpoint: every report goes to the fleet, and a WebSocket is
/// also sent what the fleet wants its Agents to have when that changes.
struct Fleet(Arc<AppState>);

/// One connection as the fleet sees it.
struct Carrier {
    transport: Transport,
    /// Set on an enrolment connection (ADR-0059 clause 21): the bootstrap certificate it carried,
    /// where from, and the key its request asks to be certified, once it has sent one.
    enrolling: Option<Enrolling>,
    /// This connection's identity — what the duplicate detection tells connections apart by. Plain
    /// HTTP is stateless polling, so there is none to pass.
    conn: Option<ConnId>,
    /// The Agents this socket carried, any number of them told apart by `instance_uid` alone
    /// (ADR-0009), so all of them are marked unreachable when it goes.
    seen: Vec<InstanceUid>,
    /// What admitted the connection: re-checked before a CSR is signed, and the presented
    /// certificate is the predecessor of one it renews (ADR-0065 clauses 2, 4).
    proofs: Proofs,
    /// Why the session was ended, once its [`Guard`] has ended it.
    ended: Arc<Mutex<Option<&'static str>>>,
}

/// What an enrolling connection carried, and what it has asked for.
struct Enrolling {
    requester: Requester,
    request: Arc<Mutex<Option<crate::enrolment::Request>>>,
    instance_uid: Arc<Mutex<Vec<u8>>>,
}

/// What a connection is sent without asking. A member hears of every change to the fleet's
/// desired state — the "within seconds" of the control loop. An enrolling host hears of its own
/// request's decision, and its connection ends when the window closes or the request is rejected.
enum Pushes {
    Desired(watch::Receiver<u64>),
    Enrolment {
        changes: watch::Receiver<u64>,
        enrolment: Arc<Enrolment>,
        request: Arc<Mutex<Option<crate::enrolment::Request>>>,
    },
}

/// A WebSocket session's outbound side, and what ends it: a revocation of what admitted it, or the
/// expiry of its certificate (ADR-0065 clauses 9, 10).
struct Session {
    pushes: Pushes,
    guard: Option<Guard>,
}

struct Guard {
    revocations: Arc<Revocations>,
    audit: Option<Arc<dyn Audit>>,
    changes: watch::Receiver<u64>,
    proofs: Proofs,
    expires: Option<tokio::time::Instant>,
    ended: Arc<Mutex<Option<&'static str>>>,
}

impl Guard {
    fn new(
        revocations: Arc<Revocations>,
        audit: Option<Arc<dyn Audit>>,
        proofs: Proofs,
        ended: Arc<Mutex<Option<&'static str>>>,
    ) -> Self {
        let expires = proofs.certificate.as_ref().map(|(_, not_after_ms)| {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            tokio::time::Instant::now()
                + std::time::Duration::from_millis(not_after_ms.saturating_sub(now_ms))
        });
        let mut changes = revocations.subscribe();
        // A revocation that landed between admission and here is checked at once.
        changes.mark_changed();
        Guard {
            changes,
            revocations,
            audit,
            proofs,
            expires,
            ended,
        }
    }

    /// Waits until the session must end, and says why.
    async fn tripped(&mut self) -> &'static str {
        let Guard {
            revocations,
            changes,
            proofs,
            expires,
            ..
        } = self;
        let expiry = async {
            match expires {
                Some(at) => tokio::time::sleep_until(*at).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(expiry);
        loop {
            tokio::select! {
                () = &mut expiry => return "certificate expired",
                changed = changes.changed() => {
                    if changed.is_err() {
                        // The list is gone with the Server; only the expiry is left to wait for.
                        (&mut expiry).await;
                        return "certificate expired";
                    }
                    if proofs.revoked(revocations) {
                        return "revoked";
                    }
                }
            }
        }
    }
}

impl Outbound for Session {
    type Item = Push;

    async fn next(&mut self) -> Option<Push> {
        let Session { pushes, guard } = self;
        let Some(guard) = guard else {
            return pushes.next().await;
        };
        let ended = guard.ended.clone();
        let audit = guard.audit.clone();
        let serial = guard
            .proofs
            .certificate
            .as_ref()
            .map(|(id, _)| id.serial.clone());
        tokio::select! {
            item = pushes.next() => item,
            reason = guard.tripped() => {
                info!(reason, "ending a session");
                if let Some(audit) = audit {
                    audit.refusal(
                        Entry::new("session.ended", "ended")
                            .with("reason", reason)
                            .with("serial", serial),
                    );
                }
                *ended.lock().expect("ended lock") = Some(reason);
                None
            }
        }
    }
}

/// One occasion to push.
enum Push {
    Desired,
    Enrolment,
}

impl Pushes {
    async fn next(&mut self) -> Option<Push> {
        match self {
            Pushes::Desired(changes) => changes.changed().await.ok().map(|()| Push::Desired),
            Pushes::Enrolment {
                changes,
                enrolment,
                request,
            } => {
                changes.changed().await.ok()?;
                let key = request
                    .lock()
                    .expect("request lock")
                    .as_ref()
                    .map(|r| r.key_fingerprint.clone());
                if !enrolment.is_open() || key.is_some_and(|key| enrolment.is_rejected(&key)) {
                    return None;
                }
                Some(Push::Enrolment)
            }
        }
    }
}

impl Handler for Fleet {
    type Connection = Carrier;
    type Outbound = Session;

    fn on_connecting(
        &self,
        request: &RequestInfo<'_>,
    ) -> Result<(Carrier, Option<Session>), Rejection> {
        let proofs = request
            .extensions
            .get::<Proofs>()
            .cloned()
            .unwrap_or_default();
        let ended = Arc::new(Mutex::new(None));
        let session = |pushes: Pushes| Session {
            pushes,
            guard: self.0.revocations().map(|revocations| {
                Guard::new(
                    revocations.clone(),
                    self.0.audit().cloned(),
                    proofs.clone(),
                    ended.clone(),
                )
            }),
        };
        let websocket = request.transport == opamp::server::Transport::WebSocket;
        if let (
            Some(Peer::Enrolling {
                subject,
                fingerprint,
            }),
            Some(enrolment),
        ) = (request.extensions.get::<Peer>(), self.0.enrolment())
        {
            let enrolling = Enrolling {
                requester: Requester {
                    bootstrap_subject: subject.clone(),
                    bootstrap_fingerprint: fingerprint.clone(),
                    peer: request.peer.map(|peer| peer.ip()),
                },
                request: Arc::new(Mutex::new(None)),
                instance_uid: Arc::new(Mutex::new(Vec::new())),
            };
            let outbound = websocket.then(|| {
                session(Pushes::Enrolment {
                    changes: enrolment.subscribe(),
                    enrolment: enrolment.clone(),
                    request: enrolling.request.clone(),
                })
            });
            return Ok((
                Carrier {
                    transport: if websocket {
                        Transport::WebSocket
                    } else {
                        Transport::Http
                    },
                    enrolling: Some(enrolling),
                    conn: None,
                    seen: Vec::new(),
                    proofs: proofs.clone(),
                    ended,
                },
                outbound,
            ));
        }
        let (transport, conn, outbound) = if websocket {
            (
                Transport::WebSocket,
                Some(self.0.connection_id()),
                Some(session(Pushes::Desired(self.0.subscribe()))),
            )
        } else {
            (Transport::Http, None, None)
        };
        Ok((
            Carrier {
                transport,
                enrolling: None,
                conn,
                seen: Vec::new(),
                proofs,
                ended,
            },
            outbound,
        ))
    }

    async fn on_message(&self, carrier: &mut Carrier, report: AgentToServer) -> Reply {
        if let Some(enrolling) = &carrier.enrolling {
            return Reply::Send(self.enrol(enrolling, &report));
        }
        let csr = report
            .connection_settings_request
            .as_ref()
            .and_then(|request| request.opamp.as_ref())
            .is_some_and(|opamp| opamp.certificate_request.is_some());
        if csr {
            if let Some(revocations) = self.0.revocations() {
                // A session whose proof was revoked a moment ago is not handed a certificate
                // before its guard closes it (ADR-0065 clause 4).
                if carrier.proofs.revoked(revocations) {
                    return Reply::Send(bad_request("this connection is no longer admitted"));
                }
            }
        }
        // The connection's own certificate, whatever the message claims to be: behind a Gateway
        // that is the Gateway's, so revoking it reaches what was renewed through it without a
        // renewal proof (ADR-0065 clauses 2, 11).
        let presented = carrier
            .proofs
            .certificate
            .as_ref()
            .map(|(id, _)| Presented {
                id: id.clone(),
                host: carrier.proofs.host.clone(),
            });
        let outcome =
            self.0
                .process_presented(report, carrier.transport, carrier.conn, presented.as_ref());
        if let (Some(uid), Some(_)) = (outcome.uid, carrier.conn) {
            if outcome.disconnected {
                carrier.seen.retain(|s| s != &uid);
            } else if !carrier.seen.contains(&uid) {
                carrier.seen.push(uid);
            }
        }
        Reply::Send(outcome.reply)
    }

    fn on_outbound(&self, carrier: &mut Carrier, push: Push) -> Vec<ServerToAgent> {
        if let (Push::Enrolment, Some(enrolling)) = (push, &carrier.enrolling) {
            // A decision reached while the host waits: an approval is handed over at once.
            let Some(request) = enrolling.request.lock().expect("request lock").clone() else {
                return Vec::new();
            };
            let Some(enrolment) = self.0.enrolment() else {
                return Vec::new();
            };
            let uid = enrolling.instance_uid.lock().expect("uid lock").clone();
            return match enrolment.submit(request, enrolling.requester.clone()) {
                Submitted::Issued(cert) => vec![self.0.enrolment_answer(&uid, Some(cert))],
                _ => Vec::new(),
            };
        }
        let mut messages = Vec::new();
        for uid in &carrier.seen {
            // A queued restart goes first, as its own message — the Baseline's command message is
            // never combined with an offer.
            if let Some(command) = self.0.restart_command_for(uid) {
                debug!(agent = %uid, "pushing a restart command");
                messages.push(command);
            }
            if let Some(offer) = self.0.offer_for(uid) {
                debug!(agent = %uid, "pushing a configuration offer");
                messages.push(offer);
            }
        }
        messages
    }

    fn on_unreadable(&self, carrier: &mut Carrier, _error: &Unreadable) -> Reply {
        Reply::Send(bad_request(match carrier.transport {
            Transport::Http => "the request body is not a valid AgentToServer message",
            Transport::WebSocket => "the frame is not a valid OpAMP message",
        }))
    }

    fn closing(&self, carrier: &Carrier) -> Option<Closing> {
        let reason = (*carrier.ended.lock().expect("ended lock"))?;
        Some(Closing::policy(reason))
    }

    /// The connection is gone; every Agent it carried is unreachable until it reports again.
    /// An enrolling host was never one of the fleet's Agents, so there is nothing to mark.
    fn on_closed(&self, carrier: Carrier) {
        if let Some(conn) = carrier.conn {
            self.0.mark_disconnected(&carrier.seen, conn);
        }
    }
}

impl Fleet {
    /// One message on an enrolment connection (ADR-0059 clause 21). Nothing in it is read but the
    /// certificate signing request: no Agent record is created and nothing is offered but the
    /// issued certificate.
    fn enrol(&self, enrolling: &Enrolling, report: &AgentToServer) -> ServerToAgent {
        let Some(enrolment) = self.0.enrolment() else {
            return bad_request("this Server does not enrol");
        };
        *enrolling.instance_uid.lock().expect("uid lock") = report.instance_uid.clone();
        let Some(csr) = report
            .connection_settings_request
            .as_ref()
            .and_then(|request| request.opamp.as_ref())
            .and_then(|opamp| opamp.certificate_request.as_ref())
        else {
            // The capabilities say a request may be sent; nothing else is answered.
            return self.0.enrolment_answer(&report.instance_uid, None);
        };
        // A request claiming another identity never reaches the queue (ADR-0050).
        let request = match String::from_utf8(csr.csr.clone())
            .map_err(|_| "the certificate signing request is not PEM".to_string())
            .and_then(|pem| {
                crate::ca::check_claims(&pem, &report.instance_uid)?;
                crate::ca::enrolment_request(&pem, &report.instance_uid)
            }) {
            Ok(request) => request,
            Err(e) => return bad_request(&e),
        };
        *enrolling.request.lock().expect("request lock") = Some(request.clone());
        let key = request.key_fingerprint.clone();
        let new = !enrolment.pending().iter().any(|pending| pending.id == key);
        let subject = request.subject.clone();
        match enrolment.submit(request, enrolling.requester.clone()) {
            Submitted::Waiting => {
                info!(key = %key, "an enrolment request waits for an operator");
                if let (true, Some(audit)) = (new, self.0.audit()) {
                    audit.refusal(
                        Entry::new("enrolment.queued", "queued")
                            .peer(enrolling.requester.peer)
                            .with("id", key.clone())
                            .with("subject", subject)
                            .with(
                                "bootstrap_subject",
                                enrolling.requester.bootstrap_subject.clone(),
                            )
                            .with("instance_uid", hex::encode(&report.instance_uid)),
                    );
                }
                self.0.enrolment_answer(&report.instance_uid, None)
            }
            Submitted::Issued(cert) => {
                info!(key = %key, "handed an enrolling host its certificate");
                self.0.enrolment_answer(&report.instance_uid, Some(cert))
            }
            Submitted::Rejected => bad_request("an operator rejected this enrolment request"),
            Submitted::Full => unavailable("the enrolment queue is full — retry later"),
            Submitted::Closed => unavailable("no enrolment window is open"),
        }
    }
}
