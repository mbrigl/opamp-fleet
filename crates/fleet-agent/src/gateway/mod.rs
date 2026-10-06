//! Gateway Mode (ADR-0071): the Client at a network boundary.
//!
//! It is an OpAMP **server** downstream and an OpAMP **client** upstream, and it folds many
//! downstream connections onto a small pool of upstream ones. What it does *not* do is as
//! load-bearing as what it does: it forwards messages unchanged, holds no admission policy of its
//! own — the one admission refusal beyond its handshake is a certificate its Server revoked
//! ([`revocations`]), and the one refusal beyond admission is a download the Server did not offer
//! to the requesting host through this Gateway ([`cache`]) — forwards no `Authorization`, and
//! never speaks in an Agent's name, not even to say the goodbye a vanished Agent did not send.
//!
//! The pieces: this module serves the downstream endpoint on both transports (a downstream Client
//! picks its transport by URL scheme, so serving only one would silently exclude half of them),
//! [`pool`] holds the upstream connections, [`registry`] routes replies back by `instance_uid`, and
//! [`cache`] holds the uploaded artifacts the Gateway relays offers of and serves them on the
//! download route to the hosts they were offered to (ADR-0070).
//! The endpoint's communication is `opamp::server`'s, the one the Server sits on too (ADR-0032);
//! what is the Gateway's is the [`Forwarding`] handler behind it.

pub mod cache;
pub mod pool;
pub mod registry;
pub mod revocations;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use axum::http::{header, StatusCode};
use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::server::listen::{
    ClientAuth, Handle, Listener, PeerCertificate, ServerTls, HEADER_READ_TIMEOUT,
};
use opamp::server::{
    Closing, Handler, Outbound, Rejection, Reply, RequestInfo, Settings, Transport, Unreadable,
};
use opamp::tls::Identity;
use opamp::uid::InstanceUid;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::config::{ClientConfig, GatewayTlsConfig};
use crate::shutdown::Shutdown;
use pool::Pool;
use registry::Registry;
use revocations::{
    RevocationList, Verdict, REVOCATION_FIRST_WAIT, REVOCATION_MAX_AGE, REVOCATION_REFRESH,
};

/// How long the downstream endpoint has to drain in-flight exchanges once shutdown is requested,
/// before connections are dropped. One [`EXCHANGE_TIMEOUT`] plus a little, so an exchange already
/// waiting on the Server is not cut short by the stop it did not cause.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(35);

/// How long a plain-HTTP peer waits for its reply to come back through the pool. Beyond this the
/// exchange fails and the peer retries, which is what its transport already does on any error.
const EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The Gateway's handler behind the downstream endpoint: what arrives goes upstream, what the
/// Server says comes back down.
struct Forwarding {
    pool: Pool,
    registry: Arc<Registry>,
    /// The most distinct Agents one downstream connection may carry (ADR-0071): past it a report
    /// for a *new* Agent is dropped, so a single peer cannot grow the routing state without bound.
    max_agents: usize,
    /// What the Server revoked, which this Gateway refuses (ADR-0071 clause 14).
    revocations: RevocationList,
}

/// The time bounds a Gateway runs with: the constants, unless a test drives them short.
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    pub header_read_timeout: std::time::Duration,
    pub revocation_refresh: std::time::Duration,
    pub revocation_max_age: std::time::Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            header_read_timeout: HEADER_READ_TIMEOUT,
            revocation_refresh: REVOCATION_REFRESH,
            revocation_max_age: REVOCATION_MAX_AGE,
        }
    }
}

/// Serves the downstream endpoint until `shutdown` fires.
///
/// Mutual TLS is per hop (ADR-0071): the downstream hop serves TLS 1.3 from `[gateway.tls]`, and
/// every downstream Agent must present a certificate that chains to its `client_ca_file` — the
/// access-control boundary of the hop. Both are required; a Gateway without them does not start.
/// Downstream Agents travel upstream under this Gateway's own member certificate, so that CA must
/// be the fleet's client CA and never a bootstrap CA.
///
/// # Errors
/// Returns an error when the configured address cannot be bound — at startup, so a taken port is
/// loud rather than a Gateway that quietly carries nobody — or when the TLS material cannot be read.
pub async fn run(config: Arc<ClientConfig>, shutdown: Shutdown) -> Result<(), String> {
    let Some(gateway) = &config.gateway else {
        return Ok(());
    };
    let listen = gateway.listen;
    let listener = std::net::TcpListener::bind(listen)
        .map_err(|e| format!("cannot bind the gateway endpoint {listen}: {e}"))?;
    run_on(config, listener, shutdown).await
}

