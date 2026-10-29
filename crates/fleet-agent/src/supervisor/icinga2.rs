//! The `icinga2` plugin (ADR-draft-icinga-2): Icinga 2 in the Agent role, delivered as a package
//! and run out of this Supervisor's own directory.
//!
//! Icinga 2 relocates only if it is *told* to, on every invocation, and the arguments that tell it
//! are not operator choices: the account it may run under follows the Client's service account
//! (ADR-0037), the include directory follows the delivered tree, and the state directories follow
//! `supervisor_dir`. So the block states values — the parent, the node name, where state lives —
//! and this plugin assembles the command line. What supervision *means* is unchanged: the shared
//! [`Runner`] spawns, watches, swaps packages, gates health, and stops.
//!
//! Three properties are Icinga's own and are why this is a kind rather than a recipe:
//!
//! - **Nothing creates its directories.** Debian's packages leave that to a helper that needs the
//!   `nagios` user; a fleet-managed host has neither, so the plugin creates them.
//! - **The daemon runs a worker of its own**, so it is started in its own process group and the
//!   Runner's stop signals the group — otherwise a killed umbrella leaves the worker holding the
//!   data directory and port 5665.
//! - **A failed reload is silent from the outside**: Icinga aborts it and keeps running the old
//!   configuration, so every apply is validated with `daemon -C` before it reaches the Runner —
//!   otherwise the fleet would be told `APPLIED` for a configuration that never took effect.
//!
//! Enrolment (ADR-draft-icinga-2) rides in the adapter beside the Runner: the daemon stays
//! unstarted until the Icinga parent has signed this node's certificate, and an unreachable parent
//! is a wait with a reason rather than a crash loop.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;

use crate::supervisor::ports::{
    parse_settings, EventSender, Plugin, ProcessCommand, ProcessEvent, SupervisorContext,
};
use crate::supervisor::process::{
    sighup, unhealthy, Preflight, ProcessSpec, Runner, Spawner, VersionProbe,
    AWAITING_CONFIGURATION, STAGED,
};
use opamp::client::Backoff;

/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per ADR-0008.
///
/// `binary` is not among them: the core takes it out and resolves it (ADR-0017), and what arrives
/// here is [`SupervisorContext::program`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Icinga2Settings {
    /// This node's common name: its `NodeName`, the CN of its certificate, and its Endpoint name
    /// — Icinga requires the three to be the same string, and the master's ticket was minted for
    /// exactly it. Not derivable for that reason: nothing on this side can know what was typed on
    /// the other (ADR-draft-icinga-2).
    node_name: Option<String>,
    /// The parent (master or satellite) this Agent enrols with and connects to, as `host` or
    /// `host:port` — the port defaults to Icinga's 5665. Absent means a standalone node: no
    /// enrolment, no certificate to fetch, only local checks.
    parent_host: Option<String>,
    /// The file holding this host's enrolment ticket — a Configuration delivered with
    /// `role = "supplementary"` through a Deployment that claims this one Agent (ADR-draft-icinga-2
    /// clause 11). Absent means the signing request waits for `icinga2 ca sign` on the parent.
    ticket_file: Option<String>,
    /// The parent's certificate — **its own**, not the CA that signed it: `pki request` compares
    /// what the parent presents against this file. Pinned rather than trusted on sight. Not named
    /// at all falls back to `pki save-cert`, which is trust on first use and is logged as such; a
    /// file named here that is not there is an error the enrolment retries, never that fallback.
    trusted_cert_file: Option<String>,
}

/// The keys this kind supplies itself (ADR-draft-icinga-2), each with what answers it. Refused by
/// name rather than met with serde's "unknown field": a block that carries one expects a Client
/// that needs it, and the operator deleting the line deserves to be told where the value went — the
/// pattern `package` and `accepts_packages` already run.
const RETIRED: &[(&str, &str)] = &[
    ("binary", "the kind installs and names its own program"),
    (
        "program_path",
        "the kind knows where its program sits in the tree it delivers",
    ),
    ("service_name", "the kind states the Agent type it presents"),
    ("include_dir", "the ITL is where the delivered tree puts it"),
    (
        "plugin_dir",
        "the check plugins are where the delivered tree puts them",
    ),
    (
        "data_dir",
        "state lives beside the tree, in this Supervisor's own directory",
    ),
    ("log_dir", "as data_dir"),
    ("cache_dir", "as data_dir"),
    ("spool_dir", "as data_dir"),
    ("run_dir", "as data_dir"),
    (
        "log_level",
        "logging belongs in Icinga's own configuration, where `object FileLogger` carries a \
         `severity` the fleet can roll out",
    ),
    (
        "renew_before_days",
        "a certificate is renewed 30 days before it expires",
    ),
    ("parent_port", "write it into `parent_host` as `host:port`"),
    (
        "run_as_user",
        "the daemon runs under the account this Client runs as — a Managed Process the fleet \
         installed has no business under another",
    ),
    ("run_as_group", "as run_as_user"),
    ("args", "the kind builds the daemon's arguments whole"),
    (
        "main_config",
        "the fleet marks its root Configuration with `role = \"main\"`, and where it marks none \
         the conventional name `icinga2-conf` stands in",
    ),
    (
        "env",
        "the kind sets what the delivered tree needs, LD_LIBRARY_PATH included",
    ),
];

/// Everything the daemon needs on its command line, resolved once at start.
///
/// A struct rather than nine arguments, because it is what the tests assert against: the arguments
/// are derived, so the derivation is the thing worth checking.
#[derive(Debug, Clone)]
pub struct Layout {
    program: PathBuf,
    /// What the delivered tree needs in its environment to run at all — on Unix its own libraries
    /// ([`tree_environment`]). Every invocation carries it, the daemon's and each subcommand's
    /// alike: a `pki` or `daemon -C` run without it fails on a host that lacks the bundled Boost.
    env: Vec<(String, String)>,
    /// Where the fleet's Configuration entries land; the root is resolved out of it on demand
    /// (ADR-draft-icinga-2), because which entry is the root can change with every rollout.
    config_dir: PathBuf,
    include_dir: PathBuf,
    plugin_dir: PathBuf,
    data_dir: PathBuf,
    log_dir: PathBuf,
    cache_dir: PathBuf,
    spool_dir: PathBuf,
    run_dir: PathBuf,
    node_name: String,
    run_as: Option<(String, String)>,
    parent: Option<(String, u16)>,
    /// Where the enrolment ticket is read from — a Configuration entry, delivered per host
    /// (ADR-draft-icinga-2). Absent means on-demand signing: the request waits in the parent's
    /// queue.
    ticket_file: Option<PathBuf>,
    /// The parent certificate to pin, as delivered. Copied out of `config/` at enrolment, because
    /// the next apply empties that directory.
    trusted_cert_file: Option<PathBuf>,
    /// Where the pinned parent certificate is kept, outside `config/` and outside the tree.
    pinned_parent: PathBuf,
    /// How close to expiry a certificate may come before it is renewed.
    renew_before: Duration,
    /// What was enrolled, so the ordinary start costs no subprocess. The certificate on disk is
    /// the state; this is a hint (ADR-draft-icinga-2).
    marker: PathBuf,
    /// The delivered configuration Icinga refused, by digest, with its reason: what is on disk
    /// after a refused apply is not started at the next spawn either (ADR-draft-icinga-2 clause 9).
    refused: PathBuf,
}

impl Layout {
    /// Where the enrolled certificate and key live — `DataDir/certs`, which is where the
    /// `ApiListener` looks for `<NodeName>.crt` without being told (ADR-draft-icinga-2).
    pub fn certs_dir(&self) -> PathBuf {
        self.data_dir.join("certs")
    }

    /// The certificate this node runs with, and the CA that signed it.
    fn certificate(&self) -> PathBuf {
        self.certs_dir().join(format!("{}.crt", self.node_name))
    }

    fn ca_certificate(&self) -> PathBuf {
        self.certs_dir().join("ca.crt")
    }

    /// The state directories, in the order they are created. Icinga creates none of them and fails
    /// on the first write into one that is missing.
    fn state_dirs(&self) -> Vec<PathBuf> {
        vec![
            self.data_dir.clone(),
            self.certs_dir(),
            self.log_dir.clone(),
            self.cache_dir.clone(),
            self.spool_dir.clone(),
            self.run_dir.clone(),
        ]
    }

    /// Creates what the daemon will write into, owner-only. Runs before every spawn, so a
    /// directory an operator removed under a running fleet comes back rather than taking the
    /// Supervisor down.
    fn prepare_dirs(&self) -> Result<(), String> {
        for dir in self.state_dirs() {
            crate::storage::create_private_dir(&dir)
                .map_err(|e| format!("cannot prepare {}: {e}", dir.display()))?;
        }
        Ok(())
    }

    /// Icinga's root configuration: the delivered entry the fleet marked, or the conventional
    /// name if it marked none.
    ///
    /// **The mark is a role, and a role is the kind's to define.** The Baseline says so of
    /// `AgentConfigFile.role`: *"The values and their semantics are Agent type-specific."* So
    /// `main` means *this* to `icinga2` and nothing to anyone else, which is the field working as
    /// intended rather than being borrowed. ADR-0025's own two values stay what they are for kinds
    /// that define nothing further.
    ///
    /// Resolved on every spawn rather than once at start: which entry is the root is the fleet's
    /// answer, and a rollout may change it while this Supervisor runs.
    ///
    /// The fallback is the name `opamp-fleetctl package fetch` uploads. It exists so a rollout
    /// written before this Client keeps working — not as the rule, which is why a fleet naming its
    /// root otherwise only has to mark it.
    ///
    /// # Errors
    /// Returns the reason not to start: nothing delivered yet, or two entries claiming to be the
    /// root, naming them.
    fn root_config(&self) -> Result<PathBuf, String> {
        let roles = crate::storage::entry_roles(&self.config_dir);
        let marked: Vec<&String> = roles
            .iter()
            .filter(|(_, role)| role.as_str() == ROOT_ROLE)
            .map(|(name, _)| name)
            .collect();
        match marked.as_slice() {
            [one] => return Ok(self.config_dir.join(one)),
            [] => {}
            _ => {
                let names: Vec<&str> = marked.iter().map(|name| name.as_str()).collect();
                return Err(format!(
                    "two delivered Configurations claim to be Icinga's root: {} both carry \
                     `role = \"{ROOT_ROLE}\"`, and the daemon reads one",
                    names.join(" and ")
                ));
            }
        }
        let conventional = self.config_dir.join(CONVENTIONAL_ROOT);
        if conventional.is_file() {
            return Ok(conventional);
        }
        Err(format!(
            "awaiting Icinga's root configuration in {}: no entry carries `role = \"{ROOT_ROLE}\"` \
             and none is named {CONVENTIONAL_ROOT}",
            self.config_dir.display()
        ))
    }

    /// The root to start the daemon with, or why it is not started: the root configuration has
    /// not arrived or is ambiguous, it is the one Icinga refused, or the certificate is missing.
    /// What `build()` asks before every spawn, and what the health says while it waits.
    fn ready(&self) -> Result<PathBuf, String> {
        let root = self.root_config()?;
        if let Some(reason) = self.refusal() {
            return Err(reason);
        }
        self.blocked_by().map_or(Ok(root), Err)
    }

