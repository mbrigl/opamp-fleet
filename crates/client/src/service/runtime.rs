//! The daemon body (ADR-0010): the same loop whether started standalone in the foreground or
//! under a service manager, stopping cleanly on a shutdown request instead of running forever.
//!
//! systemd and launchd stop a service with `SIGTERM`; the Windows SCM delivers a Stop control.
//! Both funnel into one [`Shutdown`] handle the transports select on, so the clean-shutdown
//! `agent_disconnect` goodbye (the Baseline's final message) fires on every stop path, not only
//! on Ctrl-C.

use std::path::PathBuf;

use tokio::sync::watch;

use crate::config::{ClientConfig, TransportKind};
use crate::connection;
use crate::supervisor;
use crate::transport::{self, RunOutcome};

/// What a daemon run needs to know: where the configuration file is, and an optional state-dir
/// override (`--state-dir`, baked into installed units so they never depend on a relative path).
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// Path to `supervisor.toml` (ADR-0008); defaults apply if the file does not exist.
    pub config_path: PathBuf,
    /// Overrides the configuration file's `state_dir` when present.
    pub state_dir: Option<PathBuf>,
    /// Started by the machine's service manager rather than by a person (ADR-0041). Set from the
    /// hidden `--service` marker `service install` writes into the command line on every platform,
    /// and the whole of the condition for writing the log file: it says no terminal is watching.
    pub service: bool,
}

/// How a daemon run ended, which decides how the process leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The operator stopped it. A clean exit, and the service manager leaves it stopped.
    Normal,
    /// A self-update switched to a new version (ADR-0020). The process must exit *non-zero* so
    /// the manager's restart-on-failure brings the new version up: none of the three managers
    /// offers "restart on success", and issuing the restart from inside the unit deadlocks.
    RestartForUpdate,
}

/// A multi-use shutdown handle: resolves once shutdown is requested, immediately when it already
/// was — the transports await it at several points in their loops.
#[derive(Debug, Clone)]
pub struct Shutdown(watch::Receiver<bool>);

impl Shutdown {
    /// Wait until shutdown has been requested (returns immediately if it already was).
    pub async fn requested(&mut self) {
        while !*self.0.borrow_and_update() {
            if self.0.changed().await.is_err() {
                // The requesting side is gone; treat that as a shutdown rather than hang.
                return;
            }
        }
    }
}

/// Starts Gateway Mode when `[gateway]` arms it (ADR-0037), or nothing when it does not.
///
/// It runs as its own task rather than inside the transport loop: the downstream endpoint's
/// lifetime is the process's, not one upstream connection's, and an Agent behind the Gateway must
/// not lose its endpoint because this Client's own connection dropped.
fn spawn_gateway(
    config: &ClientConfig,
    shutdown: &Shutdown,
) -> Option<tokio::task::JoinHandle<()>> {
    config.gateway.as_ref()?;
    let config = std::sync::Arc::new(config.clone());
    let shutdown = shutdown.clone();
    Some(tokio::spawn(async move {
        if let Err(e) = crate::gateway::run(config, shutdown).await {
            tracing::error!(error = %e, "gateway mode stopped");
        }
    }))
}

/// Create the pair: the sender flips shutdown on, every [`Shutdown`] clone observes it.
#[must_use]
pub fn shutdown_channel() -> (watch::Sender<bool>, Shutdown) {
    let (tx, rx) = watch::channel(false);
    (tx, Shutdown(rx))
}

