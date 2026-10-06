//! Server-set labels on the filesystem (ADR-0013): the default adapter behind
//! [`LabelStore`](crate::labels::LabelStore).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::RwLock;

use opamp::uid::InstanceUid;

use crate::labels::LabelStore;

/// One file per Agent under a directory the Configuration store's loader ignores, so a write
/// touches nothing else and clearing a set is a deletion. Loaded whole when opened and held in
/// memory after that.
pub struct FsLabelStore {
    dir: PathBuf,
    labels: RwLock<HashMap<InstanceUid, BTreeMap<String, String>>>,
}

impl FsLabelStore {
    /// Opens the store, creating the directory and loading every persisted set. A file that does
    /// not parse fails startup rather than being skipped: a rollout channel that silently vanished is
    /// worse than one that refuses to start (ADR-0025's principle).
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let mut labels = HashMap::new();
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| format!("cannot read the Agent identity from {}", path.display()))?;
            let uid = InstanceUid::parse(stem)
                .ok_or_else(|| format!("{} is not named after an Instance UID", path.display()))?;
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let set: BTreeMap<String, String> = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            labels.insert(uid, set);
        }
        Ok(FsLabelStore {
            dir,
            labels: RwLock::new(labels),
        })
    }
}

impl LabelStore for FsLabelStore {
    fn get(&self, uid: &InstanceUid) -> BTreeMap<String, String> {
        self.labels
            .read()
            .expect("labels lock")
            .get(uid)
            .cloned()
            .unwrap_or_default()
    }

    fn put(&self, uid: &InstanceUid, set: BTreeMap<String, String>) -> Result<(), String> {
        let path = self.dir.join(format!("{uid}.json"));
        if set.is_empty() {
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("cannot delete {}: {e}", path.display()));
                }
            }
            self.labels.write().expect("labels lock").remove(uid);
            return Ok(());
        }
        let json = serde_json::to_vec_pretty(&set).expect("labels serialize");
        crate::fs::replace(&path, &json)?;
        self.labels.write().expect("labels lock").insert(*uid, set);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// A channel assignment that evaporated with a restart would be worse than none, because it would
    /// evaporate quietly.
    #[test]
    fn labels_survive_a_reopen_and_an_empty_set_clears_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uid = InstanceUid::default();
        {
            let store = FsLabelStore::open(dir.path().to_path_buf()).expect("open");
            store
                .put(&uid, labels(&[("rollout", "canary")]))
                .expect("put");
            assert_eq!(store.get(&uid), labels(&[("rollout", "canary")]));
        }

        let reopened = FsLabelStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(reopened.get(&uid), labels(&[("rollout", "canary")]));

        reopened.put(&uid, BTreeMap::new()).expect("clear");
        assert!(reopened.get(&uid).is_empty());
        let again = FsLabelStore::open(dir.path().to_path_buf()).expect("reopen");
        assert!(again.get(&uid).is_empty(), "the clear persisted too");
    }
}
