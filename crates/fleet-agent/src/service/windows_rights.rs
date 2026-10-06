//! The rights a system-scope install needs, asked for **before** anything is written.
//!
//! On Windows every part of the registration goes through the service control manager and every
//! part of it needs Administrator: `sc create` in the `service-manager` backend, the
//! `CHANGE_CONFIG` handle [`windows_config`](super::windows_config) opens for the recovery actions,
//! and the `sc config` / `sc description` calls after it. A running process cannot raise its own
//! token, so a refusal from inside the install is not something the install can do anything about —
//! ADR-0014 says such an install "must fail with a clear message", and this is where that message
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

/// Who keeps full control of a system-scope data root once its inherited rights are cut: LocalSystem
/// and the Administrators group, by SID so a localised group name cannot miss. The service account
/// is granted its own rights afterwards, by the handover.
#[cfg(any(windows, test))]
const KEEPERS: [&str; 2] = ["*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F"];

/// The `icacls.exe` arguments that cut `path` off from the rights it inherits and leave it to
/// [`KEEPERS`]. Without the inheritance flags propagated, what is already inside — the configuration
/// written moments ago — loses the inherited read right with its parent.
#[cfg(any(windows, test))]
fn restrict_args(path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut args = vec![path.as_os_str().to_owned(), "/inheritance:r".into()];
    for keeper in KEEPERS {
        args.push("/grant:r".into());
        args.push(keeper.into());
    }
    args
}

/// Cuts a system-scope data root off from what `%ProgramData%` lets every local user do.
///
/// Every folder created under `%ProgramData%` inherits `BUILTIN\Users:(OI)(CI)(RX)`: any local
/// user could read the configuration and the archive key it may hold, the private key and the
/// stored connection settings. On Unix the Client writes those `0600` in a `0700` directory itself;
/// here the install removes the inherited rights and leaves the directory to LocalSystem and the
/// Administrators, before the handover grants the service account its own. A user-scope install
/// lives in the user's profile, which is private already.
///
/// # Errors
/// Returns an error when `icacls.exe` cannot be run or refuses the change.
#[cfg(windows)]
pub fn restrict_data_root(level: ServiceLevel, path: &std::path::Path) -> Result<(), String> {
    if level != ServiceLevel::System {
        return Ok(());
    }
    let output = std::process::Command::new("icacls.exe")
        .args(restrict_args(path))
        .output()
        .map_err(|e| format!("cannot run icacls.exe: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "cannot restrict {} to LocalSystem and the Administrators: icacls.exe exited with {} ({})",
        path.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

/// A no-op on Unix, where the Client writes its secrets owner-only itself.
///
/// # Errors
/// Never.
#[cfg(not(windows))]
pub fn restrict_data_root(_level: ServiceLevel, _path: &std::path::Path) -> Result<(), String> {
    Ok(())
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
    use super::{restrict_args, ACCESS_DENIED, KEEPERS, NEEDS_ADMINISTRATOR};

    /// The data root keeps no inherited right — `BUILTIN\Users` read among them — and full control
    /// only for LocalSystem and the Administrators, each granted by SID.
    /// Verifies: ADR-0061
    #[test]
    fn the_data_root_is_cut_off_from_what_every_local_user_inherits() {
        let args: Vec<String> = restrict_args(std::path::Path::new(r"C:\ProgramData\opamp-fleet"))
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], r"C:\ProgramData\opamp-fleet");
        assert_eq!(
            args[1], "/inheritance:r",
            "inherited rights are removed, not kept"
        );
        assert_eq!(&args[2..], ["/grant:r", KEEPERS[0], "/grant:r", KEEPERS[1]]);
        assert!(KEEPERS[0].starts_with("*S-1-5-18:") && KEEPERS[1].starts_with("*S-1-5-32-544:"));
        assert!(
            !args.iter().any(|arg| arg.contains("S-1-5-32-545")),
            "no grant to Users"
        );
    }

    /// The message *is* the feature: this check exists only so that a refusal says what was
    /// refused and what to do about it (ADR-0014). It must not offer `--user` as the way out —
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
