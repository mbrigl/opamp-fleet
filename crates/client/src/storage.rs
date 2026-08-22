//! What the Client persists across restarts: its identity and the last received remote
//! configuration.
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

const UID_FILE: &str = "instance-uid";
const CONFIG_PB_FILE: &str = "remote-config.pb";
const CONFIG_DIR: &str = "config";
const PACKAGE_FILE: &str = "installed-package.json";

/// The role each delivered entry carries (ADR-0016) — `<name> <role>` per line, written into the
/// config directory beside the entries themselves.
///
/// It lives in that directory because that is where a plugin looks, and it is written only when
/// there is something to say — a fleet that never sets a role never sees this file. The leading
/// dot is what keeps it from colliding with an entry: a Configuration name follows the ADR-0010
/// grammar (lowercase letters, digits, `-`) and [`entry_file_name`] strips leading dots, so no
/// entry file can ever be named this.
///
/// **The value is here because the Baseline says it matters.** `AgentConfigFile.role` is defined as
/// *"Optional role of the content in the body field. The values and their semantics are Agent
/// type-specific"* — so a kind may define its own vocabulary, and to read it the value has to
/// survive the write. This file used to hold names alone, which answered only *whether* an entry
/// carried a role; a line without a second field still reads that way, which is exactly what an
/// older Client left behind.
pub const SUPPLEMENTARY_FILE: &str = ".supplementary";

/// The package this Supervisor's Managed Process currently runs (ADR-0015), persisted so a
/// restarted Client reports the version it has and is not re-offered it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub hash_hex: String,
}

pub struct Storage {
    dir: PathBuf,
}

impl Storage {
    pub fn new(dir: PathBuf) -> io::Result<Self> {
        create_private_dir(&dir)?;
        Ok(Storage { dir })
    }

    /// The persisted identity, or a fresh UUID v7 persisted on first start.
    pub fn load_or_create_uid(&self) -> io::Result<InstanceUid> {
        let path = self.dir.join(UID_FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Some(uid) = InstanceUid::parse(&text) {
                return Ok(uid);
            }
            warn!(file = %path.display(), "unreadable identity; generating a fresh one");
        }
        let uid = InstanceUid::default();
        std::fs::write(&path, format!("{uid}\n"))?;
        Ok(uid)
    }

    /// Persists a Server-assigned identity (AgentIdentification) so the reassignment survives a
    /// restart.
    pub fn save_uid(&self, uid: &InstanceUid) -> io::Result<()> {
        std::fs::write(self.dir.join(UID_FILE), format!("{uid}\n"))
    }

