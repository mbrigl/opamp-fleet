//! The Supervisor Host configuration: which supervisors the Host runs (ADR-0009).
//!
//! One YAML file declares a list of supervisors, each `type: collector` (an OpenTelemetry Collector) or
//! `type: custom` (a non-OpAMP Foreign Agent). Each entry becomes one Supervisor — one OpAMP Agent —
//! with its own Instance UID and storage subdirectory. Adding a kind of agent is a new entry type, not
//! a change to the domain.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer};

/// The whole host configuration.
#[derive(Debug, Deserialize)]
pub struct HostConfig {
    /// The default OpAMP server URL for supervisors that do not set their own.
    #[serde(default = "default_server")]
    pub server: String,
    /// The base storage directory; each supervisor gets `storage/<name>`.
    #[serde(default = "default_storage")]
    pub storage: PathBuf,
    /// The default heartbeat interval, in seconds.
    #[serde(default = "default_heartbeat")]
    pub heartbeat: u64,
    /// The Host's own observability settings (its logging), separate from the managed agents' telemetry.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// The shared bearer token every supervisor presents to the Server (ADR-0012); a literal or, with a
    /// leading `@`, a file to read it from. `None` connects unauthenticated.
    #[serde(default)]
    pub auth_token: Option<String>,
    /// TLS settings for the OpAMP connection (ADR-0012); `None` uses the platform's default roots for a
    /// `wss://` server and is irrelevant for plain `ws://`.
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    /// An optional health-check endpoint for the Host itself (ADR-0013); `None` disables it.
    #[serde(default)]
    pub healthcheck: Option<HealthcheckConfig>,
    /// The supervisors to run.
    pub supervisors: Vec<SupervisorConfig>,
}

/// TLS settings for validating the Server's certificate on a `wss://` connection (ADR-0012).
#[derive(Debug, Default, Deserialize)]
pub struct TlsConfig {
    /// A PEM CA certificate to validate the Server against, instead of the platform's default roots.
    pub ca_cert: Option<PathBuf>,
    /// Skip certificate validation entirely — **dangerous**, development only.
    #[serde(default)]
    pub insecure: bool,
}

/// The Host's health-check endpoint (ADR-0013): an address like `127.0.0.1:13133` to serve `GET` on.
#[derive(Debug, Deserialize)]
pub struct HealthcheckConfig {
    pub endpoint: String,
}

/// The Supervisor Host's own telemetry (currently just how it encodes its logs), mirroring the Go
/// supervisor's `telemetry` section.
#[derive(Debug, Default, Deserialize)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub logs: LogsConfig,
}

/// The Host's log settings.
#[derive(Debug, Default, Deserialize)]
pub struct LogsConfig {
    /// How the Host encodes its own log lines.
    #[serde(default)]
    pub encoding: LogEncoding,
}

/// The encoding of the Host's own logs: human-readable `console` (the default, for the dev environment)
/// or structured `json`. The Go supervisor defaults to `json`; we default to `console` because these
/// logs are read by an operator, not the Server, and readable output is the better local default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogEncoding {
    #[default]
    Console,
    Json,
}

/// One supervisor entry, tagged by `type`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SupervisorConfig {
    /// An OpAMP-native OpenTelemetry Collector.
    Collector(CollectorConfig),
    /// A non-OpAMP Foreign Agent, managed by a Custom Supervisor.
    Custom(CustomConfig),
}

impl SupervisorConfig {
    /// The unique name of this supervisor (its storage subdirectory and fleet label).
    pub fn name(&self) -> &str {
        match self {
            SupervisorConfig::Collector(c) => &c.name,
            SupervisorConfig::Custom(c) => &c.name,
        }
    }

    /// The OpAMP server URL for this supervisor, its own override or the host default.
    pub fn server<'a>(&'a self, default: &'a str) -> &'a str {
        let own = match self {
            SupervisorConfig::Collector(c) => &c.server,
            SupervisorConfig::Custom(c) => &c.server,
        };
        own.as_deref().unwrap_or(default)
    }

    /// Extra non-identifying attributes to attach to this supervisor's reported `AgentDescription`.
    pub fn attributes(&self) -> &BTreeMap<String, String> {
        match self {
            SupervisorConfig::Collector(c) => &c.attributes,
            SupervisorConfig::Custom(c) => &c.attributes,
        }
    }
}

