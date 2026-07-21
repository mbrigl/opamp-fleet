//! Cross-platform service lifecycle over the `service-manager` crate (ADR-0006).
//!
//! `service-manager` targets the platform's native manager (systemd, launchd, Windows SCM, rc.d,
//! OpenRC) behind one API. This module wraps it in the project's vocabulary and installs the service
//! so that ADR-0007's self-update is a pointer switch: the installed program is the current binary
//! (a `current` pointer once ADR-0007 lands), started with the `run --service` marker argument.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};
use service_manager::{
    RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceLevel as SmLevel, ServiceManager,
    ServiceStartCtx, ServiceStopCtx, ServiceUninstallCtx,
};

use super::runtime::RuntimeConfig;
use super::{ServiceControl, ServiceLevel, ServiceState};

/// The service label, in specification vocabulary (reverse-DNS, per platform conventions).
const SERVICE_LABEL: &str = "io.opamp-fleet.supervisor-host";

/// The captured runtime configuration a `service install` bakes into the unit, as `OPAMP_*` env vars
/// so the installed service is self-contained.
fn environment(config: &RuntimeConfig) -> Vec<(String, String)> {
    vec![
        ("OPAMP_SERVER_URL".to_string(), config.endpoint.clone()),
        (
            "OPAMP_STATE_DIR".to_string(),
            config.state_dir.display().to_string(),
        ),
        (
            "OPAMP_POLL_SECONDS".to_string(),
            config.poll.as_secs().to_string(),
        ),
    ]
}

fn label() -> Result<ServiceLabel> {
    SERVICE_LABEL
        .parse()
        .with_context(|| format!("parsing the service label {SERVICE_LABEL}"))
}

/// Build the native service manager, selecting user-level when requested.
fn manager(level: ServiceLevel) -> Result<Box<dyn ServiceManager>> {
    let mut manager =
        <dyn ServiceManager>::native().context("detecting the native service manager")?;
    if level == ServiceLevel::User {
        manager
            .set_level(SmLevel::User)
            .context("selecting a user-level service (not supported on this platform)")?;
    }
    Ok(manager)
}

/// The arguments the installed service is started with: `run --service`. The `--service` marker is
/// what routes into the Windows SCM dispatcher; it is harmless (ignored) on Unix.
fn service_args() -> Vec<OsString> {
    vec![OsString::from("run"), OsString::from("--service")]
}

/// Register the Supervisor Host as a service, capturing `config` into the unit.
///
/// # Errors
/// Returns an error if the current executable cannot be resolved or the manager rejects the install
/// (commonly: not running as root/Administrator for a system-level service).
pub fn install(level: ServiceLevel, config: &RuntimeConfig) -> Result<()> {
    let program: PathBuf =
        std::env::current_exe().context("resolving the current executable path")?;
    manager(level)?
        .install(ServiceInstallCtx {
            label: label()?,
            program,
            args: service_args(),
            contents: None,
            username: None,
            working_directory: None,
            environment: Some(environment(config)),
            autostart: true,
            // Restart only on a crash (non-zero exit), never after an explicit stop — this is what
            // lets ADR-0007's Updater stop the service, swap the binary, and start it again without
            // the manager racing a restart in between (ADR-0006 stop semantics).
            restart_policy: RestartPolicy::OnFailure {
                delay_secs: Some(5),
                max_retries: None,
                reset_after_secs: None,
            },
        })
        .context("installing the service (a system-level install needs root/Administrator)")?;
    Ok(())
}

/// Deregister the service.
///
/// # Errors
/// Returns an error if the manager cannot uninstall the service.
pub fn uninstall(level: ServiceLevel) -> Result<()> {
    manager(level)?
        .uninstall(ServiceUninstallCtx { label: label()? })
        .context("uninstalling the service")?;
    Ok(())
}

/// A handle to the installed service at a given level, implementing the [`ServiceControl`] seam.
pub struct NativeService {
    level: ServiceLevel,
}

impl NativeService {
    /// Create a handle for the service at the given level.
    #[must_use]
    pub fn new(level: ServiceLevel) -> Self {
        Self { level }
    }
}

impl ServiceControl for NativeService {
    fn start(&self) -> Result<()> {
        manager(self.level)?
            .start(ServiceStartCtx { label: label()? })
            .context("starting the service")?;
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        manager(self.level)?
            .stop(ServiceStopCtx { label: label()? })
            .context("stopping the service")?;
        Ok(())
    }

    fn state(&self) -> Result<ServiceState> {
        use service_manager::{ServiceStatus, ServiceStatusCtx};
        let status = manager(self.level)?
            .status(ServiceStatusCtx { label: label()? })
            .context("querying the service status")?;
        Ok(match status {
            ServiceStatus::NotInstalled => ServiceState::NotInstalled,
            ServiceStatus::Stopped(_) => ServiceState::Stopped,
            ServiceStatus::Running => ServiceState::Running,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sample_config() -> RuntimeConfig {
        RuntimeConfig {
            endpoint: "http://example:4320/v1/opamp".to_string(),
            state_dir: PathBuf::from("/var/lib/opamp"),
            poll: Duration::from_secs(15),
        }
    }

    #[test]
    fn label_parses() {
        assert!(label().is_ok());
    }

    #[test]
    fn service_is_started_with_the_scm_marker() {
        let args = service_args();
        assert_eq!(
            args,
            vec![OsString::from("run"), OsString::from("--service")]
        );
    }

    #[test]
    fn captured_environment_carries_resolved_config() {
        let env = environment(&sample_config());
        assert!(env.contains(&(
            "OPAMP_SERVER_URL".to_string(),
            "http://example:4320/v1/opamp".to_string()
        )));
        assert!(env.contains(&("OPAMP_POLL_SECONDS".to_string(), "15".to_string())));
    }
}
