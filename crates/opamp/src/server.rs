//! One OpAMP server endpoint, for every surface that speaks the Server side of the protocol
//! (ADR-0008). Behind the `server` feature.
//!
//! This crate carries the communication and nothing else: it tells a WebSocket upgrade from a
//! plain-HTTP exchange as the specification describes, applies the media type, gzip and the
//! receive limit on both transports, frames WebSocket messages, closes with 1009 on an oversized
//! one, and never puts an oversized reply on the wire. What a message *means* is the
//! application's: it implements [`Handler`], and the endpoint calls it.
//!
//! [`router`] hands out an axum [`Router`], so a surface can add its admission layers and
//! neighbouring routes. [`listen`] serves the result, with its TLS and its bounds on connection
//! setup.

pub mod listen;
pub mod pace;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::endpoint::{BodyError, OPAMP_PATH, PROTOBUF_CONTENT_TYPE};
use crate::frame::{self, FrameError};
use crate::proto::{AgentToServer, ServerToAgent};
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{header, Extensions, HeaderMap, StatusCode, Version};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, MethodRouter};
use axum::Router;
use futures_util::{SinkExt as _, StreamExt as _};
use prost::Message as _;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::warn;

/// The transport one connection arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    WebSocket,
    /// One plain-HTTP exchange: a request carrying one report, a response carrying one reply.
    Http,
}

/// Which transports an endpoint serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transports {
    Both,
    WebSocketOnly,
}

/// How an endpoint is served.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// The largest message accepted or sent, on both transports — after decompression on plain
    /// HTTP, framing header included on a WebSocket.
    pub max_message_size: usize,
    pub transports: Transports,
    /// Answer on any path instead of the specification's `/v1/opamp` only — for an endpoint that
    /// serves exactly one local client and has nothing to route by.
    pub any_path: bool,
}

impl Settings {
    /// Both transports on `/v1/opamp`, with the given limit.
    #[must_use]
    pub fn new(max_message_size: usize) -> Self {
        Settings {
            max_message_size,
            transports: Transports::Both,
            any_path: false,
        }
    }
}

/// What a handler sees of a new connection, to accept or refuse it by.
pub struct RequestInfo<'a> {
    pub transport: Transport,
    pub headers: &'a HeaderMap,
    /// The request's extensions — where a TLS acceptor puts the peer certificate.
    pub extensions: &'a Extensions,
    /// The peer, when the listener was served with connect info.
    pub peer: Option<SocketAddr>,
}

/// What the endpoint does with one message the handler was given.
// Returned once per message and moved straight to the wire, never stored: boxing the message would
// cost an allocation per reply to save stack space nobody holds on to.
#[allow(clippy::large_enum_variant)]
pub enum Reply {
    /// Send this message.
    Send(ServerToAgent),
    /// Send nothing. On plain HTTP the response then carries an empty `ServerToAgent`, since an
    /// exchange always has a response.
    Nothing,
    /// Refuse the exchange: on plain HTTP this status and text are the response; on a WebSocket
    /// nothing is sent and the connection stays open.
    Refuse(StatusCode, String),
}

/// A refused connection: the status, any headers to send with it (`WWW-Authenticate`,
/// `Retry-After`), and a sentence saying why.
pub struct Rejection {
    pub status: StatusCode,
    pub headers: Vec<(header::HeaderName, header::HeaderValue)>,
    pub message: String,
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        let mut response = (self.status, self.message).into_response();
        response.headers_mut().extend(self.headers);
        response
    }
}

/// Why a message could not be read — handed to [`Handler::on_unreadable`].
#[derive(Debug)]
pub enum Unreadable {
    /// A WebSocket frame that is not a framed OpAMP message.
    Frame(FrameError),
    /// A plain-HTTP body that is not an `AgentToServer`.
    Body(prost::DecodeError),
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unreadable::Frame(e) => e.fmt(f),
            Unreadable::Body(e) => e.fmt(f),
        }
    }
}

/// What a connection is sent without being asked: it waits for the next occasion and yields it,
/// and [`Handler::on_outbound`] turns that into messages. `None` ends the connection.
pub trait Outbound: Send + 'static {
    type Item: Send;

    fn next(&mut self) -> impl Future<Output = Option<Self::Item>> + Send;
}

/// An endpoint whose connections are sent nothing unasked.
pub struct NoOutbound;

impl Outbound for NoOutbound {
    type Item = std::convert::Infallible;

    async fn next(&mut self) -> Option<Self::Item> {
        std::future::pending().await
    }
}

