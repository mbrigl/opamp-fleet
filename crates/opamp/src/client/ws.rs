//! The WebSocket transport: one connection, reconnected with backoff, carrying every Agent of the
//! session — the *n* over one of ADR-0015.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::frame;
use crate::proto::{AgentToServer, ServerToAgent};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderMap, StatusCode};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{
    client_async_tls_with_config, connect_async_tls_with_config, MaybeTlsStream, WebSocketStream,
};
use tracing::{info, warn};

pub use tokio_tungstenite::Connector;

use super::{AfterReply, Backoff, Ended, ReportSink, Session, StopSignal};

/// One open WebSocket to a server.
pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// One WebSocket connection's material, built by the application.
pub struct Settings {
    /// `ws://` or `wss://`.
    pub endpoint: String,
    /// Sent with every upgrade request — the `Authorization` header, typically, which the Server
    /// checks before the WebSocket comes up.
    pub headers: HeaderMap,
    /// The TLS configuration for `wss://`; `None` takes the transport's default.
    pub connector: Option<Connector>,
    /// The largest message received or sent, framing header included.
    pub max_message_size: usize,
    /// A routine report per Agent this often; `None` sends none.
    pub heartbeat: Option<Duration>,
}

/// How long a connection may stay silent: a WebSocket ping goes out once nothing has arrived for
/// `ping_after`, and the connection ends once nothing has arrived for `end_after`
/// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Silence {
    pub ping_after: Duration,
    pub end_after: Duration,
}

impl Silence {
    /// The bound without a heartbeat: the default heartbeat interval.
    pub const WITHOUT_HEARTBEAT: Duration = Duration::from_secs(30);

    /// The bounds for a connection with the heartbeat `heartbeat`: the end after one interval, the
    /// ping after half of one.
    #[must_use]
    pub fn of(heartbeat: Option<Duration>) -> Self {
        let end_after = heartbeat
            .filter(|interval| !interval.is_zero())
            .unwrap_or(Self::WITHOUT_HEARTBEAT);
        Silence {
            ping_after: end_after / 2,
            end_after,
        }
    }

    /// When the silence is next acted on, with something last heard at `heard` and the ping sent
    /// since at `pinged`, if one was: without a ping, the moment to send one; with one, the end,
    /// which gives the pong the rest of the interval from when the ping went out, so a loop that
    /// was busy reading nothing does not take its own delay for the peer's silence.
    #[must_use]
    pub fn next(
        &self,
        heard: tokio::time::Instant,
        pinged: Option<tokio::time::Instant>,
    ) -> tokio::time::Instant {
        match pinged {
            None => heard + self.ping_after,
            Some(pinged) => {
                (heard + self.end_after).max(pinged + (self.end_after - self.ping_after))
            }
        }
    }
}

/// How one connection ended.
enum Served {
    Stopped,
    ConnectionLost,
    /// The Server answered `Unavailable` and the connection was closed to honour it: the Client
    /// stays away the wait it named, as after a refused upgrade, and draws nothing on top.
    /// `answered` says whether a reply was handled on the connection before it.
    Throttled {
        wait: Duration,
        answered: bool,
    },
    Reconnect,
    End,
}

/// Runs the session over a WebSocket until it is stopped or asks to end, reconnecting with backoff
/// whenever the connection is lost.
///
/// # Errors
/// Returns an error when the endpoint is not a valid URL.
pub async fn run<S: Session, X: StopSignal>(
    settings: &Settings,
    session: &mut S,
    stop: &mut X,
) -> Result<Ended, String> {
    run_from(settings, None, session, stop).await
}

/// [`run`], with every connection opened from `local` where it names an address. A Client names
/// none; a program that stands in for many hosts on one machine names one per host, so that a
/// balancer keyed on the peer's address tells them apart.
///
/// # Errors
/// Returns an error when the endpoint is not a valid URL.
pub async fn run_from<S: Session, X: StopSignal>(
    settings: &Settings,
    local: Option<IpAddr>,
    session: &mut S,
    stop: &mut X,
) -> Result<Ended, String> {
    let mut backoff = Backoff::new();
    // Whether the last connection ended on an `Unavailable` before any reply was handled on it:
    // the next connection does not start the steps again, so consecutive `Unavailable`s grow
    // along the backoff rather than come back every first step.
    let mut throttled = false;
    loop {
        // What to wait before the next attempt: drawn over the first window when a connection
        // that stood was lost, the next exponential step when an attempt failed, and no less
        // than a refusal's `Retry-After` or the wait an `Unavailable` named.
        let mut lost = false;
        let mut retry_after = None;
        match open(settings, local).await {
            Ok(socket) => {
                info!(endpoint = %settings.endpoint, "connected");
                if !throttled {
                    backoff.reset();
                }
                let served = serve(socket, settings, session, stop).await;
                throttled = matches!(served, Served::Throttled { .. });
                match served {
                    Served::Stopped => {
                        // Usually already stopped before the goodbyes went out; idempotent.
                        session.stop().await;
                        return Ok(Ended::Stopped);
                    }
                    Served::Reconnect => return Ok(Ended::Reconnect),
                    Served::End => return Ok(Ended::End),
                    Served::ConnectionLost => lost = true,
                    Served::Throttled { wait, answered } => {
                        // A connection that handled a reply first was a working one: the steps
                        // start again from it.
                        if answered {
                            backoff.reset();
                        }
                        warn!(
                            endpoint = %settings.endpoint,
                            retry_after = ?wait,
                            "the server answered unavailable; waiting"
                        );
                        retry_after = Some(wait);
                    }
                }
            }
            Err(Connect::Refused(WsError::Http(response)))
                if response.status() == StatusCode::UNAUTHORIZED =>
            {
                warn!(
                    endpoint = %settings.endpoint,
                    "admission failed: the server refused this client (HTTP 401)"
                );
            }
            // Throttled (ADR-0045 clause 4): the Server asked
            // to be given this long, and is given at least that.
            Err(Connect::Refused(WsError::Http(response)))
                if response.status() == StatusCode::SERVICE_UNAVAILABLE
                    || response.status() == StatusCode::TOO_MANY_REQUESTS =>
            {
                retry_after = response
                    .headers()
                    .get(tokio_tungstenite::tungstenite::http::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(super::retry_after_seconds);
                warn!(
                    endpoint = %settings.endpoint,
                    status = %response.status(),
                    retry_after = ?retry_after,
                    "the server is throttling new connections; waiting"
                );
            }
            Err(Connect::Refused(e)) => {
                warn!(endpoint = %settings.endpoint, error = %e, "cannot connect");
            }
            Err(Connect::InvalidEndpoint(e)) => return Err(e),
        }

        let delay = next_delay(&mut backoff, lost, retry_after);
        if lost {
            // The delay drawn is logged, so that a fleet's spread can be read back.
            warn!(
                delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                "connection lost; reconnecting"
            );
        }
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = stop.requested() => {
                // Stopped while disconnected: no goodbyes to send, but what the Agents run still
                // stops before the run ends.
                session.stop().await;
                return Ok(Ended::Stopped);
            }
        }
    }
}

