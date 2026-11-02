//! The listener an OpAMP endpoint is served on (ADR-0008): TLS from the material the application
//! hands it, the bounds on connection setup of ADR-0012, and what the handshake learned carried
//! into every request.
//!
//! Every other limit an endpoint enforces starts at a request. A peer that completes the TCP
//! connection, sends a request line and falls silent reaches none of them, and would hold a task and
//! hyper's read buffer for as long as it likes. hyper defaults its HTTP/1 `header_read_timeout` to
//! 30 seconds, but the default is inert while no `Timer` is installed, and neither `axum::serve`
//! nor `axum_server` installs one. Configuring the timeout without a timer is worse: hyper panics.
//! So [`Listener::serve`] installs both, and every OpAMP listener goes through it.
//!
//! The bound is on **connection setup**, not on a request. A WebSocket session is long-lived and a
//! package download is as large as its artifact; a request timeout would break exactly those and
//! leave the slow peer untouched.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::time::Instant;

use axum::extract::ConnectInfo;
use axum::{Extension, Router};
use axum_server::accept::Accept;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use axum_server::Server;
use hyper_util::rt::TokioTimer;
use rustls::pki_types::CertificateDer;
use rustls::server::WebPkiClientVerifier;
use rustls::ServerConfig;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use super::pace::{self, Pace, MIN_PACE_BYTES, PACE_WINDOW};
use crate::endpoint::is_loopback_literal;
use crate::tls::{certificates, private_key, provider, root_store, server_builder, Identity};

/// How long a connection may take to send its request line and headers: hyper's own default,
/// with the timer that makes it take effect. It bounds the request line and the headers only.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The connections a listener holds at once unless the application states another cap
/// ([`Listener::with_max_connections`]).
pub const DEFAULT_MAX_CONNECTIONS: usize = 10_000;

/// The HTTP/2 streams one connection may have open at once (ADR-0012).
pub const H2_MAX_CONCURRENT_STREAMS: u32 = 100;

/// How often an HTTP/2 connection is pinged (ADR-0012).
pub const H2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// How long a ping may go unanswered before the HTTP/2 connection is dropped (ADR-0012).
pub const H2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the TLS handshake may take before the connection is dropped — `axum_server`'s default,
/// stated so that every listener visibly has one.
pub const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Shuts listeners down. One handle may drain several; how long a drain may take is the caller's
/// to state (ADR-0008 clause 6).
///
/// It is `axum_server`'s handle plus what that one cannot reach: the WebSocket sessions, which
/// outlive the HTTP connection they were upgraded from and so are not counted by its drain. A
/// shutdown tells them at once, and the same deadline bounds them (ADR-0012 clause 17).
#[derive(Clone, Debug)]
pub struct Handle {
    server: axum_server::Handle<SocketAddr>,
    /// `None` while serving; once shutting down, the deadline — `None` for a drain without one.
    shutdown: Arc<tokio::sync::watch::Sender<Option<Option<Instant>>>>,
    /// Until when the WebSocket sessions are told to close, each at an instant of its own drawn
    /// over the window; `None` tells them all at once.
    spread: Arc<std::sync::Mutex<Option<Instant>>>,
}

/// What a spread drain leaves after its window for the last sessions' close frames, before what is
/// still open is cut.
pub const SPREAD_GRACE: Duration = Duration::from_secs(2);

impl Default for Handle {
    fn default() -> Self {
        Self::new()
    }
}

impl Handle {
    /// A handle for listeners that have not started yet.
    #[must_use]
    pub fn new() -> Self {
        Handle {
            server: axum_server::Handle::new(),
            shutdown: Arc::new(tokio::sync::watch::channel(None).0),
            spread: Arc::default(),
        }
    }

    /// Stops accepting, and gives what is open `duration` to end — every connection still serving a
    /// request, and every WebSocket session, which is told to close now. `None` waits for as long
    /// as they take. Whatever is open at the deadline is cut before `serve` returns.
    pub fn graceful_shutdown(&self, duration: Option<Duration>) {
        self.shutdown
            .send_replace(Some(duration.map(|d| Instant::now() + d)));
        self.server.graceful_shutdown(duration);
    }

