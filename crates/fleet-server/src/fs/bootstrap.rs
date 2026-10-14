//! The bootstrap certificates the Server issued, on the filesystem (ADR-0028): the default
//! adapter behind [`BootstrapStore`](crate::bootstrap::BootstrapStore).

use std::path::PathBuf;

use crate::bootstrap::{BootstrapStore, Entry};

/// One JSON file per certificate, named after its serial, in a directory only its owner enters, and
/// beside it the certificate's PKCS#12 file, `<serial>.p12`, when one was made (ADR-0029).
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
        self.named(serial, "json")
    }

    fn p12_path(&self, serial: &str) -> Result<PathBuf, String> {
        self.named(serial, "p12")
    }

    fn named(&self, serial: &str, extension: &str) -> Result<PathBuf, String> {
        // A serial is hex the Server wrote; anything else never names a file.
        if serial.is_empty() || !serial.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{serial:?} is not a serial"));
        }
        Ok(self.dir.join(format!("{serial}.{extension}")))
    }
}

/// Removes a file, and is content when it is not there.
fn remove(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("cannot delete {}: {e}", path.display()))
        }
        _ => Ok(()),
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
        remove(&self.p12_path(serial)?)?;
        remove(&self.path(serial)?)
    }

    fn put_p12(&self, serial: &str, file: &[u8]) -> Result<(), String> {
        crate::fs::replace_owner_only(&self.p12_path(serial)?, file)
    }

    fn p12(&self, serial: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.p12_path(serial)?;
        match std::fs::read(&path) {
            Ok(file) => Ok(Some(file)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    fn remove_p12(&self, serial: &str) -> Result<(), String> {
        remove(&self.p12_path(serial)?)
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
            p12: false,
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

    /// The PKCS#12 file lies beside its entry, owner-only, and goes with it.
    /// Verifies: ADR-0029
    #[test]
    fn a_pkcs12_file_is_kept_owner_only_and_removed_with_its_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsBootstrapStore::open(dir.path().join("b")).expect("open");
        store.put_p12("a1", b"file").expect("put");
        assert_eq!(store.p12("a1").expect("read"), Some(b"file".to_vec()));
        assert_eq!(store.p12("b2").expect("read"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("b/a1.p12"))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "owner-only");
        }
        assert!(store.load().expect("load").is_empty(), "a file is no entry");
        store.remove_p12("a1").expect("remove");
        assert_eq!(store.p12("a1").expect("read"), None);
        store.put_p12("a1", b"file").expect("put");
        store.remove("a1").expect("remove");
        assert_eq!(store.p12("a1").expect("read"), None, "gone with its entry");
        assert!(store.p12("../x").is_err(), "a serial never names a path");
    }
}
