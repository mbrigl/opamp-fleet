//! Runs the OpAMP Fleet Server: it distributes the OpenTelemetry Collector configuration to the agents
//! that connect to it, and pushes a new configuration whenever the file it reads changes.
//!
//! In the development environment (ADR-0003) the agents are the sidecars — the upstream OpAMP
//! Supervisor, Bindplane's agent, and the Splunk collector — which connect to `ws://dev:4320/v1/opamp`.
//! The OpAMP endpoint and the fleet UI + REST API run on two listeners so the agent-facing and
//! human-facing ports can be exposed, forwarded, and firewalled independently (ADR-0006, ADR-0007).

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use opamp::api;
use opamp::config::ConfigSource;
use opamp::fleet::Fleet;
use opamp::proto;
use opamp::server::{self, AppState, LISTEN_PATH};
use opamp::ui::{self, UiState};

/// The command-line configuration, mirroring the flags the README documents.
struct Options {
    endpoint: String,
    ui_endpoint: String,
    config_path: String,
    poll_interval: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            endpoint: "0.0.0.0:4320".to_string(),
            ui_endpoint: "0.0.0.0:4321".to_string(),
            config_path: "config/collector.yaml".to_string(),
            poll_interval: Duration::from_secs(2),
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let opts = match parse_args() {
        Ok(opts) => opts,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    let config = Arc::new(ConfigSource::new(&opts.config_path));
    // Fail fast: a server that starts without a configuration would silently leave the fleet running
    // whatever it happens to have.
    if let Err(e) = config.reload() {
        error!(path = %opts.config_path, error = %e, "cannot read collector configuration");
        std::process::exit(1);
    }

    let fleet = Arc::new(Fleet::new());

    // The initial server authenticates nobody; say so loudly, since it is only defensible on a trusted
    // network / in the dev environment (ADR-0006).
    info!(
        "the OpAMP endpoint and the fleet UI/API are UNAUTHENTICATED (trusted-network / dev only)"
    );

    // A bounded channel: a connection that falls this far behind recovers via the hash comparison on
    // its next report, so dropping stale pushes for a slow agent is safe.
    let (pushes, _) = broadcast::channel(16);

    let app_state = Arc::new(AppState::new(config.clone(), fleet.clone(), pushes.clone()));

    // One shutdown signal drives both listeners: a background task flips the watch, and each server
    // awaits it. Calling `ctrl_c()` once per server would race over who receives the signal.
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    tokio::spawn(await_shutdown(shutdown_tx));

    tokio::spawn(poll_config(config.clone(), pushes, opts.poll_interval));

    let opamp = serve(
        &opts.endpoint,
        server::router(app_state),
        shutdown_rx.clone(),
        format!("OpAMP server listening (path {LISTEN_PATH})"),
    );

    // The UI listener also serves the JSON REST API under /api (ADR-0007), plain.
    let ui_state = UiState { fleet, config };
    let ui = serve(
        &opts.ui_endpoint,
        ui::router(ui_state.clone()).merge(api::router(ui_state)),
        shutdown_rx,
        "fleet UI + REST API listening (unauthenticated — do not expose)".to_string(),
    );

    // Bring both up; if either cannot bind, the whole server is broken, so report and exit non-zero.
    let (opamp, ui) = tokio::join!(opamp, ui);
    if opamp.is_err() || ui.is_err() {
        std::process::exit(1);
    }
    info!("shutdown complete");
}

/// Binds `addr` and serves `router` until `shutdown` fires. Returns an error if binding fails.
async fn serve(
    addr: &str,
    router: axum::Router,
    mut shutdown: watch::Receiver<()>,
    announce: String,
) -> Result<(), ()> {
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!(addr, error = %e, "cannot bind listener");
            return Err(());
        }
    };
    info!(addr, "{announce}");

    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = shutdown.changed().await;
        })
        .await;
    if let Err(e) = result {
        error!(addr, error = %e, "server stopped with an error");
        return Err(());
    }
    Ok(())
}

