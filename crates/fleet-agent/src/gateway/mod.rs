//! Gateway Mode (ADR-0009): the Client at a network boundary.
//!
//! It is an OpAMP **server** downstream and an OpAMP **client** upstream, and it folds many
//! downstream connections onto a small pool of upstream ones. What it does *not* do is as
//! load-bearing as what it does: it forwards messages unchanged, makes no authentication decision,
//! and never speaks in an Agent's name — not even to say the goodbye a vanished Agent did not send.
//!
//! Three pieces: this module serves the downstream endpoint on both transports (a downstream Client
//! picks its transport by URL scheme, so serving only one would silently exclude half of them),
//! [`pool`] holds the upstream connections, and [`registry`] routes replies back by `instance_uid`.
//! The endpoint's communication is `opamp::server`'s, the one the Server sits on too (ADR-0032);
//! what is the Gateway's is the [`Forwarding`] handler behind it.

pub mod pool;
pub mod registry;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::{header, StatusCode};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;
use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::server::{
    Handler, Outbound, Rejection, Reply, RequestInfo, Settings, Transport, Unreadable,
};
use opamp::uid::InstanceUid;
use rustls::server::WebPkiClientVerifier;
use rustls::ServerConfig;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::config::{ClientConfig, GatewayTlsConfig};
use crate::shutdown::Shutdown;
use pool::Pool;
use registry::Registry;

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
    /// The most distinct Agents one downstream connection may carry (ADR-0009): past it a report
    /// for a *new* Agent is dropped, so a single peer cannot grow the routing state without bound.
    max_agents: usize,
}

/// Serves the downstream endpoint until `shutdown` fires.
///
/// Mutual TLS is per hop (ADR-0017, ADR-0009): with a `[gateway.tls]` section the downstream hop is
/// encrypted and, when a `client_ca_file` is configured, every downstream Agent must present a
/// certificate that chains to it — the access-control boundary the section exists for. Without the
/// section the hop is plaintext, which a fleet still bootstrapping may want but which also carries
/// the `Authorization` credential in the clear, so it is announced rather than assumed.
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
    let Some(gateway) = &config.gateway else {
        return Ok(());
    };
    let listen = listener
        .local_addr()
        .map_err(|e| format!("cannot read the gateway endpoint's address: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("cannot prepare the gateway endpoint {listen}: {e}"))?;
    let registry = Arc::new(Registry::new());
    let handler = Arc::new(Forwarding {
        pool: Pool::new(config.clone(), registry.clone()),
        registry,
        max_agents: gateway.max_carried_agents,
    });
    // The receive and send limits the Baseline requires, enforced per hop.
    let app = opamp::server::router(handler, Settings::new(config.max_message_size_bytes));

    match &gateway.tls {
        Some(tls) => serve_tls(app, listener, tls, gateway.upstream_connections, shutdown).await,
        None => serve_plain(app, listener, gateway.upstream_connections, shutdown).await,
    }
}

/// The plaintext downstream endpoint. Documented for a bootstrapping fleet, but the hop then carries
/// the `Authorization` credential in the clear, so say so loudly.
async fn serve_plain(
    app: Router,
    listener: std::net::TcpListener,
    upstream_cap: usize,
    mut shutdown: Shutdown,
) -> Result<(), String> {
    let listen = listener
        .local_addr()
        .map_err(|e| format!("cannot read the gateway endpoint's address: {e}"))?;
    let listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|e| format!("cannot prepare the gateway endpoint {listen}: {e}"))?;
    warn!(
        %listen,
        "the gateway endpoint is serving plaintext — configure [gateway.tls] to encrypt the \
         downstream hop and gate it with a client CA"
    );
    info!(%listen, upstream_cap, "gateway listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.requested().await;
    })
    .await
    .map_err(|e| format!("the gateway endpoint stopped: {e}"))
}

/// The TLS downstream endpoint (ADR-0009): the same server-side rustls terminator the Server uses,
/// with the handshake proving the downstream Agent against `client_ca_file` when one is set.
async fn serve_tls(
    app: Router,
    listener: std::net::TcpListener,
    tls: &GatewayTlsConfig,
    upstream_cap: usize,
    mut shutdown: Shutdown,
) -> Result<(), String> {
    let server_config = tls_server_config(tls)?;
    let handle = Handle::new();
    // axum_server drains rather than drops: on shutdown the handle lets in-flight exchanges finish
    // (up to the grace) instead of tearing every downstream connection down mid-message.
    let trigger = handle.clone();
    tokio::spawn(async move {
        shutdown.requested().await;
        trigger.graceful_shutdown(Some(DRAIN_GRACE));
    });

    let mutual = tls.client_ca_file.is_some();
    let listen = listener
        .local_addr()
        .map_err(|e| format!("cannot read the gateway endpoint's address: {e}"))?;
    info!(%listen, upstream_cap, mutual_tls = mutual, "gateway listening over TLS");
    axum_server::from_tcp_rustls(listener, RustlsConfig::from_config(server_config))
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .map_err(|e| format!("the gateway endpoint stopped: {e}"))
}

