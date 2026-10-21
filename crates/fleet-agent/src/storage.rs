//! What the Client persists across restarts — its identity, the last received remote
//! configuration, and the installed package — on the filesystem: the adapter behind
//! [`AgentStorage`]. Beside it, [`HighWaterFile`] keeps the high-water marks every Supervisor
//! shares (ADR-0043).
//!
//! The identity file keeps the `instance_uid` stable across restarts, as the Baseline recommends.
//! The remote configuration is stored losslessly as the received protobuf, plus one plain file per
//! config-map entry so an operator (and, later, a Managed Process) can read it off disk.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use opamp::proto::AgentRemoteConfig;
use opamp::uid::InstanceUid;
use prost::Message;
use tracing::warn;

use crate::supervisor::ports::{AgentStorage, InstalledPackage};

/// The file that keeps the `instance_uid`.
pub const UID_FILE: &str = "instance-uid";
const CONFIG_PB_FILE: &str = "remote-config.pb";
const CONFIG_DIR: &str = "config";
const PACKAGE_FILE: &str = "installed-package.json";

/// The role each delivered entry carries (ADR-0023) — `<name> <role>` per line, written into the
/// config directory beside the entries themselves.
///
/// It lives in that directory because that is where a plugin looks, and it is written only when
/// there is something to say — a fleet that never sets a role never sees this file. The leading
/// dot is what keeps it from colliding with an entry: a Configuration name follows the ADR-0019
/// grammar (lowercase letters, digits, `-`) and [`entry_file_name`] strips leading dots, so no
/// entry file can ever be named this.
///
/// **The value is here because the Baseline says it matters.** `AgentConfigFile.role` is defined as
/// *"Optional role of the content in the body field. The values and their semantics are Agent
/// type-specific"* — so a kind may define its own vocabulary, and to read it the value has to
/// survive the write. A line without a second field — what an older Client wrote — answers only
/// *whether* an entry carries a role.
pub const SUPPLEMENTARY_FILE: &str = ".supplementary";

pub struct Storage {
    dir: PathBuf,
    /// The process group, where a process account is registered (ADR-0053 clause 6): the
    /// directory is passed through by it, and what is stored is kept from it.
    group: Option<ProcessGroup>,
    /// Whether `config/` is read by the Managed Process, and so shared with the process group: a
    /// Supervisor's, never the Client's own.
    config_shared: bool,
}

impl Storage {
    /// The storage in `dir`, kept to the Client alone, as where no process account is registered.
    pub fn new(dir: PathBuf) -> io::Result<Self> {
        Storage::open(dir, None, false)
    }

    /// The Client's own storage in the state directory: passed through by the process group on the
    /// way to the Supervisors' directories, and nothing in it shared (ADR-0053 clause 6).
    pub fn client(dir: PathBuf, group: Option<ProcessGroup>) -> io::Result<Self> {
        Storage::open(dir, group, false)
    }

    /// A Supervisor's storage in its own directory: passed through by the process group, with
    /// `config/` read by its Managed Process and the rest kept from it (ADR-0053 clause 6).
    pub fn supervised(dir: PathBuf, group: Option<ProcessGroup>) -> io::Result<Self> {
        Storage::open(dir, group, true)
    }

    fn open(dir: PathBuf, group: Option<ProcessGroup>, config_shared: bool) -> io::Result<Self> {
        create_shared_dir(&dir, group, Shared::Traversed)?;
        let storage = Storage {
            dir,
            group,
            config_shared,
        };
        storage.share_config()?;
        Ok(storage)
    }

    /// Shares what `config/` already holds with the process group — files written before a process
    /// account was registered, or placed there by the operator — so the Managed Process reads them
    /// before the next configuration is written. Nothing where `config/` is not shared, or does
    /// not exist yet.
    fn share_config(&self) -> io::Result<()> {
        let Some(group) = self.config_group() else {
            return Ok(());
        };
        let config_dir = self.config_dir();
        if !config_dir.is_dir() {
            return Ok(());
        }
        create_shared_dir(&config_dir, Some(group), Shared::Read)?;
        #[cfg(unix)]
        for entry in std::fs::read_dir(&config_dir)? {
            let entry = entry?;
            // Never through a link, and never a directory: only what the Client writes there.
            if entry.file_type()?.is_file() {
                share_file(&entry.path(), group)?;
            }
        }
        Ok(())
    }

    /// The process group `config/` is shared with, if it is.
    fn config_group(&self) -> Option<ProcessGroup> {
        self.group.filter(|_| self.config_shared)
    }

    /// Writes one of the Client's own records. In a directory the process group passes through it
    /// is kept from that group, `0600`; elsewhere it is written as it always was, at the umask.
    fn write_record(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        if self.group.is_some() {
            write_private(path, contents)
        } else {
            std::fs::write(path, contents)
        }
    }

