//! The Client's own configuration file — TOML (ADR-0008).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
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
    /// Where the per-Supervisor directories live (ADR-0021); absent means
    /// `<state_dir>/supervisors`, which is where they have always been. Set it to put the
    /// Managed Processes' programs somewhere `state_dir` cannot go — off a `noexec` mount, or
    /// onto a volume sized for a few hundred megabytes of agent rather than for state.
    pub supervisor_dir: Option<PathBuf>,
    /// Operator-defined attributes (ADR-0012), reported as non-identifying attributes of **every**
    /// Agent this Client presents — machine-level tags like `env = "prod"` that Selectors can
    /// match. A `[[supervisor]]` block's own `attributes` override these per key; attributes the
    /// code or the Managed Process reports win over configured ones.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Optional TLS trust override for `wss://` / `https://` endpoints.
    pub tls: Option<TlsConfig>,
    /// Optional authentication toward the Server (ADR-0013); absent means no `Authorization`
    /// header, as before.
    pub auth: Option<AuthConfig>,
    /// A Server-rotated `Authorization` value (ADR-0014), applied from the persisted connection
    /// settings at startup — never from the file, and it wins over `[auth]`.
    #[serde(skip)]
    pub authorization_override: Option<String>,
    /// Package verification (ADR-0015); absent means unsigned packages are accepted on their
    /// content hash alone.
    pub packages: Option<PackagesConfig>,
    /// Consent for the Server to replace this Client's own binary (ADR-0020); absent — the
    /// default — means the Client's Agent accepts no packages at all.
    pub self_update: Option<SelfUpdateConfig>,
    /// The `[packages].verification_key` decoded once at load — the Ed25519 public key a package
    /// signature is checked against. Set from the file at load; not itself a file key.
    #[serde(skip)]
    pub package_key: Option<Vec<u8>>,
    /// The largest OpAMP message the Client accepts or sends, on either transport and in either
    /// direction — the Supervisor Endpoint included. The Baseline requires the limit, recommends
    /// this default, and asks that it be configurable.
    #[serde(default = "default_max_message_size")]
    pub max_message_size_bytes: usize,
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
    /// Where the program sits *inside* a package that is a whole directory tree (ADR-0023), e.g.
    /// `bin/fluent-bit`. `None` — the default — is the single-file package of ADR-0015: one
    /// member, one file. Setting it is what asks for the tree to be unpacked whole.
    ///
    /// It never decides *whether* packages are taken; the written shape of `binary`/`command`
    /// still does that alone (ADR-0021).
    pub program_path: Option<PathBuf>,
    /// The plugin-specific keys, handed over verbatim for the second-stage strict parse.
    pub settings: toml::Table,
}

/// The subdirectory of a Supervisor's own directory holding its Managed Process (ADR-0021).
///
/// Called `program` and not `bin` on purpose: it holds one file for a single-file package, and a
/// Foreign Agent's whole tree — an executable with the shared objects it loads — is unpacked under
/// the same root (ADR-0023, in [`TREE_DIR`]), so no path on disk moved when that arrived. A
/// directory name is cheap; a layout migration on every host is not.
pub const PROGRAM_DIR: &str = "program";

/// The subdirectory of `program/` holding an unpacked package tree (ADR-0023), with the tree it
/// replaced kept beside it under the same name plus `.rollback`.
///
/// Two fixed names rather than a version directory and a pointer: it is the mechanism the
/// single-file swap already uses, a directory rename is atomic on every platform this Client runs
/// on, and nothing has to be reconciled after a crash halfway through an install. Which version is
/// in there is reported by the Agent, not spelled on disk.
pub const TREE_DIR: &str = "tree";

/// The subdirectory a downloaded artifact is staged in, per Supervisor.
const PACKAGES_DIR: &str = "packages";

/// Where a Supervisor's Managed Process lives — and, as the same fact, whether this Client may
/// replace it (ADR-0021).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// What the process is spawned from, and what a package is installed over.
    pub path: PathBuf,
    /// Whether the Client owns the directory `path` sits in. The swap renames within that
    /// directory rather than writing the file in place, so owning it is exactly what makes an
    /// update possible — which is why this is also the Agent's consent to `AcceptsPackages`.
    pub owned: bool,
}