/// Build the runtime and run the daemon until the platform shutdown signal (`SIGTERM`/`SIGINT` on
/// Unix, Ctrl-C on Windows). The standalone foreground path; the Windows SCM shim supplies its own
/// runtime and shutdown source and calls [`run_until_shutdown`] directly.
///
/// # Errors
/// Returns an error if the runtime cannot be built or the daemon fails to start.
pub fn run_foreground(spec: RunSpec) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot build the tokio runtime: {e}"))?;
    let exit = runtime.block_on(async {
        let (tx, shutdown) = shutdown_channel();
        tokio::spawn(async move {
            shutdown_signal().await;
            let _ = tx.send(true);
        });
        #[cfg(unix)]
        tokio::spawn(ignore_sighup());
        run_until_shutdown(spec, shutdown).await
    })?;
    if exit == Exit::RestartForUpdate {
        // Not an error, but it has to look like one: systemd's `Restart=on-failure` and launchd's
        // `KeepAlive{SuccessfulExit:false}` are the only "bring it back" either offers, and a
        // clean exit is precisely what tells them not to (ADR-0010, ADR-0020).
        tracing::info!("exiting so the service manager starts the newly installed version");
        std::process::exit(crate::selfupdate::EXIT_RESTART_FOR_UPDATE);
    }
    Ok(())
}

/// Opens the Client's own log file for a service run (ADR-0041).
///
/// **A log that cannot be written never stops the Client.** A directory the installer did not make
/// writable, or a full disk, is reported on stderr — which the SCM discards, which is the whole
/// problem, so it also stays visible to anyone running the same command by hand — and the run
/// continues. A monitoring agent that refuses to start because it could not open its own log has
/// turned a diagnostic into an outage.
fn start_log_file(config: &crate::config::ClientConfig) {
    if !config.logging.enabled {
        return;
    }
    let dir = config
        .logging
        .dir
        .clone()
        .unwrap_or_else(|| crate::logging::default_dir(&config.state_dir));
    match crate::logging::open(&dir, config.logging.keep) {
        Ok(dir) => {
            tracing::info!(dir = %dir.display(), keep = config.logging.keep, "logging to file")
        }
        Err(e) => tracing::warn!(error = %e, "running without a log file"),
    }
}

