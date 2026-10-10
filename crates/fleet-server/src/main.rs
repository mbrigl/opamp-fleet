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
use tracing::{info, warn};

fn usage() -> ! {
    eprintln!(
        "Usage: server [--config <server.toml>] [--version]\n       \
         server hash-credential --basic   (reads the password from standard input)\n       \
         server audit-verify <config_dir>/audit\n       \
         server pki init --server-dir <dir> --offline-dir <dir> --name <dns|ip>... \
         [--server-path <path>] [--host-path <path>] [--fleet <name>] [--ca-days <n>] \
         [--server-days <n>] [--bootstrap-days <n>]\n       \
         server pki server-cert --offline-dir <dir> --name <dns|ip>... --out <dir> [--days <n>]\n       \
         server pki bootstrap-cert --offline-dir <dir> --out <dir> [--days <n>]\n       \
         server pki status [--config <server.toml>] [--offline-dir <dir>]"
    );
    std::process::exit(2);
}

/// `server audit-verify <dir>` (ADR-0024 clause 3): walks the audit record's files in order and
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

/// The options of a `server pki` command, read as `--flag value` pairs; `--name` may repeat.
struct PkiArgs {
    values: std::collections::HashMap<String, String>,
    names: Vec<String>,
}

impl PkiArgs {
    fn parse(args: impl Iterator<Item = String>, allowed: &[&str]) -> Self {
        let mut values = std::collections::HashMap::new();
        let mut names = Vec::new();
        let mut args = args.peekable();
        while let Some(flag) = args.next() {
            let Some(value) = args.next() else { usage() };
            match flag.as_str() {
                "--name" if allowed.contains(&"--name") => names.push(value),
                flag if allowed.contains(&flag) => {
                    if values.insert(flag.to_string(), value).is_some() {
                        usage()
                    }
                }
                _ => usage(),
            }
        }
        PkiArgs { values, names }
    }

    fn path(&self, flag: &str) -> Option<PathBuf> {
        self.values.get(flag).map(PathBuf::from)
    }

    fn required(&self, flag: &str) -> PathBuf {
        self.path(flag).unwrap_or_else(|| {
            eprintln!("{flag} is required");
            std::process::exit(2);
        })
    }

    fn days(&self, flag: &str, default: u32) -> u32 {
        match self.values.get(flag) {
            None => default,
            Some(days) => days.parse().unwrap_or_else(|_| {
                eprintln!("{flag} {days:?} is not a number of days");
                std::process::exit(2);
            }),
        }
    }
}

/// `server pki …` (ADR-0029): makes and inspects the fleet's certificate authorities, the
/// Server's or a Gateway's certificate and the bootstrap certificate. Never a host certificate.
fn pki(mut args: impl Iterator<Item = String>) -> ! {
    use fleet_server::pki;
    let done = |result: Result<Vec<String>, String>| -> ! {
        match result {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    };
    match args.next().as_deref() {
        Some("init") => {
            let a = PkiArgs::parse(
                args,
                &[
                    "--server-dir",
                    "--offline-dir",
                    "--server-path",
                    "--host-path",
                    "--fleet",
                    "--name",
                    "--ca-days",
                    "--server-days",
                    "--bootstrap-days",
                ],
            );
            done(pki::init(&pki::InitOptions {
                server_dir: a.required("--server-dir"),
                offline_dir: a.required("--offline-dir"),
                server_path: a.path("--server-path"),
                host_path: a.path("--host-path"),
                fleet: a
                    .values
                    .get("--fleet")
                    .cloned()
                    .unwrap_or_else(|| pki::DEFAULT_FLEET.to_string()),
                names: a.names.clone(),
                ca_days: a.days("--ca-days", pki::CA_DAYS),
                server_days: a.days("--server-days", pki::SERVER_DAYS),
                bootstrap_days: a.days("--bootstrap-days", pki::BOOTSTRAP_DAYS),
            }))
        }
        Some("server-cert") => {
            let a = PkiArgs::parse(args, &["--offline-dir", "--name", "--out", "--days"]);
            done(pki::server_cert(
                &a.required("--offline-dir"),
                &a.names,
                a.days("--days", pki::SERVER_DAYS),
                &a.required("--out"),
            ))
        }
        Some("bootstrap-cert") => {
            let a = PkiArgs::parse(args, &["--offline-dir", "--out", "--days"]);
            done(pki::bootstrap_cert(
                &a.required("--offline-dir"),
                a.days("--days", pki::BOOTSTRAP_DAYS),
                &a.required("--out"),
            ))
        }
        Some("status") => {
            let a = PkiArgs::parse(args, &["--config", "--offline-dir"]);
            let config = a.path("--config").map(|path| {
                ServerConfig::load(&path).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(1);
                })
            });
            if config.is_none() && a.path("--offline-dir").is_none() {
                usage()
            }
            let endings = pki::status(config.as_ref(), a.path("--offline-dir").as_deref())
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(1);
                });
            let now = time::OffsetDateTime::now_utc();
            for ending in &endings {
                let standing = match ending.standing(now) {
                    pki::Standing::Fine => "ok",
                    pki::Standing::Ending => "ENDS SOON",
                    pki::Standing::Ended => "ENDED",
                };
                println!(
                    "{standing:9} {} days  {}  serial {}  {}  ({})",
                    ending.days_left(now),
                    ending.not_after.date(),
                    ending.serial,
                    ending.subject,
                    ending.file.display()
                );
            }
            std::process::exit(pki::exit_code(&endings, now));
        }
        _ => usage(),
    }
}