/// Resolves the program path of a `[[supervisor]]` block and decides, in the same step, whether
/// that Supervisor takes package updates (ADR-0021).
///
/// `key` is the block's own name for it (`binary`, `command`) so the error names what the operator
/// wrote. Three cases, and nothing between them:
///
/// - a **bare file name** — the program lives in `<supervisor_dir>/program/`, a directory this
///   Client creates and owns, so it may be replaced: `owned` is true. A bare name cannot escape
///   that directory, which is why nothing here has to sanitize a path.
/// - an **absolute path** — the machine's file, put there by a distribution package or by
///   configuration management. Spawned, never written to: `owned` is false.
/// - **anything else** — `./x`, `a/b`, `../x`. Refused, rather than guessed at.
///
/// # Errors
/// Returns an error for the third case, naming the rule.
pub fn resolve_program(
    key: &str,
    value: &Path,
    program_path: Option<&Path>,
    supervisor_dir: &Path,
    name: &str,
) -> Result<Program, String> {
    if value.is_absolute() {
        // A tree is unpacked into a directory this Client owns, and an absolute path says the
        // program is the machine's. Refusing beats picking one of the two to ignore.
        if program_path.is_some() {
            return Err(format!(
                "supervisor {name:?}: `{key} = {}` is the machine's program, so there is nowhere \
                 to unpack a package into — drop `program_path`, or name the program with a bare \
                 file name to keep it in this Supervisor's own directory",
                value.display()
            ));
        }
        return Ok(Program {
            path: value.to_path_buf(),
            owned: false,
        });
    }
    // On Windows a rooted path with no drive — `\Program Files\otelcol\otelcol.exe` — is
    // *drive-relative*: it resolves against whichever drive the process happens to be on, which
    // under a service manager is nothing an operator controls. It looks absolute and is not, so it
    // gets a message that says which half is missing instead of the general one below.
    #[cfg(windows)]
    if value.has_root() {
        return Err(format!(
            "supervisor {name:?}: `{key} = {}` is relative to the current drive rather than \
             absolute — name the drive (`C:\\...`) to leave the program to the machine, or use a \
             bare file name to keep it in this Supervisor's own directory",
            value.display()
        ));
    }
    let mut components = value.components();
    let bare = matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none();
    if bare {
        // With a tree the program is one file *inside* the unpacked package (ADR-0023), and the
        // bare name above is what it always was: the consent, readable in the file.
        let path = match program_path {
            Some(inside) => supervisor_dir.join(PROGRAM_DIR).join(TREE_DIR).join(inside),
            None => supervisor_dir.join(PROGRAM_DIR).join(value),
        };
        return Ok(Program { path, owned: true });
    }
    Err(format!(
        "supervisor {name:?}: `{key} = {}` is neither — it must be a bare file name, and then \
         the program lives in this Supervisor's own directory and is updated from Server-offered \
         packages, or an absolute path, and then it is the machine's program and this Client \
         leaves it alone",
        value.display()
    ))
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
        let program_path = match take_string(&mut table, "program_path")
            .map_err(|e| format!("supervisor {name:?}: {e}"))?
        {
            None => None,
            Some(raw) => Some(
                validate_program_path(&raw)
                    .map_err(|e| format!("supervisor {name:?}: `program_path = {raw:?}` {e}"))?,
            ),
        };
        // `package = "name"` chose the artifact on the host; ADR-0017 moved that decision to the
        // Server's Selector. Refuse it loudly rather than ignore a key an operator believes in.
        if table.contains_key("package") {
            return Err(format!(
                "supervisor {name:?}: `package` is no longer a supervisor key — the Server \
                 decides which artifact this Agent receives, through the package's Selector \
                 (PUT /api/v1/packages/<name>/selector)"
            ));
        }
        // And `accepts_packages = true` said *whether*, while the program's path said *where* —
        // two keys for one truth, and nothing ever checked that the second permitted the first
        // (ADR-0021). The path alone decides now, so the key would only be a way to disagree.
        if table.contains_key("accepts_packages") {
            return Err(format!(
                "supervisor {name:?}: `accepts_packages` is no longer a supervisor key — a \
                 program named by a bare file name lives in this Supervisor's own directory and \
                 is updated from Server-offered packages; one named by an absolute path belongs \
                 to the machine and is left alone"
            ));
        }
        Ok(SupervisorBlock {
            kind,
            name,
            endpoint_port,
            stop_timeout_secs,
            apply_grace_secs,
            attributes,
            program_path,
            settings: table,
        })
    }
}

