//! Runs the Supervisor Host (ADR-0009): one process that hosts many supervisors — OpAMP-native
//! Collector Supervisors and Custom Supervisors for non-OpAMP Foreign Agents — declared in a YAML
//! configuration. Each supervisor is its own OpAMP Agent.
//!
//! In the development environment it runs inside the `dev` container next to the server, alongside —
//! never replacing — the upstream OpenTelemetry Supervisor sidecar that remains the behavioural oracle
//! (ADR-0003).

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use opamp_supervisor::agent::ManagedAgent;
use opamp_supervisor::collector::Collector;
use opamp_supervisor::collector_agent::CollectorAgent;
use opamp_supervisor::config::{CollectorConfig, HostConfig, LogEncoding, SupervisorConfig};
use opamp_supervisor::host::SupervisorHost;
use opamp_supervisor::local_server;
use opamp_supervisor::process_agent::{ProcessAgent, ProcessConfig};
use opamp_supervisor::supervisor::{Config, Supervisor};
use opamp_supervisor::uid;

#[tokio::main]
async fn main() {
    let config_path = match parse_args() {
        Ok(path) => path,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    // The config chooses the log encoding, so it is parsed before logging is initialised; failures here
    // predate the subscriber and go to stderr directly.
    let yaml = match std::fs::read(&config_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!(
                "cannot read the host configuration {}: {e}",
                config_path.display()
            );
            std::process::exit(1);
        }
    };
    let host_config = match HostConfig::parse(&yaml) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("cannot parse the host configuration: {e}");
            std::process::exit(1);
        }
    };

    init_logging(host_config.telemetry.logs.encoding);

    if host_config.supervisors.is_empty() {
        error!("the host configuration declares no supervisors");
        std::process::exit(1);
    }

    // Install the rustls crypto provider (ring) once for the process, so a `wss://` connection uses it
    // (ADR-0012).
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Resolve the shared connection security once and apply it to every supervisor (ADR-0012): the
    // bearer token (a literal or, with a leading `@`, a file) and the TLS CA / insecure option.
    let auth_token = match host_config
        .auth_token
        .clone()
        .map(resolve_token)
        .transpose()
    {
        Ok(token) => token,
        Err(e) => {
            error!(error = %e, "cannot read the auth token");
            std::process::exit(1);
        }
    };
    let (tls_ca, tls_insecure) = match &host_config.tls {
        Some(tls) => {
            let ca = match &tls.ca_cert {
                Some(path) => match std::fs::read(path) {
                    Ok(bytes) => Some(bytes),
                    Err(e) => {
                        error!(path = %path.display(), error = %e, "cannot read the TLS CA certificate");
                        std::process::exit(1);
                    }
                },
                None => None,
            };
            (ca, tls.insecure)
        }
        None => (None, false),
    };
    let security = ConnSecurity {
        auth_token,
        tls_ca,
        tls_insecure,
    };

    let heartbeat = Duration::from_secs(host_config.heartbeat.max(1));
    let mut host = SupervisorHost::new();

    for entry in &host_config.supervisors {
        let name = entry.name().to_string();
        let server_url = entry.server(&host_config.server).to_string();
        let storage_dir = host_config.storage.join(&name);
        let uid_path = storage_dir.join("instance_uid");
        let instance_uid = match uid::load_or_create(&uid_path) {
            Ok(uid) => uid,
            Err(e) => {
                error!(supervisor = %name, error = %e, "cannot read or create the instance UID; skipping");
                continue;
            }
        };
        info!(supervisor = %name, uid = %uid::format(&instance_uid), "supervisor instance UID");

        let attributes: Vec<(String, String)> = entry
            .attributes()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        match entry {
            SupervisorConfig::Collector(c) => {
                let collector =
                    Collector::new(c.collector.clone(), storage_dir.join("collector.yaml"))
                        .with_crash_log_snippet(c.collector_crash_log_snippet_kib);
                let agent_version = collector.version().await;
                let (link, local_addr) = match local_server::start("127.0.0.1:0").await {
                    Ok(started) => started,
                    Err(e) => {
                        error!(supervisor = %name, error = %e, "cannot start the local OpAMP server; skipping");
                        continue;
                    }
                };
                let endpoint = format!("ws://{local_addr}/v1/opamp");
                // The local config layer (ADR-0014): base_config, if set, is the first entry, then the
                // config_files list — deep-merged under every remote config.
                let mut config_files = Vec::new();
                if let Some(base) = read_optional_config(c.base_config.as_deref()) {
                    config_files.push(base);
                }
                config_files.extend(read_config_list(&c.config_files));
                let agent =
                    CollectorAgent::new(collector, link, endpoint, &instance_uid, config_files);
                spawn(
                    &mut host,
                    &name,
                    server_url,
                    instance_uid,
                    uid_path,
                    storage_dir,
                    agent_version,
                    read_config_list(&c.fallback),
                    heartbeat,
                    attributes,
                    own_telemetry_capabilities(c),
                    c.automatic_config_rollback,
                    security.clone(),
                    agent,
                );
            }
            SupervisorConfig::Custom(c) => {
                let agent = ProcessAgent::new(ProcessConfig {
                    name: name.clone(),
                    command: c.command.clone(),
                    config_path: c.config_path.clone(),
                    reload: c.reload.clone(),
                });
                spawn(
                    &mut host,
                    &name,
                    server_url,
                    instance_uid,
                    uid_path,
                    storage_dir,
                    None,
                    read_config_list(&c.fallback),
                    heartbeat,
                    attributes,
                    0,     // a Foreign Agent does not report its own telemetry
                    false, // and cannot confirm health, so it never rolls back
                    security.clone(),
                    agent,
                );
            }
        }
    }

    if host.is_empty() {
        error!("no supervisor could be started");
        std::process::exit(1);
    }

    // The Host's own health-check endpoint, if configured (ADR-0013): reports whether the Host can
    // persist state and read its configuration. Runs alongside the supervisors.
    if let Some(hc) = &host_config.healthcheck {
        tokio::spawn(opamp_supervisor::health::serve(
            hc.endpoint.clone(),
            host_config.storage.clone(),
            config_path.clone(),
        ));
    }

    info!(supervisors = host.len(), "supervisor host running");
    host.run().await;
}

