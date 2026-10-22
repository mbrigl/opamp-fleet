//! The account the system service runs as (ADR-0042 clause 2): decided and resolved before
//! anything is written, and the ownership handover after the layout exists.
//!
//! `service install --run-as <account>` names the account; without the flag a re-registration
//! keeps the account its registration names, and a first registration runs as root (Linux, macOS)
//! or `LocalSystem` (Windows). The installation's files — the configuration, the state directory,
//! and the executable layout, across both roots (ADR-0019 clause 8) — belong to that account
//! afterwards. *What* the service manager is told is [`manager`](super::manager)'s and
//! [`windows_config`](super::windows_config)'s business.
//!
//! **The decision and the resolution come first** because an install that cannot succeed fails
//! before it writes: an account that does not exist (Unix), or a Windows account form that would
//! need a password nobody may pass (ADR-0019), is such an install. On Unix a name is resolved
//! through `id(1)`, POSIX and present on every Linux and macOS host; the owner of a data root is
//! named through `getpwuid_r(3)`, which takes a uid and nothing else.
//!
//! **The handover is `chown` on Unix and an ACL grant on Windows.** On Unix it never follows a
//! link: the account owns the trees it walks and could swap a directory for a link while root walks
//! them. Each directory is opened without following a link and re-owned through that handle, its
//! entries are read from the handle, a symbolic link is re-owned as a link, and a regular file with
//! a second hard link is skipped and named. On Windows the files under `%ProgramData%` stay owned by
//! Administrators and the account is *granted* Modify with inheritance instead — the platform's
//! idiom for "these directories are yours to use", and what lets the service create tomorrow's
//! files (a staged version, a rewritten `supervisor.toml`) in directories it did not create today.

use std::path::{Path, PathBuf};

use super::manager::Registration;
use super::ServiceLevel;

/// Where the account a registration runs under came from (ADR-0042 clause 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `--run-as`.
    Flag,
    /// The unit or plist an earlier registration wrote, which names the account.
    Registered(PathBuf),
    /// The owner of the data root, because the registration names no account.
    DataRootOwner(PathBuf),
    /// No registration names one: root under systemd and launchd, `LocalSystem` or the account the
    /// SCM already holds on Windows.
    Default,
}

impl Source {
    /// The words the install prints after the account.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Flag => "named with --run-as".to_string(),
            Self::Registered(file) => format!("kept from {}", file.display()),
            Self::DataRootOwner(root) => format!(
                "the owner of the data root {}, because the registration names no account",
                root.display()
            ),
            Self::Default => "the default".to_string(),
        }
    }
}

/// The account a registration runs under, and where it came from.
#[derive(Debug)]
pub struct Decision {
    /// The resolved account; `None` where nothing is named — the user scope, and Windows without
    /// `--run-as`, where the SCM keeps the logon account it holds.
    pub run_as: Option<RunAs>,
    /// Where the account came from.
    pub source: Source,
}

/// Decide the account of a `service install` (ADR-0042 clause 2) and resolve it, before anything is
/// written: `--run-as`, else the account `registration` names, else — a registration that names
/// none, on a data root a non-root account owns — that owner, else root.
///
/// # Errors
/// Returns an error if the account does not exist (Unix) or is not one of the passwordless Windows
/// forms; the message names where a kept account came from.
pub fn decide(
    level: ServiceLevel,
    flag: Option<&str>,
    registration: Option<&Registration>,
    data_root: &Path,
    service: &str,
) -> Result<Decision, String> {
    if level == ServiceLevel::User {
        // A `--user` service runs as its user; `--run-as` conflicts with `--user`.
        return Ok(Decision {
            run_as: None,
            source: Source::Default,
        });
    }
    #[cfg(unix)]
    {
        let (account, source) =
            choose(flag, registration, data_root, || data_root_owner(data_root))?;
        let run_as = RunAs::resolve(&account, service, &source, data_root)?;
        Ok(Decision {
            run_as: Some(run_as),
            source,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = registration;
        match flag {
            Some(account) => Ok(Decision {
                run_as: Some(RunAs::resolve(account, service, &Source::Flag, data_root)?),
                source: Source::Flag,
            }),
            None => Ok(Decision {
                run_as: None,
                source: Source::Default,
            }),
        }
    }
}

/// The order of ADR-0042 clause 2, on what was found. `owner` names the data root's owner when it
/// is not root, and is asked only for a registration that names no account.
#[cfg(unix)]
fn choose(
    flag: Option<&str>,
    registration: Option<&Registration>,
    data_root: &Path,
    owner: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<(String, Source), String> {
    if let Some(account) = flag {
        return Ok((account.to_string(), Source::Flag));
    }
    match registration {
        Some(Registration {
            file,
            account: Some(account),
        }) => Ok((account.clone(), Source::Registered(file.clone()))),
        Some(Registration { account: None, .. }) => Ok(match owner()? {
            Some(owner) => (owner, Source::DataRootOwner(data_root.to_path_buf())),
            None => ("root".to_string(), Source::Default),
        }),
        None => Ok(("root".to_string(), Source::Default)),
    }
}

/// The name of the account that owns `data_root`, or `None` where root owns it or it does not
/// exist yet.
///
/// # Errors
/// Returns an error if a non-root uid owns it that no account on this host carries — the install
/// then fails closed rather than falling back to root.
#[cfg(unix)]
fn data_root_owner(data_root: &Path) -> Result<Option<String>, String> {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(meta) = std::fs::symlink_metadata(data_root) else {
        return Ok(None);
    };
    if meta.uid() == 0 {
        return Ok(None);
    }
    match account_of(meta.uid()).map(|found| found.map(|account| account.name)) {
        Ok(Some(name)) => Ok(Some(name)),
        Ok(None) => Err(format!(
            "the data root {} is owned by uid {}, which no account on this host carries, and the \
             registration names no account. Name one with --run-as <account> or --run-as root. \
             Nothing has been installed or written.",
            data_root.display(),
            meta.uid()
        )),
        Err(e) => Err(format!(
            "cannot name the owner (uid {}) of {}: {e}",
            meta.uid(),
            data_root.display()
        )),
    }
}

/// What the password database says of one uid.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub home: PathBuf,
    /// The primary group.
    pub gid: u32,
}

/// The account of `uid`, through `getpwuid_r(3)`; `None` where no entry names it.
///
/// # Errors
/// Returns the error the lookup failed with.
#[cfg(unix)]
pub fn account_of(uid: u32) -> std::io::Result<Option<Account>> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut buffer = vec![0 as libc::c_char; 1024];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call and the buffer's length is the one passed;
        // `found` is either null or points at `entry`, whose strings live in `buffer`.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &raw mut found,
            )
        };
        if rc == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc));
        }
        if found.is_null() {
            return Ok(None);
        }
        // SAFETY: `found` points at the filled `entry`, and `pw_name` and `pw_dir` at
        // NUL-terminated strings inside `buffer`, which outlives these reads.
        let (name, home, gid) = unsafe {
            (
                std::ffi::CStr::from_ptr((*found).pw_name),
                std::ffi::CStr::from_ptr((*found).pw_dir),
                (*found).pw_gid,
            )
        };
        return Ok(Some(Account {
            name: name.to_string_lossy().into_owned(),
            home: PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes())),
            gid,
        }));
    }
}

