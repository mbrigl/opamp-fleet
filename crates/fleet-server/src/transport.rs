//! The OpAMP endpoint — both transports on one path (ADR-0012).
//!
//! `/v1/opamp` serves the whole protocol: a request carrying the protobuf `Content-Type` is the
//! plain-HTTP transport, a WebSocket upgrade is the other — exactly the detection the Baseline
//! describes. The communication is `opamp::server`'s, shared with the Gateway and the Supervisor
//! Endpoint (ADR-0032); what is the Server's is the [`Fleet`] handler, which hands every decoded
//! report to the same [`AppState::process`], so transport is carriage, never semantics.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::server::{Handler, Outbound, Rejection, Reply, RequestInfo, Settings, Unreadable};
use opamp::uid::InstanceUid;
use tokio::sync::watch;
use tracing::debug;

use crate::config::AuthConfig;
use crate::credentials::Credentials;
use crate::fleet::{bad_request, AppState, ConnId, Transport};

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

/// What a peer must prove to reach `/v1/opamp`. **Every configured mechanism must succeed**
/// (ADR-0017): a credential when `[auth]` is set, a client certificate when `[tls] client_ca_file`
/// is, both when both are. Nothing configured leaves the endpoint open, as it has always been.
///
/// The rule is deliberately not "either one". Header authorization is what the Baseline expects an
/// Agent to carry and client certificates are what it adds "optionally also" on top — so stacking
/// them is the protocol's own layering, and it is the only rule under which switching mutual TLS on
/// cannot make a fleet admit anything it did not admit before.
#[derive(Default)]
pub struct Admission {
    auth: Option<OpampAuth>,
    /// Set while the listener has a client CA: the connection must have carried a certificate.
    /// The certificate itself is already verified — rustls refuses one it cannot chain — so this
    /// is a presence check, never a second verification.
    require_client_certificate: bool,
}

impl Admission {
    /// No proof required — the default deployment, and every test that is not about admission.
    pub fn open() -> Self {
        Admission::default()
    }

    pub fn new(auth: Option<OpampAuth>, require_client_certificate: bool) -> Self {
        Admission {
            auth,
            require_client_certificate,
        }
    }

    fn required(&self) -> bool {
        self.auth.is_some() || self.require_client_certificate
    }
}

pub fn router(state: Arc<AppState>, admission: Admission) -> Router {
    // The limits the Baseline requires of the Server, on both transports and in both directions.
    let settings = Settings::new(state.max_message_size());
    let mut router = opamp::server::router(Arc::new(Fleet(state)), settings);
    if admission.required() {
        // The outermost layer: every plain-HTTP POST and the upgrade GET — checked before the
        // WebSocket upgrade completes — answers 401 when a required proof is missing (ADR-0017,
        // ADR-0017).
        router = router.layer(middleware::from_fn_with_state(Arc::new(admission), admit));
    }
    router
}

async fn admit(State(admission): State<Arc<Admission>>, request: Request, next: Next) -> Response {
    // What this gate proves is *fleet membership*, not which Agent is speaking: the credential and
    // the client certificate are fleet-wide, and `instance_uid` stays self-asserted behind them
    // (ADR-0017). Admission is the trust boundary; there is no authorization between admitted Agents.
    // Every configured proof, not the first that happens to pass.
    if admission.require_client_certificate {
        let presented = request
            .extensions()
            .get::<opamp::server::listen::PeerCertificate>()
            .is_some_and(opamp::server::listen::PeerCertificate::present);
        if !presented {
            debug!("refused: the OpAMP endpoint requires a client certificate");
            return (
                StatusCode::UNAUTHORIZED,
                "the OpAMP endpoint requires a client certificate",
            )
                .into_response();
        }
    }
    if let Some(auth) = &admission.auth {
        if !auth.0.permits(request.headers()) {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, auth.0.challenge().to_string())],
                "the OpAMP endpoint requires authentication",
            )
                .into_response();
        }
    }
    next.run(request).await
}

/// The Server's handler behind the endpoint: every report goes to the fleet, and a WebSocket is
/// also sent what the fleet wants its Agents to have when that changes.
struct Fleet(Arc<AppState>);

/// One connection as the fleet sees it.
struct Carrier {
    transport: Transport,
    /// This connection's identity — what the duplicate detection tells connections apart by. Plain
    /// HTTP is stateless polling, so there is none to pass.
    conn: Option<ConnId>,
    /// The Agents this socket carried, any number of them told apart by `instance_uid` alone
    /// (ADR-0009), so all of them are marked unreachable when it goes.
    seen: Vec<InstanceUid>,
}

/// A change to the fleet's desired state — the "within seconds" of the control loop, reaching
/// connected Agents without waiting for them to speak.
struct DesiredState(watch::Receiver<u64>);

impl Outbound for DesiredState {
    type Item = ();

    async fn next(&mut self) -> Option<()> {
        self.0.changed().await.ok()
    }
}

impl Handler for Fleet {
    type Connection = Carrier;
    type Outbound = DesiredState;

    fn on_connecting(
        &self,
        request: &RequestInfo<'_>,
    ) -> Result<(Carrier, Option<DesiredState>), Rejection> {
        let (transport, conn, outbound) = match request.transport {
            opamp::server::Transport::WebSocket => (
                Transport::WebSocket,
                Some(self.0.connection_id()),
                Some(DesiredState(self.0.subscribe())),
            ),
            opamp::server::Transport::Http => (Transport::Http, None, None),
        };
        Ok((
            Carrier {
                transport,
                conn,
                seen: Vec::new(),
            },
            outbound,
        ))
    }

    async fn on_message(&self, carrier: &mut Carrier, report: AgentToServer) -> Reply {
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

    fn on_outbound(&self, carrier: &mut Carrier, (): ()) -> Vec<ServerToAgent> {
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
    fn on_closed(&self, carrier: Carrier) {
        if let Some(conn) = carrier.conn {
            self.0.mark_disconnected(&carrier.seen, conn);
        }
    }
}
