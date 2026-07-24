//! The Client's own configuration file — TOML (ADR-0008).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `client.toml`. Every setting has a default; unknown keys are rejected so a typo fails loudly at
/// startup instead of silently applying a default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    /// The Server's OpAMP endpoint. The URL scheme selects the transport (ADR-0007):
    /// `ws://` / `wss://` is the WebSocket transport, `http://` / `https://` the polling one.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// The Agent's `service.name`, its human identity in the fleet.
    #[serde(default = "default_name")]
    pub name: String,
    /// How often the plain-HTTP transport polls. The Baseline's default is 30 seconds; ignored on
    /// WebSocket, where the Server pushes.
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// How often each Agent heartbeats over the WebSocket transport (`ReportsHeartbeat`). The
    /// Baseline's default is 30 seconds; `0` disables heartbeats and undeclares the capability.
    /// Ignored on plain HTTP, where every poll is the periodic report.
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,
    /// Where the Client persists its identity and the received remote configuration.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    /// Operator-defined attributes (ADR-0012), reported as non-identifying attributes of **every**
    /// Agent this Client presents — machine-level tags like `env = "prod"` that Selectors can
    /// match. A `[[supervisor]]` block's own `attributes` override these per key; attributes the
    /// code or the Managed Process reports win over configured ones.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Optional TLS trust override for `wss://` / `https://` endpoints.
    pub tls: Option<TlsConfig>,
    /// The `[[supervisor]]` blocks (ADR-0011): each runs one Supervisor managing one local
    /// process, appearing to the Server as its own Agent. Absent means the Client presents
    /// itself as a single Agent, as before.
    #[serde(default, rename = "supervisor")]
    pub supervisors: Vec<SupervisorBlock>,
}

/// One `[[supervisor]]` block (ADR-0011). The common keys are extracted here; everything else
/// stays in [`settings`](Self::settings) for the plugin the `type` selects, which parses it
/// strictly — serde cannot combine `flatten` with `deny_unknown_fields` (serde-rs/serde#1547),
/// so this two-stage split is what keeps a typo anywhere in the block failing loudly at startup.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "toml::Table")]
pub struct SupervisorBlock {
    /// The plugin this block selects (the TOML key `type`), e.g. `"collector"` or `"command"`.
    pub kind: String,
    /// The Supervisor's name: the Agent's `service.name` and its state directory name, so it
    /// follows the instance-name grammar of ADR-0010. Must be unique across blocks.
    pub name: String,
    /// The Supervisor Endpoint's loopback port; `0` (the default) binds an ephemeral port. Pin
    /// it when the distributed configuration carries the `opampextension` pointing at it.
    pub endpoint_port: u16,
    /// How long a graceful stop may take before the Managed Process is killed.
    pub stop_timeout_secs: u64,
    /// How long a freshly (re)started Managed Process must survive before a received
    /// configuration is acknowledged `APPLIED`; exiting within the grace reports `FAILED`
    /// (the health-gated acknowledgement ADR-0011 names). `0` acknowledges on start, as before.
    pub apply_grace_secs: u64,
    /// This Supervisor's operator-defined attributes (ADR-0012), merged over the top-level ones.
    pub attributes: BTreeMap<String, String>,
    /// The plugin-specific keys, handed over verbatim for the second-stage strict parse.
    pub settings: toml::Table,
}

impl TryFrom<toml::Table> for SupervisorBlock {
    type Error = String;

    fn try_from(mut table: toml::Table) -> Result<Self, String> {
        let kind = take_string(&mut table, "type")?
            .ok_or_else(|| "a [[supervisor]] block needs a `type`".to_string())?;
        let name = take_string(&mut table, "name")?
            .ok_or_else(|| "a [[supervisor]] block needs a `name`".to_string())?;
        crate::cli::parse_instance_name(&name)
            .map_err(|e| format!("invalid supervisor name {name:?}: {e}"))?;
        let endpoint_port = match take_integer(&mut table, "endpoint_port")? {
            None => 0,
            Some(port) => u16::try_from(port)
                .map_err(|_| format!("supervisor {name:?}: endpoint_port {port} is not a port"))?,
        };
        let stop_timeout_secs = match take_integer(&mut table, "stop_timeout_secs")? {
            None => default_stop_timeout_secs(),
            Some(secs) => u64::try_from(secs).map_err(|_| {
                format!("supervisor {name:?}: stop_timeout_secs must not be negative")
            })?,
        };
        let apply_grace_secs = match take_integer(&mut table, "apply_grace_secs")? {
            None => default_apply_grace_secs(),
            Some(secs) => u64::try_from(secs).map_err(|_| {
                format!("supervisor {name:?}: apply_grace_secs must not be negative")
            })?,
        };
        let attributes = take_string_table(&mut table, "attributes")
            .map_err(|e| format!("supervisor {name:?}: {e}"))?;
        Ok(SupervisorBlock {
            kind,
            name,
            endpoint_port,
            stop_timeout_secs,
            apply_grace_secs,
            attributes,
            settings: table,
        })
    }
}

