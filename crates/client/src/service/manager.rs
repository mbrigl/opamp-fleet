//! Cross-platform service lifecycle over the `service-manager` crate (ADR-0010).
//!
//! `service-manager` targets the platform's native manager (systemd, launchd, Windows SCM) behind
//! one API. This module wraps it in the project's vocabulary, parameterized by the instance name:
//! every instance is its own independently registered service, `io.opamp-fleet.client.<instance>`.
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

use super::{ServiceControl, ServiceLevel, ServiceState};
use crate::cli::InstanceName;

/// The per-instance service label (reverse-DNS, per platform conventions): the systemd unit
/// `io.opamp-fleet.client.<instance>.service`, the launchd label and plist name, the Windows
/// service name.
fn label(instance: &InstanceName) -> Result<ServiceLabel, String> {
    let qualified = format!("io.opamp-fleet.client.{instance}");
    qualified
        .parse()
        .map_err(|e| format!("cannot parse the service label {qualified}: {e}"))
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

/// Register the instance as a service running `spec.program`.
///
/// # Errors
/// Returns an error if the manager rejects the install (commonly: not running as
/// root/Administrator for a system-level service).
pub fn install(spec: &InstallSpec) -> Result<(), String> {
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
            restart_policy: RestartPolicy::OnFailure {
                delay_secs: Some(5),
                max_retries: None,
                reset_after_secs: None,
            },
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

    #[test]
    fn the_label_embeds_the_instance() {
        let label = label(&instance("prod")).expect("parse the label");
        assert_eq!(label.to_qualified_name(), "io.opamp-fleet.client.prod");
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

    #[cfg(target_os = "linux")]
    #[test]
    fn default_roots_are_per_scope_and_instance() {
        let root = default_root(ServiceLevel::System, &instance("prod")).expect("system root");
        assert_eq!(root, PathBuf::from("/var/lib/opamp-fleet/client/prod"));
        let user = default_root(ServiceLevel::User, &instance("prod")).expect("user root");
        assert!(user.ends_with("opamp-fleet/client/prod"));
    }
}