/// The application's half of an endpoint. The endpoint never calls two of these concurrently for
/// the same connection; it may for different connections. A WebSocket's messages are handed over
/// one at a time in the order they arrived, while the socket is read on and its pings answered.
pub trait Handler: Send + Sync + 'static {
    /// The application's own state for one connection — one plain-HTTP exchange, or one socket.
    type Connection: Send + 'static;
    /// What a WebSocket connection is sent unasked.
    type Outbound: Outbound;

    /// Accept a new connection, or refuse it with the response to send. The outbound side is
    /// asked for only on a WebSocket; return `None` for one that is sent nothing unasked.
    ///
    /// # Errors
    /// The refusal to answer with.
    fn on_connecting(
        &self,
        request: &RequestInfo<'_>,
    ) -> Result<(Self::Connection, Option<Self::Outbound>), Rejection>;

    /// One decoded message, and what to reply.
    fn on_message(
        &self,
        connection: &mut Self::Connection,
        message: AgentToServer,
    ) -> impl Future<Output = Reply> + Send;

    /// The messages one outbound occasion becomes.
    fn on_outbound(
        &self,
        connection: &mut Self::Connection,
        item: <Self::Outbound as Outbound>::Item,
    ) -> Vec<ServerToAgent>;

    /// A message that could not be decoded. By default it is dropped and the connection read on.
    fn on_unreadable(&self, _connection: &mut Self::Connection, _error: &Unreadable) -> Reply {
        Reply::Nothing
    }

    /// The close frame to send when the outbound side ends the connection; by default one with no
    /// code.
    fn closing(&self, _connection: &Self::Connection) -> Option<Closing> {
        None
    }

    /// The connection is gone: after its one exchange on plain HTTP, when the socket closes on a
    /// WebSocket.
    fn on_closed(&self, _connection: Self::Connection) {}

    /// A message the endpoint did not put on the wire, so that what it carried can wait for the
    /// next occasion. On a WebSocket: a reply or a push whose send failed, the pushes of the same
    /// occasion after it, and one discarded past the size limit. On plain HTTP: a reply past the
    /// limit, answered `500`. A message the transport took and lost afterwards is not seen; on
    /// plain HTTP that is every lost answer, since the body goes out with the response's head. On
    /// a WebSocket this comes before [`on_closed`](Self::on_closed); on plain HTTP after it. By
    /// default nothing is done.
    fn on_unsent(&self, _message: &ServerToAgent) {}
}

/// Why the endpoint closes a WebSocket: a close code of RFC 6455 and a reason for the peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closing {
    pub code: u16,
    pub reason: String,
}

impl Closing {
    /// `1008`: the connection violates the endpoint's policy — what a revoked proof does.
    #[must_use]
    pub fn policy(reason: &str) -> Self {
        Closing {
            code: u16::from(CloseCode::Policy),
            reason: reason.to_string(),
        }
    }
}

/// How many messages of one WebSocket may wait behind the one being handled. With them, or with
/// as many bytes as the message limit waiting, the socket is read no further until one is handled:
/// what a slow handler buffers per connection stays bounded, at most two messages' limit beyond
/// the one in hand.
const WAITING_MESSAGES: usize = 8;

struct Endpoint<H> {
    handler: Arc<H>,
    limit: usize,
}

/// The endpoint for `handler`, as a router to serve or to merge into one.
pub fn router<H: Handler>(handler: Arc<H>, settings: Settings) -> Router {
    let endpoint = Arc::new(Endpoint {
        handler,
        limit: settings.max_message_size,
    });
    let methods: MethodRouter<Arc<Endpoint<H>>> = match settings.transports {
        Transports::Both => get(upgrade::<H>).post(exchange::<H>),
        Transports::WebSocketOnly => get(upgrade::<H>),
    };
    let router = if settings.any_path {
        Router::new().fallback(methods)
    } else {
        Router::new().route(OPAMP_PATH, methods)
    };
    router
        // The receive limit on the plain-HTTP transport: a body past it never reaches a handler,
        // and axum answers it with the 413 the specification prescribes.
        .layer(DefaultBodyLimit::max(settings.max_message_size))
        .with_state(endpoint)
}

fn peer_of(extensions: &Extensions) -> Option<SocketAddr> {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| *peer)
}

/// The WebSocket upgrade, answered here and taken over through hyper, so that `opamp` reads the
/// frames itself and can judge a message by its data (ADR-0008 clause 5).
async fn upgrade<H: Handler>(
    State(endpoint): State<Arc<Endpoint<H>>>,
    mut request: Request,
) -> Response {
    let refuse = |why: &str| (StatusCode::BAD_REQUEST, why.to_string()).into_response();
    // `get` routes HEAD here too; an upgrade is a GET (RFC 6455 §4.1).
    if request.method() != axum::http::Method::GET {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            "a WebSocket upgrade is a GET",
        )
            .into_response();
    }
    let headers = request.headers();
    let has_token = |name: &header::HeaderName, token: &str| {
        headers.get_all(name).iter().any(|value| {
            value.to_str().is_ok_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case(token))
            })
        })
    };
    if request.version() != Version::HTTP_11 {
        return refuse("a WebSocket upgrade needs HTTP/1.1");
    }
    if !has_token(&header::CONNECTION, "upgrade") || !has_token(&header::UPGRADE, "websocket") {
        return refuse("expected a WebSocket upgrade");
    }
    if header_str(headers, &header::SEC_WEBSOCKET_VERSION) != "13" {
        return refuse("the WebSocket version must be 13");
    }
    let Some(key) = headers.get(header::SEC_WEBSOCKET_KEY) else {
        return refuse("the WebSocket key is missing");
    };
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
    let info = RequestInfo {
        transport: Transport::WebSocket,
        headers,
        extensions: request.extensions(),
        peer: peer_of(request.extensions()),
    };
    let (connection, outbound) = match endpoint.handler.on_connecting(&info) {
        Ok(accepted) => accepted,
        Err(refusal) => return refusal.into_response(),
    };
    let floor = request.extensions().get::<pace::Pace>().copied();
    let Some(on_upgrade) = request
        .extensions_mut()
        .remove::<hyper::upgrade::OnUpgrade>()
    else {
        endpoint.handler.on_closed(connection);
        return (
            StatusCode::UPGRADE_REQUIRED,
            "this connection cannot be upgraded",
        )
            .into_response();
    };
    let limit = endpoint.limit;
    // A session served by a `Listener` closes when the listener asks it to; one served any other
    // way runs until its peer leaves.
    let sessions = request.extensions().get::<listen::Sessions>().cloned();
    let session = {
        let sessions = sessions.clone();
        async move {
            let upgraded = match on_upgrade.await {
                Ok(upgraded) => upgraded,
                Err(e) => {
                    warn!(error = %e, "a WebSocket upgrade failed");
                    endpoint.handler.on_closed(connection);
                    return;
                }
            };
            let stream = pace::FrameMeter::new(hyper_util::rt::TokioIo::new(upgraded));
            // The transport's own guard, so an oversized frame is refused before it is buffered whole;
            // the loop still checks, because that is what turns the refusal into the 1009 close. The
            // per-frame cap moves with it: left at its default it would refuse messages *below* the
            // configured limit, which is the limit's business.
            let config = WebSocketConfig::default()
                .max_message_size(Some(limit))
                .max_frame_size(Some(limit));
            let socket = WebSocketStream::from_raw_socket(stream, Role::Server, Some(config)).await;
            let messages = floor.map(pace::Messages::new);
            serve_socket(socket, endpoint, connection, outbound, messages, sessions).await;
        }
    };
    match &sessions {
        Some(listener) => listener.spawn(session),
        None => {
            tokio::spawn(session);
        }
    }
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_ACCEPT, accept)
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// What was read from a WebSocket and waits for its turn: messages are handled in the order they
/// arrived, and the end of what the peer sends comes after them.
enum Incoming {
    Data(Bytes),
    /// The peer closed, or its side of the connection ended.
    Closed,
    /// A receive error, such as a message past the limit the transport refused.
    Failed,
}

