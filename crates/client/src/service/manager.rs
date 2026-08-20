//! Cross-platform service lifecycle over the `service-manager` crate (ADR-0010).
//!
//! `service-manager` targets the platform's native manager (systemd, launchd, Windows SCM) behind
//! one API. This module wraps it in the project's vocabulary. There is **one service per build**
//! (ADR-0084): it is named after the product, takes no suffix, and nothing has to be looked up to
//! address it. The installed program is the layout's `current` pointer, so a self-update is a
//! pointer switch — never a re-registration.

use std::ffi::OsString;
// `Path` is used only by the Unix `default_base` variants; Windows builds with -D warnings.
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use service_manager::{
    RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceLevel as SmLevel, ServiceManager,
    ServiceStartCtx, ServiceStatus, ServiceStatusCtx, ServiceStopCtx, ServiceUninstallCtx,
};

use super::{ServiceControl, ServiceLevel, ServiceState};
use crate::product::{PRODUCT_DISPLAY_NAME, PRODUCT_NAME};

/// Restart after a failure, never after a clean stop (ADR-0010) — with a delay so a Client that
/// fails at startup does not spin, and no retry limit so a host recovers however long it takes.
const RESTART_POLICY: RestartPolicy = RestartPolicy::OnFailure {
    delay_secs: Some(RESTART_DELAY_SECS),
    max_retries: None,
    reset_after_secs: None,
};

/// Read by [`windows_config`](super::windows_config) so both platforms wait the same before a
/// restart — and by the service smoke test, which has to outwait it before it may conclude that a
/// killed service is not coming back (ADR-0024 widens visibility by need).
pub const RESTART_DELAY_SECS: u32 = 5;

/// The name the service is registered under — the same on every platform (ADR-0030), and the
/// **product's** name rather than the program's (ADR-0084 clause 5).
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

/// The name a human reads where the platform has somewhere to put one (ADR-0030): the Windows
/// services list. systemd shows the unit name as its `Description`, and a launchd job *is* its
/// label — neither has a second name to give.
#[must_use]
pub fn display_name() -> &'static str {
    PRODUCT_DISPLAY_NAME
}

/// What the Windows services list shows in its **Description** column — a field of its own, beside
/// the display name rather than derived from it, and left empty by everything that registers the
/// service unless it is set explicitly.
///
/// This column answers "what is this?", so it says what the program does rather than repeating the
/// display name beside it. Windows is the only platform with somewhere to put it — systemd shows
/// the unit name as its `Description` and a launchd job has no such field at all (ADR-0030).
pub const WINDOWS_DESCRIPTION: &str =
    "Places this machine under OpAMP management: connects to a Server, supervises the Agents \
     configured for this host, and updates them and itself from packages the Server offers.";

/// The service label, built so that **every backend renders it identically** (ADR-0030).
///
/// `service-manager` renders a label through two functions that do not agree — `{organization}-`
/// `{application}` for systemd, `{qualifier}.{organization}.{application}` for launchd and the
/// SCM — so any label with more than one part has two spellings in the field. With no qualifier
/// and no organization both reduce to the application alone, which is why this is built by hand
/// rather than parsed from a dotted string, and why the name must stay free of dots.
fn label() -> Result<ServiceLabel, String> {
    Ok(ServiceLabel {
        qualifier: None,
        organization: None,
        application: service_name().to_string(),
    })
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
    /// Absolute path of the TOML configuration file (ADR-0008) the unit carries. The unit holds
    /// the *path*, never the configuration itself — one source of truth.
    pub config_path: PathBuf,
    /// Absolute state directory baked into the unit (a service's working directory is `/` or
    /// `System32`; relative paths would be meaningless).
    pub state_dir: PathBuf,
    /// The account the service runs as instead of root/`LocalSystem` (ADR-0062) — already
    /// resolved by [`run_as`](super::run_as), so what arrives here exists and needs no password.
    pub run_as: Option<String>,
}

