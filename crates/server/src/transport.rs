//! The OpAMP endpoint — both transports on one path (ADR-0012).
//!
//! `/v1/opamp` serves the whole protocol: a request carrying the protobuf `Content-Type` is the
//! plain-HTTP transport, a WebSocket upgrade is the other — exactly the detection the Baseline
//! describes. Both hand every decoded report to the same [`AppState::process`], so transport is
//! carriage, never semantics.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use opamp::frame;
use opamp::proto::AgentToServer;
use opamp::uid::InstanceUid;
use prost::Message as _;
use tracing::{debug, warn};

use crate::fleet::{bad_request, AppState, Transport};

/// The endpoint path the Baseline names as the default, and the protobuf media type it requires —
/// both from the shared crate, because the Gateway serves the same endpoint (ADR-0011).
pub use opamp::endpoint::{OPAMP_PATH, PROTOBUF_CONTENT_TYPE};

    // The receive limit the Baseline requires of the Server on both transports; a request body
    // past it never reaches a handler, and axum answers it with the 413 the Baseline prescribes.
    let limit = state.max_message_size();
        // One path, both transports — split exactly as the Baseline describes: a WebSocket
        // upgrade (a GET) starts the WebSocket transport, a POST carrying the protobuf
        // Content-Type is one plain-HTTP exchange.
        .route(OPAMP_PATH, get(upgrade).post(post_exchange))
        .layer(DefaultBodyLimit::max(limit))
}

async fn upgrade(State(state): State<Arc<AppState>>, upgrade: WebSocketUpgrade) -> Response {
    let limit = state.max_message_size();
    upgrade
        // The transport's own guard, so an oversized frame is refused before it is buffered
        // whole; `serve_socket` still checks, because that is what turns the refusal into the
        // 1009 close the Baseline asks for. The per-frame cap moves with it: left at its default
        // it would refuse messages *below* the configured limit, which is the limit's business.
        .max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| serve_socket(socket, state))
}

async fn post_exchange(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    plain_http(&state, &headers, body)
}

/// One plain-HTTP exchange: protobuf `AgentToServer` in (gzip accepted — a Baseline MUST),
/// protobuf `ServerToAgent` out.
fn plain_http(state: &AppState, headers: &HeaderMap, body: Bytes) -> Response {
    let content_type = header_str(headers, header::CONTENT_TYPE);
    if !opamp::endpoint::is_protobuf(content_type) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!(
                "the OpAMP plain-HTTP transport requires Content-Type: {PROTOBUF_CONTENT_TYPE}"
            ),
        )
            .into_response();
    }

    let limit = state.max_message_size();
    // Accepting gzip is a Baseline MUST, and the limit applying *after* decompression is the other
    // half of it. Both live in `opamp::endpoint` (ADR-0011), so the Gateway's endpoint follows the
    // same rule instead of a reading of its own; what stays here is the status code, which is this
    // transport's decision.
    let raw = match opamp::endpoint::decode_body(
        &body,
        header_str(headers, header::CONTENT_ENCODING),
        limit,
    ) {
        Ok(raw) => raw,
        Err(e @ opamp::endpoint::BodyError::TooLarge) => {
            warn!(
                limit,
                "rejecting a request body that decompresses past the limit"
            );
            return (StatusCode::PAYLOAD_TOO_LARGE, e.to_string()).into_response();
        }
        Err(e @ opamp::endpoint::BodyError::UndecodableGzip) => {
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
        Err(e @ opamp::endpoint::BodyError::UnsupportedEncoding(_)) => {
            return (StatusCode::UNSUPPORTED_MEDIA_TYPE, e.to_string()).into_response();
        }
    };

    let reply = match AgentToServer::decode(raw.as_slice()) {
        // Plain HTTP is stateless polling — there is no connection identity to pass.
        Ok(msg) => state.process(msg, Transport::Http, None).reply,
        Err(e) => {
            warn!(error = %e, "undecodable report on the plain-HTTP transport");
            bad_request("the request body is not a valid AgentToServer message")
        }
    };
    // The send side of the same limit: the Baseline forbids putting an oversized response on the
    // wire, so a reply that outgrew it is discarded — recorded here rather than shipped — and the
    // Client sees a failed exchange instead of a body it must refuse.
    let encoded = reply.encode_to_vec();
    if encoded.len() > limit {
        warn!(
            size = encoded.len(),
            limit, "discarding a response that exceeds the message size limit"
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the response exceeds the message size limit",
        )
            .into_response();
    }
    ([(header::CONTENT_TYPE, PROTOBUF_CONTENT_TYPE)], encoded).into_response()
}