/// Initialises the tracing subscriber with the configured encoding: readable `console` or structured
/// `json` (mirroring the Go supervisor's `telemetry.logs.encoding`). The level filter is taken from the
/// environment (`RUST_LOG`), defaulting to `info`, in both encodings.
fn init_logging(encoding: LogEncoding) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match encoding {
        LogEncoding::Console => builder.init(),
        LogEncoding::Json => builder.json().init(),
    }
}

/// Builds one supervisor from its parts and spawns it on the host.
#[allow(clippy::too_many_arguments)]
fn spawn<A: ManagedAgent>(
    host: &mut SupervisorHost,
    name: &str,
    server_url: String,
    instance_uid: [u8; 16],
    uid_path: PathBuf,
    storage_dir: PathBuf,
    agent_version: Option<String>,
    fallback: Vec<Vec<u8>>,
    heartbeat: Duration,
    extra_attributes: Vec<(String, String)>,
    own_telemetry_capabilities: u64,
    automatic_config_rollback: bool,
    security: ConnSecurity,
    agent: A,
) {
    let supervisor = Supervisor::new(
        Config {
            server_url,
            instance_uid,
            uid_path,
            storage_dir,
            service_name: name.to_string(),
            agent_version,
            fallback,
            heartbeat,
            extra_attributes,
            own_telemetry_capabilities,
            automatic_config_rollback,
            auth_token: security.auth_token,
            tls_ca: security.tls_ca,
            tls_insecure: security.tls_insecure,
        },
        agent,
    );
    host.spawn(supervisor);
}

/// The shared connection security applied to every supervisor (ADR-0012): the bearer token and the TLS
/// CA / insecure option, resolved once from the host configuration.
#[derive(Clone)]
struct ConnSecurity {
    auth_token: Option<String>,
    tls_ca: Option<Vec<u8>>,
    tls_insecure: bool,
}

/// Resolves an auth token: a literal, or — with a leading `@` — the trimmed contents of a file (ADR-0012).
fn resolve_token(spec: String) -> Result<String, String> {
    match spec.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("cannot read auth token file {path}: {e}")),
        None => Ok(spec),
    }
}

/// The `ReportsOwn{Metrics,Logs,Traces}` capability bits a collector supervisor declares, per its
/// configuration toggles (ADR-0010).
fn own_telemetry_capabilities(c: &CollectorConfig) -> u64 {
    use opamp_proto::proto::AgentCapabilities;
    let mut caps = 0;
    if c.own_metrics {
        caps |= AgentCapabilities::ReportsOwnMetrics as u64;
    }
    if c.own_logs {
        caps |= AgentCapabilities::ReportsOwnLogs as u64;
    }
    if c.own_traces {
        caps |= AgentCapabilities::ReportsOwnTraces as u64;
    }
    caps
}

/// Reads an optional config file (fallback, base config, …), warning (not failing) if it cannot be read.
fn read_optional_config(path: Option<&Path>) -> Option<Vec<u8>> {
    let path = path?;
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "cannot read config file; ignoring");
            None
        }
    }
}

/// Reads a list of config files (the ordered startup fallback configs), skipping — with a warning — any
/// that cannot be read. The order is preserved so they merge deterministically.
fn read_config_list(paths: &[PathBuf]) -> Vec<Vec<u8>> {
    paths
        .iter()
        .filter_map(|path| read_optional_config(Some(path)))
        .collect()
}

/// Parses `-config <path>` (default `supervisors.yaml`). `-flag value` and `-flag=value` are accepted.
fn parse_args() -> Result<PathBuf, String> {
    let mut config = PathBuf::from("supervisors.yaml");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let flag = arg.trim_start_matches('-');
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (flag, None),
        };
        match name {
            "config" | "c" => {
                config = PathBuf::from(match inline {
                    Some(v) => v,
                    None => args
                        .next()
                        .ok_or_else(|| "flag -config needs a value".to_string())?,
                });
            }
            "help" | "h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag -{other}\n\n{USAGE}")),
        }
    }
    Ok(config)
}

const USAGE: &str = "\
usage: opamp-supervisor [flags]

  -config <path>   host configuration (YAML) declaring the supervisors to run
                   (default supervisors.yaml). Each entry is a collector or a custom agent.";
