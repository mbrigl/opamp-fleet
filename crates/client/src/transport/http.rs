//! The plain-HTTP(S) transport (ADR-0007): one POST per exchange, polling at the configured
//! interval (the Baseline's default: 30 seconds), with an immediate follow-up when something
//! changed — so a config outcome is acknowledged now, not a poll later.

use std::time::Duration;

use opamp::proto::{AgentToServer, ServerToAgent};
use prost::Message;
use tracing::{info, warn};

use crate::agent::Agent;
use crate::config::ClientConfig;

/// The media type the Baseline requires the Client to set.
const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";

pub async fn run(mut agent: Agent, config: &ClientConfig) -> Result<(), String> {
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
    agent.force_full();

    loop {
        let report = agent.next_report();
        let mut immediately = false;
        match exchange(&client, &config.endpoint, report).await {
            Ok(reply) => {
                let handled = agent.handle(&reply);
                if let Some(delay) = handled.retry_after {
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = tokio::signal::ctrl_c() => break,
                    }
                    continue;
                }
                immediately = handled.send_report;
            }
            Err(e) => {
                warn!(error = %e, "exchange failed");
                // The report was lost; make the next one a full snapshot so the Server can rebuild.
                agent.force_full();
            }
        }
        if immediately {
            continue;
        }
        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    // The Baseline: the final message sets agent_disconnect.
    let goodbye = agent.disconnect_message();
    let _ = exchange(&client, &config.endpoint, goodbye).await;
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