/// The installed command line: `run --service --config … --state-dir …`. The hidden `--service`
/// marker is what routes into the Windows SCM dispatcher; it is ignored on Unix.
///
/// Both paths are absolute and both are baked in, which is what makes an upgrade a re-registration
/// rather than a migration (ADR-0084 clause 8): only `ExecStart` changes.
fn service_args(spec: &InstallSpec) -> Vec<OsString> {
    vec![
        OsString::from("run"),
        OsString::from("--service"),
        OsString::from("--config"),
        spec.config_path.clone().into_os_string(),
        OsString::from("--state-dir"),
        spec.state_dir.clone().into_os_string(),
    ]
}

/// Register the service running `spec.program`, **including** the failure recovery
/// that makes ADR-0010's restart-on-failure real on every platform. On Windows that is a second
/// step against the SCM, because `service-manager` silently drops the policy there; on systemd and
/// launchd the manager has already written it.
///
/// # Errors
/// Returns an error if the manager rejects the install (commonly: not running as
/// root/Administrator for a system-level service), or if the recovery actions cannot be set.
pub fn install(spec: &InstallSpec) -> Result<(), String> {
    install_service(spec)?;
    // What `service-manager` does not do on Windows, done here (see `windows_config`) — since
    // ADR-0062 that includes the logon account, which its `sc.exe` backend ignores.
    super::windows_config::configure(
        service_name(),
        display_name(),
        WINDOWS_DESCRIPTION,
        spec.run_as.as_deref(),
    )
}