    /// Where [`store_remote_config`](Self::store_remote_config) writes the plain entry files —
    /// what a Managed Process is pointed at (ADR-0017).
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.dir.join(CONFIG_DIR)
    }

    /// Takes a stored remote configuration out of force for a Supervisor whose remote
    /// configuration is switched off (ADR-0017 clause 52): each entry file, and `.supplementary`,
    /// whose bytes are still the ones the stored map would write is deleted, every other file in
    /// `config/` stays, and `remote-config.pb` goes. A `.pb` that does not decode is deleted and
    /// `config/` is left alone. `None` when nothing was stored.
    ///
    /// # Errors
    /// Returns an error when a file that has to go cannot be read or deleted.
    pub fn drop_remote_config(&self) -> io::Result<Option<DroppedRemoteConfig>> {
        let pb = self.dir.join(CONFIG_PB_FILE);
        let bytes = match std::fs::read(&pb) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let Ok(config) = AgentRemoteConfig::decode(bytes.as_slice()) else {
            std::fs::remove_file(&pb)?;
            return Ok(Some(DroppedRemoteConfig::Undecodable));
        };
        let config_dir = self.config_dir();
        let mut kept = Vec::new();
        for (file_name, body) in written_files(&config) {
            let path = config_dir.join(&file_name);
            match std::fs::read(&path) {
                Ok(on_disk) if on_disk == body => std::fs::remove_file(&path)?,
                Ok(_) => kept.push(file_name),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        std::fs::remove_file(&pb)?;
        Ok(Some(DroppedRemoteConfig::Removed {
            hash: config.config_hash,
            kept,
        }))
    }
}

impl Storage {
    /// Takes the Client's own stored Supervisor set out of the way on a host that keeps its set
    /// (ADR-0017 clause 44): `remote-config.pb` goes **first**, so nothing that fails after it can
    /// leave a hash to report, and then each entry copy in `config/` whose bytes are still the
    /// stored ones goes as far as it can. Nothing runs on the copies, so one that cannot be read
    /// or deleted is named in the result, not an error. A `.pb` that does not decode is deleted
    /// and `config/` is left alone. `None` when nothing was stored.
    ///
    /// # Errors
    /// Returns an error only when `remote-config.pb` itself cannot be read or deleted.
    pub fn drop_stored_set(&self) -> io::Result<Option<DroppedStoredSet>> {
        let pb = self.dir.join(CONFIG_PB_FILE);
        let bytes = match std::fs::read(&pb) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        std::fs::remove_file(&pb)?;
        let Ok(config) = AgentRemoteConfig::decode(bytes.as_slice()) else {
            return Ok(Some(DroppedStoredSet::Undecodable));
        };
        let config_dir = self.config_dir();
        let (mut kept, mut unremoved) = (Vec::new(), Vec::new());
        for (file_name, body) in written_files(&config) {
            let path = config_dir.join(&file_name);
            match std::fs::read(&path) {
                Ok(on_disk) if on_disk == body => {
                    if let Err(e) = std::fs::remove_file(&path) {
                        unremoved.push((file_name, e.to_string()));
                    }
                }
                Ok(_) => kept.push(file_name),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => unremoved.push((file_name, e.to_string())),
            }
        }
        Ok(Some(DroppedStoredSet::Removed {
            hash: config.config_hash,
            kept,
            unremoved,
        }))
    }
}

/// What [`Storage::drop_stored_set`] found and did (ADR-0017 clause 44).
#[derive(Debug, PartialEq, Eq)]
pub enum DroppedStoredSet {
    /// `remote-config.pb` is gone, and so is every entry copy still as stored; `kept` names the
    /// copies whose content had changed, `unremoved` those that could not be read or deleted, each
    /// with the error.
    Removed {
        hash: Vec<u8>,
        kept: Vec<String>,
        unremoved: Vec<(String, String)>,
    },
    /// The stored `remote-config.pb` did not decode and is gone; `config/` was left as it was.
    Undecodable,
}

/// What [`Storage::drop_remote_config`] found and did (ADR-0017 clause 52).
#[derive(Debug, PartialEq, Eq)]
pub enum DroppedRemoteConfig {
    /// The stored configuration and the files it wrote are gone; `kept` names the files
    /// left in place because their content had changed since.
    Removed { hash: Vec<u8>, kept: Vec<String> },
    /// The stored `remote-config.pb` did not decode and is gone; `config/` was left as it was.
    Undecodable,
}

/// The files [`Storage::store_remote_config`] writes into `config/` for `config`, by file name:
/// each entry, then [`SUPPLEMENTARY_FILE`] when any entry carries a role.
fn written_files(config: &AgentRemoteConfig) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut supplementary: Vec<String> = Vec::new();
    if let Some(map) = &config.config {
        for (name, file) in &map.config_map {
            let file_name = entry_file_name(name);
            if !file.role.is_empty() {
                supplementary.push(format!("{file_name} {}", file.role));
            }
            files.push((file_name, file.body.clone()));
        }
    }
    if !supplementary.is_empty() {
        supplementary.sort();
        files.push((
            SUPPLEMENTARY_FILE.to_string(),
            (supplementary.join("\n") + "\n").into_bytes(),
        ));
    }
    files
}

impl AgentStorage for Storage {
    /// The persisted identity, or a fresh UUID v7 persisted on first start.
    fn load_or_create_uid(&self) -> io::Result<InstanceUid> {
        let path = self.dir.join(UID_FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Some(uid) = InstanceUid::parse(&text) {
                return Ok(uid);
            }
            warn!(file = %path.display(), "unreadable identity; generating a fresh one");
        }
        let uid = InstanceUid::default();
        self.save_uid(&uid)?;
        Ok(uid)
    }

    /// Persists a Server-assigned identity (AgentIdentification) so the reassignment survives a
    /// restart.
    fn save_uid(&self, uid: &InstanceUid) -> io::Result<()> {
        self.write_record(&self.dir.join(UID_FILE), format!("{uid}\n").as_bytes())
    }

    /// The last stored remote configuration, if any survived a previous run.
    fn load_remote_config(&self) -> Option<AgentRemoteConfig> {
        let bytes = std::fs::read(self.dir.join(CONFIG_PB_FILE)).ok()?;
        match AgentRemoteConfig::decode(bytes.as_slice()) {
            Ok(config) => Some(config),
            Err(e) => {
                warn!(error = %e, "stored remote configuration is unreadable; ignoring it");
                None
            }
        }
    }

