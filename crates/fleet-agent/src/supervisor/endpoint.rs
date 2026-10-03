//! The Supervisor Endpoint (ADR-0014, ADR-0017): the loopback OpAMP endpoint every Supervisor
//! exposes, WebSocket-only — what a Managed Process carrying an OpAMP client of its own
//! (notably the Collector's `opampextension`) connects to.
//!
//! It folds **content, not identity**: the process's description, health, and effective
//! configuration become [`ProcessEvent`]s for the owning Agent, whose `instance_uid` stays the
//! Supervisor's. It is not a Server in the specification's sense — it manages no fleet, holds
//! no configuration, and serves exactly one local process; for a Foreign Agent nothing ever
//! connects, and that is the whole of the handling. The communication itself is
//! `opamp::server`'s, the same endpoint the Server and the Gateway sit on (ADR-0009).

use std::net::SocketAddr;
use std::sync::Arc;

use opamp::proto::{AgentToServer, ServerCapabilities, ServerToAgent};
use opamp::server::listen::{Handle, Listener};
use opamp::server::{Handler, Outbound, Rejection, Reply, RequestInfo, Settings, Transports};
use tracing::{debug, info, warn};

use crate::shutdown::Shutdown;
use crate::supervisor::ports::{EventSender, ProcessEvent};

/// What this endpoint declares to the connecting client: it takes status reports and effective
/// configuration; it offers nothing (no remote config — that flows through the Supervisor).
const ENDPOINT_CAPABILITIES: u64 =
    ServerCapabilities::AcceptsStatus as u64 | ServerCapabilities::AcceptsEffectiveConfig as u64;

pub struct Endpoint {
    listener: std::net::TcpListener,
    name: String,
    events: EventSender,
    /// The message size limit this endpoint enforces in both directions. It speaks the Server
    /// side of the protocol, so the Baseline's limits bind it exactly as they bind the Server.
    max_message_size: usize,
}

impl Endpoint {
    /// Binds `127.0.0.1:<port>` (`0` = ephemeral) — at startup, so a taken port fails loudly.
    ///
    /// # Errors
    /// Returns an error when the loopback port cannot be bound.
    pub fn bind(
        name: String,
        port: u16,
        events: EventSender,
        max_message_size: usize,
    ) -> Result<Self, String> {
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", port))
            .map_err(|e| format!("supervisor {name:?}: cannot bind 127.0.0.1:{port}: {e}"))?;
        std_listener
            .set_nonblocking(true)
            .map_err(|e| format!("supervisor {name:?}: cannot prepare the endpoint: {e}"))?;
        Ok(Endpoint {
            listener: std_listener,
            name,
            events,
            max_message_size,
        })
    }

    /// The bound address — logged so an operator can point the `opampextension` at it.
    ///
    /// # Errors
    /// Returns an error when the local address cannot be read back.
    pub fn local_addr(&self) -> Result<SocketAddr, String> {
        self.listener
            .local_addr()
            .map_err(|e| format!("supervisor {:?}: no endpoint address: {e}", self.name))
    }

    /// Serves connections until shutdown, which also closes a session still open.
    pub async fn run(self, mut shutdown: Shutdown) {
        let handler = Arc::new(Folding {
            name: self.name.clone(),
            events: self.events,
            shutdown: shutdown.clone(),
        });
        // WebSocket-only, and on any path: this listener serves exactly one local process, so
        // there is nothing to route by.
        let app = opamp::server::router(
            handler,
            Settings {
                max_message_size: self.max_message_size,
                transports: Transports::WebSocketOnly,
                any_path: true,
            },
        );
        // On the listener every OpAMP endpoint is served on (ADR-0009), so a local process that
        // falls silent mid-request is bounded as a remote one is.
        let handle = Handle::new();
        let trigger = handle.clone();
        tokio::spawn(async move {
            shutdown.requested().await;
            trigger.graceful_shutdown(None);
        });
        let served = Listener::new(self.listener, handle).serve(app).await;
        if let Err(e) = served {
            warn!(supervisor = %self.name, error = %e, "the endpoint stopped");
        }
    }
}

/// The endpoint's handler: every `AgentToServer` is folded into the owning Agent and answered with
/// this endpoint's capability set, so the client keeps reporting what we accept.
struct Folding {
    name: String,
    events: EventSender,
    shutdown: Shutdown,
}

/// A session ends when the Client does: a socket the extension holds open is closed on shutdown
/// rather than outliving the listener.
struct UntilShutdown(Shutdown);

impl Outbound for UntilShutdown {
    type Item = std::convert::Infallible;

    async fn next(&mut self) -> Option<Self::Item> {
        self.0.requested().await;
        None
    }
}

impl Handler for Folding {
    type Connection = ();
    type Outbound = UntilShutdown;

    fn on_connecting(
        &self,
        _request: &RequestInfo<'_>,
    ) -> Result<((), Option<UntilShutdown>), Rejection> {
        debug!(supervisor = %self.name, "endpoint connection");
        Ok(((), Some(UntilShutdown(self.shutdown.clone()))))
    }

    async fn on_message(&self, _: &mut (), report: AgentToServer) -> Reply {
        let reply = ServerToAgent {
            instance_uid: report.instance_uid.clone(),
            capabilities: ENDPOINT_CAPABILITIES,
            ..Default::default()
        };
        self.fold(report).await;
        Reply::Send(reply)
    }