/// One WebSocket connection: any number of Agents, told apart by `instance_uid` alone (ADR-0009).
/// The loop also watches the desired-config channel, so a change reaches connected Agents without
/// waiting for them to speak — the "within seconds" of the control loop.
async fn serve_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mut seen: Vec<InstanceUid> = Vec::new();
    let mut push = state.subscribe();
    // This connection's identity — what the duplicate detection tells connections apart by.
    let conn = state.connection_id();
    let limit = state.max_message_size();

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let message = match incoming {
                    Some(Ok(message)) => message,
                    // The upgrade capped what this socket will buffer, so a peer past the limit
                    // surfaces here as a receive error rather than as a frame. Answer it the way
                    // the Baseline asks — close with 1009 — and let the send fail harmlessly when
                    // the connection is simply gone or already closing.
                    Some(Err(e)) => {
                        warn!(error = %e, "closing the WebSocket after a receive error");
                        let _ = socket.send(too_big_close()).await;
                        break;
                    }
                    None => break,
                };
                match message {
                    Message::Binary(data) => {
                        let reply = match frame::decode::<AgentToServer>(&data, limit) {
                            Ok(msg) => {
                                let outcome = state.process(msg, Transport::WebSocket, Some(conn));
                                if let Some(uid) = outcome.uid {
                                    if outcome.disconnected {
                                        seen.retain(|s| s != &uid);
                                    } else if !seen.contains(&uid) {
                                        seen.push(uid);
                                    }
                                }
                                outcome.reply
                            }
                            // An oversized frame is malformed, and the Baseline's answer to it is
                            // not an error message but the 1009 close.
                            Err(e @ frame::FrameError::TooLarge(..)) => {
                                warn!(error = %e, "closing the WebSocket: the peer sent an oversized message");
                                let _ = socket.send(too_big_close()).await;
                                break;
                            }
                            Err(e) => {
                                warn!(error = %e, "undecodable frame on the WebSocket transport");
                                bad_request("the frame is not a valid OpAMP message")
                            }
                        };
                        if !send_framed(&mut socket, &reply, limit, "reply").await {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    // axum answers pings itself; pongs and text need nothing from us.
                    _ => {}
                }
            }
            changed = push.changed() => {
                if changed.is_err() {
                    break;
                }
                for uid in &seen {
                    // A queued restart goes first, as its own frame — the Baseline's command
                    // message is never combined with an offer.
                    if let Some(command) = state.restart_command_for(uid) {
                        debug!(agent = %uid, "pushing a restart command");
                        if !send_framed(&mut socket, &command, limit, "restart command").await {
                            state.mark_disconnected(&seen, conn);
                            return;
                        }
                    }
                    if let Some(offer) = state.offer_for(uid) {
                        debug!(agent = %uid, "pushing a configuration offer");
                        if !send_framed(&mut socket, &offer, limit, "configuration offer").await {
                            state.mark_disconnected(&seen, conn);
                            return;
                        }
                    }
                }
            }
        }
    }

    // The connection is gone; every Agent it carried is unreachable until it reports again.
    state.mark_disconnected(&seen, conn);
}

/// Sends one framed message under the size limit. A message that would exceed it is **not** sent —
/// the Baseline's MUST for the Server's outbound direction — but discarded with a log line, since
/// the fault is on this end and the connection is fine. Returns `false` only when the connection is
/// gone, so a caller can tell "dropped one message" from "lost the Agent".
async fn send_framed<M: prost::Message>(
    socket: &mut WebSocket,
    msg: &M,
    limit: usize,
    what: &str,
) -> bool {
    match frame::encode_within(msg, limit) {
        Ok(framed) => socket.send(Message::Binary(framed.into())).await.is_ok(),
        Err(e) => {
            warn!(error = %e, message = what, "discarding a message that exceeds the size limit");
            true
        }
    }
}

/// The close the Baseline names for a message past the size limit: 1009, Message Too Big.
fn too_big_close() -> Message {
    Message::Close(Some(CloseFrame {
        code: close_code::SIZE,
        reason: frame::TOO_BIG_CLOSE_REASON.into(),
    }))
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}