    /// Stores a received remote configuration: the protobuf for lossless restart, and each
    /// config-map entry as a plain file under `config/`. Entry files from a previous offer are
    /// removed first — the composed entry set changes over time (ADR-0023), and a stale file
    /// would otherwise still be handed to the Managed Process.
    ///
    /// Every entry is written, whatever its role. An entry that carries one (ADR-0023) is content
    /// the process reads *by path* rather than being configured with, so it lands in the same
    /// directory under the same name — that is what makes a `${file:...}` reference resolve — and
    /// its name is recorded in [`SUPPLEMENTARY_FILE`] for the plugin to leave out of what it
    /// starts the process with.
    fn store_remote_config(&self, config: &AgentRemoteConfig) -> io::Result<()> {
        // Refused before the previous offer's files go: a refusal leaves them as they were.
        check_entry_names(config).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // The protobuf and the entry files can carry secret material (a roled `${file:...}` entry
        // that is a certificate or key), so both the config directory and the files are owner-only.
        // Where a process account is registered, a Supervisor's `config/` is what its Managed
        // Process reads: shared with the process group to read, never to write (ADR-0053
        // clause 6).
        let config_dir = self.dir.join(CONFIG_DIR);
        create_shared_dir(&config_dir, self.config_group(), Shared::Read)?;
        for entry in std::fs::read_dir(&config_dir)? {
            let path = entry?.path();
            if path.is_file() {
                std::fs::remove_file(path)?;
            }
        }
        for (file_name, body) in written_files(config) {
            write_shared(&config_dir.join(file_name), &body, self.config_group())?;
        }
        // Last: a store cut short leaves the previous `.pb`, never a new one beside the previous
        // offer's files, which `drop_remote_config` would keep as the operator's (ADR-0017
        // clause 32).
        write_private(&self.dir.join(CONFIG_PB_FILE), &config.encode_to_vec())
    }

    /// The installed package (ADR-0026), if one survived a previous run.
    fn load_package(&self) -> Option<InstalledPackage> {
        let text = std::fs::read_to_string(self.dir.join(PACKAGE_FILE)).ok()?;
        match serde_json::from_str(&text) {
            Ok(package) => Some(package),
            Err(e) => {
                warn!(error = %e, "stored package record is unreadable; ignoring it");
                None
            }
        }
    }

    /// Records the installed package, so a restarted Client reports what it runs.
    fn store_package(&self, package: &InstalledPackage) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(package).expect("an InstalledPackage serializes");
        self.write_record(&self.dir.join(PACKAGE_FILE), &json)
    }

    /// Drops the record: this Agent does not have that package. No record to drop is the state
    /// asked for, not an error.
    fn forget_package(&self) -> io::Result<()> {
        match std::fs::remove_file(self.dir.join(PACKAGE_FILE)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// The entry files a Managed Process should be **configured with**, in deterministic (sorted)
/// order — the Collector's own merge semantics are order-dependent.
///
/// Everything written by [`Storage::store_remote_config`] except the entries that carry a role
/// (ADR-0023) and the bookkeeping that names them. Those files are deliberately still *there*:
/// supplementary content is read by path, so leaving it out of this list is the whole of what the
/// role changes.
///
/// A free function over the directory rather than a method, because a plugin is handed the
/// directory and not the [`Storage`] that filled it. An unreadable directory is no configuration
/// rather than an error — the caller's process simply has nothing to run on yet.
#[must_use]
pub fn config_entries(config_dir: &std::path::Path) -> Vec<PathBuf> {
    let supplementary = entry_roles(config_dir);
    let Ok(entries) = std::fs::read_dir(config_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    warn!(dir = %config_dir.display(), error = %e, "unreadable config entry");
                    return None;
                }
            };
            if !entry.file_type().is_ok_and(|t| t.is_file()) {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // No entry file can begin with a dot (`entry_file_name` strips them), so anything
            // that does is this module's own bookkeeping and never configuration.
            if name.starts_with('.') || supplementary.contains_key(&name) {
                return None;
            }
            Some(entry.path())
        })
        .collect();
    files.sort();
    files
}

/// What role each delivered entry carries, by entry file name (ADR-0023).
///
/// A line an older Client wrote carries no role, only a name; it reads back as an empty value —
/// "this entry carries *a* role", which is all that version ever recorded and all that the
/// pass-it-or-not decision needs. A kind that defines its own vocabulary asks for the value and
/// finds it as soon as the next configuration lands.
#[must_use]
pub fn entry_roles(config_dir: &std::path::Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(config_dir.join(SUPPLEMENTARY_FILE)) else {
        return BTreeMap::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| match line.split_once(char::is_whitespace) {
            Some((name, role)) => (name.to_string(), role.trim().to_string()),
            None => (line.to_string(), String::new()),
        })
        .collect()
}

/// Create `dir` (and its parents) and, on Unix, narrow it to `0700`.
///
/// The state directory holds the Agent's identity and the Server-pushed configuration, and a
/// config-map entry read by path (`${file:...}`) can be a certificate or a key (ADR-0023). At the
/// umask default the directory is world-listable, so on a multi-user host another local user could
/// read that material; owner-only closes it. Where no process account is registered the Managed
/// Process runs as this same user, so it still reads its own config; where one is, what it reads
/// is shared with its group instead ([`create_shared_dir`]). On Windows the `%ProgramData%` ACL is what protects it (ADR-0019).
pub(crate) fn create_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The process account's primary group, where a process account is registered (ADR-0053 clause 6).
/// What the Client shares with the Managed Processes belongs to this group, and nothing else does.
///
/// Resolved from the `--process-uid` of the Client's command line when it starts, never from the
/// configuration file: the registration that names the process account is the only place it is
/// stated (ADR-0053 clause 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessGroup(pub u32);

