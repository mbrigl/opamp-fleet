//! The `icinga2` plugin (ADR-0068): Icinga 2 in the Agent role, delivered as a package and run
//! out of this Supervisor's own directory.
//!
//! Icinga 2 relocates only if it is *told* to, on every invocation, and the arguments that tell it
//! are not operator choices: the account it may run under follows the Client's service account
//! (ADR-0062), the include directory follows the delivered tree, and the state directories follow
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
//! Enrolment (ADR-0069) rides in the adapter beside the Runner: the daemon stays unstarted until
//! the Icinga parent has signed this node's certificate, and an unreachable parent is a wait with
//! a reason rather than a crash loop.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;

use crate::supervisor::ports::{
    EventSender, Plugin, ProcessCommand, ProcessEvent, SupervisorContext,
};
use crate::supervisor::process::{unhealthy, Preflight, ProcessSpec, Runner, VersionProbe};
use crate::transport::Backoff;

/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per ADR-0008.
///
/// `binary` is not among them: the core takes it out and resolves it (ADR-0021), and what arrives
/// here is [`SupervisorContext::program`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Icinga2Settings {
    /// The **name of the Configuration** whose entry file is the daemon's root configuration —
    /// not a path. Icinga reads one root file and pulls the rest in with `include`, so unlike the
    /// Collector there is nothing to merge: the fleet decides which entry is the root.
    main_config: String,
    /// Where the ITL lives inside the delivered tree. Reached with `-D IncludeConfDir`, which is
    /// what `include <itl>` resolves against — `-I` does *not* override it, so a host with Icinga
    /// installed would otherwise silently use the machine's copy.
    include_dir: Option<String>,
    /// Where Icinga keeps its state — and its certificates, under `certs/`. Beside the tree by
    /// default, never inside it: a package swap replaces the tree whole.
    data_dir: Option<String>,
    log_dir: Option<String>,
    cache_dir: Option<String>,
    spool_dir: Option<String>,
    run_dir: Option<String>,
    /// Where the check plugins are, for `PluginDir`. Unset leaves the constant alone.
    plugin_dir: Option<String>,
    /// This node's common name: `NodeName`, and the name its certificate is issued for. Unset
    /// takes the Supervisor's own name, which is what the fleet already calls this Agent.
    node_name: Option<String>,
    /// The parent (master or satellite) this Agent enrols with and connects to.
    parent_host: Option<String>,
    parent_port: Option<u16>,
    /// The file holding this host's enrolment ticket — a Configuration delivered with
    /// `role = "supplementary"` and a Selector naming one Agent (ADR-0069). Absent means the
    /// signing request waits for `icinga2 ca sign` on the parent.
    ticket_file: Option<String>,
    /// The parent's certificate — **its own**, not the CA that signed it: `pki request` compares
    /// what the parent presents against this file. Pinned rather than trusted on sight; absent
    /// falls back to `pki save-cert`, which is trust on first use and is logged as such.
    trusted_cert_file: Option<String>,
    /// How long before a certificate expires it is renewed, in days. The renewal runs at start
    /// only, and never mid-run: two things renewing one file is worse than one (ADR-0069).
    renew_before_days: Option<u64>,
    /// The account the daemon may run under. Unset takes the account this Client runs as — the
    /// compiled-in `nagios` does not exist on a fleet-managed host, and *every* invocation of
    /// `icinga2`, not only the daemon, refuses without it.
    run_as_user: Option<String>,
    run_as_group: Option<String>,
    /// Console log severity (`-x`).
    log_level: Option<String>,
    /// Extra daemon arguments, verbatim and last.
    #[serde(default)]
    args: Vec<String>,
    /// Additional environment — the natural home for `LD_LIBRARY_PATH` when the tree carries its
    /// own libraries.
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// Everything the daemon needs on its command line, resolved once at start.
///
/// A struct rather than nine arguments, because it is what the tests assert against: the arguments
/// are derived, so the derivation is the thing worth checking.
#[derive(Debug, Clone)]
pub struct Layout {
    program: PathBuf,
    main_config: PathBuf,
    include_dir: Option<PathBuf>,
    plugin_dir: Option<PathBuf>,
    data_dir: PathBuf,
    log_dir: PathBuf,
    cache_dir: PathBuf,
    spool_dir: PathBuf,
    run_dir: PathBuf,
    node_name: String,
    run_as: Option<(String, String)>,
    log_level: String,
    parent: Option<(String, u16)>,
    extra: Vec<String>,
    /// Where the enrolment ticket is read from — a Configuration entry, delivered per host
    /// (ADR-0069). Absent means on-demand signing: the request waits in the parent's queue.
    ticket_file: Option<PathBuf>,
    /// The parent certificate to pin, as delivered. Copied out of `config/` at enrolment, because
    /// the next apply empties that directory.
    trusted_cert_file: Option<PathBuf>,
    /// Where the pinned parent certificate is kept, outside `config/` and outside the tree.
    pinned_parent: PathBuf,
    /// How close to expiry a certificate may come before it is renewed.
    renew_before: Duration,
    /// What was enrolled, so the ordinary start costs no subprocess. The certificate on disk is
    /// the state; this is a hint (ADR-0069).
    marker: PathBuf,
}

