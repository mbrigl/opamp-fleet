//! The daemon body (ADR-0021): the same loop whether started standalone in the foreground or
//! under a service manager, stopping cleanly on a shutdown request instead of running forever.
//!
//! systemd and launchd stop a service with `SIGTERM`; the Windows SCM delivers a Stop control.
//! Both funnel into one [`Shutdown`] handle the transports select on, so the clean-shutdown
//! `agent_disconnect` goodbye (the Baseline's final message) fires on every stop path, not only
//! on Ctrl-C.

use std::path::PathBuf;

use tokio::sync::watch;

use crate::config::{ClientConfig, TransportKind};
use crate::supervisor;

/// What a daemon run needs to know: where the configuration file is, and an optional state-dir
/// override (`--state-dir`, baked into installed units so they never depend on a relative path).
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub config_path: PathBuf,
    /// Overrides the configuration file's `state_dir` when present.
    pub state_dir: Option<PathBuf>,
    /// Started by the machine's service manager rather than by a person (ADR-0021). Set from the
    /// hidden `--service` marker `service install` writes into the command line on every platform,
    /// and the whole of the condition for writing the log file: it says no terminal is watching.
    pub service: bool,
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

/// Starts Gateway Mode when `[gateway]` arms it (ADR-0014), or nothing when it does not.
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
        let (tx, shutdown) = shutdown_channel();
        tokio::spawn(async move {
            shutdown_signal().await;
            let _ = tx.send(true);
        });
        #[cfg(unix)]
        tokio::spawn(ignore_sighup());
        run_until_shutdown(spec, shutdown).await
}

/// Opens the Client's own log file for a service run (ADR-0021).
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

/// What this process is and what it will use, in one line each, before it uses any of it.
///
/// **The version is the point of the first line.** It rides in every report to the Server and names
/// the directory this binary runs from (ADR-0021), and until now it appeared in no log line at all
/// — so the file a self-update left behind (ADR-0020, ADR-0021) could not be attributed to the
/// version that wrote it, which is the situation that file exists for. The rest of the line is what
/// an operator otherwise has to reconstruct from the command line of a service they did not start.
///
/// **The second line is the trust and the identity in force**, resolved through the same two
/// accessors the transports build their TLS from (ADR-0012, ADR-0022) rather than read off the
/// configuration — so it states what *will* be presented, including a Server-issued certificate
/// that no `supervisor.toml` mentions. Without it, a handshake that fails because the identity is
/// not the one anybody assumed is diagnosed from the peer's error message, which is written by the
/// end that knows least about it.
fn announce(config: &ClientConfig, config_path: &std::path::Path) {
    // A missing file is not an error — `ClientConfig::load` runs on defaults, deliberately, so a
    // Client can start before anything is written. It is worth a word all the same: a mistyped
    // `--config` produces exactly this, and the result is a Client that starts cleanly, points at
    // the default endpoint and supervises nothing, which reads like a healthy start. Said here
    // rather than in `config`, which parses and does not log.
    if !config_path.exists() {
        tracing::warn!(
            config = %config_path.display(),
            "no configuration file there; running on defaults"
        );
    }
    tracing::info!(
        version = opamp::version::current(),
        config = %config_path.display(),
        state_dir = %config.state_dir.display(),
        endpoint = %config.endpoint,
        supervisors = config.supervisors.len(),
        "client starting"
    );
    let (trust, identity) = tls_posture(config);
    tracing::info!(trust = %trust, client_certificate = %identity, "outbound tls");
}

/// What the transports will trust, and what they will present — as the two strings the line above
/// reports.
///
/// Its own function because it makes a claim worth a test: the certificate named here is whichever
/// [`ClientConfig::client_identity`] resolves to, which prefers the **Server-issued** one in the
/// state directory over anything `[tls]` names (ADR-0022). A line that reported the configured
/// certificate while the connection presented the issued one would be worse than no line at all.
fn tls_posture(config: &ClientConfig) -> (String, String) {
    let trust = config.ca_file().map_or_else(
        || "the built-in roots".to_string(),
        |ca| ca.display().to_string(),
    );
    let identity = config.client_identity().map_or_else(
        || "none".to_string(),
        |(cert, _)| cert.display().to_string(),
    );
    (trust, identity)
}

