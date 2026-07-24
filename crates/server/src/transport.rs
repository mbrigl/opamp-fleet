//! The OpAMP endpoint — both transports on one path (ADR-0007).
//!
//! `/v1/opamp` serves the whole protocol: a request carrying the protobuf `Content-Type` is the
//! plain-HTTP transport, a WebSocket upgrade is the other — exactly the detection the Baseline
//! describes. Both hand every decoded report to the same [`AppState::process`], so transport is
//! carriage, never semantics.

use std::io::Read;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use opamp::frame::{self, MAX_MESSAGE_SIZE};
use opamp::proto::AgentToServer;
use opamp::uid::InstanceUid;
use prost::Message as _;
use tracing::{debug, warn};

use crate::fleet::{bad_request, AppState, Transport};

/// The endpoint path the Baseline names as the default.
pub const OPAMP_PATH: &str = "/v1/opamp";

/// The protobuf media type the Baseline requires on the plain-HTTP transport.
const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        // One path, both transports — split exactly as the Baseline describes: a WebSocket
        // upgrade (a GET) starts the WebSocket transport, a POST carrying the protobuf
        // Content-Type is one plain-HTTP exchange.
        .route(OPAMP_PATH, get(upgrade).post(post_exchange))
        .layer(DefaultBodyLimit::max(MAX_MESSAGE_SIZE))
        .with_state(state)
}

async fn upgrade(State(state): State<Arc<AppState>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade
        .max_message_size(MAX_MESSAGE_SIZE)
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
    if !content_type.starts_with(PROTOBUF_CONTENT_TYPE) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!(
                "the OpAMP plain-HTTP transport requires Content-Type: {PROTOBUF_CONTENT_TYPE}"
            ),
        )
            .into_response();
    }

    let raw = match header_str(headers, header::CONTENT_ENCODING) {
        "" | "identity" => body.to_vec(),
        "gzip" => {
            let mut decoded = Vec::new();
            // Cap the *decompressed* size too: a tiny gzip bomb must not bypass the body limit.
            let mut reader =
                flate2::read::GzDecoder::new(&body[..]).take(MAX_MESSAGE_SIZE as u64 + 1);
            if reader.read_to_end(&mut decoded).is_err() || decoded.len() > MAX_MESSAGE_SIZE {
                return (StatusCode::BAD_REQUEST, "invalid or oversized gzip body").into_response();
            }
            decoded
        }
        other => {
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!("unsupported Content-Encoding: {other}"),
            )
                .into_response();
        }
    };

    let reply = match AgentToServer::decode(raw.as_slice()) {
        Ok(msg) => state.process(msg, Transport::Http).reply,
        Err(e) => {
            warn!(error = %e, "undecodable report on the plain-HTTP transport");
            bad_request("the request body is not a valid AgentToServer message")
        }
    };
    (
        [(header::CONTENT_TYPE, PROTOBUF_CONTENT_TYPE)],
        reply.encode_to_vec(),
    )
        .into_response()
}

/// One WebSocket connection: any number of Agents, told apart by `instance_uid` alone (ADR-0003).
/// The loop also watches the desired-config channel, so a change reaches connected Agents without
/// waiting for them to speak — the "within seconds" of the control loop.
async fn serve_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mut seen: Vec<InstanceUid> = Vec::new();
    let mut push = state.subscribe();

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break };
                match message {
                    Message::Binary(data) => {
                        let reply = match frame::decode::<AgentToServer>(&data) {
                            Ok(msg) => {
                                let outcome = state.process(msg, Transport::WebSocket);
                                if let Some(uid) = outcome.uid {
                                    if outcome.disconnected {
                                        seen.retain(|s| s != &uid);
                                    } else if !seen.contains(&uid) {
                                        seen.push(uid);
                                    }
                                }
                                outcome.reply
                            }
                            Err(e) => {
                                warn!(error = %e, "undecodable frame on the WebSocket transport");
                                bad_request("the frame is not a valid OpAMP message")
                            }
                        };
                        if socket
                            .send(Message::Binary(frame::encode(&reply).into()))
                            .await
                            .is_err()
                        {
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
                        if socket
                            .send(Message::Binary(frame::encode(&command).into()))
                            .await
                            .is_err()
                        {
                            state.mark_disconnected(&seen);
                            return;
                        }
                    }
                    if let Some(offer) = state.offer_for(uid) {
                        debug!(agent = %uid, "pushing a configuration offer");
                        if socket
                            .send(Message::Binary(frame::encode(&offer).into()))
                            .await
                            .is_err()
                        {
                            state.mark_disconnected(&seen);
                            return;
                        }
                    }
                }
            }
        }
    }

    // The connection is gone; every Agent it carried is unreachable until it reports again.
    state.mark_disconnected(&seen);
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}
