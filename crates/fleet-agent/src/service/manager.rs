//! Cross-platform service lifecycle over the `service-manager` crate (ADR-0019).
//!
//! `service-manager` targets the platform's native manager (systemd, launchd, Windows SCM) behind
//! one API. This module wraps it in the project's vocabulary. There is **one service per build**
//! (ADR-0019): it is named after the product, takes no suffix, and nothing has to be looked up to
//! address it. Where a process account is registered, a second service, `<PRODUCT_NAME>-agents`,
//! runs beside it under that account (ADR-0053), and every verb acts on both. The installed
//! program is the layout's `current` pointer, so a self-update is a pointer switch — never a
//! re-registration.

use std::ffi::OsString;
// `Path` is used only by the Unix `default_base` variants; Windows builds with -D warnings.
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use service_manager::{
    RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceLevel as SmLevel, ServiceManager,
    ServiceManagerKind, ServiceStartCtx, ServiceStatus, ServiceStatusCtx, ServiceStopCtx,
    ServiceUninstallCtx,
};

use super::{ServiceControl, ServiceLevel, ServiceState};
use crate::product::{PRODUCT_NAME, PRODUCT_TITLE};

/// Restart after a failure, never after a clean stop (ADR-0019) — with a delay so a Client that
/// fails at startup does not spin, and no retry limit so a host recovers however long it takes.
const RESTART_POLICY: RestartPolicy = RestartPolicy::OnFailure {
    delay_secs: Some(RESTART_DELAY_SECS),
    max_retries: None,
    reset_after_secs: None,
};

/// Read by [`windows_config`](super::windows_config) so both platforms wait the same before a
/// restart — and by the service smoke test, which has to outwait it before it may conclude that a
/// killed service is not coming back (ADR-0010 widens visibility by need).
pub const RESTART_DELAY_SECS: u32 = 5;

/// The name the service is registered under — the same on every platform (ADR-0019), and the
/// **product's** name rather than the program's (ADR-0019 clause 3).
///
/// There is no suffix and nothing to look up: one build installs one service, and a second
/// installation is a second build with its own `PRODUCT_NAME`. The name that identifies an
/// *installation* is the product's; the program inside it is `supervisor` either way, which is why
/// this is not [`layout::COMPONENT`]. A hyphen and never a dot, for the reason [`label`] gives —
/// the grammar `build.rs` enforces guarantees both.
#[must_use]
pub fn service_name() -> &'static str {
    PRODUCT_NAME
}

/// The name of the second service, which starts the Managed Processes under the process account
/// (ADR-0053 clause 2): the product's, with the suffix `-agents`.
#[must_use]
pub fn agents_service_name() -> String {
    format!("{PRODUCT_NAME}-agents")
}

/// The second service's socket, in the runtime directory systemd creates for it, named after it
/// (`RuntimeDirectory=`), and removes when it stops.
#[must_use]
pub fn agents_socket() -> PathBuf {
    PathBuf::from("/run")
        .join(agents_service_name())
        .join("agents.sock")
}

/// How long systemd waits for the second service to stop before it kills it: longer than the
/// service's own grace for its programs and the drain of their output after it, so a stop ends
/// them through the service rather than through the manager.
pub const AGENTS_STOP_TIMEOUT_SECS: u32 = 30;

/// The name a human reads where the platform has somewhere to put one (ADR-0019): the Windows
/// services list. systemd shows the unit name as its `Description`, and a launchd job *is* its
/// label — neither has a second name to give.
#[must_use]
pub fn display_name() -> &'static str {
    PRODUCT_TITLE
}

/// What the Windows services list shows in its **Description** column — a field of its own, beside
/// the display name rather than derived from it, and left empty by everything that registers the
/// service unless it is set explicitly.
///
/// This column answers "what is this?", so it says what the program does rather than repeating the
/// display name beside it. Windows is the only platform with somewhere to put it — systemd shows
/// the unit name as its `Description` and a launchd job has no such field at all (ADR-0019).
pub const WINDOWS_DESCRIPTION: &str =
    "Places this machine under OpAMP management: connects to a Server, supervises the Agents \
     configured for this host, and updates them and itself from packages the Server offers.";

/// The service label, built so that **every backend renders it identically** (ADR-0019).
///
/// `service-manager` renders a label through two functions that do not agree — `{organization}-`
/// `{application}` for systemd, `{qualifier}.{organization}.{application}` for launchd and the
/// SCM — so any label with more than one part has two spellings in the field. With no qualifier
/// and no organization both reduce to the application alone, which is why this is built by hand
/// rather than parsed from a dotted string, and why the name must stay free of dots.
fn label() -> ServiceLabel {
    ServiceLabel {
        qualifier: None,
        organization: None,
        application: service_name().to_string(),
    }
}

/// The second service's label, built as [`label`] is.
fn agents_label() -> ServiceLabel {
    ServiceLabel {
        qualifier: None,
        organization: None,
        application: agents_service_name(),
    }
}

/// Build the native service manager, selecting user-level when requested.
fn manager(level: ServiceLevel) -> Result<Box<dyn ServiceManager>, String> {
    let mut manager = <dyn ServiceManager>::native()
        .map_err(|e| format!("cannot detect the native service manager: {e}"))?;
    if level == ServiceLevel::User {
        manager
            .set_level(SmLevel::User)
            .map_err(|e| format!("user-level services are not supported here: {e}"))?;
    }
    Ok(manager)
}

/// Everything a `service install` registers.
#[derive(Debug)]
pub struct InstallSpec {
    /// System or user scope.
    pub level: ServiceLevel,
    /// The program to run: the layout's `current` pointer, never a version directory directly.
    pub program: PathBuf,
    /// Absolute path of the TOML configuration file (ADR-0010) the unit carries. The unit holds
    /// the *path*, never the configuration itself — one source of truth.
    pub config_path: PathBuf,
    /// Absolute state directory baked into the unit (a service's working directory is `/` or
    /// `System32`; relative paths would be meaningless).
    pub state_dir: PathBuf,
    /// The account the service runs as (ADR-0042 clause 2) — already resolved by
    /// [`run_as`](super::run_as), so what arrives here exists and needs no password. On Linux and
    /// macOS a system registration always names one, `root` included; on Windows `None` leaves the
    /// logon account the SCM holds, `LocalSystem` on a first registration.
    pub run_as: Option<String>,
    /// The second service, where a process account is registered (ADR-0053); already decided and
    /// checked by [`run_as`](super::run_as).
    pub agents: Option<Agents>,
}

/// The second service a registration names (ADR-0053 clause 2).
#[derive(Debug, Clone)]
pub struct Agents {
    /// The process account it runs under.
    pub account: String,
    /// That account's uid, which the Client accepts as the socket's peer.
    pub uid: u32,
    /// The uid of the Client's account, the one peer the second service serves.
    pub client_uid: u32,
}