/// What a run does when its configuration cannot be read: resolve the update in flight anyway, so
/// the failure counts (ADR-0020).
///
/// Returns `RestartForUpdate` when that resolution rolled the Client back — the pointer now names
/// the version that *could* read this file, and the manager must start it — and otherwise hands the
/// configuration error back to be reported and exited on, one attempt closer to that rollback.
///
/// Without this, the net catches everything except the one failure a *new* version is most likely
/// to bring: a file it refuses. A removed configuration key is exactly that
/// ([ADR-0017](../../../../docs/adr/0017-supervisor-mode-kinds-directories-and-what-the-server-may-change.md)), and for some kinds there
/// is no block both versions accept, so the cutover per host cannot be avoided — only caught.
fn unreadable_config(spec: &RunSpec, error: String) -> Result<Exit, String> {
    let state_dir = recovery_state_dir(spec);
    tracing::error!(error = %error, state_dir = %state_dir.display(), "cannot read the configuration");
    match crate::selfupdate::on_start(&state_dir) {
        Ok(crate::selfupdate::Startup::RolledBack(_)) => Ok(Exit::RestartForUpdate),
        // Counted, or nothing was in flight. Either way this run cannot continue.
        _ => Err(error),
    }
}

/// Where to look for the update marker when the configuration did not load.
///
/// Three sources, in the order of how much they can be trusted to be right. `--state-dir` is what an
/// installed service passes (`service install` bakes it into the unit), so the case that matters —
/// a service updating itself into a file it cannot read — is answered by the command line alone. A
/// foreground run falls back to reading **only** `state_dir` out of the file, leniently: the file
/// that fails to load usually parses as TOML and fails on a key's meaning, so its own answer is
/// still there to be read. What is left is the default, which is right for a Client that never
/// named one.
fn recovery_state_dir(spec: &RunSpec) -> PathBuf {
    if let Some(dir) = &spec.state_dir {
        return dir.clone();
    }
    std::fs::read_to_string(&spec.config_path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| {
            table
                .get("state_dir")
                .and_then(toml::Value::as_str)
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| ClientConfig::default().state_dir)
}

/// Load the configuration, build the Engine (the configured Supervisors, or the self-Agent when
/// none are), and run the transport the endpoint selects (ADR-0012) until `shutdown` fires.
///
/// # Errors
/// Returns an error if the configuration cannot be loaded or the Agent state cannot be restored.
    heal_torn_pointer();
    let mut config = match load_effective_config(&spec) {
        Ok(config) => config,
        // A version that cannot read this host's file is a failed update like any other, and until
        // now it was the one failure the probation of ADR-0020 could not see: the load happens
        // before `on_start`, so the process left before the attempt was counted, the manager
        // restarted it, and the host stayed on a version that never reached the Server to say so.
        Err(error) => return unreadable_config(&spec, error),
    };
    if spec.service {
        start_log_file(&config);
    }
    // After the log file, so the line that says which version is running is the first line *in the
    // file* — a log whose opening line is already about work in progress starts one step too late.
    announce(&config, &spec.config_path);


    // Gateway Mode (ADR-0014), if armed: a downstream endpoint and an upstream pool, running
    // beside everything else. It is restarted when a verified offer moves this Client's endpoint,
    // since the pool dials that endpoint and would otherwise keep reaching for the old one.
    let mut gateway = spawn_gateway(&config, &shutdown);
    for uid in engine.uids() {
        tracing::info!(agent = %uid, "starting");
    }

                if let Some(handle) = gateway.take() {
                    handle.abort();
                    // The listener is only released once the task has unwound; binding the new
                    // one before that races an "address already in use" that nothing retries.
                    let _ = handle.await;
                }
                gateway = spawn_gateway(&config, &shutdown);
    let mut config = ClientConfig::load(&spec.config_path)?;
    }
}

