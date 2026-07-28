//! The WebSocket transport (ADR-0007): one persistent connection, either side sends at will —
//! this is what makes a configuration change arrive within seconds instead of a poll interval.
//!
//! The connection carries every Agent the [`Engine`] holds, disambiguated by `instance_uid`
//! alone (ADR-0003): n Agents over one connection, routed by the Engine, never by this loop.

use futures_util::{SinkExt, StreamExt};
use opamp::frame;
use opamp::proto::{AgentToServer, ServerToAgent};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream,
};
use tracing::{info, warn};

use crate::config::ClientConfig;
use crate::engine::Engine;
use crate::service::runtime::Shutdown;
use crate::transport::{Backoff, RunOutcome};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum Served {
    /// The operator stopped the Client; the goodbyes are already sent.
    Shutdown,
    /// The connection is gone; reconnect with backoff and report full state again.
    ConnectionLost,
    /// Verified connection settings took effect (ADR-0014); the runtime reconnects with them.
    Reconfigured,
}

pub async fn run(
    engine: &mut Engine,
    config: &ClientConfig,
    shutdown: &mut Shutdown,
) -> Result<RunOutcome, String> {
    let connector = match &config.tls {
        Some(tls) => Some(Connector::Rustls(crate::tls::rustls_config_with_ca(
            &tls.ca_file,
        )?)),
        None => None,
    };

    // The Authorization header (ADR-0013, rotated per ADR-0014) rides the upgrade request — the
    // server checks it before the WebSocket comes up.
    let authorization = match config.authorization_value()? {
        Some(value) => {
            let value: tokio_tungstenite::tungstenite::http::HeaderValue = value
                .parse()
                .map_err(|e| format!("the [auth] credentials are not a valid header: {e}"))?;
            if config.sends_credentials_in_cleartext() {
                warn!(
                    "sending credentials over unencrypted ws:// beyond the loopback — use wss://"
                );
            }
            Some(value)
        }
        None => None,
    };

    let mut backoff = Backoff::new();
    loop {
        // tungstenite consumes the request per attempt; rebuild it from the endpoint each time.
        let mut request = config
            .endpoint
            .as_str()
            .into_client_request()
            .map_err(|e| format!("invalid endpoint {}: {e}", config.endpoint))?;
        if let Some(value) = &authorization {
            request.headers_mut().insert(AUTHORIZATION, value.clone());
        }
        match connect_async_tls_with_config(request, None, false, connector.clone()).await {
            Ok((socket, _)) => {
                info!(endpoint = %config.endpoint, "connected");
                backoff.reset();
                match serve(socket, engine, config, shutdown).await {
                    Served::Shutdown => {
                        // Usually already stopped before the goodbyes went out; idempotent.
                        engine.shutdown_processes().await;
                        return Ok(RunOutcome::Shutdown);
                    }
                    Served::Reconfigured => return Ok(RunOutcome::Reconfigured),
                    Served::ConnectionLost => warn!("connection lost; reconnecting"),
                }
            }
            Err(WsError::Http(response)) if response.status() == StatusCode::UNAUTHORIZED => {
                warn!(
                    endpoint = %config.endpoint,
                    "the server rejected the credentials (HTTP 401) — check [auth]"
                );
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
                return Ok(RunOutcome::Shutdown);
            }
        }
    }
}

async fn serve(
    mut socket: Socket,
    engine: &mut Engine,
    config: &ClientConfig,
    shutdown: &mut Shutdown,
) -> Served {
    // A (re)connected Server may know nothing about us: every Agent starts from a full snapshot.
    engine.force_full_all();
    if send_all(&mut socket, engine.poll_reports()).await.is_err() {
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
                        // A connection-settings offer (ADR-0014): the APPLYING acknowledgement
                        // just went out with the owed reports; now verify by actually
                        // connecting. Success persists the settings and reconnects with them;
                        // failure reports FAILED and stays on the working connection.
                        if let Some(offer) = engine.take_connection_offer() {
                            let settings = offer.opamp.clone().unwrap_or_default();
                            let probe = || engine.probe_report();
                            match crate::connection::verify(&settings, config, probe).await {
                                Ok(()) => {
                                    engine.connection_settings_outcome(&offer.hash, Ok(()));
                                    let merged = crate::connection::merge(
                                        crate::connection::load(&config.state_dir).as_ref(),
                                        &offer,
                                    );
                                    if let Err(e) =
                                        crate::connection::store(&config.state_dir, &merged)
                                    {
                                        warn!(error = %e, "cannot persist the connection settings");
                                    }
                                    info!("connection settings verified; reconnecting with them");
                                    let _ = socket.close(None).await;
                                    return Served::Reconfigured;
                                }
                                Err(e) => {
                                    warn!(error = %e, "offered connection settings failed verification");
                                    engine.connection_settings_outcome(&offer.hash, Err(&e));
                                    if send_all(&mut socket, engine.owed_reports()).await.is_err() {
                                        return Served::ConnectionLost;
                                    }
                                }
                            }
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
            _ = heartbeat_due => {
                if send_all(&mut socket, engine.poll_reports()).await.is_err() {
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