/// A message being handled, and the connection it is handled on.
type Handling<'a, C> = std::pin::Pin<Box<dyn Future<Output = (C, Reply)> + Send + 'a>>;

/// One WebSocket connection, until the peer closes it, it fails, or its outbound side ends.
///
/// The socket is read on while a message is handled, so a ping is answered even then: the
/// messages read meanwhile wait, at most [`WAITING_MESSAGES`] of them and no more bytes than the
/// message limit, and are handed to the handler one at a time in the order they arrived.
async fn serve_socket<H: Handler, S>(
    mut socket: WebSocketStream<pace::FrameMeter<S>>,
    endpoint: Arc<Endpoint<H>>,
    connection: H::Connection,
    mut outbound: Option<H::Outbound>,
    mut messages: Option<pace::Messages>,
    mut sessions: Option<listen::Sessions>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let handler: &H = &endpoint.handler;
    let limit = endpoint.limit;
    // A listener that holds its connections to a floor (ADR-0045 clause 18) is judged once a window.
    let mut check = messages.as_ref().map(|messages| {
        let mut check = tokio::time::interval_at(
            tokio::time::Instant::now() + messages.window(),
            messages.window(),
        );
        // Checks a busy loop missed are not made up in a burst: two in a row would judge a message
        // by what arrived in no time at all.
        check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        check
    });
    // The connection is here while no message is being handled, and inside `handling` while one is.
    let mut connection = Some(connection);
    let mut handling: Option<Handling<'_, H::Connection>> = None;
    let mut waiting: std::collections::VecDeque<Incoming> = std::collections::VecDeque::new();
    let mut waiting_bytes = 0usize;
    // The peer's end has been read: nothing more is read. After its close no reply can go out.
    let mut ended = false;
    let mut closed = false;
    loop {
        let reading = !ended && waiting.len() < WAITING_MESSAGES && waiting_bytes < limit;
        let idle = handling.is_none();
        tokio::select! {
            // A shutdown comes first, so a peer that keeps sending cannot hold its session past it,
            // but after the message in hand, whose reply still goes out. Then a handled message's
            // reply, then the next message waiting, then what has arrived is read before a check
            // judges it, so bytes that waited on this side while the loop was busy count for the
            // peer.
            biased;
            () = next_closing(&mut sessions), if idle => {
                // The listener is shutting down: the peer is told so, and reconnects to whatever
                // serves this endpoint next.
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Away,
                        reason: "the server is shutting down".into(),
                    })))
                    .await;
                break;
            }
            (handled, reply) = next_handled(&mut handling) => {
                handling = None;
                connection = Some(handled);
                if answer(&mut socket, handler, reply, limit, closed).await == Sending::Gone {
                    break;
                }
            }
            () = std::future::ready(()), if idle && !waiting.is_empty() => {
                let Some(mut current) = connection.take() else { break };
                let data = match waiting.pop_front() {
                    Some(Incoming::Data(data)) => data,
                    Some(Incoming::Failed) => {
                        connection = Some(current);
                        let _ = socket.send(too_big_close()).await;
                        break;
                    }
                    Some(Incoming::Closed) | None => {
                        connection = Some(current);
                        break;
                    }
                };
                waiting_bytes -= data.len();
                match frame::decode::<AgentToServer>(&data, limit) {
                    Ok(report) => {
                        handling = Some(Box::pin(async move {
                            let reply = handler.on_message(&mut current, report).await;
                            (current, reply)
                        }));
                    }
                    // An oversized frame is malformed, and the specification's answer to it is
                    // not an error message but the 1009 close.
                    Err(e @ FrameError::TooLarge(..)) => {
                        connection = Some(current);
                        warn!(error = %e, "closing the WebSocket: the peer sent an oversized message");
                        let _ = socket.send(too_big_close()).await;
                        break;
                    }
                    Err(e) => {
                        warn!(error = %e, "undecodable frame on the WebSocket transport");
                        let reply = handler.on_unreadable(&mut current, &Unreadable::Frame(e));
                        connection = Some(current);
                        if answer(&mut socket, handler, reply, limit, closed).await == Sending::Gone {
                            break;
                        }
                    }
                }
            }
            incoming = socket.next(), if reading => {
                match incoming {
                    Some(Ok(Message::Binary(data))) => {
                        waiting_bytes += data.len();
                        waiting.push_back(Incoming::Data(data));
                    }
                    // The upgrade capped what this socket will buffer, so a peer past the limit
                    // surfaces here as a receive error rather than as a frame. It is answered with
                    // the 1009 close once the messages before it are handled; the send fails
                    // harmlessly when the connection is simply gone or already closing.
                    Some(Err(e)) => {
                        warn!(error = %e, "closing the WebSocket after a receive error");
                        waiting.push_back(Incoming::Failed);
                        ended = true;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        waiting.push_back(Incoming::Closed);
                        ended = true;
                        closed = true;
                    }
                    // The library answers pings itself; pongs and text need nothing from us.
                    Some(Ok(_)) => {}
                }
            }
            item = next_outbound(&mut outbound), if idle && waiting.is_empty() => {
                let Some(current) = connection.as_mut() else { break };
                let Some(item) = item else {
                    let frame = handler.closing(current).map(|closing| CloseFrame {
                        code: CloseCode::from(closing.code),
                        reason: closing.reason.into(),
                    });
                    let _ = socket.send(Message::Close(frame)).await;
                    break;
                };
                let mut gone = false;
                for message in handler.on_outbound(current, item) {
                    // What follows a failed send is not tried on a connection that is gone.
                    let sent = if gone {
                        Sending::Gone
                    } else {
                        send_framed(&mut socket, &message, limit).await
                    };
                    if sent != Sending::Sent {
                        handler.on_unsent(&message);
                    }
                    gone |= sent == Sending::Gone;
                }
                if gone {
                    break;
                }
            }
            scheduled = next_check(&mut check) => {
                let Some(messages) = messages.as_mut() else { continue };
                let now = socket.get_ref().progress();
                // A check this late means the loop was busy and read nothing meanwhile, and one
                // while the socket is not read finds no progress the peer could have made: the
                // peer is judged from here, not by what waited on this side.
                if !reading || scheduled.elapsed() > messages.window() / 4 {
                    messages.rebase(now);
                    continue;
                }
                if messages.check(now) {
                    warn!("closing the WebSocket: a message fell below its pace");
                    let _ = socket
                        .send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Policy,
                            reason: "message below its pace".into(),
                        })))
                        .await;
                    break;
                }
            }
        }
    }
    // A message still in hand when the connection ends is handled to its end, not cut off; its
    // reply can no longer go out.
    if let Some(handling) = handling {
        let (handled, reply) = handling.await;
        connection = Some(handled);
        if let Reply::Send(reply) = reply {
            handler.on_unsent(&reply);
        }
    }
    if let Some(connection) = connection {
        handler.on_closed(connection);
    }
}

