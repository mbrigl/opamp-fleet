//! The plain-HTTP(S) transport (ADR-0007): one POST per exchange, polling at the configured
//! interval (the Baseline's default: 30 seconds), with an immediate follow-up when something
//! changed — so a config outcome is acknowledged now, not a poll later.
//!
//! Every Agent the [`Engine`] holds is polled each cycle — one exchange per Agent, since a
//! plain-HTTP exchange carries exactly one `AgentToServer`; the shared connection pool of the
//! HTTP client is the m = 1 of ADR-0003 here.

use std::time::Duration;

use opamp::proto::{AgentToServer, ServerToAgent};
use prost::Message;
use tracing::{info, warn};

use crate::config::ClientConfig;
use crate::engine::Engine;
use crate::service::runtime::Shutdown;
use crate::transport::{ReportSink, RunOutcome};

/// The media type the Baseline requires the Client to set.
const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";

pub async fn run(
    engine: &mut Engine,
    config: &ClientConfig,
    shutdown: &mut Shutdown,
) -> Result<RunOutcome, String> {
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(30));
    if let Some(value) = config.authorization_value()? {
        // The Authorization header (ADR-0013, rotated per ADR-0014) rides every request, the
        // disconnect included.
        let mut value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|e| format!("the [auth] credentials are not a valid header: {e}"))?;
        value.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, value);
        builder = builder.default_headers(headers);
        if config.sends_credentials_in_cleartext() {
            warn!(
                "sending credentials over unencrypted http:// beyond the loopback — use https://"
            );
        }
    }
    if let Some(tls) = &config.tls {
        let pem = std::fs::read(&tls.ca_file)
            .map_err(|e| format!("cannot read {}: {e}", tls.ca_file.display()))?;
        let ca = reqwest::Certificate::from_pem(&pem)
            .map_err(|e| format!("cannot parse {}: {e}", tls.ca_file.display()))?;
        builder = builder
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca);
    }
    let client = builder
        .build()
        .map_err(|e| format!("cannot build the HTTP client: {e}"))?;

    let poll = Duration::from_secs(config.poll_interval_secs.max(1));
    let limit = config.max_message_size_bytes;
    info!(endpoint = %config.endpoint, interval = ?poll, "polling");
    engine.force_full_all();

    'poll: loop {
        // The routine cycle, then immediate follow-ups until no Agent owes a report — a config
        // outcome is acknowledged now, not a poll later.
        let mut reports = engine.poll_reports();
        loop {
            for report in reports {
                match exchange(&client, &config.endpoint, report, limit).await {
                    Ok(reply) => {
                        let handled = engine.handle(&reply);
                        if let Some(delay) = handled.retry_after {
                            tokio::select! {
                                _ = tokio::time::sleep(delay) => {}
                                _ = shutdown.requested() => break 'poll,
                            }
                            continue 'poll;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "exchange failed");
                        // A report was lost; full snapshots so the Server can rebuild.
                        engine.force_full_all();
                    }
                }
            }
            // A connection-settings offer (ADR-0014): verify by actually connecting. Success
            // persists the settings and leaves this loop so the runtime reconnects with them;
            // failure reports FAILED with the owed reports of the next round.
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
                        if let Err(e) = crate::connection::store(&config.state_dir, &merged) {
                            warn!(error = %e, "cannot persist the connection settings");
                        }
                        info!("connection settings verified; reconnecting with them");
                        return Ok(RunOutcome::Reconfigured);
                    }
                    Err(e) => {
                        warn!(error = %e, "offered connection settings failed verification");
                        engine.connection_settings_outcome(&offer.hash, Err(&e));
                    }
                }
            }
            // A package offer (ADR-0015): download and verify; the outcome rides the owed reports.
            let mut sink = PollSink {
                client: &client,
                endpoint: &config.endpoint,
                limit,
            };
            crate::transport::process_package_downloads(engine, config, &mut sink).await;
            reports = engine.owed_reports();
            if engine.restart_for_update() {
                // Send the owed `Installing` and stop: the pointer already points at the new
                // version, and this process exists only to get out of its way (ADR-0020).
                if !reports.is_empty() {
                    let _ = sink.send(reports).await;
                }
                return Ok(RunOutcome::RestartForUpdate);
            }
            if reports.is_empty() {
                break;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            // A Managed Process changed some Agent's state: exchange now, not at the next poll.
            _ = engine.changed() => {}
            _ = shutdown.requested() => break,
        }
    }

    // Managed Processes stop first; then the Baseline's final messages, one per Agent — which
    // v0.19.0 asks the plain-HTTP transport for too, so the Server marks the Agent disconnected
    // now instead of after a missed poll.
    engine.shutdown_processes().await;
    for goodbye in engine.disconnect_messages() {
        let _ = exchange(&client, &config.endpoint, goodbye, limit).await;
    }
    info!("disconnected");
    Ok(RunOutcome::Shutdown)
}

