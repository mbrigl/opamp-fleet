//! Cross-platform service lifecycle over the `service-manager` crate (ADR-0010).
//!
//! `service-manager` targets the platform's native manager (systemd, launchd, Windows SCM) behind
//! one API. This module wraps it in the project's vocabulary, parameterized by the instance name:
//! every instance is its own independently registered service, named by [`service_name`].
//! The installed program is the layout's `current` pointer, so a future self-update is a pointer
//! switch — never a re-registration.

use std::ffi::OsString;
// `Path` is used only by the Unix `default_base` variants; Windows builds with -D warnings.
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use service_manager::{
    RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceLevel as SmLevel, ServiceManager,
    ServiceStartCtx, ServiceStatus, ServiceStatusCtx, ServiceStopCtx, ServiceUninstallCtx,
};

use super::{layout, ServiceControl, ServiceLevel, ServiceState};
use crate::cli::InstanceName;

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

/// The name this instance's service is registered under — the same on every platform (ADR-0030).
///
/// `opamp-fleet-client` for the default instance, `opamp-fleet-client-<instance>` for any other:
/// most hosts run exactly one Client and should show the product's name and nothing else, and the
/// suffix appears only where an operator asked for a second one. A hyphen and not a dot, for the
/// reason [`label`] gives.
#[must_use]
pub fn service_name(instance: &InstanceName) -> String {
    if instance.as_str() == DEFAULT_INSTANCE {
        layout::COMPONENT.to_string()
    } else {
        format!("{}-{instance}", layout::COMPONENT)
    }
}

/// The instance every host has unless it asked for another — matching the CLI's default.
const DEFAULT_INSTANCE: &str = "default";

/// The name a human reads where the platform has somewhere to put one (ADR-0030): the Windows
/// services list. systemd shows the unit name as its `Description`, and a launchd job *is* its
/// label — neither has a second name to give.
#[must_use]
pub fn display_name(instance: &InstanceName) -> String {
    if instance.as_str() == DEFAULT_INSTANCE {
        "OpAMP Fleet Client".to_string()
    } else {
        format!("OpAMP Fleet Client ({instance})")
    }
}

/// What the Windows services list shows in its **Description** column — a field of its own, beside
/// the display name rather than derived from it, and left empty by everything that registers the
/// service unless it is set explicitly.
///
/// It carries no instance. The display name beside it already distinguishes those; this column
/// answers "what is this?", and the answer is the same on a host running one Client and on a host
/// running four. Windows is the only platform with somewhere to put it — systemd shows the unit
/// name as its `Description` and a launchd job has no such field at all (ADR-0030).
pub const WINDOWS_DESCRIPTION: &str = "OpAMP Fleet Client for Windows";

