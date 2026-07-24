//! The Client's own configuration file — TOML (ADR-0025).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// startup instead of silently applying a default.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    /// The Server's OpAMP endpoint. The URL scheme selects the transport (ADR-0023):
    /// `ws://` / `wss://` is the WebSocket transport, `http://` / `https://` the polling one.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// [`CLIENT_AGENT_TYPE`](crate::supervisor::agent::CLIENT_AGENT_TYPE) — `supervisor`, a
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
    /// Optional Gateway Mode (ADR-0034); absent means this Client gateways for nobody.
    pub gateway: Option<GatewayConfig>,
    /// Optional TLS trust override for `wss://` / `https://` endpoints.
    pub tls: Option<TlsConfig>,
    /// Where this Client's own log goes when it runs as a service (ADR-0028). Absent takes the
    /// defaults: a rotating file in the state directory, seven days kept.
    #[serde(default)]
    pub logging: LoggingConfig,
    /// The fleet's timing policy for every Managed Process (ADR-0010): the graceful-stop budget
    /// and the apply grace. Absent takes the defaults.
    #[serde(default, rename = "supervisors")]
    pub supervisor_defaults: SupervisorsConfig,
    /// The largest OpAMP message the Client accepts or sends, on either transport and in either
    /// direction — the Supervisor Endpoint included. The Baseline requires the limit, recommends
    /// this default, and asks that it be configurable.
    #[serde(default = "default_max_message_size")]
    pub max_message_size_bytes: usize,
    /// The `[[supervisor]]` blocks (ADR-0010): each runs one Supervisor managing one local
    /// process, appearing to the Server as its own Agent. Absent means the Client presents
    /// itself as a single Agent, as before.
    #[serde(default, rename = "supervisor")]
    pub supervisors: Vec<SupervisorBlock>,
}

/// One `[[supervisor]]` block (ADR-0010). The common keys are extracted here; everything else
/// stays in [`settings`](Self::settings) for the plugin the `type` selects, which parses it
/// strictly — serde cannot combine `flatten` with `deny_unknown_fields` (serde-rs/serde#1547),
/// so this two-stage split is what keeps a typo anywhere in the block failing loudly at startup.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "toml::Table")]
pub struct SupervisorBlock {
    /// The plugin this block selects (the TOML key `type`), e.g. `"collector"` or `"command"`.
    pub kind: String,
    pub name: String,
    /// The Supervisor Endpoint's loopback port; `0` (the default) binds an ephemeral port. Pin
    /// it when the distributed configuration carries the `opampextension` pointing at it.
    pub endpoint_port: u16,
    /// Overrides the global `[supervisors] stop_timeout_secs` for this Supervisor: how long a
    /// graceful stop may take before the Managed Process is killed. `None` — the default — takes
    /// the global value.
    ///
    /// Only a kind that knows nothing about its agent may state it (ADR-0010): how long an agent
    /// needs to shut down is a property of that agent, so a wrapped kind holds the value and the
    /// key is refused in its block.
    pub stop_timeout_secs: Option<u64>,
    /// Overrides the global `[supervisors] apply_grace_secs` for this Supervisor: how long a
    /// freshly (re)started Managed Process must survive before a received configuration is
    /// acknowledged `APPLIED`; exiting within the grace reports `FAILED` (the health-gated
    /// acknowledgement ADR-0010 names). `0` acknowledges on start. `None` takes the global value,
    /// and a wrapped kind refuses the key for the reason above.
    pub apply_grace_secs: Option<u64>,
    /// The plugin-specific keys, handed over verbatim for the second-stage strict parse.
    pub settings: toml::Table,
}

    ///
    /// Always inside this Supervisor's own `program/` directory (ADR-0032): a Managed Process is
    /// always the fleet's. There is no `owned` flag beside this any more, because there is nothing
    /// left for it to distinguish — every block that parses names a program this Client installs,
    /// so `AcceptsPackages` is a constant of this Client rather than a function of its
    /// configuration.