/// ADR-0021 self-heal: when running from a versioned install layout, make sure `current`
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

    /// The three sources, in order. `--state-dir` wins because an installed service always passes
    /// it, which is the case this recovery exists for.
    #[test]
    fn the_recovery_state_dir_prefers_the_flag_then_the_file_then_the_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("supervisor.toml");
        let named = dir.path().join("from-the-file");
        // Serialised as TOML rather than interpolated: a Windows path is full of backslashes, and
        // in a basic string `\U` is an invalid escape — the file was unparseable there, so the
        // lenient read fell back to the default and the test failed on Windows alone.
        std::fs::write(
            &config_path,
            format!(
                "state_dir = {}\n",
                toml::Value::from(named.to_string_lossy().into_owned())
            ),
        )
        .expect("write");

        let flagged = dir.path().join("from-the-flag");
        let spec = |state_dir: Option<PathBuf>, config: &std::path::Path| RunSpec {
            config_path: config.to_path_buf(),
            state_dir,
            service: false,
        };
        assert_eq!(
            recovery_state_dir(&spec(Some(flagged.clone()), &config_path)),
            flagged
        );
        assert_eq!(recovery_state_dir(&spec(None, &config_path)), named);
        assert_eq!(
            recovery_state_dir(&spec(None, &dir.path().join("absent.toml"))),
            ClientConfig::default().state_dir
        );
    }

    /// A configuration this version cannot read is a failed update attempt, and the marker has to
    /// be resolved rather than stepped over — otherwise the service manager restarts the version
    /// that refuses the file, for ever, and ADR-0020's rollback never fires.
    ///
    /// The test binary does not run from an install layout, so the resolution takes the "the new
    /// version did not take over" path: the marker is cleared and an outcome recorded. What is
    /// asserted is that the marker was *seen at all*, which before this change it was not.
    #[test]
    fn an_unreadable_configuration_resolves_the_update_in_flight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let marker = crate::selfupdate::UpdateMarker {
            previous_dir: dir.path().join("previous"),
            new_dir: dir.path().join("new"),
            version: "9.9.9".to_string(),
            package_hash_hex: "abcd".to_string(),
            attempts: 1,
            trace: None,
        };
        // `selfupdate` owns this file name; the test names it to prove the file was consumed.
        let marker_file = state_dir.join("update-marker.json");
        std::fs::write(
            &marker_file,
            serde_json::to_vec(&marker).expect("serialize"),
        )
        .expect("write the marker");

        let outcome = unreadable_config(
            &RunSpec {
                config_path: dir.path().join("supervisor.toml"),
                state_dir: Some(state_dir.clone()),
                service: true,
            },
            "supervisor \"x\": `main_config` is no longer a supervisor key".to_string(),
        );

        assert!(outcome.is_err(), "the run still ends on the configuration");
        assert!(
            !marker_file.exists(),
            "the update in flight was resolved, not stepped over"
        );
    }

    /// With nothing configured, the line says so in both halves rather than leaving an operator to
    /// read an absent field as "unknown".
    #[test]
    fn the_tls_line_reports_the_defaults_as_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = ClientConfig {
            state_dir: dir.path().to_path_buf(),
            ..ClientConfig::default()
        };
        let (trust, identity) = tls_posture(&config);
        assert_eq!(trust, "the built-in roots");
        assert_eq!(identity, "none");
    }

    /// And it reports what will actually be presented: a Server-issued pair in the state directory
    /// outranks the configured one (ADR-0022), so the line has to name the issued one — that is the
    /// whole reason it resolves the identity instead of printing `[tls] cert_file`.
    #[test]
    fn the_tls_line_names_the_issued_certificate_over_the_configured_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let configured = dir.path().join("operator-cert.pem");
        std::fs::write(&configured, b"cert").expect("write");
        std::fs::write(dir.path().join("operator-key.pem"), b"key").expect("write");
        let config = ClientConfig {
            state_dir: dir.path().to_path_buf(),
            tls: Some(crate::config::TlsConfig {
                ca_file: None,
                cert_file: Some(configured.clone()),
                key_file: Some(dir.path().join("operator-key.pem")),
            }),
            ..ClientConfig::default()
        };

        // With no issued pair on disk, the operator's is what gets presented.
        let (_, identity) = tls_posture(&config);
        assert_eq!(identity, configured.display().to_string());

        // Once the Server has issued one, that is the pair — and the line must follow.
        let issued = dir.path().join(crate::tls::ISSUED_CERT_FILE);
        std::fs::write(&issued, b"cert").expect("write");
        std::fs::write(dir.path().join(crate::tls::ISSUED_KEY_FILE), b"key").expect("write");
        let (_, identity) = tls_posture(&config);
        assert_eq!(identity, issued.display().to_string());
    }
}