    fn on_outbound(&self, _: &mut (), item: std::convert::Infallible) -> Vec<ServerToAgent> {
        match item {}
    }
}

impl Folding {
    /// Content, not identity: what the process reported about itself becomes events for the
    /// owning Agent; the process's own `instance_uid` stays local to this session.
    async fn fold(&self, report: AgentToServer) {
        if let Some(description) = report.agent_description {
            self.events
                .send(ProcessEvent::Description(description))
                .await;
        }
        if let Some(health) = report.health {
            self.events.send(ProcessEvent::Health(health)).await;
        }
        if let Some(effective) = report.effective_config {
            self.events
                .send(ProcessEvent::EffectiveConfig(effective))
                .await;
        }
        if let Some(components) = report.available_components {
            self.events
                .send(ProcessEvent::AvailableComponents(components))
                .await;
        }
    }
}

/// Bind and start an endpoint task, returning the bound address.
///
/// # Errors
/// Returns an error when the port cannot be bound.
pub fn start(
    name: String,
    port: u16,
    events: EventSender,
    shutdown: Shutdown,
    max_message_size: usize,
) -> Result<SocketAddr, String> {
    let endpoint = Endpoint::bind(name.clone(), port, events, max_message_size)?;
    let addr = endpoint.local_addr()?;
    info!(supervisor = %name, endpoint = %format!("ws://{addr}/v1/opamp"), "supervisor endpoint ready");
    tokio::spawn(endpoint.run(shutdown));
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shutdown::shutdown_channel;
    use futures_util::{SinkExt, StreamExt};
    use opamp::frame;
    use opamp::proto::{AgentDescription, ComponentHealth, EffectiveConfig};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::Message;

    /// A fake `opampextension`: connects, reports, and expects the capability echo.
    #[tokio::test]
    async fn extension_reports_are_folded_into_process_events() {
        let (event_tx, mut events) = mpsc::channel(16);
        let (_shutdown_tx, shutdown) = shutdown_channel();
        let addr = start(
            "test".to_string(),
            0,
            EventSender::new(0, event_tx),
            shutdown,
            opamp::frame::DEFAULT_MAX_MESSAGE_SIZE,
        )
        .expect("endpoint starts");

        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/opamp"))
            .await
            .expect("the fake extension connects");

        let report = AgentToServer {
            instance_uid: opamp::uid::InstanceUid::default().as_bytes().to_vec(),
            agent_description: Some(AgentDescription::default()),
            health: Some(ComponentHealth {
                healthy: true,
                ..Default::default()
            }),
            effective_config: Some(EffectiveConfig::default()),
            available_components: Some(opamp::proto::AvailableComponents {
                components: Default::default(),
                hash: b"h".to_vec(),
            }),
            ..Default::default()
        };
        socket
            .send(Message::Binary(
                frame::encode_within(&report, opamp::frame::DEFAULT_MAX_MESSAGE_SIZE)
                    .expect("within the limit")
                    .into(),
            ))
            .await
            .expect("send the report");

        let mut kinds = Vec::new();
        for _ in 0..4 {
            let (index, event) = tokio::time::timeout(Duration::from_secs(10), events.recv())
                .await
                .expect("an event in time")
                .expect("an open channel");
            assert_eq!(index, 0);
            kinds.push(match event {
                ProcessEvent::Description(_) => "description",
                ProcessEvent::Pid(_) => "pid",
                ProcessEvent::Health(_) => "health",
                ProcessEvent::EffectiveConfig(_) => "effective",
                ProcessEvent::AvailableComponents(_) => "components",
                ProcessEvent::ConfigApplied { .. } => "applied",
                ProcessEvent::PackageApplied { .. } => "package",
                ProcessEvent::Uninstalled { .. } => "uninstalled",
            });
        }
        assert_eq!(
            kinds,
            vec!["description", "health", "effective", "components"]
        );

        // The reply echoes the extension's uid and declares what this endpoint accepts.
        let reply = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a reply in time")
            .expect("an open socket")
            .expect("a frame");
        let Message::Binary(data) = reply else {
            panic!("expected a binary reply");
        };
        let decoded: ServerToAgent =
            frame::decode(&data, opamp::frame::DEFAULT_MAX_MESSAGE_SIZE).expect("decodable");
        assert_eq!(decoded.instance_uid, report.instance_uid);
        assert_eq!(decoded.capabilities, ENDPOINT_CAPABILITIES);
    }

    #[tokio::test]
    async fn shutdown_stops_the_endpoint() {
        let (event_tx, _events) = mpsc::channel(16);
        let (shutdown_tx, shutdown) = shutdown_channel();
        let addr = start(
            "test".to_string(),
            0,
            EventSender::new(0, event_tx),
            shutdown,
            opamp::frame::DEFAULT_MAX_MESSAGE_SIZE,
        )
        .expect("endpoint starts");
        shutdown_tx.send(true).expect("signal shutdown");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            tokio_tungstenite::connect_async(format!("ws://{addr}/v1/opamp"))
                .await
                .is_err(),
            "a stopped endpoint accepts no connections"
        );
    }
}
