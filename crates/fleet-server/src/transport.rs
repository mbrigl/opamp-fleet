//! The OpAMP endpoint — both transports on one path (ADR-0012).
//!
//! `/v1/opamp` serves the whole protocol: a request carrying the protobuf `Content-Type` is the
//! plain-HTTP transport, a WebSocket upgrade is the other — exactly the detection the Baseline
//! describes. The communication is `opamp::server`'s, shared with the Gateway and the Supervisor
//! Endpoint (ADR-0032); what is the Server's is the [`Fleet`] handler, which hands every decoded
//! report to the same [`AppState::process`], so transport is carriage, never semantics.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::server::listen::PeerCertificate;
use opamp::server::{Handler, Outbound, Rejection, Reply, RequestInfo, Settings, Unreadable};
use opamp::uid::InstanceUid;
use tokio::sync::watch;
use tracing::{debug, info};

use crate::config::AuthConfig;
use crate::credentials::Credentials;
use crate::enrolment::{Enrolment, Requester, Submitted};
use crate::fleet::{bad_request, unavailable, AppState, ConnId, Transport};
use crate::throttle::Throttle;
use crate::tls::{Issuers, Peer};

/// The endpoint path the Baseline names as the default, and the protobuf media type it requires —
/// both from the shared crate, because the Gateway serves the same endpoint (ADR-0011).
pub use opamp::endpoint::{OPAMP_PATH, PROTOBUF_CONTENT_TYPE};

/// The OpAMP endpoint's credential check (ADR-0017), precomputed from the `[auth]` section — Bearer
/// and Basic alike. The comparison itself lives in [`crate::credentials`], shared with the Operator
/// plane's own check (ADR-0017).
pub struct OpampAuth(Credentials);

impl OpampAuth {
    pub fn from_config(auth: &AuthConfig) -> Self {
        OpampAuth(Credentials::new(auth.accepted_headers(), auth.challenge()))
    }
}

/// What a peer must prove to reach `/v1/opamp` (ADR-0039): **both** a fleet credential and a
/// client certificate the handshake verified. A certificate from the bootstrap CA admits an
/// enrolling host, and only while an operator holds the enrolment window open. Repeated failures
/// from one peer address are throttled before the credential is even compared.
///
/// The rule is deliberately not "either one". Header authorization is what the Baseline expects an
/// Agent to carry and client certificates are what it adds on top — so stacking them is the
/// protocol's own layering.
#[derive(Default)]
pub struct Admission {
    auth: Option<OpampAuth>,
    /// The connection must have carried a certificate. The certificate itself is already verified
    /// — rustls refuses one it cannot chain — so this is a presence check, never a second
    /// verification.
    require_client_certificate: bool,
    issuers: Issuers,
    enrolment: Option<Arc<Enrolment>>,
    throttle: Option<Arc<Throttle>>,
}

impl Admission {
    /// No proof required — what a test that is not about admission serves with. A Server never
    /// does: its configuration requires both proofs (ADR-0039).
    pub fn open() -> Self {
        Admission::default()
    }