/// This transport's way of putting reports on the wire — one exchange each — for jobs that report
/// while they run: a package download reporting its progress (ADR-0015). The Server's replies are
/// discarded: an interim progress report asks nothing, and anything the Server wants to say is
/// picked up by the polling loop that resumes right after.
struct PollSink<'a> {
    client: &'a reqwest::Client,
    endpoint: &'a str,
    limit: usize,
}

impl crate::transport::ReportSink for PollSink<'_> {
    async fn send(&mut self, reports: Vec<AgentToServer>) -> Result<(), ()> {
        for report in reports {
            exchange(self.client, self.endpoint, report, self.limit)
                .await
                .map_err(|_| ())?;
        }
        Ok(())
    }
}

/// One exchange: `AgentToServer` out, `ServerToAgent` back — both under `limit`, the size limit
/// the Baseline requires on this transport in either direction.
async fn exchange(
    client: &reqwest::Client,
    endpoint: &str,
    report: AgentToServer,
    limit: usize,
) -> Result<ServerToAgent, String> {
    // The send side: a request past the limit is not made at all.
    let body = report.encode_to_vec();
    if body.len() > limit {
        return Err(format!(
            "discarding a report of {} bytes: it exceeds the {limit}-byte message size limit",
            body.len()
        ));
    }
    let response = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, PROTOBUF_CONTENT_TYPE)
        .body(body)
        .send()
        .await
        .map_err(|e| format!("cannot reach {endpoint}: {e}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err("the server rejected the credentials (HTTP 401) — check [auth]".to_string());
    }
    if !status.is_success() {
        return Err(format!("the server answered {status}"));
    }
    let body = read_within(response, limit).await?;
    ServerToAgent::decode(body.as_slice()).map_err(|e| format!("undecodable response: {e}"))
}

/// Reads a response body, refusing one that grows past `limit`.
///
/// The Baseline requires the Client to enforce the limit on what it *receives*, after any
/// decompression — so the body is taken chunk by chunk (reqwest has already inflated gzip by
/// then) and abandoned the moment it grows too big, rather than buffered whole and measured
/// afterwards, which is the allocation the limit exists to prevent.
async fn read_within(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("cannot read the response: {e}"))?
    {
        if body.len() + chunk.len() > limit {
            return Err(format!(
                "discarding the response: it exceeds the {limit}-byte message size limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// The Baseline: the Client MUST NOT make a request whose body exceeds the size limit — it
    /// never reaches the network, so no server has to refuse it.
    #[tokio::test]
    async fn an_oversized_report_is_never_sent() {
        let report = AgentToServer {
            instance_uid: vec![9; 512],
            ..Default::default()
        };
        // Port 1 is unreachable: if the check did not fire first, this would fail as a connection
        // error instead — which is exactly what the assertion tells apart.
        let err = exchange(
            &reqwest::Client::new(),
            "http://127.0.0.1:1/v1/opamp",
            report,
            64,
        )
        .await
        .expect_err("the report exceeds the limit");
        assert!(err.contains("exceeds"), "{err}");
    }

    /// And the receive side: a response body past the limit is discarded rather than decoded,
    /// with the limit applied as the body arrives instead of after it is buffered whole.
    #[tokio::test]
    async fn an_oversized_response_is_discarded() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        // A minimal HTTP server: one response, 4 KiB of body, no protobuf in sight — the limit
        // decides before anything is decoded.
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut scratch = [0u8; 1024];
                let _ = stream.read(&mut scratch).await;
                let body = vec![0u8; 4096];
                // `connection: close` keeps each exchange on a fresh connection: this stub serves
                // one request per connection, so a pooled keep-alive would fail the next one.
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\nconnection: close\r\ncontent-length: {}\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.flush().await;
            }
        });

        let url = format!("http://{addr}/v1/opamp");
        let client = reqwest::Client::new();
        let err = exchange(&client, &url, AgentToServer::default(), 1024)
            .await
            .expect_err("the response exceeds the limit");
        assert!(err.contains("exceeds"), "{err}");

        // The same exchange with room for the body gets past the limit check and fails only on
        // the payload not being a message — proof the limit, not the transport, refused it above.
        let err = exchange(&client, &url, AgentToServer::default(), 8192)
            .await
            .expect_err("the body is not a valid message");
        assert!(err.contains("undecodable"), "{err}");
    }
}