    /// A digest of the delivered configuration — every entry file's name and content — so a
    /// refusal can be recognised as long as exactly that configuration is what is on disk.
    fn config_digest(&self) -> Option<String> {
        use sha2::Digest as _;
        let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(&self.config_dir)
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.file_name())
            .collect();
        names.sort();
        let mut digest = sha2::Sha256::new();
        for name in names {
            let body = std::fs::read(self.config_dir.join(&name)).ok()?;
            digest.update(name.as_encoded_bytes());
            digest.update([0]);
            digest.update((body.len() as u64).to_le_bytes());
            digest.update(&body);
        }
        Some(hex::encode(digest.finalize()))
    }

    /// Records that Icinga refused the configuration on disk, and why.
    fn record_refusal(&self, reason: &str) {
        let record = serde_json::json!({ "digest": self.config_digest(), "reason": reason });
        if let Err(e) = std::fs::write(&self.refused, record.to_string()) {
            tracing::warn!(path = %self.refused.display(), error = %e, "cannot record the refused configuration");
        }
    }

    /// Forgets a refusal once a configuration validated.
    fn clear_refusal(&self) {
        let _ = std::fs::remove_file(&self.refused);
    }

    /// Why the configuration on disk is not started: it is the one Icinga refused. A delivery that
    /// changed anything in it ends the refusal; the apply that brings it validates it.
    fn refusal(&self) -> Option<String> {
        let text = std::fs::read_to_string(&self.refused).ok()?;
        let record: serde_json::Value = serde_json::from_str(&text).ok()?;
        let digest = record.get("digest")?.as_str()?;
        (Some(digest) == self.config_digest().as_deref()).then(|| {
            format!(
                "the delivered configuration is the one Icinga refused: {}",
                record
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("no reason recorded")
            )
        })
    }

    /// Why the daemon is not started yet once its root configuration is there, or `None` when it
    /// may run.
    ///
    /// Two gates, and both are honest waits rather than failures: the root configuration has to
    /// have arrived ([`root_config`](Self::root_config)), and — with a parent configured (the Agent
    /// role) — the certificate has to have been issued. Starting without either produces a process
    /// that cannot do its job and a crash loop that says nothing about why.
    fn blocked_by(&self) -> Option<String> {
        if self.parent.is_some()
            && !(self.certificate().is_file() && self.ca_certificate().is_file())
        {
            return Some(format!(
                "awaiting the certificate for {} from the Icinga parent",
                self.node_name
            ));
        }
        None
    }

    /// The ticket this host enrols with, read from the file the fleet delivered it in. No
    /// `ticket_file` means on-demand signing (ADR-draft-icinga-2); a named one that is not there
    /// yet, or holds nothing, is a wait — sending the request without it would queue a request on
    /// the parent that nobody meant to sign by hand.
    fn ticket(&self) -> Result<Option<String>, String> {
        let Some(path) = self.ticket_file.as_ref() else {
            return Ok(None);
        };
        let ticket = match std::fs::read_to_string(path) {
            Ok(ticket) => ticket.trim().to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("cannot read the ticket at {}: {e}", path.display())),
        };
        if ticket.is_empty() {
            return Err(format!(
                "no ticket in the ticket_file at {} yet: enrolment waits for it rather than \
                 queueing a request on the parent",
                path.display()
            ));
        }
        Ok(Some(ticket))
    }

    /// Whether the marker says this is the enrolment we want — same node, same parent. It answers
    /// *"has anything changed?"*, never *"is there a certificate?"*, which is what the file on disk
    /// answers (ADR-draft-icinga-2).
    fn enrolled_for(&self, want: &Enrolment) -> bool {
        std::fs::read_to_string(&self.marker)
            .ok()
            .and_then(|text| serde_json::from_str::<Enrolment>(&text).ok())
            .is_some_and(|have| {
                have.common_name == want.common_name
                    && have.parent_host == want.parent_host
                    && have.parent_port == want.parent_port
            })
    }

    fn record_enrolment(&self, what: &Enrolment) -> Result<(), String> {
        let record = Enrolment {
            common_name: what.common_name.clone(),
            parent_host: what.parent_host.clone(),
            parent_port: what.parent_port,
            enrolled_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
        };
        let text = serde_json::to_string_pretty(&record)
            .map_err(|e| format!("cannot record the enrolment: {e}"))?;
        std::fs::write(&self.marker, text)
            .map_err(|e| format!("cannot write {}: {e}", self.marker.display()))
    }

    /// Validates a configuration exactly as the daemon would, without touching what runs.
    ///
    /// This is the gate ADR-draft-icinga-2 requires: Icinga aborts a reload it cannot validate and
    /// **keeps running the old configuration**, printing the reason to stderr — so a Supervisor
    /// that forwarded the apply blindly would report `APPLIED` for a configuration that never took
    /// effect.
    async fn validate(&self) -> Result<(), String> {
        let mut args = self.daemon_args(&self.root_config()?);
        // `-C` next to `daemon`, before the rest, so the same command line is exercised.
        args.insert(1, "-C".to_string());
        run_subcommand(self, &args).await.map(|_| ())
    }

    /// The daemon's argument vector. Foreground — no `-d`, no `--close-stdio` — because the Runner
    /// supervises what it started and the Client's logging carries the output (ADR-0019).
    fn daemon_args(&self, root: &std::path::Path) -> Vec<String> {
        let mut args = vec![
            "daemon".to_string(),
            "-c".to_string(),
            root.display().to_string(),
        ];
        let mut define = |key: &str, value: String| {
            args.push("-D".to_string());
            args.push(format!("{key}={value}"));
        };
        if let Some((user, group)) = &self.run_as {
            define("RunAsUser", user.clone());
            define("RunAsGroup", group.clone());
        }
        define("NodeName", self.node_name.clone());
        define("IncludeConfDir", self.include_dir.display().to_string());
        define("PluginDir", self.plugin_dir.display().to_string());
        define("DataDir", self.data_dir.display().to_string());
        define("LogDir", self.log_dir.display().to_string());
        define("CacheDir", self.cache_dir.display().to_string());
        define("SpoolDir", self.spool_dir.display().to_string());
        define("InitRunDir", self.run_dir.display().to_string());
        args.push("-x".to_string());
        args.push(DEFAULT_LOG_LEVEL.to_string());
        args
    }
}

/// How long any one `icinga2` subcommand may take before it is given up on. Enrolment talks to a
/// parent over the network, and a parent that accepts the connection but never answers must not
/// wedge the adapter — the version probe bounds itself the same way.
const SUBCOMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs one `icinga2` subcommand, bounded, with the constants and the environment every
/// invocation needs.
///
/// `-D RunAsUser`/`-D RunAsGroup` are not a daemon concern: **every** subcommand drops privileges
/// to the compiled-in account first and refuses when it cannot, so `pki` needs them exactly as the
/// daemon does (ADR-draft-icinga-2). The tree's environment is the same: the program links the
/// libraries the tree carries, whichever subcommand it runs.
async fn run_subcommand(layout: &Layout, args: &[String]) -> Result<String, String> {
    let mut command = tokio::process::Command::new(&layout.program);
    command.args(args);
    command.envs(layout.env.iter().map(|(key, value)| (key, value)));
    if let Some((user, group)) = &layout.run_as {
        command.args(["-D".to_string(), format!("RunAsUser={user}")]);
        command.args(["-D".to_string(), format!("RunAsGroup={group}")]);
    }
    command.kill_on_drop(true);
    let output = match tokio::time::timeout(SUBCOMMAND_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("cannot run icinga2 {}: {e}", shown(args))),
        Err(_) => return Err(format!("icinga2 {} timed out", shown(args))),
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        return Ok(text);
    }
    // The last line Icinga printed says more than the exit status: "Cannot connect to host",
    // "Ticket is invalid", "Certificate has expired". That is what the fleet view should carry.
    let reason = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_string();
    let reason = secrets(args).fold(reason, |text, secret| text.replace(secret, REDACTED));
    Err(format!("icinga2 {} failed: {reason}", shown(args)))
}

/// What stands where a secret was: the enrolment ticket never reaches a log line, the health, or
/// the fleet view, which carry what a subcommand failed with.
const REDACTED: &str = "<redacted>";

/// The values on a command line that are secrets: what follows `--ticket`.
fn secrets(args: &[String]) -> impl Iterator<Item = &str> {
    args.windows(2)
        .filter(|pair| pair[0] == "--ticket" && !pair[1].is_empty())
        .map(|pair| pair[1].as_str())
}

/// A subcommand's arguments as a message may show them, the ticket replaced.
fn shown(args: &[String]) -> String {
    let mut shown: Vec<&str> = Vec::with_capacity(args.len());
    for (i, arg) in args.iter().enumerate() {
        let secret = i > 0 && args[i - 1] == "--ticket";
        shown.push(if secret { REDACTED } else { arg });
    }
    shown.join(" ")
}

/// The expiry `icinga2 pki verify` printed, as `Valid Until: Aug 13 16:20:17 2041 GMT`.
///
/// Read rather than assumed, because `pki verify` answers a *different* question than "is this
/// still valid": measured against a real Icinga master, it checks the signature and exits `0` for a
/// certificate whose validity has run out. So the renewal window is decided from this line, and a
/// missing or unreadable one means "cannot tell" — which is treated as due for renewal rather than
/// as fine.
fn valid_until(text: &str) -> Option<time::OffsetDateTime> {
    let format = time::macros::format_description!(
        "[month repr:short] [day padding:none] [hour]:[minute]:[second] [year]"
    );
    let line = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Valid Until:"))?;
    // OpenSSL pads a single-digit day with a second space; the trailing zone is always GMT.
    let normalised = line
        .split_whitespace()
        .filter(|word| *word != "GMT")
        .collect::<Vec<_>>()
        .join(" ");
    time::PrimitiveDateTime::parse(&normalised, &format)
        .ok()
        .map(|stamp| stamp.assume_utc())
}

/// What the enrolment marker records — a hint, never the authority (ADR-draft-icinga-2).
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct Enrolment {
    common_name: String,
    parent_host: String,
    parent_port: u16,
    enrolled_at_unix: u64,
}

