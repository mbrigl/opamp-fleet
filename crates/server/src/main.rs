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
                println!("server {}", env!("CARGO_PKG_VERSION"));
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
                info!("offering connection settings to the fleet (ADR-0014)");
            }
            offer
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let state = match AppState::new(config.config_dir.clone()) {
        Ok(state) => Arc::new(state.with_connection_offer(connection_offer)),
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
        info!("the OpAMP endpoint requires authentication (ADR-0013)");
    }
    let app = server::app(state, auth);

    match &config.tls {
        Some(tls) => {
            let rustls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert_file, &tls.key_file)
                    .await
                    .expect("load the TLS certificate and key");
            info!(listen = %config.listen, "serving OpAMP, REST API, and UI over TLS");
            tokio::select! {
                served = axum_server::bind_rustls(config.listen, rustls_config)
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
}
