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
use crate::supervisor;
use crate::transport;

/// What a daemon run needs to know: where the configuration file is, and an optional state-dir
/// override (`--state-dir`, baked into installed units so they never depend on a relative path).
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// Path to `client.toml` (ADR-0008); defaults apply if the file does not exist.
    pub config_path: PathBuf,
    /// Overrides the configuration file's `state_dir` when present.
    pub state_dir: Option<PathBuf>,
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
    runtime.block_on(async {
        let (tx, shutdown) = shutdown_channel();
        tokio::spawn(async move {
            shutdown_signal().await;
            let _ = tx.send(true);
        });
        #[cfg(unix)]
        tokio::spawn(ignore_sighup());
        run_until_shutdown(spec, shutdown).await
    })
}

/// Load the configuration, build the Engine (the configured Supervisors, or the self-Agent when
/// none are), and run the transport the endpoint selects (ADR-0007) until `shutdown` fires.
///
/// # Errors
/// Returns an error if the configuration cannot be loaded or the Agent state cannot be restored.
pub async fn run_until_shutdown(spec: RunSpec, mut shutdown: Shutdown) -> Result<(), String> {
    heal_torn_pointer();
    let mut config = ClientConfig::load(&spec.config_path)?;
    if let Some(state_dir) = spec.state_dir {
        config.state_dir = state_dir;
    }
    let transport = config.transport()?;

    let engine = supervisor::build_engine(&config, &shutdown)?;
    for uid in engine.uids() {
        tracing::info!(agent = %uid, "starting");
    }

    match transport {
        TransportKind::WebSocket => transport::ws::run(engine, &config, &mut shutdown).await,
        TransportKind::Http => transport::http::run(engine, &config, &mut shutdown).await,
    }
}

/// ADR-0010 self-heal: when running from a versioned install layout, make sure `current`
/// resolves to the directory this binary actually runs from — a crash mid-switch otherwise
/// leaves the pointer torn. Best-effort: a plain foreground run outside a layout is untouched.
fn heal_torn_pointer() {
    let Ok(exe) = std::env::current_exe() else {
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