/// One attempt to open a connection, for [`run_from`] and for `Connection::connect_websocket`:
/// the endpoint's name is resolved within one silence bound, and its addresses are tried in turn
/// by [`open_each`], so an endpoint that accepts and never answers is a failed attempt rather than
/// a wait without end, and one dead address does not hide the next
/// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clause 3).
pub(crate) async fn open(settings: &Settings, local: Option<IpAddr>) -> Result<Socket, Connect> {
    let bound = Silence::of(settings.heartbeat).end_after;
    let (_, host, port) = target(&settings.endpoint, &settings.headers)?;
    let addresses =
        match tokio::time::timeout(bound, tokio::net::lookup_host((host.as_str(), port))).await {
            Ok(Ok(addresses)) => addresses.collect(),
            Ok(Err(e)) => return Err(Connect::Refused(WsError::Io(e))),
            Err(_) => {
                return Err(timed_out(format!(
                    "{host} did not resolve within {} ms",
                    bound.as_millis()
                )))
            }
        };
    open_each(settings, local, addresses, bound).await
}

/// Opens a WebSocket to the first of `addresses`, in their order, that one is opened to within
/// `bound`, the TCP connection, the TLS handshake and the upgrade together. The TLS server name
/// and the `Host` header are the endpoint's name. An address not reached, by its bound or by an
/// error of the connection, the TLS handshake or the I/O, gives way to the next. An address that
/// answered the upgrade, with an HTTP refusal or with anything that is not a WebSocket, ends the
/// attempt with that answer, as does any other error. With `local`, only the addresses of its
/// family are tried, each from it.
async fn open_each(
    settings: &Settings,
    local: Option<IpAddr>,
    addresses: Vec<SocketAddr>,
    bound: Duration,
) -> Result<Socket, Connect> {
    let mut last = None;
    for remote in addresses
        .into_iter()
        .filter(|remote| local.is_none_or(|local| remote.is_ipv4() == local.is_ipv4()))
    {
        let attempt = async {
            let (request, _, _) = target(&settings.endpoint, &settings.headers)?;
            let stream = stream_to(remote, local)
                .await
                .map_err(|e| Connect::Refused(WsError::Io(e)))?;
            client_async_tls_with_config(
                request,
                stream,
                Some(config(settings.max_message_size)),
                settings.connector.clone(),
            )
            .await
            .map(|(socket, _)| socket)
            .map_err(Connect::Refused)
        };
        match tokio::time::timeout(bound, attempt).await {
            Ok(Ok(socket)) => return Ok(socket),
            // Not reached: the connection, the TLS handshake or the I/O failed.
            Ok(Err(Connect::Refused(WsError::Io(e)))) => {
                last = Some(Connect::Refused(WsError::Io(std::io::Error::new(
                    e.kind(),
                    format!("{remote}: {e}"),
                ))));
            }
            Ok(Err(e @ Connect::Refused(WsError::Tls(_)))) => last = Some(e),
            // Reached: something answered the upgrade, and its answer is the attempt's.
            Ok(Err(answered)) => return Err(answered),
            Err(_) => {
                last = Some(timed_out(format!(
                    "nothing arrived from {remote} within {} ms of opening the connection",
                    bound.as_millis()
                )));
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        Connect::Refused(WsError::Io(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            format!("no address of {} to connect to", settings.endpoint),
        )))
    }))
}

fn timed_out(message: String) -> Connect {
    Connect::Refused(WsError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        message,
    )))
}

/// What to wait before the next attempt: the first reconnect after a connection that stood was
/// lost is drawn over `[0, 30 s)`; an attempt that failed, or a connection closed to honour an
/// `Unavailable`, waits the next exponential step, or the `Retry-After` it was refused with or the
/// wait the `Unavailable` named when that is longer.
fn next_delay(backoff: &mut Backoff, lost: bool, retry_after: Option<Duration>) -> Duration {
    if lost {
        return backoff.after_loss();
    }
    let step = backoff.advance();
    retry_after.map_or(step, |after| after.max(step))
}

/// Why [`connect`] failed.
#[derive(Debug)]
pub enum Connect {
    /// The endpoint is not a WebSocket URL; trying again will not help.
    InvalidEndpoint(String),
    /// The server could not be reached, or it refused the upgrade.
    Refused(WsError),
}

impl std::fmt::Display for Connect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Connect::InvalidEndpoint(e) => f.write_str(e),
            Connect::Refused(e) => e.fmt(f),
        }
    }
}

/// Opens one WebSocket to `endpoint`, sending `headers` with the upgrade request.
///
/// The receive limit the specification requires of an Agent is set on the socket: the transport
/// refuses to buffer a message past it, so an oversized server can never make this process
/// allocate without bound. The per-frame cap moves with it: left at its default it would refuse
/// messages below the limit, which is the limit's business, not the framing's.
///
/// # Errors
/// Returns why the socket could not be opened.
pub async fn connect(
    endpoint: &str,
    headers: &HeaderMap,
    connector: Option<Connector>,
    max_message_size: usize,
) -> Result<Socket, Connect> {
    connect_from(endpoint, headers, connector, max_message_size, None).await
}

/// [`connect`], from the local address `local` where it names one.
///
/// # Errors
/// Returns why the socket could not be opened.
pub async fn connect_from(
    endpoint: &str,
    headers: &HeaderMap,
    connector: Option<Connector>,
    max_message_size: usize,
    local: Option<IpAddr>,
) -> Result<Socket, Connect> {
    let (request, host, port) = target(endpoint, headers)?;
    let config = config(max_message_size);
    let Some(local) = local else {
        return connect_async_tls_with_config(request, Some(config), false, connector)
            .await
            .map(|(socket, _)| socket)
            .map_err(Connect::Refused);
    };
    let stream = bound_stream(local, &host, port)
        .await
        .map_err(|e| Connect::Refused(WsError::Io(e)))?;
    client_async_tls_with_config(request, stream, Some(config), connector)
        .await
        .map(|(socket, _)| socket)
        .map_err(Connect::Refused)
}

