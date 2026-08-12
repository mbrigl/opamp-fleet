//! The rights a system-scope install needs, asked for **before** anything is written.
//!
//! On Windows every part of the registration goes through the service control manager and every
//! part of it needs Administrator: `sc create` in the `service-manager` backend, the
//! `CHANGE_CONFIG` handle [`windows_config`](super::windows_config) opens for the recovery actions,
//! and the `sc config` / `sc description` calls after it. A running process cannot raise its own
//! token, so a refusal from inside the install is not something the install can do anything about —
//! ADR-0010 says such an install "must fail with a clear message", and this is where that message
//! comes from.
//!
//! It is a **capability probe, not an identity check**: opening the SCM with
//! `SC_MANAGER_CREATE_SERVICE` asks the exact question the install turns on — may this process
//! register a service? — and answers it without registering one. Reading the token's elevation
//! instead would answer a weaker and different question, because what decides the outcome is the
//! SCM's own access check against the machine's security descriptor, not the shape of the token.
//!
//! It runs before the first write because the writing comes first and `%ProgramData%` lets an
//! ordinary user create folders under it: without this check a non-elevated install staged a
//! version directory and swung the `current` junction at it, and only *then* failed at `sc create`
//! — leaving half an install behind and a UAC path in `windows_config` that was never reached.
//!
//! A no-op on Unix, where there is nothing to probe short of doing it: systemd and launchd refuse
//! at the unit write, and the install roots (`/opt`, `/var/lib`, `/Library/Application Support`)
//! are not writable without root either, so the failure is already both early and plain.

use super::ServiceLevel;

/// Fail with an actionable message if this process may not register a system service. Call this
/// before the install writes anything, which is what lets the message promise that nothing has.
///
/// # Errors
/// Returns an error if the service control manager refuses this process for want of rights, or
/// cannot be reached at all — an install that would fail either way, and better before the layout
/// exists than after.
#[cfg(not(windows))]
pub fn ensure_can_register(_level: ServiceLevel) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn ensure_can_register(level: ServiceLevel) -> Result<(), String> {
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    // The SCM has no user-scope services to probe for. `--user` on Windows is refused by the
    // manager itself, with a message of its own that says exactly that.
    if level != ServiceLevel::System {
        return Ok(());
    }

    // Connect *and* create: the two rights the registration actually exercises, asked for together
    // so an administrator is never refused by a probe narrower than the call it stands in for.
    let access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
    match ServiceManager::local_computer(None::<&str>, access) {
        Ok(_) => Ok(()),
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(ACCESS_DENIED) => {
            Err(NEEDS_ADMINISTRATOR.to_string())
        }
        Err(e) => Err(format!(
            "cannot ask the service control manager whether a service may be registered: {e}"
        )),
    }
}

/// `ERROR_ACCESS_DENIED` — the one refusal that means "elevate", matched on the number because the
/// message is localised (the report this check came from read *"OpenSCManager FEHLER 5"*).
#[cfg(any(windows, test))]
const ACCESS_DENIED: i32 = 5;

/// What an operator sees instead of a bare Win32 error: what was refused, why nothing can be done
/// about it from here, and the one thing that fixes it.
#[cfg(any(windows, test))]
const NEEDS_ADMINISTRATOR: &str = "the Windows service control manager denied access: registering \
     a machine-wide service needs Administrator, and a running process cannot raise its own \
     rights. Open a shell with \"Run as administrator\" — from PowerShell, `Start-Process \
     powershell -Verb RunAs` — and run this command again. Nothing has been installed or written.";

#[cfg(test)]
mod tests {
    use super::{ACCESS_DENIED, NEEDS_ADMINISTRATOR};

    /// The message *is* the feature: this check exists only so that a refusal says what was
    /// refused and what to do about it (ADR-0010). It must not offer `--user` as the way out —
    /// Windows has no user-scope service, so that would send an operator somewhere with no door.
    #[test]
    fn the_refusal_says_what_to_do_about_it() {
        assert!(NEEDS_ADMINISTRATOR.contains("Administrator"));
        assert!(NEEDS_ADMINISTRATOR.contains("Run as administrator"));
        assert!(
            !NEEDS_ADMINISTRATOR.contains("--user"),
            "a user-scope service is not the way out — the SCM has none"
        );
        assert_eq!(ACCESS_DENIED, 5, "ERROR_ACCESS_DENIED");
    }

    /// On Unix the check has nothing to probe and must never be the thing that stops an install —
    /// including the smoke test's, which runs as root and would otherwise be told it is not.
    #[cfg(not(windows))]
    #[test]
    fn unix_is_never_refused_by_a_probe_it_cannot_run() {
        use crate::service::ServiceLevel;

        for level in [ServiceLevel::System, ServiceLevel::User] {
            assert!(super::ensure_can_register(level).is_ok(), "{level:?}");
        }
    }
}
