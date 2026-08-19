//! The Client's own configuration file — TOML (ADR-0008).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::Deserialize;

/// `supervisor.toml`. Every setting has a default; unknown keys are rejected so a typo fails loudly at
/// startup instead of silently applying a default.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    /// The Server's OpAMP endpoint. The URL scheme selects the transport (ADR-0007):
    /// `ws://` / `wss://` is the WebSocket transport, `http://` / `https://` the polling one.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// The operator's name for the Client's own Agent, reported as `service.instance.name`
    /// (ADR-0033) — *which* Client this is. Its `service.name` is the type
    /// [`CLIENT_SERVICE_NAME`](crate::supervisor::agent::CLIENT_SERVICE_NAME) — `supervisor`, a
    /// constant (ADR-0077) — so this key cannot state it: every Client in a fleet is the same kind
    /// of thing.
    #[serde(default = "default_name")]
    pub name: String,
    /// The deployment's `service.namespace`. The Baseline asks for it "if it is used in the
    /// environment where the Agent runs", which is knowledge only an operator has — so it is
    /// configured rather than detected, and absent means it is not reported at all. Reported as
    /// an **identifying** attribute of every Agent this Client presents, which is where the
    /// Baseline puts it: it says which deployment the service belongs to.
    pub service_namespace: Option<String>,
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
    /// Optional Gateway Mode (ADR-0037); absent means this Client gateways for nobody.
    pub gateway: Option<GatewayConfig>,
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
    /// Consent for the Server to replace this Client's own binary (ADR-0020). Absent means the
    /// section's own defaults, which since ADR-0075 are **consent given** under the Client's own
    /// name — write `enabled = false` to withdraw it.
    #[serde(default)]
    pub self_update: SelfUpdateConfig,
    /// Where this Client's own log goes when it runs as a service (ADR-0041). Absent takes the
    /// defaults: a rotating file in the state directory, seven days kept.
    #[serde(default)]
    pub logging: LoggingConfig,
    /// How Managed-Process package updates behave once applied (ADR-0058) — the retention of a
    /// superseded version. Absent takes the defaults: one day.
    #[serde(default)]
    pub updates: UpdatesConfig,
    /// The `[packages].verification_key` decoded once at load — the Ed25519 public key a package
    /// signature is checked against. Set from the file at load; not itself a file key.
    #[serde(skip)]
    pub package_key: Option<Vec<u8>>,
    /// The file's own text with secret values masked (see [`redact_secrets`]), kept from load so
    /// the Client's own Agent can report it as its effective configuration — the file *is* what
    /// this Client runs (a file that fails to load fails startup, so a running Client and its file
    /// never disagree). `None` when no file exists and the defaults run.
    #[serde(skip)]
    pub source: Option<String>,
    /// The path this configuration was loaded from — where an accepted Supervisor set is written
    /// back to (ADR-0056). Kept even when the file does not exist yet: the first applied offer
    /// creates it. `None` only for a configuration never loaded from a path (tests, defaults).
    #[serde(skip)]
    pub path: Option<PathBuf>,
    /// The largest OpAMP message the Client accepts or sends, on either transport and in either
    /// direction — the Supervisor Endpoint included. The Baseline requires the limit, recommends
    /// this default, and asks that it be configurable.
    #[serde(default = "default_max_message_size")]
    pub max_message_size_bytes: usize,
    /// The largest package or self-update artifact the Client downloads before verifying it
    /// (ADR-0015). Streaming already caps peak memory at one chunk, but disk is finite: without a
    /// ceiling a Server could answer the artifact GET with an endless body and fill the staging
    /// filesystem before the content hash is ever checked. Matches the Server's own per-package
    /// ceiling; `0` is refused at load, the same as the message limit.
    #[serde(default = "default_max_artifact_size")]
    pub max_artifact_size_bytes: u64,
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
    /// The Supervisor's name: the Agent's `service.instance.name` and its state directory name, so
    /// it follows the instance-name grammar of ADR-0010. Must be unique across blocks.
    ///
    /// It is the operator's name for *this* Agent, never its type — that is
    /// [`service_name`](Self::service_name), which the grammar here could not spell anyway
    /// (ADR-0033): a reverse FQDN has dots, and this value is a path component on three operating
    /// systems.
    pub name: String,
    /// The Agent *type* this Supervisor presents as `service.name` — the Baseline's "reverse FQDN
    /// that uniquely identifies the Agent type" (ADR-0033). `None` falls back to the program's own
    /// file name, and a Managed Process that reports a type of its own overrides both: a
    /// Collector's `opampextension` states the `dist.name` it was built with, which is the truth
    /// this key can only approximate.
    ///
    /// Set it for a Managed Process that reports nothing — the core `otelcol` distribution, every
    /// Foreign Agent — so a Selector can aim at what this Agent *is* (ADR-0017).
    pub service_name: Option<String>,
    /// The Supervisor Endpoint's loopback port; `0` (the default) binds an ephemeral port. Pin
    /// it when the distributed configuration carries the `opampextension` pointing at it.
    pub endpoint_port: u16,
    /// How long a graceful stop may take before the Managed Process is killed.
    pub stop_timeout_secs: u64,
    /// How long a freshly (re)started Managed Process must survive before a received
    /// configuration is acknowledged `APPLIED`; exiting within the grace reports `FAILED`
    /// (the health-gated acknowledgement ADR-0011 names). `0` acknowledges on start, as before.
    pub apply_grace_secs: u64,
    /// Overrides the global `[updates] retain_previous_secs` for this Supervisor (ADR-0058): how
    /// long the version a successful update supersedes is kept before deletion. `None` — the
    /// default — takes the global value.
    pub retain_previous_secs: Option<u64>,
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
        // Deliberately not run through `parse_instance_name`: a type may be a reverse FQDN
        // (ADR-0033), which that grammar forbids. Only emptiness is refused — an empty
        // `service.name` would report "no type" as if it were one.
        let service_name = match take_string(&mut table, "service_name")
            .map_err(|e| format!("supervisor {name:?}: {e}"))?
        {
            Some(raw) if raw.trim().is_empty() => {
                return Err(format!(
                    "supervisor {name:?}: `service_name` must not be empty — leave it out to use \
                     the program's file name"
                ));
            }
            other => other,
        };
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
        let retain_previous_secs = match take_integer(&mut table, "retain_previous_secs")? {
            None => None,
            Some(secs) => Some(u64::try_from(secs).map_err(|_| {
                format!("supervisor {name:?}: retain_previous_secs must not be negative")
            })?),
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
            service_name,
            endpoint_port,
            stop_timeout_secs,
            apply_grace_secs,
            retain_previous_secs,
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
#[derive(Debug, Clone, Deserialize)]
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

/// Keys whose values are credentials: `[auth]`'s `bearer_token` and `password`, and
/// `[packages]`'s `archive_key`. Paths and public keys are not on the list — a path locates a
/// secret, it is not one, and the `verification_key` is the *public* half of the signing pair.
const SECRET_KEYS: &[&str] = &["bearer_token", "password", "archive_key"];

/// The file's text with every secret value replaced by `***`, for reporting it off the host —
/// the Server persists effective configurations to disk, so a credential must never be in one.
///
/// Text-based on purpose: parsing and re-serialising would drop the operator's comments and
/// ordering, which are half of what a configuration file says. A line assigning a secret key
/// keeps its key with a masked value; any other non-comment line merely *mentioning* a secret
/// key (an inline table, some spelling this scan does not know) is masked whole — over-redaction
/// is the cheap failure here, a leaked credential the expensive one.
pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start();
        let assigned_key = SECRET_KEYS.iter().find(|key| {
            trimmed
                .split_once('=')
                .is_some_and(|(lhs, _)| lhs.trim() == **key)
        });
        if trimmed.starts_with('#') || trimmed.is_empty() {
            out.push_str(line);
        } else if let Some(key) = assigned_key {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(indent);
            out.push_str(key);
            out.push_str(" = \"***\"");
        } else if SECRET_KEYS.iter().any(|key| line.contains(key)) {
            out.push_str("# (line redacted: it mentions a credential key)");
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

/// The `[packages]` block (ADR-0015): how downloaded package artifacts are verified.
#[derive(Debug, Clone, Deserialize)]
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
/// **The section is absent on most hosts, and absent means consent** (ADR-0075, superseding
/// ADR-0027 point 4): a Client the fleet cannot update is a Client that has to be updated by hand
/// on every host, which is the state fleet management exists to end. What used to be the default —
/// no consent at all — is now written down, as `enabled = false`.
///
/// The *name* is what the consent is narrowed to, and it does the work the absent section used to:
/// a package with an empty Selector reaches every Agent that accepts packages (ADR-0017), so
/// without a name to match, the first fleet-wide Collector artifact an operator uploads would be
/// installed over the Client and take the host out of reach. An offer under any other name is
/// refused and reported, never applied. The default name is the Client's own Agent type —
/// `supervisor` since ADR-0077 — which is what a Set carrying this Client is keyed by anyway
/// (ADR-0034), so the default is not a wildcard: it is the one package that could legitimately be
/// this Client.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelfUpdateConfig {
    /// Whether the consent stands. `false` is the withdrawal — the Client's own Agent then declares
    /// no package capability at all and no offer can reach it, which is exactly what an absent
    /// section meant before ADR-0075.
    #[serde(default = "default_self_update_enabled")]
    pub enabled: bool,
    /// The name of the package that carries this Client; defaults to the Client's own Agent type
    /// (ADR-0077). An empty name with the consent standing is refused at load: the name is the
    /// whole of the narrowing, and an empty one would widen it to every package the Server offers.
    #[serde(default = "default_self_update_package")]
    pub package: String,
}

impl Default for SelfUpdateConfig {
    fn default() -> Self {
        SelfUpdateConfig {
            enabled: default_self_update_enabled(),
            package: default_self_update_package(),
        }
    }
}

/// What the configuration file was called until ADR-0080, and the one reason a missing file is an
/// error rather than the defaults.
pub const LEGACY_CONFIG_FILE_NAME: &str = "client.toml";

/// Refuses to carry on when the configuration is only *missing* because it was renamed
/// (ADR-0080): the file this Client looks for is absent and a `supervisor.toml` — what it was called
/// until ADR-0080 — sits where it would be.
///
/// Everywhere else a missing configuration is not an error: a Client comes up on defaults, says so,
/// and manages nothing until one exists (ADR-0027). That is exactly the wrong answer here, and the
/// dangerous one: an upgraded host would go on running, connect to the development endpoint, report
/// none of the Agents it used to, and nothing about it would look like a failure. So this one case
/// fails closed, naming both paths and the single command that fixes it.
fn legacy_name_beside(path: &Path) -> Result<(), String> {
    let legacy = path.with_file_name(LEGACY_CONFIG_FILE_NAME);
    if path
        .file_name()
        .is_some_and(|name| name == LEGACY_CONFIG_FILE_NAME)
        || !legacy.exists()
    {
        return Ok(());
    }
    Err(format!(
        "no configuration at {}, but {} is beside it: the file was renamed in this release \
         (ADR-0080). Rename it — `mv {} {}` — and start the service again. Nothing else about it \
         changed.",
        path.display(),
        legacy.display(),
        legacy.display(),
        path.display()
    ))
}

fn default_self_update_enabled() -> bool {
    true
}

/// The Client's own Agent type (ADR-0077): the Set that carries this Client is keyed by the type it
/// is built for (ADR-0034), so the type is also what names it. Deliberately *not* the product's name
/// [`layout::COMPONENT`](crate::service::layout::COMPONENT), which since ADR-0077 is a different
/// string and names the binary, the service, and the version directories rather than the package.
fn default_self_update_package() -> String {
    crate::supervisor::agent::CLIENT_SERVICE_NAME.to_string()
}

/// The `[logging]` section (ADR-0041): this Client's own log, on disk, while it runs as a service.
///
/// It exists because the Windows SCM discards a service's stderr, so a Client installed there had
/// no readable log at all — and because the OTLP own-logs bridge (ADR-0036) needs a Server that is
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
    /// update and which `uninstall` deliberately does not delete (ADR-0010) — the lifetime a log
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

/// The `[updates]` section (ADR-0058): how a Managed Process's package updates behave once applied.
/// Global here, overridable per `[[supervisor]]` block, the shape `apply_grace_secs` already has.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatesConfig {
    /// How long the version a successful update supersedes is kept before it is deleted, so an
    /// operator has a fallback window (ADR-0058). `0` deletes it on success, the pre-ADR-0058
    /// behaviour. A per-Supervisor `retain_previous_secs` overrides this for one block.
    #[serde(default = "default_retain_previous_secs")]
    pub retain_previous_secs: u64,
}

