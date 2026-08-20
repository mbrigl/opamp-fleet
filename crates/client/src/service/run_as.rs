//! The operator-named service account of ADR-0062: resolved before anything is written, and the
//! ownership handover after the layout exists.
//!
//! `service install --run-as <account>` makes the system service run as that account, and the
//! installation's files — the configuration, the state directory, and the executable layout,
//! across both roots since ADR-0084 clause 3 — belong
//! to it afterwards. The two halves live here; *what* the service manager is told is
//! [`manager`](super::manager)'s and [`windows_config`](super::windows_config)'s business.
//!
//! **Resolution comes first** because ADR-0010 wants an install that cannot succeed to fail
//! before it writes: an account that does not exist (Unix), or a Windows account form that would
//! need a password nobody may pass (ADR-0046), is such an install. On Unix the account is
//! resolved through `id(1)` — POSIX, present on every Linux and macOS host, and the alternative
//! is `getpwnam(3)` behind `unsafe` or a user-lookup dependency for two integers.
//!
//! **The handover is `chown` on Unix and an ACL grant on Windows.** Ownership is walked without
//! following symlinks: the layout's `current` symlink is re-owned as a link (`lchown`), and what
//! it points to is re-owned as the `versions/` entry it is. On Windows the files under
//! `%ProgramData%` stay owned by Administrators and the account is *granted* Modify with
//! inheritance instead — the platform's idiom for "these directories are yours to use", and what
//! lets the service create tomorrow's files (a staged version, a rewritten `supervisor.toml`) in
//! directories it did not create today.

use std::path::Path;

/// An account the service may run as — validated (Windows) or resolved (Unix), never taken on
/// faith from the command line.
#[derive(Debug, Clone)]
pub struct RunAs {
    account: String,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
}

impl RunAs {
    /// Validate `account` against the platform's rules and resolve what the handover needs.
    /// `service` is the service's name, which since ADR-0084 is the product's — on Windows the
    /// one virtual account that may be
    /// named is the service's own.
    ///
    /// # Errors
    /// Returns an error if the account does not exist (Unix) or is not one of the passwordless
    /// Windows forms — in both cases before the install has written anything.
    pub fn resolve(account: &str, service: &str) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let _ = service;
            let (uid, gid) = resolve_unix(account)?;
            Ok(Self {
                account: account.to_string(),
                uid,
                gid,
            })
        }
        #[cfg(not(unix))]
        {
            windows_account_form(account, service)?;
            Ok(Self {
                account: account.to_string(),
            })
        }
    }

    /// The account as the service manager must be told it.
    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    /// Hand the given paths over to the account — recursively, skipping paths that do not exist
    /// (a configuration file the operator has not written yet is a warning elsewhere, not a
    /// failure here). The paths may overlap; the handover is idempotent.
    ///
    /// # Errors
    /// Returns an error naming the first path that could not be handed over.
    pub fn hand_over(&self, paths: &[&Path]) -> Result<(), String> {
        for path in paths {
            if !path.exists() && std::fs::symlink_metadata(path).is_err() {
                continue;
            }
            self.own(path).map_err(|e| {
                format!(
                    "cannot hand {} over to {}: {e}",
                    path.display(),
                    self.account
                )
            })?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn own(&self, path: &Path) -> std::io::Result<()> {
        chown_tree(path, self.uid, self.gid)
    }

    #[cfg(not(unix))]
    fn own(&self, path: &Path) -> Result<(), String> {
        // `(OI)(CI)` — inherit to files and subdirectories created later — is a directory-only
        // grant spec; a file takes the bare right. `/T` re-grants across what already exists.
        let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
        let grant = if is_dir {
            format!("{}:(OI)(CI)M", self.account)
        } else {
            format!("{}:M", self.account)
        };
        let mut cmd = std::process::Command::new("icacls.exe");
        cmd.arg(path).arg("/grant").arg(&grant);
        if is_dir {
            cmd.arg("/T");
        }
        let output = cmd
            .output()
            .map_err(|e| format!("cannot run icacls.exe: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "icacls.exe exited with {} ({})",
            output.status,
            String::from_utf8_lossy(&output.stdout).trim()
        ))
    }
}

/// Resolve an account name to its uid and primary gid through `id(1)`.
#[cfg(unix)]
fn resolve_unix(account: &str) -> Result<(u32, u32), String> {
    let uid = id(account, "-u")?;
    let gid = id(account, "-g")?;
    Ok((uid, gid))
}

#[cfg(unix)]
fn id(account: &str, flag: &str) -> Result<u32, String> {
    let output = std::process::Command::new("id")
        .arg(flag)
        .arg(account)
        .output()
        .map_err(|e| format!("cannot run id(1) to resolve the account {account}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "the account {account} does not exist on this host — create it first (e.g. `useradd \
             --system --home-dir /nonexistent --shell /usr/sbin/nologin {account}`). Nothing has \
             been installed or written."
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|e| format!("cannot read what id(1) said about {account}: {e}"))
}

/// Re-own a tree without following symlinks: every entry changes owner as what it is, and the
/// layout's `current` symlink never becomes a second walk into `versions/`.
#[cfg(unix)]
fn chown_tree(path: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            chown_tree(&entry?.path(), uid, gid)?;
        }
    }
    Ok(())
}