impl Layout {
    /// Where the enrolled certificate and key live — `DataDir/certs`, which is where the
    /// `ApiListener` looks for `<NodeName>.crt` without being told (ADR-0069).
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

    /// Why the daemon is not started yet, or `None` when it may run.
    ///
    /// Two gates, and both are honest waits rather than failures: the root configuration has to
    /// have arrived, and — with a parent configured (the Agent role) — the certificate has to have
    /// been issued. Starting without either produces a process that cannot do its job and a crash
    /// loop that says nothing about why.
    fn blocked_by(&self) -> Option<String> {
        if !self.main_config.is_file() {
            return Some(format!(
                "awaiting the configuration {}",
                self.main_config.display()
            ));
        }
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

    /// The ticket this host enrols with, read from the file the fleet delivered it in. A file that
    /// is not there is not an error: it means on-demand signing (ADR-0069).
    fn ticket(&self) -> Result<Option<String>, String> {
        let Some(path) = self.ticket_file.as_ref().filter(|p| p.is_file()) else {
            return Ok(None);
        };
        let ticket = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read the ticket at {}: {e}", path.display()))?;
        let ticket = ticket.trim().to_string();
        Ok((!ticket.is_empty()).then_some(ticket))
    }

    /// Whether the marker says this is the enrolment we want — same node, same parent. It answers
    /// *"has anything changed?"*, never *"is there a certificate?"*, which is what the file on disk
    /// answers (ADR-0069).
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
            enrolled_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
            ..Enrolment {
                common_name: what.common_name.clone(),
                parent_host: what.parent_host.clone(),
                parent_port: what.parent_port,
                enrolled_at_unix: 0,
            }
        };
        let text = serde_json::to_string_pretty(&record)
            .map_err(|e| format!("cannot record the enrolment: {e}"))?;
        std::fs::write(&self.marker, text)
            .map_err(|e| format!("cannot write {}: {e}", self.marker.display()))
    }

    /// Validates a configuration exactly as the daemon would, without touching what runs.
    ///
    /// This is the gate ADR-0068 requires: Icinga aborts a reload it cannot validate and **keeps
    /// running the old configuration**, printing the reason to stderr — so a Supervisor that
    /// forwarded the apply blindly would report `APPLIED` for a configuration that never took
    /// effect.
    async fn validate(&self) -> Result<(), String> {
        let mut args = self.daemon_args();
        // `-C` next to `daemon`, before the rest, so the same command line is exercised.
        args.insert(1, "-C".to_string());
        run_subcommand(self, &args).await.map(|_| ())
    }

    /// The daemon's argument vector. Foreground — no `-d`, no `--close-stdio` — because the Runner
    /// supervises what it started and the Client's logging carries the output (ADR-0041).
    fn daemon_args(&self) -> Vec<String> {
        let mut args = vec![
            "daemon".to_string(),
            "-c".to_string(),
            self.main_config.display().to_string(),
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
        if let Some(dir) = &self.include_dir {
            define("IncludeConfDir", dir.display().to_string());
        }
        if let Some(dir) = &self.plugin_dir {
            define("PluginDir", dir.display().to_string());
        }
        define("DataDir", self.data_dir.display().to_string());
        define("LogDir", self.log_dir.display().to_string());
        define("CacheDir", self.cache_dir.display().to_string());
        define("SpoolDir", self.spool_dir.display().to_string());
        define("InitRunDir", self.run_dir.display().to_string());
        args.push("-x".to_string());
        args.push(self.log_level.clone());
        args.extend(self.extra.iter().cloned());
        args
    }
}