/// The installed command line: `run --service --config … --state-dir …`. The hidden `--service`
/// marker is what routes into the Windows SCM dispatcher; it is ignored on Unix.
///
/// Both paths are absolute and both are baked in, which is what makes an upgrade a re-registration
/// rather than a migration (ADR-0042 clause 1): the program path and the rest of the unit are
/// rewritten from the version that registers, and the configuration, identity and state stay
/// where they are.
///
/// Where a process account is registered, the line also names the second service's socket and the
/// process account's uid: that is how the running Client learns that it starts every Managed
/// Process through the second service, and whom to accept as its peer (ADR-0053 clauses 3, 4).
fn service_args(spec: &InstallSpec) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("run"),
        OsString::from("--service"),
        OsString::from("--config"),
        spec.config_path.clone().into_os_string(),
        OsString::from("--state-dir"),
        spec.state_dir.clone().into_os_string(),
    ];
    if let Some(agents) = &spec.agents {
        args.extend([
            OsString::from("--agents-socket"),
            agents_socket().into_os_string(),
            OsString::from("--process-uid"),
            OsString::from(agents.uid.to_string()),
        ]);
    }
    args
}

/// The second service's command line: `run --agents <socket> --client-uid <uid>` (ADR-0053
/// clause 2). It reads neither the configuration nor the state directory.
fn agents_args(agents: &Agents) -> Vec<OsString> {
    vec![
        OsString::from("run"),
        OsString::from("--agents"),
        agents_socket().into_os_string(),
        OsString::from("--client-uid"),
        OsString::from(agents.client_uid.to_string()),
    ]
}

/// Register the service running `spec.program`, **including** the failure recovery
/// that makes ADR-0019's restart-on-failure real on every platform. On Windows that is a second
/// step against the SCM, because `service-manager` silently drops the policy there; on systemd and
/// launchd the manager has already written it.
///
/// # Errors
/// Returns an error if the manager rejects the install (commonly: not running as
/// root/Administrator for a system-level service), or if the recovery actions cannot be set.
pub fn install(spec: &InstallSpec) -> Result<(), String> {
    // The second service first: the Client's unit names it as what it wants and starts after.
    if let Some(agents) = &spec.agents {
        install_agents(spec, agents)?;
    }
    install_service(spec)?;
    // What `service-manager` does not do on Windows, done here (see `windows_config`) — since
    // ADR-0019 that includes the logon account, which its `sc.exe` backend ignores.
    super::windows_config::configure(
        service_name(),
        display_name(),
        WINDOWS_DESCRIPTION,
        spec.run_as.as_deref(),
    )
}

fn install_service(spec: &InstallSpec) -> Result<(), String> {
    let contents = unit_contents(
        spec,
        cfg!(target_os = "linux")
            && matches!(
                ServiceManagerKind::native(),
                Ok(ServiceManagerKind::Systemd)
            ),
    )?;
    manager(spec.level)?
        .install(ServiceInstallCtx {
            label: label(),
            program: spec.program.clone(),
            args: service_args(spec),
            // The systemd system unit is the Client's own text (ADR-0042 clause 6); the user
            // scope and every other manager keep the crate's rendering.
            contents,
            // launchd `UserName` (ADR-0042 clause 2); the systemd system unit carries `User=`
            // itself. The Windows backend ignores this field, which is why `install` sets the
            // logon account through `windows_config`.
            username: spec.run_as.clone(),
            working_directory: None,
            // The Client is file-configured (ADR-0010): the unit carries the config path in the
            // arguments above, never settings as environment variables.
            environment: None,
            autostart: true,
            // Restart only on a crash, never after an explicit stop — what lets a future updater
            // stop the service, switch `current`, and start it without the manager racing it.
            //
            // Honoured natively on systemd (`Restart=on-failure`) and launchd
            // (`KeepAlive{SuccessfulExit:false}`). The Windows backend of `service-manager`
            // *discards* this and only logs a warning, which is why `windows_config` exists.
            restart_policy: RESTART_POLICY,
        })
        .map_err(|e| {
            format!("cannot install the service (system scope needs root/Administrator): {e}")
        })
}

/// Registers the second service (ADR-0053 clause 2): a systemd system unit of the Client's own text,
/// which only a host where [`agents_supported`] holds gets.
fn install_agents(spec: &InstallSpec, agents: &Agents) -> Result<(), String> {
    manager(spec.level)?
        .install(ServiceInstallCtx {
            label: agents_label(),
            program: spec.program.clone(),
            args: agents_args(agents),
            contents: Some(agents_unit(spec, agents)?),
            username: Some(agents.account.clone()),
            working_directory: None,
            environment: None,
            autostart: true,
            restart_policy: RESTART_POLICY,
        })
        .map_err(|e| format!("cannot install the second service: {e}"))
}

/// Whether this host can register the second service: a system service under systemd on Linux,
/// so far (ADR-0053).
#[must_use]
pub fn agents_supported(level: ServiceLevel) -> bool {
    level == ServiceLevel::System
        && cfg!(target_os = "linux")
        && matches!(
            ServiceManagerKind::native(),
            Ok(ServiceManagerKind::Systemd)
        )
}

/// What `service-manager` is handed as the unit's text: the Client's own systemd **system** unit
/// (ADR-0042 clause 6) where systemd is the native manager, and nothing — the crate's own
/// rendering — for the user scope and every other manager.
fn unit_contents(spec: &InstallSpec, systemd: bool) -> Result<Option<String>, String> {
    if !systemd || spec.level != ServiceLevel::System {
        return Ok(None);
    }
    systemd_unit(spec, spec.run_as.as_deref().unwrap_or(ROOT)).map(Some)
}

/// The account that is root, and that decides the unit's directive tier (ADR-0042 clause 7).
const ROOT: &str = "root";

/// What every system unit carries (ADR-0042 clause 7): `/sys/fs/cgroup` becomes read-only. It does
/// not imply `NoNewPrivileges=`, and no supported agent writes the cgroup tree itself.
const EVERY_SYSTEM_UNIT: &[&str] = &["ProtectControlGroups=yes"];

/// What a unit that runs as root carries on top (ADR-0042 clause 7). Root holds `CAP_SYS_ADMIN`,
/// so none of these implies `no_new_privs` there; under any other account each one would, and
/// `sudo` and every setuid helper a Managed Process uses would stop working.
const ROOT_UNIT: &[&str] = &[
    "ProtectKernelTunables=yes",
    "LockPersonality=yes",
    "RestrictRealtime=yes",
];

