//! Gateway Mode (ADR-0003, ADR-0037): the Client at a network boundary.
//!
//! It is an OpAMP **server** downstream and an OpAMP **client** upstream, and it folds many
//! downstream connections onto a small pool of upstream ones. What it does *not* do is as
//! load-bearing as what it does: it forwards messages unchanged, makes no authentication decision,
//! and never speaks in an Agent's name — not even to say the goodbye a vanished Agent did not send.
//!
//! Three pieces: this module serves the downstream endpoint on both transports (a downstream Client
//! picks its transport by URL scheme, so serving only one would silently exclude half of them),
//! [`pool`] holds the upstream connections, and [`registry`] routes replies back by `instance_uid`.

pub mod pool;
pub mod registry;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{
    close_code, CloseFrame, Message as AxumMessage, WebSocket, WebSocketUpgrade,
};
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;
use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::uid::InstanceUid;
use prost::Message as _;
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::config::{ClientConfig, GatewayTlsConfig};
use crate::service::runtime::Shutdown;
use pool::Pool;
use registry::Registry;

/// How long the downstream endpoint has to drain in-flight exchanges once shutdown is requested,
/// before connections are dropped. One [`EXCHANGE_TIMEOUT`] plus a little, so an exchange already
/// waiting on the Server is not cut short by the stop it did not cause.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(35);

use opamp::endpoint::{OPAMP_PATH, PROTOBUF_CONTENT_TYPE};

/// How long a plain-HTTP peer waits for its reply to come back through the pool. Beyond this the
/// exchange fails and the peer retries, which is what its transport already does on any error.
const EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

struct Gateway {
    pool: Pool,
    registry: Arc<Registry>,
    limit: usize,
    /// The most distinct Agents one downstream connection may carry (ADR-0037): past it a report
    /// for a *new* Agent is dropped, so a single peer cannot grow the routing state without bound.
    max_agents: usize,
}

/// Serves the downstream endpoint until `shutdown` fires.
///
/// Mutual TLS is per hop (ADR-0035, ADR-0037): with a `[gateway.tls]` section the downstream hop is
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
    let registry = Arc::new(Registry::new());
    let state = Arc::new(Gateway {
        pool: Pool::new(config.clone(), registry.clone()),
        registry,
        limit: config.max_message_size_bytes,
        max_agents: gateway.max_carried_agents,
    });

    let app = Router::new()
        .route(OPAMP_PATH, get(upgrade).post(exchange))
        // The receive limit the Baseline requires, enforced per hop.
        .layer(DefaultBodyLimit::max(config.max_message_size_bytes))
        .with_state(state);

    match &gateway.tls {
        Some(tls) => serve_tls(app, listen, tls, gateway.upstream_connections, shutdown).await,
        None => serve_plain(app, listen, gateway.upstream_connections, shutdown).await,
    }
}

/// The plaintext downstream endpoint. Documented for a bootstrapping fleet, but the hop then carries
/// the `Authorization` credential in the clear, so say so loudly.
async fn serve_plain(
    app: Router,
    listen: SocketAddr,
    upstream_cap: usize,
    mut shutdown: Shutdown,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("cannot bind the gateway endpoint {listen}: {e}"))?;
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

/// The TLS downstream endpoint (ADR-0037): the same server-side rustls terminator the Server uses,
/// with the handshake proving the downstream Agent against `client_ca_file` when one is set.
async fn serve_tls(
    app: Router,
    listen: SocketAddr,
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
    info!(%listen, upstream_cap, mutual_tls = mutual, "gateway listening over TLS");
    axum_server::bind_rustls(listen, RustlsConfig::from_config(server_config))
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .map_err(|e| format!("the gateway endpoint stopped: {e}"))
}