fn take_string(table: &mut toml::Table, key: &str) -> Result<Option<String>, String> {
    match table.remove(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(format!(
            "`{key}` must be a string, not {}",
            other.type_str()
        )),
    }
}

fn take_integer(table: &mut toml::Table, key: &str) -> Result<Option<i64>, String> {
    match table.remove(key) {
        None => Ok(None),
        Some(toml::Value::Integer(i)) => Ok(Some(i)),
        Some(other) => Err(format!(
            "`{key}` must be an integer, not {}",
            other.type_str()
        )),
    }
}

fn take_string_table(
    table: &mut toml::Table,
    key: &str,
) -> Result<BTreeMap<String, String>, String> {
    match table.remove(key) {
        None => Ok(BTreeMap::new()),
        Some(toml::Value::Table(entries)) => entries
            .into_iter()
            .map(|(k, v)| match v {
                toml::Value::String(s) => Ok((k, s)),
                other => Err(format!(
                    "`{key}.{k}` must be a string, not {}",
                    other.type_str()
                )),
            })
            .collect(),
        Some(other) => Err(format!(
            "`{key}` must be a table of strings, not {}",
            other.type_str()
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM CA bundle that *replaces* the built-in webpki roots — the self-signed-deployment case.
    pub ca_file: PathBuf,
}

/// The transport the endpoint's scheme selects (ADR-0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    WebSocket,
    Http,
}

fn default_endpoint() -> String {
    // The Baseline's default port and path.
    "ws://127.0.0.1:4320/v1/opamp".to_string()
}

fn default_name() -> String {
    "opamp-fleet-client".to_string()
}

fn default_poll_interval_secs() -> u64 {
    30
}

fn default_heartbeat_interval_secs() -> u64 {
    // The Baseline: "The interval between the heartbeats SHOULD be 30 seconds".
    30
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("client-state")
}

fn default_stop_timeout_secs() -> u64 {
    10
}

fn default_apply_grace_secs() -> u64 {
    3
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            endpoint: default_endpoint(),
            name: default_name(),
            poll_interval_secs: default_poll_interval_secs(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            state_dir: default_state_dir(),
            attributes: BTreeMap::new(),
            tls: None,
            supervisors: Vec::new(),
        }
    }
}

impl ClientConfig {
    /// Loads the file, or the defaults when it does not exist. A file that exists but does not
    /// parse is an error — never silently ignored.
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(ClientConfig::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let config: ClientConfig =
            toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        config.check_supervisor_names()?;
        Ok(config)
    }

    /// Supervisor names key state directories and Agent identities — a duplicate would silently
    /// merge two Supervisors into one.
    fn check_supervisor_names(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for block in &self.supervisors {
            if !seen.insert(block.name.as_str()) {
                return Err(format!("duplicate supervisor name {:?}", block.name));
            }
        }
        Ok(())
    }

    /// The operator-defined attributes one Agent reports (ADR-0012): the machine-level table,
    /// with a Supervisor's own entries merged over it per key.
    pub fn agent_attributes(&self, block: Option<&SupervisorBlock>) -> BTreeMap<String, String> {
        let mut merged = self.attributes.clone();
        if let Some(block) = block {
            merged.extend(block.attributes.clone());
        }
        merged
    }

    pub fn transport(&self) -> Result<TransportKind, String> {
        match self.endpoint.split("://").next() {
            Some("ws") | Some("wss") => Ok(TransportKind::WebSocket),
            Some("http") | Some("https") => Ok(TransportKind::Http),
            _ => Err(format!(
                "endpoint {} must start with ws://, wss://, http:// or https://",
                self.endpoint
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_select_websocket_on_port_4320() {
        let cfg = ClientConfig::default();
        assert_eq!(
            cfg.transport().expect("transport"),
            TransportKind::WebSocket
        );
        assert!(cfg.endpoint.contains(":4320/v1/opamp"));
        assert_eq!(cfg.poll_interval_secs, 30);
        // The Baseline's heartbeat default; 0 is the documented way to disable.
        assert_eq!(cfg.heartbeat_interval_secs, 30);
        let disabled: ClientConfig = toml::from_str("heartbeat_interval_secs = 0").expect("parse");
        assert_eq!(disabled.heartbeat_interval_secs, 0);
    }

    #[test]
    fn scheme_selects_the_transport() {
        for (endpoint, kind) in [
            ("ws://x/v1/opamp", TransportKind::WebSocket),
            ("wss://x/v1/opamp", TransportKind::WebSocket),
            ("http://x/v1/opamp", TransportKind::Http),
            ("https://x/v1/opamp", TransportKind::Http),
        ] {
            let cfg = ClientConfig {
                endpoint: endpoint.to_string(),
                ..ClientConfig::default()
            };
            assert_eq!(cfg.transport().expect("transport"), kind);
        }
    }

    #[test]
    fn rejects_an_unknown_scheme_and_unknown_keys() {
        let cfg = ClientConfig {
            endpoint: "ftp://x".to_string(),
            ..ClientConfig::default()
        };
        assert!(cfg.transport().is_err());
        assert!(toml::from_str::<ClientConfig>("endpont = \"ws://x\"").is_err());
    }

    #[test]
    fn supervisor_blocks_split_common_keys_from_plugin_settings() {
        let cfg: ClientConfig = toml::from_str(
            r#"
            [[supervisor]]
            type = "collector"
            name = "otelcol"
            endpoint_port = 4321
            binary = "/usr/local/bin/otelcol"

            [[supervisor]]
            type = "command"
            name = "my-agent"
            command = "/usr/bin/my-agent"
            args = ["--verbose"]
            "#,
        )
        .expect("parse");
        assert_eq!(cfg.supervisors.len(), 2);

        let collector = &cfg.supervisors[0];
        assert_eq!(collector.kind, "collector");
        assert_eq!(collector.name, "otelcol");
        assert_eq!(collector.endpoint_port, 4321);
        assert_eq!(collector.stop_timeout_secs, 10);
        assert_eq!(collector.apply_grace_secs, 3, "the default grace");
        assert_eq!(
            collector.settings.get("binary").and_then(|v| v.as_str()),
            Some("/usr/local/bin/otelcol")
        );
        assert!(!collector.settings.contains_key("type"));

        let command = &cfg.supervisors[1];
        assert_eq!(command.endpoint_port, 0);
        assert!(command.settings.contains_key("args"));
    }

    #[test]
    fn a_supervisor_block_needs_type_and_a_valid_name() {
        let missing_type = toml::from_str::<ClientConfig>("[[supervisor]]\nname = \"x\"\n");
        assert!(missing_type.unwrap_err().to_string().contains("`type`"));

        let missing_name = toml::from_str::<ClientConfig>("[[supervisor]]\ntype = \"command\"\n");
        assert!(missing_name.unwrap_err().to_string().contains("`name`"));

        for bad_name in ["Über", "with space", "-lead", "con"] {
            let toml = format!("[[supervisor]]\ntype = \"command\"\nname = \"{bad_name}\"\n");
            assert!(
                toml::from_str::<ClientConfig>(&toml).is_err(),
                "{bad_name:?} should be rejected"
            );
        }
    }

    #[test]
    fn common_keys_are_type_checked() {
        let bad_port = "[[supervisor]]\ntype = \"command\"\nname = \"x\"\nendpoint_port = 70000\n";
        assert!(toml::from_str::<ClientConfig>(bad_port).is_err());
        let not_an_int =
            "[[supervisor]]\ntype = \"command\"\nname = \"x\"\nendpoint_port = \"a\"\n";
        assert!(toml::from_str::<ClientConfig>(not_an_int).is_err());
        let negative_grace =
            "[[supervisor]]\ntype = \"command\"\nname = \"x\"\napply_grace_secs = -1\n";
        assert!(toml::from_str::<ClientConfig>(negative_grace).is_err());
        let zero_grace: ClientConfig = toml::from_str(
            "[[supervisor]]\ntype = \"command\"\nname = \"x\"\napply_grace_secs = 0\n",
        )
        .expect("parse");
        assert_eq!(zero_grace.supervisors[0].apply_grace_secs, 0);
    }

    #[test]
    fn attributes_parse_at_both_levels_and_merge_per_agent() {
        let cfg: ClientConfig = toml::from_str(
            r#"
            [attributes]
            env = "prod"
            role = "machine"

            [[supervisor]]
            type = "command"
            name = "stub"
            command = "/bin/true"
            [supervisor.attributes]
            role = "edge"
            "#,
        )
        .expect("parse");

        // The self-Agent case: the machine-level table alone.
        assert_eq!(
            cfg.agent_attributes(None).get("env").map(String::as_str),
            Some("prod")
        );

        // A Supervisor's own entries override the machine-level ones per key.
        let merged = cfg.agent_attributes(Some(&cfg.supervisors[0]));
        assert_eq!(merged.get("env").map(String::as_str), Some("prod"));
        assert_eq!(merged.get("role").map(String::as_str), Some("edge"));

        // `attributes` is a common key, never plugin settings.
        assert!(!cfg.supervisors[0].settings.contains_key("attributes"));
    }

    #[test]
    fn non_string_attributes_are_rejected() {
        assert!(toml::from_str::<ClientConfig>("[attributes]\nport = 80\n").is_err());
        let block = "[[supervisor]]\ntype = \"command\"\nname = \"x\"\n[supervisor.attributes]\nflag = true\n";
        assert!(toml::from_str::<ClientConfig>(block).is_err());
    }

    #[test]
    fn duplicate_supervisor_names_are_rejected() {
        let cfg: ClientConfig = toml::from_str(
            r#"
            [[supervisor]]
            type = "command"
            name = "twin"
            [[supervisor]]
            type = "collector"
            name = "twin"
            "#,
        )
        .expect("parses; the duplicate is a semantic error");
        assert!(cfg.check_supervisor_names().is_err());
    }
}
