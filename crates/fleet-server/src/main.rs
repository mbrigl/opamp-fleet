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
    eprintln!(
        "Usage: server [--config <server.toml>] [--version]\n       \
         server hash-credential --basic   (reads the password from standard input)\n       \
         server audit-verify <config_dir>/audit"
    );
    std::process::exit(2);
}

/// `server audit-verify <dir>` (ADR-0063 clause 3): walks the audit record's files in order and
/// names the first entry whose `prev` does not match the entry before it.
fn audit_verify(dir: Option<String>) -> ! {
    let Some(dir) = dir else { usage() };
    let files = match fleet_server::fs::audit_files(std::path::Path::new(&dir)) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let mut texts = Vec::new();
    for file in &files {
        match std::fs::read_to_string(file) {
            Ok(text) => texts.push((file.display().to_string(), text)),
            Err(e) => {
                eprintln!("cannot read {}: {e}", file.display());
                std::process::exit(1);
            }
        }
    }
    let lines = texts.iter().flat_map(|(name, text)| {
        text.lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(move |(index, line)| (format!("{name}:{}", index + 1), line))
    });
    match fleet_server::audit_log::verify(lines) {
        Ok(count) => {
            println!("{count} entries in {} files, the chain holds", files.len());
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("the chain breaks at {e}");
            std::process::exit(1);
        }
    }
}

/// What makes the entry `server.toml` keeps from a secret.
type Hasher = fn(&str) -> Result<String, String>;

/// The hash `server hash-credential` makes for a scheme: Basic, for an operator's password in
/// `[rest.auth]`, and nothing else — the Agent plane has no credential (ADR-0059 clause 26).
fn hasher(scheme: Option<&str>) -> Option<Hasher> {
    match scheme {
        Some("--basic") => Some(fleet_server::credentials::hash_basic),
        _ => None,
    }
}