/// What makes the entry `server.toml` keeps from a secret.
type Hasher = fn(&str) -> Result<String, String>;

/// The hash `server hash-credential` makes for a scheme: Basic, for an operator's password in
/// `[rest.auth]`, and nothing else — the Agent plane has no credential (ADR-0022 clause 26).
fn hasher(scheme: Option<&str>) -> Option<Hasher> {
    match scheme {
        Some("--basic") => Some(fleet_server::credentials::hash_basic),
        _ => None,
    }
}

/// `server hash-credential --basic` (ADR-0022 clause 26): reads the password from standard input —
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
            "pki" => pki(args),
            "--config" => match args.next() {
                Some(path) => config = PathBuf::from(path),
                None => usage(),
            },
            "--version" => {
                // The baked version, not `CARGO_PKG_VERSION` (ADR-0011): the number in the file is
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
    // The log goes to stderr, so that stdout carries only what a command prints as its result.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
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
    // to know which CA issued what (ADR-0012, ADR-0022).
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
        // ADR-0022: closed until an operator opens it.
        info!("hosts with a bootstrap certificate may enrol while an operator opens the window");
        Arc::new(fleet_server::enrolment::Enrolment::new(clock.clone()))
    });
    let limits = config.admission_throttle.limits();

    let connection_offer = config
        .connection_offer
        .as_ref()
        .map(fleet_server::fleet::ConnectionOffer::from_config);
    if connection_offer.is_some() {
        // ADR-0013.
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
                // ADR-0022.
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
    if let Some(notice) = uploaded_artifacts_notice(client_ca.is_some()) {
        // ADR-0028 clause 37.
        info!("{notice}");
    }
    let telemetry_offer = config
        .telemetry_offer
        .as_ref()
        .map(fleet_server::fleet::TelemetryOffer::from_config)
        .unwrap_or_default();
    if config.telemetry_offer.is_some() {
        // ADR-0016.
        info!("offering the fleet somewhere to send its own telemetry");
    }
    let packages = fleet_server::packages::PackageStore::open(config.packages_dir.clone())
        .and_then(|store| {
            if !store.is_empty() {
                // ADR-0028.
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
    // What the client CA signed and what is revoked (ADR-0023), kept beside the fleet's records.
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
    // The audit record (ADR-0024), opened before anything is decided.
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
    // Before a certificate the Server depends on ends, say so: at startup and once a day
    // (ADR-0029 clause 11). Nothing stops; the hosts decide for themselves.
    let watched = fleet_server::pki::depended_on(&config);
    fleet_server::pki::warn_endings(
        &watched,
        Some(audit.as_ref()),
        time::OffsetDateTime::now_utc(),
    );
    tokio::spawn({
        let audit = audit.clone();
        async move {
            let mut daily = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
            daily.tick().await;
            loop {
                daily.tick().await;
                fleet_server::pki::warn_endings(
                    &watched,
                    Some(audit.as_ref()),
                    time::OffsetDateTime::now_utc(),
                );
            }
        }
    });
    if let Some(warning) = config.rate_limit_warning() {
        // ADR-0012 clause 20.
        warn!("{warning}");
    }
    let agent_rate = Arc::new(fleet_server::agent_rate::AgentRate::new(
        config.agent_rate_limit.limits(),
        config.max_agents,
        clock.clone(),
    ));
    let state = match AppState::new(config.config_dir.clone()) {
        Ok(state) => Arc::new(
            state
                .with_agent_rate(Some(agent_rate))
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
    // The certificate is the whole of admission (ADR-0022): the configuration was refused at load
    // without the client CA.
    info!("the OpAMP endpoint admits by client certificate");
    // Two planes, two listeners (ADR-0012): Agents reach the OpAMP endpoint and the package
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
        // ADR-0022. Both planes serve TLS, so the password never crosses a network in clear
        // (ADR-0012).
        info!("the REST API and the UI require authentication");
    }
    let operators = fleet_server::operator_app(state.clone(), operator_auth);

    let agent_listener = bind(config.listen, "the Agent plane");
    let operator_listener = bind(config.rest.listen, "the Operator plane");
    // One signal, both planes: the interrupt is watched once, and the handle both servers hold
    // drains them together within a bounded window (ADR-0012).
    let handle = Handle::new();
    let signalled = shutdown_signal();
    tokio::spawn({
        let handle = handle.clone();
        async move {
            signalled.await;
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

/// The shutdown signal: `SIGTERM`, which a service manager sends on stop, or `SIGINT` from a
/// terminal. Either one drains both planes and saves the fleet before the process exits. Both
/// handlers are installed when this is called, not when the future is first polled, so a signal
/// that arrives while the Server is still starting is not left to its default of ending the process.
fn shutdown_signal() -> impl std::future::Future<Output = ()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate =
            signal(SignalKind::terminate()).expect("installing the SIGTERM handler");
        let mut interrupt = signal(SignalKind::interrupt()).expect("installing the SIGINT handler");
        async move {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        }
    }
    #[cfg(not(unix))]
    {
        async {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// What a Server that signs no CSRs says once at startup (ADR-0028 clause 37): its hosts hold the
/// certificates an operator provisioned, and only one that names its host is served an uploaded
/// artifact.
fn uploaded_artifacts_notice(signs_csrs: bool) -> Option<String> {
    (!signs_csrs).then(|| {
        format!(
            "without [client_ca], uploaded artifacts reach only hosts whose certificate names a \
             host ({}<id>)",
            fleet_server::ca::HOST_URI_PREFIX
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `hash-credential` makes an operator's Basic entry and nothing else: the Agent plane has no
    /// credential to hash.
    /// Verifies: ADR-0022
    #[test]
    fn hash_credential_hashes_a_basic_password_alone() {
        let basic = hasher(Some("--basic")).expect("--basic is a scheme");
        let entry = basic("s3cret").expect("hash");
        fleet_server::credentials::check_basic(&entry).expect("an entry server.toml keeps");
        assert!(!entry.contains("s3cret"));
        assert!(hasher(Some("--bearer")).is_none(), "--bearer is gone");
        assert!(hasher(None).is_none());
    }

    /// Without `[client_ca]` the Server says at startup that an uploaded artifact needs a
    /// certificate naming a host, and how one is named; with it, it says nothing.
    /// Verifies: ADR-0037
    #[test]
    fn a_server_without_client_ca_says_uploaded_artifacts_need_a_host() {
        let notice = uploaded_artifacts_notice(false).expect("a notice");
        assert!(notice.contains("uploaded artifacts"), "{notice}");
        assert!(notice.contains("urn:opamp-fleet:host:<id>"), "{notice}");
        assert!(uploaded_artifacts_notice(true).is_none());
    }
}
