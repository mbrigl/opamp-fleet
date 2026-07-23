//! What the Client persists across restarts: its identity and the last received remote
//! configuration.
//!
//! The identity file keeps the `instance_uid` stable across restarts, as the Baseline recommends.
//! The remote configuration is stored losslessly as the received protobuf, plus one plain file per
//! config-map entry so an operator (and, later, a Managed Process) can read it off disk.

// Consumed by the transports that arrive with ADR-0007; unit-tested below.
#![allow(dead_code)]

use std::io;
use std::path::PathBuf;

use opamp::proto::AgentRemoteConfig;
use opamp::uid::InstanceUid;
use prost::Message;
use tracing::warn;

const UID_FILE: &str = "instance-uid";
const CONFIG_PB_FILE: &str = "remote-config.pb";
const CONFIG_DIR: &str = "config";

pub struct Storage {
    dir: PathBuf,
}

impl Storage {
    pub fn new(dir: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
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
    /// config-map entry as a plain file under `config/`.
    pub fn store_remote_config(&self, config: &AgentRemoteConfig) -> io::Result<()> {
        std::fs::write(self.dir.join(CONFIG_PB_FILE), config.encode_to_vec())?;
        let config_dir = self.dir.join(CONFIG_DIR);
        std::fs::create_dir_all(&config_dir)?;
        if let Some(map) = &config.config {
            for (name, file) in &map.config_map {
                std::fs::write(config_dir.join(entry_file_name(name)), &file.body)?;
            }
        }
        Ok(())
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
    use opamp::proto::{AgentConfigFile, AgentConfigMap};
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
    fn remote_config_round_trips_and_writes_plain_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let config = AgentRemoteConfig {
            config: Some(AgentConfigMap {
                config_map: HashMap::from([(
                    String::new(),
                    AgentConfigFile {
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
    fn entry_names_cannot_escape_the_config_directory() {
        assert_eq!(entry_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(entry_file_name(""), "config");
        assert_eq!(entry_file_name("collector.yaml"), "collector.yaml");
    }
}
