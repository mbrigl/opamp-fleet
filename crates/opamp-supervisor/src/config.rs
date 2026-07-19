//! The Supervisor Host configuration: which supervisors the Host runs (ADR-0009).
//!
//! One YAML file declares a list of supervisors, each `type: collector` (an OpenTelemetry Collector) or
//! `type: custom` (a non-OpAMP Foreign Agent). Each entry becomes one Supervisor — one OpAMP Agent —
//! with its own Instance UID and storage subdirectory. Adding a kind of agent is a new entry type, not
//! a change to the domain.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

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
    /// The supervisors to run.
    pub supervisors: Vec<SupervisorConfig>,
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
    /// A config to run before the server answers (optional).
    pub fallback: Option<PathBuf>,
    /// A base collector config merged *underneath* every remote config (remote keys win), so an
    /// operator can pin local settings the Server's config is layered on top of (optional).
    pub base_config: Option<PathBuf>,
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
    /// A config to run before the server answers (optional).
    pub fallback: Option<PathBuf>,
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
