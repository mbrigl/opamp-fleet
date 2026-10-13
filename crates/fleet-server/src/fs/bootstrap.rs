//! The bootstrap certificates the Server issued, on the filesystem (ADR-0028): the default
//! adapter behind [`BootstrapStore`](crate::bootstrap::BootstrapStore).

use std::path::PathBuf;

use crate::bootstrap::{BootstrapStore, Entry};

/// One JSON file per certificate, named after its serial, in a directory only its owner enters.
pub struct FsBootstrapStore {
    dir: PathBuf,
}

impl FsBootstrapStore {
    /// Opens the store, creating the directory.
    ///
    /// # Errors
    /// Returns an error when the directory cannot be created.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        crate::fs::create_private_dir(&dir)?;
        Ok(FsBootstrapStore { dir })
    }

    fn path(&self, serial: &str) -> Result<PathBuf, String> {
        // A serial is hex the Server wrote; anything else never names a file.
        if serial.is_empty() || !serial.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{serial:?} is not a serial"));
        }
        Ok(self.dir.join(format!("{serial}.json")))
    }
}

impl BootstrapStore for FsBootstrapStore {
    fn load(&self) -> Result<Vec<Entry>, String> {
        let entries = std::fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?;
        let mut loaded = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            loaded.push(
                serde_json::from_str(&text)
                    .map_err(|e| format!("cannot parse {}: {e}", path.display()))?,
            );
        }
        Ok(loaded)
    }

    fn put(&self, entry: &Entry) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(entry).expect("a bootstrap entry serializes");
        crate::fs::replace_owner_only(&self.path(&entry.serial)?, &json)
    }

    fn remove(&self, serial: &str) -> Result<(), String> {
        let path = self.path(serial)?;
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("cannot delete {}: {e}", path.display()))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revocation::CertId;

    /// What was issued is still listed after a restart, and a removal persists.
    #[test]
    fn entries_survive_a_reopen_and_a_removal_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = Entry {
            id: CertId::new(b"issuer", "a1"),
            serial: "a1".to_string(),
            subject: "O=Kunde, CN=fleet bootstrap".to_string(),
            key_fingerprint: "00".to_string(),
            issued_ms: 1,
            not_after_ms: 2,
            ca_key: "ab".to_string(),
        };
        let store = FsBootstrapStore::open(dir.path().join("b")).expect("open");
        store.put(&entry).expect("put");
        let reopened = FsBootstrapStore::open(dir.path().join("b")).expect("reopen");
        assert_eq!(reopened.load().expect("load"), vec![entry]);
        reopened.remove("a1").expect("remove");
        assert!(reopened.load().expect("load").is_empty());
        assert!(
            reopened.remove("../x").is_err(),
            "a serial never names a path"
        );
    }
}