/// Obtains a certificate for this node from its Icinga parent, once.
///
/// The order matters and is ADR-draft-icinga-2's: a usable certificate ends it before anything
/// runs; the key is generated locally and never travels; the parent is pinned before it is talked
/// to; and the ticket — when there is one — turns the request into an immediate signature instead
/// of a queue entry. `Ok(false)` means there was nothing to do.
///
/// A request without a ticket that the parent queued is a wait, not a success: Icinga answers it
/// with *"is pending"* and exit status 0, and the certificate on disk is still the self-signed one
/// `new-cert` made. The parent files the request under that certificate's fingerprint, so the next
/// attempt asks again with the **same** key and certificate — which is how the signed certificate
/// is fetched once `icinga2 ca sign` ran — and makes no new key. `ca.crt` is written only beside a
/// certificate the parent signed, so the gate of [`Layout::blocked_by`] stays shut meanwhile.
async fn ensure_enrolled(layout: &Layout) -> Result<bool, String> {
    let Some((host, port)) = layout.parent.clone() else {
        return Ok(false);
    };
    layout.prepare_dirs()?;
    let (cert, key, ca) = (
        layout.certificate(),
        layout.certs_dir().join(format!("{}.key", layout.node_name)),
        layout.ca_certificate(),
    );
    let want = Enrolment {
        common_name: layout.node_name.clone(),
        parent_host: host.clone(),
        parent_port: port,
        enrolled_at_unix: 0,
    };
    // Whether this is a *renewal* — an existing certificate near its expiry — rather than a first
    // enrolment. The two differ in one place each: a renewal keeps its key, and it authenticates
    // itself with the certificate it already holds instead of with a ticket (ADR-draft-icinga-2).
    let mut renewing = false;
    if cert.is_file() && ca.is_file() && layout.enrolled_for(&want) {
        // The certificate is the state, so it is *verified* rather than assumed: one that does not
        // verify enrols again instead of starting a daemon that cannot connect.
        let verified = run_subcommand(
            layout,
            &[
                "pki".to_string(),
                "verify".to_string(),
                "--cert".to_string(),
                cert.display().to_string(),
                "--cacert".to_string(),
                ca.display().to_string(),
            ],
        )
        .await;
        match verified {
            Ok(output) => match valid_until(&output) {
                Some(expiry) if expiry - time::OffsetDateTime::now_utc() > layout.renew_before => {
                    return Ok(false)
                }
                Some(expiry) => {
                    tracing::info!(node = %layout.node_name, %expiry, "renewing the Icinga certificate before it expires");
                    renewing = true;
                }
                None => {
                    tracing::warn!(
                        "cannot read the certificate's expiry; renewing rather than guessing"
                    );
                    renewing = true;
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "the stored Icinga certificate does not verify; enrolling again")
            }
        }
    }

    // The parent is pinned before it is talked to, and before a key is made for it. A named file
    // that is missing — not yet arrived, or a path mistyped — is an error the enrolment retries:
    // ADR-draft-icinga-2 clause 12 trusts on first use only when none was delivered. That holds for
    // a renewal too: the pin kept from an earlier run may itself have come from trust on first use.
    if let Some(delivered) = layout.trusted_cert_file.as_ref().filter(|f| !f.is_file()) {
        return Err(format!(
            "the parent certificate named in trusted_cert_file is not a file at {}: enrolment \
             waits for it rather than trusting the parent on first use",
            delivered.display()
        ));
    }
    // The same for a named ticket: read before anything is made or asked. A renewal needs none —
    // the certificate in force authenticates it — so a ticket the fleet withdrew after the first
    // enrolment does not hold it up.
    let ticket = if renewing { None } else { layout.ticket()? };

    tracing::info!(node = %layout.node_name, parent = %host, renewing, "requesting an Icinga certificate");
    // A key and a certificate no CA on disk signed are a request in flight: one the parent queued,
    // or one an earlier attempt could not deliver. It is asked again as it is.
    let in_flight = !renewing && key.is_file() && cert.is_file() && !ca.is_file();
    if in_flight {
        tracing::info!(node = %layout.node_name, "asking again for the certificate requested earlier, with the same key");
    } else if !renewing {
        // A CA on disk stands only beside a certificate it signed: the one about to be replaced
        // goes with it, so the gate stays shut until the parent signed the new one.
        match std::fs::remove_file(&ca) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove {}: {e}", ca.display())),
        }
        // A renewal must not do this: it would overwrite the very key and certificate that
        // authenticate the renewal, leaving a request nothing can prove (ADR-draft-icinga-2).
        run_subcommand(
            layout,
            &[
                "pki".to_string(),
                "new-cert".to_string(),
                "--cn".to_string(),
                layout.node_name.clone(),
                "--key".to_string(),
                key.display().to_string(),
                "--cert".to_string(),
                cert.display().to_string(),
            ],
        )
        .await?;
    }

    // The parent is pinned from what the fleet delivered. Only when no certificate was named at all
    // is `save-cert` the fallback, and it is trust on first use — logged, so an operator can see
    // that it happened and against what. *First* use: a pin already held for this parent is kept,
    // so neither a renewal nor a retry trusts what the parent presents at that moment.
    let pinned_for = format!("{host}:{port}");
    let source = layout.pinned_parent.with_extension("source");
    match layout.trusted_cert_file.as_ref() {
        Some(delivered) => {
            std::fs::copy(delivered, &layout.pinned_parent).map_err(|e| {
                format!(
                    "cannot copy the parent certificate {} to {}: {e}",
                    delivered.display(),
                    layout.pinned_parent.display()
                )
            })?;
            write_pin_source(&source, &pinned_for)?;
        }
        None if layout.pinned_parent.is_file()
            && std::fs::read_to_string(&source).is_ok_and(|held| held == pinned_for) =>
        {
            tracing::info!(parent = %host, "keeping the parent certificate pinned earlier");
        }
        None => {
            tracing::warn!(
                parent = %host,
                "no parent certificate was delivered: trusting the one the parent presents now \
                 (trust on first use)"
            );
            run_subcommand(
                layout,
                &[
                    "pki".to_string(),
                    "save-cert".to_string(),
                    "--host".to_string(),
                    host.clone(),
                    "--port".to_string(),
                    port.to_string(),
                    "--trustedcert".to_string(),
                    layout.pinned_parent.display().to_string(),
                ],
            )
            .await?;
            write_pin_source(&source, &pinned_for)?;
        }
    }

    let requested_ca = ca.with_extension("crt.requested");
    let mut request = vec![
        "pki".to_string(),
        "request".to_string(),
        "--host".to_string(),
        host.clone(),
        "--port".to_string(),
        port.to_string(),
        "--key".to_string(),
        key.display().to_string(),
        "--cert".to_string(),
        cert.display().to_string(),
        // Not `ca.crt` itself: Icinga writes the CA for a queued request too, and `ca.crt` stands
        // only beside a signed certificate.
        "--ca".to_string(),
        requested_ca.display().to_string(),
        "--trustedcert".to_string(),
        layout.pinned_parent.display().to_string(),
    ];
    match ticket {
        Some(ticket) => request.extend(["--ticket".to_string(), ticket]),
        None if renewing => tracing::info!(
            node = %layout.node_name,
            "the certificate in force authenticates its own renewal; no ticket is used"
        ),
        None => tracing::info!(
            node = %layout.node_name,
            "no ticket: the signing request waits for `icinga2 ca sign` on the parent"
        ),
    }
    match run_subcommand(layout, &request).await {
        Ok(answer) if !answer.contains("is pending") => {}
        answer => {
            let _ = std::fs::remove_file(&requested_ca);
            answer?;
            return Err(format!(
                "the parent holds the signing request for {} until `icinga2 ca sign` runs there; \
                 asking again with the same key",
                layout.node_name
            ));
        }
    }
    std::fs::rename(&requested_ca, &ca)
        .map_err(|e| format!("cannot keep the CA as {}: {e}", ca.display()))?;
    layout.record_enrolment(&want)?;
    tracing::info!(node = %layout.node_name, parent = %host, "the Icinga parent signed this node's certificate");
    Ok(true)
}

/// Records which parent the pinned certificate is for, so a later trust on first use is not
/// repeated against the same parent.
fn write_pin_source(source: &std::path::Path, parent: &str) -> Result<(), String> {
    std::fs::write(source, parent).map_err(|e| format!("cannot write {}: {e}", source.display()))
}

/// Icinga's own version banner is `icinga2 … (version: r2.14.6-1)`: an `r`, and a packaging
/// revision the strict SemVer read rejects outright. Reported as `2.14.6`, which is the version an
/// operator compares and the one a package Set is named after (ADR-0033).
fn parse_version(text: &str) -> Option<String> {
    fn numeric(part: Option<&str>) -> bool {
        part.is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    }
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|token| {
            let mut parts = token.split('.');
            let three = [parts.next(), parts.next(), parts.next()];
            three.iter().all(|part| numeric(*part)) && parts.next().is_none()
        })
        // Written by the rule the Package is named by, so the two agree
        // (ADR-0028 clause 6).
        .and_then(fleet_core::version::written_as_semver)
}

/// The account this Client runs as, which is the account Icinga may drop to. Resolved with `id(1)`
/// — the idiom ADR-0037's account handling already uses, rather than `getpwuid(3)` behind `unsafe`
/// or a user-lookup dependency for two strings.
#[cfg(unix)]
fn current_account() -> Option<(String, String)> {
    let ask = |flag: &str| {
        let out = std::process::Command::new("id").arg(flag).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Some((ask("-un")?, ask("-gn")?))
}

/// Windows Icinga drops no privileges, so there is no account to name.
#[cfg(not(unix))]
fn current_account() -> Option<(String, String)> {
    None
}

/// The `icinga2` kind. Its [`Spawner`] is the one the registry hands every plugin (ADR-0037).
#[derive(Default)]
pub struct Icinga2Plugin {
    pub spawner: Spawner,
}

impl Icinga2Plugin {
    /// Resolves the block against this Supervisor's directories. Everything an operator may write
    /// goes through the placeholders of ADR-0017; the defaults are expressed in them too, so a
    /// relocated `supervisor_dir` moves the state with it.
    fn layout(ctx: &SupervisorContext, settings: &Icinga2Settings) -> Layout {
        let path = |below: &str| -> PathBuf { PathBuf::from(ctx.expand(below)) };
        // Inside the tree this kind delivers (ADR-draft-icinga-2): the ITL the root configuration
        // `include`s, and the check plugins. On Windows the checks stay beside the daemon in
        // `sbin`, because a Windows program finds its DLLs in its own directory first and the
        // checks share that runtime (ADR-draft-icinga-2).
        let tree = ctx
            .supervisor_dir
            .join(crate::config::PROGRAM_DIR)
            .join(crate::config::TREE_DIR);
        Layout {
            program: ctx.program.clone(),
            env: tree_environment(&tree.to_string_lossy()),
            config_dir: ctx.config_dir.clone(),
            include_dir: tree.join("share").join("icinga2").join("include"),
            plugin_dir: if cfg!(windows) {
                tree.join("sbin")
            } else {
                tree.join("plugins")
            },
            data_dir: path("${supervisor_dir}/data"),
            log_dir: path("${supervisor_dir}/log"),
            cache_dir: path("${supervisor_dir}/cache"),
            spool_dir: path("${supervisor_dir}/spool"),
            run_dir: path("${supervisor_dir}/run"),
            // The operator's, else this host's fully qualified name, else the Supervisor's own —
            // which the instance-name grammar cannot spell as an FQDN, so it is the last resort
            // rather than the default (ADR-draft-icinga-2).
            node_name: settings
                .node_name
                .clone()
                .or_else(|| resolved_fqdn().map(str::to_string))
                .unwrap_or_else(|| ctx.name.clone()),
            run_as: current_account(),
            parent: settings.parent_host.as_deref().map(parent_address),
            ticket_file: settings
                .ticket_file
                .as_deref()
                .map(|f| PathBuf::from(ctx.expand(f))),
            trusted_cert_file: settings
                .trusted_cert_file
                .as_deref()
                .map(|f| PathBuf::from(ctx.expand(f))),
            pinned_parent: ctx.supervisor_dir.join("trusted-parent.crt"),
            renew_before: Duration::from_secs(DEFAULT_RENEW_BEFORE_DAYS * 24 * 60 * 60),
            marker: ctx.supervisor_dir.join("icinga2-enrolment.json"),
            refused: ctx.supervisor_dir.join("icinga2-refused.json"),
        }
    }
}

/// What a delivered tree rooted at `tree` needs in its environment (ADR-draft-icinga-2): on Unix
/// its own libraries, so the bundled copies win over whatever the machine has. Windows needs none —
/// a program there finds its DLLs beside itself.
///
/// One rule for every invocation: the daemon, the version probe and each subcommand get it for the
/// installed tree, the preflight for the staged one (`${staged}`, which the Runner resolves).
fn tree_environment(tree: &str) -> Vec<(String, String)> {
    if cfg!(windows) {
        return Vec::new();
    }
    vec![("LD_LIBRARY_PATH".to_string(), format!("{tree}/lib"))]
}

/// The proof a staged package runs before it is installed (ADR-draft-icinga-2 clause 15):
/// `--version`, with the staged tree's own libraries — a tree built against a newer glibc than the
/// host's is refused with the dynamic linker's message before the running daemon is stopped.
fn preflight() -> Preflight {
    Preflight {
        args: vec!["--version".to_string()],
        env: tree_environment(STAGED),
        // The daemon starts where it inherits, so the check does too
        // (ADR-draft-supervisor-mode-and-its-kinds clause 56).
        working_dir: None,
    }
}

/// This host's fully qualified name, if it has one — the default for `node_name`
/// (ADR-draft-icinga-2).
///
/// **Why a resolution rather than the host name this Agent already reports.** That one is
/// `gethostname`, which the semantic conventions permit to be either form and which is the short
/// name on most Linux hosts. Icinga wants the FQDN: its own `NodeName` defaults to
/// `hostname --fqdn`, and an operator following Icinga's instructions mints the ticket with
/// `pki ticket --cn <fqdn>`. A default that is the short name would therefore be wrong in the way
/// that *fails enrolment* rather than the way that reads oddly, which is worse than no default.
///
/// **Only a name with a dot in it is taken.** What `getaddrinfo` returns for an unqualified host is
/// often the unqualified name back; accepting that would reintroduce the very default this exists
/// to replace. Without a dot there is no FQDN to be had here, and the Supervisor's name stands as
/// before — with `node_name` as the way to say what the master actually knows.
///
/// Resolved once per process and only when no `node_name` was configured, so a host whose resolver
/// is slow or unreachable pays for it once at most — and an operator who sets the key never at all.
fn resolved_fqdn() -> Option<&'static str> {
    static FQDN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FQDN.get_or_init(read_fqdn)
        .as_deref()
        .filter(|name| is_qualified_host_name(name))
}

