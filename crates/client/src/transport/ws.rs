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
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream,
};
use tracing::{info, warn};

use crate::config::ClientConfig;
use crate::engine::Engine;
use crate::service::runtime::Shutdown;
use crate::transport::{too_big_close, Backoff, RunOutcome};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum Served {
    /// The operator stopped the Client; the goodbyes are already sent.
    Shutdown,
    /// The connection is gone; reconnect with backoff and report full state again.
    ConnectionLost,
    /// Verified connection settings took effect (ADR-0014); the runtime reconnects with them.
    Reconfigured,
    /// A self-update switched to a new version (ADR-0020); the run ends and the process asks the
    /// service manager for a restart.
    RestartForUpdate,
}

pub async fn run(
    engine: &mut Engine,
    config: &ClientConfig,
    shutdown: &mut Shutdown,
) -> Result<RunOutcome, String> {
    // Trust and identity in one configuration: a private CA when one is configured, and this
    // Client's client certificate when it has one (ADR-0007, ADR-0035).
    let connector = crate::tls::rustls_client_config(config)?.map(Connector::Rustls);

    // The Authorization header (ADR-0013, rotated per ADR-0014) rides the upgrade request — the
    // server checks it before the WebSocket comes up.
    let authorization = match config.authorization_value()? {
        Some(value) => {
            let mut value: tokio_tungstenite::tungstenite::http::HeaderValue = value
                .parse()
                .map_err(|e| format!("the [auth] credentials are not a valid header: {e}"))?;
            // Redact it from any `Debug` of the request headers, as the HTTP transport does
            // (`transport/http.rs`): a credential must not surface in a log line by accident.
            value.set_sensitive(true);
            if config.sends_credentials_in_cleartext() {
                warn!(
                    "sending credentials over unencrypted ws:// beyond the loopback — use wss://"
                );
            }
            Some(value)
        }
        None => None,
    };

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
        // tungstenite consumes the request per attempt; rebuild it from the endpoint each time.
        let mut request = config
            .endpoint
            .as_str()
            .into_client_request()
            .map_err(|e| format!("invalid endpoint {}: {e}", config.endpoint))?;
        if let Some(value) = &authorization {
            request.headers_mut().insert(AUTHORIZATION, value.clone());
        }
        match connect_async_tls_with_config(request, ws_config, false, connector.clone()).await {
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
                    Served::RestartForUpdate => return Ok(RunOutcome::RestartForUpdate),
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
                        // Enrolment (ADR-0035): with the Server's capabilities now known, ask it
                        // to sign a certificate if it signs them and this Client needs one. The
                        // answer arrives as an ordinary connection-settings offer.
                        engine.request_certificate(config);
                        // A connection-settings offer (ADR-0014): the APPLYING acknowledgement
                        // just went out with the owed reports; now verify by actually
                        // connecting. Success persists the settings and reconnects with them;
                        // failure reports FAILED and stays on the working connection.
                        if let Some(offer) = engine.take_connection_offer() {
                            let settings = offer.opamp.clone().unwrap_or_default();
                            let probe = || engine.probe_report();
                            match crate::connection::verify(&settings, config, probe).await {
                                Ok(()) => {
                                    // Applied as far as this Client honours the offer, and said
                                    // so — `FAILED` when it had to drop a field (ADR-0035).
                                    // The issued certificate is stored only now, after connecting with it
                                    // proved it works — the old one stayed in force until here (ADR-0035).
                                    if let Some(certificate) = &settings.certificate {
                                        if let Err(e) = crate::csr::accept(
                                            &config.state_dir,
                                            &certificate.cert,
                                        ) {
                                            warn!(error = %e, "cannot store the issued certificate");
                                        } else {
                                            info!("a client certificate was issued and is now in force");
                                        }
                                    }
                                    // Applied as far as this Client honours it, and said so (ADR-0035).
                                    match crate::connection::unhonoured(&settings) {
                                        Ok(()) => engine
                                            .connection_settings_outcome(&offer.hash, Ok(())),
                                        Err(e) => {
                                            warn!(error = %e, "connection settings partly applied");
                                            engine.connection_settings_outcome(
                                                &offer.hash,
                                                Err(&e),
                                            );
                                        }
                                    }
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
                                    if send_all(&mut socket, engine.owed_reports(), limit).await.is_err() {
                                        return Served::ConnectionLost;
                                    }
                                }
                            }
                        }
                        // A package offer (ADR-0015): download and verify; the Installed/Failed
                        // status flows back through the process events, but a synchronous
                        // download failure is reported now.
                        let mut sink = FrameSink { socket: &mut socket, limit };
                        if crate::transport::process_package_downloads(engine, config, &mut sink).await
                            && send_all(&mut socket, engine.owed_reports(), limit).await.is_err()
                        {
                            return Served::ConnectionLost;
                        }
                        // The `Installing` above is the last thing this version says (ADR-0020).
                        if engine.restart_for_update() {
                            return Served::RestartForUpdate;
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

/// This transport's way of putting reports on the wire, for jobs that report while they run —
/// a package download reporting its progress (ADR-0015).
struct FrameSink<'a> {
    socket: &'a mut Socket,
    limit: usize,
}

impl crate::transport::ReportSink for FrameSink<'_> {
    async fn send(&mut self, reports: Vec<AgentToServer>) -> Result<(), ()> {
        send_all(self.socket, reports, self.limit).await
    }
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
        let config = ClientConfig {
            endpoint: format!("ws://{addr}/v1/opamp"),
            max_message_size_bytes: LIMIT,
            heartbeat_interval_secs: 0,
            ..ClientConfig::default()
        };
        let (_shutdown_tx, mut shutdown) = shutdown_channel();
        let (socket, _) = tokio_tungstenite::connect_async(&config.endpoint)
            .await
            .expect("connect");

        let outcome = serve(socket, &mut engine, &config, &mut shutdown).await;
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
