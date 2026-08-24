//! Entry point: load `server.toml`, bind the two listeners (plain or TLS) — the Agent plane and
//! the Operator plane (ADR-0066) — and serve both until interrupted.
//!
//! Both planes are served the same way whether or not TLS is configured, so that what bounds a
//! connection before it becomes a request holds on all four surfaces (ADR-0073). Only the acceptor
//! differs.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum_server::accept::DefaultAcceptor;
use axum_server::Handle;
use server::config::ServerConfig;
use server::fleet::AppState;
use server::listen;
use tracing::info;

fn usage() -> ! {
    eprintln!("Usage: server [--config <server.toml>] [--version]");
    std::process::exit(2);
}

fn parse_args() -> PathBuf {
    let mut config = PathBuf::from("server.toml");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => match args.next() {
                Some(path) => config = PathBuf::from(path),
                None => usage(),
            },
            "--version" => {
                // The baked version, not `CARGO_PKG_VERSION` (ADR-0009): the number in the file is
                // the release this build is *heading for*, and only `opamp::version::current` knows
                // whether this is it.
                println!("server {}", opamp::version::current());
                std::process::exit(0);
            }
            _ => usage(),
        }
    }
    config
}

/// Binds one plane's listener, or explains which one could not be bound and stops. A busy port is
/// an operator's mistake, not a panic — and with two listeners the message has to say *which*.
///
/// Bound up front, before either plane starts serving, so a busy port is reported as the message
/// above rather than as a failure out of a running server — and the TLS case gets that too, which
/// it did not while it bound lazily inside `serve`.
fn bind(address: SocketAddr, plane: &str) -> std::net::TcpListener {
    match std::net::TcpListener::bind(address) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("cannot bind {plane} on {address}: {e}");
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // One TLS provider for the whole process (ADR-0007): ring, never a system library.
    server::tls::install_ring_provider();

    let config_path = parse_args();
    let config = match ServerConfig::load(&config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // A credential-bearing offer with no [auth] in front of it hands that credential to anyone who
    // connects (ADR-0013/0014/0036). Open by default is intentional; leaking a backend token by
    // default is not — so it is surfaced loudly rather than gated, which would break zero-config.
    let unguarded = config.unauthenticated_secret_offers();
    if !unguarded.is_empty() {
        tracing::warn!(
            offers = %unguarded.join(", "),
            "these offers hand a credential to any Agent that connects, but [auth] is unset so the \
             OpAMP endpoint admits anyone — set [auth] to gate credential delivery (ADR-0013)"
        );
    }

    let connection_offer = match config
        .connection_offer
        .as_ref()
        .map(server::fleet::ConnectionOffer::from_config)
        .transpose()
    {
        Ok(offer) => {
            if offer.is_some() {
                // ADR-0014.
                info!("offering connection settings to the fleet");
            }
            offer
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let client_ca = match config
        .client_ca
        .as_ref()
        .map(server::ca::ClientCa::from_config)
        .transpose()
    {
        Ok(ca) => {
            if let Some(ca) = &ca {
                // ADR-0035.
                info!(
                    validity_days = ca.validity_days(),
                    "signing client certificates for Agents that ask"
                );
            }
            ca
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let telemetry_offer = config
        .telemetry_offer
        .as_ref()
        .map(server::fleet::TelemetryOffer::from_config)
        .unwrap_or_default();
    if config.telemetry_offer.is_some() {
        // ADR-0036.
        info!("offering the fleet somewhere to send its own telemetry");
    }
    let packages = match server::packages::PackageStore::open(config.packages_dir.clone()) {
        Ok(store) => {
            if !store.is_empty() {
                // ADR-0015.
                info!("offering software packages to the fleet");
            }
            match server::fleet::PackageOffering::new(
                store,
                config.advertised_url.clone().unwrap_or_default(),
            ) {
                Ok(offering) => Some(offering),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let state = match AppState::new(config.config_dir.clone()) {
        Ok(state) => Arc::new(
            state
                .with_connection_offer(connection_offer)
                .with_client_ca(client_ca)
                .with_telemetry_offer(telemetry_offer)
                .with_packages(packages)
                .with_max_message_size(config.max_message_size_bytes)
                .with_max_package_size(config.max_package_size_bytes)
                .with_max_total_package_bytes(config.max_total_package_bytes)
                .with_stale_after(std::time::Duration::from_secs(config.stale_after_secs))
                .with_max_agents(config.max_agents),
        ),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let auth = config
        .auth
        .as_ref()
        .map(server::transport::OpampAuth::from_config);
    if auth.is_some() {
        // ADR-0013.
        info!("the OpAMP endpoint requires authentication");
    }
    // Mutual TLS is on when the listener has a CA to verify client certificates against; the
    // OpAMP endpoint then requires one *in addition to* whatever `[auth]` requires (ADR-0035).
    let mutual_tls = config
        .tls
        .as_ref()
        .is_some_and(|tls| tls.client_ca_file.is_some());
    if mutual_tls {
        info!("the OpAMP endpoint requires a client certificate");
    }
    // Two planes, two listeners (ADR-0066): Agents reach the OpAMP endpoint and the package
    // downloads their offers point at; operators reach the REST API, its docs, and the UI.
    let agents = server::agent_app(
        state.clone(),
        server::transport::Admission::new(auth, mutual_tls),
    );
    let operator_auth = config
        .rest
        .auth
        .as_ref()
        .map(server::api::OperatorAuth::from_config);
    if operator_auth.is_some() {
        // ADR-0067.
        info!("the REST API and the UI require authentication");
        // Basic puts a reusable password on the wire on every request. On loopback that stays on
        // the host; published in cleartext it does not, and the operator should hear so once.
        if config.tls.is_none() && !config.rest.listen.ip().is_loopback() {
            tracing::warn!(
                listen = %config.rest.listen,
                "[rest.auth] sends its password in the clear on a listener that is not loopback — \
                 add [tls], or put a TLS-terminating proxy in front (ADR-0067)"
            );
        }
    }
    let operators = server::operator_app(state.clone(), operator_auth);

    let agent_listener = bind(config.listen, "the Agent plane");
    let operator_listener = bind(config.rest.listen, "the Operator plane");
    // One signal, both planes: the interrupt is watched once, and the handle both servers hold
    // drains them together within a bounded window (ADR-0073).
    let handle = Handle::new();
    tokio::spawn({
        let handle = handle.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutting down");
            listen::shut_down(&handle);
        }
    });

    match &config.tls {
        Some(tls) => {
            let rustls_config = match server::tls::server_config(tls) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            info!(listen = %config.listen, "serving the OpAMP endpoint and package downloads over TLS");
            info!(listen = %config.rest.listen, "serving the REST API, the API docs, and the UI over TLS");
            // The Agent plane's acceptor is its own: it is what carries the handshake's peer
            // certificate into the request the OpAMP route checks. The Operator plane needs
            // nothing of the sort — no route there reads a certificate — so it serves with the
            // same certificate and key through the plain rustls acceptor.
            let (agents, operators) = tokio::join!(
                listen::plane(
                    agent_listener,
                    server::tls::PeerCertAcceptor::new(rustls_config.clone()),
                    handle.clone(),
                )
                .serve(agents.into_make_service()),
                listen::plane(
                    operator_listener,
                    server::tls::rustls_acceptor(rustls_config),
                    handle,
                )
                .serve(operators.into_make_service()),
            );
            agents.expect("serve the Agent plane");
            operators.expect("serve the Operator plane");
        }
        None => {
            info!(listen = %config.listen, "serving the OpAMP endpoint and package downloads");
            info!(listen = %config.rest.listen, "serving the REST API, the API docs, and the UI");
            let (agents, operators) = tokio::join!(
                listen::plane(agent_listener, DefaultAcceptor::new(), handle.clone())
                    .serve(agents.into_make_service()),
                listen::plane(operator_listener, DefaultAcceptor::new(), handle)
                    .serve(operators.into_make_service()),
            );
            agents.expect("serve the Agent plane");
            operators.expect("serve the Operator plane");
        }
    }
    // The graceful-shutdown flush (ADR-0051): every record's current timestamp and sequence
    // number, so the ordinary restart restores a fleet without gaps or false silence.
    state.flush_agents();
}