/// Whether a resolved name may become `node_name`: a host name with a dot in it, and nothing else.
/// It names this host's certificate and key files and is the CN the parent signs, so what the
/// resolver answered is held to the grammar of a DNS host name — labels of letters, digits and
/// inner hyphens — rather than taken as it came.
fn is_qualified_host_name(name: &str) -> bool {
    let label = |label: &str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    name.len() <= 253 && name.contains('.') && name.split('.').all(label)
}

/// The canonical name the resolver gives this host — `hostname --fqdn` by another route, and the
/// same one it takes: `getaddrinfo` with `AI_CANONNAME`, which consults `/etc/hosts` before DNS
/// wherever nsswitch says so.
#[cfg(unix)]
fn read_fqdn() -> Option<String> {
    let host = std::ffi::CString::new(crate::host::host_name()?).ok()?;
    // SAFETY: `hints` is a zeroed `addrinfo` with only the fields below set, `host` outlives the
    // call, and `result` is written by the callee and freed exactly once below.
    unsafe {
        let mut hints: libc::addrinfo = std::mem::zeroed();
        hints.ai_flags = libc::AI_CANONNAME;
        hints.ai_family = libc::AF_UNSPEC;
        let mut result: *mut libc::addrinfo = std::ptr::null_mut();
        if libc::getaddrinfo(host.as_ptr(), std::ptr::null(), &hints, &mut result) != 0
            || result.is_null()
        {
            return None;
        }
        let canonical = (*result).ai_canonname;
        let name = (!canonical.is_null()).then(|| {
            std::ffi::CStr::from_ptr(canonical)
                .to_string_lossy()
                .into_owned()
        });
        libc::freeaddrinfo(result);
        name.filter(|name| !name.is_empty())
    }
}

/// Windows keeps the answer without a lookup, in the machine's own DNS name — but this kind is
/// unproven there (`docs/manual/icinga2.md`), so rather than reach for an API this crate does not
/// otherwise use, the default stays what it was and `node_name` says the rest.
#[cfg(not(unix))]
fn read_fqdn() -> Option<String> {
    None
}

/// The role a delivered Configuration carries to say it is Icinga's root (ADR-draft-icinga-2), and
/// the name that stands in where the fleet marked nothing — the one `opamp-fleetctl package fetch`
/// uploads.
const ROOT_ROLE: &str = "main";
const CONVENTIONAL_ROOT: &str = "icinga2-conf";

/// The console severity the daemon is started with (`-x`). Icinga's own default, and not a block
/// key: where verbosity is worth raising, `object FileLogger` in Icinga's own configuration is the
/// place, which the fleet rolls out (ADR-draft-icinga-2).
const DEFAULT_LOG_LEVEL: &str = "information";

/// Splits a parent into host and port, the port defaulting to Icinga's 5665 (ADR-draft-icinga-2).
///
/// One address is one value. A bare IPv6 address has colons of its own, so the split is taken from
/// the **last** one and only when what follows is a port — `::1` stays a host, `[::1]:5665` and
/// `master.example:5665` name a port.
fn parent_address(raw: &str) -> (String, u16) {
    // Bracketed, the one form that is unambiguous: `[::1]:5665`.
    if let Some(rest) = raw.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|port| port.parse().ok())
                .unwrap_or(DEFAULT_PARENT_PORT);
            return (host.to_string(), port);
        }
    }
    // Unbracketed, exactly one colon separates a host from a port. More than one is an IPv6
    // address written bare — it carries no port, and reading its last group as one would send this
    // Agent to `::` on port 1.
    if raw.matches(':').count() == 1 {
        if let Some((host, port)) = raw.split_once(':') {
            if let (false, Ok(port)) = (host.is_empty(), port.parse::<u16>()) {
                return (host.to_string(), port);
            }
        }
    }
    (raw.to_string(), DEFAULT_PARENT_PORT)
}

/// The daemon's file name and its place inside the delivered tree (ADR-draft-icinga-2), per
/// platform.
///
/// Windows carries the `.exe` and — the difference that is easy to miss — keeps the check plugins
/// beside the daemon in `sbin` rather than in `plugins/`, because a Windows program finds its DLLs
/// in its own directory first and the checks share that runtime (ADR-draft-icinga-2).
#[cfg(windows)]
const PROGRAM: &str = "icinga2.exe";
#[cfg(not(windows))]
const PROGRAM: &str = "icinga2";
#[cfg(windows)]
const PROGRAM_PATH: &str = "sbin/icinga2.exe";
#[cfg(not(windows))]
const PROGRAM_PATH: &str = "sbin/icinga2";

/// The port an Icinga master or satellite listens on for the cluster protocol.
const DEFAULT_PARENT_PORT: u16 = 5665;

/// How long before expiry a certificate is renewed by default. Icinga's own default validity is
/// years, and the daemon renews over its established connection; this is the start-time safety net
/// for the host that was switched off for longer than that (ADR-draft-icinga-2).
const DEFAULT_RENEW_BEFORE_DAYS: u64 = 30;

/// Enrols this node, retrying until it succeeds, and nudges the Runner when it does.
///
/// An unreachable parent is a *wait*, not a failure: the health says what is missing, the attempt
/// backs off, and no daemon is started — a crash loop would say nothing about why
/// (ADR-draft-icinga-2). The nudge is an ordinary `Restart`, because the gate this opens is the one
/// `build()` reads.
async fn enrol(layout: Layout, runner: mpsc::Sender<ProcessCommand>, events: EventSender) {
    let mut backoff = Backoff::new();
    loop {
        match ensure_enrolled(&layout).await {
            Ok(false) => return,
            Ok(true) => {
                // Something changed on disk that `build()` gates on, and only a command makes the
                // Runner look again.
                let _ = runner.send(ProcessCommand::Restart).await;
                return;
            }
            Err(e) => {
                tracing::warn!(node = %layout.node_name, error = %e, "cannot enrol with the Icinga parent yet");
                events
                    .send(ProcessEvent::Health(unhealthy(
                        format!("awaiting the certificate for {}", layout.node_name),
                        e,
                    )))
                    .await;
                tokio::time::sleep(backoff.advance()).await;
            }
        }
    }
}

/// Sits between the core and the [`Runner`], and validates a configuration before it is applied.
///
/// Everything else is forwarded untouched. A configuration that does not validate is answered
/// `ConfigApplied{Err}` **and swallowed**: the running daemon is not stopped, not reloaded, and not
/// left claiming to run something it refused (ADR-draft-icinga-2).
async fn intercept(
    mut from_core: mpsc::Receiver<ProcessCommand>,
    runner: mpsc::Sender<ProcessCommand>,
    layout: Layout,
    events: EventSender,
) {
    while let Some(command) = from_core.recv().await {
        let command = match command {
            ProcessCommand::ApplyConfig { config, span } => match layout.validate().await {
                Ok(()) => {
                    layout.clear_refusal();
                    ProcessCommand::ApplyConfig { config, span }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "refusing a configuration Icinga 2 will not accept");
                    // The files stay where the fleet wrote them; the next spawn must not start
                    // what was just refused.
                    layout.record_refusal(&e);
                    // The apply ends here rather than at the Runner, so this is where its trace
                    // learns why (ADR-0016).
                    crate::telemetry::failed(&span, &e);
                    events
                        .send(ProcessEvent::ConfigApplied {
                            hash: config.config_hash,
                            result: Err(e),
                        })
                        .await;
                    continue;
                }
            },
            other => other,
        };
        if runner.send(command).await.is_err() {
            return;
        }
    }
}

/// Forwards what the Runner reports, adding the gate's reason where it reports a wait without one.
///
/// The Runner knows only that `build()` yielded nothing, and says "awaiting configuration"; why —
/// no root yet, two roots, the configuration Icinga refused, the certificate — is the kind's to
/// say, and an operator reading the health needs it (ADR-draft-icinga-2 clause 8).
async fn explain_waits(
    mut from_runner: mpsc::Receiver<(usize, ProcessEvent)>,
    layout: Layout,
    events: EventSender,
) {
    while let Some((_, event)) = from_runner.recv().await {
        let event = match event {
            ProcessEvent::Health(health)
                if !health.healthy
                    && health.status == AWAITING_CONFIGURATION
                    && health.last_error.is_empty() =>
            {
                match layout.ready() {
                    Err(reason) => ProcessEvent::Health(unhealthy(health.status, reason)),
                    Ok(_) => ProcessEvent::Health(health),
                }
            }
            other => other,
        };
        events.send(event).await;
    }
}

/// `node_name` names the certificate and key files of this host, so it is one plain file-name
/// component and nothing that could climb out of the certificate directory (ADR-0017 clause 36).
fn check_node_name(name: &str, node_name: Option<&str>) -> Result<(), String> {
    let Some(node_name) = node_name else {
        return Ok(());
    };
    let plain = !node_name.is_empty()
        && !node_name.contains(['/', '\\', ':'])
        && node_name != "."
        && node_name != ".."
        && node_name.trim() == node_name;
    if plain {
        Ok(())
    } else {
        Err(format!(
            "supervisor {name:?}: node_name {node_name:?} must be a plain name — it names this \
             host's certificate and key files"
        ))
    }
}