/// How long any one `icinga2` subcommand may take before it is given up on. Enrolment talks to a
/// parent over the network, and a parent that accepts the connection but never answers must not
/// wedge the adapter — the version probe bounds itself the same way.
const SUBCOMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs one `icinga2` subcommand, bounded, with the constants every invocation needs.
///
/// `-D RunAsUser`/`-D RunAsGroup` are not a daemon concern: **every** subcommand drops privileges
/// to the compiled-in account first and refuses when it cannot, so `pki` needs them exactly as the
/// daemon does (ADR-0068).
async fn run_subcommand(layout: &Layout, args: &[String]) -> Result<String, String> {
    let mut command = tokio::process::Command::new(&layout.program);
    command.args(args);
    if let Some((user, group)) = &layout.run_as {
        command.args(["-D".to_string(), format!("RunAsUser={user}")]);
        command.args(["-D".to_string(), format!("RunAsGroup={group}")]);
    }
    command.kill_on_drop(true);
    let output = match tokio::time::timeout(SUBCOMMAND_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("cannot run icinga2 {}: {e}", args.join(" "))),
        Err(_) => return Err(format!("icinga2 {} timed out", args.join(" "))),
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
    Err(format!("icinga2 {} failed: {reason}", args.join(" ")))
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

/// What the enrolment marker records — a hint, never the authority (ADR-0069).
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct Enrolment {
    common_name: String,
    parent_host: String,
    parent_port: u16,
    enrolled_at_unix: u64,
}

/// Obtains a certificate for this node from its Icinga parent, once.
///
/// The order matters and is ADR-0069's: a usable certificate ends it before anything runs; the key
/// is generated locally and never travels; the parent is pinned before it is talked to; and the
/// ticket — when there is one — turns the request into an immediate signature instead of a queue
/// entry. `Ok(false)` means there was nothing to do.
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
    // itself with the certificate it already holds instead of with a ticket (ADR-0069).
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

    tracing::info!(node = %layout.node_name, parent = %host, renewing, "requesting an Icinga certificate");
    if !renewing {
        // A renewal must not do this: it would overwrite the very key and certificate that
        // authenticate the renewal, leaving a request nothing can prove (ADR-0069).
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

    // The parent is pinned from what the fleet delivered; `save-cert` is the fallback, and it is
    // trust on first use — logged, so an operator can see that it happened and against what.
    match layout.trusted_cert_file.as_ref().filter(|f| f.is_file()) {
        Some(delivered) => {
            std::fs::copy(delivered, &layout.pinned_parent).map_err(|e| {
                format!(
                    "cannot keep the parent certificate at {}: {e}",
                    layout.pinned_parent.display()
                )
            })?;
        }
        None => {
            tracing::warn!(
                parent = %host,
                "no parent certificate was delivered: trusting the one the parent presents now \
                 (trust on first use, ADR-0069)"
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
        }
    }

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
        "--ca".to_string(),
        ca.display().to_string(),
        "--trustedcert".to_string(),
        layout.pinned_parent.display().to_string(),
    ];
    match layout.ticket()?.filter(|_| !renewing) {
        Some(ticket) => request.extend(["--ticket".to_string(), ticket]),
        None if renewing => tracing::info!(
            node = %layout.node_name,
            "the certificate in force authenticates its own renewal; no ticket is used"
        ),
        None => tracing::info!(
            node = %layout.node_name,
            "no ticket: the signing request waits for `icinga2 ca sign` on the parent (ADR-0069)"
        ),
    }
    run_subcommand(layout, &request).await?;
    layout.record_enrolment(&want)?;
    tracing::info!(node = %layout.node_name, parent = %host, "the Icinga parent signed this node's certificate");
    Ok(true)
}

/// Icinga's own version banner is `icinga2 … (version: r2.14.6-1)`: an `r`, and a packaging
/// revision the strict SemVer read rejects outright. Reported as `2.14.6`, which is the version an
/// operator compares and the one a package Set is named after (ADR-0029).
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
        .map(str::to_string)
}

/// The account this Client runs as, which is the account Icinga may drop to. Resolved with `id(1)`
/// — the idiom ADR-0062's account handling already uses, rather than `getpwuid(3)` behind `unsafe`
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

pub struct Icinga2Plugin;