fn default_retain_previous_secs() -> u64 {
    24 * 60 * 60 // one day
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        UpdatesConfig {
            retain_previous_secs: default_retain_previous_secs(),
        }
    }
}

/// The `[gateway]` section (ADR-0037): the Client stands at a network boundary, accepts OpAMP from
/// other Clients, and folds them onto a small pool of upstream connections. Present arms the mode;
/// it composes with `[[supervisor]]` blocks on the same host, since the two modes are orthogonal
/// (ADR-0003).
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
    /// The most distinct Agents a single downstream connection may carry. It bounds the routing
    /// state one peer can make this Gateway hold: a misbehaving or hostile downstream Client
    /// streaming reports under endless fabricated `instance_uid`s would otherwise grow the registry
    /// and pool maps without limit. Generous for a nested Gateway carrying a real sub-fleet; a
    /// report for a *new* Agent past the cap is dropped, the ones already carried keep working. `0`
    /// is refused at load.
    #[serde(default = "default_max_carried_agents")]
    pub max_carried_agents: usize,
    /// TLS for the downstream hop. Mutual TLS is per hop (ADR-0035): what this verifies is the
    /// Agents connecting *here*, and the identity presented *upstream* is the Client's own.
    pub tls: Option<GatewayTlsConfig>,
}