/// Checks a `program_path` (ADR-0023): a relative path inside the package, and nothing that could
/// reach outside it.
///
/// The same three refusals the archive sanitizer makes, made here instead — at startup, where the
/// operator is still looking at the file, rather than at rollout time on every matched host.
///
/// # Errors
/// Returns an error naming which rule the value breaks.
fn validate_program_path(raw: &str) -> Result<PathBuf, String> {
    use std::path::Component;
    let path = Path::new(raw);
    if raw.trim().is_empty() {
        return Err("names nothing".to_string());
    }
    let mut components = path.components().peekable();
    if components.peek().is_none() {
        return Err("names nothing".to_string());
    }
    for component in components {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => return Err("must not contain `.`".to_string()),
            Component::ParentDir => return Err("must not contain `..`".to_string()),
            Component::RootDir | Component::Prefix(_) => {
                return Err(
                    "must be relative — it names a path *inside* the package, not on the host"
                        .to_string(),
                )
            }
        }
    }
    Ok(path.to_path_buf())
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

/// The `[auth]` block (ADR-0013): exactly one scheme — `bearer_token`, or `username` and
/// `password` together. Mixing or halving them fails loudly at startup (ADR-0008).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    pub bearer_token: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl AuthConfig {
    /// The `Authorization` header value this block yields, sent on every plain-HTTP request and
    /// on the WebSocket upgrade.
    pub fn authorization(&self) -> Result<String, String> {
        match (&self.bearer_token, &self.username, &self.password) {
            (Some(token), None, None) => Ok(format!("Bearer {token}")),
            (None, Some(user), Some(password)) => {
                let encoded =
                    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
                Ok(format!("Basic {encoded}"))
            }
            (Some(_), _, _) => Err(
                "[auth] must set either bearer_token or username/password, not both".to_string(),
            ),
            _ => Err("[auth] needs bearer_token, or username and password together".to_string()),
        }
    }
}

/// The `[packages]` block (ADR-0015): how downloaded package artifacts are verified.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagesConfig {
    /// Hex-encoded Ed25519 public key. When set, every offered package MUST carry a valid
    /// signature against it; when unset, an unsigned package is accepted on its content hash alone
    /// and a *signed* one is refused (there is nothing to check it with).
    pub verification_key: Option<String>,
    /// The key that opens an encrypted `.7z` package artifact (ADR-0018). Unset means artifacts are
    /// expected unencrypted; an encrypted one then fails to install, naming this key.
    ///
    /// One secret for the fleet — a single archive serves every Agent — and never the OpAMP
    /// credential from `[auth]`, which the Server rotates on its own (ADR-0014): a rotation would
    /// leave every packed archive unopenable.
    pub archive_key: Option<String>,
}