    /// Stops accepting at once and tells each WebSocket session to close at an instant drawn
    /// uniformly over `over`
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 3), so the
    /// peers come back spread rather than all at once; what is still open [`SPREAD_GRACE`] after
    /// the window is cut. A window of zero tells them all at once.
    pub fn drain_spread(&self, over: Duration) {
        *self.spread.lock().expect("spread lock") =
            (!over.is_zero()).then(|| Instant::now() + over);
        self.graceful_shutdown(Some(over + SPREAD_GRACE));
    }

    /// Stops accepting and cuts everything open at once.
    pub fn shutdown(&self) {
        self.shutdown.send_replace(Some(Some(Instant::now())));
        self.server.shutdown();
    }

    /// The HTTP connections currently open; upgraded WebSocket sessions are not among them.
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.server.connection_count()
    }

    /// The address the listener is bound to, once it serves; `None` if it ended first.
    pub async fn listening(&self) -> Option<SocketAddr> {
        self.server.listening().await
    }
}

/// The WebSocket sessions one listener has upgraded to: the signal that tells them to close, and
/// their tasks, so a shutdown can wait for them and cut what outlives its deadline.
#[derive(Clone)]
pub(crate) struct Sessions {
    shutdown: tokio::sync::watch::Receiver<Option<Option<Instant>>>,
    spread: Arc<std::sync::Mutex<Option<Instant>>>,
    /// This session's instant to close in a spread drain, drawn once: the loop that waits for it
    /// asks again after every message, and a draw each time would put it off for as long as the
    /// peer keeps sending.
    turn: Option<Instant>,
    tasks: Arc<std::sync::Mutex<tokio::task::JoinSet<()>>>,
}

impl Sessions {
    /// Runs one session as a task of this listener's.
    pub(crate) fn spawn(&self, session: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks.lock().expect("sessions lock");
        // Ended sessions are reaped here, so a long-lived listener holds no record of them.
        while tasks.try_join_next().is_some() {}
        tasks.spawn(session);
    }

    /// Resolves once the listener is shutting down and this session's turn to close has come: at
    /// once, or at an instant drawn over the window of a spread drain.
    pub(crate) async fn closing(&mut self) {
        let _ = self.shutdown.wait_for(Option::is_some).await;
        let turn = *self.turn.get_or_insert_with(|| {
            let now = Instant::now();
            let until = *self.spread.lock().expect("spread lock");
            until.map_or(now, |until| {
                let window = until.saturating_duration_since(now);
                let nanos = u64::try_from(window.as_nanos()).unwrap_or(u64::MAX);
                now + Duration::from_nanos(crate::random::below(nanos))
            })
        });
        tokio::time::sleep_until(turn).await;
    }
}

/// Whether a peer has to present a client certificate, and the CA it is verified against.
#[derive(Clone, Debug)]
pub enum ClientAuth {
    /// Server-authenticated TLS only.
    None,
    /// A certificate the peer offers must verify, but the handshake succeeds without one. The
    /// application decides per route whether to require it, through [`PeerCertificate`]. A
    /// listener that also serves a route reached without a certificate needs this.
    Optional { ca_pem: Vec<u8> },
    /// The handshake fails without a certificate that verifies. For a listener that speaks only
    /// OpAMP, where the CA is an access-control boundary.
    Required { ca_pem: Vec<u8> },
}

/// What a listener serves TLS with.
#[derive(Clone, Debug)]
pub struct ServerTls {
    /// The certificate chain and key this listener presents.
    pub identity: Identity,
    pub client_auth: ClientAuth,
}

impl ServerTls {
    /// The rustls configuration for this material, with HTTP/2 and HTTP/1.1 offered by ALPN.
    ///
    /// # Errors
    /// Returns an error naming the part — certificate, key, or client CA — that cannot be used.
    pub fn rustls_config(&self) -> Result<Arc<ServerConfig>, String> {
        let certs = certificates(&self.identity.cert_pem)
            .map_err(|e| format!("the TLS certificate: {e}"))?;
        let key = private_key(&self.identity.key_pem).map_err(|e| format!("the TLS key: {e}"))?;
        let builder = match &self.client_auth {
            ClientAuth::None => server_builder().with_no_client_auth(),
            ClientAuth::Optional { ca_pem } | ClientAuth::Required { ca_pem } => {
                let roots = root_store(ca_pem).map_err(|e| format!("the client CA: {e}"))?;
                let verifier = WebPkiClientVerifier::builder_with_provider(
                    Arc::new(roots),
                    Arc::new(provider()),
                );
                let verifier = if matches!(self.client_auth, ClientAuth::Optional { .. }) {
                    verifier.allow_unauthenticated()
                } else {
                    verifier
                };
                let verifier = verifier
                    .build()
                    .map_err(|e| format!("cannot build the client verifier: {e}"))?;
                server_builder().with_client_cert_verifier(verifier)
            }
        };
        let mut config = builder
            .with_single_cert(certs, key)
            .map_err(|e| format!("cannot use the TLS certificate and key: {e}"))?;
        // `RustlsConfig::from_config` leaves ALPN to the caller, unlike the from-PEM constructors,
        // and without it every HTTP/2 client fails the negotiation.
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }
}

