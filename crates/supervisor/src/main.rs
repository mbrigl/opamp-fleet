//! OpAMP Fleet Supervisor Host — the client process that runs Supervisors.
//!
//! This first version runs a single Supervisor (specification: "close the loop before widening
//! it"). It loads or generates a persistent Instance UID, then reports to the Server on an interval
//! and applies any remote configuration it receives (ADR-0004).

use std::path::PathBuf;

use anyhow::{Context, Result};
use opamp::transport::{DEFAULT_PORT, OPAMP_HTTP_PATH};
use opamp::InstanceUid;
use supervisor::{OpampHttpClient, Supervisor, DEFAULT_POLL};
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let endpoint = std::env::var("OPAMP_SERVER_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{DEFAULT_PORT}{OPAMP_HTTP_PATH}"));
    let state_dir = PathBuf::from(
        std::env::var("OPAMP_STATE_DIR").unwrap_or_else(|_| "./supervisor-state".to_string()),
    );

    let instance_uid = load_or_create_instance_uid(&state_dir)?;
    let config_path = state_dir.join("effective-config.txt");
    let effective_config = std::fs::read(&config_path).unwrap_or_default();

    info!(%instance_uid, endpoint, "Supervisor Host starting");

    let mut supervisor = Supervisor::new(instance_uid, config_path, effective_config);
    let client = OpampHttpClient::new(endpoint).context("creating the OpAMP client")?;

    let poll = std::env::var("OPAMP_POLL_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DEFAULT_POLL, std::time::Duration::from_secs);

    run(&mut supervisor, &client, poll).await
}

/// The report loop: report, apply any offered config, re-report immediately if it changed, sleep.
async fn run(
    supervisor: &mut Supervisor,
    client: &OpampHttpClient,
    poll: std::time::Duration,
) -> Result<()> {
    let uid = supervisor.instance_uid();
    loop {
        report_once(supervisor, client, &uid).await;
        tokio::time::sleep(poll).await;
    }
}

async fn report_once(supervisor: &mut Supervisor, client: &OpampHttpClient, uid: &InstanceUid) {
    let message = supervisor.build_message();
    match client.send(uid, &message).await {
        Ok(reply) => {
            if supervisor.handle_response(reply) {
                info!("applied remote configuration; reporting new status");
                let followup = supervisor.build_message();
                if let Err(err) = client.send(uid, &followup).await {
                    warn!(
                        error = format!("{err:#}"),
                        "failed to report applied status"
                    );
                }
            }
        }
        Err(err) => warn!(
            error = format!("{err:#}"),
            "status report failed; will retry"
        ),
    }
}

/// Load the Instance UID from the state directory, generating and persisting one on first run so it
/// stays stable across restarts (specification: Instance UID is stable across restarts by default).
fn load_or_create_instance_uid(state_dir: &std::path::Path) -> Result<InstanceUid> {
    let path = state_dir.join("instance-uid");
    if let Ok(contents) = std::fs::read_to_string(&path) {
        if let Ok(uid) = InstanceUid::parse_str(contents.trim()) {
            return Ok(uid);
        }
    }
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating state directory {}", state_dir.display()))?;
    let uid = InstanceUid::generate();
    std::fs::write(&path, uid.to_string())
        .with_context(|| format!("persisting Instance UID to {}", path.display()))?;
    Ok(uid)
}