/// Re-reads the configuration every `interval` and pushes it to the fleet whenever it changed.
/// Polling — rather than watching the filesystem — keeps this free of a dependency and behaves
/// identically across the bind-mounted workspace, where inotify events from the host are not always
/// delivered (ADR-0003).
async fn poll_config(
    config: Arc<ConfigSource>,
    pushes: broadcast::Sender<Arc<proto::AgentRemoteConfig>>,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    // The first tick fires immediately; skip it, since the initial configuration was already loaded.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        match config.reload() {
            Ok(false) => {}
            Ok(true) => {
                let Some(cfg) = config.current() else {
                    continue;
                };
                // `receiver_count` is the number of connected agents: each connection subscribes.
                let agents = pushes.receiver_count();
                info!(
                    hash = %hex::encode(&cfg.config_hash[..cfg.config_hash.len().min(6)]),
                    agents,
                    "collector configuration changed, pushing to fleet"
                );
                // An error means no connection is subscribed right now; the next agent to report
                // reconciles via the hash comparison, so there is nothing to recover here.
                let _ = pushes.send(Arc::new(cfg));
            }
            Err(e) => error!(error = %e, "cannot read collector configuration"),
        }
    }
}

/// Completes when the process is asked to stop — Ctrl-C or SIGTERM — and signals every listener.
async fn await_shutdown(shutdown: watch::Sender<()>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    info!("shutting down");
    // Dropping the sender, or sending, wakes every receiver; ignore the error if all already stopped.
    let _ = shutdown.send(());
}

/// Parses the documented flags: `-endpoint`, `-ui-endpoint`, `-config`, `-poll-interval`. Both
/// `-flag value` and `-flag=value` are accepted, with `--flag` as an alias. Unknown flags are an
/// error so a typo does not silently run with a default.
fn parse_args() -> Result<Options, String> {
    let mut opts = Options::default();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        let flag = arg.trim_start_matches('-');
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (flag, None),
        };
        let mut value = || match inline.clone() {
            Some(v) => Ok(v),
            None => args
                .next()
                .ok_or_else(|| format!("flag -{name} needs a value")),
        };

        match name {
            "endpoint" => opts.endpoint = value()?,
            "ui-endpoint" => opts.ui_endpoint = value()?,
            "config" => opts.config_path = value()?,
            "poll-interval" => opts.poll_interval = parse_duration(&value()?)?,
            "help" | "h" => {
                // Help is not an error: print usage to stdout and exit cleanly.
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag -{other}\n\n{USAGE}")),
        }
    }
    Ok(opts)
}

const USAGE: &str = "\
usage: opamp-server [flags]

  -endpoint <addr>        address to accept OpAMP agent connections on (default 0.0.0.0:4320)
  -ui-endpoint <addr>     address to serve the fleet UI + REST API on (default 0.0.0.0:4321)
  -config <path>          collector configuration to distribute (default config/collector.yaml)
  -poll-interval <dur>    how often the file is checked for changes, e.g. 2s, 500ms (default 2s)";

/// Parses a short duration string: an integer followed by `ms`, `s`, or `m`. Kept minimal on purpose
/// — the only caller is the poll interval.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let cut = |suffix: &str| s.strip_suffix(suffix).map(str::trim);
    // Check "ms" before "s": "500ms" also ends in "s".
    if let Some(n) = cut("ms") {
        return n
            .parse()
            .map(Duration::from_millis)
            .map_err(|_| bad_duration(s));
    }
    if let Some(n) = cut("s") {
        return n
            .parse()
            .map(Duration::from_secs)
            .map_err(|_| bad_duration(s));
    }
    if let Some(n) = cut("m") {
        return n
            .parse::<u64>()
            .map(|m| Duration::from_secs(m * 60))
            .map_err(|_| bad_duration(s));
    }
    Err(bad_duration(s))
}

fn bad_duration(s: &str) -> String {
    format!("cannot parse duration {s:?} (expected e.g. 2s, 500ms, 1m)")
}