/// Resolves the program path of a `[[supervisor]]` block (ADR-0032 clause 1, as amended by
/// ADR-0032).
/// wrote. **One shape**, since ADR-0032 removed the second: a **bare file name**, resolving to
/// `<supervisor_dir>/program/<value>` — or `program/tree/<program_path>` for a multi-file package
/// (ADR-0018) — a directory this Client creates and owns, so it may replace what is in it. A bare
/// name cannot escape that directory, which is why nothing here has to sanitize a path.
/// Everything else is refused, and an **absolute path** gets its own message: it is the shape this
/// Client used to accept, so its refusal is the only notice an operator carrying such a block will
/// get and it carries the whole explanation rather than a rule number.
/// Returns an error for anything that is not a bare file name, naming the rule and the way across.
    // The machine's program, which this Client no longer manages (ADR-0032). `has_root` rather
    // than `is_absolute` so the Windows drive-relative form — `\Program Files\otelcol\otelcol.exe`,
    // no drive letter — folds into the same message: it was only ever a near-miss of the absolute
    // form, and both now have the same answer.
    if value.is_absolute() || value.has_root() {
            "supervisor {name:?}: `{key} = {}` names a program on the machine, and this Client \
             manages only programs it installs. A program the fleet is to manage must reach the \
             host as a package: build or repack it, upload it as a Set, and name it here with a \
             bare file name — it then lives in this Supervisor's own directory, where an update \
             is a rename this Client can make. To keep the machine's copy instead, take the block \
             out and let whatever put the file there keep it.",
        return Ok(Program { path });
        "supervisor {name:?}: `{key} = {}` is not a bare file name — no path separator and no \
         `..`. The program lives in this Supervisor's own directory and is updated from \
         Server-offered packages (ADR-0032); name the file, not a path to it",
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
                format!("supervisor {name:?}: stop_timeout_secs must not be negative")
            })?),
        };
        let apply_grace_secs = match take_integer(&mut table, "apply_grace_secs")? {
                format!("supervisor {name:?}: apply_grace_secs must not be negative")
            })?),
        };
        // Retired by ADR-0010, and refused by name rather than left to the plugin's strict parse:
        // a block carrying it was written against a Client that took it, and what replaces it is
        // not another key but a different place entirely.
        if table.contains_key("attributes") {
            return Err(format!(
                "supervisor {name:?}: `[supervisor.attributes]` is no longer a supervisor key — a \
                 Server label tags this Agent from the fleet (ADR-0013), keyed by its \
                 `instance_uid` and matched by the same Selectors; the Client-wide `[attributes]` \
                 still describe the host. Remove the table"
            ));
        }
        // (ADR-0032). ADR-0032 left one shape, so every Supervisor accepts packages and the key
        // would only be a way to disagree with a constant.
                 program is named by a bare file name, so it lives in this Supervisor's own \
                 directory and is updated from Server-offered packages; there is no longer a \
                 second shape for the key to distinguish"
        Ok(SupervisorBlock {
            kind,
            name,
            endpoint_port,
            stop_timeout_secs,
            apply_grace_secs,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
    crate::supervisor::agent::CLIENT_AGENT_TYPE.to_string()
/// The `[logging]` section (ADR-0028): this Client's own log, on disk, while it runs as a service.
///
/// It exists because the Windows SCM discards a service's stderr, so a Client installed there had
/// no readable log at all — and because the OTLP own-logs bridge (ADR-0022) needs a Server that is
/// already reachable, which is precisely what a startup failure is not. In the foreground nothing
/// is written: somebody is reading stderr there.
///
/// It is the machine's, never the Server's. A Server able to redirect or silence a Client's own log
/// could hide its own effects, so nothing here arrives over the wire.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// Write the file at all. `false` is for an operator whose platform already collects stderr —
    /// systemd and launchd do — and who does not want the copy.
    #[serde(default = "default_logging_enabled")]
    pub enabled: bool,
    /// Where the file goes. Absent puts it in the instance's state directory, which survives an
    /// update and which `uninstall` deliberately does not delete (ADR-0028) — the lifetime a log
    /// wants, since one that vanished with a failed install would be missing exactly when needed.
    pub dir: Option<PathBuf>,
    /// How many daily files to keep. The bound is not optional: `0` is refused at load rather than
    /// read as "keep everything", because unbounded is the setting that fills a disk on a host
    /// nobody is watching.
    #[serde(default = "default_log_keep_days")]
    pub keep: usize,
}

fn default_logging_enabled() -> bool {
    true
}

fn default_log_keep_days() -> usize {
    7
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            enabled: default_logging_enabled(),
            dir: None,
            keep: default_log_keep_days(),
        }
    }
}