/// What the TLS handshake proved about the peer, in every request on that connection.
///
/// `None` means the peer presented no certificate, or the listener serves no TLS. A certificate
/// that is present has been verified against the client CA: rustls refuses a bad one during the
/// handshake, so this type never carries an unverified certificate. It proves membership of
/// whoever holds the CA's certificates, never which Agent is speaking.
#[derive(Clone, Debug, Default)]
pub struct PeerCertificate(pub Option<CertificateDer<'static>>);

impl PeerCertificate {
    #[must_use]
    pub fn present(&self) -> bool {
        self.0.is_some()
    }
}

/// A bound TCP listener, ready to serve a router.
pub struct Listener {
    listener: std::net::TcpListener,
    handle: Handle,
    tls: Option<Arc<ServerConfig>>,
    header_read_timeout: Duration,
    max_connections: usize,
    pace: (Duration, u64),
    max_new_per_sec: Option<u32>,
}

impl Listener {
    /// A plain listener. `handle` drains it on shutdown, and one handle may drain several.
    #[must_use]
    pub fn new(listener: std::net::TcpListener, handle: Handle) -> Self {
        Listener {
            listener,
            handle,
            tls: None,
            header_read_timeout: HEADER_READ_TIMEOUT,
            pace: (PACE_WINDOW, MIN_PACE_BYTES),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_new_per_sec: None,
        }
    }

    /// Serves TLS with `config`, usually built by [`ServerTls::rustls_config`].
    #[must_use]
    pub fn with_tls(mut self, config: Arc<ServerConfig>) -> Self {
        self.tls = Some(config);
        self
    }

    /// Replaces [`HEADER_READ_TIMEOUT`] — the seam a test drives, since nothing else makes a
    /// 30-second bound observable in a suite.
    #[must_use]
    pub fn with_header_read_timeout(mut self, timeout: Duration) -> Self {
        self.header_read_timeout = timeout;
        self
    }

    /// Sets the floor on the pace of bodies and messages, [`PACE_WINDOW`] and [`MIN_PACE_BYTES`]
    /// unless stated. For a test to drive it short: the floor is no setting (ADR-0012 clause 18).
    #[doc(hidden)]
    #[must_use]
    pub fn with_pace(mut self, window: Duration, min_bytes: u64) -> Self {
        self.pace = (window, min_bytes);
        self
    }

    /// Caps the connections held at once. A connection past the cap is closed on accept, before
    /// any TLS handshake, and the ones already held keep working (ADR-0012).
    #[must_use]
    pub fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
        self
    }

    /// Takes at most `rate` new connections a second
    /// (ADR-draft-tls-1-3-two-planes-and-bounded-input clause 4). A connection past it is served
    /// nothing but `503` with a `Retry-After` of whole seconds drawn from 1 to 30, so a fleet that
    /// comes back at once is spread rather than turned away together; the connections already
    /// held keep working. Without it, a listener takes new connections as fast as they come; a rate
    /// of `0` throttles every one, which an application refuses before it gets here.
    #[must_use]
    pub fn with_max_new_connections_per_sec(mut self, rate: u32) -> Self {
        self.max_new_per_sec = Some(rate);
        self
    }

    /// Serves `router` until the handle shuts the listener down. Every request carries the peer's
    /// address as `ConnectInfo<SocketAddr>` and the handshake's [`PeerCertificate`].
    ///
    /// # Errors
    /// Returns the I/O error that stopped the listener, and refuses a listener without TLS on any
    /// address but `127.0.0.1` or `::1`.
    pub async fn serve(self, router: Router) -> io::Result<()> {
        // Plaintext is for the loopback alone (specification Q-1): a listener reachable from the
        // network without TLS is refused before it accepts anything.
        if self.tls.is_none() {
            let address = self.listener.local_addr()?;
            if !is_loopback_literal(&address.ip().to_string()) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("refusing to serve {address} without TLS: plaintext is for the loopback alone"),
                ));
            }
        }
        // The runtime polls the listener; a blocking one would stall the worker it accepts on.
        self.listener.set_nonblocking(true)?;
        let server = axum_server::from_tcp(self.listener)?.handle(self.handle.server.clone());
        let slots = Slots::new(self.max_connections, self.pace, self.max_new_per_sec);
        let sessions = Sessions {
            shutdown: self.handle.shutdown.subscribe(),
            spread: self.handle.spread.clone(),
            turn: None,
            tasks: Arc::default(),
        };
        let router = router
            .layer(axum::middleware::from_fn(pace::bodies))
            .layer(Extension(sessions.clone()));
        let served = match self.tls {
            None => {
                bounded(server.acceptor(Plain(slots)), self.header_read_timeout)
                    .serve(router.into_make_service())
                    .await
            }
            Some(config) => {
                let acceptor = |config: Arc<ServerConfig>| {
                    RustlsAcceptor::new(RustlsConfig::from_config(config))
                        .handshake_timeout(TLS_HANDSHAKE_TIMEOUT)
                };
                // A connection past the rate is offered HTTP/1.1 alone: HTTP/2 has no
                // `Connection: close`, and a pooled HTTP/2 connection would be answered `503` for
                // as long as it lived.
                let mut throttled = (*config).clone();
                throttled.alpn_protocols = vec![b"http/1.1".to_vec()];
                bounded(
                    server.acceptor(Tls {
                        served: acceptor(config),
                        throttled: acceptor(Arc::new(throttled)),
                        slots,
                    }),
                    self.header_read_timeout,
                )
                .serve(router.into_make_service())
                .await
            }
        };
        // HTTP has drained. The sessions were told when the shutdown began; what is still open at
        // its deadline is cut here, so nothing a session does outlives `serve`. A listener that
        // stopped for another reason cuts its own sessions at once, and leaves the handle — which
        // other listeners may share — alone.
        let deadline = (*self.handle.shutdown.borrow()).unwrap_or(Some(Instant::now()));
        let mut tasks = std::mem::take(&mut *sessions.tasks.lock().expect("sessions lock"));
        let ended = async { while tasks.join_next().await.is_some() {} };
        match deadline {
            Some(deadline) => {
                let _ = tokio::time::timeout_at(deadline, ended).await;
            }
            None => ended.await,
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        served
    }
}

