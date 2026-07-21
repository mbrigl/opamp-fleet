//! The daemon runtime (ADR-0006): the OpAMP report loop plus graceful shutdown.
//!
//! This is the body the Supervisor Host runs whether it was started standalone in the foreground or
//! under a service manager. The loop itself is unchanged from the first version; what is new is that
//! it stops cleanly on a shutdown signal instead of running forever, so the init system (or the
//! Windows SCM) can stop it in a bounded time.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use opamp::transport::{DEFAULT_PORT, OPAMP_HTTP_PATH};
use opamp::InstanceUid;
use supervisor::{OpampHttpClient, Supervisor, DEFAULT_POLL};
use tracing::{info, warn};

use crate::cli::ConfigArgs;
use crate::update::health::HealthWriter;
use crate::update::layout::Layout;
use crate::update::{resume_if_pending, ResumeOutcome};

/// Resolved runtime configuration for the daemon: the CLI/env inputs turned into concrete values.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// The Server's full OpAMP endpoint URL.
    pub endpoint: String,
    /// Directory holding the Instance UID and effective configuration.
    pub state_dir: PathBuf,
    /// Interval between status reports.
    pub poll: Duration,
}

impl RuntimeConfig {
    /// Resolve raw CLI/env arguments into runtime configuration, applying the same defaults the
    /// first version used (local Server endpoint, `DEFAULT_POLL`).
    #[must_use]
    pub fn resolve(args: ConfigArgs) -> Self {
        let endpoint = args
            .server_url
            .unwrap_or_else(|| format!("http://127.0.0.1:{DEFAULT_PORT}{OPAMP_HTTP_PATH}"));
        let poll = args.poll_seconds.map_or(DEFAULT_POLL, Duration::from_secs);
        Self {
            endpoint,
            state_dir: args.state_dir,
            poll,
        }
    }
}

/// Build a multi-threaded runtime and run the daemon until a shutdown signal (`SIGTERM`/`SIGINT`, or
/// Ctrl-C on Windows). Used by the standalone foreground path.
///
/// # Errors
/// Returns an error if the runtime cannot be built or the report loop fails to start.
pub fn run_foreground(config: RuntimeConfig) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?;
    runtime.block_on(run_until_shutdown(config, shutdown_signal()))
}

/// Run the report loop until `shutdown` resolves, then return. Shared by the foreground path and the
/// Windows SCM shim, which supply different shutdown futures (OS signals vs. an SCM stop request).
///
/// # Errors
/// Returns an error if the persistent Instance UID cannot be loaded/created or the OpAMP client
/// cannot be constructed. Transient report failures are logged and retried, not surfaced.
pub async fn run_until_shutdown<S: Future<Output = ()>>(
    config: RuntimeConfig,
    shutdown: S,
) -> Result<()> {
    // Recover from an update interrupted by a crash before serving (ADR-0007).
    let layout = Layout::new(&config.state_dir);
    match resume_if_pending(&layout) {
        Ok(ResumeOutcome::Committed) => {
            info!("resumed an interrupted update: the switch had completed")
        }
        Ok(ResumeOutcome::Aborted) => {
            warn!("recovered from an interrupted update: the switch had not completed");
        }
        Ok(ResumeOutcome::Nothing) => {}
        Err(err) => warn!(error = format!("{err:#}"), "update resume check failed"),
    }

    let instance_uid = load_or_create_instance_uid(&config.state_dir)?;
    let config_path = config.state_dir.join("effective-config.txt");
    let effective_config = std::fs::read(&config_path).unwrap_or_default();

    info!(%instance_uid, endpoint = config.endpoint, "Supervisor Host starting");

    let mut supervisor = Supervisor::new(instance_uid, config_path, effective_config);
    let client =
        OpampHttpClient::new(config.endpoint.clone()).context("creating the OpAMP client")?;
    let uid = supervisor.instance_uid();

    // Publish the local health signal the self-update gate reads (ADR-0007): healthy as soon as the
    // loop is up, and `server_reported` once a round-trip has succeeded.
    let health = HealthWriter::new(layout.health_file());
    let mut server_reported = false;
    if let Err(err) = health.publish(server_reported) {
        warn!(
            error = format!("{err:#}"),
            "failed to publish initial health"
        );
    }

    tokio::pin!(shutdown);
    loop {
        if report_once(&mut supervisor, &client, &uid).await {
            server_reported = true;
        }
        if let Err(err) = health.publish(server_reported) {
            warn!(error = format!("{err:#}"), "failed to publish health");
        }
        tokio::select! {
            () = &mut shutdown => {
                info!("shutdown requested; stopping the Supervisor Host");
                break;
            }
            () = tokio::time::sleep(config.poll) => {}
        }
    }
    Ok(())
}

/// Send one report. Returns whether the Server was successfully reached (a completed round-trip).
async fn report_once(
    supervisor: &mut Supervisor,
    client: &OpampHttpClient,
    uid: &InstanceUid,
) -> bool {
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
            true
        }
        Err(err) => {
            warn!(
                error = format!("{err:#}"),
                "status report failed; will retry"
            );
            false
        }
    }
}

/// Resolve to the platform shutdown signal: `SIGTERM` or `SIGINT` on Unix, Ctrl-C on Windows.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate =
            signal(SignalKind::terminate()).expect("installing the SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Load the Instance UID from the state directory, generating and persisting one on first run so it
/// stays stable across restarts (specification: Instance UID is stable across restarts by default).
fn load_or_create_instance_uid(state_dir: &Path) -> Result<InstanceUid> {
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
