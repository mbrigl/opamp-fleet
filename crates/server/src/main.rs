//! Entry point: load `server.toml`, bind the two listeners (plain or TLS) — the Agent plane and
//! the Operator plane (ADR-0012) — and serve both until interrupted.
//!
//! Both planes are served the same way whether or not TLS is configured, so that what bounds a
//! connection before it becomes a request holds on all four surfaces (ADR-0012). Only the acceptor
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
                // The baked version, not `CARGO_PKG_VERSION` (ADR-0013): the number in the file is
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

    // One TLS provider for the whole process (ADR-0012): ring, never a system library.

    let config_path = parse_args();
    let config = match ServerConfig::load(&config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

                // ADR-0018.
                info!("offering connection settings to the fleet");
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
                // ADR-0019.
                info!("offering software packages to the fleet");
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
                .with_packages(packages)
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
        // ADR-0017.
        info!("the OpAMP endpoint requires authentication");
    // Two planes, two listeners (ADR-0012): Agents reach the OpAMP endpoint and the package
    // downloads their offers point at; operators reach the REST API, its docs, and the UI.
    let agents = server::agent_app(

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

    match &config.tls {
        Some(tls) => {
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
}
