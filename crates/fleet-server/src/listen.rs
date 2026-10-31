//! How the Server's two planes are served (ADR-0012). The listener, its TLS and its bounds on
//! connection setup are `opamp`'s (ADR-0008); what is the Server's is that both planes share one
//! handle and one idea of shutting down.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use opamp::server::listen::{Handle, Listener};
use rustls::ServerConfig;

/// A plane on an already-bound listener, over TLS when `tls` is given, holding at most
/// `max_connections` at once (ADR-0012). `handle` is shared by both planes, so one signal drains
/// both.
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

/// Asks both planes to stop: neither takes a new connection, and the WebSocket sessions are closed
/// spread over `drain`, so their Agents come back to the other instances, or to this one once it is
/// started again, without arriving all at once
/// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
/// clause 3). What is still open shortly after the window is cut, and only then does the record
/// flush that follows shutdown (ADR-0024) run.
///
/// A free function rather than a call at the signal site, so the two planes cannot end up with
/// different ideas of what shutting down means.
pub fn shut_down(handle: &Handle, drain: Duration) {
    handle.drain_spread(drain);
}