/// How a directory the Client creates is shared with the process group (ADR-0053 clause 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shared {
    /// Passed through on the way to what is shared, never listed: `0710`. The state directory, the
    /// Supervisors' root and each Supervisor's directory.
    Traversed,
    /// Read, never written: `0750`. `program/`, the preflight's staging, `config/`.
    Read,
    /// Written by the Managed Processes: `2770`, so what they create there keeps the group and
    /// the Client can remove it. A kind's data directories.
    Written,
}

impl Shared {
    #[cfg(unix)]
    fn mode(self) -> u32 {
        match self {
            Shared::Traversed => 0o710,
            Shared::Read => 0o750,
            Shared::Written => 0o2770,
        }
    }
}

/// Create `dir` (and its parents) shared with `group` as `shared` says: owned by the group, at the
/// mode of ADR-0053 clause 6. Without a process group it is [`create_private_dir`], exactly as
/// where none is registered.
///
/// The parents this creates are not shared: each that must be is named by its own call.
pub(crate) fn create_shared_dir(
    dir: &Path,
    group: Option<ProcessGroup>,
    shared: Shared,
) -> io::Result<()> {
    let Some(group) = group else {
        return create_private_dir(dir);
    };
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Never through a link: what is shared is the directory the Client made.
        if !std::fs::symlink_metadata(dir)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not a directory", dir.display()),
            ));
        }
        // The group first: a change of group may clear the set-group-ID bit the mode sets.
        std::os::unix::fs::lchown(dir, None, Some(group.0))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(shared.mode()))?;
    }
    #[cfg(not(unix))]
    let _ = (group, shared);
    Ok(())
}

/// Makes the regular file at `path` readable by `group` and no wider: `0640`, owned by the group.
#[cfg(unix)]
fn share_file(path: &Path, group: ProcessGroup) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    std::os::unix::fs::fchown(&file, None, Some(group.0))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o640))
}

/// Write `contents` to `path` readable by `group`: `0640`, owned by the group (ADR-0053 clause 6).
/// Without a process group it is [`write_private`].
pub(crate) fn write_shared(
    path: &Path,
    contents: &[u8],
    group: Option<ProcessGroup>,
) -> io::Result<()> {
    let Some(group) = group else {
        return write_private(path, contents);
    };
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        std::os::unix::fs::fchown(&file, None, Some(group.0))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o640))?;
        file.write_all(contents)
    }
    #[cfg(not(unix))]
    {
        let _ = group;
        std::fs::write(path, contents)
    }
}

/// Write `contents` to `path`, owner-only on Unix — for the files that can carry secret material
/// (the stored configuration protobuf and each config-map entry). Defence in depth beside the
/// `0700` directory: the mode is set in the open call so the bytes are never briefly world-readable,
/// and a pre-existing file is narrowed too.
pub(crate) fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(contents)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

/// Refuses a configuration whose entries cannot each be stored as a file of their own on every
/// host: a configuration means the same wherever it lands, so what Windows cannot hold is refused
/// on Linux and macOS too.
///
/// An entry file name may not be a Windows device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
/// `LPT1`–`LPT9`, in any case and with any extension) nor end in a dot, which Windows strips. Two
/// entries may not map to one file name, compared without case, as a case-insensitive file system
/// compares them: which of them was written would depend on the map's iteration order.
///
/// # Errors
/// Returns a sentence naming the entry, or the two entries, refused.
pub fn check_entry_names(config: &AgentRemoteConfig) -> Result<(), String> {
    let Some(map) = &config.config else {
        return Ok(());
    };
    let mut names: Vec<&String> = map.config_map.keys().collect();
    names.sort();
    let mut seen: BTreeMap<String, &String> = BTreeMap::new();
    for name in names {
        let file_name = entry_file_name(name);
        let folded = file_name.to_ascii_lowercase();
        let stem = folded.split('.').next().unwrap_or_default();
        if crate::config::WINDOWS_RESERVED.contains(&stem) {
            return Err(format!(
                "entry {name:?} would be stored as {file_name:?}, a reserved device name on Windows"
            ));
        }
        if file_name.ends_with('.') {
            return Err(format!(
                "entry {name:?} would be stored as {file_name:?}, a name ending in a dot, which \
                 Windows cannot hold"
            ));
        }
        if let Some(other) = seen.insert(folded, name) {
            return Err(format!(
                "entries {other:?} and {name:?} would be stored in one file, {file_name:?}"
            ));
        }
    }
    Ok(())
}

/// Config-map keys are arbitrary peer input; a file name derived from one must never escape the
/// config directory or hide itself.
fn entry_file_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // An empty key, or one of nothing but dots, leaves no name at all — and joined onto the config
    // directory that is the directory itself.
    match sanitized.trim_start_matches('.') {
        "" => "config".to_string(),
        name => name.to_string(),
    }
}

/// The file under `state_dir` that holds the high-water marks (ADR-0043 clause 8). It sits outside
/// `supervisor_dir`, and its name holds a `.`, which no Supervisor name can: no purge of a
/// Supervisor's directory deletes it, even where `supervisor_dir` is `state_dir` itself.
pub const HIGH_WATER_FILE: &str = "package-high-water.json";

/// The high-water marks on the filesystem (ADR-0043 clause 8): `<state_dir>/package-high-water.json`,
/// a JSON object mapping an Agent type to the highest version this Client installed for it,
/// written owner-only through a temporary file and a rename.
///
/// Read once, when the Client starts. A file that exists but does not parse, or that maps a type to
/// a value that is not SemVer, is never read as empty: every mark then answers the reason, which
/// refuses every Supervisor package until the operator repairs or deletes the file (clause 11).
pub struct HighWaterFile {
    path: PathBuf,
    marks: std::sync::Mutex<Result<BTreeMap<String, String>, String>>,
}

