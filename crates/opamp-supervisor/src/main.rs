//! Runs the Supervisor Host (ADR-0008): a process that hosts the OpAMP-native Collector Supervisor,
//! an OpAMP Agent that owns an OpenTelemetry Collector process.
//!
//! It connects to the OpAMP server, applies the collector configuration the server distributes by
//! writing it out and restarting the collector, and reports back what it applied. In the development
//! environment it runs inside the `dev` container next to the server, alongside — never replacing —
//! the upstream OpenTelemetry Supervisor sidecar that remains the behavioural oracle (ADR-0003).

use std::path::PathBuf;
use std::time::Duration;

use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use opamp_supervisor::collector::Collector;
use opamp_supervisor::host::SupervisorHost;
use opamp_supervisor::local_server;
use opamp_supervisor::supervisor::{Config, Supervisor};
use opamp_supervisor::uid;

/// The command-line configuration, mirroring the flag style of the server binary.
struct Options {
    server_url: String,
    collector: String,
    storage_dir: PathBuf,
    fallback_path: Option<PathBuf>,
    service_name: String,
    heartbeat: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            server_url: "ws://127.0.0.1:4320/v1/opamp".to_string(),
            collector: "otelcol-contrib".to_string(),
            storage_dir: PathBuf::from("/tmp/opamp-supervisor"),
            fallback_path: None,
            service_name: "io.opentelemetry.collector".to_string(),
            // The OpAMP default when the server does not dictate one.
            heartbeat: Duration::from_secs(30),
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

    // A stable 16-byte instance UID, persisted so a restart is recognised as the same agent.
    let uid_path = opts.storage_dir.join("instance_uid");
    let instance_uid = match uid::load_or_create(&uid_path) {
        Ok(uid) => uid,
        Err(e) => {
            error!(path = %uid_path.display(), error = %e, "cannot read or create the instance UID");
            std::process::exit(1);
        }
    };
    info!(uid = %uid::format(&instance_uid), "supervisor instance UID");

    // The fallback config lets the collector run before the server answers; optional.
    let fallback = match opts.fallback_path.as_ref().map(std::fs::read).transpose() {
        Ok(fallback) => fallback,
        Err(e) => {
            error!(error = %e, "cannot read the fallback configuration");
            std::process::exit(1);
        }
    };

    let config_path = opts.storage_dir.join("collector.yaml");
    let collector = Collector::new(opts.collector, config_path);

    // Ask the collector its version for the agent description; `None` if it cannot be run.
    let collector_version = collector.version().await;
    if let Some(version) = &collector_version {
        info!(version = %version, "managed collector version");
    }

    // The local OpAMP server the managed collector reports to (ADR-0008). Started before the collector
    // so it is up when the collector's opamp extension dials in.
    let (collector_link, local_addr) = match local_server::start("127.0.0.1:0").await {
        Ok(started) => started,
        Err(e) => {
            error!(error = %e, "cannot start the local OpAMP server for the collector");
            std::process::exit(1);
        }
    };
    info!(%local_addr, "local OpAMP server for the managed collector");
    let local_opamp_endpoint = format!("ws://{local_addr}/v1/opamp");

    let supervisor = Supervisor::new(
        Config {
            server_url: opts.server_url,
            instance_uid,
            uid_path,
            service_name: opts.service_name,
            collector_version,
            collector_link,
            local_opamp_endpoint,
            fallback,
            heartbeat: opts.heartbeat,
        },
        collector,
    );

    // Run the hosted supervisor until asked to stop; on Ctrl-C / SIGTERM, `kill_on_drop` tears the
    // collector down with us.
    SupervisorHost::new(supervisor).run().await;
}

/// Parses the documented flags: `-server`, `-collector`, `-storage`, `-fallback`, `-service-name`,
/// `-heartbeat`. Both `-flag value` and `-flag=value` are accepted, with `--flag` as an alias. Unknown
/// flags are an error so a typo does not silently run with a default.
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
            "server" => opts.server_url = value()?,
            "collector" => opts.collector = value()?,
            "storage" => opts.storage_dir = PathBuf::from(value()?),
            "fallback" => opts.fallback_path = Some(PathBuf::from(value()?)),
            "service-name" => opts.service_name = value()?,
            "heartbeat" => {
                let secs: u64 = value()?
                    .parse()
                    .map_err(|_| "flag -heartbeat needs a whole number of seconds".to_string())?;
                if secs == 0 {
                    return Err("flag -heartbeat must be at least 1 second".to_string());
                }
                opts.heartbeat = Duration::from_secs(secs);
            }
            "help" | "h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag -{other}\n\n{USAGE}")),
        }
    }
    Ok(opts)
}

const USAGE: &str = "\
usage: opamp-supervisor [flags]

  -server <url>          OpAMP server WebSocket URL (default ws://127.0.0.1:4320/v1/opamp)
  -collector <path>      OpenTelemetry Collector executable (default otelcol-contrib on PATH)
  -storage <dir>         directory for the instance UID and generated collector config
                         (default /tmp/opamp-supervisor)
  -fallback <path>       collector config to run before the server answers (optional)
  -service-name <name>   service.name the agent reports (default io.opentelemetry.collector)
  -heartbeat <seconds>   how often to send a keepalive heartbeat (default 30)";