/// Windows hosts are not served by this kind until a repacked tree has been run on one
/// (ADR-draft-icinga-2 clause 23): whether it runs without the MSI's product registration is
/// unproven, and the kind has no other form to fall back on. Its block still checks, so a
/// Supervisor set naming it is refused where it would run, naming why, rather than started on a
/// guess.
fn refuse_an_unproven_platform(name: &str) -> Result<(), String> {
    if cfg!(windows) {
        return Err(format!(
            "Supervisor {name:?}: the icinga2 kind does not run on Windows yet — whether the \
             repacked tree runs without the MSI's product registration has not been proven"
        ));
    }
    Ok(())
}

impl Icinga2Plugin {
    /// Icinga 2 runs only on a host without a process account (ADR-draft-icinga-2): its daemon and
    /// every subcommand drop to an account of their own (`-D RunAsUser`), which a program the
    /// second service starts under the process account cannot do, and the kind runs its `pki` and
    /// `daemon -C` subcommands itself. A block is refused there, naming the way back, rather than
    /// started past the second service (ADR-0037 clause 18).
    fn refuse_a_process_account(&self, name: &str) -> Result<(), String> {
        if matches!(self.spawner, Spawner::Channel(_)) {
            return Err(format!(
                "Supervisor {name:?}: the icinga2 kind does not run where a process account is \
                 registered; register the Client without one (`service install \
                 --no-process-account`) to run Icinga 2"
            ));
        }
        Ok(())
    }
}