/// The header-read bound, with the timer it needs first: without the timer hyper discards the
/// default, and with the timeout set but no timer it panics. HTTP/2 has no header phase to time,
/// so it is bounded by its streams and its pings instead (ADR-0012).
fn bounded<A>(
    mut server: Server<SocketAddr, A>,
    header_read_timeout: Duration,
) -> Server<SocketAddr, A> {
    let builder = server.http_builder();
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout);
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_concurrent_streams(H2_MAX_CONCURRENT_STREAMS)
        .keep_alive_interval(H2_KEEP_ALIVE_INTERVAL)
        .keep_alive_timeout(H2_KEEP_ALIVE_TIMEOUT);
    server
}

/// The connections a listener holds, against its cap, and the new ones it takes a second.
#[derive(Clone)]
struct Slots {
    held: Arc<AtomicUsize>,
    max: usize,
    pace: (Duration, u64),
    new: Option<Arc<std::sync::Mutex<Bucket>>>,
}

/// The new connections a listener may still take: `rate` a second, at most `rate` at once.
struct Bucket {
    rate: f64,
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn new(rate: u32) -> Self {
        Bucket {
            rate: f64::from(rate),
            tokens: f64::from(rate),
            at: Instant::now(),
        }
    }

    /// Whether one more new connection is taken now.
    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.rate);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl Slots {
    fn new(max: usize, pace: (Duration, u64), max_new_per_sec: Option<u32>) -> Self {
        Slots {
            held: Arc::new(AtomicUsize::new(0)),
            max,
            pace,
            new: max_new_per_sec.map(|rate| Arc::new(std::sync::Mutex::new(Bucket::new(rate)))),
        }
    }

    /// Whether a new connection is within the rate; always, without one.
    fn within_rate(&self) -> bool {
        self.new.as_ref().is_none_or(|bucket| {
            bucket
                .lock()
                .expect("new connections lock")
                .take(Instant::now())
        })
    }

    /// One slot for `stream`, or `None` when the listener is full and the stream is to be closed.
    fn take(&self, stream: TcpStream) -> Option<Counted> {
        let held = self.held.fetch_add(1, Ordering::AcqRel);
        if held >= self.max {
            self.held.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Counted {
            stream,
            held: self.held.clone(),
        })
    }

    /// The floor every connection of this listener is held to.
    fn pace(&self) -> Pace {
        Pace {
            window: self.pace.0,
            min_bytes: self.pace.1,
        }
    }
}