/// Builds the rustls configuration the downstream endpoint serves with. A configured
/// `client_ca_file` turns on mutual TLS and — unlike the Server, whose one port also answers
/// browsers (ADR-0005) — makes a client certificate **mandatory**: this endpoint speaks only OpAMP,
/// so a configured CA is an access-control boundary, not a hint. Its absence keeps the hop
/// server-authenticated only, which a bootstrapping fleet uses.
fn tls_server_config(tls: &GatewayTlsConfig) -> Result<Arc<ServerConfig>, String> {
    let certs = opamp::pem::certificates(&read(&tls.cert_file)?)
        .map_err(|e| format!("cannot parse {}: {e}", tls.cert_file.display()))?;
    let key = opamp::pem::private_key(&read(&tls.key_file)?)
        .map_err(|_| format!("{} contains no private key", tls.key_file.display()))?;

    let builder = match &tls.client_ca_file {
        None => ServerConfig::builder().with_no_client_auth(),
        Some(ca_file) => {
            let mut roots = RootCertStore::empty();
            for cert in opamp::pem::certificates(&read(ca_file)?)
                .map_err(|e| format!("cannot parse {}: {e}", ca_file.display()))?
            {
                roots.add(cert).map_err(|e| {
                    format!("cannot trust a certificate from {}: {e}", ca_file.display())
                })?;
            }
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
    // negotiation. Matches the Server's listener (ADR-0044).
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn read(path: &std::path::Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// The WebSocket half of the downstream endpoint.
async fn upgrade(
    State(state): State<Arc<Gateway>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    upgrade
        .max_message_size(state.limit)
        .on_upgrade(move |socket| serve_socket(socket, state, peer, authorization))
}

/// One downstream WebSocket peer: everything it sends goes upstream, everything the Server says
/// about the Agents it carries comes back down here.
async fn serve_socket(
    mut socket: WebSocket,
    state: Arc<Gateway>,
    peer: SocketAddr,
    authorization: Option<String>,
) {
    debug!(%peer, "a downstream client connected");
    let (replies_tx, mut replies_rx) = mpsc::channel::<ServerToAgent>(64);
    // Every Agent this peer turned out to carry, so all of them are released when it goes. A set,
    // not a list: membership is checked per report, and the cap below bounds how large it grows.
    let mut carried: std::collections::HashSet<InstanceUid> = std::collections::HashSet::new();

    loop {
        tokio::select! {
            reply = replies_rx.recv() => {
                let Some(reply) = reply else { break };
                match opamp::frame::encode_within(&reply, state.limit) {
                    Ok(frame) => {
                        if socket.send(AxumMessage::Binary(frame.into())).await.is_err() {
                            break;
                        }
                    }
                    // The Baseline's rule for an oversized message: never truncate, never ship.
                    Err(e) => warn!(error = %e, "dropping an oversized Server message"),
                }
            }
            incoming = socket.recv() => {
                let payload = match incoming {
                    Some(Ok(AxumMessage::Binary(payload))) => payload,
                    Some(Ok(AxumMessage::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        // Past the limit a message never becomes a frame, so it surfaces here. The
                        // Baseline's answer is the 1009 close, and a Gateway owes it downstream for
                        // the same reason the Server owes it upstream — it *is* an OpAMP server to
                        // the Agents behind it (ADR-0037). It used to hang up without a status.
                        debug!(%peer, error = %e, "a downstream connection failed");
                        let _ = socket.send(too_big_close()).await;
                        break;
                    }
                };
                let report = match opamp::frame::decode::<AgentToServer>(&payload, state.limit) {
                    Ok(report) => report,
                    // Oversized is malformed, and the Baseline answers it with the close rather
                    // than by dropping the message and reading on.
                    Err(e @ opamp::frame::FrameError::TooLarge(..)) => {
                        warn!(%peer, error = %e, "closing a downstream connection: oversized message");
                        let _ = socket.send(too_big_close()).await;
                        break;
                    }
                    Err(e) => {
                        warn!(%peer, error = %e, "dropping an unreadable downstream report");
                        continue;
                    }
                };
                let Some(uid) = InstanceUid::from_wire(&report.instance_uid) else {
                    warn!(%peer, "dropping a downstream report with a malformed instance_uid");
                    continue;
                };
                if !carried.contains(&uid) {
                    // Bound the routing state one connection can create: past the cap a report for
                    // a new Agent is dropped, while the Agents already carried keep being served.
                    if carried.len() >= state.max_agents {
                        warn!(
                            %peer, agent = %uid, cap = state.max_agents,
                            "a downstream connection reached its Agent cap; dropping a report for a new Agent"
                        );
                        continue;
                    }
                    carried.insert(uid);
                    info!(agent = %uid, %peer, "carrying an Agent");
                }
                state.registry.attach(uid, replies_tx.clone());
                if let Err(e) = state
                    .pool
                    .forward(uid, &report, authorization.as_deref())
                    .await
                {
                    warn!(agent = %uid, error = %e, "cannot forward a report upstream");
                }
            }
        }
    }

    // The peer is gone. Its Agents stop being routable here — and nothing is said upstream on
    // their behalf, because they said nothing (ADR-0037 rule 7).
    let count = carried.len();
    state.registry.detach_all(carried);
    debug!(%peer, agents = count, "a downstream client disconnected");
}

/// The plain-HTTP half: one report in, one reply out, with the pool in between.
async fn exchange(
    State(state): State<Arc<Gateway>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with(PROTOBUF_CONTENT_TYPE))
    {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/x-protobuf",
        )
            .into_response();
    }
    // A downstream Client may gzip its report — the Baseline says a server MUST accept it — and the
    // size limit applies after decompression, so a small bomb buys no more memory than a large
    // plain body would. This endpoint implemented neither until ADR-0044 put both in one place with
    // the Server's; a Gateway that refused what the Server accepts would break the hop for exactly
    // the Agents that compress.
    let raw = match opamp::endpoint::decode_body(
        &body,
        headers
            .get(header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default(),
        state.limit,
    ) {
        Ok(raw) => raw,
        Err(e @ opamp::endpoint::BodyError::TooLarge) => {
            warn!(%peer, limit = state.limit, "dropping an oversized downstream report");
            return (StatusCode::PAYLOAD_TOO_LARGE, e.to_string()).into_response();
        }
        Err(e @ opamp::endpoint::BodyError::UndecodableGzip) => {
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
        Err(e @ opamp::endpoint::BodyError::UnsupportedEncoding(_)) => {
            return (StatusCode::UNSUPPORTED_MEDIA_TYPE, e.to_string()).into_response();
        }
    };
    // Plain HTTP carries the bare protobuf, not the varint-framed message the WebSocket transport
    // uses — the same split the Server makes, and the reason the two halves of this endpoint do not
    // share a codec.
    let report = match AgentToServer::decode(&raw[..]) {
        Ok(report) => report,
        Err(e) => {
            warn!(%peer, error = %e, "dropping an unreadable downstream report");
            return (StatusCode::BAD_REQUEST, "unreadable report").into_response();
        }
    };
    let Some(uid) = InstanceUid::from_wire(&report.instance_uid) else {
        return (StatusCode::BAD_REQUEST, "instance_uid must be 16 bytes").into_response();
    };

    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (reply_tx, reply_rx) = oneshot::channel();
    state.registry.expect_once(uid, reply_tx);
    if let Err(e) = state
        .pool
        .forward(uid, &report, authorization.as_deref())
        .await
    {
        warn!(agent = %uid, error = %e, "cannot forward a report upstream");
        return (StatusCode::BAD_GATEWAY, "cannot reach the Server").into_response();
    }

    match tokio::time::timeout(EXCHANGE_TIMEOUT, reply_rx).await {
        Ok(Ok(reply)) => {
            let encoded = reply.encode_to_vec();
            // The send side of the Baseline's limit, enforced on this hop: an oversized reply is
            // never truncated and never shipped.
            if encoded.len() > state.limit {
                warn!(size = encoded.len(), "discarding an oversized Server reply");
                return (StatusCode::INTERNAL_SERVER_ERROR, "oversized reply").into_response();
            }
            ([(header::CONTENT_TYPE, PROTOBUF_CONTENT_TYPE)], encoded).into_response()
        }
        _ => {
            debug!(agent = %uid, "no reply came back for a forwarded report");
            (StatusCode::GATEWAY_TIMEOUT, "no reply from the Server").into_response()
        }
    }
}

/// The close the Baseline names for a message past the size limit: 1009, Message Too Big.
///
/// axum's spelling of it; the Client's other two sockets speak tungstenite and have their own in
/// `transport`. The sentence is shared by all of them (ADR-0044).
fn too_big_close() -> AxumMessage {
    AxumMessage::Close(Some(CloseFrame {
        code: close_code::SIZE,
        reason: opamp::frame::TOO_BIG_CLOSE_REASON.into(),
    }))
}