impl HighWaterFile {
    /// Reads the marks under `state_dir`. No file is no mark; an unreadable one is logged as an
    /// error naming it.
    #[must_use]
    pub fn open(state_dir: &Path) -> Self {
        let path = state_dir.join(HIGH_WATER_FILE);
        let marks = match std::fs::read(&path) {
            Ok(bytes) => parse_marks(&bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e.to_string()),
        }
        .map_err(|why| {
            let reason = format!(
                "the high-water file {} cannot be read ({why}) — every Supervisor package is \
                 refused until it is repaired or deleted",
                path.display()
            );
            tracing::error!("{reason}");
            reason
        });
        HighWaterFile {
            path,
            marks: std::sync::Mutex::new(marks),
        }
    }

    fn write(&self, marks: &BTreeMap<String, String>) -> io::Result<()> {
        let mut json = serde_json::to_vec_pretty(marks).expect("a map of strings serializes");
        json.push(b'\n');
        let tmp = self.path.with_extension("json.tmp");
        write_private(&tmp, &json)?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// The marks a file holds, or why they cannot be read: it must be a JSON object of strings, each
/// a SemVer version.
fn parse_marks(bytes: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let marks: BTreeMap<String, String> =
        serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if let Some((agent_type, version)) = marks
        .iter()
        .find(|(_, version)| !fleet_core::version::is_semver(version))
    {
        return Err(format!(
            "the mark of {agent_type:?} is {version:?}, which is not SemVer"
        ));
    }
    Ok(marks)
}

impl crate::supervisor::ports::HighWaterMarks for HighWaterFile {
    fn mark(&self, agent_type: &str) -> Result<Option<String>, String> {
        match &*self.marks.lock().expect("marks lock") {
            Ok(marks) => Ok(marks.get(agent_type).cloned()),
            Err(reason) => Err(reason.clone()),
        }
    }

    fn raise(&self, agent_type: &str, version: &str) {
        if !fleet_core::version::is_semver(version) {
            return;
        }
        let mut guard = self.marks.lock().expect("marks lock");
        // An unreadable file stays as the operator finds it: it is theirs to repair.
        let Ok(marks) = &mut *guard else {
            return;
        };
        let higher = marks.get(agent_type).is_none_or(|mark| {
            fleet_core::version::precedence(version, mark) == Some(std::cmp::Ordering::Greater)
        });
        if !higher {
            return;
        }
        marks.insert(agent_type.to_string(), version.to_string());
        if let Err(e) = self.write(marks) {
            warn!(
                file = %self.path.display(), agent_type, version, error = %e,
                "cannot write the high-water mark; it is kept in memory, and the next start seeds \
                 it again from the installed-package records"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::proto::{AgentConfigMap, AgentConfigObject};
    use std::collections::HashMap;

    #[test]
    fn identity_is_stable_across_restarts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let first = storage.load_or_create_uid().expect("uid");
        let second = storage.load_or_create_uid().expect("uid");
        assert_eq!(first, second);
    }

    #[test]
    fn a_forgotten_package_is_gone_and_forgetting_nothing_is_no_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");

        // Nothing recorded yet: the state asked for is the state there is.
        storage.forget_package().expect("forget nothing");

        storage
            .store_package(&InstalledPackage {
                name: "opamp-fleet-client".to_string(),
                version: "1.2.3".to_string(),
                hash_hex: "aabb".to_string(),
            })
            .expect("store");
        assert!(storage.load_package().is_some());

        storage.forget_package().expect("forget");
        assert!(storage.load_package().is_none());
        assert!(!dir.path().join(PACKAGE_FILE).exists());
    }

    /// The state directory holds the identity and the Server-pushed configuration — which can carry
    /// secret material by path (ADR-0023) — so the directories are owner-only and the secret-bearing
    /// files are `0600`, whatever the process umask.
    #[cfg(unix)]
    #[test]
    fn the_state_and_configuration_are_kept_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("state");
        let storage = Storage::new(root.clone()).expect("storage");
        let mode = |p: &std::path::Path| {
            std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777
        };
        assert_eq!(mode(&root), 0o700, "the state directory is owner-only");

        storage
            .store_remote_config(&roled_offer(&[("certs", b"PEM-SECRET\n", "supplementary")]))
            .expect("store");
        let config_dir = storage.config_dir();
        assert_eq!(
            mode(&config_dir),
            0o700,
            "the config directory is owner-only"
        );
        assert_eq!(
            mode(&root.join(CONFIG_PB_FILE)),
            0o600,
            "the stored configuration protobuf is owner-only"
        );
        assert_eq!(
            mode(&config_dir.join("certs")),
            0o600,
            "a config entry — which may be a certificate or key — is owner-only"
        );
    }

    /// The group this test may share with: its own primary group, of which it is a member as the
    /// Client is of the process account's.
    #[cfg(unix)]
    fn own_group() -> ProcessGroup {
        // SAFETY: getegid(2) cannot fail and touches no memory.
        ProcessGroup(unsafe { libc::getegid() })
    }

    #[cfg(unix)]
    fn mode_and_group(path: &Path) -> (u32, u32) {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::symlink_metadata(path).expect("metadata");
        (meta.mode() & 0o7777, meta.gid())
    }

    /// Where a process account is registered, the Client shares with the process group what the
    /// Managed Process reads and nothing else: the state directory and a Supervisor's directory
    /// are passed through, `0710`; a Supervisor's `config/` and its entries are read, `0750` and
    /// `0640`; a data directory is written, `2770`. The Client's own `config/`, the stored
    /// protobuf, the identity and the package record stay the Client's, `0700` and `0600`.
    /// Verifies: ADR-0053
    #[cfg(unix)]
    #[test]
    fn with_a_process_account_the_process_group_reads_only_what_it_runs_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        let group = own_group();
        let state = dir.path().join("state");
        let client = Storage::client(state.clone(), Some(group)).expect("the Client's storage");
        client
            .store_remote_config(&roled_offer(&[("set", b"[[supervisor]]\n", "")]))
            .expect("store the Client's own");
        client
            .save_uid(&InstanceUid::default())
            .expect("the Client's identity");
        assert_eq!(mode_and_group(&state), (0o710, group.0));
        assert_eq!(mode_and_group(&client.config_dir()).0, 0o700);
        assert_eq!(mode_and_group(&client.config_dir().join("set")).0, 0o600);
        assert_eq!(mode_and_group(&state.join(UID_FILE)).0, 0o600);

        let supervisor = state.join("supervisors").join("agent");
        let storage =
            Storage::supervised(supervisor.clone(), Some(group)).expect("a Supervisor's storage");
        storage
            .store_remote_config(&roled_offer(&[("certs", b"PEM\n", "supplementary")]))
            .expect("store");
        storage
            .save_uid(&InstanceUid::default())
            .expect("the Supervisor's identity");
        storage
            .store_package(&InstalledPackage {
                name: "agent".to_string(),
                version: "1.0.0".to_string(),
                hash_hex: "01".to_string(),
            })
            .expect("the package record");
        assert_eq!(mode_and_group(&supervisor), (0o710, group.0));
        assert_eq!(mode_and_group(&storage.config_dir()), (0o750, group.0));
        assert_eq!(
            mode_and_group(&storage.config_dir().join("certs")),
            (0o640, group.0)
        );
        assert_eq!(
            mode_and_group(&storage.config_dir().join(SUPPLEMENTARY_FILE)),
            (0o640, group.0)
        );
        assert_eq!(mode_and_group(&supervisor.join(CONFIG_PB_FILE)).0, 0o600);
        assert_eq!(mode_and_group(&supervisor.join(UID_FILE)).0, 0o600);
        assert_eq!(mode_and_group(&supervisor.join(PACKAGE_FILE)).0, 0o600);

        let data = supervisor.join("data");
        create_shared_dir(&data, Some(group), Shared::Written).expect("a data directory");
        assert_eq!(mode_and_group(&data), (0o2770, group.0));
    }

    /// What `config/` held before a process account was registered is shared when the
    /// Supervisor's storage opens, so its Managed Process reads it before the next configuration
    /// arrives; a link there is left alone.
    /// Verifies: ADR-0053
    #[cfg(unix)]
    #[test]
    fn what_config_held_before_is_shared_when_the_storage_opens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let supervisor = dir.path().join("agent");
        let before = Storage::new(supervisor.clone()).expect("without a process account");
        before
            .store_remote_config(&roled_offer(&[("fleet", b"service: {}\n", "")]))
            .expect("store");
        std::os::unix::fs::symlink("/etc/hostname", before.config_dir().join("link"))
            .expect("a link the operator placed");
        assert_eq!(mode_and_group(&before.config_dir().join("fleet")).0, 0o600);

        let group = own_group();
        let after = Storage::supervised(supervisor, Some(group)).expect("with one");
        assert_eq!(mode_and_group(&after.config_dir()), (0o750, group.0));
        assert_eq!(
            mode_and_group(&after.config_dir().join("fleet")),
            (0o640, group.0)
        );
    }

