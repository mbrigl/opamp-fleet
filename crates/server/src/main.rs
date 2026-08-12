//! Entry point: load `server.toml`, bind one listener (plain or TLS), serve until interrupted.

use std::path::PathBuf;
use std::sync::Arc;

use server::config::ServerConfig;
use server::fleet::AppState;
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

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // One TLS provider for the whole process (ADR-0007): ring, never a system library.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install the rustls ring provider");

    let config_path = parse_args();
    let config = match ServerConfig::load(&config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

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
            Some(server::fleet::PackageOffering::new(
                store,
                config.advertised_url.clone().unwrap_or_default(),
            ))
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
                .with_stale_after(std::time::Duration::from_secs(config.stale_after_secs)),
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
    let app = server::app(
        state.clone(),
        server::transport::Admission::new(auth, mutual_tls),
    );

    match &config.tls {
        Some(tls) => {
            let rustls_config = match server::tls::server_config(tls) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            info!(listen = %config.listen, "serving OpAMP, REST API, and UI over TLS");
            tokio::select! {
                // The acceptor's own, rather than `bind_rustls`: it is what carries the
                // handshake's peer certificate into the request the OpAMP route checks.
                served = axum_server::bind(config.listen)
                    .acceptor(server::tls::PeerCertAcceptor::new(rustls_config))
                    .serve(app.into_make_service()) => {
                    served.expect("serve");
                }
                _ = tokio::signal::ctrl_c() => info!("shutting down"),
            }
        }
        None => {
            let listener = tokio::net::TcpListener::bind(config.listen)
                .await
                .expect("bind the listener");
            info!(listen = %config.listen, "serving OpAMP, REST API, and UI");
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                    info!("shutting down");
                })
                .await
                .expect("serve");
        }
    }
    // The graceful-shutdown flush (ADR-0051): every record's current timestamp and sequence
    // number, so the ordinary restart restores a fleet without gaps or false silence.
    state.flush_agents();
}
