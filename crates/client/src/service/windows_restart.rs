//! What `service-manager` leaves undone on Windows: the recovery actions that make ADR-0010's
//! "restart on failure" true there as well.
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
//! Everything here is a no-op on Unix.

/// Configure the platform's failure recovery for an installed service, by its qualified name.
///
/// # Errors
/// Returns an error if the service cannot be opened or reconfigured. On Unix this never fails —
/// the service manager already carries the policy.
#[cfg(not(windows))]
pub fn configure(_service_name: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn configure(service_name: &str) -> Result<(), String> {
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

    Ok(())
}