/// The systemd system unit the Client registers for `account` (ADR-0042 clauses 6 and 7).
///
/// `ExecStart=` quotes the program and every argument, so any absolute path reaches the program as
/// it was given. `Restart=` and `RestartSec=` say what [`RESTART_POLICY`] says. `User=` always
/// names the account, `root` included, so the unit records what was chosen and a re-registration
/// keeps it. What an operator adds or removes goes into a drop-in under
/// `/etc/systemd/system/<PRODUCT_NAME>.service.d/`, which nothing here touches.
///
/// # Errors
/// Returns an error if a path is not UTF-8 (a unit file is text), or if the account name holds a
/// character that would change what the unit says.
pub fn systemd_unit(spec: &InstallSpec, account: &str) -> Result<String, String> {
    use std::fmt::Write as _;
    check_unit_account(account)?;
    let exec = exec_line(&spec.program, service_args(spec))?;
    let mut unit = String::new();
    let _ = writeln!(unit, "[Unit]");
    let _ = writeln!(unit, "Description={}", service_name());
    // The second service is started first (ADR-0053 clause 2): `After=` orders both, and `Wants=`
    // pulls it in at boot and on every start of the Client. Not `Requires=`: a second service that
    // fails, or is stopped on its own, leaves the Client running — it still reports to the Server,
    // its Supervisors report that their programs do not start, and it reaches the service again
    // once the manager has restarted it.
    if spec.agents.is_some() {
        let agents = agents_service_name();
        let _ = writeln!(unit, "Wants={agents}.service");
        let _ = writeln!(unit, "After={agents}.service");
    }
    let _ = writeln!(unit);
    let _ = writeln!(unit, "[Service]");
    let _ = writeln!(unit, "ExecStart={exec}");
    let _ = writeln!(unit, "Restart=on-failure");
    let _ = writeln!(unit, "RestartSec={RESTART_DELAY_SECS}");
    let _ = writeln!(unit, "User={account}");
    let tier: &[&str] = if account == ROOT { ROOT_UNIT } else { &[] };
    for directive in EVERY_SYSTEM_UNIT.iter().chain(tier) {
        let _ = writeln!(unit, "{directive}");
    }
    let _ = writeln!(unit);
    let _ = writeln!(unit, "[Install]");
    let _ = writeln!(unit, "WantedBy=multi-user.target");
    Ok(unit)
}

/// The second service's systemd system unit (ADR-0053 clause 2): the same program as the Client's,
/// under the process account, with the directives ADR-0042 clause 7 sets for that account.
///
/// `RuntimeDirectory=` is where its socket lives: systemd creates it for the process account at
/// every start, `0750`, so only that account and its group — the Client's account is in it
/// (ADR-0053 clause 6) — reach the socket, and removes it at the stop. `TimeoutStopSec=` outlasts
/// the service's own stop of its programs.
///
/// # Errors
/// Returns an error if a path is not UTF-8, or if the account name holds a character that would
/// change what the unit says.
pub fn agents_unit(spec: &InstallSpec, agents: &Agents) -> Result<String, String> {
    use std::fmt::Write as _;
    check_unit_account(&agents.account)?;
    let exec = exec_line(&spec.program, agents_args(agents))?;
    let name = agents_service_name();
    let mut unit = String::new();
    let _ = writeln!(unit, "[Unit]");
    let _ = writeln!(unit, "Description={name}");
    let _ = writeln!(unit);
    let _ = writeln!(unit, "[Service]");
    let _ = writeln!(unit, "ExecStart={exec}");
    let _ = writeln!(unit, "Restart=on-failure");
    let _ = writeln!(unit, "RestartSec={RESTART_DELAY_SECS}");
    let _ = writeln!(unit, "User={}", agents.account);
    let _ = writeln!(unit, "RuntimeDirectory={name}");
    let _ = writeln!(unit, "RuntimeDirectoryMode=0750");
    let _ = writeln!(unit, "TimeoutStopSec={AGENTS_STOP_TIMEOUT_SECS}");
    // Never root (ADR-0053 clause 1), so never the root tier.
    for directive in EVERY_SYSTEM_UNIT {
        let _ = writeln!(unit, "{directive}");
    }
    let _ = writeln!(unit);
    let _ = writeln!(unit, "[Install]");
    let _ = writeln!(unit, "WantedBy=multi-user.target");
    Ok(unit)
}

/// Refuses an account name that would change what a unit says.
///
/// # Errors
/// Returns the refusal, naming the account.
pub fn check_unit_account(account: &str) -> Result<(), String> {
    if account.is_empty()
        || account
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '%' | '\\' | '"'))
    {
        return Err(format!(
            "the account {account:?} cannot be written into a systemd unit"
        ));
    }
    Ok(())
}

/// `ExecStart=`'s value: the program and every argument, each quoted by [`exec_word`].
fn exec_line(program: &std::path::Path, args: Vec<OsString>) -> Result<String, String> {
    let mut exec = vec![exec_word(program.as_os_str())?];
    for arg in args {
        exec.push(exec_word(&arg)?);
    }
    Ok(exec.join(" "))
}

