//! The register and revocation list on the filesystem (ADR-0049): the default adapter behind
//! [`LedgerStore`](crate::revocation::LedgerStore).

use std::path::PathBuf;

use std::collections::BTreeMap;

use crate::revocation::{CertId, Host, Issued, Ledger, LedgerStore, Revocation};

/// A directory of its own under `config_dir`, owner-only: one JSON file per certificate in the
/// register under `issued/`, so a renewal writes one small file, and the list in
/// `revocations.json`. Every file is replaced in one step.
pub struct FsLedgerStore {
    issued: PathBuf,
    revocations: PathBuf,
    hosts: PathBuf,
}

impl FsLedgerStore {
    /// Opens the store in `dir`, creating it and `issued/` beneath it owner-only.
    ///
    /// # Errors
    /// Returns an error when a directory cannot be created.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        super::create_private_dir(&dir)?;
        let issued = dir.join("issued");
        super::create_private_dir(&issued)?;
        Ok(FsLedgerStore {
            issued,
            revocations: dir.join("revocations.json"),
            hosts: dir.join("hosts.json"),
        })
    }

    fn path(&self, id: &CertId) -> PathBuf {
        self.issued.join(format!(
            "{}-{}.json",
            &id.issuer[..16.min(id.issuer.len())],
            id.serial
        ))
    }
}

impl LedgerStore for FsLedgerStore {
    fn load(&self) -> Result<Ledger, String> {
        let mut issued = Vec::new();
        let entries = std::fs::read_dir(&self.issued)
            .map_err(|e| format!("cannot read {}: {e}", self.issued.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", self.issued.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes =
                std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            issued.push(
                serde_json::from_slice::<Issued>(&bytes)
                    .map_err(|e| format!("cannot parse {}: {e}", path.display()))?,
            );
        }
        let revocations = match std::fs::read(&self.revocations) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("cannot parse {}: {e}", self.revocations.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", self.revocations.display())),
        };
        let hosts = match std::fs::read(&self.hosts) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("cannot parse {}: {e}", self.hosts.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", self.hosts.display())),
        };
        Ok(Ledger {
            issued,
            revocations,
            hosts,
        })
    }

    fn put_issued(&self, issued: &Issued) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(issued).expect("a register entry serializes");
        super::replace_owner_only(&self.path(&issued.facts.id), &json)
    }

    fn remove_issued(&self, id: &CertId) -> Result<(), String> {
        let path = self.path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot delete {}: {e}", path.display())),
        }
    }

    fn save_hosts(&self, hosts: &BTreeMap<String, Host>) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(hosts).expect("the hosts serialize");
        super::replace_owner_only(&self.hosts, &json)
    }

    fn save_revocations(&self, revocations: &[Revocation]) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(revocations).expect("the list serializes");
        super::replace_owner_only(&self.revocations, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revocation::{Facts, Revoked};

    /// Verifies: ADR-0056
    #[test]
    fn the_ledger_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsLedgerStore::open(dir.path().join("revocation")).expect("open");
        let issued = Issued {
            facts: Facts {
                id: CertId::new(b"ca", "0a"),
                issuer_name: "CN=ca".into(),
                subject: "CN=edge-01".into(),
                key_fingerprint: "k".into(),
                not_after_ms: 9,
                host: None,
            },
            instance_uid: "00".into(),
            predecessor: None,
            issued_ms: 1,
        };
        store.put_issued(&issued).expect("put");
        let revocation = Revocation {
            id: "x".into(),
            revoked: Revoked::Credential { sha256: "s".into() },
            revoked_ms: 2,
        };
        store
            .save_revocations(std::slice::from_ref(&revocation))
            .expect("save");
        let reopened = FsLedgerStore::open(dir.path().join("revocation")).expect("open");
        let ledger = reopened.load().expect("load");
        assert_eq!(ledger.issued, vec![issued.clone()]);
        assert_eq!(ledger.revocations, vec![revocation]);
        reopened.remove_issued(&issued.facts.id).expect("remove");
        assert!(reopened.load().expect("load").issued.is_empty());
    }
}