fn install_service(spec: &InstallSpec) -> Result<(), String> {
    manager(spec.level)?
        .install(ServiceInstallCtx {
            label: label()?,
            program: spec.program.clone(),
            args: service_args(spec),
            contents: None,
            // systemd `User=` / launchd `UserName` (ADR-0062). The Windows backend ignores this
            // field, which is why `install` sets the logon account through `windows_config`.
            username: spec.run_as.clone(),
            working_directory: None,
            // The Client is file-configured (ADR-0008): the unit carries the config path in the
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

/// Deregister the service. The install layout and state are never deleted.
///
/// # Errors
/// Returns an error if the manager cannot uninstall the service.
pub fn uninstall(level: ServiceLevel) -> Result<(), String> {
    manager(level)?
        .uninstall(ServiceUninstallCtx { label: label()? })
        .map_err(|e| format!("cannot uninstall the service: {e}"))
}

/// The installed service, implementing the [`ServiceControl`] seam.
pub struct NativeService {
    level: ServiceLevel,
}

impl NativeService {
    /// A handle to the service at the given level.
    #[must_use]
    pub fn new(level: ServiceLevel) -> Self {
        Self { level }
    }
}

impl ServiceControl for NativeService {
    fn start(&self) -> Result<(), String> {
        manager(self.level)?
            .start(ServiceStartCtx { label: label()? })
            .map_err(|e| format!("cannot start the service: {e}"))
    }

    fn stop(&self) -> Result<(), String> {
        manager(self.level)?
            .stop(ServiceStopCtx { label: label()? })
            .map_err(|e| format!("cannot stop the service: {e}"))
    }

    fn state(&self) -> Result<ServiceState, String> {
        let status = manager(self.level)?
            .status(ServiceStatusCtx { label: label()? })
            .map_err(|e| format!("cannot query the service status: {e}"))?;
        Ok(match status {
            ServiceStatus::NotInstalled => ServiceState::NotInstalled,
            ServiceStatus::Stopped(_) => ServiceState::Stopped,
            ServiceStatus::Running => ServiceState::Running,
        })
    }
}

/// The default **data** root for a scope: `<base>/<PRODUCT_NAME>`, where `supervisor.toml` and the
/// state directory live (ADR-0084 clause 2).
///
/// One level named after the product, and no level below it. What used to be
/// `<base>/opamp-fleet/client/<instance>` asserted `client` where the file says `supervisor` and
/// held the constant `default` in its last level; a second installation is a second build now, so
/// there is nothing left for either level to distinguish. `--data-root` overrides it, `--root`
/// collapses both halves into one directory, and no path is ever fixed.
///
/// # Errors
/// Returns an error if the platform's base directory cannot be determined from the environment.
pub fn default_root(level: ServiceLevel) -> Result<PathBuf, String> {
    Ok(per_product(default_base(level)?))
}

/// The default root of the executable layout — `versions/` and the `current` pointer — for a
/// scope.
///
/// **Linux at system scope is the only place this differs from the data root** (ADR-0084 clause 3,
/// carrying ADR-0053). A binary staged under `/var/lib` carries the SELinux type `var_lib_t`,
/// which systemd may never execute: the service would register cleanly and then die at its first
/// start with `status=203/EXEC` on every enforcing host (Fedora, RHEL, SUSE 16). The layout
/// therefore lives under `/opt`, whose `usr_t` label is an entrypoint type systemd runs
/// third-party services from, and which every version the self-update stages later inherits.
///
/// Everywhere else — macOS, Windows, and every user scope — layout and data share
/// [`default_root`]. Windows does not split even under the MSI: `Program Files` holds the
/// installer's payload and nothing the daemon rewrites, because the self-update means the
/// service's own account must be able to write `versions/` and `current` (ADR-0084 clause 12).
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

/// `<base>/<PRODUCT_NAME>` — one level, named after the product (ADR-0084 clause 2).
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

    /// ADR-0030's whole mechanism: a label with no qualifier and no organization renders the
    /// same through *both* of the crate's functions, so systemd, launchd, and the SCM show one
    /// name. A dot anywhere in it would split the label again and undo that.
    #[test]
    fn every_backend_renders_the_same_name() {
        let label = label().expect("build the label");
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

    /// The service carries the **product's** name and the program carries its own (ADR-0084
    /// clause 9). They are separate constants holding different strings, and the day someone
    /// derives one from the other, one published package Set stops serving every variant.
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
    /// tell apart and nothing for a verb to look up (ADR-0084 clauses 5 and 7).
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
    /// read correctly for the next variant build (ADR-0084 clause 6).
    #[test]
    fn the_names_a_human_reads() {
        assert_eq!(display_name(), PRODUCT_DISPLAY_NAME);
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

    #[test]
    fn the_installed_command_line_is_the_marker_plus_absolute_paths() {
        let spec = InstallSpec {
            level: ServiceLevel::System,
            program: PathBuf::from("/opt/opamp-fleet/current/supervisor"),
            config_path: PathBuf::from("/var/lib/opamp-fleet/supervisor.toml"),
            state_dir: PathBuf::from("/var/lib/opamp-fleet/state"),
            run_as: Some("opamp-fleet".to_string()),
        };
        let args = service_args(&spec);
        assert!(
            !args.contains(&OsString::from("opamp-fleet")),
            "the account decides who runs the command, it is never part of it (ADR-0062)"
        );
        assert!(
            !args.contains(&OsString::from("--instance")),
            "the flag is removed, not hidden (ADR-0084 clause 7)"
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
            other => panic!("the Client restarts only on failure (ADR-0010), got {other:?}"),
        }
    }

    /// One level under the platform's base, named after the product — no `client` level asserting
    /// a name the file contradicts, and no level holding the constant `default` (ADR-0084).
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

    /// ADR-0084 clause 3, carrying ADR-0053: a system service's binary must not live under
    /// `/var/lib` — SELinux's `var_lib_t` is no entrypoint type, and the service would fail its
    /// first start with `status=203/EXEC` on every enforcing host. The executable layout defaults
    /// to `/opt`; the data root stays under `/var/lib`, so an upgrade re-registers and moves no
    /// state. User scope has no such constraint and keeps one root for both.
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

    /// Linux at system scope is the *only* split (ADR-0084 clause 3). Windows keeps one root
    /// however it was installed, because a layout the daemon rewrites cannot live in the tree the
    /// installer owns — see clause 12 and the `--run-as` hand-over.
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