/// An account the service may run as — validated (Windows) or resolved (Unix), never taken on
/// faith from the command line or from a file.
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
    /// `service` is the service's name, which is the product's (ADR-0019 clause 3) — on Windows the
    /// one virtual account that may be named is the service's own. `source` and `data_root` go into
    /// the refusal of an account that does not exist.
    ///
    /// # Errors
    /// Returns an error if the account does not exist (Unix) or is not one of the passwordless
    /// Windows forms — in both cases before the install has written anything.
    pub fn resolve(
        account: &str,
        service: &str,
        source: &Source,
        data_root: &Path,
    ) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let _ = service;
            let (uid, gid) = resolve_unix(account).map_err(|missing| {
                missing_account(account, source, data_root, missing.as_deref())
            })?;
            Ok(Self {
                account: account.to_string(),
                uid,
                gid,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (source, data_root);
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

    /// The account's uid.
    #[cfg(unix)]
    #[must_use]
    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// Hand the given paths over to the account — recursively, skipping paths that do not exist
    /// (a configuration file the operator has not written yet is a warning elsewhere, not a
    /// failure here), and a path inside one handed over before it in the same call, which that
    /// walk covered without following a link. The handover is idempotent.
    ///
    /// Where `process` names a process account, its data is left alone (ADR-0059 clause 10): an
    /// entry it owns is skipped with everything beneath it, and an entry in its primary group
    /// keeps that group, a directory its mode too, so a re-registration takes from no Managed
    /// Process what it wrote or reads. Without one, everything is re-owned, group included.
    ///
    /// Returns the regular files left as they were because they have a second hard link (Unix):
    /// another name for the file may stand outside the roots, and re-owning it would hand that
    /// over too.
    ///
    /// # Errors
    /// Returns an error naming the first path that could not be handed over.
    pub fn hand_over(
        &self,
        paths: &[&Path],
        process: Option<&ProcessAccount>,
    ) -> Result<Vec<PathBuf>, String> {
        let mut skipped = Vec::new();
        let mut walked: Vec<&Path> = Vec::new();
        for path in paths {
            if walked.iter().any(|done| path.starts_with(done))
                || std::fs::symlink_metadata(path).is_err()
            {
                continue;
            }
            self.own(path, process, &mut skipped).map_err(|e| {
                format!(
                    "cannot hand {} over to {}: {e}",
                    path.display(),
                    self.account
                )
            })?;
            walked.push(path);
        }
        Ok(skipped)
    }

    #[cfg(unix)]
    fn own(
        &self,
        path: &Path,
        process: Option<&ProcessAccount>,
        skipped: &mut Vec<PathBuf>,
    ) -> std::io::Result<()> {
        let kept = process.map(|process| walk::Kept {
            uid: process.uid,
            gid: process.gid,
        });
        walk::chown_tree(path, self.uid, self.gid, kept, skipped)
    }

    /// No process account is registered on Windows yet (ADR-0059), so there is nothing to keep.
    #[cfg(not(unix))]
    fn own(
        &self,
        path: &Path,
        _process: Option<&ProcessAccount>,
        _skipped: &mut Vec<PathBuf>,
    ) -> Result<(), String> {
        // Asked of the entry itself: a link or junction is never a directory to walk.
        let is_dir = std::fs::symlink_metadata(path)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        let output = std::process::Command::new("icacls.exe")
            .args(grant_args(path, &self.account, is_dir))
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

/// The `icacls.exe` arguments that grant `account` Modify on `path` (ADR-0042 clause 2).
///
/// `(OI)(CI)` — inherit to files and subdirectories created later — is a directory-only grant
/// spec; a file takes the bare right. `/T` re-grants across what already exists, and `/L` acts on a
/// symbolic link or junction itself, never on what it names: the account holds Modify on the tree
/// already and could have planted a junction to any directory in it.
#[cfg(any(windows, test))]
fn grant_args(path: &Path, account: &str, is_dir: bool) -> Vec<std::ffi::OsString> {
    let mut args = vec![path.as_os_str().to_owned(), "/grant".into()];
    if is_dir {
        args.push(format!("{account}:(OI)(CI)M").into());
        args.push("/T".into());
    } else {
        args.push(format!("{account}:M").into());
    }
    args.push("/L".into());
    args
}

/// Resolve an account name to its uid and primary gid through `id(1)`. `Err(None)` means the
/// account does not exist; `Err(Some(..))` that `id(1)` could not be asked or answered oddly.
#[cfg(unix)]
fn resolve_unix(account: &str) -> Result<(u32, u32), Option<String>> {
    let uid = id(account, "-u")?;
    let gid = id(account, "-g")?;
    Ok((uid, gid))
}

#[cfg(unix)]
fn id(account: &str, flag: &str) -> Result<u32, Option<String>> {
    let output = std::process::Command::new("id")
        .arg(flag)
        .arg("--")
        .arg(account)
        .output()
        .map_err(|e| Some(format!("cannot run id(1): {e}")))?;
    if !output.status.success() {
        return Err(None);
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|e| Some(format!("cannot read what id(1) said: {e}")))
}

/// The refusal of an account that does not exist (ADR-0042 clause 2): it names the account and,
/// for a kept one, the file it came from, shows the line that creates it, and names the flag that
/// names another.
#[cfg(unix)]
fn missing_account(
    account: &str,
    source: &Source,
    data_root: &Path,
    failure: Option<&str>,
) -> String {
    if let Some(failure) = failure {
        return format!(
            "cannot resolve the account {account}: {failure}. Nothing has been installed or \
             written."
        );
    }
    let which = match source {
        Source::Registered(file) => format!(
            "the account {account}, which {} names, does not exist on this host",
            file.display()
        ),
        Source::DataRootOwner(root) => format!(
            "the account {account}, which owns the data root {}, does not exist on this host",
            root.display()
        ),
        Source::Flag | Source::Default => {
            format!("the account {account} does not exist on this host")
        }
    };
    format!(
        "{which}. Create it first (`useradd --system --user-group --no-create-home --home-dir {} \
         --shell /usr/sbin/nologin {account}`), or name another with --run-as <account> or \
         --run-as root. Nothing has been installed or written.",
        data_root.display()
    )
}

/// The account every Managed Process runs under, through the second service (ADR-0059 clause 1):
/// decided and checked before anything is written, like the Client's own.
#[derive(Debug, Clone)]
pub struct ProcessAccount {
    account: String,
    uid: u32,
    /// The primary group: what the Client shares with the Managed Processes belongs to it
    /// (ADR-0059 clause 6).
    gid: u32,
    /// The second service's unit it was kept from; `None` where `--process-account` named it.
    kept_from: Option<PathBuf>,
}

impl ProcessAccount {
    /// The account as the service manager must be told it.
    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    /// The account's uid, the one the Client accepts as the second service.
    #[must_use]
    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// The words the install prints after the account.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.kept_from {
            Some(unit) => format!("kept from {}", unit.display()),
            None => "named with --process-account".to_string(),
        }
    }
}

/// What the checks of ADR-0059 clause 1 read of an account, as `id(1)` answers for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ids {
    pub uid: u32,
    /// The primary group.
    pub gid: u32,
    /// The primary group's name.
    pub group: String,
    /// Every group the account is in, the primary one included.
    pub groups: Vec<u32>,
}