impl Icinga2Plugin {
    /// Resolves the block against this Supervisor's directories. Everything an operator may write
    /// goes through the placeholders of ADR-0022; the defaults are expressed in them too, so a
    /// relocated `supervisor_dir` moves the state with it.
    fn layout(ctx: &SupervisorContext, settings: &Icinga2Settings) -> Layout {
        let path = |value: &Option<String>, default: &str| -> PathBuf {
            PathBuf::from(ctx.expand(value.as_deref().unwrap_or(default)))
        };
        Layout {
            program: ctx.program.clone(),
            main_config: ctx.config_dir.join(&settings.main_config),
            include_dir: settings
                .include_dir
                .as_deref()
                .map(|dir| PathBuf::from(ctx.expand(dir))),
            plugin_dir: settings
                .plugin_dir
                .as_deref()
                .map(|dir| PathBuf::from(ctx.expand(dir))),
            data_dir: path(&settings.data_dir, "${supervisor_dir}/data"),
            log_dir: path(&settings.log_dir, "${supervisor_dir}/log"),
            cache_dir: path(&settings.cache_dir, "${supervisor_dir}/cache"),
            spool_dir: path(&settings.spool_dir, "${supervisor_dir}/spool"),
            run_dir: path(&settings.run_dir, "${supervisor_dir}/run"),
            node_name: settings
                .node_name
                .clone()
                .unwrap_or_else(|| ctx.name.clone()),
            run_as: match (&settings.run_as_user, &settings.run_as_group) {
                (Some(user), Some(group)) => Some((user.clone(), group.clone())),
                _ => current_account(),
            },
            log_level: settings
                .log_level
                .clone()
                .unwrap_or_else(|| "information".to_string()),
            parent: settings
                .parent_host
                .clone()
                .map(|host| (host, settings.parent_port.unwrap_or(DEFAULT_PARENT_PORT))),
            extra: settings.args.iter().map(|a| ctx.expand(a)).collect(),
            ticket_file: settings
                .ticket_file
                .as_deref()
                .map(|f| PathBuf::from(ctx.expand(f))),
            trusted_cert_file: settings
                .trusted_cert_file
                .as_deref()
                .map(|f| PathBuf::from(ctx.expand(f))),
            pinned_parent: ctx.supervisor_dir.join("trusted-parent.crt"),
            renew_before: Duration::from_secs(
                settings
                    .renew_before_days
                    .unwrap_or(DEFAULT_RENEW_BEFORE_DAYS)
                    * 24
                    * 60
                    * 60,
            ),
            marker: ctx.supervisor_dir.join("icinga2-enrolment.json"),
        }
    }
}

/// The port an Icinga master or satellite listens on for the cluster protocol.
const DEFAULT_PARENT_PORT: u16 = 5665;

/// How long before expiry a certificate is renewed by default. Icinga's own default validity is
/// years, and the daemon renews over its established connection; this is the start-time safety net
/// for the host that was switched off for longer than that (ADR-0069).
const DEFAULT_RENEW_BEFORE_DAYS: u64 = 30;