/// The reply of the message being handled, once it is; never, while none is.
async fn next_handled<C>(handling: &mut Option<Handling<'_, C>>) -> (C, Reply) {
    match handling {
        Some(handling) => handling.await,
        None => std::future::pending().await,
    }
}

/// Puts a reply on the wire; after the peer's close, when it can no longer go out, it is reported
/// unsent instead.
async fn answer<H: Handler, S>(
    socket: &mut WebSocketStream<S>,
    handler: &H,
    reply: Reply,
    limit: usize,
    closed: bool,
) -> Sending
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Reply::Send(reply) = reply else {
        return Sending::Sent;
    };
    if closed {
        handler.on_unsent(&reply);
        return Sending::Discarded;
    }
    let sent = send_framed(socket, &reply, limit).await;
    if sent != Sending::Sent {
        handler.on_unsent(&reply);
    }
    sent
}

/// Resolves when the listener asks its sessions to close; never, for a session without one.
async fn next_closing(sessions: &mut Option<listen::Sessions>) {
    match sessions {
        Some(sessions) => sessions.closing().await,
        None => std::future::pending().await,
    }
}

/// The next check of a paced socket, and when it was due; never, for one without a floor.
async fn next_check(check: &mut Option<tokio::time::Interval>) -> tokio::time::Instant {
    match check {
        Some(check) => check.tick().await,
        None => std::future::pending().await,
    }
}

async fn next_outbound<O: Outbound>(outbound: &mut Option<O>) -> Option<O::Item> {
    match outbound {
        Some(outbound) => outbound.next().await,
        None => std::future::pending().await,
    }
}

/// What became of one message given to [`send_framed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sending {
    Sent,
    /// Past the size limit, and not sent; the connection is fine.
    Discarded,
    /// The send failed: the connection is gone.
    Gone,
}

/// Sends one framed message under the size limit. A message that would exceed it is **not** sent —
/// the specification's MUST for the outbound direction — but discarded with a log line, since the
/// fault is on this end and the connection is fine.
async fn send_framed<S>(
    socket: &mut WebSocketStream<S>,
    message: &ServerToAgent,
    limit: usize,
) -> Sending
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match frame::encode_within(message, limit) {
        Ok(framed) => match socket.send(Message::Binary(framed.into())).await {
            Ok(()) => Sending::Sent,
            Err(_) => Sending::Gone,
        },
        Err(e) => {
            warn!(error = %e, "discarding a message that exceeds the size limit");
            Sending::Discarded
        }
    }
}

/// The close the specification names for a message past the size limit: 1009, Message Too Big.
fn too_big_close() -> Message {
    Message::Close(Some(CloseFrame {
        code: CloseCode::Size,
        reason: frame::TOO_BIG_CLOSE_REASON.into(),
    }))
}