/// Decide the process account of a `service install` (ADR-0059 clause 1) and check it against the
/// Client's account `client`, before anything is written: none where `dropped`
/// (`--no-process-account`), else `--process-account`, else the account `registration` — the
/// second service's unit — names, else none, and no second service. `supported` says whether this
/// host can register the second service at all.
///
/// # Errors
/// Returns why the account is refused: where a second service cannot be registered, root, a
/// Client that runs as root, an account that does not exist, the Client's own account, one whose
/// primary group is root's or the Client's primary group, one in the Client's primary group, or
/// one whose primary group the Client's account is not in.
pub fn decide_process_account(
    flag: Option<&str>,
    dropped: bool,
    registration: Option<&Registration>,
    client: Option<&RunAs>,
    supported: bool,
) -> Result<Option<ProcessAccount>, String> {
    if dropped {
        return Ok(None);
    }
    let Some((account, kept_from)) = choose_process_account(flag, registration)? else {
        return Ok(None);
    };
    let client = match client {
        Some(client) if supported => client,
        _ => {
            return Err(format!(
                "a process account needs the second service, which only a system service under \
                 systemd on Linux registers so far; {account} cannot be named here. Nothing has \
                 been installed or written."
            ))
        }
    };
    // The account goes into the second service's unit: one that cannot be written there is
    // refused here, before the install writes anything, not when the unit is rendered.
    super::manager::check_unit_account(&account)
        .map_err(|e| format!("{e}. Nothing has been installed or written."))?;
    #[cfg(unix)]
    {
        let found = account_ids(&account)?;
        let client_ids = account_ids(client.account())?.ok_or_else(|| {
            format!(
                "the Client's account {} cannot be resolved. Nothing has been installed or \
                 written.",
                client.account()
            )
        })?;
        check_process_account(
            &account,
            found.as_ref(),
            client.account(),
            &client_ids,
            kept_from.as_deref(),
        )?;
        let (uid, gid) = found.map_or((0, 0), |ids| (ids.uid, ids.gid));
        Ok(Some(ProcessAccount {
            account,
            uid,
            gid,
            kept_from,
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = (client, kept_from);
        Err(format!(
            "a process account cannot be named on this platform yet; {account} is refused. \
             Nothing has been installed or written."
        ))
    }
}

/// The order of ADR-0059 clause 1, on what was found: the flag, else the account the second
/// service's unit names, with that unit, else none.
fn choose_process_account(
    flag: Option<&str>,
    registration: Option<&Registration>,
) -> Result<Option<(String, Option<PathBuf>)>, String> {
    if let Some(account) = flag {
        return Ok(Some((account.to_string(), None)));
    }
    match registration {
        Some(Registration {
            file,
            account: Some(account),
        }) => Ok(Some((account.clone(), Some(file.clone())))),
        Some(Registration {
            file,
            account: None,
        }) => Err(format!(
            "{} names no account for the second service. Name one with --process-account \
             <account>, or drop it with {DROP} Nothing has been installed or written.",
            file.display()
        )),
        None => Ok(None),
    }
}

/// How a refusal that a kept process account causes names the way to drop it (ADR-0059 clause 1).
const DROP: &str = "--no-process-account, and the Client starts its Managed Processes itself.";

/// The checks of ADR-0059 clause 1 on the process account `account` — `found` as `id(1)` answers
/// for it, `None` where it does not exist — against the Client's account `client_name` with
/// `client`. `kept_from` is the unit a kept account came from.
///
/// # Errors
/// Returns the refusal, with the way out.
pub fn check_process_account(
    account: &str,
    found: Option<&Ids>,
    client_name: &str,
    client: &Ids,
    kept_from: Option<&Path>,
) -> Result<(), String> {
    refuse_process_account(account, found, client_name, client, kept_from).map_err(|refusal| {
        // A kept account was not asked for in this install: the refusal says how to drop it.
        match (kept_from, refusal.strip_suffix(NOTHING)) {
            (Some(_), Some(reason)) => format!("{reason}Or drop it with {DROP} {NOTHING}"),
            _ => refusal,
        }
    })
}

const NOTHING: &str = "Nothing has been installed or written.";

fn refuse_process_account(
    account: &str,
    found: Option<&Ids>,
    client_name: &str,
    client: &Ids,
    kept_from: Option<&Path>,
) -> Result<(), String> {
    let named = match kept_from {
        Some(unit) => format!(
            "the process account {account}, which {} names,",
            unit.display()
        ),
        None => format!("the process account {account}"),
    };
    if account == "root" || found.is_some_and(|ids| ids.uid == 0) {
        return Err(format!(
            "{named} is root, and a process account never is. A host whose agents need root runs \
             as root as a whole: --run-as root, and no --process-account. {NOTHING}"
        ));
    }
    if client.uid == 0 {
        return Err(format!(
            "the Client runs as root, and a process account separates the Managed Processes from \
             a Client that runs under an account of its own. Name the Client's account with \
             --run-as <account>, or run everything as root without --process-account. {NOTHING}"
        ));
    }
    let Some(found) = found else {
        return Err(format!(
            "{named} does not exist on this host. Create it first (`useradd --system --user-group \
             --no-create-home --shell /usr/sbin/nologin {account}`) and make its group one of \
             the Client's (`usermod --append --groups {account} {client_name}`), or name another \
             with --process-account <account>. {NOTHING}"
        ));
    };
    if found.uid == client.uid {
        return Err(format!(
            "{named} is the account the Client runs as ({client_name}). The process account is \
             another account, which the Client's does not share. {NOTHING}"
        ));
    }
    // What the process account's primary group may read is what the Managed Processes may read
    // (ADR-0059 clause 6): never root's group, and never a group that reaches the Client's files.
    if found.gid == 0 {
        return Err(format!(
            "{named} has the primary group {} (gid 0), and a process account's group is never \
             root's. Give it a group of its own (`useradd --user-group`), then install again. \
             {NOTHING}",
            found.group
        ));
    }
    if found.gid == client.gid {
        return Err(format!(
            "{named} has the primary group {}, which is the primary group of the Client's account \
             {client_name}: the Managed Processes would read what the Client's group may. Give it \
             a group of its own (`useradd --user-group`), then install again. {NOTHING}",
            found.group
        ));
    }
    if found.groups.contains(&client.gid) {
        return Err(format!(
            "{named} is a member of the primary group of the Client's account {client_name}: the \
             Managed Processes would read what the Client's group may. Remove it from that group \
             (`gpasswd --delete {account} {}`), then install again. {NOTHING}",
            client.group
        ));
    }
    if !client.groups.contains(&found.gid) {
        return Err(format!(
            "{named} has the primary group {}, and the Client's account {client_name} is not in \
             it. Add it (`usermod --append --groups {} {client_name}`), then install again. \
             {NOTHING}",
            found.group, found.group
        ));
    }
    Ok(())
}

/// What `id(1)` answers for `account`; `None` where it does not exist.
#[cfg(unix)]
fn account_ids(account: &str) -> Result<Option<Ids>, String> {
    let failed = |e: String| format!("cannot resolve the account {account}: {e}");
    let uid = match id(account, "-u") {
        Ok(uid) => uid,
        Err(None) => return Ok(None),
        Err(Some(e)) => return Err(failed(e)),
    };
    let gid =
        id(account, "-g").map_err(|e| failed(e.unwrap_or_else(|| "id -g failed".to_string())))?;
    let group = id_text(account, "-gn").map_err(failed)?;
    let groups = id_text(account, "-G")
        .map_err(failed)?
        .split_whitespace()
        .filter_map(|g| g.parse().ok())
        .collect();
    Ok(Some(Ids {
        uid,
        gid,
        group,
        groups,
    }))
}

/// What `id(1)` prints with `flag` for `account`, trimmed.
#[cfg(unix)]
fn id_text(account: &str, flag: &str) -> Result<String, String> {
    let output = std::process::Command::new("id")
        .arg(flag)
        .arg("--")
        .arg(account)
        .output()
        .map_err(|e| format!("cannot run id(1): {e}"))?;
    if !output.status.success() {
        return Err(format!("id {flag} failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The handover walk on Unix: descriptors, never paths below the top (ADR-0042 clause 2, ADR-0054).
#[cfg(unix)]
mod walk {
    use std::ffi::{CStr, CString, OsStr};
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};

    /// The entry a name stands for, opened as what it is and never through a link. On Linux this
    /// is a path-only handle: it holds a link, a device or a FIFO without opening what it is.
    #[cfg(target_os = "linux")]
    const ENTRY: libc::c_int = libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    /// On macOS `O_SYMLINK` opens a link itself; anything else is opened without blocking on a
    /// FIFO or taking a terminal.
    #[cfg(target_os = "macos")]
    const ENTRY: libc::c_int =
        libc::O_RDONLY | libc::O_SYMLINK | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;
    /// A directory already held, opened again for reading its entries.
    #[cfg(target_os = "linux")]
    const DIRECTORY: libc::c_int =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    /// What the walk leaves to the process account (ADR-0059 clause 10): an entry `uid` owns is
    /// not re-owned and not entered, and an entry in the group `gid` keeps it.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Kept {
        pub(super) uid: u32,
        pub(super) gid: u32,
    }

    /// Re-own `path` and, if it is a directory, everything under it, without following a link.
    /// The top is the one name resolved as a path, and its last component is never followed.
    /// What `kept` names is left as [`Kept`] says.
    pub(super) fn chown_tree(
        path: &Path,
        uid: u32,
        gid: u32,
        kept: Option<Kept>,
        skipped: &mut Vec<PathBuf>,
    ) -> io::Result<()> {
        chown_tree_with(path, uid, gid, kept, skipped, &|_| {})
    }

    /// `chown_tree`, calling `between` with each entry's path after its name was read from the
    /// directory and before the entry is opened: the window in which the account can swap it.
    pub(super) fn chown_tree_with(
        path: &Path,
        uid: u32,
        gid: u32,
        kept: Option<Kept>,
        skipped: &mut Vec<PathBuf>,
        between: &dyn Fn(&Path),
    ) -> io::Result<()> {
        let name = CString::new(path.as_os_str().as_bytes())?;
        let mut walk = Walk {
            uid,
            gid,
            kept,
            skipped,
            between,
        };
        walk.entry(libc::AT_FDCWD, &name, path)
    }

    /// Re-own the entry `name` in the directory `dir` holds, and nothing below it: through a
    /// handle on the entry itself, a link as a link. An entry that is no directory and has a
    /// second hard link is skipped, whoever owns it; one that already has the owner is left alone.
    /// A missing entry is nothing to do.
    pub(super) fn chown_entry(
        dir: RawFd,
        name: &OsStr,
        path: &Path,
        (uid, gid): (u32, u32),
        skipped: &mut Vec<PathBuf>,
    ) -> io::Result<()> {
        let name = CString::new(name.as_bytes())?;
        let handle = match open(dir, &name, ENTRY) {
            Ok(handle) => handle,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let st = fstat(&handle)?;
        if st.st_mode & libc::S_IFMT != libc::S_IFDIR && st.st_nlink > 1 {
            skipped.push(path.to_path_buf());
            return Ok(());
        }
        if (st.st_uid, st.st_gid) == (uid, gid) {
            return Ok(());
        }
        chown(&handle, uid, gid)
    }

    struct Walk<'a> {
        uid: u32,
        gid: u32,
        kept: Option<Kept>,
        skipped: &'a mut Vec<PathBuf>,
        between: &'a dyn Fn(&Path),
    }

    impl Walk<'_> {
        /// Re-own the entry `name` in `dir` through a handle on the entry itself. Its type and its
        /// link count are read from that handle, and its owner is changed through it, so a name
        /// swapped after it was read changes only what the handle holds. A directory is walked;
        /// anything else with a second hard link is skipped. What the process account owns is
        /// neither re-owned nor walked, and what is in its group keeps that group; a change of
        /// owner by root leaves a directory's mode, its set-group-ID bit included.
        fn entry(&mut self, dir: RawFd, name: &CStr, path: &Path) -> io::Result<()> {
            let handle = match open(dir, name, ENTRY) {
                Ok(handle) => handle,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e),
            };
            let st = fstat(&handle)?;
            let gid = match self.kept {
                Some(kept) if st.st_uid == kept.uid => return Ok(()),
                Some(kept) if st.st_gid == kept.gid => kept.gid,
                _ => self.gid,
            };
            if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
                return self.directory(handle, gid, path);
            }
            if st.st_nlink > 1 {
                self.skipped.push(path.to_path_buf());
                return Ok(());
            }
            chown(&handle, self.uid, gid)
        }

        /// Re-own the directory `handle` holds to the group `gid`, then its entries, read from the
        /// handle.
        fn directory(&mut self, handle: OwnedFd, gid: u32, path: &Path) -> io::Result<()> {
            // The path-only handle cannot list entries; `.` through it is that same directory.
            #[cfg(target_os = "linux")]
            let handle = match open(handle.as_raw_fd(), c".", DIRECTORY) {
                Ok(handle) => handle,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e),
            };
            chown(&handle, self.uid, gid)?;
            for name in entries(&handle)? {
                let child = path.join(OsStr::from_bytes(name.as_bytes()));
                (self.between)(&child);
                self.entry(handle.as_raw_fd(), &name, &child)?;
            }
            Ok(())
        }
    }

    /// The names in `dir`, read from the handle, without `.` and `..`.
    fn entries(dir: &OwnedFd) -> io::Result<Vec<CString>> {
        // `fdopendir` takes the descriptor it is given; it gets a duplicate, and `dir` stays ours.
        let raw = dir.try_clone()?.into_raw_fd();
        // SAFETY: `raw` is an open directory descriptor this function owns.
        let stream = unsafe { libc::fdopendir(raw) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: `fdopendir` failed, so `raw` is still ours to close.
            unsafe { libc::close(raw) };
            return Err(error);
        }
        let mut names = Vec::new();
        let result = loop {
            // SAFETY: errno is thread-local; clearing it tells the end of the stream from a
            // failure, which `readdir` both report as null.
            unsafe { *errno() = 0 };
            // SAFETY: `stream` is the open stream from `fdopendir`.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                break match error.raw_os_error() {
                    Some(0) | None => Ok(()),
                    Some(_) => Err(error),
                };
            }
            // SAFETY: `entry` is valid until the next `readdir` on this stream, and `d_name` is
            // NUL-terminated; the name is copied out at once.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() != b"." && name.to_bytes() != b".." {
                names.push(name.to_owned());
            }
        };
        // SAFETY: `stream` is open and closed once; this closes `raw` with it.
        unsafe { libc::closedir(stream) };
        result.map(|()| names)
    }

    #[cfg(target_os = "linux")]
    fn errno() -> *mut libc::c_int {
        // SAFETY: returns the calling thread's errno location.
        unsafe { libc::__errno_location() }
    }

    #[cfg(target_os = "macos")]
    fn errno() -> *mut libc::c_int {
        // SAFETY: returns the calling thread's errno location.
        unsafe { libc::__error() }
    }

    /// What `handle` holds, as the kernel sees it.
    fn fstat(handle: &OwnedFd) -> io::Result<libc::stat> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: a valid descriptor and room for one `stat`.
        if unsafe { libc::fstat(handle.as_raw_fd(), st.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fstat` succeeded and filled it.
        Ok(unsafe { st.assume_init() })
    }

    /// `name` in `dir`, opened with `flags`; the last component never follows a link.
    fn open(dir: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
        // SAFETY: a descriptor or `AT_FDCWD` and a NUL-terminated name; no `O_CREAT`, so no mode.
        let fd = unsafe { libc::openat(dir, name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a new descriptor that nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Re-own what `handle` holds, a link as a link. On Linux the handle may be path-only, which
    /// `fchown` refuses; an empty name with `AT_EMPTY_PATH` changes the handle's own entry.
    #[cfg(target_os = "linux")]
    fn chown(handle: &OwnedFd, uid: u32, gid: u32) -> io::Result<()> {
        // SAFETY: a valid descriptor and a NUL-terminated (empty) name.
        let rc = unsafe {
            libc::fchownat(
                handle.as_raw_fd(),
                c"".as_ptr(),
                uid,
                gid,
                libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Re-own what `handle` holds; a handle opened with `O_SYMLINK` holds the link itself.
    #[cfg(target_os = "macos")]
    fn chown(handle: &OwnedFd, uid: u32, gid: u32) -> io::Result<()> {
        // SAFETY: a valid descriptor.
        if unsafe { libc::fchown(handle.as_raw_fd(), uid, gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Give each of `paths` the owner of `like`, the configuration of an installed Client: what
/// `service config` creates belongs to the account the service runs as (ADR-0019 clause 13), not
/// to the administrator who ran it. A missing path is skipped.
///
/// Each path is re-owned as what it is and nothing is walked: the state directory belongs to that
/// account, which could swap a directory for a link while root walked it, so the caller names
/// exactly the files it wrote. Nor is a link followed on the way (ADR-0054): each path's
/// directory is opened without following a link at its name, and the entry is re-owned through a
/// handle on it, a link as a link. The data root the administrator named, `data_root`, and the
/// configuration's own directory are the two directories reached as their paths say (ADR-0054
/// clause 4); every other is one the account could have swapped. A regular file with a second hard
/// link is skipped, as the handover skips one: another name for it may stand anywhere.
///
/// Returns the files skipped for a second hard link.
///
/// On Windows there is nothing to do: the handover granted the account Modify with inheritance, so
/// what is created under the data root is the account's to use already.
///
/// # Errors
/// Returns an error naming the path whose owner could not be changed, or whose directory is a link.
pub fn match_owner(like: &Path, data_root: &Path, paths: &[&Path]) -> Result<Vec<PathBuf>, String> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut skipped = Vec::new();
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::MetadataExt as _;
        let Ok(owner) = std::fs::symlink_metadata(like) else {
            return Ok(skipped);
        };
        for path in paths {
            let refused = |e: std::io::Error| {
                format!(
                    "cannot give {} the owner of {}: {e}",
                    path.display(),
                    like.display()
                )
            };
            let held = match (path.parent(), path.file_name().and_then(|n| n.to_str())) {
                (Some(parent), Some(name))
                    if parent == data_root || Some(parent) == like.parent() =>
                {
                    crate::nofollow::HeldDir::open_root(parent).map(|dir| (dir, name))
                }
                _ => crate::nofollow::held_parent(path),
            };
            let (dir, name) = match held {
                Ok(held) => held,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(refused(e)),
            };
            walk::chown_entry(
                dir.as_raw_fd(),
                std::ffi::OsStr::new(name),
                path,
                (owner.uid(), owner.gid()),
                &mut skipped,
            )
            .map_err(refused)?;
        }
    }
    #[cfg(not(unix))]
    let _ = (like, data_root, paths);
    Ok(skipped)
}

/// The passwordless Windows account forms — the only ones `--run-as` accepts, because a password
/// parameter must not exist (ADR-0019: it would stand in the process list and the installer log).
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
         line. Passwordless forms: the service's own virtual account \
         (NT SERVICE\\{service}), a group-managed service account (name ending in $), NT \
         AUTHORITY\\LocalService, or NT AUTHORITY\\NetworkService."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Windows hand-over never follows a link (ADR-0042 clause 2): every grant acts on a link
    /// or junction itself, so one the account planted under a root it already holds cannot carry
    /// Modify to the directory it names.
    /// Verifies: ADR-0042
    #[test]
    fn the_windows_handover_grants_on_a_link_itself() {
        let strings = |args: Vec<std::ffi::OsString>| -> Vec<String> {
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            strings(grant_args(Path::new(r"C:\data"), r"NT SERVICE\x", true)),
            [r"C:\data", "/grant", r"NT SERVICE\x:(OI)(CI)M", "/T", "/L"]
        );
        assert_eq!(
            strings(grant_args(Path::new(r"C:\data\f"), r"NT SERVICE\x", false)),
            [r"C:\data\f", "/grant", r"NT SERVICE\x:M", "/L"]
        );
    }

    /// ADR-0019: the accepted Windows forms are exactly the passwordless ones, and the refusal
    /// names them — an operator typing a plain account must learn the forms, not a Win32 error.
    /// Verifies: ADR-0019
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
        assert!(!plain.contains("ADR-"), "{plain}");
    }

    #[cfg(unix)]
    const DATA_ROOT: &str = "/var/lib/opamp-fleet";
    #[cfg(unix)]
    const UNIT: &str = "/etc/systemd/system/opamp-fleet.service";

    #[cfg(unix)]
    fn registered(account: Option<&str>) -> Registration {
        Registration {
            file: PathBuf::from(UNIT),
            account: account.map(str::to_string),
        }
    }

    #[cfg(unix)]
    fn me() -> String {
        let out = std::process::Command::new("id")
            .arg("-un")
            .output()
            .expect("id -un");
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }

    /// An owner lookup that must not be asked.
    #[cfg(unix)]
    fn unasked() -> Result<Option<String>, String> {
        panic!("the data root's owner is asked only for a registration that names no account")
    }

    /// The refusal for a missing Unix account is the actionable message installs fail with — and
    /// it must promise that nothing was written, because resolution runs first.
    /// Verifies: ADR-0019, ADR-0042
    #[cfg(unix)]
    #[test]
    fn a_missing_account_is_refused_with_the_way_out() {
        let err = RunAs::resolve(
            "no-such-account-0062",
            "supervisor",
            &Source::Flag,
            Path::new(DATA_ROOT),
        )
        .expect_err("must not exist");
        assert!(err.contains("does not exist"), "{err}");
        assert!(
            err.contains("useradd --system --user-group --no-create-home --home-dir /var/lib/opamp-fleet --shell /usr/sbin/nologin no-such-account-0062"),
            "{err}"
        );
        assert!(err.contains("--run-as root"), "{err}");
        assert!(
            err.contains("Nothing has been installed or written"),
            "{err}"
        );
    }

    /// A re-registration without `--run-as` keeps the account the unit names, and says where it
    /// came from — the regression of a plain `service install` on every package upgrade, which
    /// returned a host installed with `--run-as` to root.
    /// Verifies: ADR-0042
    #[cfg(unix)]
    #[test]
    fn a_reregistration_without_run_as_keeps_the_units_account() {
        let unit = registered(Some("telegraf"));
        let (account, source) =
            choose(None, Some(&unit), Path::new(DATA_ROOT), unasked).expect("decided");
        assert_eq!(account, "telegraf");
        assert_eq!(source, Source::Registered(PathBuf::from(UNIT)));
        assert!(source.describe().contains(UNIT), "{}", source.describe());

        // `--run-as` outranks what the unit names.
        let (account, source) =
            choose(Some("other"), Some(&unit), Path::new(DATA_ROOT), unasked).expect("decided");
        assert_eq!((account.as_str(), source), ("other", Source::Flag));

        // And the whole decision, with the account resolved, on a real one.
        let me = me();
        let decision = decide(
            ServiceLevel::System,
            None,
            Some(&registered(Some(&me))),
            Path::new(DATA_ROOT),
            "opamp-fleet",
        )
        .expect("the test's own account resolves");
        assert_eq!(decision.run_as.expect("an account").account(), me);
        assert_eq!(decision.source, Source::Registered(PathBuf::from(UNIT)));
    }

    /// `--run-as root` is how root is chosen, and the unit records it as `User=root`, so the next
    /// re-registration keeps root.
    /// Verifies: ADR-0042
    #[cfg(unix)]
    #[test]
    fn run_as_root_is_written_as_user_root() {
        let decision = decide(
            ServiceLevel::System,
            Some("root"),
            Some(&registered(Some("opamp-fleet"))),
            Path::new(DATA_ROOT),
            "opamp-fleet",
        )
        .expect("root exists");
        let run_as = decision.run_as.expect("an account");
        assert_eq!(run_as.account(), "root");
        let unit = super::super::manager::systemd_unit(
            &super::super::manager::InstallSpec {
                level: ServiceLevel::System,
                program: PathBuf::from("/opt/opamp-fleet/current/supervisor"),
                config_path: PathBuf::from("/var/lib/opamp-fleet/supervisor.toml"),
                state_dir: PathBuf::from("/var/lib/opamp-fleet/state"),
                run_as: Some(run_as.account().to_string()),
                agents: None,
            },
            run_as.account(),
        )
        .expect("unit");
        assert!(unit.lines().any(|l| l == "User=root"), "{unit}");
    }

    /// A unit that names no account, on a data root an account owns, was rewritten by a
    /// re-registration that dropped its account: the install takes the owner, and names the data
    /// root it took it from.
    /// Verifies: ADR-0042
    #[cfg(unix)]
    #[test]
    fn a_unit_without_an_account_on_an_account_owned_data_root_takes_the_owner() {
        let (account, source) = choose(None, Some(&registered(None)), Path::new(DATA_ROOT), || {
            Ok(Some("telegraf".to_string()))
        })
        .expect("decided");
        assert_eq!(account, "telegraf");
        assert_eq!(source, Source::DataRootOwner(PathBuf::from(DATA_ROOT)));
        assert!(
            source.describe().contains(DATA_ROOT),
            "{}",
            source.describe()
        );

        // The owner as the install reads it: a directory this test created is the test's.
        let root = tempfile::tempdir().expect("tempdir");
        let owner = data_root_owner(root.path()).expect("an account owns it");
        let me = me();
        if me == "root" {
            assert_eq!(owner, None, "root owns it, and root is no account to take");
        } else {
            assert_eq!(owner.as_deref(), Some(me.as_str()));
        }
    }

    /// A unit that names no account, on a data root root owns, is an install that ran as root,
    /// and it stays root.
    /// Verifies: ADR-0042
    #[cfg(unix)]
    #[test]
    fn a_unit_without_an_account_on_a_root_owned_data_root_stays_root() {
        let (account, source) = choose(None, Some(&registered(None)), Path::new(DATA_ROOT), || {
            Ok(None)
        })
        .expect("decided");
        assert_eq!((account.as_str(), source), ("root", Source::Default));
        assert_eq!(
            data_root_owner(Path::new("/")).expect("root owns /"),
            None,
            "a root-owned data root names no account"
        );
        assert_eq!(
            data_root_owner(Path::new("/no/such/data/root")).expect("missing"),
            None
        );
    }

    /// Where no unit exists, nothing is kept and the data root is not asked: a first registration
    /// runs as root. The Linux packages name their account themselves.
    /// Verifies: ADR-0042
    #[cfg(unix)]
    #[test]
    fn no_registration_means_root() {
        let (account, source) = choose(None, None, Path::new(DATA_ROOT), unasked).expect("decided");
        assert_eq!((account.as_str(), source), ("root", Source::Default));
        let user =
            decide(ServiceLevel::User, None, None, Path::new(DATA_ROOT), "x").expect("user scope");
        assert!(user.run_as.is_none(), "a user service runs as its user");
    }

    /// A kept account that has been deleted fails closed, before anything is written: the message
    /// names the account and the unit it came from, and the two ways out.
    /// Verifies: ADR-0042
    #[cfg(unix)]
    #[test]
    fn a_kept_account_that_no_longer_exists_is_refused_naming_its_source() {
        let err = decide(
            ServiceLevel::System,
            None,
            Some(&registered(Some("no-such-account-0043"))),
            Path::new(DATA_ROOT),
            "opamp-fleet",
        )
        .expect_err("must not fall back to root");
        assert!(err.contains("no-such-account-0043"), "{err}");
        assert!(err.contains(UNIT), "{err}");
        assert!(err.contains("useradd --system"), "{err}");
        assert!(err.contains("--run-as <account>"), "{err}");
        assert!(err.contains("--run-as root"), "{err}");
        assert!(
            err.contains("Nothing has been installed or written"),
            "{err}"
        );
    }

    const AGENTS_UNIT: &str = "/etc/systemd/system/opamp-fleet-agents.service";

    /// The Client's account as the checks see it: uid 998, in the groups 998 and 997.
    fn client_ids() -> Ids {
        Ids {
            uid: 998,
            gid: 998,
            group: "opamp-fleet".to_string(),
            groups: vec![998, 997],
        }
    }

    /// A process account of uid and primary group 997.
    fn agents_ids() -> Ids {
        Ids {
            uid: 997,
            gid: 997,
            group: "opamp-fleet-agents".to_string(),
            groups: vec![997],
        }
    }

    /// `--process-account` names the account; without it a re-registration keeps the one the
    /// second service's unit names, and with neither there is no process account.
    /// Verifies: ADR-0059
    #[test]
    fn a_process_account_is_named_or_kept_from_the_second_services_unit() {
        let unit = Registration {
            file: PathBuf::from(AGENTS_UNIT),
            account: Some("opamp-fleet-agents".to_string()),
        };
        assert_eq!(
            choose_process_account(None, Some(&unit)).expect("kept"),
            Some((
                "opamp-fleet-agents".to_string(),
                Some(PathBuf::from(AGENTS_UNIT))
            ))
        );
        assert_eq!(
            choose_process_account(Some("other"), Some(&unit)).expect("named"),
            Some(("other".to_string(), None)),
            "the flag outranks the unit"
        );
        assert_eq!(choose_process_account(None, None).expect("none"), None);
        let lost = Registration {
            file: PathBuf::from(AGENTS_UNIT),
            account: None,
        };
        let err = choose_process_account(None, Some(&lost)).expect_err("no account to keep");
        assert!(err.contains(AGENTS_UNIT), "{err}");
        assert!(err.contains("--process-account"), "{err}");

        let kept = ProcessAccount {
            account: "opamp-fleet-agents".to_string(),
            uid: 997,
            gid: 997,
            kept_from: Some(PathBuf::from(AGENTS_UNIT)),
        };
        assert!(kept.describe().contains(AGENTS_UNIT), "{}", kept.describe());
    }

    /// A process account that exists, is neither root nor the Client's, and whose group the
    /// Client's account is in, is taken.
    /// Verifies: ADR-0059
    #[test]
    fn a_process_account_in_the_clients_groups_is_taken() {
        check_process_account(
            "opamp-fleet-agents",
            Some(&agents_ids()),
            "opamp-fleet",
            &client_ids(),
            None,
        )
        .expect("taken");
    }

    /// Root is never the process account, by name or by uid, and a Client that runs as root has
    /// none: the refusal names `--run-as root` as the way to run everything as root.
    /// Verifies: ADR-0059
    #[test]
    fn the_process_account_is_never_root_and_the_client_never_is() {
        let root = Ids {
            uid: 0,
            gid: 0,
            group: "root".to_string(),
            groups: vec![0],
        };
        for (account, found) in [("root", Some(&root)), ("toor", Some(&root))] {
            let err = check_process_account(account, found, "opamp-fleet", &client_ids(), None)
                .expect_err("root");
            assert!(err.contains("--run-as root"), "{err}");
            assert!(
                err.contains("Nothing has been installed or written"),
                "{err}"
            );
        }
        let err = check_process_account(
            "opamp-fleet-agents",
            Some(&agents_ids()),
            "root",
            &root,
            None,
        )
        .expect_err("a root Client");
        assert!(err.contains("--run-as <account>"), "{err}");
    }

    /// The process account is never the Client's own.
    /// Verifies: ADR-0059
    #[test]
    fn the_process_account_is_never_the_clients() {
        let err = check_process_account(
            "opamp-fleet",
            Some(&client_ids()),
            "opamp-fleet",
            &client_ids(),
            None,
        )
        .expect_err("the Client's own");
        assert!(err.contains("the Client runs as"), "{err}");
    }

    /// An account that does not exist is refused with the lines that create it and give its group
    /// to the Client's account; a kept one names the unit it came from.
    /// Verifies: ADR-0059
    #[test]
    fn a_missing_process_account_is_refused_with_the_way_out() {
        let err = check_process_account(
            "opamp-fleet-agents",
            None,
            "opamp-fleet",
            &client_ids(),
            Some(Path::new(AGENTS_UNIT)),
        )
        .expect_err("missing");
        assert!(err.contains("does not exist"), "{err}");
        assert!(err.contains(AGENTS_UNIT), "{err}");
        assert!(
            err.contains(
                "useradd --system --user-group --no-create-home --shell /usr/sbin/nologin \
                 opamp-fleet-agents"
            ),
            "{err}"
        );
        assert!(
            err.contains("usermod --append --groups opamp-fleet-agents opamp-fleet"),
            "{err}"
        );
        assert!(
            err.contains("Nothing has been installed or written"),
            "{err}"
        );
    }

    /// The Client reaches what the process account may read through its primary group (ADR-0059
    /// clause 6): an account whose group the Client's account is not in is refused, with the line
    /// that adds it.
    /// Verifies: ADR-0059
    #[test]
    fn a_process_account_whose_group_the_client_lacks_is_refused() {
        let mut client = client_ids();
        client.groups = vec![998];
        let err = check_process_account(
            "opamp-fleet-agents",
            Some(&agents_ids()),
            "opamp-fleet",
            &client,
            None,
        )
        .expect_err("not in the group");
        assert!(
            err.contains("usermod --append --groups opamp-fleet-agents opamp-fleet"),
            "{err}"
        );
        assert!(
            err.contains("Nothing has been installed or written"),
            "{err}"
        );
    }

    /// `--no-process-account` drops a kept process account; a refusal that a kept one causes —
    /// here `--run-as root` beside a registered second service — names that flag, and one an
    /// install named does not (ADR-0059 clause 1).
    /// Verifies: ADR-0059
    #[test]
    fn a_kept_process_account_can_be_dropped_and_its_refusals_say_so() {
        let unit = Registration {
            file: PathBuf::from(AGENTS_UNIT),
            account: Some("opamp-fleet-agents".to_string()),
        };
        assert!(
            decide_process_account(None, true, Some(&unit), None, false)
                .expect("dropped")
                .is_none(),
            "dropped before anything else is asked"
        );
        let root = Ids {
            uid: 0,
            gid: 0,
            group: "root".to_string(),
            groups: vec![0],
        };
        let kept = check_process_account(
            "opamp-fleet-agents",
            Some(&agents_ids()),
            "root",
            &root,
            Some(Path::new(AGENTS_UNIT)),
        )
        .expect_err("a root Client");
        assert!(kept.contains("--no-process-account"), "{kept}");
        assert!(
            kept.ends_with("Nothing has been installed or written."),
            "{kept}"
        );
        let named = check_process_account(
            "opamp-fleet-agents",
            Some(&agents_ids()),
            "root",
            &root,
            None,
        )
        .expect_err("a root Client");
        assert!(!named.contains("--no-process-account"), "{named}");
        let lost = Registration {
            file: PathBuf::from(AGENTS_UNIT),
            account: None,
        };
        let err = choose_process_account(None, Some(&lost)).expect_err("no account to keep");
        assert!(err.contains("--no-process-account"), "{err}");
    }

    /// The process account's primary group is neither root's nor the Client's own, and the
    /// account is not in the Client's primary group (ADR-0059 clause 6): what that group may read
    /// is what every Managed Process may.
    /// Verifies: ADR-0059
    #[test]
    fn a_process_account_whose_group_reaches_the_clients_files_is_refused() {
        let refused = |agents: Ids, client: Ids| {
            check_process_account(
                "opamp-fleet-agents",
                Some(&agents),
                "opamp-fleet",
                &client,
                None,
            )
            .expect_err("refused")
        };
        let mut wheel = agents_ids();
        wheel.gid = 0;
        wheel.group = "root".to_string();
        let mut client = client_ids();
        client.groups.push(0);
        let err = refused(wheel, client);
        assert!(err.contains("gid 0"), "{err}");

        let mut shared = agents_ids();
        shared.gid = 998;
        shared.group = "opamp-fleet".to_string();
        let err = refused(shared, client_ids());
        assert!(
            err.contains("primary group of the Client's account"),
            "{err}"
        );

        let mut member = agents_ids();
        member.groups.push(998);
        let err = refused(member, client_ids());
        assert!(err.contains("is a member of the primary group"), "{err}");
        assert!(
            err.contains("gpasswd --delete opamp-fleet-agents opamp-fleet"),
            "{err}"
        );
        assert!(
            err.contains("Nothing has been installed or written"),
            "{err}"
        );
    }

    /// Where no second service can be registered — the user scope, another service manager, or
    /// another platform for now — a named process account is refused; with none named, nothing
    /// changes. A name no unit can hold is refused here too, before the install writes anything.
    /// Against the accounts of this host: a missing one, and the test's own named as the Client's.
    /// Verifies: ADR-0059
    #[cfg(unix)]
    #[test]
    fn a_process_account_is_decided_before_anything_is_written() {
        let me = RunAs::resolve(&me(), "supervisor", &Source::Flag, Path::new(DATA_ROOT))
            .expect("own account resolves");
        assert!(decide_process_account(None, false, None, Some(&me), true)
            .expect("none named")
            .is_none());
        let err = decide_process_account(Some("opamp-fleet-agents"), false, None, Some(&me), false)
            .expect_err("no second service here");
        assert!(err.contains("systemd"), "{err}");
        let err = decide_process_account(Some("opamp-fleet-agents"), false, None, None, true)
            .expect_err("a user service names none");
        assert!(err.contains("systemd"), "{err}");
        let err = decide_process_account(Some("bad\"account"), false, None, Some(&me), true)
            .expect_err("not writable into a unit");
        assert!(
            err.contains("cannot be written into a systemd unit"),
            "{err}"
        );
        assert!(
            err.contains("Nothing has been installed or written"),
            "{err}"
        );
        let err =
            decide_process_account(Some("no-such-account-0059"), false, None, Some(&me), true)
                .expect_err("missing");
        if me.uid() == 0 {
            assert!(err.contains("the Client runs as root"), "{err}");
        } else {
            assert!(err.contains("does not exist"), "{err}");
            let err = decide_process_account(Some(&me.account), false, None, Some(&me), true)
                .expect_err("the Client's own");
            assert!(err.contains("the Client runs as"), "{err}");
        }
    }

    /// The test's own account, and a group it may give its own files: a chown to that pair is one
    /// an unprivileged process is allowed, and a changed group shows what the walk reached. `None`
    /// where the test's user has no second group.
    #[cfg(unix)]
    fn me_with_another_group(owned: &Path) -> (RunAs, Option<u32>) {
        use std::os::unix::fs::MetadataExt as _;
        let me = RunAs::resolve(&me(), "supervisor", &Source::Flag, Path::new(DATA_ROOT))
            .expect("own account resolves");
        let groups = String::from_utf8(
            std::process::Command::new("id")
                .arg("-G")
                .output()
                .expect("id -G")
                .stdout,
        )
        .expect("utf8");
        let current = std::fs::metadata(owned).expect("metadata").gid();
        let other = groups
            .split_whitespace()
            .filter_map(|g| g.parse::<u32>().ok())
            .find(|g| *g != current);
        (me, other)
    }

    /// A tree as a Supervisor's directory stands where a process account is registered: the
    /// directories clause 6 gives the process account's group — the state directory passed
    /// through, a data directory written — and a configuration entry, all in `group`, beside a
    /// record of the Client's left in the group it was created with. Returns the entries in
    /// `group` with their modes, and the record.
    #[cfg(unix)]
    fn shared_tree(root: &Path, group: u32) -> (Vec<(PathBuf, u32)>, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let state = root.join("state");
        let data = state.join("data");
        let entry = state.join("config.yaml");
        std::fs::create_dir_all(&data).expect("mkdir");
        std::fs::write(&entry, b"x").expect("write");
        let record = state.join("package.json");
        std::fs::write(&record, b"{}").expect("write");
        let shared = vec![(state, 0o710), (data, 0o2770), (entry, 0o640)];
        for (path, mode) in &shared {
            std::os::unix::fs::chown(path, None, Some(group)).expect("chgrp");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode)).expect("chmod");
        }
        (shared, record)
    }

    #[cfg(unix)]
    fn mode_and_group(path: &Path) -> (u32, u32) {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::symlink_metadata(path).expect("metadata");
        (meta.mode() & 0o7777, meta.gid())
    }

    /// Where a process account is registered, the hand-over leaves its group where clause 6 put
    /// it: a directory in the process account's group keeps that group and its mode, and so does
    /// an entry in it, while the rest is handed over as before. A re-registration — every package
    /// upgrade — takes from no Managed Process what it reads or writes.
    /// Verifies: ADR-0059
    #[cfg(unix)]
    #[test]
    fn the_handover_keeps_the_process_accounts_group_and_its_modes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).expect("mkdir");
        let (me, other) = me_with_another_group(&root);
        let Some(group) = other else {
            return;
        };
        let (shared, record) = shared_tree(&root, group);
        // Another account's: nothing the test owns is skipped for its owner.
        let process = ProcessAccount {
            account: "agents".to_string(),
            uid: me.uid.wrapping_add(1),
            gid: group,
            kept_from: None,
        };
        let skipped = me.hand_over(&[&root], Some(&process)).expect("handover");
        assert!(skipped.is_empty(), "{skipped:?}");
        for (path, mode) in &shared {
            assert_eq!(mode_and_group(path), (*mode, group), "{}", path.display());
        }
        assert_eq!(mode_and_group(&record).1, me.gid);
        assert_eq!(mode_and_group(&root).1, me.gid);
    }

    /// An entry the process account owns is skipped with everything beneath it, whatever its
    /// group: the hand-over re-owns nothing of what a Managed Process wrote. The test's own
    /// account stands in for the process account, so the top and every entry beneath it are
    /// the process account's.
    /// Verifies: ADR-0059
    #[cfg(unix)]
    #[test]
    fn the_handover_skips_what_the_process_account_owns_with_everything_beneath_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).expect("mkdir");
        let (mut me, other) = me_with_another_group(&root);
        let Some(group) = other else {
            return;
        };
        let before = mode_and_group(&root).1;
        let (shared, record) = shared_tree(&root, before);
        me.gid = group;
        let process = ProcessAccount {
            account: "agents".to_string(),
            uid: me.uid,
            gid: group.wrapping_add(1),
            kept_from: None,
        };
        me.hand_over(&[&root], Some(&process)).expect("handover");
        for path in [&root, &record] {
            assert_eq!(mode_and_group(path).1, before, "{}", path.display());
        }
        for (path, mode) in &shared {
            assert_eq!(mode_and_group(path), (*mode, before), "{}", path.display());
        }
    }

    /// Without a process account the hand-over is what it always was: every entry, in whatever
    /// group, is handed to the Client's account and group.
    /// Verifies: ADR-0059
    #[cfg(unix)]
    #[test]
    fn without_a_process_account_the_handover_re_owns_every_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).expect("mkdir");
        let (me, other) = me_with_another_group(&root);
        let Some(group) = other else {
            return;
        };
        let (shared, record) = shared_tree(&root, group);
        me.hand_over(&[&root], None).expect("handover");
        for path in shared.iter().map(|(path, _)| path).chain([&record, &root]) {
            assert_eq!(mode_and_group(path).1, me.gid, "{}", path.display());
        }
    }

    /// The handover re-owns a tree including a symlink as a link, and skips what is missing.
    /// Verifies: ADR-0019
    #[cfg(unix)]
    #[test]
    fn the_handover_walks_the_tree_and_skips_what_is_missing() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let versions = root.join("versions/supervisor-1.0.0-abc");
        std::fs::create_dir_all(&versions).expect("mkdir");
        let binary = versions.join(crate::service::layout::BINARY_FILENAME);
        std::fs::write(&binary, b"binary").expect("write");
        std::os::unix::fs::symlink("versions/supervisor-1.0.0-abc", root.join("current"))
            .expect("symlink");

        let (mut me, other) = me_with_another_group(root);
        if let Some(group) = other {
            me.gid = group;
        }
        let missing = root.join("supervisor.toml");
        let skipped = me
            .hand_over(&[root, &missing], None)
            .expect("a missing config file is skipped, the tree is walked");
        assert!(skipped.is_empty(), "{skipped:?}");
        for reached in [root.to_path_buf(), versions, binary, root.join("current")] {
            let gid = std::fs::symlink_metadata(&reached).expect("exists").gid();
            assert_eq!(gid, me.gid, "{}", reached.display());
        }
    }

    /// A directory inside the tree that is a link to one outside it is re-owned as the link; the
    /// walk never reaches what it points to.
    /// Verifies: ADR-0042, ADR-0054
    #[cfg(unix)]
    #[test]
    fn the_handover_never_follows_a_directory_swapped_for_a_link() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("state")).expect("mkdir");
        std::fs::create_dir_all(outside.join("deeper")).expect("mkdir");
        std::fs::write(outside.join("secret"), b"x").expect("write");
        // `state` was a directory; the account swapped it for a link to one outside the root.
        std::fs::remove_dir(root.join("state")).expect("rmdir");
        std::os::unix::fs::symlink(&outside, root.join("state")).expect("symlink");

        let (mut me, other) = me_with_another_group(&root);
        let before = std::fs::metadata(&outside).expect("outside").gid();
        if let Some(group) = other {
            me.gid = group;
        }
        me.hand_over(&[&root, &root.join("state")], None)
            .expect("handover");
        for untouched in [
            outside.clone(),
            outside.join("secret"),
            outside.join("deeper"),
        ] {
            assert_eq!(
                std::fs::metadata(&untouched).expect("exists").gid(),
                before,
                "{} was reached through the link",
                untouched.display()
            );
        }
        assert_eq!(
            std::fs::symlink_metadata(root.join("state"))
                .expect("link")
                .gid(),
            me.gid,
            "the link itself is re-owned"
        );
    }

    /// A regular file with a second hard link is left as it is and named: its other name may stand
    /// outside the roots.
    /// Verifies: ADR-0042, ADR-0054
    #[cfg(unix)]
    #[test]
    fn the_handover_skips_a_file_with_a_second_hard_link() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).expect("mkdir");
        let outside = dir.path().join("outside-file");
        std::fs::write(&outside, b"x").expect("write");
        let linked = root.join("linked");
        std::fs::hard_link(&outside, &linked).expect("hard link");
        std::fs::write(root.join("plain"), b"y").expect("write");

        let (mut me, other) = me_with_another_group(&root);
        let before = std::fs::metadata(&outside).expect("outside").gid();
        if let Some(group) = other {
            me.gid = group;
        }
        let skipped = me.hand_over(&[&root], None).expect("handover");
        assert_eq!(skipped, vec![linked.clone()]);
        assert_eq!(std::fs::metadata(&outside).expect("outside").gid(), before);
        assert_eq!(
            std::fs::metadata(root.join("plain")).expect("plain").gid(),
            me.gid
        );
    }

    /// A directory swapped for a link to one outside the tree, after the walk read its name and
    /// before it opened it, is re-owned as the link; nothing outside is reached.
    /// Verifies: ADR-0042, ADR-0054
    #[cfg(unix)]
    #[test]
    fn the_handover_never_follows_a_directory_swapped_for_a_link_mid_walk() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("mkdir");
        std::fs::create_dir_all(outside.join("deeper")).expect("mkdir");
        std::fs::write(outside.join("secret"), b"x").expect("write");

        let (mut me, other) = me_with_another_group(&root);
        let before = std::fs::metadata(&outside).expect("outside").gid();
        if let Some(group) = other {
            me.gid = group;
        }
        let mut skipped = Vec::new();
        super::walk::chown_tree_with(&root, me.uid, me.gid, None, &mut skipped, &|entry| {
            if entry == state {
                std::fs::remove_dir(&state).expect("rmdir");
                std::os::unix::fs::symlink(&outside, &state).expect("symlink");
            }
        })
        .expect("handover");
        assert!(skipped.is_empty(), "{skipped:?}");
        for untouched in [
            outside.clone(),
            outside.join("secret"),
            outside.join("deeper"),
        ] {
            assert_eq!(
                std::fs::metadata(&untouched).expect("exists").gid(),
                before,
                "{} was reached through the link",
                untouched.display()
            );
        }
        assert_eq!(
            std::fs::symlink_metadata(&state).expect("link").gid(),
            me.gid,
            "the link itself is re-owned"
        );
    }

    /// An entry swapped for a hard link to a file outside the tree, after the walk read its name
    /// and before it opened it, is skipped and named; the file outside keeps its owner.
    /// Verifies: ADR-0042, ADR-0054
    #[cfg(unix)]
    #[test]
    fn the_handover_skips_an_entry_swapped_for_a_hard_link_mid_walk() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).expect("mkdir");
        let outside = dir.path().join("outside-file");
        std::fs::write(&outside, b"x").expect("write");
        std::fs::write(root.join("plain"), b"y").expect("write");
        // The entry starts as a link, the type a by-name re-own trusts.
        let swapped = root.join("swapped");
        std::os::unix::fs::symlink("plain", &swapped).expect("symlink");

        let (mut me, other) = me_with_another_group(&root);
        let before = std::fs::metadata(&outside).expect("outside").gid();
        if let Some(group) = other {
            me.gid = group;
        }
        let mut skipped = Vec::new();
        super::walk::chown_tree_with(&root, me.uid, me.gid, None, &mut skipped, &|entry| {
            if entry == swapped {
                std::fs::remove_file(&swapped).expect("unlink");
                std::fs::hard_link(&outside, &swapped).expect("hard link");
            }
        })
        .expect("handover");
        assert_eq!(
            std::fs::metadata(&outside).expect("outside").gid(),
            before,
            "the file outside was re-owned through its second name"
        );
        assert_eq!(skipped, vec![swapped]);
        assert_eq!(
            std::fs::metadata(root.join("plain")).expect("plain").gid(),
            me.gid
        );
    }
}
