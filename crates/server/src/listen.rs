//! How a plane is served, and what bounds a connection before it is a request (ADR-0073).
//!
//! Everything else the Server enforces — the message size limits, Admission, `max_agents` — starts
//! at a request. A peer that completes the TCP connection, sends `GET /v1/opamp HTTP/1.1` and then
//! falls silent reaches none of it, and would otherwise hold a task and hyper's read buffer for as
//! long as it likes, without presenting a credential to anyone.
//!
//! hyper defaults its HTTP/1 `header_read_timeout` to 30 seconds, but the default is inert while no
//! `Timer` is installed — `Time::check` warns *"timeout `header_read_timeout` has default, but no
//! timer set"* and resolves it to `None`. Neither `axum::serve` (0.8) nor `axum_server` (0.7)
//! installs one, so both planes ran without that bound. Configuring the timeout *without* a timer
//! is worse than not configuring it: hyper panics. Both go together, and they go here.
//!
//! This is deliberately a bound on **connection setup**, not on a request. The Agent plane carries
//! a long-lived WebSocket and streams package artifacts of arbitrary size (ADR-0015), and the
//! Operator plane accepts a package upload with the body limit switched off (ADR-0008): a request
//! timeout would break exactly those three and leave the slow peer untouched.

use std::net::TcpListener;
use std::time::Duration;

use axum_server::{Handle, Server};
use hyper_util::rt::TokioTimer;

/// How long a connection may take to send its request line and headers.
///
/// hyper's own default, and the value `axum::serve` applies from the release that carries
/// [tokio-rs/axum#3478](https://github.com/tokio-rs/axum/pull/3478) — so when that lands here this
/// stops being an opinion and becomes agreement. It bounds the request line and the headers only:
/// a body, a WebSocket session and a package download are all unaffected by it.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the TLS handshake may take before the connection is dropped.
///
/// The same value `axum_server` applies by default; stated here so it reads as a decision rather
/// than an inheritance, and so that both listeners visibly have one.
pub const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long connections are given to finish once shutdown has been asked for.
///
/// Bounded on purpose: an Agent's WebSocket is idle most of the time and would otherwise decide
/// how long a restart takes. Whatever has not ended by then is cut, and the record flush that
/// follows shutdown (ADR-0051) still runs.
pub const SHUTDOWN_DRAIN: Duration = Duration::from_secs(10);

/// A plane's server on an already-bound listener, with its connection setup bounded.
///
/// `acceptor` is what distinguishes the four cases: [`axum_server::accept::DefaultAcceptor`] for a
/// plain listener, [`crate::tls::PeerCertAcceptor`] for the Agent plane over TLS, and a plain
/// [`axum_server::tls_rustls::RustlsAcceptor`] for the Operator plane over TLS. `handle` is shared
/// by both planes, so one signal drains both.
pub fn plane<A>(listener: TcpListener, acceptor: A, handle: Handle) -> Server<A> {
    plane_with_header_read_timeout(listener, acceptor, handle, HEADER_READ_TIMEOUT)
}

/// [`plane`] with the header-read timeout named explicitly — the seam the test for it drives, since
/// nothing else would make a 30-second bound observable in a test suite.
pub fn plane_with_header_read_timeout<A>(
    listener: TcpListener,
    acceptor: A,
    handle: Handle,
    header_read_timeout: Duration,
) -> Server<A> {
    let mut server = axum_server::from_tcp(listener)
        .handle(handle)
        .acceptor(acceptor);
    server
        .http_builder()
        .http1()
        // The timer first: the timeout below panics without one, and the default above is silently
        // discarded without one.
        .timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout);
    server
}

/// Asks both planes to stop and gives their connections [`SHUTDOWN_DRAIN`] to end.
///
/// A free function rather than a call at the signal site, so the two planes cannot end up with
/// different ideas of what shutting down means.
pub fn shut_down(handle: &Handle) {
    handle.graceful_shutdown(Some(SHUTDOWN_DRAIN));
}