/// Builds the rustls configuration the downstream endpoint serves with. A configured
/// `client_ca_file` turns on mutual TLS and — unlike the Server, whose one port also answers
/// browsers (ADR-0011) — makes a client certificate **mandatory**: this endpoint speaks only OpAMP,
/// so a configured CA is an access-control boundary, not a hint. Its absence keeps the hop
/// server-authenticated only, which a bootstrapping fleet uses.
fn tls_server_config(tls: &GatewayTlsConfig) -> Result<Arc<ServerConfig>, String> {
    let certs = crate::tls::read_certs(&tls.cert_file)?;
    let key = crate::tls::read_key(&tls.key_file)?;

    let builder = match &tls.client_ca_file {
        None => ServerConfig::builder().with_no_client_auth(),
        Some(ca_file) => {
            let roots = crate::tls::root_store(ca_file)?;
            let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .map_err(|e| format!("cannot build the downstream client verifier: {e}"))?;
            ServerConfig::builder().with_client_cert_verifier(verifier)
        }
    };

    let mut config = builder
        .with_single_cert(certs, key)
        .map_err(|e| format!("cannot use the gateway TLS certificate and key: {e}"))?;
    // `RustlsConfig::from_config` leaves ALPN to the caller; without it an HTTP/2 client fails the
    // negotiation. Matches the Server's listener (ADR-0011).
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// One downstream connection: a socket, or one plain-HTTP exchange.
struct Downstream {
    transport: Transport,
    peer: String,
    /// Forwarded upstream with every report — the Gateway makes no authentication decision of its
    /// own (ADR-0009).
    authorization: Option<String>,
    /// Where the registry routes this socket's replies; `None` on plain HTTP, whose one reply
    /// comes back through a `oneshot` instead.
    replies: Option<mpsc::Sender<ServerToAgent>>,
    /// Every Agent this peer turned out to carry, so all of them are released when it goes. A set,
    /// not a list: membership is checked per report, and the cap bounds how large it grows.
    carried: HashSet<InstanceUid>,
}

/// What the Server says about the Agents a socket carries, routed back to it by the registry.
struct Replies(mpsc::Receiver<ServerToAgent>);

impl Outbound for Replies {
    type Item = ServerToAgent;

    async fn next(&mut self) -> Option<ServerToAgent> {
        self.0.recv().await
    }
}

impl Handler for Forwarding {
    type Connection = Downstream;
    type Outbound = Replies;

    fn on_connecting(
        &self,
        request: &RequestInfo<'_>,
    ) -> Result<(Downstream, Option<Replies>), Rejection> {
        let authorization = request
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let peer = request
            .peer
            .map_or_else(|| "unknown".to_string(), |peer| peer.to_string());
        let (replies, outbound) = match request.transport {
            Transport::WebSocket => {
                debug!(%peer, "a downstream client connected");
                let (tx, rx) = mpsc::channel::<ServerToAgent>(64);
                (Some(tx), Some(Replies(rx)))
            }
            Transport::Http => (None, None),
        };
        Ok((
            Downstream {
                transport: request.transport,
                peer,
                authorization,
                replies,
                carried: HashSet::new(),
            },
            outbound,
        ))
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

    /// An unreadable report is dropped, never answered: the Gateway invents nothing (ADR-0009). A
    /// plain-HTTP exchange still needs a response, and it says why.
    fn on_unreadable(&self, downstream: &mut Downstream, error: &Unreadable) -> Reply {
        warn!(peer = %downstream.peer, %error, "dropping an unreadable downstream report");
        match downstream.transport {
            Transport::WebSocket => Reply::Nothing,
            Transport::Http => Reply::Refuse(StatusCode::BAD_REQUEST, "unreadable report".into()),
        }
    }

    /// The peer is gone. Its Agents stop being routable here — and nothing is said upstream on
    /// their behalf, because they said nothing (ADR-0009 rule 10).
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
            self.registry.attach(uid, replies.clone());
        }
        if let Err(e) = self
            .pool
            .forward(uid, &report, downstream.authorization.as_deref())
            .await
        {
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
        self.registry.expect_once(uid, reply_tx);
        if let Err(e) = self
            .pool
            .forward(uid, &report, downstream.authorization.as_deref())
            .await
        {
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
