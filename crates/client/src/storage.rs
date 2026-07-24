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
/// The role each delivered entry carries (ADR-0011) — `<name> <role>` per line, written into the
/// config directory beside the entries themselves.
///
/// **The value is here because the Baseline says it matters.** `AgentConfigFile.role` is defined as
/// *"Optional role of the content in the body field. The values and their semantics are Agent
/// type-specific"* — so a kind may define its own vocabulary, and to read it the value has to
/// survive the write. This file used to hold names alone, which answered only *whether* an entry
/// carried a role; a line without a second field still reads that way, which is exactly what an
/// older Client left behind.

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
    /// what a Managed Process is pointed at (ADR-0010).
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
    pub fn store_remote_config(&self, config: &AgentRemoteConfig) -> io::Result<()> {
        // The protobuf and the entry files can carry secret material (a roled `${file:...}` entry
        // that is a certificate or key), so both the config directory and the files are owner-only.
        write_private(&self.dir.join(CONFIG_PB_FILE), &config.encode_to_vec())?;
        let config_dir = self.dir.join(CONFIG_DIR);
        create_private_dir(&config_dir)?;
        if let Some(map) = &config.config {
            for (name, file) in &map.config_map {
                write_private(&config_dir.join(&file_name), &file.body)?;
                    supplementary.push(format!("{file_name} {}", file.role));
            }
        }
            write_private(
                &config_dir.join(SUPPLEMENTARY_FILE),
                (supplementary.join("\n") + "\n").as_bytes(),
        Ok(())
    }
}

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
    entry_roles(config_dir).into_keys().collect()
}

/// What role each delivered entry carries, by entry file name (ADR-0011).
///
/// A line an older Client wrote carries no role, only a name; it reads back as an empty value —
/// "this entry carries *a* role", which is all that version ever recorded and all that the
/// pass-it-or-not decision needs. A kind that defines its own vocabulary asks for the value and
/// finds it as soon as the next configuration lands.
#[must_use]
pub fn entry_roles(config_dir: &std::path::Path) -> BTreeMap<String, String> {
        return BTreeMap::new();
        .map(|line| match line.split_once(char::is_whitespace) {
            Some((name, role)) => (name.to_string(), role.trim().to_string()),
            None => (line.to_string(), String::new()),
        })
/// Create `dir` (and its parents) and, on Unix, narrow it to `0700`.
///
/// The state directory holds the Agent's identity and the Server-pushed configuration, and a
/// config-map entry read by path (`${file:...}`) can be a certificate or a key (ADR-0011). At the
/// umask default the directory is world-listable, so on a multi-user host another local user could
/// read that material; owner-only closes it. The Managed Process runs as this same user, so it still
/// reads its own config. On Windows the `%ProgramData%` ACL is what protects it (ADR-0028).
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

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
    /// The state directory holds the identity and the Server-pushed configuration — which can carry
    /// secret material by path (ADR-0011) — so the directories are owner-only and the secret-bearing
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

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
            config: Some(AgentConfigMap {
                            AgentConfigObject {
                                role: String::new(),
    #[test]
    fn config_entries_are_files_only_and_sorted() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(config_entries(dir.path()).is_empty());
        std::fs::write(dir.path().join("b.yaml"), "b").expect("write");
        std::fs::write(dir.path().join("a.yaml"), "a").expect("write");
        std::fs::create_dir(dir.path().join("subdir")).expect("mkdir");
            .into_iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            config: Some(AgentConfigMap {
                            AgentConfigObject {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
    /// The value survives the write, because a kind may define its own vocabulary — the Baseline
    /// defines `role` as *"Agent type-specific"*, which is only usable if the value comes back.
    #[test]
    fn a_roles_value_is_readable_per_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
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
    /// A file an older Client wrote holds names alone. It reads back as "carries a role, value
    /// unknown", which is exactly what that version recorded and all the pass-it-or-not decision
    /// ever needed — so an update in flight does not start handing supplementary content to a
    /// Managed Process as configuration.
    #[test]
    fn a_file_written_before_roles_carried_their_value_still_excludes_its_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(&config_dir).expect("config dir");
        for name in ["base", "certs"] {
            std::fs::write(config_dir.join(name), b"x\n").expect("entry");
        }
        std::fs::write(config_dir.join(SUPPLEMENTARY_FILE), "certs\n").expect("old bookkeeping");

        assert_eq!(
            entry_roles(&config_dir).get("certs").map(String::as_str),
            Some(""),
            "a role is recorded, its value is not known"
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
    #[test]
    fn entry_names_cannot_escape_the_config_directory() {
        assert_eq!(entry_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(entry_file_name(""), "config");
        assert_eq!(entry_file_name("collector.yaml"), "collector.yaml");
    }
}