/// A Collector supervisor entry.
#[derive(Debug, Deserialize)]
pub struct CollectorConfig {
    pub name: String,
    /// The collector executable (default `otelcol-contrib` on `PATH`).
    #[serde(default = "default_collector")]
    pub collector: String,
    /// Configs to run before the server answers — a single path or a list, merged in order (later files
    /// win), matching the Go supervisor's `startup_fallback_configs`. Empty when none is configured.
    #[serde(default, deserialize_with = "one_or_many_paths")]
    pub fallback: Vec<PathBuf>,
    /// A base collector config merged *underneath* every remote config (remote keys win), so an
    /// operator can pin local settings the Server's config is layered on top of (optional). Sugar for a
    /// first entry of `config_files` (ADR-0014).
    pub base_config: Option<PathBuf>,
    /// Local regular config files, a single path or a list, deep-merged in order as the base layer under
    /// every remote config (remote wins), and always part of the composition — so a reconnect never
    /// strands the collector without them (`config_files`, ADR-0014). `base_config`, if set, is the first
    /// entry.
    #[serde(default, deserialize_with = "one_or_many_paths")]
    pub config_files: Vec<PathBuf>,
    /// How much of the collector's most recent stderr to include when it crashes, in KiB (0 disables it,
    /// the default; clamped to 1024). Mirrors the Go supervisor's `collector_crash_log_snippet_kib`.
    #[serde(default)]
    pub collector_crash_log_snippet_kib: usize,
    /// Revert to the last healthy configuration when a newly applied remote config does not make the
    /// collector healthy, reporting the new config `FAILED` (ADR-0008). Off by default, matching the Go
    /// supervisor's `automatic_config_rollback`.
    #[serde(default)]
    pub automatic_config_rollback: bool,
    /// An OpAMP server URL overriding the host default (optional).
    pub server: Option<String>,
    /// Extra non-identifying attributes for this agent's description (e.g. team, environment).
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Report the collector's own metrics to the destination the Server offers (ADR-0010). Default on.
    #[serde(default = "default_true")]
    pub own_metrics: bool,
    /// Report the collector's own logs to the destination the Server offers (ADR-0010). Default on.
    #[serde(default = "default_true")]
    pub own_logs: bool,
    /// Report the collector's own traces to the destination the Server offers (ADR-0010). Default on.
    #[serde(default = "default_true")]
    pub own_traces: bool,
}

/// A Custom (Foreign Agent) supervisor entry.
#[derive(Debug, Deserialize)]
pub struct CustomConfig {
    pub name: String,
    /// The command to run the foreign agent: executable followed by its arguments.
    pub command: Vec<String>,
    /// Where the Server-distributed config is written for the agent to read.
    pub config_path: PathBuf,
    /// A command to reload the agent in place after a config write; if absent, it is restarted.
    pub reload: Option<Vec<String>>,
    /// Configs to run before the server answers — a single path or a list, merged in order (later files
    /// win), matching the Go supervisor's `startup_fallback_configs`. Empty when none is configured.
    #[serde(default, deserialize_with = "one_or_many_paths")]
    pub fallback: Vec<PathBuf>,
    /// An OpAMP server URL overriding the host default (optional).
    pub server: Option<String>,
    /// Extra non-identifying attributes for this agent's description (e.g. team, environment).
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

fn default_server() -> String {
    "ws://127.0.0.1:4320/v1/opamp".to_string()
}
fn default_storage() -> PathBuf {
    PathBuf::from("/tmp/opamp-supervisor")
}
fn default_heartbeat() -> u64 {
    30
}
fn default_collector() -> String {
    "otelcol-contrib".to_string()
}
fn default_true() -> bool {
    true
}

/// Deserializes a `fallback` field that is either a single path (`fallback: a.yaml`) or a list
/// (`fallback: [a.yaml, b.yaml]`) into a `Vec`, so a single fallback stays ergonomic while a list is
/// supported (the Go supervisor's `startup_fallback_configs`).
fn one_or_many_paths<'de, D>(deserializer: D) -> Result<Vec<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(PathBuf),
        Many(Vec<PathBuf>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(path) => vec![path],
        OneOrMany::Many(paths) => paths,
    })
}