/// The upgrade request to `endpoint` with `headers`, and the host and port it names.
fn target(
    endpoint: &str,
    headers: &HeaderMap,
) -> Result<
    (
        tokio_tungstenite::tungstenite::handshake::client::Request,
        String,
        u16,
    ),
    Connect,
> {
    let mut request = endpoint
        .into_client_request()
        .map_err(|e| Connect::InvalidEndpoint(format!("invalid endpoint {endpoint}: {e}")))?;
    request.headers_mut().extend(headers.clone());
    let uri = request.uri();
    let host = uri
        .host()
        .ok_or_else(|| Connect::InvalidEndpoint(format!("no host in {endpoint}")))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("wss") {
            443
        } else {
            80
        });
    Ok((request, host, port))
}

/// The socket's limits: the receive limit, and the per-frame cap moved with it.
fn config(max_message_size: usize) -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(max_message_size))
        .max_frame_size(Some(max_message_size))
}

/// A TCP connection to `host:port` from `local`, trying each address of `host` of `local`'s family.
async fn bound_stream(local: IpAddr, host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut last = None;
    for remote in tokio::net::lookup_host((host, port)).await? {
        if remote.is_ipv4() != local.is_ipv4() {
            continue;
        }
        match stream_to(remote, Some(local)).await {
            Ok(stream) => return Ok(stream),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            format!("no address of {host} in the family of {local}"),
        )
    }))
}

/// A TCP connection to `remote`, from `local` where it names one.
async fn stream_to(remote: SocketAddr, local: Option<IpAddr>) -> std::io::Result<TcpStream> {
    let Some(local) = local else {
        return TcpStream::connect(remote).await;
    };
    let socket = if local.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    socket.bind(SocketAddr::new(local, 0))?;
    socket.connect(remote).await
}