/// Enrols this node, retrying until it succeeds, and nudges the Runner when it does.
///
/// An unreachable parent is a *wait*, not a failure: the health says what is missing, the attempt
/// backs off, and no daemon is started — a crash loop would say nothing about why (ADR-0069). The
/// nudge is an ordinary `Restart`, because the gate this opens is the one `build()` reads.
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
/// left claiming to run something it refused (ADR-0068).
async fn intercept(
    mut from_core: mpsc::Receiver<ProcessCommand>,
    runner: mpsc::Sender<ProcessCommand>,
    layout: Layout,
    events: EventSender,
) {
    while let Some(command) = from_core.recv().await {
        let command = match command {
            ProcessCommand::ApplyConfig { config } => match layout.validate().await {
                Ok(()) => ProcessCommand::ApplyConfig { config },
                Err(e) => {
                    tracing::warn!(error = %e, "refusing a configuration Icinga 2 will not accept");
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

impl Plugin for Icinga2Plugin {
    fn kind(&self) -> &'static str {
        "icinga2"
    }

    fn program_key(&self) -> &'static str {
        "binary"
    }

    fn start(&self, mut ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        let settings: Icinga2Settings = std::mem::take(&mut ctx.settings)
            .try_into()
            .map_err(|e| format!("supervisor {:?}: {e}", ctx.name))?;
        let layout = Self::layout(&ctx, &settings);
        let env: Vec<(String, String)> = settings
            .env
            .iter()
            .map(|(k, v)| (k.clone(), ctx.expand(v)))
            .collect();
        // Two channels, not one: what the core holds goes through the validation gate first, and
        // what the Runner receives is what survived it (ADR-0068).
        let (commands, from_core) = mpsc::channel(16);
        let (to_runner, command_rx) = mpsc::channel(16);
        let name = ctx.name.clone();
        let events = ctx.events.clone();
        let gate = layout.clone();
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            retain_previous: ctx.retain_previous,
            install: Some(ctx.install),
            archive_key: ctx.archive_key.clone(),
            version_probe: Some(VersionProbe {
                program: layout.program.clone(),
                args: vec!["--version".to_string()],
                parse: Some(parse_version),
            }),
            // The delivered tree carries its own libraries, so what proves it runs must be run
            // against *those* — a version banner costs 30 ms and answers the one question a
            // repacked tree raises: does this host's libc satisfy it (ADR-0068, ADR-0070)?
            preflight: Some(Preflight {
                args: vec!["--version".to_string()],
                env: vec![("LD_LIBRARY_PATH".to_string(), "${staged}/lib".to_string())],
            }),
            // Icinga re-reads its configuration on SIGHUP and keeps the umbrella's pid, so the
            // reload of ADR-0060 applies as it stands. Windows refuses the concept, and the
            // Runner falls back to the restart there.
            reload_signal: reload_signal(),
            events: ctx.events,
            commands: command_rx,
            build: Box::new(move || match layout.blocked_by() {
                Some(reason) => {
                    tracing::debug!(supervisor = %name, reason = %reason, "not starting Icinga 2 yet");
                    None
                }
                None => match layout.prepare_dirs() {
                    Ok(()) => Some(ProcessSpec {
                        program: layout.program.clone(),
                        args: layout.daemon_args(),
                        env: env.clone(),
                        working_dir: None,
                        // Its worker must not survive the stop (ADR-0068).
                        own_process_group: true,
                    }),
                    Err(e) => {
                        tracing::warn!(supervisor = %name, error = %e, "cannot prepare Icinga 2's directories");
                        None
                    }
                },
            }),
        };
        tokio::spawn(runner.run(ctx.shutdown));
        tokio::spawn(intercept(
            from_core,
            to_runner.clone(),
            gate.clone(),
            events.clone(),
        ));
        // In the adapter, never in `start`: a parent that is down must not hold up the Client's
        // startup (ADR-0069).
        tokio::spawn(enrol(gate, to_runner, events));
        Ok(commands)
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        let settings: Icinga2Settings = settings
            .try_into()
            .map_err(|e| format!("supervisor {name:?}: {e}"))?;
        if settings.main_config.trim().is_empty() {
            return Err(format!(
                "supervisor {name:?}: `main_config` names the Configuration that is Icinga's root \
                 configuration file, and cannot be empty"
            ));
        }
        if settings.parent_host.is_none() && settings.parent_port.is_some() {
            return Err(format!(
                "supervisor {name:?}: `parent_port` without `parent_host` names no parent"
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn reload_signal() -> Option<i32> {
    Some(libc::SIGHUP)
}

#[cfg(not(unix))]
fn reload_signal() -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(toml_text: &str) -> Icinga2Settings {
        toml_text
            .parse::<toml::Table>()
            .expect("table")
            .try_into()
            .expect("settings")
    }

    fn layout(dir: &std::path::Path) -> Layout {
        Layout {
            program: dir.join("program/tree/sbin/icinga2"),
            main_config: dir.join("config/icinga2-conf"),
            include_dir: Some(dir.join("program/tree/share/icinga2/include")),
            plugin_dir: Some(dir.join("program/tree/plugins")),
            data_dir: dir.join("data"),
            log_dir: dir.join("log"),
            cache_dir: dir.join("cache"),
            spool_dir: dir.join("spool"),
            run_dir: dir.join("run"),
            node_name: "edge-01".to_string(),
            run_as: Some(("fleet".to_string(), "fleet".to_string())),
            log_level: "information".to_string(),
            parent: Some(("master.example".to_string(), 5665)),
            extra: vec!["--extra".to_string()],
            ticket_file: None,
            trusted_cert_file: None,
            pinned_parent: dir.join("trusted-parent.crt"),
            renew_before: Duration::from_secs(30 * 24 * 60 * 60),
            marker: dir.join("icinga2-enrolment.json"),
        }
    }

    /// `binary` is gone from these settings — the core resolves it (ADR-0021) — so a block that
    /// still carried it here would be an unknown key, which is exactly what must fail.
    #[test]
    fn settings_parse_strictly() {
        let parsed = settings(
            r#"
            main_config = "icinga2-conf"
            parent_host = "master.example"
            node_name = "edge-01"
            [env]
            LD_LIBRARY_PATH = "${supervisor_dir}/program/tree/lib"
            "#,
        );
        assert_eq!(parsed.main_config, "icinga2-conf");
        assert_eq!(parsed.parent_port, None);
        assert!(parsed.env.contains_key("LD_LIBRARY_PATH"));

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

    /// The three arguments an operator must never have to write, and the shape ADR-0068 fixes:
    /// the account, the include directory, and every state directory.
    #[test]
    fn the_daemon_arguments_carry_the_relocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let args = layout(dir.path()).daemon_args();
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
        // detach from it (ADR-0063's lesson, ADR-0068's requirement).
        assert!(
            !args.iter().any(|a| a == "-d" || a == "--daemonize"),
            "{joined}"
        );
        assert!(!args.iter().any(|a| a == "--close-stdio"), "{joined}");
        assert_eq!(args.last().map(String::as_str), Some("--extra"));
    }

    /// Nothing starts before the root configuration is there — and, in the Agent role, before the
    /// certificate is. Both are waits with a reason, not failures.
    #[test]
    fn the_daemon_waits_for_its_configuration_and_its_certificate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = layout(dir.path());

        let blocked = layout.blocked_by().expect("no configuration yet");
        assert!(blocked.contains("awaiting the configuration"), "{blocked}");

        std::fs::create_dir_all(dir.path().join("config")).expect("config dir");
        std::fs::write(&layout.main_config, "include <itl>\n").expect("config");
        let blocked = layout.blocked_by().expect("no certificate yet");
        assert!(blocked.contains("awaiting the certificate"), "{blocked}");

        std::fs::create_dir_all(layout.certs_dir()).expect("certs dir");
        std::fs::write(layout.certificate(), "cert").expect("cert");
        std::fs::write(layout.ca_certificate(), "ca").expect("ca");
        assert_eq!(layout.blocked_by(), None);
    }

    /// Without a parent there is no enrolment to wait for: a standalone Icinga 2 runs as soon as
    /// its configuration arrives.
    #[test]
    fn a_standalone_node_needs_no_certificate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = layout(dir.path());
        layout.parent = None;
        std::fs::create_dir_all(dir.path().join("config")).expect("config dir");
        std::fs::write(&layout.main_config, "include <itl>\n").expect("config");
        assert_eq!(layout.blocked_by(), None);
    }

    /// Icinga creates none of its directories and fails on the first write into a missing one.
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

    /// The whole of ADR-0069's happy path: a key generated here, a parent pinned, a signature — and
    /// then nothing at all, because the certificate on disk is the state.
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

    /// A parent that cannot be reached is a wait: the error names the reason, and nothing is
    /// recorded as enrolled.
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

    /// ADR-0069's renewal, and the two things that make it a renewal rather than an enrolment: the
    /// key is kept — it is what authenticates the request — and no ticket is used.
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

    /// The measured case behind ADR-0068's validation gate: Icinga aborts a reload it cannot
    /// validate and keeps running the old configuration, so the apply has to be refused *before*
    /// it reaches the running daemon.
    #[tokio::test]
    async fn a_configuration_icinga_refuses_does_not_reach_the_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = layout(dir.path());
        layout.program = stub();
        std::fs::create_dir_all(dir.path().join("config")).expect("config dir");

        std::fs::write(&layout.main_config, "include <itl>\n").expect("good config");
        layout
            .validate()
            .await
            .expect("a valid configuration passes");

        std::fs::write(&layout.main_config, "object INVALID\n").expect("bad config");
        let err = layout.validate().await.expect_err("refused");
        assert!(err.contains("syntax error"), "{err}");
    }

    /// ADR-0056: an offered Supervisor set is validated before a running process is touched.
    #[test]
    fn check_refuses_a_block_that_names_no_root_configuration() {
        let table: toml::Table = "main_config = \"\"".parse().expect("table");
        let err = Icinga2Plugin.check("icinga2", table).expect_err("refused");
        assert!(err.contains("main_config"), "{err}");

        let table: toml::Table = "main_config = \"c\"\nparent_port = 5665"
            .parse()
            .expect("table");
        let err = Icinga2Plugin.check("icinga2", table).expect_err("refused");
        assert!(err.contains("parent_port"), "{err}");
    }
}
