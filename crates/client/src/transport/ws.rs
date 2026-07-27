//! The WebSocket transport (ADR-0007): one persistent connection, either side sends at will —
//! this is what makes a configuration change arrive within seconds instead of a poll interval.
//!
//! The connection carries every Agent the [`Engine`] holds, disambiguated by `instance_uid`
//! alone (ADR-0003): n Agents over one connection, routed by the Engine, never by this loop.

use futures_util::{SinkExt, StreamExt};
use opamp::frame;
use opamp::proto::{AgentToServer, ServerToAgent};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream,
};
use tracing::{info, warn};

use crate::config::ClientConfig;
use crate::engine::Engine;
use crate::service::runtime::Shutdown;
use crate::transport::Backoff;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum Served {
    /// The operator stopped the Client; the goodbyes are already sent.
    Shutdown,
    /// The connection is gone; reconnect with backoff and report full state again.
    ConnectionLost,
}

pub async fn run(
    mut engine: Engine,
    config: &ClientConfig,
    shutdown: &mut Shutdown,
) -> Result<(), String> {
    let connector = match &config.tls {
        Some(tls) => Some(Connector::Rustls(crate::tls::rustls_config_with_ca(
            &tls.ca_file,
        )?)),
        None => None,
    };

    let mut backoff = Backoff::new();
    loop {
        match connect_async_tls_with_config(&config.endpoint, None, false, connector.clone()).await
        {
            Ok((socket, _)) => {
                info!(endpoint = %config.endpoint, "connected");
                backoff.reset();
                match serve(socket, &mut engine, shutdown).await {
                    Served::Shutdown => {
                        // Usually already stopped before the goodbyes went out; idempotent.
                        engine.shutdown_processes().await;
                        return Ok(());
                    }
                    Served::ConnectionLost => warn!("connection lost; reconnecting"),
                }
            }
            Err(e) => warn!(endpoint = %config.endpoint, error = %e, "cannot connect"),
        }

        let delay = backoff.advance();
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.requested() => {
                // Stopped while disconnected: no goodbyes to send, but the Managed Processes
                // still stop before the runtime goes away.
                engine.shutdown_processes().await;
                return Ok(());
            }
        }
    }
}

async fn serve(mut socket: Socket, engine: &mut Engine, shutdown: &mut Shutdown) -> Served {
    // A (re)connected Server may know nothing about us: every Agent starts from a full snapshot.
    engine.force_full_all();
    if send_all(&mut socket, engine.poll_reports()).await.is_err() {
        return Served::ConnectionLost;
    }

    loop {
        tokio::select! {
            incoming = socket.next() => {
                let Some(Ok(message)) = incoming else {
                    return Served::ConnectionLost;
                };
                match message {
                    Message::Binary(data) => {
                        let reply: ServerToAgent = match frame::decode(&data) {
                            Ok(reply) => reply,
                            Err(e) => {
                                warn!(error = %e, "undecodable message from the server");
                                continue;
                            }
                        };
                        let handled = engine.handle(&reply);
                        if let Some(delay) = handled.retry_after {
                            // The server is throttling: drop the connection and come back later.
                            let _ = socket.close(None).await;
                            tokio::select! {
                                _ = tokio::time::sleep(delay) => {}
                                _ = shutdown.requested() => return Served::Shutdown,
                            }
                            return Served::ConnectionLost;
                        }
                        if send_all(&mut socket, engine.owed_reports()).await.is_err() {
                            return Served::ConnectionLost;
                        }
                    }
                    Message::Close(_) => return Served::ConnectionLost,
                    // tungstenite answers pings on the next write; text frames are not OpAMP.
                    _ => {}
                }
            }
            // A Managed Process changed some Agent's state: push it now, not at the next poll.
            _ = engine.changed() => {
                if send_all(&mut socket, engine.owed_reports()).await.is_err() {
                    return Served::ConnectionLost;
                }
            }
            _ = shutdown.requested() => {
                // Managed Processes stop first; then the Baseline's final messages, one
                // agent_disconnect per Agent.
                engine.shutdown_processes().await;
                let _ = send_all(&mut socket, engine.disconnect_messages()).await;
                let _ = socket.close(None).await;
                info!("disconnected");
                return Served::Shutdown;
            }
        }
    }
}

async fn send_all(socket: &mut Socket, reports: Vec<AgentToServer>) -> Result<(), ()> {
    for report in reports {
        socket
            .send(Message::Binary(frame::encode(&report).into()))
            .await
            .map_err(|e| {
                warn!(error = %e, "cannot send a report");
            })?;
    }
    Ok(())
}