/// The service label, built so that **every backend renders it identically** (ADR-0030).
///
/// `service-manager` renders a label through two functions that do not agree — `{organization}-`
/// `{application}` for systemd, `{qualifier}.{organization}.{application}` for launchd and the
/// SCM — so any label with more than one part has two spellings in the field. With no qualifier
/// and no organization both reduce to the application alone, which is why this is built by hand
/// rather than parsed from a dotted string, and why the name must stay free of dots.
fn label(instance: &InstanceName) -> Result<ServiceLabel, String> {
    Ok(ServiceLabel {
        qualifier: None,
        organization: None,
        application: service_name(instance),
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
    /// The instance this service embodies.
    pub instance: InstanceName,
    /// The program to run: the layout's `current` pointer, never a version directory directly.
    pub program: PathBuf,
    /// Absolute path of the TOML configuration file (ADR-0008) the unit carries. The unit holds
    /// the *path*, never the configuration itself — one source of truth.
    pub config_path: PathBuf,
    /// Absolute state directory baked into the unit (a service's working directory is `/` or
    /// `System32`; relative paths would be meaningless).
    pub state_dir: PathBuf,
}

/// The installed command line: `run --service --config … --instance … --state-dir …`. The hidden
/// `--service` marker is what routes into the Windows SCM dispatcher; it is ignored on Unix.
fn service_args(spec: &InstallSpec) -> Vec<OsString> {
    vec![
        OsString::from("run"),
        OsString::from("--service"),
        OsString::from("--config"),
        spec.config_path.clone().into_os_string(),
        OsString::from("--instance"),
        OsString::from(spec.instance.as_str()),
        OsString::from("--state-dir"),
        spec.state_dir.clone().into_os_string(),
    ]
}

/// Register the instance as a service running `spec.program`, **including** the failure recovery
/// that makes ADR-0010's restart-on-failure real on every platform. On Windows that is a second
/// step against the SCM, because `service-manager` silently drops the policy there; on systemd and
/// launchd the manager has already written it.
///
/// # Errors
/// Returns an error if the manager rejects the install (commonly: not running as
/// root/Administrator for a system-level service), or if the recovery actions cannot be set.
pub fn install(spec: &InstallSpec) -> Result<(), String> {
    install_service(spec)?;
    // What `service-manager` does not do on Windows, done here (see `windows_config`).
    let name = service_name(&spec.instance);
    super::windows_config::configure(&name, &display_name(&spec.instance), WINDOWS_DESCRIPTION)
}

fn install_service(spec: &InstallSpec) -> Result<(), String> {
    manager(spec.level)?
        .install(ServiceInstallCtx {
            label: label(&spec.instance)?,
            program: spec.program.clone(),
            args: service_args(spec),
            contents: None,
            username: None,
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

/// Deregister the instance's service. The install layout and state are never deleted.
///
/// # Errors
/// Returns an error if the manager cannot uninstall the service.
pub fn uninstall(level: ServiceLevel, instance: &InstanceName) -> Result<(), String> {
    manager(level)?
        .uninstall(ServiceUninstallCtx {
            label: label(instance)?,
        })
        .map_err(|e| format!("cannot uninstall the service: {e}"))
}

/// The installed service of one instance, implementing the [`ServiceControl`] seam.
pub struct NativeService {
    level: ServiceLevel,
    instance: InstanceName,
}

impl NativeService {
    /// A handle to the instance's service at the given level.
    #[must_use]
    pub fn new(level: ServiceLevel, instance: InstanceName) -> Self {
        Self { level, instance }
    }
}

impl ServiceControl for NativeService {
    fn start(&self) -> Result<(), String> {
        manager(self.level)?
            .start(ServiceStartCtx {
                label: label(&self.instance)?,
            })
            .map_err(|e| format!("cannot start the service: {e}"))
    }

    fn stop(&self) -> Result<(), String> {
        manager(self.level)?
            .stop(ServiceStopCtx {
                label: label(&self.instance)?,
            })
            .map_err(|e| format!("cannot stop the service: {e}"))
    }

    fn state(&self) -> Result<ServiceState, String> {
        let status = manager(self.level)?
            .status(ServiceStatusCtx {
                label: label(&self.instance)?,
            })
            .map_err(|e| format!("cannot query the service status: {e}"))?;
        Ok(match status {
            ServiceStatus::NotInstalled => ServiceState::NotInstalled,
            ServiceStatus::Stopped(_) => ServiceState::Stopped,
            ServiceStatus::Running => ServiceState::Running,
        })
    }
}

/// The default install root for a scope and instance — the platform's data directory, per
/// instance so any number of instances coexist (ADR-0010). `--root` overrides it; no path is
/// ever fixed.
///
/// # Errors
/// Returns an error if the platform's base directory cannot be determined from the environment.
pub fn default_root(level: ServiceLevel, instance: &InstanceName) -> Result<PathBuf, String> {
    let base = default_base(level)?;
    Ok(base
        .join("opamp-fleet")
        .join("client")
        .join(instance.as_str()))
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

    fn instance(name: &str) -> InstanceName {
        crate::cli::parse_instance_name(name).expect("a valid instance name")
    }

    /// ADR-0030's whole mechanism: a label with no qualifier and no organization renders the
    /// same through *both* of the crate's functions, so systemd, launchd, and the SCM show one
    /// name. A dot anywhere in it would split the label again and undo that.
    #[test]
    fn every_backend_renders_the_same_name() {
        for (given, expected) in [
            ("default", "opamp-fleet-client"),
            ("prod", "opamp-fleet-client-prod"),
        ] {
            let label = label(&instance(given)).expect("build the label");
            assert_eq!(label.to_qualified_name(), expected, "launchd and the SCM");
            assert_eq!(label.to_script_name(), expected, "systemd");
            assert!(!expected.contains('.'), "a dot would split the label again");
        }
    }

    /// The default instance shows the product's name; a second Client says which one it is.
    #[test]
    fn the_names_a_human_reads() {
        assert_eq!(service_name(&instance("default")), "opamp-fleet-client");
        assert_eq!(service_name(&instance("prod")), "opamp-fleet-client-prod");
        assert_eq!(display_name(&instance("default")), "OpAMP Fleet Client");
        assert_eq!(display_name(&instance("prod")), "OpAMP Fleet Client (prod)");
    }

    /// The Windows Description column is its own field: a service can carry a display name and
    /// still show nothing there, which is what it did. It is one string for every instance —
    /// the display name beside it is what says *which* Client this is.
    #[test]
    fn the_windows_services_list_has_a_description_to_show() {
        assert_eq!(WINDOWS_DESCRIPTION, "OpAMP Fleet Client for Windows");
        assert!(
            !WINDOWS_DESCRIPTION.is_empty(),
            "an empty one clears the field"
        );
        for named in ["default", "prod"] {
            assert_ne!(
                WINDOWS_DESCRIPTION,
                display_name(&instance(named)),
                "the description repeats the product, it does not restate the display name"
            );
        }
    }

    #[test]
    fn the_installed_command_line_is_the_marker_plus_absolute_paths() {
        let spec = InstallSpec {
            level: ServiceLevel::System,
            instance: instance("prod"),
            program: PathBuf::from("/opt/fleet/current/client"),
            config_path: PathBuf::from("/etc/opamp/client.toml"),
            state_dir: PathBuf::from("/opt/fleet/state"),
        };
        let args = service_args(&spec);
        assert_eq!(args[0], OsString::from("run"));
        assert_eq!(args[1], OsString::from("--service"));
        assert!(args.contains(&OsString::from("/etc/opamp/client.toml")));
        assert!(args.contains(&OsString::from("prod")));
        assert!(args.contains(&OsString::from("/opt/fleet/state")));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn default_roots_are_per_scope_and_instance() {
        let root = default_root(ServiceLevel::System, &instance("prod")).expect("system root");
        assert_eq!(root, PathBuf::from("/var/lib/opamp-fleet/client/prod"));
        let user = default_root(ServiceLevel::User, &instance("prod")).expect("user root");
        assert!(user.ends_with("opamp-fleet/client/prod"));
    }
}