/// The `[supervisors]` section (ADR-0010): the fleet's timing policy for every Managed Process.
///
/// Global here, because these are decisions about *this deployment* — how long an operator is
/// willing to wait for a stop, how long a restart must hold before it counts as applied — and not
/// about one host. A wrapped kind overrides them where its agent's own behaviour demands it, and
/// nothing below that states them: a block of an unwrapped kind may, because there no kind exists
/// to hold the value.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorsConfig {
    /// How long a graceful stop may take before the Managed Process is killed.
    #[serde(default = "default_stop_timeout_secs")]
    pub stop_timeout_secs: u64,
    /// How long a freshly (re)started Managed Process must survive before a received
    /// configuration is acknowledged `APPLIED`; `0` acknowledges on start.
    #[serde(default = "default_apply_grace_secs")]
    pub apply_grace_secs: u64,
}

impl Default for SupervisorsConfig {
    fn default() -> Self {
        SupervisorsConfig {
            stop_timeout_secs: default_stop_timeout_secs(),
            apply_grace_secs: default_apply_grace_secs(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
/// The `[gateway]` section (ADR-0034): the Client stands at a network boundary, accepts OpAMP from
/// other Clients, and folds them onto a small pool of upstream connections. Present arms the mode;
/// it composes with `[[supervisor]]` blocks on the same host, since the two modes are orthogonal
/// (ADR-0034).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Where the downstream OpAMP endpoint binds. No default: a Gateway that binds nothing is a
    /// configuration error, and loopback is the Supervisor Endpoint's job.
    pub listen: SocketAddr,
    /// The **cap** on upstream connections, not the count. The pool grows to it as Agents appear
    /// and never beyond, so a Gateway in front of three Agents holds three connections.
    #[serde(default = "default_upstream_connections")]
    pub upstream_connections: usize,
    /// TLS for the downstream hop. Mutual TLS is per hop (ADR-0026): what this verifies is the
    /// Agents connecting *here*, and the identity presented *upstream* is the Client's own.
    pub tls: Option<GatewayTlsConfig>,
}

/// The downstream hop's TLS material (ADR-0034). Separate from the top-level `[tls]`, which is
/// about reaching the Server: this is about being reached.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayTlsConfig {
    /// PEM certificate chain this Gateway presents to the Agents that connect to it.
    pub cert_file: PathBuf,
    /// PEM private key for it.
    pub key_file: PathBuf,
    /// Optional PEM bundle a downstream Agent's client certificate must chain to. Absent accepts
    /// any peer at the TLS layer, which is what a fleet still bootstrapping wants.
    pub client_ca_file: Option<PathBuf>,
}

impl GatewayConfig {
    /// Loud validation (ADR-0025): a pool of zero would carry nothing, and the pool is a WebSocket
    /// pool — a polling upstream cannot carry the Server's pushes to the Agents behind it.
    fn check(&self, endpoint: &str) -> Result<(), String> {
        if self.upstream_connections == 0 {
            return Err("[gateway] upstream_connections must be at least 1".to_string());
        }
        if !endpoint.starts_with("ws://") && !endpoint.starts_with("wss://") {
            return Err(format!(
                "[gateway] needs a WebSocket endpoint upstream, and this Client's is {endpoint} —                  a polling connection cannot carry the Server's pushes to the Agents behind a                  Gateway"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM CA bundle that *replaces* the built-in webpki roots — the self-signed-deployment case.
}

/// The transport the endpoint's scheme selects (ADR-0023).
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
}

fn default_poll_interval_secs() -> u64 {
    30
}

fn default_heartbeat_interval_secs() -> u64 {
    // The Baseline: "The interval between the heartbeats SHOULD be 30 seconds".
    30
}

/// The pool cap when none is configured — the OpAMP Gateway Extension's default (ADR-0034). It is
/// a ceiling, not a cost: connections are opened as Agents appear.
fn default_upstream_connections() -> usize {
    10
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("client-state")
}

fn default_max_message_size() -> usize {
    opamp::frame::DEFAULT_MAX_MESSAGE_SIZE
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
            supervisor_defaults: SupervisorsConfig::default(),
            endpoint: default_endpoint(),
            name: default_name(),
            logging: LoggingConfig::default(),
            poll_interval_secs: default_poll_interval_secs(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            state_dir: default_state_dir(),
            gateway: None,
            tls: None,
            max_message_size_bytes: default_max_message_size(),
            supervisors: Vec::new(),
        }
    }
}

/// `path` against the current working directory when it is relative, unchanged when it is not.
///
/// Lexical rather than `canonicalize`: that needs the file to exist, and these directories are
/// named before they are created. It also follows symbolic links, which would be wrong here — the
/// versioned install layout (ADR-0028) points at its current version *with* a link, and resolving
/// it would freeze a path that stops being true at the next update.
pub(crate) fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        // Nothing to be relative to: hand the path over as written and let the failure name the
        // real reason rather than inventing a directory.
        Err(_) => path.to_path_buf(),
    }
}

impl ClientConfig {
    /// Loads the file, or the defaults when it does not exist. A file that exists but does not
    /// parse is an error — never silently ignored.
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            let default = ClientConfig::default();
                state_dir: absolute(&default.state_dir),
                ..default
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        // **Every directory this Client derives is made absolute here**, and this is the one place
        // it can be done once. Since ADR-0010 a Managed Process starts in its own directory, so a
        // path the Client hands it — its program, a `--config` a plugin builds, a `${config_dir}`
        // it substitutes — is resolved by that process against a directory the Client has left.
        // `state_dir` defaults to the relative `client-state`, so leaving these relative made the
        // ordinary configuration the broken one: the program was looked for under itself, and a
        // Collector that did start could not find the configuration written for it.
        config.state_dir = absolute(&config.state_dir);
        config.supervisor_dir = config.supervisor_dir.as_deref().map(absolute);
        config.check_supervisor_names()?;
        if let Some(gateway) = &config.gateway {
            gateway
                .check(&config.endpoint)
        // A limit of zero would refuse every message, and the Baseline knows no "unlimited": the
        // limit is mandatory, so a value that cannot carry a message fails startup.
        if config.max_message_size_bytes == 0 {
            return Err(format!(
                "{}: max_message_size_bytes must be greater than zero",
                path.display()
            ));
        }
        // The retention bound is not optional (ADR-0028). Elsewhere a zero often means "no limit";
        // here that is the one setting that fills a disk on a host nobody is watching, so it fails
        // startup instead of being reachable by typing a digit.
        if config.logging.enabled && config.logging.keep == 0 {
            return Err(format!(
                "{}: [logging] keep must be at least 1 — it is a retention bound, not a switch; \
                 set enabled = false to write no log at all",
                path.display()
            ));
        }
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

        // The block half is gone (ADR-0010): tagging one Agent among several is a Server label's
        // job, which does it from the fleet and takes effect at once. The parameter stays because
        // the two call sites still differ in nothing else, and a future per-block statement would
        // land here.
        let _ = block;
        self.attributes.clone()
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

    /// Every directory this Client derives is absolute, however the operator wrote it.
    ///
    /// Since ADR-0010 a Managed Process starts in its own directory, so a relative path handed to
    /// it is resolved against a directory the Client has left — and `state_dir` defaults to the
    /// relative `client-state`, which made the ordinary configuration the broken one. Two failures
    /// came out of it: the program was looked for beneath itself, and a Collector that did start
    /// could not open the configuration written for it.
    #[test]
    fn a_relative_state_dir_yields_absolute_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(&path, "state_dir = \"client-state\"\n").expect("write");

        let config = ClientConfig::load(&path).expect("load");
        assert!(
            config.state_dir.is_absolute(),
            "state_dir stayed relative: {}",
            config.state_dir.display()
        );
        for derived in [
            config.supervisors_root(),
            config.supervisor_dir("otelcol"),
            config.staging_dir_for(Some("otelcol")),
            config.staging_dir_for(None),
        ] {
            assert!(
                derived.is_absolute(),
                "a derived directory stayed relative: {}",
                derived.display()
            );
        }

        // An explicit supervisor_dir follows the same rule, and an absolute one is left alone.
        std::fs::write(
            &path,
            "state_dir = \"client-state\"\nsupervisor_dir = \"supervisors\"\n",
        )
        .expect("write");
        assert!(ClientConfig::load(&path)
            .expect("load")
            .supervisors_root()
            .is_absolute());
        // An already-absolute path is handed through untouched. What *is* absolute is the
        // platform's answer, not this test's: on Windows `/var/lib/fleet` has no drive and is
        // root-relative, so it would be joined onto the current one — correctly. The fixture is
        // therefore a path this host calls absolute, written as a TOML **literal** string so a
        // Windows backslash needs no escaping.
        let already = dir.path().join("state");
        assert!(already.is_absolute(), "the fixture must be absolute here");
        std::fs::write(&path, format!("state_dir = '{}'\n", already.display())).expect("write");
        assert_eq!(
            ClientConfig::load(&path).expect("load").state_dir,
            already,
            "an absolute path is handed through untouched"
        );
    }
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

    /// ADR-0028. The log is on by default with a bound that cannot be removed, and `[logging]` is
    /// the machine's — so a typo in it fails startup rather than quietly disabling the one thing
    /// that would have explained the next failure.
    #[test]
    fn the_log_file_is_on_by_default_and_its_retention_is_not_optional() {
        let defaults = ClientConfig::default().logging;
        assert!(defaults.enabled);
        assert_eq!(defaults.keep, 7);
        assert!(defaults.dir.is_none(), "the state directory decides");
            toml::from_str("[logging]\nkeep = 3\ndir = \"/var/log/opamp\"\n").expect("parse");
        assert_eq!(configured.logging.keep, 3);
        assert_eq!(
            configured.logging.dir.expect("dir"),
            PathBuf::from("/var/log/opamp")
        );

        let off: ClientConfig = toml::from_str("[logging]\nenabled = false\n").expect("parse");
        assert!(!off.logging.enabled);

        assert!(
            toml::from_str::<ClientConfig>("[logging]\nkep = 3\n").is_err(),
            "a typo fails startup rather than silently taking the default"
        );

        // `keep = 0` is the one setting that fills a disk on a host nobody watches, so it is not
        // reachable: it fails startup and the message points at the switch that does mean "off".
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(&path, "[logging]\nkeep = 0\n").expect("write");
        let err = ClientConfig::load(&path).expect_err("zero retention must fail startup");
        assert!(err.contains("keep"), "{err}");
        assert!(
            err.contains("enabled = false"),
            "it names the way out: {err}"
        );

        // ...but a zero is irrelevant when no file is written at all.
        std::fs::write(&path, "[logging]\nenabled = false\nkeep = 0\n").expect("write");
        assert!(ClientConfig::load(&path).is_ok());
    }

        let dir = tempfile::tempdir().expect("tempdir");
            Some(crate::supervisor::agent::CLIENT_AGENT_TYPE),
            Some(crate::supervisor::agent::CLIENT_AGENT_TYPE)
        let dir = tempfile::tempdir().expect("tempdir");
    /// The Baseline requires a message size limit, recommends 64 MiB, and asks that it be
    /// configurable; zero is not "unlimited" but a limit that could carry nothing, so it fails.
    #[test]
    fn the_message_size_limit_defaults_to_the_recommended_value_and_is_configurable() {
        assert_eq!(
            ClientConfig::default().max_message_size_bytes,
            64 * 1024 * 1024
        );
        let tightened: ClientConfig =
            toml::from_str("max_message_size_bytes = 65536").expect("parse");
        assert_eq!(tightened.max_message_size_bytes, 65536);

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(&path, "max_message_size_bytes = 0\n").expect("write");
        let err = ClientConfig::load(&path).expect_err("zero must fail startup");
        assert!(err.contains("max_message_size_bytes"), "{err}");
    }

        let tightened: ClientConfig =

        let dir = tempfile::tempdir().expect("tempdir");
        let err = ClientConfig::load(&path).expect_err("zero must fail startup");
        let dir = tempfile::tempdir().expect("tempdir");
                command = "agent"
            message.contains("bare file name"),
    /// One shape (ADR-0032): a bare name, which is what puts the program in a directory this
    /// Client owns and may therefore replace. Everything else is refused rather than guessed at.
    fn a_bare_name_resolves_and_everything_else_is_refused() {
            resolved,
    /// ADR-0032: the machine's program is refused, and the message is the only notice an operator
    /// carrying such a block will get — so it must name the way across, not a rule number.
    ///
    /// Whose *spelling* is platform-specific even though the rule is not: on Unix a leading `/`
    /// makes a path absolute, on Windows nothing does until it names a drive. Written per platform
    /// rather than with one string that only happens to work on the machine the tests were first
    /// run on.
    fn an_absolute_program_path_is_refused_and_names_the_way_across() {
        let err = resolve_program("binary", Path::new(foreign), None, &dir, "otelcol")
            .expect_err("a program on the machine must be refused");
        assert!(err.contains("only programs it installs"), "{err}");
        assert!(err.contains("package"), "it names the route: {err}");
        assert!(err.contains("bare file name"), "it names the shape: {err}");
        assert!(err.contains(foreign), "it quotes what was written: {err}");
    /// absolute and is not. Since ADR-0032 it folds into the same refusal as the absolute form,
    /// because it was only ever a near-miss of it and both now have one answer.
    fn a_drive_relative_windows_path_folds_into_the_same_refusal() {
        assert!(err.contains("only programs it installs"), "{err}");
            [[supervisor]]
            type = "command"
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
        // Neither is stated in this block, and unstated now stays unstated: what a Supervisor runs
        // with is resolved against the fleet's `[supervisors]` policy and its kind (ADR-0010),
        // rather than being filled in with a compiled-in number here.
        assert_eq!(collector.stop_timeout_secs, None);
        assert_eq!(collector.apply_grace_secs, None);
        assert_eq!(
            collector.settings.get("binary").and_then(|v| v.as_str()),
            Some("/usr/local/bin/otelcol")
        );
        assert!(!collector.settings.contains_key("type"));

        let command = &cfg.supervisors[1];
        assert_eq!(command.endpoint_port, 0);
        assert!(command.settings.contains_key("args"));
    }

        let cfg: ClientConfig = toml::from_str(
            r#"
            [[supervisor]]
            type = "command"

            [[supervisor]]
            type = "command"
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
        assert_eq!(zero_grace.supervisors[0].apply_grace_secs, Some(0));
    }

        let cfg: ClientConfig = toml::from_str(
            r#"
            [[supervisor]]
            type = "collector"
            r#"
            [[supervisor]]
            type = "collector"
            name = "otelcol"
    /// The Client-wide table stays — it describes the *host*, and it is what a fresh Agent carries
    /// into its first message, before there is anything for a Server to label. The block's own
    /// table is gone, and refused by name (ADR-0010).
    fn attributes_describe_the_host_and_a_block_no_longer_tags_one_agent() {
        let cfg: ClientConfig = toml::from_str(
            r#"

            [[supervisor]]
            type = "command"
        for agent in [None, Some(&cfg.supervisors[0])] {
            let attributes = cfg.agent_attributes(agent);
            assert_eq!(attributes.get("env").map(String::as_str), Some("prod"));
            assert_eq!(attributes.get("role").map(String::as_str), Some("machine"));
        }
        let tagged = "[[supervisor]]\ntype = \"command\"\nname = \"x\"\ncommand = \"/bin/true\"\n\
                      [supervisor.attributes]\nrole = \"edge\"\n";
        let error = toml::from_str::<ClientConfig>(tagged).expect_err("refused");
        assert!(error.to_string().contains("Server label"), "{error}");
        ] {
            let cfg = ClientConfig {
                endpoint: endpoint.to_string(),
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
        let dir = tempfile::tempdir().expect("tempdir");
}