impl Plugin for Icinga2Plugin {
    fn kind(&self) -> &'static str {
        "icinga2"
    }

    fn program_key(&self) -> &'static str {
        "binary"
    }

    /// What the tree `opamp-fleetctl package fetch --agent icinga2` packs decides, per platform
    /// (ADR-draft-icinga-2): the daemon's file name, where it sits inside that tree, and the Agent
    /// type every Icinga Configuration is aimed at. None of the three is a decision a host makes.
    fn defaults(&self) -> crate::supervisor::ports::KindDefaults {
        crate::supervisor::ports::KindDefaults {
            program: Some(PROGRAM),
            program_path: Some(PROGRAM_PATH),
            // A tree, so no single-file member to name.
            member: None,
            // The repack dereferences every link of the tree, and none leads out.
            tree_links: &[],
            service_name: Some("icinga2"),
            // Icinga's own shutdown is slow — it drains its checks and closes its cluster
            // connections — so 60 seconds is a property of Icinga, not of a host, and the fleet's
            // 10 would kill it mid-drain. The apply grace is raised for the same reason: a daemon
            // that takes this long to stop takes a while to be trustworthy after a start.
            timing: Some(crate::supervisor::ports::KindTiming {
                stop_timeout: Some(Duration::from_secs(60)),
                apply_grace: Some(Duration::from_secs(30)),
                retain_previous: None,
            }),
            // Nothing connects to an Icinga Supervisor's Endpoint: the daemon speaks Icinga's
            // cluster protocol to its parent, not OpAMP to us.
            endpoint_port: false,
        }
    }

    fn start(&self, mut ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        refuse_an_unproven_platform(&ctx.name)?;
        self.refuse_a_process_account(&ctx.name)?;
        let settings: Icinga2Settings = parse_settings(
            &ctx.name,
            self.kind(),
            RETIRED,
            std::mem::take(&mut ctx.settings),
        )?;
        check_node_name(&ctx.name, settings.node_name.as_deref())?;
        let layout = Self::layout(&ctx, &settings);
        // What the delivered tree needs to run at all, and nothing else (ADR-draft-icinga-2): its
        // own libraries have to win over whatever the machine has, which is the whole point of a
        // relocatable tree. Windows needs no equivalent — a program there finds its DLLs in its own
        // directory first, which is also why the check plugins sit beside the daemon on that
        // platform. The daemon, the version probe and every subcommand carry the same.
        let env = layout.env.clone();
        // Two channels, not one: what the core holds goes through the validation gate first, and
        // what the Runner receives is what survived it (ADR-draft-icinga-2).
        let (commands, from_core) = mpsc::channel(16);
        let (to_runner, command_rx) = mpsc::channel(16);
        let name = ctx.name.clone();
        let events = ctx.events.clone();
        // The Runner's events pass through the kind, so that its "awaiting configuration" says what
        // is awaited — the gate's reason, which only the kind knows (ADR-draft-icinga-2 clause 8).
        let (runner_events, from_runner) = mpsc::channel(64);
        let gate = layout.clone();
        let runner = Runner {
            // None. The daemon speaks Icinga's cluster protocol and never connects to the
            // Supervisor Endpoint, so the token would only sit in the environment of every check
            // plugin it runs. An empty token is one the Runner does not set
            // (ADR-draft-client-modes-and-the-gateway clause 28).
            endpoint_token: zeroize::Zeroizing::default(),
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            retain_previous: ctx.retain_previous,
            install: Some(ctx.install),
            archive_key: ctx.archive_key,
            version_probe: Some(VersionProbe {
                program: layout.program.clone(),
                args: vec!["--version".to_string()],
                working_dir: None,
                parse: Some(parse_version),
                env: env.clone(),
            }),
            // The delivered tree carries its own libraries, so what proves it runs must be run
            // against *those* — a version banner costs 30 ms and answers the one question a
            // repacked tree raises: does this host's libc satisfy it (ADR-draft-icinga-2)?
            preflight: Some(preflight()),
            // Icinga re-reads its configuration on SIGHUP and keeps the umbrella's pid, so the
            // reload of ADR-0017 applies as it stands. Windows refuses the concept, and the
            // Runner falls back to the restart there.
            reload_signal: sighup(),
            // The Client itself: where a process account is registered the kind refused to start
            // above (ADR-0037 clause 18).
            spawner: self.spawner.clone(),
            process_account_group: ctx.process_account_group,
            supervisor_dir: ctx.supervisor_dir.clone(),
            events: EventSender::new(0, runner_events),
            commands: command_rx,
            build: Box::new(move || {
                match layout.ready() {
                    Err(reason) => {
                        tracing::debug!(supervisor = %name, reason = %reason, "not starting Icinga 2 yet");
                        None
                    }
                    Ok(root) => Some(ProcessSpec {
                        program: layout.program.clone(),
                        args: layout.daemon_args(&root),
                        env: env.clone(),
                        working_dir: None,
                        // Its worker must not survive the stop (ADR-draft-icinga-2).
                        own_process_group: true,
                        // Icinga creates none of its directories and exits when one is missing
                        // (ADR-draft-icinga-2). Naming them here rather than making them in this
                        // closure is the same guarantee through the seam every kind now uses: made
                        // before every spawn, so one an operator removed comes back.
                        ensure_dirs: layout.state_dirs(),
                    }),
                }
            }),
        };
        tokio::spawn(runner.run(ctx.shutdown));
        tokio::spawn(explain_waits(from_runner, gate.clone(), ctx.events));
        tokio::spawn(intercept(
            from_core,
            to_runner.clone(),
            gate.clone(),
            events.clone(),
        ));
        // In the adapter, never in `start`: a parent that is down must not hold up the Client's
        // startup (ADR-draft-icinga-2).
        tokio::spawn(enrol(gate, to_runner, events));
        Ok(commands)
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        // Retired keys are refused before the strict parse, so a block carrying one is told where
        // its value went instead of meeting serde's "unknown field" (ADR-draft-icinga-2).
        self.refuse_a_process_account(name)?;
        let settings = parse_settings::<Icinga2Settings>(name, self.kind(), RETIRED, settings)?;
        check_node_name(name, settings.node_name.as_deref())
    }

    /// A delivered block reads its ticket and the parent's certificate only from its own
    /// configuration directory, and pins the parent it names (ADR-0017 clause 36): otherwise the
    /// Server could name any file on the host and a parent to send it to.
    fn check_delivered(
        &self,
        settings: &toml::Table,
        running: Option<&toml::Table>,
    ) -> Result<(), String> {
        // The block the operator wrote, delivered back unchanged, brings nothing new.
        if running == Some(settings) {
            return Ok(());
        }
        // A file the operator named is kept only while it goes to the parent the operator named,
        // under the node name the operator chose.
        let same_parent = ["parent_host", "node_name"]
            .iter()
            .all(|key| running.map(|running| running.get(*key)) == Some(settings.get(*key)));
        for key in ["ticket_file", "trusted_cert_file"] {
            let Some(value) = settings.get(key) else {
                continue;
            };
            if same_parent && running.and_then(|running| running.get(key)) == Some(value) {
                continue;
            }
            let inside = value
                .as_str()
                .and_then(|path| path.strip_prefix("${config_dir}/"))
                .is_some_and(|rest| {
                    let rest = std::path::Path::new(rest);
                    rest.components().count() > 0
                        && rest
                            .components()
                            .all(|c| matches!(c, std::path::Component::Normal(_)))
                });
            if !inside {
                return Err(format!(
                    "a delivered block's {key} must be ${{config_dir}}/<file>, inside its own \
                     configuration directory"
                ));
            }
        }
        if settings.contains_key("parent_host") && !settings.contains_key("trusted_cert_file") {
            return Err(
                "a delivered block must name trusted_cert_file: a parent the Server names is not \
                 trusted on first use"
                    .to_string(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where a process account is registered, an icinga2 block is refused, when it is checked and
    /// when it starts, before anything is run: the kind would start its subcommands past the second
    /// service (ADR-0037 clause 18).
    /// Verifies: ADR-draft-icinga-2
    #[cfg(unix)]
    #[test]
    fn a_host_with_a_process_account_refuses_an_icinga2_block() {
        let plugin = Icinga2Plugin {
            spawner: Spawner::Channel(crate::supervisor::channel::Channel::new(
                std::path::Path::new("/nonexistent/agents.sock"),
                0,
                crate::supervisor::channel::ServiceProcess::named(|| Ok(None)),
            )),
        };
        let err = plugin
            .check("icinga", toml::Table::new())
            .expect_err("refused");
        assert!(err.contains("process account"), "{err}");
        assert!(err.contains("--no-process-account"), "{err}");
        Icinga2Plugin::default()
            .check("icinga", toml::Table::new())
            .expect("no process account, no refusal");
    }

    fn settings(toml_text: &str) -> Icinga2Settings {
        toml_text
            .parse::<toml::Table>()
            .expect("table")
            .try_into()
            .expect("settings")
    }

    /// The paths are the tree's own (ADR-draft-icinga-2), and the state directories sit beside it.
    /// Asserted against a context rather than against the settings, because the settings have
    /// nothing to say about any of them.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_layout_follows_the_delivered_tree_and_needs_no_settings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("supervisors").join("icinga2");
        let (_tx, shutdown) = crate::shutdown::shutdown_channel();
        let (events, _rx) = tokio::sync::mpsc::channel(1);
        let ctx = SupervisorContext {
            endpoint_token: zeroize::Zeroizing::new(String::new()),
            name: "icinga2".to_string(),
            supervisor_dir: root.clone(),
            config_dir: root.join("config"),
            program: root.join("program/tree").join(PROGRAM_PATH),
            install: crate::supervisor::ports::InstallTarget::Tree {
                root: root.join("program"),
                program_path: PathBuf::from(PROGRAM_PATH),
                links: &[],
            },
            archive_key: None,
            process_account_group: None,
            settings: toml::Table::new(),
            stop_timeout: Duration::from_secs(1),
            apply_grace: Duration::from_secs(1),
            retain_previous: Duration::ZERO,
            events: crate::supervisor::ports::EventSender::new(0, events),
            shutdown,
        };
        let settings = settings("");
        let layout = Icinga2Plugin::layout(&ctx, &settings);

        let tree = root.join("program").join("tree");
        assert_eq!(
            layout.include_dir,
            tree.join("share").join("icinga2").join("include"),
            "the ITL the root configuration includes is the tree's own"
        );
        assert_eq!(
            layout.plugin_dir,
            if cfg!(windows) {
                tree.join("sbin")
            } else {
                tree.join("plugins")
            },
            "on Windows the checks stay beside the daemon, for the DLLs they share"
        );
        assert_eq!(layout.data_dir, root.join("data"));
        assert_eq!(layout.run_dir, root.join("run"));
        // The environment every invocation carries is the tree's own libraries (ADR-draft-icinga-2
        // clause 5).
        if cfg!(windows) {
            assert!(layout.env.is_empty(), "{:?}", layout.env);
        } else {
            assert_eq!(
                layout.env,
                [(
                    "LD_LIBRARY_PATH".to_string(),
                    format!("{}/lib", tree.display())
                )]
            );
        }
        // The rule, not this host's answer: the default is the resolved FQDN where the resolver
        // has a qualified name to give, and the Supervisor's name where it has none. Asserting
        // the second alone passed on a machine without a domain and failed on every CI runner.
        assert_eq!(
            layout.node_name,
            resolved_fqdn().map_or_else(|| "icinga2".to_string(), str::to_string),
            "the host's FQDN, or the Supervisor's name where none resolves"
        );
    }

    /// One address is one value (ADR-draft-icinga-2). The IPv6 case is why the split is taken from
    /// the last colon and only when what follows it is a port.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn a_parent_carries_its_port_or_icingas_default() {
        assert_eq!(
            parent_address("master.example"),
            ("master.example".to_string(), 5665)
        );
        assert_eq!(
            parent_address("master.example:5666"),
            ("master.example".to_string(), 5666)
        );
        assert_eq!(parent_address("::1"), ("::1".to_string(), 5665));
        assert_eq!(parent_address("[::1]:5665"), ("::1".to_string(), 5665));
    }

    /// A block carrying a key this kind supplies itself is told where its value went, rather than
    /// meeting serde's "unknown field" — the pattern `package` and `accepts_packages` already run.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn a_retired_key_is_refused_by_name_and_says_what_supplies_it_now() {
        for (key, expected) in [
            ("plugin_dir = \"x\"", "the delivered tree"),
            ("log_level = \"debug\"", "object FileLogger"),
            ("parent_port = 5665", "`parent_host` as `host:port`"),
            (
                "run_as_user = \"nagios\"",
                "the account this Client runs as",
            ),
            ("[env]\nLD_LIBRARY_PATH = \"x\"", "LD_LIBRARY_PATH included"),
        ] {
            let table: toml::Table = key.parse().expect("table");
            let error = Icinga2Plugin::default()
                .check("icinga2", table)
                .expect_err(&format!("{key} is refused"));
            assert!(error.contains(expected), "{key} -> {error}");
        }
    }

    /// Delivers a root configuration the way the fleet does: the entry, plus the role that says
    /// it is the root. `role` empty writes only the entry, which is a fleet that marked nothing.
    fn deliver_root(config_dir: &std::path::Path, name: &str, role: &str, body: &str) {
        std::fs::create_dir_all(config_dir).expect("config dir");
        std::fs::write(config_dir.join(name), body).expect("entry");
        if !role.is_empty() {
            std::fs::write(
                config_dir.join(crate::storage::SUPPLEMENTARY_FILE),
                format!("{name} {role}\n"),
            )
            .expect("roles");
        }
    }

    fn layout(dir: &std::path::Path) -> Layout {
        Layout {
            program: dir.join("program/tree/sbin/icinga2"),
            env: tree_environment(&dir.join("program/tree").to_string_lossy()),
            config_dir: dir.join("config"),
            include_dir: dir.join("program/tree/share/icinga2/include"),
            plugin_dir: dir.join("program/tree/plugins"),
            data_dir: dir.join("data"),
            log_dir: dir.join("log"),
            cache_dir: dir.join("cache"),
            spool_dir: dir.join("spool"),
            run_dir: dir.join("run"),
            node_name: "edge-01".to_string(),
            run_as: Some(("fleet".to_string(), "fleet".to_string())),
            parent: Some(("master.example".to_string(), 5665)),
            ticket_file: None,
            trusted_cert_file: None,
            pinned_parent: dir.join("trusted-parent.crt"),
            renew_before: Duration::from_secs(30 * 24 * 60 * 60),
            marker: dir.join("icinga2-enrolment.json"),
            refused: dir.join("icinga2-refused.json"),
        }
    }

    /// What is left after ADR-draft-icinga-2: the enrolment. Everything else this kind supplies
    /// itself, so a block naming one is an unknown key here — and is refused by name a step
    /// earlier, which the test below covers.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn settings_parse_strictly() {
        let parsed = settings(
            r#"
            parent_host = "master.example"
            node_name = "edge-01"
            "#,
        );
        assert_eq!(parsed.node_name.as_deref(), Some("edge-01"));
        assert!(parsed.ticket_file.is_none());

        for typo in [
            "binary = \"icinga2\"",
            "parent = \"master\"",
            "datadir = \"x\"",
        ] {
            let table: toml::Table = typo.parse().expect("table");
            assert!(
                table.try_into::<Icinga2Settings>().is_err(),
                "{typo} must not parse"
            );
        }
    }

    /// The three arguments an operator must never have to write, and the shape ADR-draft-icinga-2
    /// fixes: the account, the include directory, and every state directory.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_daemon_arguments_carry_the_relocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let args = layout(dir.path()).daemon_args(&dir.path().join("config/icinga2-conf"));
        let joined = args.join(" ");

        assert_eq!(args[0], "daemon");
        assert_eq!(args[1], "-c");
        assert!(args[2].ends_with("config/icinga2-conf"), "{joined}");
        for expected in [
            "RunAsUser=fleet",
            "RunAsGroup=fleet",
            "NodeName=edge-01",
            "IncludeConfDir=",
            "PluginDir=",
            "DataDir=",
            "LogDir=",
            "CacheDir=",
            "SpoolDir=",
            "InitRunDir=",
        ] {
            assert!(
                args.iter().any(|a| a.starts_with(expected)),
                "missing {expected} in {joined}"
            );
        }
        // Foreground: the Runner supervises what it started, and a daemonized Icinga would
        // detach from it (ADR-draft-icinga-2's requirement).
        assert!(
            !args.iter().any(|a| a == "-d" || a == "--daemonize"),
            "{joined}"
        );
        assert!(!args.iter().any(|a| a == "--close-stdio"), "{joined}");
        // Icinga's own console severity, last: verbosity is the fleet's to raise in Icinga's
        // configuration, not a value this command line takes from anywhere (ADR-draft-icinga-2).
        assert_eq!(&args[args.len() - 2..], ["-x", "information"], "{joined}");
    }

    /// Nothing starts before the root configuration is there — and, in the Agent role, before the
    /// certificate is. Both are waits with a reason, not failures.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_daemon_waits_for_its_configuration_and_its_certificate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = layout(dir.path());

        let blocked = layout.root_config().expect_err("no configuration yet");
        assert!(blocked.contains("awaiting Icinga's root"), "{blocked}");

        deliver_root(
            &layout.config_dir,
            "icinga2-conf",
            ROOT_ROLE,
            "include <itl>\n",
        );
        layout.root_config().expect("the fleet marked it");
        let blocked = layout.blocked_by().expect("no certificate yet");
        assert!(blocked.contains("awaiting the certificate"), "{blocked}");

        std::fs::create_dir_all(layout.certs_dir()).expect("certs dir");
        std::fs::write(layout.certificate(), "cert").expect("cert");
        std::fs::write(layout.ca_certificate(), "ca").expect("ca");
        assert_eq!(layout.blocked_by(), None);
    }

    /// What this kind supplies is what `opamp-fleetctl package fetch` packs — the client half of
    /// `docs/artifacts/icinga2.md`, whose packing half is
    /// `icinga_2s_linux_artifact_is_the_debian_tree_under_its_wrapper`. Icinga publishes no
    /// portable tree, so this tree's layout is entirely this project's: an upstream that moves
    /// something inside it is a change both halves have to answer.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_defaults_are_the_artifacts() {
        let defaults = Icinga2Plugin::default().defaults();
        assert_eq!(defaults.service_name, Some("icinga2"));
        assert!(!defaults.endpoint_port);
        #[cfg(windows)]
        {
            assert_eq!(defaults.program, Some("icinga2.exe"));
            assert_eq!(defaults.program_path, Some("sbin/icinga2.exe"));
        }
        #[cfg(not(windows))]
        {
            assert_eq!(defaults.program, Some("icinga2"));
            assert_eq!(defaults.program_path, Some("sbin/icinga2"));
        }
    }

    /// The default only takes a name that is actually qualified, and a host name at that. An
    /// unqualified answer — which is what a resolver hands back for a host with no domain — would
    /// put the short name where Icinga expects the CN its ticket was minted for, and enrolment
    /// would fail with a name that looks right; anything but a host name would name files.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn only_a_qualified_name_becomes_the_default() {
        // The resolution itself depends on this machine's resolver, so what is asserted is the
        // rule applied to it: whatever comes back is either qualified or not used at all.
        if let Some(name) = resolved_fqdn() {
            assert!(
                is_qualified_host_name(name),
                "{name} is not a qualified host name"
            );
        }
        for taken in ["edge-01.example.com", "a.b", "node1.local"] {
            assert!(is_qualified_host_name(taken), "{taken}");
        }
        for refused in [
            "edge-01",
            "",
            ".",
            "edge..example",
            "edge.example.",
            "../../etc.passwd",
            "edge/01.example",
            "-edge.example",
            "edge_01.example",
            "edge 01.example",
        ] {
            assert!(!is_qualified_host_name(refused), "{refused}");
        }
    }

    /// And the operator's value outranks it, because a master may know this host under a name no
    /// resolver here would produce (ADR-draft-icinga-2).
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn a_configured_node_name_outranks_the_resolved_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("icinga2");
        let (_tx, shutdown) = crate::shutdown::shutdown_channel();
        let (events, _rx) = tokio::sync::mpsc::channel(1);
        let ctx = SupervisorContext {
            endpoint_token: zeroize::Zeroizing::new(String::new()),
            name: "icinga2".to_string(),
            supervisor_dir: root.clone(),
            config_dir: root.join("config"),
            program: root.join("program/tree").join(PROGRAM_PATH),
            install: crate::supervisor::ports::InstallTarget::Tree {
                root: root.join("program"),
                program_path: PathBuf::from(PROGRAM_PATH),
                links: &[],
            },
            archive_key: None,
            process_account_group: None,
            settings: toml::Table::new(),
            stop_timeout: Duration::from_secs(1),
            apply_grace: Duration::from_secs(1),
            retain_previous: Duration::ZERO,
            events: crate::supervisor::ports::EventSender::new(0, events),
            shutdown,
        };
        let configured =
            Icinga2Plugin::layout(&ctx, &settings("node_name = \"edge-01.example\"\n"));
        assert_eq!(configured.node_name, "edge-01.example");
    }

    /// The fleet says which entry is the root, with a role this kind defines — the Baseline's own
    /// model, where *"the values and their semantics are Agent type-specific"*. A marked entry
    /// wins over the conventional name, which exists only so a rollout written earlier keeps
    /// working.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_marked_entry_is_the_root_even_beside_the_conventional_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = layout(dir.path());
        std::fs::create_dir_all(&layout.config_dir).expect("config dir");
        std::fs::write(layout.config_dir.join("icinga2-conf"), "old\n").expect("entry");
        std::fs::write(layout.config_dir.join("site-root"), "new\n").expect("entry");
        std::fs::write(
            layout.config_dir.join(crate::storage::SUPPLEMENTARY_FILE),
            format!("site-root {ROOT_ROLE}\nicinga2-zones supplementary\n"),
        )
        .expect("roles");

        assert_eq!(
            layout.root_config().expect("the marked one"),
            layout.config_dir.join("site-root")
        );
    }

    /// And two of them is a question this Client refuses to answer by guessing: the daemon reads
    /// one root, so a rollout that marks two is a rollout to fix, not a coin to flip.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn two_marked_entries_are_a_reason_not_to_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = layout(dir.path());
        std::fs::create_dir_all(&layout.config_dir).expect("config dir");
        std::fs::write(
            layout.config_dir.join(crate::storage::SUPPLEMENTARY_FILE),
            format!("one {ROOT_ROLE}\ntwo {ROOT_ROLE}\n"),
        )
        .expect("roles");

        let error = layout.root_config().expect_err("two roots");
        assert!(error.contains("one and two"), "{error}");
    }

    /// Without a parent there is no enrolment to wait for: a standalone Icinga 2 runs as soon as
    /// its configuration arrives.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn a_standalone_node_needs_no_certificate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = layout(dir.path());
        layout.parent = None;
        // A fleet that marked nothing: the conventional name is what stands in.
        deliver_root(&layout.config_dir, "icinga2-conf", "", "include <itl>\n");
        layout.root_config().expect("the conventional name");
        assert_eq!(layout.blocked_by(), None);
    }

    /// Icinga creates none of its directories and fails on the first write into a missing one.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn preparing_creates_every_state_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = layout(dir.path());
        layout.prepare_dirs().expect("prepare");
        for expected in layout.state_dirs() {
            assert!(expected.is_dir(), "{} was not created", expected.display());
        }
    }

    /// The banner the strict SemVer read rejects, and the versions it must keep reading.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_version_banner_is_read_without_its_packaging_revision() {
        assert_eq!(
            parse_version("icinga2 - The Icinga 2 network monitoring daemon (version: r2.14.6-1)")
                .as_deref(),
            Some("2.14.6")
        );
        assert_eq!(parse_version("v2.15.0").as_deref(), Some("2.15.0"));
        assert_eq!(parse_version("2.14.6").as_deref(), Some("2.14.6"));
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(parse_version("version: 2.14"), None);
    }

    /// The preflight asks the staged tree with that tree's own libraries, by the same rule the
    /// daemon and every subcommand follow for the installed one (ADR-draft-icinga-2 clauses 5
    /// and 15).
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_preflight_runs_the_staged_tree_against_its_own_libraries() {
        let check = preflight();
        assert_eq!(check.args, ["--version"]);
        assert_eq!(check.env, tree_environment(STAGED));
        assert_eq!(check.working_dir, None, "the daemon names none either");
        if cfg!(windows) {
            assert!(check.env.is_empty(), "{:?}", check.env);
        } else {
            assert_eq!(
                check.env,
                [("LD_LIBRARY_PATH".to_string(), "${staged}/lib".to_string())]
            );
        }
    }

    /// Every subcommand runs with the tree's environment, not the one the Client happens to have:
    /// `pki` and `daemon -C` link the same bundled libraries the daemon does (ADR-draft-icinga-2
    /// clause 5). The stub refuses to run without it, as the real binary does on a host without its
    /// Boost.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn every_subcommand_carries_the_trees_environment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = layout(dir.path());
        layout.program = stub();
        run_subcommand(&layout, &["--version".to_string()])
            .await
            .expect("the tree's environment lets it run");
        if cfg!(unix) {
            layout.env.clear();
            let err = run_subcommand(&layout, &["--version".to_string()])
                .await
                .expect_err("without the tree's libraries nothing runs");
            assert!(
                err.contains("error while loading shared libraries"),
                "{err}"
            );
        }
    }

    /// Windows is not served until a repacked tree has run on it (ADR-draft-icinga-2 clause 23):
    /// the kind refuses to start there, naming why, and starts everywhere else.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_kind_refuses_to_start_on_windows_until_it_is_proven_there() {
        let verdict = refuse_an_unproven_platform("icinga2");
        if cfg!(windows) {
            let err = verdict.expect_err("refused on Windows");
            assert!(err.contains("Windows"), "{err}");
        } else {
            verdict.expect("served here");
        }
    }

    /// A `ticket_file` that is named but not there yet — or holds nothing — is a wait: no key is
    /// made and no request is queued on the parent that nobody meant to sign by hand. It enrols
    /// once the ticket has arrived.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn a_named_ticket_that_is_missing_is_waited_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = enrolling(dir.path());
        let ticket = layout.ticket_file.clone().expect("named");
        std::fs::remove_file(&ticket).expect("not delivered yet");

        let err = ensure_enrolled(&layout).await.expect_err("waits");
        assert!(err.contains("ticket_file"), "{err}");
        assert!(
            !layout.certificate().exists(),
            "no key or certificate is made"
        );
        std::fs::write(&ticket, "\n").expect("an empty one");
        ensure_enrolled(&layout).await.expect_err("still waits");

        std::fs::write(&ticket, "a-ticket\n").expect("delivered");
        assert!(ensure_enrolled(&layout).await.expect("enrols"));
    }

    /// The ticket is a per-host secret: a subcommand that carried it and failed names the command
    /// with the ticket replaced, and drops it from what Icinga printed, so neither the log, the
    /// health nor the fleet view carries it.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn a_failed_request_never_shows_the_ticket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = layout(dir.path());
        layout.program = stub();
        let request = |host: &str| {
            [
                "pki",
                "request",
                "--host",
                host,
                "--ticket",
                "s3cr3t-ticket",
            ]
            .map(str::to_string)
        };
        let err = run_subcommand(&layout, &request("unreachable.example"))
            .await
            .expect_err("the parent is down");
        assert!(!err.contains("s3cr3t-ticket"), "{err}");
        assert!(err.contains("--ticket <redacted>"), "{err}");

        // A parent whose answer quotes the ticket back: the reason is scrubbed too.
        let err = run_subcommand(&layout, &request("echoing.example"))
            .await
            .expect_err("the parent refuses the ticket");
        assert!(!err.contains("s3cr3t-ticket"), "{err}");
        assert!(err.contains("Invalid ticket '<redacted>'"), "{err}");

        // A program that does not start at all names the command the same way.
        layout.program = dir.path().join("no-such-icinga2");
        let err = run_subcommand(&layout, &request("master.example"))
            .await
            .expect_err("nothing to run");
        assert!(!err.contains("s3cr3t-ticket"), "{err}");
    }

    /// The stub that stands in for `icinga2` — built by the same `cargo test` run, and next to the
    /// test binary itself.
    fn stub() -> PathBuf {
        let mut path = std::env::current_exe().expect("test binary");
        path.pop();
        if path.ends_with("deps") {
            path.pop();
        }
        path.join(format!("stub_icinga2{}", std::env::consts::EXE_SUFFIX))
    }

    /// A layout whose program is the stub, with the parent and the ticket an Agent enrols with.
    fn enrolling(dir: &std::path::Path) -> Layout {
        let mut layout = layout(dir);
        layout.program = stub();
        std::fs::create_dir_all(dir.join("config")).expect("config dir");
        std::fs::write(dir.join("config/icinga2-ticket"), "  a-ticket\n").expect("ticket");
        layout.ticket_file = Some(dir.join("config/icinga2-ticket"));
        layout.trusted_cert_file = None;
        layout
    }

    /// The whole of ADR-draft-icinga-2's happy path: a key generated here, a parent pinned, a
    /// signature — and then nothing at all, because the certificate on disk is the state.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn enrolment_obtains_a_certificate_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = enrolling(dir.path());

        assert!(
            ensure_enrolled(&layout).await.expect("enrol"),
            "first run enrols"
        );
        assert_eq!(
            std::fs::read_to_string(layout.certificate()).expect("cert"),
            "stub-signed-cert",
            "the parent's signature replaced the self-signed certificate"
        );
        assert!(
            layout.ca_certificate().is_file(),
            "the CA travelled with it"
        );
        assert!(
            layout.pinned_parent.is_file(),
            "the parent certificate is pinned outside config/, which every apply empties"
        );
        assert!(layout.marker.is_file(), "the enrolment is recorded");

        assert!(
            !ensure_enrolled(&layout).await.expect("second run"),
            "a usable certificate ends enrolment before anything runs"
        );
    }

    /// On-demand signing: a request without a ticket that the parent queues is a wait. The parent
    /// files it under the self-signed certificate's fingerprint, so every retry presents the same
    /// key and certificate, and once `icinga2 ca sign` ran there the next one fetches the signed
    /// certificate. Until then no CA stands beside the unsigned certificate, so the gate stays
    /// shut.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn a_queued_request_is_asked_again_with_the_same_key_until_the_parent_signs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = enrolling(dir.path());
        layout.ticket_file = None;
        let key = layout.certs_dir().join("edge-01.key");
        let read = |path: &std::path::Path| std::fs::read_to_string(path).expect("readable");

        let err = ensure_enrolled(&layout).await.expect_err("queued");
        assert!(err.contains("icinga2 ca sign"), "{err}");
        assert!(
            !layout.ca_certificate().exists(),
            "no CA beside a certificate nobody signed"
        );
        assert!(layout.blocked_by().is_some(), "the gate stays shut");
        assert!(!layout.marker.is_file(), "nothing is recorded as enrolled");
        let (first_key, first_cert) = (read(&key), read(&layout.certificate()));

        let err = ensure_enrolled(&layout).await.expect_err("still queued");
        assert!(err.contains("same key"), "{err}");
        assert_eq!(read(&key), first_key, "a retry makes no new key");
        assert_eq!(read(&layout.certificate()), first_cert);

        // `icinga2 ca sign` on the parent, for the request it holds.
        let pending = format!("{}.pending", layout.certificate().display());
        std::fs::copy(
            &pending,
            format!("{}.approved", layout.certificate().display()),
        )
        .expect("sign");
        assert!(ensure_enrolled(&layout).await.expect("signed"));
        assert_eq!(read(&layout.certificate()), "stub-signed-cert");
        assert_eq!(
            read(&key),
            first_key,
            "the key the parent signed for is kept"
        );
        assert!(layout.ca_certificate().is_file());
        assert!(layout.marker.is_file());
        assert_eq!(layout.blocked_by(), None);
    }

    /// Trust on first use happens once per parent: a renewal keeps the certificate pinned when the
    /// node first enrolled rather than trusting what the parent presents at that moment, and only
    /// another parent is pinned anew.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn trust_on_first_use_pins_a_parent_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = enrolling(dir.path());
        ensure_enrolled(&layout).await.expect("enrol");
        let pinned = |layout: &Layout| std::fs::read_to_string(&layout.pinned_parent).expect("pin");
        assert_eq!(pinned(&layout), "stub-parent-cert", "pinned on first use");
        std::fs::write(&layout.pinned_parent, "the-pin-of-first-use").expect("pin");

        std::fs::write(layout.certificate(), "expiring").expect("near expiry");
        assert!(ensure_enrolled(&layout).await.expect("renew"));
        assert_eq!(
            pinned(&layout),
            "the-pin-of-first-use",
            "a renewal keeps the pin"
        );

        layout.parent = Some(("other.example".to_string(), 5665));
        assert!(ensure_enrolled(&layout)
            .await
            .expect("enrol with another parent"));
        assert_eq!(
            pinned(&layout),
            "stub-parent-cert",
            "another parent is pinned anew"
        );
    }

    /// A `trusted_cert_file` that is named but not there — not yet delivered, or mistyped — never
    /// falls back to trust on first use: enrolment fails, nothing is saved from the parent, and it
    /// pins the file once it arrives.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn a_named_parent_certificate_that_is_missing_is_waited_for_not_trusted_on_sight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = enrolling(dir.path());
        let named = dir.path().join("config/parent.crt");
        layout.trusted_cert_file = Some(named.clone());

        let err = ensure_enrolled(&layout)
            .await
            .expect_err("a missing pin is not trusted on sight");
        assert!(err.contains("trusted_cert_file"), "{err}");
        assert!(
            err.contains(&named.display().to_string()),
            "names the path: {err}"
        );
        assert!(
            !layout.certificate().is_file(),
            "no key or certificate is made for a parent that cannot be pinned yet"
        );
        assert!(
            !layout.pinned_parent.is_file(),
            "nothing the parent presented was saved as trusted"
        );
        assert!(!layout.marker.is_file(), "nothing is recorded as enrolled");

        std::fs::write(&named, "the-parents-own-cert").expect("deliver it");
        assert!(
            ensure_enrolled(&layout).await.expect("enrol"),
            "enrols once it is there"
        );
        assert_eq!(
            std::fs::read_to_string(&layout.pinned_parent).expect("pinned"),
            "the-parents-own-cert",
            "the delivered certificate is what is pinned"
        );
    }

    /// What the fleet transports for enrolment is two `supplementary` Configurations beside the
    /// root, as one per-host Deployment delivers them: the ticket and the parent's own
    /// certificate. The Supervisor reads both and the daemon is pointed at neither — its root is
    /// the entry marked `main` — and what comes back is a key made on this host and a certificate
    /// the parent signed, never a key from the fleet (ADR-draft-icinga-2 clause 11).
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn the_ticket_and_the_parent_certificate_reach_the_supervisor_and_not_the_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = enrolling(dir.path());
        let config = layout.config_dir.clone();
        std::fs::remove_file(config.join("icinga2-ticket")).expect("the generic ticket");
        std::fs::write(config.join("icinga2-conf"), "include <itl>\n").expect("root");
        std::fs::write(config.join("icinga2-ticket-edge-01"), "the-ticket\n").expect("ticket");
        std::fs::write(config.join("icinga2-parent"), "the-parents-own-cert").expect("parent");
        std::fs::write(
            config.join(crate::storage::SUPPLEMENTARY_FILE),
            format!(
                "icinga2-conf {ROOT_ROLE}\nicinga2-ticket-edge-01 supplementary\n\
                 icinga2-parent supplementary\n"
            ),
        )
        .expect("roles");
        layout.ticket_file = Some(config.join("icinga2-ticket-edge-01"));
        layout.trusted_cert_file = Some(config.join("icinga2-parent"));

        let root = layout.root_config().expect("the marked root");
        assert_eq!(root, config.join("icinga2-conf"));
        let args = layout.daemon_args(&root).join(" ");
        for transported in ["icinga2-ticket-edge-01", "icinga2-parent"] {
            assert!(
                !args.contains(transported),
                "the daemon is pointed at it: {args}"
            );
        }

        assert!(ensure_enrolled(&layout).await.expect("enrol"), "enrols");
        assert_eq!(
            std::fs::read_to_string(layout.certificate()).expect("cert"),
            "stub-signed-cert",
            "the ticket was presented, so the parent signed rather than queued"
        );
        assert_eq!(
            std::fs::read_to_string(&layout.pinned_parent).expect("pinned"),
            "the-parents-own-cert"
        );
        let key = std::fs::read_to_string(layout.certs_dir().join("edge-01.key")).expect("key");
        assert!(key.starts_with("stub-key-"), "made on this host: {key}");
        assert!(
            !std::fs::read_dir(&config)
                .expect("config")
                .flatten()
                .any(|entry| std::fs::read_to_string(entry.path()).is_ok_and(|t| t == key)),
            "no key travels in the delivered configuration"
        );
    }

    /// A parent that cannot be reached is a wait: the error names the reason, and nothing is
    /// recorded as enrolled.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn an_unreachable_parent_is_reported_rather_than_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = enrolling(dir.path());
        layout.parent = Some(("unreachable.example".to_string(), 5665));

        let err = ensure_enrolled(&layout)
            .await
            .expect_err("the parent is down");
        assert!(err.contains("Cannot connect to host"), "{err}");
        assert!(!layout.marker.is_file(), "nothing is recorded as enrolled");
    }

    /// The certificate is the state, so one that no longer verifies enrols again — the marker
    /// alone must never keep a dead certificate in service.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn an_expired_certificate_enrols_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = enrolling(dir.path());
        ensure_enrolled(&layout).await.expect("enrol");

        std::fs::write(layout.certificate(), "expired").expect("expire it");
        assert!(
            ensure_enrolled(&layout).await.expect("enrol again"),
            "an unusable certificate is replaced rather than trusted"
        );
        assert_eq!(
            std::fs::read_to_string(layout.certificate()).expect("cert"),
            "stub-signed-cert"
        );
    }

    /// The line `pki verify` prints, in the shape a real Icinga 2 prints it — including the
    /// double space OpenSSL uses to pad a single-digit day.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_expiry_is_read_from_what_pki_verify_printed() {
        let real = " Subject:             CN = edge-01.local\n Valid From:          Aug 17 16:22:05 2026 GMT\n Valid Until:         Aug 13 16:20:17 2041 GMT\n";
        let parsed = valid_until(real).expect("an expiry");
        assert_eq!(parsed.year(), 2041);
        assert_eq!(parsed.month(), time::Month::August);
        assert_eq!(parsed.day(), 13);

        let padded = " Valid Until:         Aug  3 16:20:17 2041 GMT";
        assert_eq!(valid_until(padded).expect("an expiry").day(), 3);

        // No line, or one that is not a date: "cannot tell", which the caller treats as due.
        assert_eq!(valid_until("OK: signed by CA"), None);
        assert_eq!(valid_until(" Valid Until:         whenever"), None);
    }

    /// ADR-draft-icinga-2's renewal, and the two things that make it a renewal rather than an
    /// enrolment: the key is kept — it is what authenticates the request — and no ticket is used.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn a_certificate_near_expiry_is_renewed_without_a_new_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = enrolling(dir.path());
        ensure_enrolled(&layout).await.expect("enrol");
        let key = layout.certs_dir().join("edge-01.key");
        std::fs::write(&key, "the-key-that-authenticates-the-renewal").expect("key");
        let requests = || {
            std::fs::read_to_string(format!("{}.requests", layout.certificate().display()))
                .expect("the counter")
        };
        assert_eq!(requests(), "1", "the first enrolment made one request");

        // A certificate inside its renewal window: the stub prints an expiry five days out.
        std::fs::write(layout.certificate(), "expiring").expect("write");
        assert!(
            ensure_enrolled(&layout).await.expect("renew"),
            "a certificate near expiry is renewed"
        );
        assert_eq!(requests(), "2", "the renewal asked the parent again");
        assert_eq!(
            std::fs::read_to_string(&key).expect("key"),
            "the-key-that-authenticates-the-renewal",
            "the key is kept: overwriting it would destroy what proves the renewal"
        );

        // And once renewed, nothing runs again.
        assert!(!ensure_enrolled(&layout).await.expect("settled"));
        assert_eq!(requests(), "2");
    }

    /// A renewal whose named parent certificate is gone waits for it too: the pin kept from the
    /// first enrolment is not reused and the parent is not trusted on sight, while the held
    /// certificate is kept untouched.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn a_renewal_whose_named_parent_certificate_is_gone_waits_for_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = enrolling(dir.path());
        let named = dir.path().join("config/parent.crt");
        std::fs::write(&named, "the-parents-own-cert").expect("deliver it");
        layout.trusted_cert_file = Some(named.clone());
        ensure_enrolled(&layout).await.expect("enrol");

        std::fs::write(layout.certificate(), "expiring").expect("write");
        std::fs::remove_file(&named).expect("no longer delivered");
        let err = ensure_enrolled(&layout)
            .await
            .expect_err("a renewal without its pin waits");
        assert!(err.contains(&named.display().to_string()), "{err}");
        assert_eq!(
            std::fs::read_to_string(layout.certificate()).expect("cert"),
            "expiring",
            "the held certificate is kept while the renewal waits"
        );
        assert_eq!(
            std::fs::read_to_string(&layout.pinned_parent).expect("pinned"),
            "the-parents-own-cert",
            "nothing the parent presented replaced the pin"
        );
    }

    /// The measured case behind ADR-draft-icinga-2's validation gate: Icinga aborts a reload it
    /// cannot validate and keeps running the old configuration, so the apply has to be refused
    /// *before* it reaches the running daemon.
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn a_configuration_icinga_refuses_does_not_reach_the_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = layout(dir.path());
        layout.program = stub();
        deliver_root(
            &layout.config_dir,
            "icinga2-conf",
            ROOT_ROLE,
            "include <itl>\n",
        );
        layout
            .validate()
            .await
            .expect("a valid configuration passes");

        std::fs::write(layout.config_dir.join("icinga2-conf"), "object INVALID\n")
            .expect("bad config");
        let err = layout.validate().await.expect_err("refused");
        assert!(err.contains("syntax error"), "{err}");
    }

    /// A configuration Icinga refused stays on disk where the fleet wrote it, so the next spawn
    /// would start it — and fail. It is not started: the gate names the refusal, until a delivery
    /// changes the configuration or one validates (ADR-draft-icinga-2 clause 9).
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn a_refused_configuration_is_not_started_at_the_next_spawn_either() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = layout(dir.path());
        layout.program = stub();
        layout.parent = None;
        deliver_root(
            &layout.config_dir,
            "icinga2-conf",
            ROOT_ROLE,
            "object INVALID\n",
        );

        let err = layout.validate().await.expect_err("refused");
        layout.record_refusal(&err);
        let reason = layout.ready().expect_err("not started");
        assert!(reason.contains("refused"), "{reason}");
        assert!(reason.contains("syntax error"), "{reason}");

        // A delivery that changed it ends the refusal; its apply validates it.
        std::fs::write(layout.config_dir.join("icinga2-conf"), "include <itl>\n").expect("fix");
        assert!(layout.ready().is_ok(), "another configuration may start");
        layout.validate().await.expect("valid");
        layout.clear_refusal();
        assert!(!layout.refused.exists());
    }

    /// ADR-0017: an offered Supervisor set is validated before a running process is touched — and
    /// what it refuses now is a block that still carries a key this kind supplies itself.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn check_refuses_a_block_that_names_a_retired_key() {
        let table: toml::Table = "main_config = \"icinga2-conf\"".parse().expect("table");
        let err = Icinga2Plugin::default()
            .check("icinga2", table)
            .expect_err("refused");
        assert!(err.contains("role"), "{err}");

        let table: toml::Table = "parent_port = 5665".parse().expect("table");
        let err = Icinga2Plugin::default()
            .check("icinga2", table)
            .expect_err("refused");
        assert!(err.contains("`parent_host` as `host:port`"), "{err}");
    }
}
