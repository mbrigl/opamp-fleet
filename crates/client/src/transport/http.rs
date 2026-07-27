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

/// The media type the Baseline requires the Client to set.
const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";

pub async fn run(
    mut engine: Engine,
    config: &ClientConfig,
    shutdown: &mut Shutdown,
) -> Result<(), String> {
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(30));
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
    info!(endpoint = %config.endpoint, interval = ?poll, "polling");
    engine.force_full_all();

    'poll: loop {
        // The routine cycle, then immediate follow-ups until no Agent owes a report — a config
        // outcome is acknowledged now, not a poll later.
        let mut reports = engine.poll_reports();
        loop {
            for report in reports {
                match exchange(&client, &config.endpoint, report).await {
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

    // Managed Processes stop first; then the Baseline's final messages, one per Agent.
    engine.shutdown_processes().await;
    for goodbye in engine.disconnect_messages() {
        let _ = exchange(&client, &config.endpoint, goodbye).await;
    }
    info!("disconnected");
    Ok(())
}

/// One exchange: `AgentToServer` out, `ServerToAgent` back.
async fn exchange(
    client: &reqwest::Client,
    endpoint: &str,
    report: AgentToServer,
) -> Result<ServerToAgent, String> {
    let response = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, PROTOBUF_CONTENT_TYPE)
        .body(report.encode_to_vec())
        .send()
        .await
        .map_err(|e| format!("cannot reach {endpoint}: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the server answered {status}"));
    }
    let body = response
        .bytes()
        .await
        .map_err(|e| format!("cannot read the response: {e}"))?;
    ServerToAgent::decode(body.as_ref()).map_err(|e| format!("undecodable response: {e}"))
}