/// The downstream hop's TLS material (ADR-0037). Separate from the top-level `[tls]`, which is
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
    /// Loud validation (ADR-0008): a pool of zero would carry nothing, and the pool is a WebSocket
    /// pool — a polling upstream cannot carry the Server's pushes to the Agents behind it.
    fn check(&self, endpoint: &str) -> Result<(), String> {
        if self.upstream_connections == 0 {
            return Err("[gateway] upstream_connections must be at least 1".to_string());
        }
        if self.max_carried_agents == 0 {
            return Err(
                "[gateway] max_carried_agents must be at least 1 — it bounds routing state, not a \
                 switch"
                    .to_string(),
            );
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
    /// Optional, so a `[tls]` section can carry a client identity alone and keep the public roots.
    pub ca_file: Option<PathBuf>,
    /// PEM client certificate chain this Client presents (ADR-0035), for a fleet whose Server
    /// demands mutual TLS. Together with [`key_file`](Self::key_file), and useless without it.
    ///
    /// This is the operator-provisioned identity, including the **bootstrap certificate** a host
    /// enrols with. An identity the Server issued outranks it: the Client stores that one in its
    /// state directory and prefers it, exactly as persisted connection settings outrank the
    /// endpoint written here (ADR-0014). Deleting the stored pair falls back to this one.
    pub cert_file: Option<PathBuf>,
    /// PEM private key for [`cert_file`](Self::cert_file). Never leaves the host.
    pub key_file: Option<PathBuf>,
}

impl TlsConfig {
    /// Loud validation (ADR-0008): half an identity is a configuration error, not a fallback to
    /// none — a Server demanding mutual TLS would refuse the connection with no hint why.
    fn check(&self) -> Result<(), String> {
        match (&self.cert_file, &self.key_file) {
            (Some(_), None) => Err("[tls] cert_file needs key_file beside it".to_string()),
            (None, Some(_)) => Err("[tls] key_file needs cert_file beside it".to_string()),
            _ => Ok(()),
        }
    }
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
    // The program's own name (ADR-0080), which is what an operator who has not chosen one sees.
    // It reads the same as the Agent *type* on such a host, and that is the honest answer rather
    // than a defect: an unconfigured Client is not a particular one, and the questionnaire asks
    // for this key first (ADR-0027) precisely so that a fleet's Clients are told apart by a name
    // somebody chose. Naming the product it used to be would be worse — that word is on no file,
    // no service and no artifact any more.
    crate::service::layout::COMPONENT.to_string()
}

fn default_poll_interval_secs() -> u64 {
    30
}

fn default_heartbeat_interval_secs() -> u64 {
    // The Baseline: "The interval between the heartbeats SHOULD be 30 seconds".
    30
}

/// The pool cap when none is configured — the OpAMP Gateway Extension's default (ADR-0037). It is
/// a ceiling, not a cost: connections are opened as Agents appear.
fn default_upstream_connections() -> usize {
    10
}

/// Generous enough for a nested Gateway carrying a real sub-fleet, small enough that a single
/// hostile connection cannot grow the routing maps without bound.
fn default_max_carried_agents() -> usize {
    10_000
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("client-state")
}

fn default_max_message_size() -> usize {
    opamp::frame::DEFAULT_MAX_MESSAGE_SIZE
}

/// One gibibyte — the Server's own `DEFAULT_MAX_PACKAGE_SIZE`. A Server that will not store a
/// larger artifact never offers one, so the two ends agree by default.
fn default_max_artifact_size() -> u64 {
    1 << 30
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
            logging: LoggingConfig::default(),
            updates: UpdatesConfig::default(),
            service_namespace: None,
            poll_interval_secs: default_poll_interval_secs(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            state_dir: default_state_dir(),
            supervisor_dir: None,
            attributes: BTreeMap::new(),
            gateway: None,
            tls: None,
            auth: None,
            authorization_override: None,
            packages: None,
            self_update: SelfUpdateConfig::default(),
            package_key: None,
            source: None,
            path: None,
            max_message_size_bytes: default_max_message_size(),
            max_artifact_size_bytes: default_max_artifact_size(),
            supervisors: Vec::new(),
        }
    }
}

