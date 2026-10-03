//! How the Server's two planes are served (ADR-0023). The listener, its TLS and its bounds on
//! connection setup are `opamp`'s (ADR-0024); what is the Server's is that both planes share one
//! handle and one idea of shutting down.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use opamp::server::listen::{Handle, Listener};
use rustls::ServerConfig;

/// How long connections are given to finish once shutdown has been asked for.
///
/// Bounded on purpose: an Agent's WebSocket is idle most of the time and would otherwise decide
/// how long a restart takes. Whatever has not ended by then is cut, and the record flush that
/// follows shutdown (ADR-0013) still runs.
pub const SHUTDOWN_DRAIN: Duration = Duration::from_secs(10);

/// A plane on an already-bound listener, over TLS when `tls` is given. `handle` is shared by both
/// planes, so one signal drains both.
#[must_use]
pub fn plane(listener: TcpListener, tls: Option<Arc<ServerConfig>>, handle: Handle) -> Listener {
    let plane = Listener::new(listener, handle);
    match tls {
        Some(config) => plane.with_tls(config),
        None => plane,
    }
}

/// Asks both planes to stop and gives their connections [`SHUTDOWN_DRAIN`] to end.
///
/// A free function rather than a call at the signal site, so the two planes cannot end up with
/// different ideas of what shutting down means.
pub fn shut_down(handle: &Handle) {
    handle.graceful_shutdown(Some(SHUTDOWN_DRAIN));
}