fn full() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionRefused,
        "the listener is at its connection cap",
    )
}

/// A stream that holds one slot of its listener until it is dropped.
struct Counted {
    stream: TcpStream,
    held: Arc<AtomicUsize>,
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.held.fetch_sub(1, Ordering::AcqRel);
    }
}

impl AsyncRead for Counted {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Counted {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
}

/// What a connection past the rate of new ones is served: `503` to every request, with a
/// `Retry-After` drawn from 1 to 30 seconds, and the connection closed after it.
fn throttled() -> Router {
    Router::new().fallback(|| async {
        let seconds = 1 + crate::random::below(30);
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            [
                (axum::http::header::RETRY_AFTER, seconds.to_string()),
                (axum::http::header::CONNECTION, "close".to_string()),
            ],
        )
    })
}

/// The per-connection extensions, attached to the router that serves the connection. The router is
/// what `into_make_service` hands an acceptor, and `Router::layer` attaches an extension without a
/// direct dependency on `tower`.
fn with_connection(
    service: Router,
    peer: Option<SocketAddr>,
    certificate: Option<CertificateDer<'static>>,
    pace: Pace,
) -> Router {
    let service = service
        .layer(Extension(PeerCertificate(certificate)))
        .layer(Extension(pace));
    match peer {
        Some(peer) => service.layer(Extension(ConnectInfo(peer))),
        None => service,
    }
}

#[derive(Clone)]
struct Plain(Slots);

impl Accept<TcpStream, Router> for Plain {
    type Stream = Counted;
    type Service = Router;
    type Future = std::future::Ready<io::Result<(Counted, Router)>>;

    fn accept(&self, stream: TcpStream, service: Router) -> Self::Future {
        let peer = stream.peer_addr().ok();
        // A connection the cap closes takes no place in the rate.
        std::future::ready(match self.0.take(stream) {
            Some(stream) => {
                let service = if self.0.within_rate() {
                    service
                } else {
                    throttled()
                };
                Ok((stream, with_connection(service, peer, None, self.0.pace())))
            }
            None => Err(full()),
        })
    }
}

#[derive(Clone)]
struct Tls {
    /// What a connection within the rate is served over.
    served: RustlsAcceptor,
    /// What a connection past it is served over: HTTP/1.1 alone.
    throttled: RustlsAcceptor,
    slots: Slots,
}

impl Accept<TcpStream, Router> for Tls {
    type Stream = <RustlsAcceptor as Accept<Counted, Router>>::Stream;
    type Service = Router;
    type Future = Pin<Box<dyn Future<Output = io::Result<(Self::Stream, Router)>> + Send>>;

    fn accept(&self, stream: TcpStream, service: Router) -> Self::Future {
        let peer = stream.peer_addr().ok();
        // The slot is taken before the handshake: a peer that never completes one still counts.
        let counted = self.slots.take(stream);
        // So is its place in the rate of new connections, unless the cap closes it; one past the
        // rate completes the handshake and is answered `503`, so the peer learns when to come back.
        let within = counted.is_some() && self.slots.within_rate();
        let (inner, service) = if within {
            (self.served.clone(), service)
        } else {
            (self.throttled.clone(), throttled())
        };
        let pace = self.slots.pace();
        Box::pin(async move {
            let stream = counted.ok_or_else(full)?;
            let (stream, service) = inner.accept(stream, service).await?;
            // What is here was verified against the client CA: rustls completed the handshake, and
            // a certificate it could not chain never gets this far.
            let certificate = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|chain| chain.first().cloned());
            Ok((stream, with_connection(service, peer, certificate, pace)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bucket of 200 a second takes exactly 200 new connections at one instant and refuses the
    /// rest, then takes 10 more after 50 ms: the rate, without the timing of a real listener.
    /// Verifies: ADR-draft-tls-1-3-two-planes-and-bounded-input
    #[test]
    fn the_bucket_takes_its_rate_and_refills_at_it() {
        let mut bucket = Bucket::new(200);
        let now = Instant::now();
        let taken = (0..300).filter(|_| bucket.take(now)).count();
        assert_eq!(taken, 200);
        let later = now + Duration::from_millis(50);
        let refilled = (0..300).filter(|_| bucket.take(later)).count();
        assert_eq!(refilled, 10);
        // It never holds more than a second's worth.
        let much_later = later + Duration::from_secs(60);
        assert_eq!((0..300).filter(|_| bucket.take(much_later)).count(), 200);
    }
}