/// Load the configuration, build the Engine (the configured Supervisors, or the self-Agent when
/// none are), and run the transport the endpoint selects (ADR-0007) until `shutdown` fires.
///
/// # Errors
/// Returns an error if the configuration cannot be loaded or the Agent state cannot be restored.
pub async fn run_until_shutdown(spec: RunSpec, mut shutdown: Shutdown) -> Result<Exit, String> {
    heal_torn_pointer();
    let mut config = load_effective_config(&spec)?;
    if spec.service {
        start_log_file(&config);
    }

    // Resolve any self-update in flight before anything else runs (ADR-0020): this process may be
    // a freshly installed version on probation, or the previous one brought back after a rollback.
    let startup = crate::selfupdate::on_start(&config.state_dir)?;
    let (probation, owed_outcome) = match startup {
        crate::selfupdate::Startup::Ordinary => (None, None),
        crate::selfupdate::Startup::OnProbation(marker) => (Some(*marker), None),
        crate::selfupdate::Startup::Outcome(outcome) => (None, Some(*outcome)),
        // `current` now names the previous version and this one is not it. Nothing is served
        // from here; the manager restarts and the version it starts reports the failure.
        crate::selfupdate::Startup::RolledBack(_) => return Ok(Exit::RestartForUpdate),
    };

    let mut engine = supervisor::build_engine(&config, &shutdown)?;
    if config.self_update_package().is_some() {
        engine.arm_self_update(
            config.state_dir.clone(),
            config.packages.as_ref().and_then(|p| p.archive_key.clone()),
            probation,
        );
    }
    // Signing is opt-in (ADR-0015): with no `[packages] verification_key`, an offered artifact — a
    // managed process's package or this Client's own self-update — is accepted on the Server-supplied
    // content hash alone, with no signature binding those bytes to a key the operator holds. That is
    // a deliberate posture, not a bug, but it is one an operator should choose knowingly, so say so
    // loudly at startup rather than only in the code path that acts on it.
    if config.package_key().is_none() && engine.installs_packages() {
        tracing::warn!(
            "accepting packages without a signature check: no [packages] verification_key is set, so \
             an offered package or self-update is trusted on the Server's content hash alone \
             (ADR-0015). Set verification_key to require an Ed25519 signature."
        );
    }
    if let Some(outcome) = &owed_outcome {
        // The install finished in another process; this one owes the Server its terminal status.
        engine.report_self_update_outcome(outcome);
        crate::selfupdate::clear_outcome(&config.state_dir);
    }
    // Own telemetry (ADR-0036) is owned here rather than by a transport loop, because the
    // destinations outlive a connection: a reconnect must not tear the exporters down, and a
    // verified new offer is what replaces them.
    let mut telemetry = crate::telemetry::Telemetry::new();
    let mut system = sysinfo::System::new();
    let sampling = engine.sampling_handle();

    // Gateway Mode (ADR-0037), if armed: a downstream endpoint and an upstream pool, running
    // beside everything else. It is restarted when a verified offer moves this Client's endpoint,
    // since the pool dials that endpoint and would otherwise keep reaching for the old one.
    let mut gateway = spawn_gateway(&config, &shutdown);
    if let Some(stored) = connection::load(&config.state_dir) {
        // A restarted Client reports the persisted settings APPLIED, so the Server does not
        // re-offer what it already runs (ADR-0014).
        engine.adopt_connection_settings(&stored.hash);
        // …and resumes reporting to the destinations it was last told about, before it has spoken
        // to anyone: telemetry from a Client that cannot reach the Server is the useful kind.
        let refused = telemetry.apply(&stored, &engine.self_description(), &config);
        if !refused.is_empty() {
            // And the Server hears about it. The line above just reported these settings APPLIED;
            // saying nothing here would have this Client claim, on every reconnect for the life of
            // the state directory, that a destination is in force which it refused to use
            // (ADR-0036: refused *and reported*).
            let error = refused.join("; ");
            tracing::warn!(reason = %error, "not reporting own telemetry");
            engine.connection_settings_outcome(&stored.hash, Err(&error));
        }
    }
    for uid in engine.uids() {
        tracing::info!(agent = %uid, "starting");
    }

    loop {
        // The sampler runs beside the transport, not inside it: process metrics are about the host,
        // and a Client that has lost its connection is exactly when they are worth having.
        let outcome = {
            let transport = async {
                match config.transport()? {
                    TransportKind::WebSocket => {
                        transport::ws::run(&mut engine, &mut config, &mut shutdown).await
                    }
                    TransportKind::Http => {
                        transport::http::run(&mut engine, &mut config, &mut shutdown).await
                    }
                }
            };
            tokio::pin!(transport);
            let mut tick = tokio::time::interval(telemetry.sample_interval());
            tick.tick().await; // the first tick is immediate; sample on the ones after it
            loop {
                tokio::select! {
                    outcome = &mut transport => break outcome?,
                    _ = tick.tick(), if telemetry.reporting() => {
                        let targets = sampling.lock().map(|t| t.clone()).unwrap_or_default();
                        for (agent, pid) in targets {
                            telemetry.sample(&mut system, pid, &agent);
                        }
                    }
                }
            }
        };
        match outcome {
            // Both exits flush first: the batch exporters hold spans and log records that have not
            // left yet, and a process that simply returns drops them. The stop path is exactly when
            // the last records are worth having — a crash-and-restart is what they explain.
            RunOutcome::Shutdown => {
                telemetry.shutdown();
                return Ok(Exit::Normal);
            }
            RunOutcome::RestartForUpdate => {
                telemetry.shutdown();
                return Ok(Exit::RestartForUpdate);
            }
            // Verified connection settings took effect (ADR-0014): re-resolve the effective
            // configuration — endpoint, credential, intervals, possibly the other transport —
            // and reconnect. The Engine (and its Managed Processes) carries on.
            RunOutcome::Reconfigured => {
                config = load_effective_config(&spec)?;
                // The same verified offer may have named new telemetry destinations (ADR-0036).
                if let Some(stored) = connection::load(&config.state_dir) {
                    let refused = telemetry.apply(&stored, &engine.self_description(), &config);
                    if !refused.is_empty() {
                        // The transport reported the OpAMP half APPLIED before it handed control
                        // back; a refusal here is the rest of the same offer, and it corrects that
                        // acknowledgement rather than letting it stand. The correction rides the
                        // next connection's reports, and the Server's gate stops re-offering on any
                        // terminal status, so a FAILED after an APPLIED ends the offer rather than
                        // restarting it.
                        let error = refused.join("; ");
                        tracing::warn!(reason = %error, "not reporting own telemetry");
                        engine.connection_settings_outcome(&stored.hash, Err(&error));
                    }
                }
                if let Some(handle) = gateway.take() {
                    handle.abort();
                    // The listener is only released once the task has unwound; binding the new
                    // one before that races an "address already in use" that nothing retries.
                    let _ = handle.await;
                }
                gateway = spawn_gateway(&config, &shutdown);
                if config.heartbeat_interval_secs > 0 {
                    // An offered interval may enable what the file had disabled; the capability
                    // follows (the reverse never happens — 0 means "not offered").
                    engine
                        .declare_capability_all(opamp::proto::AgentCapabilities::ReportsHeartbeat);
                }
            }
        }
    }
}