    pub fn new(auth: Option<OpampAuth>, require_client_certificate: bool) -> Self {
        Admission {
            auth,
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

    /// Throttles repeated failures from one peer address (ADR-0039 clause 24).
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<Throttle>) -> Self {
        self.throttle = Some(throttle);
        self
    }

    fn required(&self) -> bool {
        self.auth.is_some() || self.require_client_certificate || self.throttle.is_some()
    }
}

/// What the package download route asks of a peer (ADR-0039 clause 23): it sits outside the
/// credential check, behind the same handshake, and a bootstrap certificate is not enough.
#[derive(Clone, Default)]
pub struct DownloadGuard {
    require_client_certificate: bool,
    issuers: Issuers,
    throttle: Option<Arc<Throttle>>,
}

impl Admission {
    /// The download route's share of these rules.
    #[must_use]
    pub fn download_guard(&self) -> DownloadGuard {
        DownloadGuard {
            require_client_certificate: self.require_client_certificate,
            issuers: self.issuers.clone(),
            throttle: self.throttle.clone(),
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
    if let (Some(throttle), Some(peer)) = (&guard.throttle, peer) {
        if let Some(wait) = throttle.retry_after(peer) {
            return throttled(wait);
        }
    }
    let member = match request
        .extensions()
        .get::<PeerCertificate>()
        .and_then(|peer| peer.0.as_ref())
    {
        Some(cert) => guard.issuers.classify(cert.as_ref()) == Peer::Member,
        None => !guard.require_client_certificate,
    };
    if !member {
        if let (Some(throttle), Some(peer)) = (&guard.throttle, peer) {
            throttle.failed(peer);
        }
        return (
            StatusCode::UNAUTHORIZED,
            "the package download requires a certificate of the fleet",
        )
            .into_response();
    }
    next.run(request).await
}

pub fn router(state: Arc<AppState>, admission: Admission) -> Router {
    // The limits the Baseline requires of the Server, on both transports and in both directions.
    let settings = Settings::new(state.max_message_size());
    let mut router = opamp::server::router(Arc::new(Fleet(state)), settings);
    if admission.required() {
        // The outermost layer: every plain-HTTP POST and the upgrade GET — checked before the
        // WebSocket upgrade completes — answers 401 when a required proof is missing.
        router = router.layer(middleware::from_fn_with_state(Arc::new(admission), admit));
    }
    router
}

/// The peer's address, as the listener put it into the request.
pub fn peer_ip(request: &Request) -> Option<std::net::IpAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
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
    // What this gate proves is *fleet membership*, not which Agent is speaking: the credential and
    // the client certificate are fleet-wide, and `instance_uid` stays self-asserted behind them
    // (ADR-0039). Admission is the trust boundary; there is no authorization between admitted Agents.
    let peer = peer_ip(&request);
    if let (Some(throttle), Some(peer)) = (&admission.throttle, peer) {
        if let Some(wait) = throttle.retry_after(peer) {
            return throttled(wait);
        }
    }
    let refuse = |response: Response| {
        if let (Some(throttle), Some(peer)) = (&admission.throttle, peer) {
            throttle.failed(peer);
        }
        response
    };
    let certificate = request
        .extensions()
        .get::<PeerCertificate>()
        .and_then(|peer| peer.0.clone());
    if admission.require_client_certificate && certificate.is_none() {
        debug!("refused: the OpAMP endpoint requires a client certificate");
        return refuse(
            (
                StatusCode::UNAUTHORIZED,
                "the OpAMP endpoint requires a client certificate",
            )
                .into_response(),
        );
    }
    if let Some(auth) = &admission.auth {
        if !auth.0.permits(request.headers()) {
            return refuse(
                (
                    StatusCode::UNAUTHORIZED,
                    [(header::WWW_AUTHENTICATE, auth.0.challenge().to_string())],
                    "the OpAMP endpoint requires authentication",
                )
                    .into_response(),
            );
        }
    }
    let classified = certificate
        .map(|cert| admission.issuers.classify(cert.as_ref()))
        .unwrap_or(Peer::Member);
    if let Peer::Enrolling { .. } = &classified {
        // A bootstrap certificate opens nothing outside an enrolment window (ADR-0039 clause 21).
        // The credential was right, so this is no guess and counts as no failure: hosts that
        // wait for an operator must not throttle the members behind the same address.
        if !admission.enrolment.as_ref().is_some_and(|e| e.is_open()) {
            debug!("refused: a bootstrap certificate outside an enrolment window");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "no enrolment window is open",
            )
                .into_response();
        }
    }
    request.extensions_mut().insert(classified);
    next.run(request).await
}

/// The Server's handler behind the endpoint: every report goes to the fleet, and a WebSocket is
/// also sent what the fleet wants its Agents to have when that changes.
struct Fleet(Arc<AppState>);

/// One connection as the fleet sees it.
struct Carrier {
    transport: Transport,
    /// Set on an enrolment connection (ADR-0039 clause 21): the bootstrap certificate it carried,
    /// where from, and the key its request asks to be certified, once it has sent one.
    enrolling: Option<Enrolling>,
    /// This connection's identity — what the duplicate detection tells connections apart by. Plain
    /// HTTP is stateless polling, so there is none to pass.
    conn: Option<ConnId>,
    /// The Agents this socket carried, any number of them told apart by `instance_uid` alone
    /// (ADR-0009), so all of them are marked unreachable when it goes.
    seen: Vec<InstanceUid>,
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

/// One occasion to push.
enum Push {
    Desired,
    Enrolment,
}

impl Outbound for Pushes {
    type Item = Push;

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
    type Outbound = Pushes;

    fn on_connecting(
        &self,
        request: &RequestInfo<'_>,
    ) -> Result<(Carrier, Option<Pushes>), Rejection> {
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
            let outbound = websocket.then(|| Pushes::Enrolment {
                changes: enrolment.subscribe(),
                enrolment: enrolment.clone(),
                request: enrolling.request.clone(),
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
                },
                outbound,
            ));
        }
        let (transport, conn, outbound) = if websocket {
            (
                Transport::WebSocket,
                Some(self.0.connection_id()),
                Some(Pushes::Desired(self.0.subscribe())),
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
            },
            outbound,
        ))
    }

    async fn on_message(&self, carrier: &mut Carrier, report: AgentToServer) -> Reply {
        if let Some(enrolling) = &carrier.enrolling {
            return Reply::Send(self.enrol(enrolling, &report));
        }
        let outcome = self.0.process(report, carrier.transport, carrier.conn);
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

    /// The connection is gone; every Agent it carried is unreachable until it reports again.
    /// An enrolling host was never one of the fleet's Agents, so there is nothing to mark.
    fn on_closed(&self, carrier: Carrier) {
        if let Some(conn) = carrier.conn {
            self.0.mark_disconnected(&carrier.seen, conn);
        }
    }
}

impl Fleet {
    /// One message on an enrolment connection (ADR-0039 clause 21). Nothing in it is read but the
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
        let request = match String::from_utf8(csr.csr.clone())
            .map_err(|_| "the certificate signing request is not PEM".to_string())
            .and_then(|pem| crate::ca::enrolment_request(&pem))
        {
            Ok(request) => request,
            Err(e) => return bad_request(&e),
        };
        *enrolling.request.lock().expect("request lock") = Some(request.clone());
        let key = request.key_fingerprint.clone();
        match enrolment.submit(request, enrolling.requester.clone()) {
            Submitted::Waiting => {
                info!(key = %key, "an enrolment request waits for an operator");
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