impl ClientConfig {
    /// Loads the file, or the defaults when it does not exist. A file that exists but does not
    /// parse is an error — never silently ignored.
    /// The package this Client consents to be replaced by, or `None` when the consent is withdrawn
    /// (ADR-0020, ADR-0075). The one place the two fields of `[self_update]` are read together, so
    /// no caller can honour the name while ignoring the switch.
    #[must_use]
    pub fn self_update_package(&self) -> Option<&str> {
        self.self_update
            .enabled
            .then_some(self.self_update.package.as_str())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            legacy_name_beside(path)?;
            return Ok(ClientConfig {
                path: Some(path.to_path_buf()),
                ..ClientConfig::default()
            });
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut config: ClientConfig =
            toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        // Redacted once, here, so no later reader can reach for the unredacted text by mistake:
        // everything downstream — the effective-configuration report above all — sees the mask.
        config.source = Some(redact_secrets(&text));
        config.path = Some(path.to_path_buf());
        config.check_supervisor_names()?;
        if let Some(auth) = &config.auth {
            // A half-configured block must fail now, not at the first exchange.
            auth.authorization()
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        if let Some(tls) = &config.tls {
            tls.check()
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        // An empty name with the consent standing must fail startup rather than widen the consent
        // to whatever the Server offers next (ADR-0075). Withdrawn, the name is not read at all,
        // so an empty one there is simply unused.
        if config.self_update.enabled && config.self_update.package.trim().is_empty() {
            return Err(format!(
                "{}: [self_update].package is empty — it is the whole of what the consent is \
                 narrowed to. Name the package that carries this Client, or write \
                 `enabled = false` to withdraw the consent.",
                path.display()
            ));
        }
        if let Some(gateway) = &config.gateway {
            gateway
                .check(&config.endpoint)
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
        // A ceiling of zero would refuse every artifact; like the message limit it is a bound, not
        // a switch, so a value that cannot carry a download fails startup rather than silently
        // rejecting every package.
        if config.max_artifact_size_bytes == 0 {
            return Err(format!(
                "{}: max_artifact_size_bytes must be greater than zero",
                path.display()
            ));
        }
        // The retention bound is not optional (ADR-0041). Elsewhere a zero often means "no limit";
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

    /// The Ed25519 public key package signatures are verified against (ADR-0015), or `None`.
    pub fn package_key(&self) -> Option<&[u8]> {
        self.package_key.as_deref()
    }

    /// The client certificate and key this Client presents on both transports (ADR-0035), or
    /// `None` when it has no identity to present.
    ///
    /// A pair the Server issued outranks the configured one, the same precedence persisted
    /// connection settings have over `supervisor.toml` (ADR-0014): the file stays what the operator
    /// wrote, and deleting the stored pair reverts to it. That is also what retires a bootstrap
    /// certificate — it keeps standing in `supervisor.toml`, unused, once a real one has been issued.
    pub fn client_identity(&self) -> Option<(PathBuf, PathBuf)> {
        let cert = self.state_dir.join(crate::tls::ISSUED_CERT_FILE);
        let key = self.state_dir.join(crate::tls::ISSUED_KEY_FILE);
        if cert.exists() && key.exists() {
            return Some((cert, key));
        }
        let tls = self.tls.as_ref()?;
        Some((tls.cert_file.clone()?, tls.key_file.clone()?))
    }

    /// The CA bundle that replaces the built-in roots, when one is configured (ADR-0007).
    pub fn ca_file(&self) -> Option<&Path> {
        self.tls.as_ref()?.ca_file.as_deref()
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

    /// Where the artifact offered to an Agent is staged, by the name of the Supervisor behind it —
    /// `None` for the Client's own Agent. Inside that Supervisor's own directory, so that the
    /// install which follows is a rename within one filesystem instead of a copy across two
    /// (ADR-0021); the Client's own Agent stages under `state_dir`, beside the versions a
    /// self-update writes (ADR-0020). Keyed by name rather than by Engine index because the Agent
    /// set can change at runtime (ADR-0056), which is exactly when an index stops naming a block.
    #[must_use]
    pub fn staging_dir_for(&self, supervisor: Option<&str>) -> PathBuf {
        match supervisor {
            Some(name) => self.supervisor_dir(name).join(PACKAGES_DIR),
            None => self.state_dir.join(PACKAGES_DIR),
        }
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

    /// The Baseline asks for `service.namespace` "if it is used in the environment where the Agent
    /// runs" — so the file is the only thing that can know, and silence means the deployment does
    /// not use one. It must be a top-level key rather than an `[attributes]` entry, because it
    /// identifies the Agent where those merely tag it.
    #[test]
    fn the_service_namespace_is_a_top_level_key_and_absent_by_default() {
        assert!(ClientConfig::default().service_namespace.is_none());
        let untouched: ClientConfig =
            toml::from_str("endpoint = \"ws://h/v1/opamp\"").expect("parse");
        assert!(untouched.service_namespace.is_none());

        let configured: ClientConfig =
            toml::from_str("service_namespace = \"telemetry\"\n").expect("parse");
        assert_eq!(configured.service_namespace.as_deref(), Some("telemetry"));

        assert!(
            toml::from_str::<ClientConfig>("service_namesapce = \"telemetry\"\n").is_err(),
            "a typo fails startup rather than silently reporting no namespace"
        );
    }

    /// ADR-0041. The log is on by default with a bound that cannot be removed, and `[logging]` is
    /// the machine's — so a typo in it fails startup rather than quietly disabling the one thing
    /// that would have explained the next failure.
    #[test]
    fn the_log_file_is_on_by_default_and_its_retention_is_not_optional() {
        let defaults = ClientConfig::default().logging;
        assert!(defaults.enabled);
        assert_eq!(defaults.keep, 7);
        assert!(defaults.dir.is_none(), "the state directory decides");

        let configured: ClientConfig =
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
        let path = dir.path().join("supervisor.toml");
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

    /// ADR-0080: the rename must not turn a managed host into a silent one. A missing
    /// configuration is ordinarily the defaults and a warning; a missing one with the *old* name
    /// beside it is an upgraded host that would otherwise come up on the development endpoint and
    /// manage nothing, which is the failure nobody sees.
    #[test]
    fn the_configurations_old_name_beside_the_new_one_is_refused_rather_than_defaulted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expected = dir.path().join(crate::config_init::FILE_NAME);

        // Neither file: an ordinary fresh host, and the defaults are the answer (ADR-0027).
        assert!(ClientConfig::load(&expected).is_ok());

        std::fs::write(
            dir.path().join(LEGACY_CONFIG_FILE_NAME),
            "endpoint = \"ws://h/v1/opamp\"\n",
        )
        .expect("write the file this host was configured with");
        let error = ClientConfig::load(&expected).expect_err("an upgraded host must not go quiet");
        assert!(
            error.contains(LEGACY_CONFIG_FILE_NAME)
                && error.contains(crate::config_init::FILE_NAME),
            "the refusal names both files: {error}"
        );

        // And once renamed, it is an ordinary configuration again.
        std::fs::rename(dir.path().join(LEGACY_CONFIG_FILE_NAME), &expected).expect("rename");
        assert_eq!(
            ClientConfig::load(&expected).expect("loads").endpoint,
            "ws://h/v1/opamp"
        );
    }

    /// ADR-0075: the consent stands unless the file withdraws it, and it is narrowed to a name
    /// either way — the Client's own Agent type when the file names none, which since ADR-0077 is
    /// `supervisor`. A withdrawal is a written `enabled = false`, so a Client the fleet cannot
    /// update says so in its own configuration instead of saying nothing at all.
    #[test]
    fn self_update_consent_stands_by_default_and_is_narrowed_to_a_package_name() {
        let default = ClientConfig::default();
        assert_eq!(
            default.self_update_package(),
            Some(crate::supervisor::agent::CLIENT_SERVICE_NAME),
            "a Client with nothing configured consents under its own Agent type"
        );

        // A file that never mentions the section is the common case, and it is consent.
        let untouched: ClientConfig =
            toml::from_str("endpoint = \"ws://h/v1/opamp\"").expect("parse");
        assert_eq!(
            untouched.self_update_package(),
            Some(crate::supervisor::agent::CLIENT_SERVICE_NAME)
        );

        // And that name is `supervisor` since ADR-0077 — pinned here because the default travels
        // into every written configuration and has to line up with the Set the Server publishes.
        assert_eq!(untouched.self_update_package(), Some("supervisor"));

        // A name of its own is honoured, and it is the *only* name an offer may carry.
        let named: ClientConfig =
            toml::from_str("[self_update]\npackage = \"opamp-client\"\n").expect("parse");
        assert_eq!(named.self_update_package(), Some("opamp-client"));

        // The withdrawal, which is what an absent section used to mean.
        let withdrawn: ClientConfig =
            toml::from_str("[self_update]\nenabled = false\n").expect("parse");
        assert_eq!(withdrawn.self_update_package(), None);

        // Withdrawn *and* named parses, and stays withdrawn: the switch wins over the name, which
        // is why no caller reads them apart (`self_update_package` is the only reader).
        let both: ClientConfig =
            toml::from_str("[self_update]\nenabled = false\npackage = \"x\"\n").expect("parse");
        assert_eq!(both.self_update_package(), None);

        // An empty section is now legal — it is the default spelled out — and a typo still is not.
        assert!(toml::from_str::<ClientConfig>("[self_update]\n").is_ok());
        assert!(
            toml::from_str::<ClientConfig>("[self_update]\npackge = \"x\"\n").is_err(),
            "a typo fails startup rather than silently changing what the consent covers"
        );
    }

    /// The name is the whole of the narrowing (ADR-0075), so an empty one with the consent standing
    /// is refused at load rather than left to widen the consent to every package the Server offers.
    /// Withdrawn, the name is not read at all and an empty one is simply unused.
    #[test]
    fn an_empty_self_update_package_is_refused_while_the_consent_stands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("supervisor.toml");

        std::fs::write(&path, "[self_update]\npackage = \"\"\n").expect("write");
        let err = ClientConfig::load(&path).expect_err("an empty name is not a narrowing");
        assert!(err.contains("[self_update].package is empty"), "{err}");

        std::fs::write(&path, "[self_update]\nenabled = false\npackage = \"\"\n").expect("write");
        assert!(
            ClientConfig::load(&path).is_ok(),
            "withdrawn, the name is never read"
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
        let path = dir.path().join("supervisor.toml");
        std::fs::write(&path, "max_message_size_bytes = 0\n").expect("write");
        let err = ClientConfig::load(&path).expect_err("zero must fail startup");
        assert!(err.contains("max_message_size_bytes"), "{err}");
    }

    /// The artifact download has a ceiling so a Server cannot fill the staging disk before the hash
    /// is checked; it defaults to the Server's own per-package limit, is configurable, and zero is
    /// a bound that could carry nothing rather than "unlimited", so it fails startup.
    #[test]
    fn the_artifact_size_limit_defaults_is_configurable_and_rejects_zero() {
        assert_eq!(ClientConfig::default().max_artifact_size_bytes, 1 << 30);
        let tightened: ClientConfig =
            toml::from_str("max_artifact_size_bytes = 1048576").expect("parse");
        assert_eq!(tightened.max_artifact_size_bytes, 1_048_576);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("supervisor.toml");
        std::fs::write(&path, "max_artifact_size_bytes = 0\n").expect("write");
        let err = ClientConfig::load(&path).expect_err("zero must fail startup");
        assert!(err.contains("max_artifact_size_bytes"), "{err}");
    }

    /// A single downstream connection's Agent cap bounds the routing state one peer can create; it
    /// has a generous default, and zero is a bound that could carry nothing rather than "unlimited",
    /// so it fails startup.
    #[test]
    fn the_gateway_agent_cap_defaults_and_rejects_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("supervisor.toml");

        std::fs::write(
            &path,
            "endpoint = \"ws://s/v1/opamp\"\n[gateway]\nlisten = \"127.0.0.1:9\"\n",
        )
        .expect("write");
        let config = ClientConfig::load(&path).expect("loads with the default cap");
        assert_eq!(config.gateway.expect("gateway").max_carried_agents, 10_000);

        std::fs::write(
            &path,
            "endpoint = \"ws://s/v1/opamp\"\n[gateway]\nlisten = \"127.0.0.1:9\"\n\
             max_carried_agents = 0\n",
        )
        .expect("write");
        let err = ClientConfig::load(&path).expect_err("zero must fail startup");
        assert!(err.contains("max_carried_agents"), "{err}");
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
            None,
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
            moved.staging_dir_for(None),
            PathBuf::from("/var/lib/fleet/state/packages")
        );
        assert_eq!(
            moved.staging_dir_for(Some("agent")),
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

    /// ADR-0058: retention defaults to a day, is set globally by `[updates]`, and a `[[supervisor]]`
    /// block overrides it for itself — the shape `apply_grace_secs` has.
    #[test]
    fn retention_defaults_globally_and_is_overridable_per_supervisor() {
        let default: ClientConfig = toml::from_str("").expect("parse");
        assert_eq!(
            default.updates.retain_previous_secs,
            24 * 60 * 60,
            "one day by default"
        );

        let cfg: ClientConfig = toml::from_str(
            r#"
            [updates]
            retain_previous_secs = 3600

            [[supervisor]]
            type = "command"
            name = "keeps-default"
            command = "agent"

            [[supervisor]]
            type = "command"
            name = "overrides"
            command = "agent"
            retain_previous_secs = 0
            "#,
        )
        .expect("parse");
        assert_eq!(
            cfg.updates.retain_previous_secs, 3600,
            "the global override"
        );
        assert_eq!(
            cfg.supervisors[0].retain_previous_secs, None,
            "a block that says nothing takes the global"
        );
        assert_eq!(
            cfg.supervisors[1].retain_previous_secs,
            Some(0),
            "a block may override to immediate deletion"
        );

        let negative = toml::from_str::<ClientConfig>(
            "[[supervisor]]\ntype = \"command\"\nname = \"x\"\ncommand = \"a\"\nretain_previous_secs = -1\n",
        );
        assert!(negative
            .unwrap_err()
            .to_string()
            .contains("retain_previous_secs"));
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

    /// The Agent type is a common key like `name`, and — unlike `name` — is not bound by the
    /// ADR-0010 instance grammar, because the Baseline asks for a reverse FQDN and that grammar
    /// forbids the dots (ADR-0033).
    #[test]
    fn a_block_may_state_its_agent_type_and_a_reverse_fqdn_is_accepted() {
        let cfg: ClientConfig = toml::from_str(
            r#"
            [[supervisor]]
            type = "collector"
            name = "otelcol-edge-01"
            binary = "otelcol"
            service_name = "io.opentelemetry.collector"
            "#,
        )
        .expect("parse");
        let block = &cfg.supervisors[0];
        assert_eq!(block.name, "otelcol-edge-01");
        assert_eq!(
            block.service_name.as_deref(),
            Some("io.opentelemetry.collector")
        );
        // A common key, never handed to the plugin's strict parse.
        assert!(!block.settings.contains_key("service_name"));
    }

    /// Absent is the documented way to fall back to the program's file name. An empty string is
    /// not the same thing — it would report "no type" as though it were one, which a Selector
    /// could then match.
    #[test]
    fn an_empty_agent_type_is_refused_rather_than_treated_as_absent() {
        let err = toml::from_str::<ClientConfig>(
            r#"
            [[supervisor]]
            type = "collector"
            name = "otelcol"
            binary = "otelcol"
            service_name = "  "
            "#,
        )
        .expect_err("empty service_name must be refused");
        assert!(
            err.to_string().contains("`service_name` must not be empty"),
            "unhelpful error: {err}"
        );

        let absent: ClientConfig = toml::from_str(
            "[[supervisor]]\ntype = \"collector\"\nname = \"otelcol\"\nbinary = \"otelcol\"\n",
        )
        .expect("parse");
        assert_eq!(absent.supervisors[0].service_name, None);
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

    /// The file's text is reported off the host as the effective configuration, and the Server
    /// persists what it receives — so a credential value must never survive the redaction, while
    /// the operator's comments and layout must (they are half of what the file says).
    #[test]
    fn redaction_masks_credential_values_and_keeps_everything_else() {
        let text = "# the fleet endpoint\nendpoint = \"wss://fleet:4320/v1/opamp\"\n\n\
                    [auth]\n  bearer_token = \"s3cret\"\n\
                    [packages]\narchive_key = \"p4ss\"\nverification_key = \"aabb\"\n";
        let redacted = redact_secrets(text);
        assert!(!redacted.contains("s3cret"), "{redacted}");
        assert!(!redacted.contains("p4ss"), "{redacted}");
        assert!(redacted.contains("  bearer_token = \"***\""), "{redacted}");
        assert!(redacted.contains("archive_key = \"***\""), "{redacted}");
        assert!(
            redacted.contains("# the fleet endpoint"),
            "comments stay: {redacted}"
        );
        assert!(
            redacted.contains("endpoint = \"wss://fleet:4320/v1/opamp\""),
            "{redacted}"
        );
        assert!(
            redacted.contains("verification_key = \"aabb\""),
            "the public half of the signing pair is no secret: {redacted}"
        );

        // A spelling the line scan cannot take apart — an inline table — is masked whole:
        // over-redaction is the cheap failure, a leaked credential the expensive one.
        let inline = redact_secrets("auth = { username = \"op\", password = \"hunter2\" }\n");
        assert!(!inline.contains("hunter2"), "{inline}");
    }

    /// `load` is the single place the redaction happens, so everything downstream — the
    /// effective-configuration report above all — can only ever see the mask.
    #[test]
    fn load_stashes_the_source_already_redacted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(crate::config_init::FILE_NAME);
        std::fs::write(
            &path,
            "endpoint = \"ws://fleet:4320/v1/opamp\"\n[auth]\nbearer_token = \"s3cret\"\n",
        )
        .expect("write");
        let cfg = ClientConfig::load(&path).expect("loads");
        let source = cfg.source.expect("the file's text is kept");
        assert!(!source.contains("s3cret"), "{source}");
        assert!(source.contains("endpoint = \"ws://fleet:4320/v1/opamp\""));

        // No file, no text: the defaults run and there is nothing truthful to report.
        assert!(ClientConfig::load(&dir.path().join("absent.toml"))
            .expect("defaults")
            .source
            .is_none());
    }
}
