//! Runs the OpAMP Fleet Server: it distributes the OpenTelemetry Collector configuration to the agents
//! that connect to it, and pushes a new configuration whenever the file it reads changes.
//!
//! In the development environment (ADR-0003) the agents are the sidecars — the upstream OpAMP
//! Supervisor, Bindplane's agent, and the Splunk collector — which connect to `ws://dev:4320/v1/opamp`.
//! The OpAMP endpoint and the fleet UI + REST API run on two listeners so the agent-facing and
//! human-facing ports can be exposed, forwarded, and firewalled independently (ADR-0006, ADR-0007).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;
use tokio::sync::{broadcast, watch};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use opamp::api;
use opamp::auth;
use opamp::config::ConfigSource;
use opamp::fleet::Fleet;
use opamp::server::{
    self, AppState, FleetPush, OpampConnectionOffer, ServerOffers, TelemetryOffer, LISTEN_PATH,
};
use opamp::ui::{self, UiState};

/// The command-line configuration, mirroring the flags the README documents.
struct Options {
    endpoint: String,
    ui_endpoint: String,
    config_path: String,
    poll_interval: Duration,
    /// The heartbeat interval to offer agents (ADR-0011); `None` offers none.
    heartbeat_interval: Option<Duration>,
    /// The OTLP/HTTP destination to offer for agents' own telemetry (ADR-0011); `None` offers none.
    own_telemetry_endpoint: Option<String>,
    /// Optional headers attached to the own-telemetry offer (e.g. an auth token).
    own_telemetry_headers: Vec<(String, String)>,
    /// New OpAMP endpoint to offer accepting agents, e.g. to migrate or rotate a token (ADR-0015).
    opamp_offer_endpoint: Option<String>,
    /// Optional headers attached to the OpAMP connection offer.
    opamp_offer_headers: Vec<(String, String)>,
    /// PEM certificate and key for TLS on both listeners (ADR-0012); `None` serves plain.
    tls_cert: Option<String>,
    tls_key: Option<String>,
    /// The shared bearer token agents and UI/API clients must present (ADR-0012); `None` authenticates
    /// nobody. A leading `@` means "read the token from this file".
    auth_token: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            endpoint: "0.0.0.0:4320".to_string(),
            ui_endpoint: "0.0.0.0:4321".to_string(),
            config_path: "config/collector.yaml".to_string(),
            poll_interval: Duration::from_secs(2),
            heartbeat_interval: None,
            own_telemetry_endpoint: None,
            own_telemetry_headers: Vec::new(),
            opamp_offer_endpoint: None,
            opamp_offer_headers: Vec::new(),
            tls_cert: None,
            tls_key: None,
            auth_token: None,
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

