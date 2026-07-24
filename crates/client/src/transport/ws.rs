//! The WebSocket transport (ADR-0012): one persistent connection, either side sends at will —
//! this is what makes a configuration change arrive within seconds instead of a poll interval.
//!
//! The connection carries every Agent the [`Engine`] holds, disambiguated by `instance_uid`
//! alone (ADR-0009): n Agents over one connection, routed by the Engine, never by this loop.

use futures_util::{SinkExt, StreamExt};
use opamp::frame;
use opamp::proto::{AgentToServer, ServerToAgent};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream,
};
use tracing::{info, warn};

use crate::config::ClientConfig;
use crate::engine::Engine;
use crate::service::runtime::Shutdown;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum Served {
    /// The operator stopped the Client; the goodbyes are already sent.
    Shutdown,
    /// The connection is gone; reconnect with backoff and report full state again.
    ConnectionLost,
}

pub async fn run(
    shutdown: &mut Shutdown,

    // The receive limit the Baseline requires of the Client: the transport refuses to buffer a
    // message past it, so an oversized Server can never make this process allocate without bound.
    // The per-frame cap moves with the message limit: left at its default it would refuse
    // messages below the configured limit, which is the limit's business, not the framing's.
    let ws_config = Some(
        WebSocketConfig::default()
            .max_message_size(Some(config.max_message_size_bytes))
            .max_frame_size(Some(config.max_message_size_bytes)),
    );

    let mut backoff = Backoff::new();
    loop {
        match connect_async_tls_with_config(request, ws_config, false, connector.clone()).await {
            Ok((socket, _)) => {
                info!(endpoint = %config.endpoint, "connected");
                backoff.reset();
                    Served::Shutdown => {
                        // Usually already stopped before the goodbyes went out; idempotent.
                        engine.shutdown_processes().await;
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
            }
        }
    }
}

async fn serve(
    mut socket: Socket,
    engine: &mut Engine,
    shutdown: &mut Shutdown,
) -> Served {
    let limit = config.max_message_size_bytes;

    // A (re)connected Server may know nothing about us: every Agent starts from a full snapshot.
    engine.force_full_all();
    if send_all(&mut socket, engine.poll_reports(), limit)
        .await
        .is_err()
    {
        return Served::ConnectionLost;
    }

    // The heartbeat (ReportsHeartbeat, Baseline default 30 s; 0 disables): a routine report per
    // Agent, so `sequence_num` advances and the Server's liveness view stays fresh without any
    // state change. Starts one period from now — the connect snapshot just went out.
    let mut heartbeat = (config.heartbeat_interval_secs > 0).then(|| {
        let period = std::time::Duration::from_secs(config.heartbeat_interval_secs);
        tokio::time::interval_at(tokio::time::Instant::now() + period, period)
    });

    loop {
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
                    // as an error rather than as a message. The Baseline's answer is the 1009
                    // close; on a merely broken connection the frame never leaves, which is fine.
                    Some(Err(e)) => {
                        warn!(error = %e, "closing the connection after a receive error");
                        let _ = socket.close(Some(too_big_close())).await;
                        return Served::ConnectionLost;
                    }
                    None => return Served::ConnectionLost,
                };
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
                        if send_all(&mut socket, engine.owed_reports(), limit).await.is_err() {
                            return Served::ConnectionLost;
                        }
                            && send_all(&mut socket, engine.owed_reports(), limit).await.is_err()
                            return Served::ConnectionLost;
                        }
                            let _ = socket.close(None).await;
                            && send_all(&mut socket, engine.owed_reports(), limit).await.is_err()
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
                if send_all(&mut socket, engine.owed_reports(), limit).await.is_err() {
                    return Served::ConnectionLost;
                }
            }
            _ = heartbeat_due => {
                if send_all(&mut socket, engine.poll_reports(), limit).await.is_err() {
                    return Served::ConnectionLost;
                }
            }
            _ = shutdown.requested() => {
                // Managed Processes stop first; then the Baseline's final messages, one
                // agent_disconnect per Agent.
                engine.shutdown_processes().await;
                let _ = send_all(&mut socket, engine.disconnect_messages(), limit).await;
                let _ = socket.close(None).await;
                info!("disconnected");
                return Served::Shutdown;
            }
        }
    }
}

/// Sends the reports, each under the send limit. A report past it is dropped with a log line
/// rather than put on the wire — the Baseline forbids sending one — while the connection stays up;
/// `Err` means the connection is gone, which is a different thing entirely.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::runtime::shutdown_channel;
    use crate::storage::Storage;
    use crate::supervisor::agent::AgentState;
    use tokio_tungstenite::accept_async;

    /// The Baseline: a `ServerToAgent` past the receive limit is malformed — the Client refuses it
    /// and closes with 1009 rather than acting on it.
    #[tokio::test]
    async fn an_oversized_message_from_the_server_closes_with_1009() {
        const LIMIT: usize = 4096;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        // A Server that answers the connect snapshot with a message twice the Client's limit,
        // then waits for what the Client does about it.
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut socket = accept_async(stream).await.expect("handshake");
            // The Client's full report on connect; ignored beyond keeping the stream moving.
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

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let state = AgentState::new("limit-test".to_string(), storage).expect("agent state");
        let mut engine = Engine::new(vec![state]);
            endpoint: format!("ws://{addr}/v1/opamp"),
            max_message_size_bytes: LIMIT,
            heartbeat_interval_secs: 0,
            ..ClientConfig::default()
        };
        let (_shutdown_tx, mut shutdown) = shutdown_channel();
        let (socket, _) = tokio_tungstenite::connect_async(&config.endpoint)
            .await
            .expect("connect");

        assert!(
            matches!(outcome, Served::ConnectionLost),
            "an oversized message ends the connection"
        );

        let close = tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("the server sees the close in time")
            .expect("the server task")
            .expect("a close frame with a status code");
        assert_eq!(
            u16::from(close.code),
            1009,
            "the Baseline names 1009 (Message Too Big)"
        );
    }
}