    /// Where [`store_remote_config`](Self::store_remote_config) writes the plain entry files —
    /// what a Managed Process is pointed at (ADR-0011).
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.dir.join(CONFIG_DIR)
    }

    /// The last stored remote configuration, if any survived a previous run.
    pub fn load_remote_config(&self) -> Option<AgentRemoteConfig> {
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
    /// removed first — the composed entry set changes over time (ADR-0012), and a stale file
    /// would otherwise still be handed to the Managed Process.
    ///
    /// Every entry is written, whatever its role. An entry that carries one (ADR-0016) is content
    /// the process reads *by path* rather than being configured with, so it lands in the same
    /// directory under the same name — that is what makes a `${file:...}` reference resolve — and
    /// its name is recorded in [`SUPPLEMENTARY_FILE`] for the plugin to leave out of what it
    /// starts the process with.
    pub fn store_remote_config(&self, config: &AgentRemoteConfig) -> io::Result<()> {
        // The protobuf and the entry files can carry secret material (a roled `${file:...}` entry
        // that is a certificate or key), so both the config directory and the files are owner-only.
        write_private(&self.dir.join(CONFIG_PB_FILE), &config.encode_to_vec())?;
        let config_dir = self.dir.join(CONFIG_DIR);
        create_private_dir(&config_dir)?;
        for entry in std::fs::read_dir(&config_dir)? {
            let path = entry?.path();
            if path.is_file() {
                std::fs::remove_file(path)?;
            }
        }
        let mut supplementary: Vec<String> = Vec::new();
        if let Some(map) = &config.config {
            for (name, file) in &map.config_map {
                let file_name = entry_file_name(name);
                write_private(&config_dir.join(&file_name), &file.body)?;
                if !file.role.is_empty() {
                    supplementary.push(format!("{file_name} {}", file.role));
                }
            }
        }
        if !supplementary.is_empty() {
            supplementary.sort();
            write_private(
                &config_dir.join(SUPPLEMENTARY_FILE),
                (supplementary.join("\n") + "\n").as_bytes(),
            )?;
        }
        Ok(())
    }

    /// The installed package (ADR-0015), if one survived a previous run.
    pub fn load_package(&self) -> Option<InstalledPackage> {
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
    pub fn store_package(&self, package: &InstalledPackage) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(package).expect("an InstalledPackage serializes");
        std::fs::write(self.dir.join(PACKAGE_FILE), json)
    }

    /// Drops the record: this Agent does not have that package. No record to drop is the state
    /// asked for, not an error.
    pub fn forget_package(&self) -> io::Result<()> {
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
/// (ADR-0016) and the bookkeeping that names them. Those files are deliberately still *there*:
/// supplementary content is read by path, so leaving it out of this list is the whole of what the
/// role changes.
///
/// A free function over the directory rather than a method, because a plugin is handed the
/// directory and not the [`Storage`] that filled it. An unreadable directory is no configuration
/// rather than an error — the caller's process simply has nothing to run on yet.
#[must_use]
pub fn config_entries(config_dir: &std::path::Path) -> Vec<PathBuf> {
    let supplementary = read_supplementary(config_dir);
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
            if name.starts_with('.') || supplementary.contains(&name) {
                return None;
            }
            Some(entry.path())
        })
        .collect();
    files.sort();
    files
}

/// The names listed in [`SUPPLEMENTARY_FILE`]; empty when there is none, which is the ordinary
/// case of a fleet that sets no roles.
fn read_supplementary(config_dir: &std::path::Path) -> Vec<String> {
    entry_roles(config_dir).into_keys().collect()
}

/// What role each delivered entry carries, by entry file name (ADR-0016).
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
/// config-map entry read by path (`${file:...}`) can be a certificate or a key (ADR-0016). At the
/// umask default the directory is world-listable, so on a multi-user host another local user could
/// read that material; owner-only closes it. The Managed Process runs as this same user, so it still
/// reads its own config. On Windows the `%ProgramData%` ACL is what protects it (ADR-0010).
pub(crate) fn create_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write `contents` to `path`, owner-only on Unix — for the files that can carry secret material
/// (the stored configuration protobuf and each config-map entry). Defence in depth beside the
/// `0700` directory: the mode is set in the open call so the bytes are never briefly world-readable,
/// and a pre-existing file is narrowed too.
fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
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

/// Config-map keys are arbitrary peer input; a file name derived from one must never escape the
/// config directory or hide itself.
fn entry_file_name(name: &str) -> String {
    if name.is_empty() {
        return "config".to_string();
    }
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
    sanitized.trim_start_matches('.').to_string()
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
    /// secret material by path (ADR-0016) — so the directories are owner-only and the secret-bearing
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
        // The composed entry set changes over time (ADR-0012); an entry dropped upstream must
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

    /// ADR-0016: a roled entry is written like any other — it has to be on disk for a
    /// `${file:...}` reference to resolve — but it is not among the files the process is
    /// configured with.
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

    /// Any other value is handled like `supplementary` and never guessed at (ADR-0016).
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

    /// A Client that stored entries before ADR-0016 has no bookkeeping file; everything it wrote
    /// is configuration, which is exactly what it was.
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
    }
}