/// The passwordless Windows account forms — the only ones `--run-as` accepts, because a password
/// parameter must not exist (ADR-0046: it would stand in the process list and the installer log).
///
/// Compiled wherever it is used — the Windows install, and the tests of any platform: the rule is
/// pure string logic, and testing it must not need a Windows host.
#[cfg(any(not(unix), test))]
fn windows_account_form(account: &str, service: &str) -> Result<(), String> {
    const NT_SERVICE: &str = r"NT SERVICE\";
    const BUILT_IN: [&str; 2] = [r"NT AUTHORITY\LocalService", r"NT AUTHORITY\NetworkService"];

    if BUILT_IN.iter().any(|b| account.eq_ignore_ascii_case(b)) {
        return Ok(());
    }
    if account.len() >= NT_SERVICE.len()
        && account[..NT_SERVICE.len()].eq_ignore_ascii_case(NT_SERVICE)
    {
        let name = &account[NT_SERVICE.len()..];
        if name.eq_ignore_ascii_case(service) {
            return Ok(());
        }
        return Err(format!(
            "the virtual account {account} belongs to another service — this service's own is \
             NT SERVICE\\{service}"
        ));
    }
    if account.ends_with('$') {
        // A group-managed service account: the domain manages its password, and grants its
        // *Log on as a service* right through group policy — not this install.
        return Ok(());
    }
    Err(format!(
        "the account {account} would need a password, and a password is never taken on a command \
         line (ADR-0046). Passwordless forms: the service's own virtual account \
         (NT SERVICE\\{service}), a group-managed service account (name ending in $), NT \
         AUTHORITY\\LocalService, or NT AUTHORITY\\NetworkService."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0062: the accepted Windows forms are exactly the passwordless ones, and the refusal
    /// names them — an operator typing a plain account must learn the forms, not a Win32 error.
    #[test]
    fn windows_forms_are_the_passwordless_ones() {
        let svc = "supervisor";
        assert!(windows_account_form(r"NT SERVICE\supervisor", svc).is_ok());
        assert!(
            windows_account_form(r"nt service\SUPERVISOR", svc).is_ok(),
            "account names compare case-insensitively on Windows"
        );
        assert!(windows_account_form(r"NT AUTHORITY\LocalService", svc).is_ok());
        assert!(windows_account_form(r"nt authority\networkservice", svc).is_ok());
        assert!(
            windows_account_form(r"CORP\fleet-agents$", svc).is_ok(),
            "gMSA"
        );

        let other = windows_account_form(r"NT SERVICE\mssqlserver", svc).expect_err("not ours");
        assert!(other.contains(r"NT SERVICE\supervisor"), "{other}");

        let plain = windows_account_form("bob", svc).expect_err("needs a password");
        assert!(plain.contains("password"), "{plain}");
        assert!(plain.contains(r"NT SERVICE\supervisor"), "{plain}");
        assert!(plain.contains("ADR-0046"), "{plain}");
    }

    /// The refusal for a missing Unix account is the actionable message ADR-0010 asks installs to
    /// fail with — and it must promise that nothing was written, because resolution runs first.
    #[cfg(unix)]
    #[test]
    fn a_missing_account_is_refused_with_the_way_out() {
        let err = RunAs::resolve("no-such-account-0062", "supervisor").expect_err("must not exist");
        assert!(err.contains("does not exist"), "{err}");
        assert!(err.contains("useradd"), "{err}");
        assert!(
            err.contains("Nothing has been installed or written"),
            "{err}"
        );
    }

    /// The handover re-owns a tree including a symlink as a link — to the account's own ids here,
    /// because a test does not run as root, and a chown to the current owner is the one chown an
    /// unprivileged process is allowed.
    #[cfg(unix)]
    #[test]
    fn the_handover_walks_the_tree_and_skips_what_is_missing() {
        let user = String::from_utf8(
            std::process::Command::new("id")
                .arg("-un")
                .output()
                .expect("id -un")
                .stdout,
        )
        .expect("utf8");
        let me = RunAs::resolve(user.trim(), "supervisor").expect("own account resolves");

        let root = std::env::temp_dir().join(format!("run-as-test-{}", std::process::id()));
        let versions = root.join("versions/supervisor-1.0.0-abc");
        std::fs::create_dir_all(&versions).expect("mkdir");
        std::fs::write(
            versions.join(crate::service::layout::BINARY_FILENAME),
            b"binary",
        )
        .expect("write");
        std::os::unix::fs::symlink("versions/supervisor-1.0.0-abc", root.join("current"))
            .expect("symlink");

        let missing = root.join("supervisor.toml");
        me.hand_over(&[&root, &missing])
            .expect("a missing config file is skipped, the tree is walked");

        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}