/// The `[self_update]` block (ADR-0020): consent for the Server to replace *this Client's* binary.
///
/// Absent — the default — the Client's own Agent declares no package capability at all, and no
/// offer can reach it. Present, it takes exactly the package named here.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelfUpdateConfig {
    /// The name of the package that carries this Client. **Required**, and the whole of the
    /// protection: a package with an empty Selector reaches every Agent that accepts packages
    /// (ADR-0017), so without a name to match, the first fleet-wide Collector artifact an operator
    /// uploads would be installed over the Client and take the host out of reach. An offer under
    /// any other name is refused and reported, never applied.
    pub package: String,
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
            endpoint: default_endpoint(),
            name: default_name(),
            poll_interval_secs: default_poll_interval_secs(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            state_dir: default_state_dir(),
            supervisor_dir: None,
            attributes: BTreeMap::new(),
            tls: None,
            auth: None,
            authorization_override: None,
            packages: None,
            self_update: None,
            package_key: None,
            max_message_size_bytes: default_max_message_size(),
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
        let mut config: ClientConfig =
            toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        config.check_supervisor_names()?;
        if let Some(auth) = &config.auth {
            // A half-configured block must fail now, not at the first exchange.
            auth.authorization()
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        // Decode the package verification key once — a malformed key must fail startup, not the
        // first package offer.
        if let Some(key_hex) = config
            .packages
            .as_ref()
            .and_then(|p| p.verification_key.as_ref())
        {
            let key = hex::decode(key_hex).map_err(|e| {
                format!(
                    "{}: [packages].verification_key is not valid hex: {e}",
                    path.display()
                )
            })?;
            config.package_key = Some(key);
        }
        // A limit of zero would refuse every message, and the Baseline knows no "unlimited": the
        // limit is mandatory, so a value that cannot carry a message fails startup.
        if config.max_message_size_bytes == 0 {
            return Err(format!(
                "{}: max_message_size_bytes must be greater than zero",
                path.display()
            ));
        }
        Ok(config)
    }

    /// The Ed25519 public key package signatures are verified against (ADR-0015), or `None`.
    pub fn package_key(&self) -> Option<&[u8]> {
        self.package_key.as_deref()
    }

    /// The root the per-Supervisor directories sit under (ADR-0021) — `supervisor_dir` when the
    /// operator set one, and `<state_dir>/supervisors` when they did not.
    #[must_use]
    pub fn supervisors_root(&self) -> PathBuf {
        self.supervisor_dir
            .clone()
            .unwrap_or_else(|| self.state_dir.join("supervisors"))
    }

    /// One Supervisor's own directory: its state, its `program/`, and its package staging, under
    /// a single root the operator can place (ADR-0021).
    #[must_use]
    pub fn supervisor_dir(&self, name: &str) -> PathBuf {
        self.supervisors_root().join(name)
    }

    /// Where the artifact offered to the Agent at `index` is staged. Inside that Supervisor's own
    /// directory, so that the install which follows is a rename within one filesystem instead of a
    /// copy across two (ADR-0021); the Client's own Agent stages under `state_dir`, beside the
    /// versions a self-update writes (ADR-0020).
    #[must_use]
    pub fn staging_dir(&self, index: usize) -> PathBuf {
        index
            .checked_sub(crate::supervisor::SELF_AGENT_OFFSET)
            .and_then(|block| self.supervisors.get(block))
            .map(|block| self.supervisor_dir(&block.name).join(PACKAGES_DIR))
            .unwrap_or_else(|| self.state_dir.join(PACKAGES_DIR))
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

    /// The `Authorization` value this Client sends, if any: a Server-rotated credential
    /// (ADR-0014) wins over the `[auth]` block (ADR-0013).
    pub fn authorization_value(&self) -> Result<Option<String>, String> {
        if let Some(rotated) = &self.authorization_override {
            return Ok(Some(rotated.clone()));
        }
        self.auth.as_ref().map(|a| a.authorization()).transpose()
    }

    /// Basic and Bearer are cleartext without TLS: sending them beyond the loopback over `ws://`
    /// or `http://` deserves a warning (ADR-0013) — ultimately the operator's choice, so never
    /// an error.
    pub fn sends_credentials_in_cleartext(&self) -> bool {
        if self.auth.is_none() && self.authorization_override.is_none() {
            return false;
        }
        let Some((scheme, rest)) = self.endpoint.split_once("://") else {
            return false;
        };
        if scheme == "wss" || scheme == "https" {
            return false;
        }
        let host_port = rest.split(['/', '?']).next().unwrap_or("");
        // A bracketed IPv6 host keeps its brackets; only a trailing `:port` is cut off.
        let host = match host_port.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(""),
            None => host_port.split(':').next().unwrap_or(""),
        };
        !matches!(host, "localhost" | "127.0.0.1" | "::1")
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

    /// ADR-0020: self-update is off unless the file says otherwise, and saying so means naming the
    /// package. A section without a name does not parse — an operator who half-configured this
    /// would otherwise have consented to receive whatever the fleet receives.
    #[test]
    fn self_update_is_off_by_default_and_must_name_its_package() {
        assert!(ClientConfig::default().self_update.is_none());
        let untouched: ClientConfig =
            toml::from_str("endpoint = \"ws://h/v1/opamp\"").expect("parse");
        assert!(untouched.self_update.is_none());

        let armed: ClientConfig =
            toml::from_str("[self_update]\npackage = \"opamp-client\"\n").expect("parse");
        assert_eq!(armed.self_update.expect("armed").package, "opamp-client");

        assert!(
            toml::from_str::<ClientConfig>("[self_update]\n").is_err(),
            "a section without a package name is not consent to anything"
        );
        assert!(
            toml::from_str::<ClientConfig>("[self_update]\npackge = \"x\"\n").is_err(),
            "a typo fails startup rather than silently disabling self-update"
        );
    }

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
        let path = dir.path().join("client.toml");
        std::fs::write(&path, "max_message_size_bytes = 0\n").expect("write");
        let err = ClientConfig::load(&path).expect_err("zero must fail startup");
        assert!(err.contains("max_message_size_bytes"), "{err}");
    }

    /// Both keys that once configured package delivery on the host are refused rather than
    /// ignored: `package` named the artifact (ADR-0017 moved that to the Server's Selector), and
    /// `accepts_packages` said whether to take one (ADR-0021 derives that from the program's
    /// path). An operator who still has either in a file believes it does something.
    #[test]
    fn the_retired_package_keys_are_refused() {
        let block = |extra: &str| {
            format!(
                r#"
                [[supervisor]]
                type = "command"
                name = "agent"
                command = "/usr/local/bin/agent"
                {extra}
                "#
            )
        };
        assert!(
            toml::from_str::<ClientConfig>(&block("")).is_ok(),
            "a block without either key still parses"
        );

        let stale = toml::from_str::<ClientConfig>(&block("package = \"otelcol\""))
            .expect_err("the old naming key must fail loudly");
        assert!(
            stale.to_string().contains("Selector"),
            "the error says what decides instead: {stale}"
        );

        let consent = toml::from_str::<ClientConfig>(&block("accepts_packages = true"))
            .expect_err("the old consent key must fail loudly");
        let message = consent.to_string();
        assert!(
            message.contains("bare file name") && message.contains("absolute path"),
            "the error states the rule that replaced it: {message}"
        );
    }

    /// A bare name is what makes the program this Client's to replace, and everything that is
    /// neither a bare name nor absolute is refused rather than guessed at. Both halves are
    /// spelled the same way on every platform, which is why they are tested here together.
    #[test]
    fn a_bare_name_is_owned_and_anything_between_the_two_cases_is_refused() {
        let dir = PathBuf::from("/srv/fleet/otelcol");

        let owned = resolve_program(
            "binary",
            Path::new("otelcol-contrib"),
            None,
            &dir,
            "otelcol",
        )
        .expect("a bare file name resolves");
        assert_eq!(
            owned,
            Program {
                path: dir.join(PROGRAM_DIR).join("otelcol-contrib"),
                owned: true,
            }
        );

        // `..` in particular never reaches a `join`, which is why nothing downstream has a path
        // to sanitize.
        for refused in ["./otelcol", "bin/otelcol", "../otelcol", "a/../../b", ""] {
            let err = resolve_program("binary", Path::new(refused), None, &dir, "otelcol")
                .expect_err("must be refused: {refused}");
            assert!(err.contains("bare file name"), "{refused}: {err}");
        }
    }

    /// With a tree (ADR-0023) the program is one file *inside* the package, so the spawn path is
    /// the one the configuration writes — and the bare name keeps meaning exactly what ADR-0021
    /// made it mean, which is consent and nothing else.
    #[test]
    fn a_tree_spawns_from_the_path_written_inside_the_package() {
        let dir = PathBuf::from("/srv/fleet/fluent-bit");

        let resolved = resolve_program(
            "command",
            Path::new("fluent-bit"),
            Some(Path::new("bin/fluent-bit")),
            &dir,
            "fluent-bit",
        )
        .expect("a bare name with a program_path resolves");
        assert_eq!(
            resolved,
            Program {
                path: dir.join(PROGRAM_DIR).join(TREE_DIR).join("bin/fluent-bit"),
                owned: true,
            },
            "the spawn path is readable in the file, before any package exists"
        );

        // The machine's program has no directory this Client may unpack into, and picking one of
        // the two keys to ignore would be the worst of the three answers.
        //
        // Written per platform, for the reason the test below this one states: `/opt/...` is not
        // absolute on Windows, it is *drive-relative*, and it would be refused there for that
        // reason instead — the same green result for the wrong reason, which is how a rule stops
        // being tested without anyone noticing.
        #[cfg(unix)]
        let foreign = "/opt/fluent-bit/bin/fluent-bit";
        #[cfg(windows)]
        let foreign = r"C:\fluent-bit\bin\fluent-bit.exe";
        let err = resolve_program(
            "command",
            Path::new(foreign),
            Some(Path::new("bin/fluent-bit")),
            &dir,
            "fluent-bit",
        )
        .expect_err("absolute and a tree cannot both be meant");
        assert!(err.contains("program_path"), "{err}");
    }

    /// Refused at startup, where the operator is still looking at the file — not at rollout time
    /// on every matched host, which is where the archive sanitizer would catch the same thing.
    #[test]
    fn a_program_path_must_stay_inside_the_package() {
        assert_eq!(
            validate_program_path("bin/fluent-bit").expect("relative"),
            PathBuf::from("bin/fluent-bit")
        );
        for (refused, because) in [
            ("../../etc/passwd", ".."),
            ("bin/../../x", ".."),
            ("./bin/fluent-bit", "`.`"),
            ("", "nothing"),
            ("   ", "nothing"),
        ] {
            let err = validate_program_path(refused).expect_err("must be refused: {refused}");
            assert!(
                err.contains(because),
                "{refused}: {err} does not say {because}"
            );
        }
        #[cfg(unix)]
        assert!(validate_program_path("/opt/fluent-bit/bin/fluent-bit")
            .expect_err("absolute")
            .contains("relative"));
        #[cfg(windows)]
        assert!(validate_program_path("C:\\fluent-bit\\bin\\fluent-bit.exe")
            .expect_err("absolute")
            .contains("relative"));
    }

    /// The other half of the rule, whose *spelling* is platform-specific even though the rule is
    /// not: on Unix a leading `/` makes a path absolute, on Windows nothing does until it names a
    /// drive. Written per platform rather than with one string that only happens to work on the
    /// machine the tests were first run on.
    #[test]
    fn an_absolute_program_path_is_the_machines_and_takes_no_packages() {
        let dir = PathBuf::from("/srv/fleet/otelcol");
        #[cfg(unix)]
        let foreign = "/usr/local/bin/otelcol-contrib";
        #[cfg(windows)]
        let foreign = r"C:\Program Files\otelcol\otelcol-contrib.exe";

        let resolved = resolve_program("binary", Path::new(foreign), None, &dir, "otelcol")
            .expect("an absolute path resolves");
        assert_eq!(
            resolved,
            Program {
                path: PathBuf::from(foreign),
                owned: false,
            }
        );
    }

    /// The case Windows adds and Unix has no equivalent of: `\Program Files\...` carries a root
    /// but no drive, so it resolves against whichever drive the process is on — it *looks*
    /// absolute and is not. Refused like any other in-between path, but told apart from a typo:
    /// the operator wrote something meaningful, it just is not a path a service can rely on.
    #[cfg(windows)]
    #[test]
    fn a_drive_relative_windows_path_is_refused_and_says_what_is_missing() {
        let dir = PathBuf::from(r"C:\ProgramData\fleet\otelcol");
        let err = resolve_program(
            "binary",
            Path::new(r"\Program Files\otelcol\otelcol.exe"),
            &dir,
            "otelcol",
        )
        .expect_err("a drive-relative path must be refused");
        assert!(
            err.contains("current drive"),
            "the message names what is missing rather than calling it neither: {err}"
        );
    }

    /// The per-Supervisor root is `<state_dir>/supervisors` unless the operator moved it, and
    /// everything that Supervisor owns hangs off the same place (ADR-0021).
    #[test]
    fn the_supervisor_root_defaults_under_the_state_dir_and_is_relocatable() {
        let default = ClientConfig {
            state_dir: PathBuf::from("/var/lib/fleet/state"),
            ..ClientConfig::default()
        };
        assert_eq!(
            default.supervisor_dir("otelcol"),
            PathBuf::from("/var/lib/fleet/state/supervisors/otelcol")
        );

        let moved: ClientConfig = toml::from_str(
            r#"
            state_dir = "/var/lib/fleet/state"
            supervisor_dir = "/opt/fleet/supervisors"

            [[supervisor]]
            type = "command"
            name = "agent"
            command = "agent"
            "#,
        )
        .expect("parse");
        assert_eq!(
            moved.supervisor_dir("agent"),
            PathBuf::from("/opt/fleet/supervisors/agent")
        );
        // The Client's own Agent keeps staging beside its versions; a Supervisor stages in its own
        // directory, which is what makes the install a rename rather than a copy.
        assert_eq!(
            moved.staging_dir(crate::supervisor::SELF_AGENT_INDEX),
            PathBuf::from("/var/lib/fleet/state/packages")
        );
        assert_eq!(
            moved.staging_dir(crate::supervisor::SELF_AGENT_OFFSET),
            PathBuf::from("/opt/fleet/supervisors/agent/packages")
        );
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
    fn auth_yields_exactly_one_authorization_scheme() {
        let bearer: ClientConfig = toml::from_str("[auth]\nbearer_token = \"tok\"").expect("parse");
        assert_eq!(
            bearer.auth.expect("auth").authorization().expect("value"),
            "Bearer tok"
        );

        let basic: ClientConfig =
            toml::from_str("[auth]\nusername = \"fleet\"\npassword = \"secret\"").expect("parse");
        assert_eq!(
            basic.auth.expect("auth").authorization().expect("value"),
            // base64("fleet:secret")
            "Basic ZmxlZXQ6c2VjcmV0"
        );

        // Mixing the schemes, halving Basic, or an empty block all fail loudly.
        for bad in [
            "[auth]\nbearer_token = \"tok\"\nusername = \"fleet\"\npassword = \"s\"",
            "[auth]\nusername = \"fleet\"",
            "[auth]\npassword = \"secret\"",
            "[auth]",
        ] {
            let cfg: ClientConfig = toml::from_str(bad).expect("parses; the mix is semantic");
            assert!(
                cfg.auth.expect("auth").authorization().is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(toml::from_str::<ClientConfig>("[auth]\ntoken = \"x\"").is_err());
    }

    #[test]
    fn cleartext_credentials_are_flagged_beyond_the_loopback() {
        for (endpoint, cleartext) in [
            ("ws://fleet.example:4320/v1/opamp", true),
            ("http://10.0.0.7:4320/v1/opamp", true),
            ("ws://127.0.0.1:4320/v1/opamp", false),
            ("http://localhost:4320/v1/opamp", false),
            ("ws://[::1]:4320/v1/opamp", false),
            ("wss://fleet.example:4320/v1/opamp", false),
            ("https://fleet.example:4320/v1/opamp", false),
        ] {
            let cfg = ClientConfig {
                endpoint: endpoint.to_string(),
                auth: Some(AuthConfig {
                    bearer_token: Some("tok".to_string()),
                    username: None,
                    password: None,
                }),
                ..ClientConfig::default()
            };
            assert_eq!(
                cfg.sends_credentials_in_cleartext(),
                cleartext,
                "{endpoint}"
            );
        }

        // Without [auth] there is nothing to leak.
        let no_auth = ClientConfig {
            endpoint: "ws://fleet.example:4320/v1/opamp".to_string(),
            ..ClientConfig::default()
        };
        assert!(!no_auth.sends_credentials_in_cleartext());
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
