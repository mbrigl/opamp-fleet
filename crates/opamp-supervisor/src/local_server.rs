//! A local OpAMP server the managed collector connects to (ADR-0008).
//!
//! The collector's `opamp` extension is an OpAMP *client*: pointed at this local server, it reports the
//! collector's own health and effective configuration. The supervisor reads the latest of those from
//! [`CollectorLink`] and forwards them to the real Server, so the health and effective config the fleet
//! sees are the collector's *actual* state rather than the supervisor's assumptions.
//!
//! This server only observes — it receives the collector's reports and acknowledges them. Configuration
//! is still applied by writing the file and restarting the collector (ADR-0008), not over this channel.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

use opamp_proto::frame;
use opamp_proto::proto::{
    AgentDescription, AgentToServer, AvailableComponents, ComponentHealth, EffectiveConfig,
    ServerCapabilities, ServerToAgent,
};

/// The latest state the managed collector has reported over the local OpAMP connection. Fields follow
/// the OpAMP delta rule — a field absent from a report leaves the previous value in place.
#[derive(Default, Clone)]
pub struct CollectorReport {
    pub health: Option<ComponentHealth>,
    pub effective_config: Option<EffectiveConfig>,
    /// The collector's own agent description (collector-authoritative identity), if it reported one.
    pub agent_description: Option<AgentDescription>,
    /// The components the collector reports as available (ReportsAvailableComponents).
    pub available_components: Option<AvailableComponents>,
}

/// A handle to the managed collector's latest reports, updated by the local OpAMP server.
#[derive(Clone)]
pub struct CollectorLink {
    latest: Arc<Mutex<CollectorReport>>,
    /// Notified whenever the collector reports a *meaningful* change (health status or effective
    /// config), so the supervisor can forward it to the real server promptly rather than waiting for
    /// the next config change (ADR-0008).
    changed: Arc<Notify>,
}

impl CollectorLink {
    /// A snapshot of what the collector last reported. Empty until the collector connects.
    pub fn latest(&self) -> CollectorReport {
        self.latest
            .lock()
            .expect("collector report lock poisoned")
            .clone()
    }

    /// Completes when the collector next reports a meaningful change. Coalescing: several changes
    /// while nothing awaits collapse into one wake-up, and the awaiter then reads the latest state.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// A link pre-seeded with a report, for tests that exercise how reports are consumed without
    /// standing up the server and a collector.
    #[cfg(test)]
    pub(crate) fn seeded(report: CollectorReport) -> Self {
        Self {
            latest: Arc::new(Mutex::new(report)),
            changed: Arc::new(Notify::new()),
        }
    }
}

/// The capabilities this local server advertises to the collector: it accepts status reports and the
/// effective configuration the collector reports back.
const LOCAL_SERVER_CAPABILITIES: u64 =
    ServerCapabilities::AcceptsStatus as u64 | ServerCapabilities::AcceptsEffectiveConfig as u64;

/// Binds a local OpAMP server on `bind` and starts accepting the collector's connection. Returns a
/// handle to the collector's reports and the address the collector should be pointed at.
pub async fn start(bind: &str) -> std::io::Result<(CollectorLink, SocketAddr)> {
    let listener = TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    let link = CollectorLink {
        latest: Arc::new(Mutex::new(CollectorReport::default())),
        changed: Arc::new(Notify::new()),
    };
    tokio::spawn(accept_loop(listener, link.clone()));
    Ok((link, addr))
}

async fn accept_loop(listener: TcpListener, link: CollectorLink) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                debug!(%peer, "collector connected to the local OpAMP server");
                tokio::spawn(serve_collector(stream, link.clone()));
            }
            Err(e) => warn!(error = %e, "local OpAMP server accept failed"),
        }
    }
}

/// Serves one collector connection: folds each report's health and effective config into the shared
/// state, wakes the supervisor on a meaningful change, and acknowledges it, for the life of the
/// connection.
async fn serve_collector(stream: TcpStream, link: CollectorLink) {
    let mut ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            debug!(error = %e, "collector websocket handshake failed");
            return;
        }
    };

    while let Some(message) = ws.next().await {
        let data = match message {
            Ok(Message::Binary(data)) => data,
            Ok(Message::Close(_)) | Err(_) => break,
            // Text/ping/pong are not OpAMP frames; the transport handles ping/pong.
            Ok(_) => continue,
        };
        let report: AgentToServer = match frame::decode(&data) {
            Ok(report) => report,
            Err(e) => {
                debug!(error = %e, "cannot decode collector report");
                continue;
            }
        };
        debug!(
            description = report.agent_description.is_some(),
            components = report.available_components.is_some(),
            health = report.health.is_some(),
            effective_config = report.effective_config.is_some(),
            "collector report"
        );

        // Fold the reported fields in (delta rule: only present fields overwrite) and note whether the
        // change is meaningful — the collector re-sends health with a fresh timestamp on every report,
        // which is not a change worth forwarding.
        let meaningful = {
            let mut current = link.latest.lock().expect("collector report lock poisoned");
            let mut meaningful = false;
            if let Some(health) = &report.health {
                if health_changed(current.health.as_ref(), health) {
                    meaningful = true;
                }
                current.health = Some(health.clone());
            }
            if let Some(effective) = &report.effective_config {
                if current.effective_config.as_ref() != Some(effective) {
                    meaningful = true;
                }
                current.effective_config = Some(effective.clone());
            }
            if let Some(description) = &report.agent_description {
                if current.agent_description.as_ref() != Some(description) {
                    meaningful = true;
                }
                current.agent_description = Some(description.clone());
            }
            if let Some(components) = &report.available_components {
                if current.available_components.as_ref() != Some(components) {
                    meaningful = true;
                }
                current.available_components = Some(components.clone());
            }
            meaningful
        };
        if meaningful {
            link.changed.notify_one();
        }

        // The collector's opamp extension expects a ServerToAgent in reply.
        let ack = ServerToAgent {
            instance_uid: report.instance_uid,
            capabilities: LOCAL_SERVER_CAPABILITIES,
            ..Default::default()
        };
        if ws
            .send(Message::Binary(frame::encode(&ack).into()))
            .await
            .is_err()
        {
            break;
        }
    }
}

/// Whether a health report differs in a way worth forwarding — the liveness, status, or error text —
/// ignoring the timestamps the collector bumps on every report.
fn health_changed(old: Option<&ComponentHealth>, new: &ComponentHealth) -> bool {
    match old {
        None => true,
        Some(old) => {
            old.healthy != new.healthy
                || old.status != new.status
                || old.last_error != new.last_error
        }
    }
}
