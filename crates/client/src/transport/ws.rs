//! The WebSocket transport (ADR-0007): one persistent connection, either side sends at will —
//! this is what makes a configuration change arrive within seconds instead of a poll interval.

use futures_util::{SinkExt, StreamExt};
use opamp::frame;
use opamp::proto::ServerToAgent;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream,
};
use tracing::{info, warn};

use crate::agent::Agent;
use crate::config::ClientConfig;
use crate::transport::Backoff;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum Served {
    /// The operator stopped the Client; the goodbye is already sent.
    Shutdown,
    /// The connection is gone; reconnect with backoff and report full state again.
    ConnectionLost,
}

pub async fn run(mut agent: Agent, config: &ClientConfig) -> Result<(), String> {
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
                match serve(socket, &mut agent).await {
                    Served::Shutdown => return Ok(()),
                    Served::ConnectionLost => warn!("connection lost; reconnecting"),
                }
            }
            Err(e) => warn!(endpoint = %config.endpoint, error = %e, "cannot connect"),
        }

        let delay = backoff.advance();
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

async fn serve(mut socket: Socket, agent: &mut Agent) -> Served {
    // A (re)connected Server may know nothing about us: start from a full snapshot.
    agent.force_full();
    if send(&mut socket, agent).await.is_err() {
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
                        let handled = agent.handle(&reply);
                        if let Some(delay) = handled.retry_after {
                            // The server is throttling: drop the connection and come back later.
                            let _ = socket.close(None).await;
                            tokio::select! {
                                _ = tokio::time::sleep(delay) => {}
                                _ = tokio::signal::ctrl_c() => return Served::Shutdown,
                            }
                            return Served::ConnectionLost;
                        }
                        if handled.send_report && send(&mut socket, agent).await.is_err() {
                            return Served::ConnectionLost;
                        }
                    }
                    Message::Close(_) => return Served::ConnectionLost,
                    // tungstenite answers pings on the next write; text frames are not OpAMP.
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                // The Baseline: the final message sets agent_disconnect.
                let goodbye = agent.disconnect_message();
                let _ = socket
                    .send(Message::Binary(frame::encode(&goodbye).into()))
                    .await;
                let _ = socket.close(None).await;
                info!("disconnected");
                return Served::Shutdown;
            }
        }
    }
}

async fn send(socket: &mut Socket, agent: &mut Agent) -> Result<(), ()> {
    let report = agent.next_report();
    socket
        .send(Message::Binary(frame::encode(&report).into()))
        .await
        .map_err(|e| {
            warn!(error = %e, "cannot send a report");
        })
}
