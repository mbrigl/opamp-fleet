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
//! The **description** is a fourth, and is not the display name under another word: the services
//! list has a Description column of its own, nothing that registers the service fills it, and a
//! service with a display name still shows an empty one. It is set the same way, with the one
//! `sc.exe` verb that carries it.
//!
//! Everything here is a no-op on Unix.

/// Configure what the Windows backend does not: failure recovery, the display name, and the
/// description.
///
/// # Errors
/// Returns an error if the service cannot be opened or reconfigured. On Unix this never fails —
/// the service manager already carries the policy, and neither systemd nor launchd has a display
/// name or a description column to fill.
#[cfg(not(windows))]
pub fn configure(
    _service_name: &str,
    _display_name: &str,
    _description: &str,
) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn configure(service_name: &str, display_name: &str, description: &str) -> Result<(), String> {
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

    // The name the services list shows (ADR-0030). `sc.exe` wants `displayname=` with the space
    // **after** the equals sign: the token is the option and the *next* argument is its value, so
    // `displayname=x` as one word is parsed as an option nobody knows and silently changes nothing.
    sc(
        &["config", service_name, "displayname="],
        display_name,
        &format!("display name of {service_name}"),
    )?;

    // The Description column beside it. `sc description` takes its text **positionally** — there is
    // no `description=` token, and adding one would write the token into the field. The two verbs
    // disagreeing about this is exactly why each call says which shape it uses.
    sc(
        &["description", service_name],
        description,
        &format!("description of {service_name}"),
    )
}

/// `sc.exe` exits with the Win32 error code, and this is the one worth a second attempt:
/// `ERROR_ACCESS_DENIED`. Matched on the number rather than on the message, which is localised.
#[cfg(windows)]
const ACCESS_DENIED: i32 = 5;

/// Runs `sc.exe` with a trailing value argument — the shape both fields above are set with, so
/// neither can be the one that forgets to check whether `sc.exe` actually accepted it.
///
/// **A refusal for want of rights is retried elevated**, which is the only way to ask for them: a
/// running process cannot raise its own token, so UAC is reached by starting a *new* process with
/// the `runas` verb. In the ordinary install this never happens — `sc create` and the
/// `CHANGE_CONFIG` handle above both need Administrator and fail first — so the retry is what
/// covers a service whose registration succeeded and whose reconfiguration is nevertheless
/// refused, rather than the everyday path.
///
/// `what` names the field for the error, because `sc.exe` reports failure through an exit code and
/// a line on stdout rather than through anything a caller could tell apart.
#[cfg(windows)]
fn sc(args: &[&str], value: &str, what: &str) -> Result<(), String> {
    let output = std::process::Command::new("sc.exe")
        .args(args)
        .arg(value)
        .output()
        .map_err(|e| format!("cannot run sc.exe to set the {what}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    if output.status.code() == Some(ACCESS_DENIED) {
        return sc_elevated(args, value).map_err(|e| format!("cannot set the {what}: {e}"));
    }
    Err(format!(
        "cannot set the {what}: sc.exe exited with {} ({})",
        output.status,
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

/// The same call again, through the UAC prompt.
///
/// `ShellExecuteEx` with the `runas` verb is what raises that prompt, and PowerShell's
/// `Start-Process -Verb RunAs` is that call — reachable on every Windows this Client supports and
/// without binding a Win32 API this project has no other use for. `-Wait -PassThru` is what makes
/// the elevated child's exit code observable at all; without it the prompt would be answered and
/// the outcome lost.
///
/// A declined prompt is a terminating error under `$ErrorActionPreference = 'Stop'`, so PowerShell
/// exits non-zero and it reads as the refusal it is rather than as a silent success.
#[cfg(windows)]
fn sc_elevated(args: &[&str], value: &str) -> Result<(), String> {
    let list = args
        .iter()
        .copied()
        .chain(std::iter::once(value))
        .map(quote_for_powershell)
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "$ErrorActionPreference = 'Stop'; \
         $p = Start-Process -FilePath 'sc.exe' -ArgumentList {list} -Verb RunAs -Wait -PassThru; \
         exit $p.ExitCode"
    );
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command"])
        .arg(&script)
        .output()
        .map_err(|e| format!("cannot run powershell.exe to ask for Administrator rights: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "sc.exe was refused, and the elevated retry exited with {} — run this as Administrator \
         ({})",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// One PowerShell single-quoted string. Inside those, `'` is the only character with a meaning, and
/// it is escaped by doubling — so a display name carrying an apostrophe stays one argument instead
/// of ending the string and becoming script.
///
/// Single quotes throughout are also what keeps the whole script free of `"`: it is handed to
/// `powershell.exe` as one argument, and Rust quotes that with `"`, so a double quote inside would
/// have to survive two levels of escaping to arrive intact.
///
/// Compiled into test builds everywhere, because this is the one part of the elevated path that
/// can be checked on a machine that is not Windows.
#[cfg(any(windows, test))]
fn quote_for_powershell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::quote_for_powershell;

    /// The elevated retry hands `sc.exe`'s arguments to PowerShell as script text, so a value that
    /// closes its own quote stops being an argument and becomes code. The service name follows the
    /// ADR-0010 grammar and cannot do that, but the display name is prose and could.
    #[test]
    fn a_value_cannot_break_out_of_its_quotes() {
        assert_eq!(
            quote_for_powershell("OpAMP Fleet Client"),
            "'OpAMP Fleet Client'"
        );
        assert_eq!(quote_for_powershell("displayname="), "'displayname='");
        assert_eq!(
            quote_for_powershell("Bob's Client"),
            "'Bob''s Client'",
            "an apostrophe is doubled, not left to end the string"
        );
        assert_eq!(
            quote_for_powershell("'; Remove-Item C:\\ -Recurse; '"),
            "'''; Remove-Item C:\\ -Recurse; '''",
            "every quote is doubled, so the payload stays one string argument"
        );
        // Nothing needs a double quote, which is what keeps the script survivable through Rust's
        // own argument quoting.
        assert!(!quote_for_powershell("a\"b").contains('\\'));
    }
}