/// One plain-HTTP exchange: protobuf `AgentToServer` in (gzip accepted — a MUST), protobuf
/// `ServerToAgent` out.
async fn exchange<H: Handler>(
    State(endpoint): State<Arc<Endpoint<H>>>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Bytes,
) -> Response {
    let content_type = header_str(&headers, &header::CONTENT_TYPE);
    if !crate::endpoint::is_protobuf(content_type) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!(
                "the OpAMP plain-HTTP transport requires Content-Type: {PROTOBUF_CONTENT_TYPE}"
            ),
        )
            .into_response();
    }
    let limit = endpoint.limit;
    let raw = match crate::endpoint::decode_body(
        &body,
        header_str(&headers, &header::CONTENT_ENCODING),
        limit,
    ) {
        Ok(raw) => raw,
        Err(e @ BodyError::TooLarge) => {
            warn!(
                limit,
                "rejecting a request body that decompresses past the limit"
            );
            return (StatusCode::PAYLOAD_TOO_LARGE, e.to_string()).into_response();
        }
        Err(e @ BodyError::UndecodableGzip) => {
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
        Err(e @ BodyError::UnsupportedEncoding(_)) => {
            return (StatusCode::UNSUPPORTED_MEDIA_TYPE, e.to_string()).into_response();
        }
    };

    let request = RequestInfo {
        transport: Transport::Http,
        headers: &headers,
        extensions: &extensions,
        peer: peer_of(&extensions),
    };
    let handler = &endpoint.handler;
    let mut connection = match handler.on_connecting(&request) {
        Ok((connection, _)) => connection,
        Err(refusal) => return refusal.into_response(),
    };
    let reply = match AgentToServer::decode(raw.as_slice()) {
        Ok(report) => handler.on_message(&mut connection, report).await,
        Err(e) => {
            warn!(error = %e, "undecodable report on the plain-HTTP transport");
            handler.on_unreadable(&mut connection, &Unreadable::Body(e))
        }
    };
    handler.on_closed(connection);

    let (reply, sent) = match reply {
        Reply::Send(reply) => (reply, true),
        Reply::Nothing => (ServerToAgent::default(), false),
        Reply::Refuse(status, why) => return (status, why).into_response(),
    };
    // The send side of the same limit: an oversized response is never put on the wire, so a reply
    // that outgrew it is discarded — recorded here rather than shipped — and the client sees a
    // failed exchange instead of a body it must refuse.
    let encoded = reply.encode_to_vec();
    if encoded.len() > limit {
        warn!(
            size = encoded.len(),
            limit, "discarding a response that exceeds the message size limit"
        );
        if sent {
            handler.on_unsent(&reply);
        }
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the response exceeds the message size limit",
        )
            .into_response();
    }
    ([(header::CONTENT_TYPE, PROTOBUF_CONTENT_TYPE)], encoded).into_response()
}