async fn serve<S: Session, X: StopSignal>(
    mut socket: Socket,
    settings: &Settings,
    session: &mut S,
    stop: &mut X,
) -> Served {
    let limit = settings.max_message_size;

    // A (re)connected Server may know nothing about us: every Agent starts from a full snapshot.
    if send_all(&mut socket, session.connected(), limit)
        .await
        .is_err()
    {
        return Served::ConnectionLost;
    }

    // The heartbeat: a routine report per Agent, so `sequence_num` advances and the Server's
    // liveness view stays fresh without any state change. Starts one period from now — the connect
    // snapshot just went out.
    let mut heartbeat = settings
        .heartbeat
        .map(|period| tokio::time::interval_at(tokio::time::Instant::now() + period, period));

    // A Server that sends nothing for a heartbeat interval, not even the pong to a ping sent
    // half-way, is taken for lost: frozen, without power or cut off, it closes nothing, and TCP
    // would hold the connection for many minutes
    // (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended).
    let silence = Silence::of(settings.heartbeat);
    let mut heard = tokio::time::Instant::now();
    let mut pinged = None;
    // Whether a reply has been handled on this connection, not one that throttled.
    let mut answered = false;

    loop {
        let quiet_until = silence.next(heard, pinged);
        let heartbeat_due = async {
            match heartbeat.as_mut() {
                Some(interval) => {
                    interval.tick().await;
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            incoming = socket.next() => {
                let message = match incoming {
                    Some(Ok(message)) => message,
                    // The connection was capped at the receive limit, so a Server past it arrives
                    // as an error rather than as a message. The specification's answer is the 1009
                    // close; on a merely broken connection the frame never leaves, which is fine.
                    Some(Err(e)) => {
                        warn!(error = %e, "closing the connection after a receive error");
                        let _ = socket.close(Some(too_big_close())).await;
                        return Served::ConnectionLost;
                    }
                    None => return Served::ConnectionLost,
                };
                // Anything that arrives is a sign of life, a pong as much as a message.
                heard = tokio::time::Instant::now();
                pinged = None;
                match message {
                    Message::Binary(data) => {
                        let reply: ServerToAgent = match frame::decode(&data, limit) {
                            Ok(reply) => reply,
                            // Oversized is malformed: refuse the message and close with 1009
                            // rather than act on a partial read of it.
                            Err(e @ frame::FrameError::TooLarge(..)) => {
                                warn!(error = %e, "the server sent an oversized message");
                                let _ = socket.close(Some(too_big_close())).await;
                                return Served::ConnectionLost;
                            }
                            Err(e) => {
                                warn!(error = %e, "undecodable message from the server");
                                continue;
                            }
                        };
                        if let Some(delay) = session.on_reply(&reply) {
                            // The server is throttling: drop the connection and come back after
                            // the wait it named (ADR-0045's *Context*).
                            let _ = socket.close(None).await;
                            return Served::Throttled {
                                wait: delay,
                                answered,
                            };
                        }
                        answered = true;
                        if send_all(&mut socket, session.owed(), limit).await.is_err() {
                            return Served::ConnectionLost;
                        }
                        let mut sink = FrameSink { socket: &mut socket, limit };
                        match session.after_reply(&mut sink).await {
                            AfterReply::Continue => {}
                            AfterReply::Reconnect => {
                                let _ = socket.close(None).await;
                                return Served::Reconnect;
                            }
                            // End *cleanly*: stop what the Agents run and send the goodbyes over
                            // this connection, exactly as a stop does.
                            AfterReply::End => {
                                say_goodbye(&mut socket, session, limit).await;
                                info!("disconnected; the session ended the run");
                                return Served::End;
                            }
                            AfterReply::ConnectionLost => return Served::ConnectionLost,
                        }
                    }
                    Message::Close(_) => return Served::ConnectionLost,
                    // tungstenite answers pings on the next write; text frames are not OpAMP.
                    _ => {}
                }
            }
            // Something changed that the Server should hear about: push it now, not at the next
            // heartbeat.
            () = session.changed() => {
                if send_all(&mut socket, session.owed(), limit).await.is_err() {
                    return Served::ConnectionLost;
                }
            }
            () = heartbeat_due => {
                if send_all(&mut socket, session.routine(), limit).await.is_err() {
                    return Served::ConnectionLost;
                }
            }
            () = tokio::time::sleep_until(quiet_until) => {
                if pinged.is_some() {
                    // Nothing to close gracefully: a Server that does not answer a ping would not
                    // read a close frame either.
                    warn!(
                        bound_ms = u64::try_from(silence.end_after.as_millis()).unwrap_or(u64::MAX),
                        "nothing arrived from the server within the bound; ending the connection"
                    );
                    return Served::ConnectionLost;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return Served::ConnectionLost;
                }
                pinged = Some(tokio::time::Instant::now());
            }
            () = stop.requested() => {
                say_goodbye(&mut socket, session, limit).await;
                info!("disconnected");
                return Served::Stopped;
            }
        }
    }
}

/// Ends the connection the way the specification asks: what the Agents run stops first, then the
/// final messages go out — one `agent_disconnect` per Agent — and the socket closes. Best effort,
/// because the run is ending either way.
async fn say_goodbye<S: Session>(socket: &mut Socket, session: &mut S, limit: usize) {
    session.stop().await;
    let _ = send_all(socket, session.goodbyes(), limit).await;
    let _ = socket.close(None).await;
}

/// Sends the reports, each under the send limit. A report past it is dropped with a log line
/// rather than put on the wire — the specification forbids sending one — while the connection
/// stays up; `Err` means the connection is gone, which is a different thing entirely.
async fn send_all(
    socket: &mut Socket,
    reports: Vec<AgentToServer>,
    limit: usize,
) -> Result<(), ()> {
    for report in reports {
        let framed = match frame::encode_within(&report, limit) {
            Ok(framed) => framed,
            Err(e) => {
                warn!(error = %e, "discarding a report that exceeds the size limit");
                continue;
            }
        };
        socket
            .send(Message::Binary(framed.into()))
            .await
            .map_err(|e| {
                warn!(error = %e, "cannot send a report");
            })?;
    }
    Ok(())
}

/// The close the specification names for a message past the size limit: 1009, Message Too Big.
fn too_big_close() -> CloseFrame {
    CloseFrame {
        code: CloseCode::Size,
        reason: frame::TOO_BIG_CLOSE_REASON.into(),
    }
}

/// This transport's way of putting reports on the wire, for jobs that report while they run.
struct FrameSink<'a> {
    socket: &'a mut Socket,
    limit: usize,
}

impl ReportSink for FrameSink<'_> {
    async fn send(&mut self, reports: Vec<AgentToServer>) -> Result<(), ()> {
        send_all(self.socket, reports, self.limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::accept_async;

    /// A refused upgrade waits its `Retry-After` or the next exponential step, whichever is
    /// longer, and one without the header waits the step; a connection that stood and was lost
    /// draws its first reconnect over `[0, 30 s)`.
    /// Verifies: ADR-0045
    #[test]
    fn a_refused_upgrade_waits_its_retry_after_or_the_next_step() {
        let mut backoff = Backoff::new();
        assert_eq!(
            next_delay(&mut backoff, false, Some(Duration::from_secs(7))),
            Duration::from_secs(7),
            "longer than the first step"
        );
        for _ in 0..8 {
            backoff.advance();
        }
        let step = next_delay(&mut backoff, false, Some(Duration::from_secs(2)));
        assert!(
            step >= Duration::from_secs(30),
            "the step is longer: {step:?}"
        );
        let without = next_delay(&mut backoff, false, None);
        assert!(without >= Duration::from_secs(30), "{without:?}");
        let lost = next_delay(&mut backoff, true, Some(Duration::from_secs(90)));
        assert!(lost < Backoff::AFTER_LOSS, "{lost:?}");
    }

    /// A session of one Agent that reports an empty message and asks nothing else.
    struct Quiet;

    impl Session for Quiet {
        fn connected(&mut self) -> Vec<AgentToServer> {
            vec![AgentToServer::default()]
        }
        fn routine(&mut self) -> Vec<AgentToServer> {
            Vec::new()
        }
        fn owed(&mut self) -> Vec<AgentToServer> {
            Vec::new()
        }
        fn on_reply(&mut self, _: &ServerToAgent) -> Option<Duration> {
            None
        }
        async fn after_reply<K: ReportSink>(&mut self, _: &mut K) -> AfterReply {
            AfterReply::Continue
        }
        async fn changed(&mut self) {
            std::future::pending().await
        }
        fn exchange_failed(&mut self) {}
        async fn stop(&mut self) {}
        fn goodbyes(&mut self) -> Vec<AgentToServer> {
            Vec::new()
        }
    }

    struct Never;

    impl StopSignal for Never {
        fn requested(&mut self) -> impl std::future::Future<Output = ()> + Send {
            std::future::pending()
        }
    }

    /// A connection opened from a named local address reaches the server from that address
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
    /// clause 20).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
    #[tokio::test]
    async fn a_connection_from_a_named_address_reaches_the_server_from_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("accept");
            let _socket = accept_async(stream).await.expect("handshake");
            peer
        });
        let local: IpAddr = "127.0.0.7".parse().expect("address");
        let _socket = connect_from(
            &format!("ws://localhost:{}/v1/opamp", addr.port()),
            &HeaderMap::new(),
            None,
            4096,
            Some(local),
        )
        .await
        .expect("connect");
        let peer = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("the server accepts in time")
            .expect("the server task");
        assert_eq!(peer.ip(), local, "the server sees the named address");
    }

    /// What the tracing of one test writes, kept to be read back.
    #[derive(Clone, Default)]
    struct Written(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Written {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A lost connection logs the delay it drew, and the next connection comes no earlier
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
    /// clause 20).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
    #[tokio::test]
    async fn a_lost_connection_logs_the_delay_it_drew_and_waits_it() {
        let written = Written::default();
        let log = written.clone();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(move || log.clone())
                .with_ansi(false)
                .finish(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        // The first connection is closed once its snapshot arrived; the second is when the
        // Agent came back.
        let server = async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut socket = accept_async(stream).await.expect("handshake");
            let _ = socket.next().await;
            let _ = socket.close(None).await;
            drop(socket);
            let lost = std::time::Instant::now();
            let (stream, _) = listener.accept().await.expect("accept again");
            let back = lost.elapsed();
            drop(stream);
            back
        };
        let settings = Settings {
            endpoint: format!("ws://{addr}/v1/opamp"),
            headers: HeaderMap::new(),
            connector: None,
            max_message_size: 4096,
            heartbeat: None,
        };
        let (mut quiet, mut never) = (Quiet, Never);
        let back = tokio::time::timeout(Backoff::AFTER_LOSS + Duration::from_secs(10), async {
            tokio::select! {
                back = server => back,
                _ = run(&settings, &mut quiet, &mut never) => panic!("the run ended"),
            }
        })
        .await
        .expect("the Agent comes back within the window");
        let text = String::from_utf8(written.0.lock().expect("log").clone()).expect("utf-8");
        let line = text
            .lines()
            .find(|line| line.contains("connection lost; reconnecting"))
            .unwrap_or_else(|| panic!("no reconnect line in {text}"));
        let drawn: u64 = line
            .split("delay_ms=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("no delay_ms in {line}"));
        assert!(drawn < 30_000, "drawn over [0, 30 s): {drawn}");
        assert!(
            back >= Duration::from_millis(drawn),
            "back after {back:?}, before the {drawn} ms drawn"
        );
    }

    /// A session whose Server is throttling it: every reply asks it to stay away `wait`.
    /// Each reply, in turn, asks it to stay away the wait it names, or nothing for `None`; past
    /// the list, `wait`.
    struct Throttled {
        wait: Duration,
        script: std::collections::VecDeque<Option<Duration>>,
    }

    impl Session for Throttled {
        fn connected(&mut self) -> Vec<AgentToServer> {
            vec![AgentToServer::default()]
        }
        fn routine(&mut self) -> Vec<AgentToServer> {
            Vec::new()
        }
        fn owed(&mut self) -> Vec<AgentToServer> {
            Vec::new()
        }
        fn on_reply(&mut self, _: &ServerToAgent) -> Option<Duration> {
            self.script.pop_front().unwrap_or(Some(self.wait))
        }
        async fn after_reply<K: ReportSink>(&mut self, _: &mut K) -> AfterReply {
            AfterReply::Continue
        }
        async fn changed(&mut self) {
            std::future::pending().await
        }
        fn exchange_failed(&mut self) {}
        async fn stop(&mut self) {}
        fn goodbyes(&mut self) -> Vec<AgentToServer> {
            Vec::new()
        }
    }

    /// An `Unavailable` the Client honours keeps it away the wait the Server named, or the next
    /// backoff step when that is longer, and no draw over the window after a lost connection is
    /// added on top: the Client closed the connection itself, the Server did not lose it, so the
    /// wait is the one a refused upgrade waits (ADR-0045's *Context* and clause 4's refused upgrade). Twice in a
    /// row, so a draw over `[0, 30 s)` on top cannot pass by landing low; each connection handles
    /// a reply first, so each starts the steps again.
    /// Verifies: ADR-0045
    #[tokio::test]
    async fn an_honoured_unavailable_waits_the_named_wait_and_draws_nothing_on_top() {
        const WAIT: Duration = Duration::from_secs(1);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        // Each connection's snapshot is answered twice, the second answer an `Unavailable`; the
        // time from it to the next connection is how long the Client stayed away.
        let server = async move {
            let mut away = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.expect("accept");
                let mut socket = accept_async(stream).await.expect("handshake");
                let _ = socket.next().await;
                for _ in 0..2 {
                    let reply =
                        frame::encode_within(&ServerToAgent::default(), 4096).expect("frame");
                    socket
                        .send(Message::Binary(reply.into()))
                        .await
                        .expect("reply");
                }
                let answered = std::time::Instant::now();
                let (stream, _) = listener.accept().await.expect("accept again");
                away.push(answered.elapsed());
                // The connection that came back is the next round's.
                drop(stream);
            }
            away
        };
        let settings = Settings {
            endpoint: format!("ws://{addr}/v1/opamp"),
            headers: HeaderMap::new(),
            connector: None,
            max_message_size: 4096,
            heartbeat: None,
        };
        let (mut throttled, mut never) = (
            Throttled {
                wait: WAIT,
                script: [None, Some(WAIT), None, Some(WAIT)].into_iter().collect(),
            },
            Never,
        );
        let away = tokio::time::timeout(Duration::from_secs(70), async {
            tokio::select! {
                away = server => away,
                _ = run(&settings, &mut throttled, &mut never) => panic!("the run ended"),
            }
        })
        .await
        .expect("the Client comes back twice");
        for away in away {
            assert!(
                away >= WAIT,
                "back after {away:?}, before the named {WAIT:?}"
            );
            assert!(
                away < WAIT + Duration::from_millis(1500),
                "back after {away:?}: more than the named {WAIT:?} or the first step"
            );
        }
    }

    /// Consecutive `Unavailable`s grow along the backoff: a named wait of zero, as a Server that
    /// leaves `retry_after_nanoseconds` unset sends, is not a reconnect a second for ever. A named
    /// wait longer than the step is waited, not added to it. A connection whose reply was handled
    /// before the next `Unavailable` starts the steps again (ADR-0045 clause 4).
    /// Verifies: ADR-0045
    #[tokio::test]
    async fn consecutive_unavailables_grow_along_the_backoff_until_a_reply_is_handled() {
        const NAMED: Duration = Duration::from_millis(2500);
        // Per connection, the replies it is sent; the session's script answers them in order.
        let rounds: [usize; 4] = [1, 1, 1, 2];
        let script = [
            Some(Duration::ZERO),
            Some(Duration::ZERO),
            Some(NAMED),
            None,
            Some(Duration::ZERO),
        ];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = async move {
            let mut away = Vec::new();
            let mut answered = None;
            for replies in rounds {
                let (stream, _) = listener.accept().await.expect("accept");
                if let Some(answered) = answered.take() {
                    away.push(std::time::Instant::elapsed(&answered));
                }
                let mut socket = accept_async(stream).await.expect("handshake");
                let _ = socket.next().await;
                for _ in 0..replies {
                    let reply =
                        frame::encode_within(&ServerToAgent::default(), 4096).expect("frame");
                    socket
                        .send(Message::Binary(reply.into()))
                        .await
                        .expect("reply");
                }
                answered = Some(std::time::Instant::now());
                // The Client closes the connection once it is told to stay away.
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    while let Some(Ok(_)) = socket.next().await {}
                })
                .await;
            }
            let (_, _) = listener.accept().await.expect("the last accept");
            away.push(answered.expect("answered").elapsed());
            away
        };
        let settings = Settings {
            endpoint: format!("ws://{addr}/v1/opamp"),
            headers: HeaderMap::new(),
            connector: None,
            max_message_size: 4096,
            heartbeat: None,
        };
        let (mut throttled, mut never) = (
            Throttled {
                wait: Duration::from_secs(60),
                script: script.into_iter().collect(),
            },
            Never,
        );
        let away = tokio::time::timeout(Duration::from_secs(60), async {
            tokio::select! {
                away = server => away,
                _ = run(&settings, &mut throttled, &mut never) => panic!("the run ended"),
            }
        })
        .await
        .expect("the Client comes back four times");
        let margin = Duration::from_millis(400);
        let within =
            |away: Duration, from: Duration, to: Duration| away >= from && away < to + margin;
        assert!(
            within(away[0], Duration::from_millis(500), Duration::from_secs(1)),
            "the first step: {away:?}"
        );
        assert!(
            within(away[1], Duration::from_secs(1), Duration::from_secs(2)),
            "the second step, not the first again: {away:?}"
        );
        assert!(
            within(away[2], NAMED, Duration::from_secs(4)),
            "the named wait or the third step, not their sum: {away:?}"
        );
        assert!(
            within(away[3], Duration::from_millis(500), Duration::from_secs(1)),
            "the first step again after a handled reply: {away:?}"
        );
    }

    /// The bounds follow the heartbeat: a ping after half an interval of silence, the end after a
    /// whole one; without a heartbeat, the default interval of 30 seconds
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clause 2).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[test]
    fn the_bounds_follow_the_heartbeat_and_are_thirty_seconds_without_one() {
        let thirty = Silence {
            ping_after: Duration::from_secs(15),
            end_after: Duration::from_secs(30),
        };
        assert_eq!(Silence::of(Some(Duration::from_secs(30))), thirty);
        assert_eq!(
            Silence::of(Some(Duration::from_secs(2))),
            Silence {
                ping_after: Duration::from_secs(1),
                end_after: Duration::from_secs(2),
            }
        );
        assert_eq!(Silence::of(None), thirty, "no heartbeat");
        assert_eq!(
            Silence::of(Some(Duration::ZERO)),
            thirty,
            "a heartbeat of zero is none"
        );
    }

    /// How soon after its bound a silent connection must have ended: a margin for a loaded test
    /// machine, well short of the bound's own length times ten.
    const WITHIN: Duration = Duration::from_millis(2_500);

    /// A session whose every reply keeps it busy for `busy`, reading nothing meanwhile, and which
    /// reports at every heartbeat; it counts the replies it was busy with.
    struct Busy {
        busy: Duration,
        replies: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Session for Busy {
        fn connected(&mut self) -> Vec<AgentToServer> {
            vec![AgentToServer::default()]
        }
        fn routine(&mut self) -> Vec<AgentToServer> {
            vec![AgentToServer::default()]
        }
        fn owed(&mut self) -> Vec<AgentToServer> {
            Vec::new()
        }
        fn on_reply(&mut self, _: &ServerToAgent) -> Option<Duration> {
            None
        }
        async fn after_reply<K: ReportSink>(&mut self, _: &mut K) -> AfterReply {
            tokio::time::sleep(self.busy).await;
            self.replies
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            AfterReply::Continue
        }
        async fn changed(&mut self) {
            std::future::pending().await
        }
        fn exchange_failed(&mut self) {}
        async fn stop(&mut self) {}
        fn goodbyes(&mut self) -> Vec<AgentToServer> {
            Vec::new()
        }
    }

    /// A session busy with each reply for longer than an interval keeps a connection to a server
    /// that answers everything: its own delay is not the server's silence
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clause 1).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[tokio::test]
    async fn a_session_busy_for_longer_than_an_interval_keeps_a_live_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        // Answers every report and, as it reads, every ping.
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut socket = accept_async(stream).await.expect("handshake");
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Binary(_)) {
                    let reply = frame::encode_within(&ServerToAgent::default(), 4096)
                        .expect("within the limit");
                    if socket.send(Message::Binary(reply.into())).await.is_err() {
                        return;
                    }
                }
            }
        });
        let heartbeat = Duration::from_millis(200);
        let settings = settings_for(addr, heartbeat);
        let (socket, _) = tokio_tungstenite::connect_async(&settings.endpoint)
            .await
            .expect("connect");
        let replies = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut busy = Busy {
            busy: heartbeat * 3,
            replies: replies.clone(),
        };
        let mut never = Never;
        let counted = replies.clone();
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::select! {
                () = async {
                    while counted.load(std::sync::atomic::Ordering::SeqCst) < 5 {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                } => {}
                _ = serve(socket, &settings, &mut busy, &mut never) => {
                    panic!(
                        "the connection ended after {} busy replies",
                        replies.load(std::sync::atomic::Ordering::SeqCst)
                    )
                }
            }
        })
        .await
        .expect("five busy replies in time");
    }

    /// The exact window: without a ping the next act is the ping, half an interval after the last
    /// sign of life; with one, the end comes a whole interval after the last sign of life, or the
    /// rest of the interval after the ping when the ping went out late
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clause 1).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[test]
    fn the_pong_is_given_the_rest_of_the_interval_from_its_ping() {
        let silence = Silence::of(Some(Duration::from_secs(30)));
        let heard = tokio::time::Instant::now();
        let at = |secs: u64| heard + Duration::from_secs(secs);
        assert_eq!(silence.next(heard, None), at(15), "the ping half-way");
        assert_eq!(
            silence.next(heard, Some(at(15))),
            at(30),
            "a ping in time: one interval"
        );
        assert_eq!(
            silence.next(heard, Some(at(20))),
            at(35),
            "a ping five seconds late: the rest of the interval from it"
        );
        assert_eq!(
            silence.next(heard, Some(at(90))),
            at(105),
            "after a busy minute: fifteen seconds for the pong"
        );
    }

    /// Settings for a test server at `addr` with a heartbeat of `heartbeat`.
    fn settings_for(addr: std::net::SocketAddr, heartbeat: Duration) -> Settings {
        Settings {
            endpoint: format!("ws://{addr}/v1/opamp"),
            headers: HeaderMap::new(),
            connector: None,
            max_message_size: 4096,
            heartbeat: Some(heartbeat),
        }
    }

    /// A server that completes the upgrade and then reads nothing, as a frozen process does: its
    /// kernel holds the connection, and no pong ever comes. The socket is kept until the test
    /// ends.
    async fn frozen_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let held = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _socket = accept_async(stream).await.expect("handshake");
            std::future::pending::<()>().await;
        });
        (addr, held)
    }

    /// A connection on which nothing arrives for one heartbeat interval is ended as a lost one,
    /// with a log line naming the bound, and no sooner than the bound
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clauses 1,
    /// 4).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[tokio::test]
    async fn a_connection_on_which_nothing_arrives_for_a_heartbeat_interval_is_ended() {
        let written = Written::default();
        let log = written.clone();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(move || log.clone())
                .with_ansi(false)
                .finish(),
        );
        let (addr, _held) = frozen_server().await;
        let heartbeat = Duration::from_millis(500);
        let settings = settings_for(addr, heartbeat);
        let (socket, _) = tokio_tungstenite::connect_async(&settings.endpoint)
            .await
            .expect("connect");
        let opened = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            serve(socket, &settings, &mut Quiet, &mut Never),
        )
        .await
        .expect("the connection is ended within the test's bound");
        assert!(matches!(outcome, Served::ConnectionLost), "ended as lost");
        let ended = opened.elapsed();
        assert!(ended >= heartbeat, "not before the bound: {ended:?}");
        assert!(
            ended < heartbeat + WITHIN,
            "once the bound passes, within {WITHIN:?} of it: {ended:?}"
        );
        let text = String::from_utf8(written.0.lock().expect("log").clone()).expect("utf-8");
        assert!(
            text.lines()
                .any(|line| line.contains("nothing arrived") && line.contains("bound_ms=500")),
            "the end is logged with its bound: {text}"
        );
    }

    /// A server that answers pings and sends nothing else keeps the connection over many bounds:
    /// the pongs are signs of life
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clauses 1,
    /// 2).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[tokio::test]
    async fn a_server_that_answers_only_pings_keeps_the_connection() {
        const PINGS: usize = 8;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        // Reads everything, so the library answers each ping; reports when it has seen enough.
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut socket = accept_async(stream).await.expect("handshake");
            let mut pings = 0;
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Ping(_)) {
                    pings += 1;
                    if pings == PINGS {
                        return Some(socket);
                    }
                }
            }
            None
        });
        let heartbeat = Duration::from_millis(200);
        let settings = settings_for(addr, heartbeat);
        let (socket, _) = tokio_tungstenite::connect_async(&settings.endpoint)
            .await
            .expect("connect");
        let opened = tokio::time::Instant::now();
        let (mut quiet, mut never) = (Quiet, Never);
        let seen = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                seen = server => seen.expect("the server task"),
                _ = serve(socket, &settings, &mut quiet, &mut never) => {
                    panic!("the connection ended after {:?}", opened.elapsed())
                }
            }
        })
        .await
        .expect("the server sees its pings in time");
        assert!(seen.is_some(), "the connection stood through {PINGS} pings");
        assert!(
            opened.elapsed() >= heartbeat * 2,
            "over more than one bound: {:?}",
            opened.elapsed()
        );
    }

    /// An upgrade that is never answered — a TCP connection accepted by a kernel whose process
    /// is frozen — fails once the bound has passed
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clause 3).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[tokio::test]
    async fn an_upgrade_that_is_never_answered_fails_within_the_bound() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let _held = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("accept");
            std::future::pending::<()>().await;
        });
        let heartbeat = Duration::from_millis(500);
        let settings = settings_for(addr, heartbeat);
        let started = tokio::time::Instant::now();
        let opened = tokio::time::timeout(Duration::from_secs(10), open(&settings, None))
            .await
            .expect("the attempt fails within the test's bound");
        assert!(opened.is_err(), "an unanswered upgrade is a failed attempt");
        let failed = started.elapsed();
        assert!(failed >= heartbeat, "not before the bound: {failed:?}");
        assert!(
            failed < heartbeat + WITHIN,
            "once the bound passes, within {WITHIN:?} of it: {failed:?}"
        );
    }

    /// A name whose first address accepts a TCP connection and never answers, as a frozen
    /// process's kernel does, still connects: that address gives way to the next once its own
    /// interval has passed, and the upgrade to the next carries the name, not the address
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    /// clause 3).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[tokio::test]
    async fn an_address_not_reached_within_its_interval_gives_way_to_the_next() {
        // The kernel completes the TCP connection into the backlog; nothing ever answers it.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addresses = vec![
            silent.local_addr().expect("addr"),
            listener.local_addr().expect("addr"),
        ];
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut host = None;
            // The callback's error type is tungstenite's, large as it is.
            #[allow(clippy::result_large_err)]
            let record = |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          response| {
                host = request
                    .headers()
                    .get("host")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                Ok(response)
            };
            let socket = tokio_tungstenite::accept_hdr_async(stream, record)
                .await
                .expect("handshake");
            (socket, host)
        });
        let interval = Duration::from_millis(500);
        let mut settings = settings_for(addresses[0], interval);
        settings.endpoint = "ws://fleet.example:4320/v1/opamp".to_string();
        let started = tokio::time::Instant::now();
        let opened = tokio::time::timeout(
            Duration::from_secs(10),
            open_each(&settings, None, addresses, interval),
        )
        .await
        .expect("the attempt ends within the test's bound");
        let took = started.elapsed();
        assert!(opened.is_ok(), "the second address opens: {opened:?}");
        assert!(
            took >= interval,
            "the first address had its interval: {took:?}"
        );
        assert!(
            took < interval + WITHIN,
            "and the second was tried right after it: {took:?}"
        );
        let (_socket, host) = server.await.expect("the server task");
        assert_eq!(
            host.as_deref(),
            Some("fleet.example:4320"),
            "the name is the host"
        );
    }

    /// Opens with [`open_each`] to two addresses: the first answers the upgrade request with
    /// `answer`, the second is never accepted, so a connection to it waits in its backlog. Gives
    /// back the attempt's outcome and whether the second address was tried.
    async fn first_answers(answer: &'static [u8]) -> (Result<Socket, Connect>, bool) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let answering = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let next = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        next.set_nonblocking(true).expect("non-blocking");
        let addresses = vec![
            answering.local_addr().expect("addr"),
            next.local_addr().expect("addr"),
        ];
        let _answer = tokio::spawn(async move {
            let (mut stream, _) = answering.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.expect("read");
                assert!(read > 0, "the request ends before its headers do");
                request.extend_from_slice(&buffer[..read]);
            }
            stream.write_all(answer).await.expect("answer");
            std::future::pending::<()>().await;
        });
        let interval = Duration::from_secs(2);
        let settings = settings_for(addresses[0], interval);
        let opened = tokio::time::timeout(
            Duration::from_secs(10),
            open_each(&settings, None, addresses, interval),
        )
        .await
        .expect("the attempt ends within the test's bound");
        let tried = match next.accept() {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(e) => panic!("accept: {e}"),
        };
        (opened, tried)
    }

    /// An HTTP answer to the upgrade is the Server's verdict, not an address that was not
    /// reached: a refusal on the first address ends the attempt as it is, and the next address is
    /// not tried
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clause 3).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[tokio::test]
    async fn an_http_answer_on_the_first_address_ends_the_attempt() {
        let (opened, tried) =
            first_answers(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n").await;
        match opened {
            Err(Connect::Refused(WsError::Http(response))) => {
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            }
            other => panic!("the refusal comes back as it is: {other:?}"),
        }
        assert!(!tried, "the next address was not tried");
    }

    /// An address that answers the upgrade with something that is not HTTP was reached all the
    /// same: its answer ends the attempt, and the next address is not tried
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clause 3).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[tokio::test]
    async fn an_answer_that_is_no_http_on_the_first_address_ends_the_attempt() {
        let (opened, tried) = first_answers(b"this is no HTTP at all\r\n\r\n").await;
        match opened {
            Err(Connect::Refused(WsError::Io(_) | WsError::Tls(_))) | Ok(_) => {
                panic!("the garbled answer comes back as it is: {opened:?}")
            }
            Err(_) => {}
        }
        assert!(!tried, "the next address was not tried");
    }

    /// A first address that refuses the connection, as `::1` does to a Server listening on
    /// `127.0.0.1` only, gives way to the next at once
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clause 3).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[tokio::test]
    async fn a_first_address_that_refuses_gives_way_to_the_next() {
        // A port that was bound and is free again: nothing listens on it, so it refuses.
        let refusing = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addresses = vec![refusing, listener.local_addr().expect("addr")];
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            accept_async(stream).await.expect("handshake")
        });
        let interval = Duration::from_secs(5);
        let settings = settings_for(addresses[1], interval);
        let started = tokio::time::Instant::now();
        let opened = tokio::time::timeout(
            Duration::from_secs(10),
            open_each(&settings, None, addresses, interval),
        )
        .await
        .expect("the attempt ends within the test's bound");
        assert!(opened.is_ok(), "the second address opens: {opened:?}");
        assert!(
            started.elapsed() < interval,
            "without waiting out the first address's interval: {:?}",
            started.elapsed()
        );
        server.await.expect("the server task");
    }

    /// A `wss` endpoint opened to an address carries the endpoint's name as the TLS server name,
    /// so the certificate is checked against the name, not the address
    /// (ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended clause 3).
    /// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
    #[tokio::test]
    async fn the_tls_server_name_is_the_endpoints_name() {
        crate::tls::install_ring_provider();
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec!["fleet.example".to_string()])
            .expect("params")
            .self_signed(&key)
            .expect("certificate");
        let server_config = crate::tls::server_builder()
            .with_no_client_auth()
            .with_single_cert(
                crate::tls::certificates(cert.pem().as_bytes()).expect("certificate"),
                crate::tls::private_key(key.serialize_pem().as_bytes()).expect("key"),
            )
            .expect("server configuration");
        let client_config = crate::tls::client_builder()
            .with_root_certificates(crate::tls::root_store(cert.pem().as_bytes()).expect("roots"))
            .with_no_client_auth();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        // A blocking TLS server that completes the upgrade and reports the name the client sent.
        let server = tokio::task::spawn_blocking(move || {
            let (tcp, _) = listener.accept().expect("accept");
            let connection =
                rustls::ServerConnection::new(std::sync::Arc::new(server_config)).expect("tls");
            let socket =
                tokio_tungstenite::tungstenite::accept(rustls::StreamOwned::new(connection, tcp))
                    .expect("handshake");
            socket.get_ref().conn.server_name().map(str::to_string)
        });
        let settings = Settings {
            endpoint: format!("wss://fleet.example:{}/v1/opamp", address.port()),
            headers: HeaderMap::new(),
            connector: Some(Connector::Rustls(std::sync::Arc::new(client_config))),
            max_message_size: 4096,
            heartbeat: Some(Duration::from_secs(5)),
        };
        let opened = tokio::time::timeout(
            Duration::from_secs(10),
            open_each(&settings, None, vec![address], Duration::from_secs(5)),
        )
        .await
        .expect("the attempt ends within the test's bound");
        assert!(
            opened.is_ok(),
            "the certificate for the name is accepted: {opened:?}"
        );
        let name = server.await.expect("the server task");
        assert_eq!(
            name.as_deref(),
            Some("fleet.example"),
            "the server name is the name"
        );
    }

    /// The specification: a `ServerToAgent` past the receive limit is malformed — the Agent refuses
    /// it and closes with 1009 rather than acting on it.
    #[tokio::test]
    async fn an_oversized_message_from_the_server_closes_with_1009() {
        const LIMIT: usize = 4096;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        // A Server that answers the connect snapshot with a message twice the Agent's limit, then
        // waits for what the Agent does about it.
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut socket = accept_async(stream).await.expect("handshake");
            // The full report on connect; ignored beyond keeping the stream moving.
            let _ = socket.next().await;
            socket
                .send(Message::Binary(vec![0u8; LIMIT * 2].into()))
                .await
                .expect("send an oversized message");
            // The close frame is the answer under test.
            loop {
                match socket.next().await {
                    Some(Ok(Message::Close(frame))) => return frame,
                    Some(Ok(_)) => continue,
                    _ => return None,
                }
            }
        });

        let settings = Settings {
            endpoint: format!("ws://{addr}/v1/opamp"),
            headers: HeaderMap::new(),
            connector: None,
            max_message_size: LIMIT,
            heartbeat: None,
        };
        let (socket, _) = tokio_tungstenite::connect_async(&settings.endpoint)
            .await
            .expect("connect");

        let outcome = serve(socket, &settings, &mut Quiet, &mut Never).await;
        assert!(
            matches!(outcome, Served::ConnectionLost),
            "an oversized message ends the connection"
        );

        let close = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("the server sees the close in time")
            .expect("the server task")
            .expect("a close frame with a status code");
        assert_eq!(
            u16::from(close.code),
            1009,
            "the specification names 1009 (Message Too Big)"
        );
    }
}
