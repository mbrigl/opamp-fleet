//! The collector configuration this server distributes: one YAML file on disk.
//!
//! Its SHA-256 doubles as the OpAMP config hash. The agent reports back the hash it last received, so
//! comparing that hash with this file's is how the server knows whether an agent already runs what it
//! is supposed to run — the entire control loop turns on it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use sha2::{Digest, Sha256};

use crate::proto::{AgentConfigFile, AgentConfigMap, AgentRemoteConfig};

/// The key an agent's single, top-level configuration file is filed under in an OpAMP config map. The
/// supervisor writes the entry it finds here out as the collector's config. The specification says a
/// single-file agent SHOULD use an empty-string key.
const MAIN_CONFIG_KEY: &str = "";

/// A monotonic suffix for temp files, so two writes racing in the same directory never collide on a
/// name. Paired with the process id it is unique without needing a random source.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The collector configuration this server distributes.
pub struct ConfigSource {
    path: PathBuf,
    /// The configuration loaded by the most recent successful [`ConfigSource::reload`], or `None`
    /// until one succeeds.
    current: RwLock<Option<AgentRemoteConfig>>,
}

impl ConfigSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            current: RwLock::new(None),
        }
    }

    /// Re-reads the file and reports whether its content differs from what was loaded before, so that
    /// callers push to the fleet on an actual change rather than on every poll.
    pub fn reload(&self) -> io::Result<bool> {
        let body = fs::read(&self.path)?;
        let hash = Sha256::digest(&body);

        let mut current = self.current.write().expect("config lock poisoned");
        if let Some(cfg) = current.as_ref() {
            if cfg.config_hash == hash.as_slice() {
                return Ok(false);
            }
        }
        *current = Some(AgentRemoteConfig {
            config: Some(AgentConfigMap {
                config_map: [(
                    MAIN_CONFIG_KEY.to_string(),
                    AgentConfigFile {
                        body,
                        content_type: "text/yaml".to_string(),
                    },
                )]
                .into_iter()
                .collect(),
            }),
            config_hash: hash.to_vec(),
        });
        Ok(true)
    }

    /// Returns the configuration loaded by the most recent successful [`ConfigSource::reload`], or
    /// `None` if none has ever succeeded.
    pub fn current(&self) -> Option<AgentRemoteConfig> {
        self.current.read().expect("config lock poisoned").clone()
    }

    /// Returns the configuration as it is on disk right now. It deliberately does not serve the cached
    /// copy: an editor must show what the file actually says, not what the last poll happened to see.
    pub fn read(&self) -> io::Result<Vec<u8>> {
        fs::read(&self.path)
    }

    /// Replaces the configuration on disk. It does not push anything: the watcher notices the change
    /// and distributes it, so writing the file is the only way configuration ever reaches the fleet —
    /// from an editor, from the UI, from anywhere.
    ///
    /// The write goes through a temporary file and a rename so that a poll landing mid-write reads
    /// either the old configuration or the new one, never half of each.
    pub fn write(&self, body: &[u8]) -> io::Result<()> {
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".collector-{}-{}.yaml", process::id(), seq));

        // Best-effort cleanup if we fail before the rename; a successful rename leaves nothing behind.
        let result = (|| {
            fs::write(&tmp, body)?;
            // Pin sane file permissions on Unix; Windows uses ACLs (inherited from the directory), where
            // Unix mode bits do not apply, so this is skipped there.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
            }
            fs::rename(&tmp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        // Each test writes under a unique subdirectory of the scratch area so they never collide.
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("opamp-config-test-{}-{}", process::id(), seq));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reload_reports_change_then_stability() {
        let dir = temp_dir();
        let path = dir.join("collector.yaml");
        fs::write(&path, b"a: 1\n").unwrap();

        let src = ConfigSource::new(&path);
        assert!(src.reload().unwrap(), "first load is a change");
        assert!(!src.reload().unwrap(), "unchanged file is not a change");

        fs::write(&path, b"a: 2\n").unwrap();
        assert!(src.reload().unwrap(), "edited file is a change");
    }

    #[test]
    fn current_carries_hash_and_body_under_the_main_key() {
        let dir = temp_dir();
        let path = dir.join("collector.yaml");
        let body = b"exporters:\n  debug:\n";
        fs::write(&path, body).unwrap();

        let src = ConfigSource::new(&path);
        src.reload().unwrap();
        let cfg = src.current().expect("loaded");

        assert_eq!(cfg.config_hash, Sha256::digest(body).to_vec());
        let file = &cfg.config.unwrap().config_map[MAIN_CONFIG_KEY];
        assert_eq!(file.body, body);
        assert_eq!(file.content_type, "text/yaml");
    }

    #[test]
    fn write_is_atomic_and_reload_sees_it() {
        let dir = temp_dir();
        let path = dir.join("collector.yaml");
        fs::write(&path, b"old\n").unwrap();

        let src = ConfigSource::new(&path);
        src.reload().unwrap();

        src.write(b"new\n").unwrap();
        assert_eq!(src.read().unwrap(), b"new\n");
        assert!(
            src.reload().unwrap(),
            "the written change is seen on reload"
        );

        // No temp files were left behind in the directory.
        let strays: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".collector-"))
            .collect();
        assert!(strays.is_empty(), "temp files leaked: {strays:?}");
    }

    #[test]
    fn reload_surfaces_a_missing_file() {
        let dir = temp_dir();
        let src = ConfigSource::new(dir.join("does-not-exist.yaml"));
        assert!(src.reload().is_err());
        assert!(src.current().is_none());
    }
}
