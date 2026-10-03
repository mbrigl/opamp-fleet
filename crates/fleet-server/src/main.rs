//! Entry point: load `server.toml`, bind the two listeners (plain or TLS) — the Agent plane and
//! the Operator plane (ADR-0012) — and serve both until interrupted.
//!
//! Both planes are served the same way whether or not TLS is configured, so that what bounds a
//! connection before it becomes a request holds on all four surfaces (ADR-0012). Only the acceptor
//! differs.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use fleet_server::config::ServerConfig;
use fleet_server::fleet::AppState;
use fleet_server::listen;
use opamp::server::listen::Handle;
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
                // The baked version, not `CARGO_PKG_VERSION` (ADR-0013): the number in the file is
                // the release this build is *heading for*, and only `fleet_core::version::current` knows
                // whether this is it.
                println!("server {}", fleet_core::version::current());
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

    // One TLS provider for the whole process (ADR-0012): ring, never a system library.
    opamp::tls::install_ring_provider();

    let config_path = parse_args();
    let config = match ServerConfig::load(&config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // The TLS material first: a Server that cannot serve it does not start, and admission needs
    // to know which CA issued what (ADR-0038, ADR-0039).
    let planes = match config
        .tls
        .as_ref()
        .ok_or_else(|| "[tls] is required".to_string())
        .and_then(|tls| fleet_server::tls::server_tls(tls, config.enrolment.as_ref()))
        .and_then(|planes| {
            let agent = planes.agent.rustls_config()?;
            let operator = planes.operator.rustls_config()?;
            Ok((agent, operator, planes.issuers))
        }) {
        Ok(planes) => planes,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let clock: Arc<dyn fleet_server::fleet::Clock> = Arc::new(fleet_server::clock::SystemClock);
    let enrolment = config.enrolment.as_ref().map(|_| {
        // ADR-0039: closed until an operator opens it.
        info!("hosts with a bootstrap certificate may enrol while an operator opens the window");
        Arc::new(fleet_server::enrolment::Enrolment::new(clock.clone()))
    });
    let limits = config.admission_throttle.limits();

    let connection_offer = match config
        .connection_offer
        .as_ref()
        .map(fleet_server::fleet::ConnectionOffer::from_config)
        .transpose()
    {
        Ok(offer) => {
            if offer.is_some() {
                // ADR-0018.
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
        .map(fleet_server::ca::ClientCa::from_config)
        .transpose()
    {
        Ok(ca) => {
            if let Some(ca) = &ca {
                // ADR-0017.
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
        .map(fleet_server::fleet::TelemetryOffer::from_config)
        .unwrap_or_default();
    if config.telemetry_offer.is_some() {
        // ADR-0025.
        info!("offering the fleet somewhere to send its own telemetry");
    }
    let packages = fleet_server::packages::PackageStore::open(config.packages_dir.clone())
        .and_then(|store| {
            if !store.is_empty() {
                // ADR-0019.
                info!("offering software packages to the fleet");
            }
            fleet_server::fleet::PackageOffering::new(
                store,
                config.advertised_url.clone().unwrap_or_default(),
            )
        });
    let packages = match packages {
        Ok(offering) => Some(offering),
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
                .with_enrolment(enrolment.clone())
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
    // Both proofs, always (ADR-0039): the configuration was refused at load without either.
    let auth = config
        .auth
        .as_ref()
        .map(fleet_server::transport::OpampAuth::from_config);
    info!("the OpAMP endpoint requires the fleet credential and a client certificate");
    // Two planes, two listeners (ADR-0038): Agents reach the OpAMP endpoint and the package
    // downloads their offers point at; operators reach the REST API, its docs, and the UI.
    let (agent_tls, operator_tls, issuers) = planes;
    let agents = fleet_server::agent_app(
        state.clone(),
        fleet_server::transport::Admission::new(auth, true)
            .with_enrolment(issuers, enrolment)
            .with_throttle(Arc::new(fleet_server::throttle::Throttle::new(
                limits,
                clock.clone(),
            ))),
    );
    let operator_auth = config.rest.auth.as_ref().map(|auth| {
        fleet_server::api::OperatorAuth::from_config(auth).with_throttle(Arc::new(
            fleet_server::throttle::Throttle::new(limits, clock.clone()),
        ))
    });
    if operator_auth.is_some() {
        // ADR-0039. Both planes serve TLS, so the password never crosses a network in clear
        // (ADR-0038).
        info!("the REST API and the UI require authentication");
    }
    let operators = fleet_server::operator_app(state.clone(), operator_auth);

    let agent_listener = bind(config.listen, "the Agent plane");
    let operator_listener = bind(config.rest.listen, "the Operator plane");
    // One signal, both planes: the interrupt is watched once, and the handle both servers hold
    // drains them together within a bounded window (ADR-0012).
    let handle = Handle::new();
    tokio::spawn({
        let handle = handle.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutting down");
            listen::shut_down(&handle);
        }
    });

    info!(listen = %config.listen, "serving the OpAMP endpoint and package downloads over TLS");
    info!(listen = %config.rest.listen, "serving the REST API, the API docs, and the UI over TLS");
    let (agents, operators) = tokio::join!(
        listen::plane(
            agent_listener,
            Some(agent_tls),
            config.max_connections,
            handle.clone()
        )
        .serve(agents),
        listen::plane(
            operator_listener,
            Some(operator_tls),
            config.rest.max_connections,
            handle
        )
        .serve(operators),
    );
    agents.expect("serve the Agent plane");
    operators.expect("serve the Operator plane");
    // The graceful-shutdown flush (ADR-0026): every record's current timestamp and sequence
    // number, so the ordinary restart restores a fleet without gaps or false silence.
    state.flush_agents();
}
