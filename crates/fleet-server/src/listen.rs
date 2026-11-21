//! How the planes an instance opens are served (SEC-tls13-bounded-input-chosen-planes §15). The listener, its TLS
//! and its bounds on connection setup are `opamp`'s (DEV-six-crates-tokio-axum); what is the Server's is that the
//! planes it opens share one handle and one idea of shutting down (SEC-tls13-bounded-input-chosen-planes §17).

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use opamp::server::listen::{Handle, Listener};
use rustls::ServerConfig;

/// A plane on an already-bound listener, over TLS when `tls` is given, holding at most
/// `max_connections` at once (SEC-tls13-two-planes-bounded-input). `handle` is shared by every plane the instance
/// opens, so one signal drains them all (SEC-tls13-bounded-input-chosen-planes §17).
#[must_use]
pub fn plane(
    listener: TcpListener,
    tls: Option<Arc<ServerConfig>>,
    max_connections: usize,
    handle: Handle,
) -> Listener {
    let plane = Listener::new(listener, handle).with_max_connections(max_connections);
    match tls {
        Some(config) => plane.with_tls(config),
        None => plane,
    }
}

/// Asks the planes the instance opens to stop: none takes a new connection, and the WebSocket sessions are closed
/// spread over `drain`, so their Agents come back to the other instances, or to this one once it is
/// started again, without arriving all at once
/// (SRV-instances-share-one-postgresql §3).
/// What is still open shortly after the window is cut, and only then does the record
/// flush that follows shutdown (SRV-fleet-record-agents-hosts) run.
///
/// A free function rather than a call at the signal site, so the planes cannot end up with
/// different ideas of what shutting down means.
pub fn shut_down(handle: &Handle, drain: Duration) {
    handle.drain_spread(drain);
}