/// One word of `ExecStart=`, in double quotes: `\` and `"` are escaped, a control character is
/// written as a C escape, `%` as `%%` (no specifier) and `$` as `$$` (no variable).
fn exec_word(word: &std::ffi::OsStr) -> Result<String, String> {
    use std::fmt::Write as _;
    let text = word
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8, which a unit file must be", word.display()))?;
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            '$' => quoted.push_str("$$"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            c if c.is_control() => {
                let mut bytes = [0; 4];
                for byte in c.encode_utf8(&mut bytes).bytes() {
                    let _ = write!(quoted, "\\x{byte:02x}");
                }
            }
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

/// A system registration of this service as the Client wrote it: the file, and the account it
/// names (ADR-0042 clause 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The systemd unit or the launchd plist.
    pub file: PathBuf,
    /// Its `User=` or `UserName`; `None` where it names none.
    pub account: Option<String>,
}

/// The registration a re-registration finds (ADR-0042 clause 2): the file the Client wrote —
/// `/etc/systemd/system/<PRODUCT_NAME>.service` or `/Library/LaunchDaemons/<PRODUCT_NAME>.plist` —
/// never a drop-in and never the loaded state of the service manager. `None` at user scope, where
/// no account is named; on Windows, where the SCM keeps the logon account itself; and where no
/// file exists.
///
/// # Errors
/// Returns an error if the file exists and cannot be read.
pub fn registration(level: ServiceLevel) -> Result<Option<Registration>, String> {
    if level == ServiceLevel::User {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    return read_registration(
        &service_manager::systemd_global_dir_path().join(format!("{}.service", service_name())),
        unit_account,
    );
    #[cfg(target_os = "macos")]
    return read_registration(
        &Path::new("/Library/LaunchDaemons").join(format!("{}.plist", service_name())),
        plist_account,
    );
    #[cfg(windows)]
    Ok(None)
}

#[cfg(unix)]
fn read_registration(
    file: &Path,
    account: fn(&str) -> Option<String>,
) -> Result<Option<Registration>, String> {
    match std::fs::read_to_string(file) {
        Ok(text) => Ok(Some(Registration {
            file: file.to_path_buf(),
            account: account(&text),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
    }
}

/// The second service's registration as the Client wrote it (ADR-0053 clause 1):
/// `/etc/systemd/system/<PRODUCT_NAME>-agents.service` and the account its `User=` names, read
/// as [`registration`] reads the Client's. `None` wherever no second service can be registered,
/// and where no file exists.
///
/// # Errors
/// Returns an error if the file exists and cannot be read.
pub fn agents_registration(level: ServiceLevel) -> Result<Option<Registration>, String> {
    if level == ServiceLevel::User {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    return read_registration(&agents_unit_path(), unit_account);
    #[cfg(not(target_os = "linux"))]
    Ok(None)
}

#[cfg(target_os = "linux")]
fn agents_unit_path() -> PathBuf {
    service_manager::systemd_global_dir_path().join(format!("{}.service", agents_service_name()))
}

/// Whether a second service is registered at `level`.
fn agents_registered(level: ServiceLevel) -> bool {
    #[cfg(target_os = "linux")]
    return level == ServiceLevel::System && agents_unit_path().exists();
    #[cfg(not(target_os = "linux"))]
    {
        let _ = level;
        false
    }
}

/// The last `User=` of a unit's `[Service]` section; an empty one resets it, as systemd reads it.
#[cfg(any(target_os = "linux", test))]
fn unit_account(unit: &str) -> Option<String> {
    let mut section = "";
    let mut account = None;
    for line in unit.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if section == "[Service]" {
            if let Some(value) = line.strip_prefix("User=") {
                let value = value.trim();
                account = (!value.is_empty()).then(|| value.to_string());
            }
        }
    }
    account
}

/// The `UserName` of a plist as `service-manager` writes it: XML, the key followed by its string.
/// No plist parser is linked for this. The job's dictionary holds no other `UserName` key, because
/// the Client sets no environment.
#[cfg(any(target_os = "macos", test))]
fn plist_account(plist: &str) -> Option<String> {
    let (_, after) = plist.split_once("<key>UserName</key>")?;
    let value = after.trim_start().strip_prefix("<string>")?;
    let (value, _) = value.split_once("</string>")?;
    let value = value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    (!value.is_empty()).then_some(value)
}

/// Deregister the service. The install layout and state are never deleted.
///
/// The second service last, as it is stopped last (ADR-0053 clause 2), and stopped first: a
/// deregistration leaves a running service running, and one that outlived its unit would go on
/// running the Managed Processes with nothing left to stop it by.
///
/// # Errors
/// Returns an error if the manager cannot uninstall the service, or stop or uninstall the second
/// service.
pub fn uninstall(level: ServiceLevel) -> Result<(), String> {
    manager(level)?
        .uninstall(ServiceUninstallCtx { label: label() })
        .map_err(|e| format!("cannot uninstall the service: {e}"))?;
    remove_agents(level).map(|_| ())
}

/// Stops and deregisters the second service, where one is registered (ADR-0053 clause 1,
/// `--no-process-account`). Returns whether there was one.
///
/// # Errors
/// Returns an error if the manager cannot stop or uninstall it.
pub fn remove_agents(level: ServiceLevel) -> Result<bool, String> {
    if !agents_registered(level) {
        return Ok(false);
    }
    manager(level)?
        .stop(ServiceStopCtx {
            label: agents_label(),
        })
        .map_err(|e| format!("cannot stop the second service: {e}"))?;
    manager(level)?
        .uninstall(ServiceUninstallCtx {
            label: agents_label(),
        })
        .map_err(|e| format!("cannot uninstall the second service: {e}"))?;
    Ok(true)
}

/// The installed service, implementing the [`ServiceControl`] seam.
pub struct NativeService {
    level: ServiceLevel,
    /// The service manager of the host; a test's own in its place.
    manager: fn(ServiceLevel) -> Result<Box<dyn ServiceManager>, String>,
    /// Whether a second service is registered at a level.
    agents_registered: fn(ServiceLevel) -> bool,
}

impl NativeService {
    /// A handle to the service at the given level.
    #[must_use]
    pub fn new(level: ServiceLevel) -> Self {
        Self {
            level,
            manager,
            agents_registered,
        }
    }
}

/// What `start` starts: the Client alone. Where a second service is registered, the Client's unit
/// wants it (`Wants=`), so the manager starts it with the Client, and neither waits for the other
/// (ADR-0053 clause 2).
fn started() -> Vec<ServiceLabel> {
    vec![label()]
}

/// What `stop` stops, in order: the Client, then the second service where one is registered —
/// `Wants=` pulls it in on a start but takes it down on no stop (ADR-0053 clause 2).
fn stopped(agents: bool) -> Vec<ServiceLabel> {
    let mut labels = vec![label()];
    if agents {
        labels.push(agents_label());
    }
    labels
}

/// The state is the Client's.
impl ServiceControl for NativeService {
    fn start(&self) -> Result<(), String> {
        for label in started() {
            (self.manager)(self.level)?
                .start(ServiceStartCtx { label })
                .map_err(|e| format!("cannot start the service: {e}"))?;
        }
        Ok(())
    }

    fn stop(&self) -> Result<(), String> {
        let mut result = Ok(());
        // Each one even when the one before failed: a stop leaves nothing running it can stop.
        for label in stopped((self.agents_registered)(self.level)) {
            let which = if label == agents_label() {
                "the second service"
            } else {
                "the service"
            };
            let stopped = (self.manager)(self.level)?
                .stop(ServiceStopCtx { label })
                .map_err(|e| format!("cannot stop {which}: {e}"));
            if result.is_ok() {
                result = stopped;
            }
        }
        result
    }

    fn state(&self) -> Result<ServiceState, String> {
        let status = (self.manager)(self.level)?
            .status(ServiceStatusCtx { label: label() })
            .map_err(|e| format!("cannot query the service status: {e}"))?;
        Ok(match status {
            ServiceStatus::NotInstalled => ServiceState::NotInstalled,
            ServiceStatus::Stopped(_) => ServiceState::Stopped,
            ServiceStatus::Running => ServiceState::Running,
        })
    }
}

/// The default **data** root for a scope: `<base>/<PRODUCT_NAME>`, where `supervisor.toml` and the
/// state directory live (ADR-0019 clause 7).
///
/// One level named after the product, and no level below it: a second installation is a second
/// build (ADR-0019 clause 5), so there is nothing for a deeper level to distinguish. `--data-root`
/// overrides it, `--root` collapses both halves into one directory, and no path is ever fixed.
///
/// # Errors
/// Returns an error if the platform's base directory cannot be determined from the environment.
pub fn default_root(level: ServiceLevel) -> Result<PathBuf, String> {
    Ok(per_product(default_base(level)?))
}

/// The default root of the executable layout — `versions/` and the `current` pointer — for a
/// scope.
///
/// **Linux at system scope is the only place this differs from the data root** (ADR-0019 clause 8,
/// carrying ADR-0019). A binary staged under `/var/lib` carries the SELinux type `var_lib_t`,
/// which systemd may never execute: the service would register cleanly and then die at its first
/// start with `status=203/EXEC` on every enforcing host (Fedora, RHEL, SUSE 16). The layout
/// therefore lives under `/opt`, whose `usr_t` label is an entrypoint type systemd runs
/// third-party services from, and which every version the self-update stages later inherits.
///
/// Everywhere else — macOS, Windows, and every user scope — layout and data share
/// [`default_root`]. Windows does not split even under the MSI: `Program Files` holds the
/// installer's payload and nothing the daemon rewrites, because the self-update means the
/// service's own account must be able to write `versions/` and `current` (ADR-0019 clause 13).
///
/// # Errors
/// Returns an error if the platform's base directory cannot be determined from the environment.
pub fn default_layout_root(level: ServiceLevel) -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    if level == ServiceLevel::System {
        return Ok(per_product(PathBuf::from("/opt")));
    }
    default_root(level)
}

/// `<base>/<PRODUCT_NAME>` — one level, named after the product (ADR-0019 clause 7).
fn per_product(base: PathBuf) -> PathBuf {
    base.join(PRODUCT_NAME)
}

#[cfg(target_os = "linux")]
fn default_base(level: ServiceLevel) -> Result<PathBuf, String> {
    match level {
        ServiceLevel::System => Ok(PathBuf::from("/var/lib")),
        ServiceLevel::User => std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))
            .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".to_string()),
    }
}

#[cfg(target_os = "macos")]
fn default_base(level: ServiceLevel) -> Result<PathBuf, String> {
    match level {
        ServiceLevel::System => Ok(PathBuf::from("/Library/Application Support")),
        ServiceLevel::User => std::env::var_os("HOME")
            .map(|home| Path::new(&home).join("Library/Application Support"))
            .ok_or_else(|| "HOME is not set".to_string()),
    }
}

#[cfg(windows)]
fn default_base(level: ServiceLevel) -> Result<PathBuf, String> {
    let var = match level {
        ServiceLevel::System => "ProgramData",
        ServiceLevel::User => "LOCALAPPDATA",
    };
    std::env::var_os(var)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{var} is not set"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::layout;

    /// ADR-0019's whole mechanism: a label with no qualifier and no organization renders the
    /// same through *both* of the crate's functions, so systemd, launchd, and the SCM show one
    /// name. A dot anywhere in it would split the label again and undo that.
    /// Verifies: ADR-0019
    #[test]
    fn every_backend_renders_the_same_name() {
        let label = label();
        assert_eq!(
            label.to_qualified_name(),
            PRODUCT_NAME,
            "launchd and the SCM"
        );
        assert_eq!(label.to_script_name(), PRODUCT_NAME, "systemd");
        assert!(
            !PRODUCT_NAME.contains('.'),
            "a dot would split the label again"
        );
    }

    /// The service carries the **product's** name and the program carries its own (ADR-0019
    /// clause 9). They are separate constants holding different strings, and the day someone
    /// derives one from the other, one published package Set stops serving every variant.
    /// Verifies: ADR-0019
    #[test]
    fn the_service_is_named_after_the_product_not_the_program() {
        assert_eq!(service_name(), PRODUCT_NAME);
        assert_eq!(service_name(), "opamp-fleet");
        assert_eq!(layout::COMPONENT, "supervisor");
        assert_ne!(
            service_name(),
            layout::COMPONENT,
            "the product names the installation, the program names the file"
        );
    }

    /// No suffix, on any platform: one build installs one service, so there is no second one to
    /// tell apart and nothing for a verb to look up (ADR-0019 clauses 3 and 6).
    /// Verifies: ADR-0019
    #[test]
    fn the_service_name_carries_no_suffix() {
        assert!(!service_name().contains('-') || service_name() == PRODUCT_NAME);
        assert_eq!(
            service_name(),
            PRODUCT_NAME,
            "whatever the product is called, the service is called exactly that"
        );
    }

    /// The display name is prose and the service name is a slug; neither is derived from the
    /// other, because no rule that produces `OpAMP Fleet Agent` from `opamp-fleet` would still
    /// read correctly for the next variant build (ADR-0019 clause 2).
    /// Verifies: ADR-0019
    #[test]
    fn the_names_a_human_reads() {
        assert_eq!(display_name(), PRODUCT_TITLE);
        assert_ne!(display_name(), service_name());
    }

    /// The Windows Description column is its own field: a service can carry a display name and
    /// still show nothing there, which is what it did. It answers "what is this?", so it must not
    /// simply repeat the display name beside it.
    #[test]
    fn the_windows_services_list_has_a_description_to_show() {
        assert!(
            !WINDOWS_DESCRIPTION.is_empty(),
            "an empty one clears the field"
        );
        assert_ne!(
            WINDOWS_DESCRIPTION,
            display_name(),
            "the description says what this is, it does not restate the display name"
        );
    }

    /// Verifies: ADR-0019
    #[test]
    fn the_installed_command_line_is_the_marker_plus_absolute_paths() {
        let spec = InstallSpec {
            level: ServiceLevel::System,
            program: PathBuf::from("/opt/opamp-fleet/current/supervisor"),
            config_path: PathBuf::from("/var/lib/opamp-fleet/supervisor.toml"),
            state_dir: PathBuf::from("/var/lib/opamp-fleet/state"),
            run_as: Some("opamp-fleet".to_string()),
            agents: None,
        };
        let args = service_args(&spec);
        assert!(
            !args.contains(&OsString::from("opamp-fleet")),
            "the account decides who runs the command, it is never part of it (ADR-0019)"
        );
        assert!(
            !args.contains(&OsString::from("--instance")),
            "the flag is removed, not hidden (ADR-0019 clause 6)"
        );
        assert_eq!(args[0], OsString::from("run"));
        assert_eq!(args[1], OsString::from("--service"));
        assert!(args.contains(&OsString::from("/var/lib/opamp-fleet/supervisor.toml")));
        assert!(args.contains(&OsString::from("/var/lib/opamp-fleet/state")));
    }

    /// The Windows recovery actions are configured separately from the policy handed to
    /// `service-manager` (see `windows_config`), so the two could drift into disagreeing about
    /// how long a failed Client waits before it comes back. They read one constant; this is what
    /// says so.
    /// Verifies: ADR-0019
    #[test]
    fn both_platforms_restart_after_the_same_delay() {
        match RESTART_POLICY {
            RestartPolicy::OnFailure {
                delay_secs,
                max_retries,
                ..
            } => {
                assert_eq!(delay_secs, Some(RESTART_DELAY_SECS));
                assert_eq!(max_retries, None, "a host recovers however long it takes");
            }
            other => panic!("the Client restarts only on failure (ADR-0019), got {other:?}"),
        }
        // The systemd system unit is the Client's own text now (ADR-0042 clause 6), so its
        // restart delay is a third place that reads the constant.
        let unit = systemd_unit(&system_spec("/opt/opamp-fleet"), "opamp-fleet").expect("unit");
        assert!(unit.lines().any(|l| l == "Restart=on-failure"), "{unit}");
        assert!(
            unit.lines()
                .any(|l| l == format!("RestartSec={RESTART_DELAY_SECS}")),
            "{unit}"
        );
    }

    fn system_spec(layout: &str) -> InstallSpec {
        InstallSpec {
            level: ServiceLevel::System,
            program: PathBuf::from(format!("{layout}/current/supervisor")),
            config_path: PathBuf::from("/var/lib/opamp-fleet/supervisor.toml"),
            state_dir: PathBuf::from("/var/lib/opamp-fleet/state"),
            run_as: Some("opamp-fleet".to_string()),
            agents: None,
        }
    }

    /// The directive name of each line of `unit` that sets one.
    fn directives(unit: &str) -> Vec<&str> {
        unit.lines()
            .filter_map(|line| line.split_once('=').map(|(key, _)| key.trim()))
            .collect()
    }

    /// The unit the Client writes holds what `service-manager` rendered before, in the Client's own
    /// text: the product's name as `Description`, a quoted `ExecStart` with the marker and absolute
    /// paths, the restart policy, the account, and the target.
    /// Verifies: ADR-0042
    #[test]
    fn the_system_unit_is_the_clients_own_text() {
        let unit = systemd_unit(&system_spec("/opt/opamp-fleet"), "opamp-fleet").expect("unit");
        let lines: Vec<&str> = unit.lines().collect();
        assert_eq!(lines[0], "[Unit]");
        assert!(lines.contains(&"Description=opamp-fleet"), "{unit}");
        assert!(lines.contains(&"[Service]"), "{unit}");
        assert!(
            lines.contains(
                &"ExecStart=\"/opt/opamp-fleet/current/supervisor\" \"run\" \"--service\" \
                  \"--config\" \"/var/lib/opamp-fleet/supervisor.toml\" \"--state-dir\" \
                  \"/var/lib/opamp-fleet/state\""
            ),
            "{unit}"
        );
        assert!(lines.contains(&"Restart=on-failure"), "{unit}");
        assert!(lines.contains(&"RestartSec=5"), "{unit}");
        assert!(lines.contains(&"User=opamp-fleet"), "{unit}");
        assert!(lines.contains(&"[Install]"), "{unit}");
        assert!(lines.contains(&"WantedBy=multi-user.target"), "{unit}");
        let service = unit.find("[Service]").expect("service section");
        let install = unit.find("[Install]").expect("install section");
        assert!(service < unit.find("ExecStart=").expect("exec") && install > service);
    }

    /// systemd splits `ExecStart=` at spaces and expands `%` and `$`, so a root holding any of them
    /// named the wrong program or the wrong file. Quoted and escaped, it reaches the program as it
    /// was given.
    /// Verifies: ADR-0042
    #[test]
    fn exec_start_survives_spaces_percent_and_dollar() {
        let unit =
            systemd_unit(&system_spec("/srv/my fleet/100%/$HOME"), "opamp-fleet").expect("unit");
        let exec = unit
            .lines()
            .find(|l| l.starts_with("ExecStart="))
            .expect("ExecStart");
        assert!(
            exec.starts_with("ExecStart=\"/srv/my fleet/100%%/$$HOME/current/supervisor\" \"run\""),
            "{exec}"
        );
        assert_eq!(
            exec_word(std::ffi::OsStr::new(r#"a\b"c"#)).expect("utf-8"),
            r#""a\\b\"c""#
        );
        assert_eq!(
            exec_word(std::ffi::OsStr::new("a\nb")).expect("utf-8"),
            "\"a\\nb\"",
            "a newline must not end the directive"
        );
    }

    /// Under an account, the unit sets nothing that implies `no_new_privs`, because that would
    /// break `sudo` and every setuid helper the agents use; it carries `ProtectControlGroups=`.
    /// Verifies: ADR-0042
    #[test]
    fn an_account_unit_sets_nothing_that_implies_no_new_privs() {
        let unit = systemd_unit(&system_spec("/opt/opamp-fleet"), "opamp-fleet").expect("unit");
        let set = directives(&unit);
        assert!(
            unit.lines().any(|l| l == "ProtectControlGroups=yes"),
            "{unit}"
        );
        for implies in [
            "NoNewPrivileges",
            "ProtectKernelTunables",
            "ProtectKernelModules",
            "ProtectKernelLogs",
            "ProtectClock",
            "ProtectHostname",
            "PrivateDevices",
            "RestrictAddressFamilies",
            "LockPersonality",
            "RestrictRealtime",
            "RestrictSUIDSGID",
            "RestrictNamespaces",
            "MemoryDenyWriteExecute",
            "SystemCallFilter",
            "SystemCallArchitectures",
            "DynamicUser",
        ] {
            assert!(!set.contains(&implies), "{implies} in {unit}");
        }
    }

    /// Under root the three directives that would imply `no_new_privs` for an account imply
    /// nothing, and the unit carries them.
    /// Verifies: ADR-0042
    #[test]
    fn a_root_unit_carries_the_root_tier() {
        let unit = systemd_unit(&system_spec("/opt/opamp-fleet"), "root").expect("unit");
        for directive in [
            "User=root",
            "ProtectControlGroups=yes",
            "ProtectKernelTunables=yes",
            "LockPersonality=yes",
            "RestrictRealtime=yes",
        ] {
            assert!(
                unit.lines().any(|l| l == directive),
                "{directive} in {unit}"
            );
        }
    }

    /// No unit, under any account, carries a directive that takes from a supported agent what it
    /// needs: writing the system for a package install, `/home` for a log receiver, a socket under
    /// `/tmp`, or root's capabilities.
    /// Verifies: ADR-0042
    #[test]
    fn no_unit_carries_what_breaks_a_supported_agent() {
        for account in ["root", "opamp-fleet"] {
            let unit = systemd_unit(&system_spec("/opt/opamp-fleet"), account).expect("unit");
            let set = directives(&unit);
            for breaks in [
                "ProtectSystem",
                "ProtectHome",
                "PrivateTmp",
                "CapabilityBoundingSet",
                "NoNewPrivileges",
                "RestrictSUIDSGID",
                "ProtectProc",
            ] {
                assert!(!set.contains(&breaks), "{breaks} under {account}: {unit}");
            }
        }
        assert!(
            systemd_unit(
                &system_spec("/opt/opamp-fleet"),
                "evil\nExecStartPre=/bin/sh"
            )
            .is_err(),
            "an account name cannot add a directive"
        );
    }

    /// The Client writes the systemd **system** unit only; the user scope and every other manager
    /// keep `service-manager`'s rendering.
    /// Verifies: ADR-0042
    #[test]
    fn a_user_unit_is_left_to_service_manager() {
        let mut spec = system_spec("/opt/opamp-fleet");
        assert!(unit_contents(&spec, true).expect("system").is_some());
        assert_eq!(unit_contents(&spec, false).expect("not systemd"), None);
        spec.level = ServiceLevel::User;
        spec.run_as = None;
        assert_eq!(unit_contents(&spec, true).expect("user"), None);
        spec.level = ServiceLevel::System;
        let unit = unit_contents(&spec, true)
            .expect("no account")
            .expect("unit");
        assert!(
            unit.lines().any(|l| l == "User=root"),
            "a system unit always names its account: {unit}"
        );
    }

    fn agents() -> Agents {
        Agents {
            account: "opamp-fleet-agents".to_string(),
            uid: 997,
            client_uid: 998,
        }
    }

    fn with_agents() -> InstallSpec {
        InstallSpec {
            agents: Some(agents()),
            ..system_spec("/opt/opamp-fleet")
        }
    }

    /// The second service is the product's name with `-agents`, one token that every backend
    /// renders alike, and its socket lives in the runtime directory named after it.
    /// Verifies: ADR-0053
    #[test]
    fn the_second_service_is_named_after_the_product() {
        assert_eq!(agents_service_name(), format!("{PRODUCT_NAME}-agents"));
        let label = agents_label();
        assert_eq!(label.to_qualified_name(), agents_service_name());
        assert_eq!(label.to_script_name(), agents_service_name());
        assert_eq!(
            agents_socket(),
            PathBuf::from("/run/opamp-fleet-agents/agents.sock")
        );
    }

    /// The second service runs the same program under the process account, as `run --agents`
    /// serving the Client's uid, with its socket in a runtime directory only the account and its
    /// group reach, and the directives ADR-0042 clause 7 gives a unit that is not root.
    /// Not on Windows, which registers no second service: the socket is a Unix path, and a Windows
    /// build joins it with its own separator.
    /// Verifies: ADR-0053
    #[cfg(unix)]
    #[test]
    fn the_second_services_unit_runs_the_program_under_the_process_account() {
        let unit = agents_unit(&with_agents(), &agents()).expect("unit");
        let lines: Vec<&str> = unit.lines().collect();
        for line in [
            "[Unit]",
            "Description=opamp-fleet-agents",
            "[Service]",
            "ExecStart=\"/opt/opamp-fleet/current/supervisor\" \"run\" \"--agents\" \
             \"/run/opamp-fleet-agents/agents.sock\" \"--client-uid\" \"998\"",
            "Restart=on-failure",
            "RestartSec=5",
            "User=opamp-fleet-agents",
            "RuntimeDirectory=opamp-fleet-agents",
            "RuntimeDirectoryMode=0750",
            "ProtectControlGroups=yes",
            "[Install]",
            "WantedBy=multi-user.target",
        ] {
            assert!(lines.contains(&line), "{line} in {unit}");
        }
        let set = directives(&unit);
        for absent in [
            "NoNewPrivileges",
            "ProtectKernelTunables",
            "LockPersonality",
            "RestrictRealtime",
            "ProtectSystem",
            "ProtectHome",
            "PrivateTmp",
            "Wants",
            "After",
        ] {
            assert!(!set.contains(&absent), "{absent} in {unit}");
        }
        let mut evil = agents();
        evil.account = "evil\nExecStartPre=/bin/sh".to_string();
        assert!(
            agents_unit(&with_agents(), &evil).is_err(),
            "an account name cannot add a directive"
        );
    }

    /// A re-registration keeps the process account the second service's unit names: what the
    /// Client writes there, it reads back.
    /// Verifies: ADR-0053
    #[test]
    fn the_second_services_account_is_read_back_from_its_unit() {
        let unit = agents_unit(&with_agents(), &agents()).expect("unit");
        assert_eq!(unit_account(&unit).as_deref(), Some("opamp-fleet-agents"));
    }

    /// systemd waits longer for the second service to stop than the service gives its programs,
    /// so a stop ends them through the service.
    /// Verifies: ADR-0053
    #[cfg(unix)]
    #[test]
    fn the_second_service_stops_before_systemd_kills_it() {
        use crate::supervisor::channel::{CLOSE_GRACE, OUTPUT_DRAIN};
        let unit = agents_unit(&with_agents(), &agents()).expect("unit");
        assert!(
            unit.lines()
                .any(|l| l == format!("TimeoutStopSec={AGENTS_STOP_TIMEOUT_SECS}")),
            "{unit}"
        );
        assert!(
            std::time::Duration::from_secs(u64::from(AGENTS_STOP_TIMEOUT_SECS))
                > CLOSE_GRACE + OUTPUT_DRAIN
        );
    }

    std::thread_local! {
        /// What the [`Recording`] service manager was asked, in order, on this test's thread.
        static ASKED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// A service manager that does nothing but note what it is asked, and fails to stop the
    /// Client's service.
    struct Recording;

    impl Recording {
        fn note(what: &str, label: &ServiceLabel) {
            ASKED.with(|asked| asked.borrow_mut().push(format!("{what} {label}")));
        }

        fn asked() -> Vec<String> {
            ASKED.with(|asked| std::mem::take(&mut *asked.borrow_mut()))
        }
    }

    impl ServiceManager for Recording {
        fn available(&self) -> std::io::Result<bool> {
            Ok(true)
        }
        fn install(&self, ctx: ServiceInstallCtx) -> std::io::Result<()> {
            Recording::note("install", &ctx.label);
            Ok(())
        }
        fn uninstall(&self, ctx: ServiceUninstallCtx) -> std::io::Result<()> {
            Recording::note("uninstall", &ctx.label);
            Ok(())
        }
        fn start(&self, ctx: ServiceStartCtx) -> std::io::Result<()> {
            Recording::note("start", &ctx.label);
            Ok(())
        }
        fn stop(&self, ctx: ServiceStopCtx) -> std::io::Result<()> {
            Recording::note("stop", &ctx.label);
            if ctx.label == label() {
                return Err(std::io::Error::other("refused"));
            }
            Ok(())
        }
        fn level(&self) -> SmLevel {
            SmLevel::System
        }
        fn set_level(&mut self, _level: SmLevel) -> std::io::Result<()> {
            Ok(())
        }
        fn status(&self, ctx: ServiceStatusCtx) -> std::io::Result<ServiceStatus> {
            Recording::note("status", &ctx.label);
            Ok(ServiceStatus::Running)
        }
    }

    fn recording(
        level: ServiceLevel,
        agents_registered: fn(ServiceLevel) -> bool,
    ) -> NativeService {
        NativeService {
            level,
            manager: |_| Ok(Box::new(Recording)),
            agents_registered,
        }
    }

    /// `start` starts the Client alone, whose unit wants the second service, and `stop` stops the
    /// Client and then the second service where one is registered — the second even when the
    /// Client's stop failed, which `stop` still reports (ADR-0053 clause 2).
    /// Verifies: ADR-0053
    #[test]
    fn start_starts_the_client_and_stop_stops_both() {
        let client = label().to_string();
        let agents = agents_label().to_string();
        let with_agents = recording(ServiceLevel::System, |_| true);
        with_agents.start().expect("start");
        assert_eq!(Recording::asked(), vec![format!("start {client}")]);
        let refused = with_agents.stop().expect_err("the Client's stop failed");
        assert!(refused.contains("cannot stop the service"), "{refused}");
        assert_eq!(
            Recording::asked(),
            vec![format!("stop {client}"), format!("stop {agents}")]
        );

        let alone = recording(ServiceLevel::System, |_| false);
        alone.start().expect("start");
        let _ = alone.stop();
        assert_eq!(
            Recording::asked(),
            vec![format!("start {client}"), format!("stop {client}")]
        );
    }

    /// Where a process account is registered, the Client's unit wants the second service and starts
    /// after it, and its command line names the socket and the process account's uid; the CLI
    /// takes both lines as written.
    /// Not on Windows, for the reason the second service's own unit is not.
    /// Verifies: ADR-0053
    #[cfg(unix)]
    #[test]
    fn the_clients_unit_names_the_second_service() {
        let unit = systemd_unit(&with_agents(), "opamp-fleet").expect("unit");
        let header = &unit[..unit.find("[Service]").expect("service section")];
        assert!(
            header
                .lines()
                .any(|l| l == "Wants=opamp-fleet-agents.service"),
            "{unit}"
        );
        assert!(
            header
                .lines()
                .any(|l| l == "After=opamp-fleet-agents.service"),
            "{unit}"
        );
        assert!(
            unit.contains(
                "\"--agents-socket\" \"/run/opamp-fleet-agents/agents.sock\" \"--process-uid\" \
                 \"997\""
            ),
            "{unit}"
        );
        let plain = systemd_unit(&system_spec("/opt/opamp-fleet"), "opamp-fleet").expect("unit");
        assert!(!plain.contains("agents"), "{plain}");

        let words = |args: Vec<OsString>| {
            std::iter::once(OsString::from("supervisor"))
                .chain(args)
                .collect::<Vec<_>>()
        };
        let client = crate::cli::parse_from(words(service_args(&with_agents())))
            .expect("the Client's line parses");
        let Some(crate::cli::Command::Run(run)) = client.cli.command else {
            panic!("run");
        };
        assert_eq!(run.agents_socket, Some(agents_socket()));
        assert_eq!(run.process_uid, Some(997));
        let second = crate::cli::parse_from(words(agents_args(&agents())))
            .expect("the second service's line parses");
        let Some(crate::cli::Command::Run(run)) = second.cli.command else {
            panic!("run");
        };
        assert_eq!(run.agents, Some(agents_socket()));
        assert_eq!(run.client_uid, Some(998));
    }

    /// A re-registration reads the account from the unit the Client wrote: the last `User=` in
    /// `[Service]`, as systemd reads it.
    /// Verifies: ADR-0042
    #[test]
    fn the_units_account_is_its_last_service_user() {
        let unit = "[Unit]\nDescription=opamp-fleet\n\n[Service]\nExecStart=/x\nUser=telegraf\n\
                    \n[Install]\nWantedBy=multi-user.target\n";
        assert_eq!(unit_account(unit).as_deref(), Some("telegraf"));
        assert_eq!(
            unit_account("[Service]\nUser=a\nUser=b\n").as_deref(),
            Some("b")
        );
        assert_eq!(unit_account("[Service]\nUser=a\nUser=\n"), None);
        assert_eq!(
            unit_account("[Unit]\nUser=a\n[Service]\nExecStart=/x\n"),
            None
        );
        assert_eq!(
            unit_account(&systemd_unit(&system_spec("/opt/opamp-fleet"), "root").expect("unit"))
                .as_deref(),
            Some("root"),
            "what the Client writes, it reads back"
        );
    }

    /// The plist's `UserName`, in the XML `service-manager` writes.
    /// Verifies: ADR-0042
    #[test]
    fn a_plists_user_name_is_kept() {
        let plist = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \
                     \"-//Apple//DTD PLIST 1.0//EN\" \
                     \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist \
                     version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>opamp-fleet</string>\
                     \n\t<key>UserName</key>\n\t<string>_opamp&amp;fleet</string>\n</dict>\n\
                     </plist>";
        assert_eq!(plist_account(plist).as_deref(), Some("_opamp&fleet"));
        assert_eq!(
            plist_account("<dict><key>Label</key><string>x</string></dict>"),
            None
        );
    }

    /// One level under the platform's base, named after the product — no `client` level asserting
    /// a name the file contradicts, and no level holding the constant `default` (ADR-0019).
    /// Verifies: ADR-0019
    #[cfg(target_os = "linux")]
    #[test]
    fn the_default_root_is_one_level_named_after_the_product() {
        let root = default_root(ServiceLevel::System).expect("system root");
        assert_eq!(root, PathBuf::from("/var/lib/opamp-fleet"));
        let user = default_root(ServiceLevel::User).expect("user root");
        assert!(user.ends_with("opamp-fleet"));
        assert!(
            !root.to_string_lossy().contains("/client/"),
            "the component level is gone"
        );
        assert!(
            !root.to_string_lossy().contains("default"),
            "the instance level is gone"
        );
    }

    /// ADR-0019 clause 8: a system service's binary must not live under
    /// `/var/lib` — SELinux's `var_lib_t` is no entrypoint type, and the service would fail its
    /// first start with `status=203/EXEC` on every enforcing host. The executable layout defaults
    /// to `/opt`; the data root stays under `/var/lib`, so an upgrade re-registers and moves no
    /// state. User scope has no such constraint and keeps one root for both.
    /// Verifies: ADR-0019
    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_system_layout_executes_from_opt() {
        let layout = default_layout_root(ServiceLevel::System).expect("layout root");
        assert_eq!(layout, PathBuf::from("/opt/opamp-fleet"));
        assert_ne!(
            layout,
            default_root(ServiceLevel::System).expect("data root"),
            "the split is the fix: data stays in /var/lib, the binary executes from /opt"
        );
        assert_eq!(
            default_layout_root(ServiceLevel::User).expect("user layout"),
            default_root(ServiceLevel::User).expect("user data"),
            "a user service runs in the user's own domain — one root for both"
        );
    }

    /// Linux at system scope is the *only* split (ADR-0019 clause 8). Windows keeps one root
    /// however it was installed, because a layout the daemon rewrites cannot live in the tree the
    /// installer owns — see clause 12 and the `--run-as` hand-over.
    /// Verifies: ADR-0019
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn no_other_platform_splits() {
        for level in [ServiceLevel::System, ServiceLevel::User] {
            assert_eq!(
                default_layout_root(level).expect("layout root"),
                default_root(level).expect("data root"),
            );
        }
    }
}
