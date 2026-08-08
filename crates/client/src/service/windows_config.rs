//! What `service-manager` leaves undone on Windows: the recovery actions that make ADR-0010's
//! "restart on failure" true there as well, and the display name of ADR-0030.
//!
//! On systemd and launchd the restart policy handed to `service-manager` is written straight into
//! the unit (`Restart=on-failure`) or the plist (`KeepAlive{SuccessfulExit:false}`). Its Windows
//! backend is the `sc.exe` wrapper, whose `install` matches on the policy **only to log a
//! warning** — *"sc.exe does not support automatic restart policies through 'sc create'; service
//! '…' will not restart automatically"* — and registers no recovery actions at all. A Windows
//! Client therefore never came back from any failure, which contradicts ADR-0010 on one of its
//! three platforms.
//!
//! Two calls close that, both on the `windows-service` crate already used for the SCM runtime
//! shim, so nothing new is depended on:
//!
//! 1. **`update_failure_actions`** — one `Restart` action after a delay. The SCM repeats the last
//!    action for every further failure, so a single entry with no reset period is the unbounded
//!    retry that `RestartPolicy::OnFailure { max_retries: None }` asks for.
//! 2. **`set_failure_actions_on_non_crash_failures`** — without it the recovery actions run only
//!    when the process dies *without* reporting `SERVICE_STOPPED`. A service that stops cleanly
//!    while reporting a non-zero exit code — a Client that failed to start on a bad configuration,
//!    or one deliberately ending its run — is otherwise treated as a clean stop and never
//!    restarted. The flag is false by default.
//!
//! The display name is the third gap. The backend's `sc create` sets `displayname=` to the service
//! name, so the Windows services list would read `opamp-fleet-client` where ADR-0010 promised
//! *"OpAMP Fleet Client (<instance>)"* — a promise never actually kept. It is set afterwards with
//! `sc.exe config`, deliberately **not** with the crate's `Service::change_config`: that call maps
//! onto `ChangeServiceConfigW` with every field supplied from a `ServiceInfo`, so setting one field
//! means restating the executable path, the launch arguments, the start type and the account
//! exactly as registered — and getting any of them wrong rewrites the registration rather than the
//! name. `sc config` changes the one field it is given and leaves the rest alone. This is also how
//! the service was created: the backend is the `sc.exe` wrapper.
//!
//! Everything here is a no-op on Unix.

/// Configure what the Windows backend does not: failure recovery, and the display name.
///
/// # Errors
/// Returns an error if the service cannot be opened or reconfigured. On Unix this never fails —
/// the service manager already carries the policy, and neither systemd nor launchd has a display
/// name to set.
#[cfg(not(windows))]
pub fn configure(_service_name: &str, _display_name: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn configure(service_name: &str, display_name: &str) -> Result<(), String> {
    use std::time::Duration;

    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceFailureActions,
        ServiceFailureResetPeriod,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("cannot open the service control manager: {e}"))?;
    let service = manager
        .open_service(
            service_name,
            ServiceAccess::CHANGE_CONFIG | ServiceAccess::START,
        )
        .map_err(|e| format!("cannot open the service {service_name}: {e}"))?;

    service
        .update_failure_actions(ServiceFailureActions {
            // Never reset the failure count: a Client that fails once a week must still be
            // restarted the second time, and the count is not otherwise interesting.
            reset_period: ServiceFailureResetPeriod::Never,
            reboot_msg: None,
            command: None,
            // One action, repeated by the SCM for every subsequent failure.
            actions: Some(vec![ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(super::manager::RESTART_DELAY_SECS.into()),
            }]),
        })
        .map_err(|e| format!("cannot set the recovery actions of {service_name}: {e}"))?;

    // Without this the actions above cover a crash but not a reported failure — see the module
    // documentation.
    service
        .set_failure_actions_on_non_crash_failures(true)
        .map_err(|e| {
            format!("cannot enable recovery on reported failures for {service_name}: {e}")
        })?;

    set_display_name(service_name, display_name)
}

/// The name the Windows services list shows (ADR-0030), set through `sc.exe config`.
///
/// `sc.exe` wants `displayname=` with the space **after** the equals sign: the token is the option
/// and the next argument is its value, so `displayname=x` as one word is parsed as an option nobody
/// knows and silently changes nothing.
#[cfg(windows)]
fn set_display_name(service_name: &str, display_name: &str) -> Result<(), String> {
    let output = std::process::Command::new("sc.exe")
        .args(["config", service_name, "displayname="])
        .arg(display_name)
        .output()
        .map_err(|e| format!("cannot run sc.exe to name {service_name}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot set the display name of {service_name}: sc.exe exited with {} ({})",
            output.status,
            String::from_utf8_lossy(&output.stdout).trim()
        ));
    }
    Ok(())
}