/// Serves the downstream endpoint on a listener that is already bound, until `shutdown` fires —
/// what [`run`] does once it has bound the configured address. A caller that picks the port itself
/// binds first and hands the listener over, so the port cannot be taken in between.
///
/// # Errors
/// Returns an error when the listener cannot be used or the TLS material cannot be read.
pub async fn run_on(
    config: Arc<ClientConfig>,
    listener: std::net::TcpListener,
    shutdown: Shutdown,
) -> Result<(), String> {
    run_on_bounded(config, listener, shutdown, HEADER_READ_TIMEOUT).await
}

/// [`run_on`] with the header bound tightened — what a test waits out instead of the 30 seconds
/// every OpAMP listener applies (ADR-0036).
///
/// # Errors
/// As [`run_on`].
pub async fn run_on_bounded(
    config: Arc<ClientConfig>,
    listener: std::net::TcpListener,
    shutdown: Shutdown,
    header_read_timeout: std::time::Duration,
) -> Result<(), String> {
    let timings = Timings {
        header_read_timeout,
        ..Timings::default()
    };
    run_on_timed(config, listener, shutdown, timings).await
}

/// [`run_on`] with every time bound stated — what a test drives short.
///
/// # Errors
/// As [`run_on`].
pub async fn run_on_timed(
    config: Arc<ClientConfig>,
    listener: std::net::TcpListener,
    shutdown: Shutdown,
    timings: Timings,
) -> Result<(), String> {
    let header_read_timeout = timings.header_read_timeout;
    let Some(gateway) = &config.gateway else {
        return Ok(());
    };
    let listen = listener
        .local_addr()
        .map_err(|e| format!("cannot read the gateway endpoint's address: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("cannot prepare the gateway endpoint {listen}: {e}"))?;
    let cache = cache::PackageCache::open(config.clone(), shutdown.clone()).await?;
    let registry = Arc::new(Registry::new(cache.clone()));
    let revocations = RevocationList::new(timings.revocation_max_age);
    // Its first peers are not refused for want of a list, nor kept waiting for a Server that does
    // not answer (ADR-0071 clause 14).
    let _ = tokio::time::timeout(
        REVOCATION_FIRST_WAIT,
        revocations::update(&revocations, &config),
    )
    .await;
    tokio::spawn(revocations::refresh(
        revocations.clone(),
        config.clone(),
        shutdown.clone(),
        timings.revocation_refresh,
    ));
    let downloads = Arc::new(cache::Downloads {
        cache,
        revocations: revocations.clone(),
    });
    let handler = Arc::new(Forwarding {
        pool: Pool::new(config.clone(), registry.clone()),
        registry,
        max_agents: gateway.max_carried_agents,
        revocations,
    });
    // The receive and send limits the Baseline requires, enforced per hop; beside the OpAMP
    // endpoint, the download route the Agents behind this Gateway resolve their offers against
    // (ADR-0070 clause 11).
    let app = opamp::server::router(handler, Settings::new(config.max_message_size_bytes))
        .merge(cache::router(downloads));

    let upstream_cap = gateway.upstream_connections;
    // Mutual TLS 1.3 and nothing less (ADR-0071); the load refused a Gateway without it.
    let tls = gateway
        .tls
        .as_ref()
        .ok_or("[gateway.tls] is required — a Gateway admits Agents over mutual TLS only")?;
    let config = server_tls(tls)?
        .rustls_config()
        .map_err(|e| format!("the gateway endpoint: {e}"))?;
    let handle = Handle::new();
    let downstream = Listener::new(listener, handle.clone())
        .with_tls(config)
        .with_header_read_timeout(header_read_timeout);
    info!(%listen, upstream_cap, "gateway listening over mutual TLS");
    // The listener drains rather than drops: on shutdown in-flight exchanges finish, up to the
    // grace, instead of every downstream connection being torn down mid-message.
    let mut shutdown = shutdown;
    tokio::spawn(async move {
        shutdown.requested().await;
        handle.graceful_shutdown(Some(DRAIN_GRACE));
    });
    downstream
        .serve(app)
        .await
        .map_err(|e| format!("the gateway endpoint stopped: {e}"))
}

/// The material the downstream endpoint serves with (ADR-0071 clause 11): a client certificate
/// that chains to `client_ca_file` is **mandatory** in the handshake. The Gateway trusts the fleet's
/// client CA and never a bootstrap CA, so a host enrols with the Server directly (ADR-0059 clause 25).
fn server_tls(tls: &GatewayTlsConfig) -> Result<ServerTls, String> {
    let ca_file = tls
        .client_ca_file
        .as_ref()
        .ok_or("[gateway.tls] client_ca_file is required")?;
    let client_auth = ClientAuth::Required {
        ca_pem: crate::tls::certificates_file(ca_file)?,
    };
    Ok(ServerTls {
        identity: Identity {
            cert_pem: crate::tls::certificates_file(&tls.cert_file)?,
            key_pem: crate::tls::key_file(&tls.key_file)?,
        },
        client_auth,
    })
}

/// One downstream connection: a socket, or one plain-HTTP exchange.
struct Downstream {
    transport: Transport,
    peer: String,
    /// Where the registry routes this socket's replies; `None` on plain HTTP, whose one reply
    /// comes back through a `oneshot` instead.
    replies: Option<mpsc::Sender<ServerToAgent>>,
    /// Every Agent this peer turned out to carry, so all of them are released when it goes. A set,
    /// not a list: membership is checked per report, and the cap bounds how large it grows.
    carried: HashSet<InstanceUid>,
    /// Why the socket was ended from this side, for its close frame (ADR-0071 clause 14).
    ended: Arc<Mutex<Option<&'static str>>>,
    /// The certificate it was admitted with, which every report is judged by again.
    certificate: Vec<u8>,
    /// The host that certificate names, which the offers relayed over it are recorded for
    /// (ADR-0070 clause 11).
    host: Option<String>,
}

/// What the Server says about the Agents a socket carries, routed back to it by the registry —
/// until the revocation list no longer admits the certificate the socket was opened with.
struct Replies {
    replies: mpsc::Receiver<ServerToAgent>,
    revocations: RevocationList,
    changes: tokio::sync::watch::Receiver<u64>,
    certificate: Vec<u8>,
    ended: Arc<Mutex<Option<&'static str>>>,
}

impl Outbound for Replies {
    type Item = ServerToAgent;

    async fn next(&mut self) -> Option<ServerToAgent> {
        loop {
            tokio::select! {
                reply = self.replies.recv() => return reply,
                changed = self.changes.changed() => {
                    if changed.is_err() {
                        return self.replies.recv().await;
                    }
                    if let Some(reason) = self.revocations.verdict(&self.certificate).reason() {
                        info!(reason, "ending a downstream session");
                        *self.ended.lock().expect("ended lock") = Some(reason);
                        return None;
                    }
                }
            }
        }
    }
}

impl Handler for Forwarding {
    type Connection = Downstream;
    type Outbound = Replies;

    fn on_connecting(
        &self,
        request: &RequestInfo<'_>,
    ) -> Result<(Downstream, Option<Replies>), Rejection> {
        // The one admission refusal beyond the handshake, and it is the Server's (ADR-0071 clause 14).
        let certificate = request
            .extensions
            .get::<PeerCertificate>()
            .and_then(|peer| peer.0.as_ref())
            .map(|cert| cert.as_ref().to_vec())
            .unwrap_or_default();
        match self.revocations.verdict(&certificate) {
            Verdict::Admit => {}
            Verdict::Revoked => {
                return Err(Rejection {
                    status: StatusCode::UNAUTHORIZED,
                    headers: Vec::new(),
                    message: "this certificate is revoked".to_string(),
                })
            }
            Verdict::Stale => {
                return Err(Rejection {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    headers: vec![(header::RETRY_AFTER, header::HeaderValue::from_static("30"))],
                    message: "the Gateway holds no current revocation list".to_string(),
                })
            }
        }
        // An `Authorization` the peer sends is ignored, never refused, and nothing is forwarded in
        // its place (ADR-0071 clause 11).
        let peer = request
            .peer
            .map_or_else(|| "unknown".to_string(), |peer| peer.to_string());
        let ended = Arc::new(Mutex::new(None));
        let (replies, outbound) = match request.transport {
            Transport::WebSocket => {
                debug!(%peer, "a downstream client connected");
                let (tx, rx) = mpsc::channel::<ServerToAgent>(64);
                let mut changes = self.revocations.subscribe();
                changes.mark_unchanged();
                (
                    Some(tx),
                    Some(Replies {
                        replies: rx,
                        revocations: self.revocations.clone(),
                        changes,
                        certificate: certificate.clone(),
                        ended: ended.clone(),
                    }),
                )
            }
            Transport::Http => (None, None),
        };
        Ok((
            Downstream {
                transport: request.transport,
                peer,
                replies,
                carried: HashSet::new(),
                ended,
                host: cache::host_of(&certificate),
                certificate,
            },
            outbound,
        ))
    }

    fn closing(&self, downstream: &Downstream) -> Option<Closing> {
        (*downstream.ended.lock().expect("ended lock")).map(Closing::policy)
    }

    async fn on_message(&self, downstream: &mut Downstream, report: AgentToServer) -> Reply {
        match downstream.transport {
            Transport::WebSocket => self.forward_from_socket(downstream, report).await,
            Transport::Http => self.forward_exchange(downstream, report).await,
        }
    }

    fn on_outbound(&self, _: &mut Downstream, reply: ServerToAgent) -> Vec<ServerToAgent> {
        vec![reply]
    }

    /// An unreadable report is dropped, never answered: the Gateway invents nothing (ADR-0071). A
    /// plain-HTTP exchange still needs a response, and it says why.
    fn on_unreadable(&self, downstream: &mut Downstream, error: &Unreadable) -> Reply {
        warn!(peer = %downstream.peer, %error, "dropping an unreadable downstream report");
        match downstream.transport {
            Transport::WebSocket => Reply::Nothing,
            Transport::Http => Reply::Refuse(StatusCode::BAD_REQUEST, "unreadable report".into()),
        }
    }

    /// The peer is gone. Its Agents stop being routable here — and nothing is said upstream on
    /// their behalf, because they said nothing (ADR-0071 clause 10).
    fn on_closed(&self, downstream: Downstream) {
        if downstream.transport == Transport::WebSocket {
            let count = downstream.carried.len();
            self.registry.detach_all(downstream.carried);
            debug!(peer = %downstream.peer, agents = count, "a downstream client disconnected");
        }
    }
}

impl Forwarding {
    /// A socket's report goes upstream; whatever the Server answers comes back through the
    /// registry, so nothing is replied here.
    async fn forward_from_socket(
        &self,
        downstream: &mut Downstream,
        report: AgentToServer,
    ) -> Reply {
        let peer = &downstream.peer;
        // A session the list no longer admits is being closed; what it still sends goes nowhere.
        if self.revocations.verdict(&downstream.certificate) != Verdict::Admit {
            return Reply::Nothing;
        }
        let Some(uid) = InstanceUid::from_wire(&report.instance_uid) else {
            warn!(%peer, "dropping a downstream report with a malformed instance_uid");
            return Reply::Nothing;
        };
        if !downstream.carried.contains(&uid) {
            // Bound the routing state one connection can create: past the cap a report for a new
            // Agent is dropped, while the Agents already carried keep being served.
            if downstream.carried.len() >= self.max_agents {
                warn!(
                    %peer, agent = %uid, cap = self.max_agents,
                    "a downstream connection reached its Agent cap; dropping a report for a new Agent"
                );
                return Reply::Nothing;
            }
            downstream.carried.insert(uid);
            info!(agent = %uid, %peer, "carrying an Agent");
        }
        if let Some(replies) = &downstream.replies {
            self.registry
                .attach(uid, replies.clone(), downstream.host.clone());
        }
        if let Err(e) = self.pool.forward(uid, &report).await {
            warn!(agent = %uid, error = %e, "cannot forward a report upstream");
        }
        Reply::Nothing
    }

    /// The plain-HTTP half: one report in, one reply out, with the pool in between.
    async fn forward_exchange(&self, downstream: &Downstream, report: AgentToServer) -> Reply {
        let Some(uid) = InstanceUid::from_wire(&report.instance_uid) else {
            return Reply::Refuse(
                StatusCode::BAD_REQUEST,
                "instance_uid must be 16 bytes".into(),
            );
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        self.registry
            .expect_once(uid, reply_tx, downstream.host.clone());
        if let Err(e) = self.pool.forward(uid, &report).await {
            warn!(agent = %uid, error = %e, "cannot forward a report upstream");
            return Reply::Refuse(StatusCode::BAD_GATEWAY, "cannot reach the Server".into());
        }
        match tokio::time::timeout(EXCHANGE_TIMEOUT, reply_rx).await {
            Ok(Ok(reply)) => Reply::Send(reply),
            _ => {
                debug!(agent = %uid, "no reply came back for a forwarded report");
                Reply::Refuse(
                    StatusCode::GATEWAY_TIMEOUT,
                    "no reply from the Server".into(),
                )
            }
        }
    }
}