/// The configuration in force: `supervisor.toml` (ADR-0008), the `--state-dir` override, and the
/// persisted Server-offered connection settings on top (ADR-0014).
fn load_effective_config(spec: &RunSpec) -> Result<ClientConfig, String> {
    let mut config = ClientConfig::load(&spec.config_path)?;
    if let Some(state_dir) = &spec.state_dir {
        config.state_dir = state_dir.clone();
    }
    if let Some(stored) = connection::load(&config.state_dir) {
        connection::apply(&mut config, &stored);
    }
    Ok(config)
}

/// ADR-0010 self-heal: when running from a versioned install layout, make sure `current`
/// resolves to the directory this binary actually runs from — a crash mid-switch otherwise
/// leaves the pointer torn. Best-effort: a plain foreground run outside a layout is untouched.
fn heal_torn_pointer() {
    let Ok(exe) = super::layout::running_exe() else {
        return;
    };
    let Some((layout, running_dir)) = super::layout::Layout::enclosing(&exe) else {
        return;
    };
    match layout.heal_current(&running_dir) {
        Ok(true) => tracing::warn!(
            current = %layout.current().display(),
            "repaired the current pointer after a torn version switch"
        ),
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "cannot verify the current pointer"),
    }
}

/// Resolve to the platform shutdown signal: `SIGTERM` or `SIGINT` on Unix (what systemd and
/// launchd send on stop), Ctrl-C on Windows (a console run; the SCM path never comes through
/// here).
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

/// daemon(7) reserves `SIGHUP` for a configuration reload. Until that exists it is explicitly
/// ignored — the default disposition would terminate the daemon.
#[cfg(unix)]
async fn ignore_sighup() {
    use tokio::signal::unix::{signal, SignalKind};
    let Ok(mut hangup) = signal(SignalKind::hangup()) else {
        return;
    };
    while hangup.recv().await.is_some() {
        tracing::debug!("SIGHUP ignored (reserved for a future configuration reload)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requested_resolves_after_the_flip_and_immediately_thereafter() {
        let (tx, mut shutdown) = shutdown_channel();
        tx.send(true).expect("send shutdown");
        // Resolves at once — and again on a second await (multi-use).
        shutdown.requested().await;
        shutdown.requested().await;
    }

    #[tokio::test]
    async fn a_dropped_sender_counts_as_shutdown() {
        let (tx, mut shutdown) = shutdown_channel();
        drop(tx);
        shutdown.requested().await;
    }
}