fn header_str<'a>(headers: &'a HeaderMap, name: &header::HeaderName) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::task::{Context, Poll};
    use std::time::Duration;
    use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
    use tokio::sync::mpsc;

    struct Pushes(mpsc::Receiver<()>);

    impl Outbound for Pushes {
        type Item = ();

        async fn next(&mut self) -> Option<()> {
            self.0.recv().await
        }
    }

    /// Answers each message with its `instance_uid`, pushes two messages per occasion, and records
    /// what the endpoint reports unsent.
    #[derive(Default)]
    struct Recording {
        unsent: Mutex<Vec<ServerToAgent>>,
    }

    fn named(uid: &[u8]) -> ServerToAgent {
        ServerToAgent {
            instance_uid: uid.to_vec(),
            ..Default::default()
        }
    }

    impl Handler for Recording {
        type Connection = ();
        type Outbound = Pushes;

        fn on_connecting(&self, _: &RequestInfo<'_>) -> Result<((), Option<Pushes>), Rejection> {
            unreachable!("the tests serve the socket directly")
        }

        async fn on_message(&self, _: &mut (), message: AgentToServer) -> Reply {
            let mut reply = named(&message.instance_uid);
            if message.instance_uid == b"big" {
                reply.error_response = Some(crate::proto::ServerErrorResponse {
                    error_message: "x".repeat(LIMIT),
                    ..Default::default()
                });
            }
            Reply::Send(reply)
        }

        fn on_outbound(&self, _: &mut (), (): ()) -> Vec<ServerToAgent> {
            vec![named(b"first"), named(b"second")]
        }

        fn on_unsent(&self, message: &ServerToAgent) {
            self.unsent.lock().expect("unsent").push(message.clone());
        }
    }

    /// The server's half of a socket, whose writes fail once it is broken.
    struct Breaking {
        inner: DuplexStream,
        broken: Arc<AtomicBool>,
    }

    impl AsyncRead for Breaking {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for Breaking {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.broken.load(Ordering::SeqCst) {
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            }
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            if self.broken.load(Ordering::SeqCst) {
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            }
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    const LIMIT: usize = 4096;

    /// A session served over a socket that breaks when told to: the handler, the switch, the
    /// client's end, the push sender, and the session itself.
    async fn session() -> (
        Arc<Recording>,
        Arc<AtomicBool>,
        WebSocketStream<DuplexStream>,
        mpsc::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let handler = Arc::new(Recording::default());
        let endpoint = Arc::new(Endpoint {
            handler: handler.clone(),
            limit: LIMIT,
        });
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let broken = Arc::new(AtomicBool::new(false));
        let stream = pace::FrameMeter::new(Breaking {
            inner: ours,
            broken: broken.clone(),
        });
        let socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
        let client = WebSocketStream::from_raw_socket(theirs, Role::Client, None).await;
        let (pushes, outbound) = mpsc::channel(4);
        let served = tokio::spawn(serve_socket(
            socket,
            endpoint,
            (),
            Some(Pushes(outbound)),
            None,
            None,
        ));
        (handler, broken, client, pushes, served)
    }

    async fn ended(served: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("the session ends in time")
            .expect("the session");
    }

    /// A reply whose send fails is reported unsent, and the session ends.
    #[tokio::test]
    async fn a_websocket_reply_whose_send_fails_is_reported_unsent() {
        let (handler, broken, mut client, _pushes, served) = session().await;
        broken.store(true, Ordering::SeqCst);
        let report = AgentToServer {
            instance_uid: b"agent".to_vec(),
            ..Default::default()
        };
        let framed = frame::encode_within(&report, LIMIT).expect("framed");
        client
            .send(Message::Binary(framed.into()))
            .await
            .expect("sent to the server");
        ended(served).await;
        assert_eq!(*handler.unsent.lock().expect("unsent"), [named(b"agent")]);
    }

    /// A reply past the limit is discarded and reported unsent, and the session reads on.
    #[tokio::test]
    async fn a_websocket_reply_past_the_limit_is_reported_unsent() {
        let (handler, _broken, mut client, _pushes, _served) = session().await;
        for uid in [b"big".as_slice(), b"agent"] {
            let report = AgentToServer {
                instance_uid: uid.to_vec(),
                ..Default::default()
            };
            let framed = frame::encode_within(&report, LIMIT).expect("framed");
            client
                .send(Message::Binary(framed.into()))
                .await
                .expect("sent to the server");
        }
        let message = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .expect("a reply in time")
            .expect("an open socket")
            .expect("a message");
        let Message::Binary(data) = message else {
            panic!("expected a binary frame, got {message:?}");
        };
        let reply: ServerToAgent = frame::decode(&data, LIMIT).expect("framed");
        assert_eq!(reply.instance_uid, b"agent", "the session reads on");
        let unsent = handler.unsent.lock().expect("unsent").clone();
        assert_eq!(unsent.len(), 1);
        assert_eq!(unsent[0].instance_uid, b"big");
    }

    /// Pushes that go out are not reported; when one fails, it and the ones after it are, and the
    /// session ends.
    #[tokio::test]
    async fn pushes_after_a_failed_send_are_reported_unsent() {
        let (handler, broken, mut client, pushes, served) = session().await;
        pushes.send(()).await.expect("push");
        for expected in [b"first".as_slice(), b"second"] {
            let message = tokio::time::timeout(Duration::from_secs(5), client.next())
                .await
                .expect("a push in time")
                .expect("an open socket")
                .expect("a message");
            let Message::Binary(data) = message else {
                panic!("expected a binary frame, got {message:?}");
            };
            let pushed: ServerToAgent = frame::decode(&data, LIMIT).expect("framed");
            assert_eq!(pushed.instance_uid, expected);
        }
        assert!(
            handler.unsent.lock().expect("unsent").is_empty(),
            "both were sent"
        );

        broken.store(true, Ordering::SeqCst);
        pushes.send(()).await.expect("push");
        ended(served).await;
        assert_eq!(
            *handler.unsent.lock().expect("unsent"),
            [named(b"first"), named(b"second")]
        );
    }

    /// Holds every message whose `instance_uid` begins with `held` until the test lets one go,
    /// answers each with its `instance_uid`, and records the order and the overlap of its calls.
    struct Gated {
        gate: tokio::sync::Semaphore,
        entered: tokio::sync::Notify,
        handling: std::sync::atomic::AtomicUsize,
        most: std::sync::atomic::AtomicUsize,
        handled: Mutex<Vec<Vec<u8>>>,
        unsent: Mutex<Vec<ServerToAgent>>,
        /// How many messages were handled to their end when the connection was closed.
        closed: Mutex<Option<usize>>,
    }

    impl Default for Gated {
        fn default() -> Self {
            Gated {
                gate: tokio::sync::Semaphore::new(0),
                entered: tokio::sync::Notify::new(),
                handling: std::sync::atomic::AtomicUsize::new(0),
                most: std::sync::atomic::AtomicUsize::new(0),
                handled: Mutex::default(),
                unsent: Mutex::default(),
                closed: Mutex::default(),
            }
        }
    }

    impl Handler for Gated {
        type Connection = ();
        type Outbound = Pushes;

        fn on_connecting(&self, _: &RequestInfo<'_>) -> Result<((), Option<Pushes>), Rejection> {
            unreachable!("the tests serve the socket directly")
        }

        async fn on_message(&self, _: &mut (), message: AgentToServer) -> Reply {
            let now = self.handling.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            self.handled
                .lock()
                .expect("handled")
                .push(message.instance_uid.clone());
            if message.instance_uid.starts_with(b"held") {
                self.entered.notify_one();
                self.gate.acquire().await.expect("the gate").forget();
            }
            self.handling.fetch_sub(1, Ordering::SeqCst);
            Reply::Send(named(&message.instance_uid))
        }

        fn on_outbound(&self, _: &mut (), (): ()) -> Vec<ServerToAgent> {
            Vec::new()
        }

        fn on_closed(&self, (): ()) {
            let handled = self.handled.lock().expect("handled").len();
            let unfinished = self.handling.load(Ordering::SeqCst);
            *self.closed.lock().expect("closed") = Some(handled - unfinished);
        }

        fn on_unsent(&self, message: &ServerToAgent) {
            self.unsent.lock().expect("unsent").push(message.clone());
        }
    }

    /// The server's half of a socket, counting the reads that brought bytes.
    struct Watched {
        inner: DuplexStream,
        reads: tokio::sync::watch::Sender<usize>,
    }

    impl AsyncRead for Watched {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let before = buf.filled().len();
            let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
            if buf.filled().len() > before {
                self.reads.send_modify(|reads| *reads += 1);
            }
            polled
        }
    }

    impl AsyncWrite for Watched {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// A session of [`Gated`]: the handler, the client's end, the server's reads, the session.
    async fn gated() -> (
        Arc<Gated>,
        WebSocketStream<DuplexStream>,
        tokio::sync::watch::Receiver<usize>,
        tokio::task::JoinHandle<()>,
    ) {
        gated_with(None).await
    }

    /// [`gated`], held to a floor on its messages' pace.
    async fn gated_with(
        floor: Option<pace::Messages>,
    ) -> (
        Arc<Gated>,
        WebSocketStream<DuplexStream>,
        tokio::sync::watch::Receiver<usize>,
        tokio::task::JoinHandle<()>,
    ) {
        let handler = Arc::new(Gated::default());
        let endpoint = Arc::new(Endpoint {
            handler: handler.clone(),
            limit: LIMIT,
        });
        let (ours, theirs) = tokio::io::duplex(256 * 1024);
        let (reads, read) = tokio::sync::watch::channel(0);
        let stream = pace::FrameMeter::new(Watched { inner: ours, reads });
        let socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
        let client = WebSocketStream::from_raw_socket(theirs, Role::Client, None).await;
        let served = tokio::spawn(serve_socket(socket, endpoint, (), None, floor, None));
        (handler, client, read, served)
    }

    async fn send_report(client: &mut WebSocketStream<DuplexStream>, uid: &[u8]) {
        let report = AgentToServer {
            instance_uid: uid.to_vec(),
            ..Default::default()
        };
        let framed = frame::encode_within(&report, LIMIT).expect("framed");
        client
            .send(Message::Binary(framed.into()))
            .await
            .expect("sent to the server");
    }

    /// The next frame the client reads: a reply as `Ok(instance_uid)`, anything else as itself.
    async fn next_frame(client: &mut WebSocketStream<DuplexStream>) -> Result<Vec<u8>, Message> {
        let message = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .expect("a frame in time")
            .expect("an open socket")
            .expect("a frame");
        match message {
            Message::Binary(data) => {
                let reply: ServerToAgent = frame::decode(&data, LIMIT).expect("framed");
                Ok(reply.instance_uid)
            }
            other => Err(other),
        }
    }

    /// Nothing reaches the client for a second: the ping sent last is not answered meanwhile.
    async fn nothing_within_a_second(client: &mut WebSocketStream<DuplexStream>) {
        if let Ok(frame) = tokio::time::timeout(Duration::from_secs(1), client.next()).await {
            panic!("expected nothing yet, got {frame:?}");
        }
    }

    async fn entered(handler: &Gated) {
        tokio::time::timeout(Duration::from_secs(5), handler.entered.notified())
            .await
            .expect("the held message is being handled");
    }

    fn pong(payload: &'static [u8]) -> Result<Vec<u8>, Message> {
        Err(Message::Pong(payload.into()))
    }

    /// A ping that arrives while a message of the same connection is being handled is answered
    /// at once, before the message's reply.
    #[tokio::test]
    async fn a_ping_is_answered_while_a_message_is_handled() {
        let (handler, mut client, _read, _served) = gated().await;
        send_report(&mut client, b"held").await;
        entered(&handler).await;
        client
            .send(Message::Ping(b"alive".as_slice().into()))
            .await
            .expect("ping");
        assert_eq!(next_frame(&mut client).await, pong(b"alive"));
        handler.gate.add_permits(1);
        assert_eq!(next_frame(&mut client).await, Ok(b"held".to_vec()));
    }

    /// Messages read while one is handled wait for it: one call at a time, in the order they
    /// arrived, and their replies in that order.
    #[tokio::test]
    async fn the_messages_of_one_connection_are_handled_one_at_a_time_and_answered_in_order() {
        let (handler, mut client, _read, _served) = gated().await;
        send_report(&mut client, b"held").await;
        entered(&handler).await;
        for uid in [b"a".as_slice(), b"held-b", b"c"] {
            send_report(&mut client, uid).await;
        }
        client
            .send(Message::Ping(b"read".as_slice().into()))
            .await
            .expect("ping");
        assert_eq!(next_frame(&mut client).await, pong(b"read"));
        handler.gate.add_permits(2);
        let mut replies = Vec::new();
        for _ in 0..4 {
            replies.push(next_frame(&mut client).await.expect("a reply"));
        }
        let order = [
            b"held".to_vec(),
            b"a".to_vec(),
            b"held-b".to_vec(),
            b"c".to_vec(),
        ];
        assert_eq!(replies, order);
        assert_eq!(*handler.handled.lock().expect("handled"), order);
        assert_eq!(handler.most.load(Ordering::SeqCst), 1, "one call at a time");
    }

    /// Behind the message being handled, the connection is read until eight more wait; then not
    /// again until one is handled, so a ping after the eighth is answered only after a reply.
    #[tokio::test]
    async fn a_connection_with_eight_messages_waiting_is_read_no_further_until_one_is_handled() {
        let (handler, mut client, _read, _served) = gated().await;
        send_report(&mut client, b"held").await;
        entered(&handler).await;
        for n in 0..WAITING_MESSAGES - 1 {
            send_report(&mut client, format!("m{n}").as_bytes()).await;
        }
        client
            .send(Message::Ping(b"seven".as_slice().into()))
            .await
            .expect("ping");
        assert_eq!(
            next_frame(&mut client).await,
            pong(b"seven"),
            "seven waiting: read on"
        );

        send_report(&mut client, b"m7").await;
        client
            .send(Message::Ping(b"eight".as_slice().into()))
            .await
            .expect("ping");
        nothing_within_a_second(&mut client).await;
        handler.gate.add_permits(1);
        assert_eq!(
            next_frame(&mut client).await,
            Ok(b"held".to_vec()),
            "eight waiting: the ping is read only once one is handled"
        );
        let mut frames = Vec::new();
        for _ in 0..WAITING_MESSAGES + 1 {
            frames.push(next_frame(&mut client).await);
        }
        assert!(frames.contains(&pong(b"eight")), "{frames:?}");
    }

    /// Messages waiting behind the one handled hold no more bytes than the message limit: once
    /// they reach it the connection is read no further until one is handled.
    #[tokio::test]
    async fn a_connection_with_the_limit_in_bytes_waiting_is_read_no_further_until_one_is_handled()
    {
        let (handler, mut client, _read, _served) = gated().await;
        send_report(&mut client, b"held").await;
        entered(&handler).await;
        let large = vec![b'x'; LIMIT - 200];
        send_report(&mut client, &large).await;
        client
            .send(Message::Ping(b"under".as_slice().into()))
            .await
            .expect("ping");
        assert_eq!(
            next_frame(&mut client).await,
            pong(b"under"),
            "under the limit: read on"
        );

        send_report(&mut client, &[b'y'; 300]).await;
        client
            .send(Message::Ping(b"over".as_slice().into()))
            .await
            .expect("ping");
        nothing_within_a_second(&mut client).await;
        handler.gate.add_permits(1);
        assert_eq!(
            next_frame(&mut client).await,
            Ok(b"held".to_vec()),
            "at the limit: the ping is read only once one is handled"
        );
        let mut frames = Vec::new();
        for _ in 0..3 {
            frames.push(next_frame(&mut client).await);
        }
        assert!(frames.contains(&pong(b"over")));
    }

    /// The peer closes while a message is handled: that message and the ones it sent before the
    /// close are still handled in order, and their replies, which can no longer go out, are
    /// reported unsent before the session ends.
    #[tokio::test]
    async fn messages_before_a_peers_close_are_handled_and_their_replies_reported_unsent() {
        let (handler, mut client, mut read, served) = gated().await;
        send_report(&mut client, b"held").await;
        entered(&handler).await;
        send_report(&mut client, b"after").await;
        client
            .send(Message::Ping(b"read".as_slice().into()))
            .await
            .expect("ping");
        assert_eq!(next_frame(&mut client).await, pong(b"read"));
        let before = *read.borrow();
        client.send(Message::Close(None)).await.expect("close");
        tokio::time::timeout(
            Duration::from_secs(5),
            read.wait_for(|reads| *reads > before),
        )
        .await
        .expect("the close is read in time")
        .expect("the session");
        handler.gate.add_permits(1);
        ended(served).await;
        assert_eq!(
            *handler.handled.lock().expect("handled"),
            [b"held".to_vec(), b"after".to_vec()]
        );
        assert_eq!(
            *handler.unsent.lock().expect("unsent"),
            [named(b"held"), named(b"after")]
        );
    }

    /// A socket that is not read because messages wait is not judged by the floor on its pace:
    /// the peer could make no progress meanwhile. Here a message that has begun when reading
    /// stops is not cut off while the held one is handled for several windows.
    #[tokio::test]
    async fn a_socket_not_read_while_messages_wait_is_not_judged_by_its_pace() {
        let window = Duration::from_millis(300);
        let floor = pace::Messages::new(pace::Pace {
            window,
            min_bytes: 64 * 1024,
        });
        let (handler, mut client, _read, _served) = gated_with(Some(floor)).await;
        send_report(&mut client, b"held").await;
        entered(&handler).await;
        for n in 0..WAITING_MESSAGES {
            send_report(&mut client, format!("m{n}").as_bytes()).await;
        }
        // The head of a masked binary frame of 200 bytes, and nothing of its payload: a message
        // that has begun.
        tokio::io::AsyncWriteExt::write_all(
            client.get_mut(),
            &[0x82, 0x80 | 126, 0, 200, 1, 2, 3, 4],
        )
        .await
        .expect("the head of a frame");
        // Several windows pass while the held message is handled and nothing is read.
        tokio::time::sleep(window * 4).await;
        handler.gate.add_permits(1);
        assert_eq!(
            next_frame(&mut client).await,
            Ok(b"held".to_vec()),
            "still open"
        );
        for n in 0..WAITING_MESSAGES {
            assert_eq!(
                next_frame(&mut client).await,
                Ok(format!("m{n}").into_bytes())
            );
        }
    }

    /// A message that trickles while another is handled is cut off by the floor all the same; the
    /// message in hand is handled to its end, its reply reported unsent, and only then is the
    /// connection closed.
    #[tokio::test]
    async fn a_connection_cut_off_while_a_message_is_handled_closes_after_it() {
        let window = Duration::from_millis(300);
        let floor = pace::Messages::new(pace::Pace {
            window,
            min_bytes: 64 * 1024,
        });
        let (handler, mut client, _read, served) = gated_with(Some(floor)).await;
        send_report(&mut client, b"held").await;
        entered(&handler).await;
        tokio::io::AsyncWriteExt::write_all(
            client.get_mut(),
            &[0x82, 0x80 | 126, 0, 200, 1, 2, 3, 4],
        )
        .await
        .expect("the head of a frame");
        match next_frame(&mut client).await {
            Err(Message::Close(Some(close))) => assert_eq!(close.code, CloseCode::Policy),
            other => panic!("expected the 1008 close, got {other:?}"),
        }
        assert_eq!(
            *handler.closed.lock().expect("closed"),
            None,
            "not closed yet"
        );
        handler.gate.add_permits(1);
        ended(served).await;
        assert_eq!(*handler.unsent.lock().expect("unsent"), [named(b"held")]);
        assert_eq!(*handler.closed.lock().expect("closed"), Some(1));
    }
}