impl HostConfig {
    /// Parses a host configuration from YAML, rejecting duplicate supervisor names (each needs its own
    /// storage subdirectory and instance UID).
    pub fn parse(yaml: &[u8]) -> Result<Self, String> {
        let config: HostConfig =
            serde_yaml::from_slice(yaml).map_err(|e| format!("invalid host config: {e}"))?;
        let mut seen = std::collections::HashSet::new();
        for s in &config.supervisors {
            if !seen.insert(s.name().to_string()) {
                return Err(format!("duplicate supervisor name: {}", s.name()));
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_mixed_host_config() {
        let yaml = br#"
server: ws://127.0.0.1:4320/v1/opamp
storage: /var/lib/opamp
supervisors:
  - type: collector
    name: otelcol
    fallback: config/collector.yaml
    attributes:
      team: telemetry
      deployment.environment: staging
  - type: custom
    name: nginx
    command: ["/usr/sbin/nginx", "-g", "daemon off;"]
    config_path: /etc/nginx/nginx.conf
    reload: ["/usr/sbin/nginx", "-s", "reload"]
"#;
        let config = HostConfig::parse(yaml).unwrap();
        assert_eq!(config.supervisors.len(), 2);
        assert_eq!(config.heartbeat, 30, "heartbeat defaults to 30");
        match &config.supervisors[0] {
            SupervisorConfig::Collector(c) => {
                assert_eq!(c.name, "otelcol");
                assert_eq!(c.collector, "otelcol-contrib", "collector defaults");
            }
            _ => panic!("first should be a collector"),
        }
        // Configured attributes parse; a supervisor without an `attributes:` block gets an empty map.
        assert_eq!(
            config.supervisors[0]
                .attributes()
                .get("team")
                .map(String::as_str),
            Some("telemetry")
        );
        assert!(config.supervisors[1].attributes().is_empty());
        match &config.supervisors[1] {
            SupervisorConfig::Custom(c) => {
                assert_eq!(c.name, "nginx");
                assert_eq!(c.command[0], "/usr/sbin/nginx");
                assert!(c.reload.is_some());
            }
            _ => panic!("second should be custom"),
        }
        // Per-supervisor server falls back to the host default.
        assert_eq!(
            config.supervisors[0].server(&config.server),
            "ws://127.0.0.1:4320/v1/opamp"
        );
    }

    #[test]
    fn telemetry_encoding_defaults_to_console_and_parses_json() {
        // Absent telemetry section → console (the readable default).
        let plain = HostConfig::parse(
            br#"
supervisors:
  - type: custom
    name: a
    command: ["true"]
    config_path: /tmp/a
"#,
        )
        .unwrap();
        assert_eq!(plain.telemetry.logs.encoding, LogEncoding::Console);

        // An explicit `json` encoding is honoured.
        let json = HostConfig::parse(
            br#"
telemetry:
  logs:
    encoding: json
supervisors:
  - type: custom
    name: a
    command: ["true"]
    config_path: /tmp/a
"#,
        )
        .unwrap();
        assert_eq!(json.telemetry.logs.encoding, LogEncoding::Json);
    }

    #[test]
    fn collector_crash_log_snippet_defaults_off_and_parses() {
        let config = HostConfig::parse(
            br#"
supervisors:
  - type: collector
    name: default-off
  - type: collector
    name: with-snippet
    collector_crash_log_snippet_kib: 64
"#,
        )
        .unwrap();
        match &config.supervisors[0] {
            SupervisorConfig::Collector(c) => assert_eq!(c.collector_crash_log_snippet_kib, 0),
            _ => panic!("first should be a collector"),
        }
        match &config.supervisors[1] {
            SupervisorConfig::Collector(c) => assert_eq!(c.collector_crash_log_snippet_kib, 64),
            _ => panic!("second should be a collector"),
        }
    }

    #[test]
    fn automatic_config_rollback_defaults_off_and_parses() {
        let config = HostConfig::parse(
            br#"
supervisors:
  - type: collector
    name: default-off
  - type: collector
    name: rollback-on
    automatic_config_rollback: true
"#,
        )
        .unwrap();
        match &config.supervisors[0] {
            SupervisorConfig::Collector(c) => assert!(!c.automatic_config_rollback),
            _ => panic!("first should be a collector"),
        }
        match &config.supervisors[1] {
            SupervisorConfig::Collector(c) => assert!(c.automatic_config_rollback),
            _ => panic!("second should be a collector"),
        }
    }

    #[test]
    fn fallback_accepts_a_single_path_a_list_or_nothing() {
        let single = HostConfig::parse(
            br#"
supervisors:
  - type: collector
    name: a
    fallback: config/collector.yaml
"#,
        )
        .unwrap();
        let many = HostConfig::parse(
            br#"
supervisors:
  - type: collector
    name: a
    fallback:
      - config/base.yaml
      - config/extra.yaml
"#,
        )
        .unwrap();
        let none = HostConfig::parse(
            br#"
supervisors:
  - type: collector
    name: a
"#,
        )
        .unwrap();
        let fallback = |c: &HostConfig| match &c.supervisors[0] {
            SupervisorConfig::Collector(c) => c.fallback.clone(),
            _ => panic!("expected a collector"),
        };
        assert_eq!(
            fallback(&single),
            vec![PathBuf::from("config/collector.yaml")]
        );
        assert_eq!(
            fallback(&many),
            vec![
                PathBuf::from("config/base.yaml"),
                PathBuf::from("config/extra.yaml"),
            ]
        );
        assert!(fallback(&none).is_empty());
    }

    #[test]
    fn config_files_accept_a_single_path_or_a_list() {
        let config = HostConfig::parse(
            br#"
supervisors:
  - type: collector
    name: single
    config_files: config/local.yaml
  - type: collector
    name: many
    config_files:
      - config/a.yaml
      - config/b.yaml
  - type: collector
    name: none
"#,
        )
        .unwrap();
        let files = |i: usize| match &config.supervisors[i] {
            SupervisorConfig::Collector(c) => c.config_files.clone(),
            _ => panic!("expected a collector"),
        };
        assert_eq!(files(0), vec![PathBuf::from("config/local.yaml")]);
        assert_eq!(
            files(1),
            vec![
                PathBuf::from("config/a.yaml"),
                PathBuf::from("config/b.yaml")
            ]
        );
        assert!(files(2).is_empty());
    }

    #[test]
    fn rejects_duplicate_names() {
        let yaml = br#"
supervisors:
  - type: custom
    name: dup
    command: ["true"]
    config_path: /tmp/a
  - type: custom
    name: dup
    command: ["true"]
    config_path: /tmp/b
"#;
        assert!(HostConfig::parse(yaml).unwrap_err().contains("duplicate"));
    }
}
