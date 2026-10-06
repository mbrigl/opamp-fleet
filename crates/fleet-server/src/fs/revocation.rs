//! The register and revocation list on the filesystem (ADR-0065): the default adapter behind
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

impl FsLedgerStore {
    /// The list as persisted, without the credential entries a list may still hold: the Agent
    /// plane admits by client certificate alone, so there is no credential to revoke (ADR-0065
    /// clause 5). They are dropped with one log line, and the list is written back without them.
    fn certificate_revocations(&self, bytes: &[u8]) -> Result<Vec<Revocation>, String> {
        let parse =
            |e: serde_json::Error| format!("cannot parse {}: {e}", self.revocations.display());
        let mut entries: Vec<serde_json::Value> = serde_json::from_slice(bytes).map_err(parse)?;
        let before = entries.len();
        entries.retain(|entry| entry.pointer("/revoked/credential").is_none());
        let dropped = before - entries.len();
        let revocations = entries
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<Revocation>, _>>()
            .map_err(parse)?;
        if dropped > 0 {
            tracing::warn!(
                dropped,
                file = %self.revocations.display(),
                "dropped credential revocations: the Agent plane admits by client certificate alone"
            );
            self.save_revocations(&revocations)?;
        }
        Ok(revocations)
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
            Ok(bytes) => self.certificate_revocations(&bytes)?,
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

    /// Verifies: ADR-0065
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
            revoked: Revoked::Certificate {
                authority: "client".into(),
                issuers: vec![issued.facts.id.issuer.clone()],
                serial: "0a".into(),
            },
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

    /// A writer the log goes to, for a test to read back.
    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log lock").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Verifies: ADR-0065
    #[test]
    fn a_persisted_credential_entry_is_dropped_on_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsLedgerStore::open(dir.path().join("revocation")).expect("open");
        let path = dir.path().join("revocation").join("revocations.json");
        std::fs::write(
            &path,
            r#"[
              {"id": "c1", "revoked": {"credential": {"sha256": "5eed"}}, "revoked_ms": 1},
              {"id": "a2", "revoked": {"certificate":
                {"authority": "client", "issuers": ["ab"], "serial": "a2"}}, "revoked_ms": 2},
              {"id": "c3", "revoked": {"credential": {"sha256": "beef"}}, "revoked_ms": 3}
            ]"#,
        )
        .expect("seed");

        let log = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer({
                let log = log.clone();
                move || log.clone()
            })
            .with_ansi(false)
            .finish();
        let ledger = tracing::subscriber::with_default(subscriber, || store.load()).expect("load");

        let certificate = Revocation {
            id: "a2".into(),
            revoked: Revoked::Certificate {
                authority: "client".into(),
                issuers: vec!["ab".into()],
                serial: "a2".into(),
            },
            revoked_ms: 2,
        };
        assert_eq!(ledger.revocations, vec![certificate.clone()]);
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            !written.contains("credential"),
            "written back without it: {written}"
        );
        assert_eq!(
            store.load().expect("load again").revocations,
            vec![certificate]
        );
        let log = String::from_utf8(log.0.lock().expect("log lock").clone()).expect("utf-8");
        assert_eq!(
            log.lines().filter(|line| line.contains("dropped")).count(),
            1,
            "one log line: {log}"
        );
        assert!(log.contains("dropped=2"), "names the number dropped: {log}");
    }
}