/// `server hash-credential --basic` (ADR-0059 clause 26): reads the password from standard input —
/// without echo on a terminal — and prints the entry `server.toml` keeps instead of it.
fn hash_credential(scheme: Option<String>) -> ! {
    let Some(hash) = hasher(scheme.as_deref()) else {
        usage()
    };
    let secret = match read_secret() {
        Ok(secret) => secret,
        Err(e) => {
            eprintln!("cannot read the secret: {e}");
            std::process::exit(1);
        }
    };
    match hash(&secret) {
        Ok(entry) => {
            println!("{entry}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// One line from standard input, the trailing newline dropped. On a Unix terminal echo is off while
/// it is typed, so the secret does not stand on the screen.
fn read_secret() -> std::io::Result<String> {
    use std::io::{BufRead as _, IsTerminal as _};
    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    if terminal {
        eprint!("secret: ");
    }
    let _echo = terminal.then(EchoOff::new).flatten();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    if terminal {
        eprintln!();
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Terminal echo switched off for as long as it lives.
struct EchoOff {
    #[cfg(unix)]
    saved: libc::termios,
}

impl EchoOff {
    #[cfg(unix)]
    fn new() -> Option<Self> {
        // SAFETY: tcgetattr/tcsetattr on standard input with a termios this function owns.
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                return None;
            }
            let mut silent = saved;
            silent.c_lflag &= !libc::ECHO;
            (libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &silent) == 0)
                .then_some(EchoOff { saved })
        }
    }

    #[cfg(not(unix))]
    fn new() -> Option<Self> {
        None
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: restores the termios read in `new`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
        }
    }
}

fn parse_args() -> PathBuf {
    let mut config = PathBuf::from("server.toml");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "hash-credential" => hash_credential(args.next()),
            "audit-verify" => audit_verify(args.next()),
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
    // to know which CA issued what (ADR-0038, ADR-0059).
    let planes = match config
        .tls
        .as_ref()
        .ok_or_else(|| "[tls] is required".to_string())
        .and_then(|tls| fleet_server::tls::server_tls(tls, config.enrolment.as_ref()))
        .and_then(|planes| {
            let agent = planes.agent.rustls_config()?;
            let operator = planes.operator.rustls_config()?;
            Ok((agent, operator, planes.issuers, planes.authorities))
        }) {
        Ok(planes) => planes,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let clock: Arc<dyn fleet_server::fleet::Clock> = Arc::new(fleet_server::clock::SystemClock);
    let enrolment = config.enrolment.as_ref().map(|_| {
        // ADR-0059: closed until an operator opens it.
        info!("hosts with a bootstrap certificate may enrol while an operator opens the window");
        Arc::new(fleet_server::enrolment::Enrolment::new(clock.clone()))
    });
    let limits = config.admission_throttle.limits();

    let connection_offer = config
        .connection_offer
        .as_ref()
        .map(fleet_server::fleet::ConnectionOffer::from_config);
    if connection_offer.is_some() {
        // ADR-0060.
        info!("offering connection settings to the fleet");
    }
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
    // What the client CA signed and what is revoked (ADR-0065), kept beside the fleet's records.
    let revocations = match fleet_server::fs::FsLedgerStore::open(
        config.config_dir.join("revocation"),
    )
    .and_then(|store| {
        fleet_server::revocation::Revocations::open(
            Box::new(store),
            clock.clone(),
            planes.3.clone(),
        )
    }) {
        Ok(revocations) => Arc::new(revocations),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    // The audit record (ADR-0063), opened before anything is decided.
    let audit = match fleet_server::fs::FsAuditStore::open(config.config_dir.join("audit"))
        .and_then(|store| {
            fleet_server::audit_log::AuditLog::start(
                Box::new(store),
                config.audit.limits(),
                clock.clone(),
            )
        }) {
        Ok(audit) => Arc::new(audit) as Arc<dyn fleet_server::audit::Audit>,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Some(enrolment) = &enrolment {
        enrolment.set_audit(audit.clone());
    }
    let state = match AppState::new(config.config_dir.clone()) {
        Ok(state) => Arc::new(
            state
                .with_connection_offer(connection_offer)
                .with_client_ca(client_ca)
                .with_enrolment(enrolment.clone())
                .with_revocations(Some(revocations.clone()))
                .with_audit(Some(audit.clone()))
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
    // The certificate is the whole of admission (ADR-0059): the configuration was refused at load
    // without the client CA.
    info!("the OpAMP endpoint admits by client certificate");
    // Two planes, two listeners (ADR-0038): Agents reach the OpAMP endpoint and the package
    // downloads their offers point at; operators reach the REST API, its docs, and the UI.
    let (agent_tls, operator_tls, issuers, _) = planes;
    let agents = fleet_server::agent_app(
        state.clone(),
        fleet_server::transport::Admission::new(true)
            .with_enrolment(issuers, enrolment)
            .with_revocations(Some(revocations))
            .with_audit(Some(audit.clone()))
            .with_throttle(Arc::new(fleet_server::throttle::Throttle::new(
                limits,
                clock.clone(),
            ))),
    );
    let operator_auth = match config
        .rest
        .auth
        .as_ref()
        .map(fleet_server::api::OperatorAuth::from_config)
        .transpose()
    {
        Ok(auth) => auth.map(|auth| {
            auth.with_audit(Some(audit.clone())).with_throttle(Arc::new(
                fleet_server::throttle::Throttle::new(limits, clock.clone()),
            ))
        }),
        Err(e) => {
            eprintln!("{}: {e}", config_path.display());
            std::process::exit(1);
        }
    };
    if operator_auth.is_some() {
        // ADR-0059. Both planes serve TLS, so the password never crosses a network in clear
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `hash-credential` makes an operator's Basic entry and nothing else: the Agent plane has no
    /// credential to hash.
    /// Verifies: ADR-0059
    #[test]
    fn hash_credential_hashes_a_basic_password_alone() {
        let basic = hasher(Some("--basic")).expect("--basic is a scheme");
        let entry = basic("s3cret").expect("hash");
        fleet_server::credentials::check_basic(&entry).expect("an entry server.toml keeps");
        assert!(!entry.contains("s3cret"));
        assert!(hasher(Some("--bearer")).is_none(), "--bearer is gone");
        assert!(hasher(None).is_none());
    }
}
