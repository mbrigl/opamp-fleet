//! The plain-HTTP(S) transport (ADR-0023): one POST per exchange, polling at the configured
//! interval (the Baseline's default: 30 seconds), with an immediate follow-up when something
//! changed — so a config outcome is acknowledged now, not a poll later.
//!
//! Every Agent the [`Engine`] holds is polled each cycle — one exchange per Agent, since a
//! plain-HTTP exchange carries exactly one `AgentToServer`; the shared connection pool of the
//! HTTP client is the m = 1 of ADR-0034 here.

use std::time::Duration;

use opamp::proto::{AgentToServer, ServerToAgent};
use prost::Message;
use tracing::{info, warn};

use crate::config::ClientConfig;
use crate::engine::Engine;
use crate::service::runtime::Shutdown;

use opamp::endpoint::PROTOBUF_CONTENT_TYPE;

pub async fn run(
    shutdown: &mut Shutdown,
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        // The OpAMP endpoint is a fixed, operator-configured address; it never legitimately
        // redirects, so following one would only let a compromised or misconfigured Server bounce
        // the authenticated session elsewhere. Refuse them.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30));
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
            reports = engine.owed_reports();
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
}

/// One exchange: `AgentToServer` out, `ServerToAgent` back — both under `limit`, the size limit
/// the Baseline requires on this transport in either direction.
async fn exchange(
    client: &reqwest::Client,
    endpoint: &str,
    report: AgentToServer,
    limit: usize,
    // The send side: a request past the limit is not made at all.
    let body = report.encode_to_vec();
    if body.len() > limit {
            "discarding a report of {} bytes: it exceeds the {limit}-byte message size limit",
            body.len()
    }
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, PROTOBUF_CONTENT_TYPE)
        .send()
        .await
    let status = response.status();
    if !status.is_success() {
    }
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
        let err = exchange(
            &reqwest::Client::new(),
        let err = exchange(
            &reqwest::Client::new(),
        let err = exchange(
            &reqwest::Client::new(),
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

        // The same exchange with room for the body gets past the limit check and fails only on
        // the payload not being a message — proof the limit, not the transport, refused it above.
        let err = exchange(&client, &url, AgentToServer::default(), 8192)
            .await
            .expect_err("the body is not a valid message");
    }
}
