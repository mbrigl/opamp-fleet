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
use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::uid::InstanceUid;
use prost::Message as _;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::config::ClientConfig;
use crate::service::runtime::Shutdown;
use pool::Pool;
use registry::Registry;

/// The endpoint path the Baseline names as the default — the same one the Server serves.
const OPAMP_PATH: &str = "/v1/opamp";

/// The protobuf media type the Baseline requires on the plain-HTTP transport.
const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";

/// How long a plain-HTTP peer waits for its reply to come back through the pool. Beyond this the
/// exchange fails and the peer retries, which is what its transport already does on any error.
const EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

struct Gateway {
    pool: Pool,
    registry: Arc<Registry>,
    limit: usize,
}

/// Serves the downstream endpoint until `shutdown` fires.
///
/// # Errors
/// Returns an error when the configured address cannot be bound — at startup, so a taken port is
/// loud rather than a Gateway that quietly carries nobody.
pub async fn run(config: Arc<ClientConfig>, mut shutdown: Shutdown) -> Result<(), String> {
    let Some(gateway) = &config.gateway else {
        return Ok(());
    };
    let listen = gateway.listen;
    let registry = Arc::new(Registry::new());
    let state = Arc::new(Gateway {
        pool: Pool::new(config.clone(), registry.clone()),
        registry,
        limit: config.max_message_size_bytes,
    });

    let app = Router::new()
        .route(OPAMP_PATH, get(upgrade).post(exchange))
        // The receive limit the Baseline requires, enforced per hop.
        .layer(DefaultBodyLimit::max(config.max_message_size_bytes))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("cannot bind the gateway endpoint {listen}: {e}"))?;
    info!(listen = %listen, upstream_cap = gateway.upstream_connections, "gateway listening");

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
    // Every Agent this peer turned out to carry, so all of them are released when it goes.
    let mut carried: Vec<InstanceUid> = Vec::new();

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
                        debug!(%peer, error = %e, "a downstream connection failed");
                        break;
                    }
                };
                let report = match opamp::frame::decode::<AgentToServer>(&payload, state.limit) {
                    Ok(report) => report,
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
                    carried.push(uid);
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
    state.registry.detach_all(&carried);
    debug!(%peer, agents = carried.len(), "a downstream client disconnected");
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
    // Plain HTTP carries the bare protobuf, not the varint-framed message the WebSocket transport
    // uses — the same split the Server makes, and the reason the two halves of this endpoint do not
    // share a codec.
    let report = match AgentToServer::decode(&body[..]) {
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