    // Install the rustls crypto provider (ring) once for the whole process, so TLS on either listener
    // uses it rather than a compiled-in default (ADR-0012).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut opts = match parse_args() {
        Ok(opts) => opts,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    // Resolve the auth token (a literal, or @file) and build the optional TLS configuration (ADR-0012).
    let auth_token = match opts.auth_token.take().map(resolve_token).transpose() {
        Ok(token) => token,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let tls = match (opts.tls_cert.take(), opts.tls_key.take()) {
        (Some(cert), Some(key)) => match RustlsConfig::from_pem_file(&cert, &key).await {
            Ok(config) => Some(config),
            Err(e) => {
                eprintln!("cannot load TLS certificate/key ({cert}, {key}): {e}");
                std::process::exit(1);
            }
        },
        (None, None) => None,
        _ => {
            eprintln!("-tls-cert and -tls-key must be given together");
            std::process::exit(2);
        }
    };
    let scheme = if tls.is_some() {
        "wss/https"
    } else {
        "ws/http"
    };
    let auth = if auth_token.is_some() {
        "shared-token auth ON"
    } else {
        "UNAUTHENTICATED"
    };

    // The control offers the Server makes beyond config distribution (ADR-0011), from the flags above.
    let offers = ServerOffers {
        heartbeat_interval_seconds: opts.heartbeat_interval.map_or(0, |d| d.as_secs()),
        own_telemetry: opts
            .own_telemetry_endpoint
            .take()
            .map(|endpoint| TelemetryOffer {
                endpoint,
                headers: std::mem::take(&mut opts.own_telemetry_headers),
            }),
        opamp_connection: opts
            .opamp_offer_endpoint
            .take()
            .map(|endpoint| OpampConnectionOffer {
                endpoint,
                headers: std::mem::take(&mut opts.opamp_offer_headers),
            }),
    };

    let config = Arc::new(ConfigSource::new(&opts.config_path));
    // Fail fast: a server that starts without a configuration would silently leave the fleet running
    // whatever it happens to have.
    if let Err(e) = config.reload() {
        error!(path = %opts.config_path, error = %e, "cannot read collector configuration");
        std::process::exit(1);
    }

    let fleet = Arc::new(Fleet::new());

    // Announce the security posture. Without TLS and a token the server is only defensible on a trusted
    // network / in the dev environment (ADR-0006, ADR-0012).
    info!(transport = scheme, auth, "server security posture");
    if auth_token.is_none() {
        info!("the server authenticates nobody — set -auth-token (and -tls-cert/-tls-key) before exposing it");
    }

    // A bounded channel: a connection that falls this far behind recovers via the hash comparison on
    // its next report, so dropping stale pushes for a slow agent is safe.
    let (pushes, _) = broadcast::channel::<FleetPush>(16);

    let app_state = Arc::new(AppState::new(
        config.clone(),
        fleet.clone(),
        pushes.clone(),
        offers,
        auth_token.clone(),
    ));

    // One shutdown signal drives both listeners: a background task flips the watch, and each server
    // awaits it. Calling `ctrl_c()` once per server would race over who receives the signal.
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    tokio::spawn(await_shutdown(shutdown_tx));

    tokio::spawn(poll_config(
        config.clone(),
        pushes.clone(),
        opts.poll_interval,
    ));

    let opamp = serve(
        &opts.endpoint,
        server::router(app_state),
        shutdown_rx.clone(),
        format!("OpAMP server listening (path {LISTEN_PATH})"),
        tls.clone(),
    );

    // The UI listener also serves the JSON REST API under /api (ADR-0007). It holds the push channel so
    // a restart request reaches the agent's connection (ADR-0011), and the shared token gates every
    // request (ADR-0012).
    let ui_state = UiState {
        fleet,
        config,
        pushes,
    };
    let ui_router = ui::router(ui_state.clone())
        .merge(api::router(ui_state))
        .layer(axum::middleware::from_fn_with_state(
            auth_token.clone(),
            auth::require_auth,
        ));
    let ui = serve(
        &opts.ui_endpoint,
        ui_router,
        shutdown_rx,
        "fleet UI + REST API listening".to_string(),
        tls,
    );

    // Bring both up; if either cannot bind, the whole server is broken, so report and exit non-zero.
    let (opamp, ui) = tokio::join!(opamp, ui);
    if opamp.is_err() || ui.is_err() {
        std::process::exit(1);
    }
    info!("shutdown complete");
}

/// Binds `addr` and serves `router` — over TLS when `tls` is given (ADR-0012), plain otherwise — until
/// `shutdown` fires. Returns an error if the address cannot be parsed or the listener cannot bind.
async fn serve(
    addr: &str,
    router: axum::Router,
    mut shutdown: watch::Receiver<()>,
    announce: String,
    tls: Option<RustlsConfig>,
) -> Result<(), ()> {
    let socket: SocketAddr = match addr.parse() {
        Ok(s) => s,
        Err(e) => {
            error!(addr, error = %e, "cannot parse listen address (expected IP:port)");
            return Err(());
        }
    };

    // axum-server drives graceful shutdown through a Handle rather than a shutdown future, so a small
    // task turns the shutdown watch into a Handle signal, giving in-flight requests a moment to finish.
    let handle = Handle::new();
    tokio::spawn({
        let handle = handle.clone();
        async move {
            let _ = shutdown.changed().await;
            handle.graceful_shutdown(Some(Duration::from_secs(2)));
        }
    });

    info!(addr = %socket, tls = tls.is_some(), "{announce}");
    let service = router.into_make_service();
    let result = match tls {
        Some(config) => {
            axum_server::bind_rustls(socket, config)
                .handle(handle)
                .serve(service)
                .await
        }
        None => {
            axum_server::bind(socket)
                .handle(handle)
                .serve(service)
                .await
        }
    };
    if let Err(e) = result {
        error!(addr = %socket, error = %e, "server stopped with an error");
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
    pushes: broadcast::Sender<FleetPush>,
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
                let _ = pushes.send(FleetPush::Config(Arc::new(cfg)));
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
            "heartbeat-interval" => opts.heartbeat_interval = Some(parse_duration(&value()?)?),
            "own-telemetry-endpoint" => opts.own_telemetry_endpoint = Some(value()?),
            "own-telemetry-header" => opts.own_telemetry_headers.push(parse_header(&value()?)?),
            "opamp-offer-endpoint" => opts.opamp_offer_endpoint = Some(value()?),
            "opamp-offer-header" => opts.opamp_offer_headers.push(parse_header(&value()?)?),
            "tls-cert" => opts.tls_cert = Some(value()?),
            "tls-key" => opts.tls_key = Some(value()?),
            "auth-token" => opts.auth_token = Some(value()?),
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

  -endpoint <addr>              address to accept OpAMP agent connections on (default 0.0.0.0:4320)
  -ui-endpoint <addr>           address to serve the fleet UI + REST API on (default 0.0.0.0:4321)
  -config <path>                collector configuration to distribute (default config/collector.yaml)
  -poll-interval <dur>          how often the file is checked for changes, e.g. 2s, 500ms (default 2s)
  -heartbeat-interval <dur>     heartbeat interval to offer agents, e.g. 30s (default: offer none)
  -own-telemetry-endpoint <url> OTLP/HTTP destination to offer for agents' own telemetry (default: none)
  -own-telemetry-header <k=v>   header for the own-telemetry offer; repeatable (e.g. Authorization=Bearer x)
  -opamp-offer-endpoint <url>   new OpAMP endpoint to offer accepting agents (re-point/rotate; default: none)
  -opamp-offer-header <k=v>     header for the OpAMP connection offer; repeatable
  -tls-cert <pem>               PEM certificate for TLS on both listeners (needs -tls-key; default: plain)
  -tls-key <pem>                PEM private key for TLS on both listeners (needs -tls-cert)
  -auth-token <token|@file>     shared bearer token agents and UI/API clients must present (default: none)";

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

/// Resolves an auth token: a literal, or — with a leading `@` — the trimmed contents of a file, so a
/// secret need not appear in the process arguments (ADR-0012).
fn resolve_token(spec: String) -> Result<String, String> {
    match spec.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("cannot read auth token file {path}: {e}")),
        None => Ok(spec),
    }
}

/// Parses a `key=value` header for the own-telemetry offer (ADR-0011). The value may contain `=`; only
/// the first splits key from value. An empty key is rejected.
fn parse_header(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(key, value)| (key.trim().to_string(), value.to_string()))
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| format!("cannot parse header {s:?} (expected key=value)"))
}