    /// Without a process account every mode is what it was before there was one: directories
    /// `0700`, the stored configuration `0600`, and the identity and the package record at the
    /// umask, as a plain write leaves them.
    /// Verifies: ADR-0053
    #[cfg(unix)]
    #[test]
    fn without_a_process_account_every_mode_stays_as_it_was() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reference = dir.path().join("reference");
        std::fs::write(&reference, b"x").expect("a plain write");
        let at_umask = mode_and_group(&reference);

        let supervisor = dir.path().join("agent");
        let storage = Storage::supervised(supervisor.clone(), None).expect("storage");
        storage
            .store_remote_config(&roled_offer(&[("certs", b"PEM\n", "")]))
            .expect("store");
        storage.save_uid(&InstanceUid::default()).expect("identity");
        storage
            .store_package(&InstalledPackage {
                name: "agent".to_string(),
                version: "1.0.0".to_string(),
                hash_hex: "01".to_string(),
            })
            .expect("package");
        assert_eq!(mode_and_group(&supervisor).0, 0o700);
        assert_eq!(mode_and_group(&storage.config_dir()).0, 0o700);
        assert_eq!(mode_and_group(&storage.config_dir().join("certs")).0, 0o600);
        assert_eq!(mode_and_group(&supervisor.join(CONFIG_PB_FILE)).0, 0o600);
        assert_eq!(mode_and_group(&supervisor.join(UID_FILE)), at_umask);
        assert_eq!(mode_and_group(&supervisor.join(PACKAGE_FILE)), at_umask);
        let data = supervisor.join("data");
        create_shared_dir(&data, None, Shared::Written).expect("a data directory");
        assert_eq!(mode_and_group(&data).0, 0o700);
    }

    #[test]
    fn remote_config_round_trips_and_writes_plain_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let config = AgentRemoteConfig {
            config: Some(AgentConfigMap {
                config_map: HashMap::from([(
                    String::new(),
                    AgentConfigObject {
                        role: String::new(),
                        body: b"receivers: {}\n".to_vec(),
                        content_type: String::new(),
                    },
                )]),
            }),
            config_hash: vec![1, 2, 3],
        };
        storage.store_remote_config(&config).expect("store");
        assert_eq!(storage.load_remote_config(), Some(config));
        let plain = std::fs::read(dir.path().join("config").join("config")).expect("plain file");
        assert_eq!(plain, b"receivers: {}\n");
    }

    #[test]
    fn a_new_offer_replaces_the_previous_entry_files() {
        // The composed entry set changes over time (ADR-0023); an entry dropped upstream must
        // not survive on disk, where it would still be handed to the Managed Process.
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let offer = |entries: &[(&str, &[u8])]| AgentRemoteConfig {
            config: Some(AgentConfigMap {
                config_map: entries
                    .iter()
                    .map(|(name, body)| {
                        (
                            name.to_string(),
                            AgentConfigObject {
                                role: String::new(),
                                body: body.to_vec(),
                                content_type: String::new(),
                            },
                        )
                    })
                    .collect(),
            }),
            config_hash: vec![1],
        };
        storage
            .store_remote_config(&offer(&[("base", b"a\n"), ("extra", b"b\n")]))
            .expect("store");
        storage
            .store_remote_config(&offer(&[("base", b"a2\n")]))
            .expect("store again");

        let config_dir = dir.path().join("config");
        assert_eq!(
            std::fs::read(config_dir.join("base")).expect("kept entry"),
            b"a2\n"
        );
        assert!(
            !config_dir.join("extra").exists(),
            "the dropped entry is gone"
        );
    }

    /// Moved here with the function itself: what the Collector plugin is started with is now
    /// decided by the module that wrote the files.
    #[test]
    fn config_entries_are_files_only_and_sorted() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(config_entries(dir.path()).is_empty());
        std::fs::write(dir.path().join("b.yaml"), "b").expect("write");
        std::fs::write(dir.path().join("a.yaml"), "a").expect("write");
        std::fs::create_dir(dir.path().join("subdir")).expect("mkdir");
        assert_eq!(entry_names(dir.path()), vec!["a.yaml", "b.yaml"]);
    }

    fn entry_names(config_dir: &std::path::Path) -> Vec<String> {
        config_entries(config_dir)
            .into_iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect()
    }

    fn roled_offer(entries: &[(&str, &[u8], &str)]) -> AgentRemoteConfig {
        AgentRemoteConfig {
            config: Some(AgentConfigMap {
                config_map: entries
                    .iter()
                    .map(|(name, body, role)| {
                        (
                            name.to_string(),
                            AgentConfigObject {
                                role: role.to_string(),
                                body: body.to_vec(),
                                content_type: String::new(),
                            },
                        )
                    })
                    .collect(),
            }),
            config_hash: vec![7],
        }
    }

    /// ADR-0023: a roled entry is written like any other — it has to be on disk for a
    /// `${file:...}` reference to resolve — but it is not among the files the process is
    /// configured with.
    // Verifies: ADR-0023
    #[test]
    fn a_roled_entry_is_written_but_not_offered_as_configuration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        storage
            .store_remote_config(&roled_offer(&[
                ("base", b"receivers: {}\n", ""),
                ("ruleset", b"rules: []\n", "supplementary"),
            ]))
            .expect("store");

        let config_dir = storage.config_dir();
        assert_eq!(
            std::fs::read(config_dir.join("ruleset")).expect("written"),
            b"rules: []\n",
            "supplementary content is on disk for the process to read by path"
        );
        assert_eq!(entry_names(&config_dir), vec!["base"]);
    }

    /// Any other value is handled like `supplementary` and never guessed at (ADR-0023).
    #[test]
    fn an_unknown_role_is_treated_like_supplementary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        storage
            .store_remote_config(&roled_offer(&[
                ("base", b"a\n", ""),
                ("certs", b"PEM\n", "some-agents-own-word"),
            ]))
            .expect("store");
        assert_eq!(entry_names(&storage.config_dir()), vec!["base"]);
    }

    /// The value survives the write, because a kind may define its own vocabulary — the Baseline
    /// defines `role` as *"Agent type-specific"*, which is only usable if the value comes back.
    #[test]
    fn a_roles_value_is_readable_per_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        storage
            .store_remote_config(&roled_offer(&[
                ("base", b"a\n", ""),
                ("root", b"b\n", "main"),
                ("certs", b"PEM\n", "supplementary"),
            ]))
            .expect("store");

        let roles = entry_roles(&storage.config_dir());
        assert_eq!(roles.get("root").map(String::as_str), Some("main"));
        assert_eq!(
            roles.get("certs").map(String::as_str),
            Some("supplementary")
        );
        assert_eq!(roles.get("base"), None, "an unroled entry is not listed");
        // And the older question — is this entry configuration? — answers as it always did.
        assert_eq!(entry_names(&storage.config_dir()), vec!["base"]);
    }

    /// A file an older Client wrote holds names alone. It reads back as "carries a role, value
    /// unknown", which is exactly what that version recorded and all the pass-it-or-not decision
    /// ever needed — so an update in flight does not start handing supplementary content to a
    /// Managed Process as configuration.
    #[test]
    fn a_file_written_before_roles_carried_their_value_still_excludes_its_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("config dir");
        for name in ["base", "certs"] {
            std::fs::write(config_dir.join(name), b"x\n").expect("entry");
        }
        std::fs::write(config_dir.join(SUPPLEMENTARY_FILE), "certs\n").expect("old bookkeeping");

        assert_eq!(
            entry_roles(&config_dir).get("certs").map(String::as_str),
            Some(""),
            "a role is recorded, its value is not known"
        );
        assert_eq!(entry_names(&config_dir), vec!["base"]);
    }

    /// The common case writes no bookkeeping at all, and a role that is later removed must not
    /// leave an entry excluded forever.
    #[test]
    fn the_bookkeeping_appears_only_while_a_role_does() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let config_dir = storage.config_dir();

        storage
            .store_remote_config(&roled_offer(&[("base", b"a\n", "")]))
            .expect("store");
        assert!(!config_dir.join(SUPPLEMENTARY_FILE).exists());

        storage
            .store_remote_config(&roled_offer(&[("base", b"a\n", "supplementary")]))
            .expect("store");
        assert!(config_dir.join(SUPPLEMENTARY_FILE).exists());
        assert!(entry_names(&config_dir).is_empty());

        storage
            .store_remote_config(&roled_offer(&[("base", b"a\n", "")]))
            .expect("store");
        assert!(
            !config_dir.join(SUPPLEMENTARY_FILE).exists(),
            "the role is gone, and so is what recorded it"
        );
        assert_eq!(entry_names(&config_dir), vec!["base"]);
    }

    /// A state directory with no bookkeeping file holds configuration alone, so everything in it
    /// reads as configuration (ADR-0023).
    #[test]
    fn entries_without_bookkeeping_are_all_configuration() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("base"), "a").expect("write");
        std::fs::write(dir.path().join("extra"), "b").expect("write");
        assert_eq!(entry_names(dir.path()), vec!["base", "extra"]);
    }

    #[test]
    fn entry_names_cannot_escape_the_config_directory() {
        assert_eq!(entry_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(entry_file_name(""), "config");
        assert_eq!(entry_file_name("collector.yaml"), "collector.yaml");
        // Nothing but dots strips to nothing, and an empty name would be the config directory
        // itself.
        assert_eq!(entry_file_name("."), "config");
        assert_eq!(entry_file_name("..."), "config");
    }

    /// A configuration means the same on every host, so an entry name Windows cannot hold as a
    /// file — a device name, with or without an extension, or a name ending in a dot — is refused
    /// on every host, and the previous offer's files stay as they were. The refusal is what the
    /// Agent reports `FAILED` with (ADR-0017 clause 7).
    /// Verifies: ADR-0017
    #[test]
    fn an_entry_name_windows_cannot_hold_is_refused_on_every_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        storage
            .store_remote_config(&roled_offer(&[("base", b"a\n", "")]))
            .expect("store");

        for name in [
            "CON", "con", "Prn", "aux", "NUL", "nul.yaml", "com1", "COM9.txt", "lpt1", "LPT9.x.y",
            "base.", "a..",
        ] {
            let error = storage
                .store_remote_config(&roled_offer(&[(name, b"x\n", "")]))
                .expect_err(name);
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name}");
            assert!(error.to_string().contains(name), "{name}: {error}");
        }
        assert_eq!(entry_names(&storage.config_dir()), vec!["base"]);
        for name in ["console", "com10", "lpt", "nul-x", "con_"] {
            check_entry_names(&roled_offer(&[(name, b"x\n", "")]))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    /// Two entries that would land in one file — equal once mapped, or equal to a case-insensitive
    /// file system — are refused rather than one silently overwriting the other in an order the map
    /// decides.
    /// Verifies: ADR-0017
    #[test]
    fn entries_that_would_share_a_file_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        for pair in [
            ["A", "a"],
            ["x y", "x_y"],
            ["", "config"],
            ["Base.yaml", "base.YAML"],
        ] {
            let error = storage
                .store_remote_config(&roled_offer(&[
                    (pair[0], b"1\n", ""),
                    (pair[1], b"2\n", ""),
                ]))
                .expect_err(&format!("{pair:?}"));
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{pair:?}");
            assert!(
                !storage.config_dir().exists() || entry_names(&storage.config_dir()).is_empty(),
                "{pair:?}: nothing written"
            );
        }
    }

    #[test]
    fn an_entry_named_only_dots_is_stored_as_a_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        storage
            .store_remote_config(&roled_offer(&[("...", b"a\n", "")]))
            .expect("store");
        let plain = std::fs::read(dir.path().join("config").join("config")).expect("plain file");
        assert_eq!(plain, b"a\n");
    }

    /// A store cut short leaves the previous `remote-config.pb`, never a new one beside the
    /// previous offer's files: the drop then still reads the previous map and deletes what it
    /// wrote instead of keeping it as the operator's (ADR-0017 clause 52). The store is stopped
    /// by a directory standing where the next offer's entry file goes — `create_private_dir`
    /// resets the mode of `config/`, so a failed removal cannot be staged from here.
    /// Verifies: ADR-0017
    #[test]
    fn a_store_cut_short_leaves_no_previous_entry_file_beside_a_new_pb() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut previous = roled_offer(&[("a", b"1\n", "")]);
        previous.config_hash = vec![1];
        storage.store_remote_config(&previous).expect("store");
        let config_dir = storage.config_dir();
        std::fs::create_dir(config_dir.join("b")).expect("in the way");

        let mut next = roled_offer(&[("b", b"x\n", "")]);
        next.config_hash = vec![2];
        storage
            .store_remote_config(&next)
            .expect_err("the entry file cannot be written");

        assert_eq!(
            storage
                .load_remote_config()
                .map(|stored| stored.config_hash),
            Some(vec![1]),
            "the stored map still describes what is on disk"
        );
        std::fs::remove_dir(config_dir.join("b")).expect("clear the way");
        assert_eq!(
            storage.drop_remote_config().expect("drop"),
            Some(DroppedRemoteConfig::Removed {
                hash: vec![1],
                kept: Vec::new()
            })
        );
        assert!(
            entry_names(&config_dir).is_empty(),
            "{:?}",
            entry_names(&config_dir)
        );
    }
}
