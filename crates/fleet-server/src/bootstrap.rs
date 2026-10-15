//! Bootstrap certificates the Server issues (ADR-0028).
//!
//! An operator asks for one with a subject of their choosing — a customer, a site, a batch — and
//! is handed the certificate and its private key once. The Server keeps the certificate's facts,
//! never its key in the clear, so it can list what is valid and an operator can revoke one by its
//! serial (ADR-0021 clause 34). Asked with a password, the key never leaves in PEM: the Server
//! seals it with its certificate into a PKCS#12 file under that password, keeps the file and
//! hands it out on request until the certificate ends or is revoked; the password it keeps
//! nowhere (ADR-0029). A bootstrap certificate opens nothing on its own: it needs an open
//! enrolment window and an operator's approval (ADR-0021 clauses 20, 21).
//!
//! The signing is the [`BootstrapSigner`] port, the bootstrap CA in [`crate::ca`] its adapter; the
//! record is the [`BootstrapStore`] port, a directory under `config_dir` its adapter.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::fleet::Clock;
use crate::revocation::CertId;

/// A bootstrap certificate's life when the request names none (ADR-0028 clause 23).
pub const DEFAULT_VALIDITY_DAYS: u32 = 10;

/// The longest life a request may ask for (ADR-0028 clause 23).
pub const MAX_VALIDITY_DAYS: u32 = 90;

/// The valid bootstrap certificates the record holds at most (ADR-0028 clause 24).
pub const MAX_VALID: usize = 10_000;

/// The longest value of a subject attribute: RFC 5280's upper bound for the names that have one.
pub const MAX_VALUE_CHARS: usize = 64;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// The subject an operator names (ADR-0028 clause 22): the organization and its unit, each
/// optional. The Server sets the common name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    /// O — the customer, typically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    /// OU — the site, typically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizational_unit: Option<String>,
}

/// An attribute of a bootstrap certificate's subject, in the order a certificate lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attribute {
    Organization,
    OrganizationalUnit,
    CommonName,
}

impl Subject {
    /// The attributes to sign, in certificate order, ending with the common name the Server sets.
    ///
    /// # Errors
    /// Returns the `400` text for a value that is empty, longer than [`MAX_VALUE_CHARS`], or
    /// carries a control character.
    pub fn attributes(&self, common_name: &str) -> Result<Vec<(Attribute, String)>, String> {
        let named = [
            (Attribute::Organization, "organization", &self.organization),
            (
                Attribute::OrganizationalUnit,
                "organizational_unit",
                &self.organizational_unit,
            ),
        ];
        let mut attributes = Vec::new();
        for (attribute, name, value) in named {
            let Some(value) = value else { continue };
            let chars = value.chars().count();
            if chars == 0 || chars > MAX_VALUE_CHARS {
                return Err(format!(
                    "subject.{name} must be 1 to {MAX_VALUE_CHARS} characters"
                ));
            }
            if value.chars().any(char::is_control) {
                return Err(format!("subject.{name} must not carry a control character"));
            }
            attributes.push((attribute, value.clone()));
        }
        attributes.push((Attribute::CommonName, common_name.to_string()));
        Ok(attributes)
    }
}

/// What an operator asks for (`POST /api/v1/bootstrap-certificates`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueRequest {
    #[serde(default)]
    pub subject: Subject,
    /// 1 to 90; 10 when absent.
    #[serde(default)]
    pub validity_days: Option<u32>,
    /// The password of the PKCS#12 file to make instead of handing out the key (ADR-0029).
    #[serde(default)]
    pub password: Option<Password>,
}

/// A password, which no `Debug` and no log line shows (ADR-0029 clause 2).
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Password(pub String);

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(..)")
    }
}

/// A certificate the signer made, with the key that belongs to it — in PEM, or sealed with the
/// certificate into a PKCS#12 file under the password the request named.
#[derive(Clone)]
pub struct Pair {
    pub id: CertId,
    /// The serial as the certificate carries it, hex.
    pub serial: String,
    /// The subject as text.
    pub subject: String,
    /// The SHA-256 of the public key, hex.
    pub key_fingerprint: String,
    pub not_before_ms: u64,
    pub not_after_ms: u64,
    pub certificate_pem: String,
    /// The key, when no password was named.
    pub private_key_pem: Option<String>,
    /// The PKCS#12 file, when a password was named (ADR-0029 clause 3).
    pub pkcs12: Option<Vec<u8>>,
}

impl std::fmt::Debug for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pair")
            .field("serial", &self.serial)
            .field("subject", &self.subject)
            .field("pkcs12", &self.pkcs12.is_some())
            .finish_non_exhaustive()
    }
}

/// The port that signs a bootstrap certificate: the bootstrap CA with its key.
pub trait BootstrapSigner: Send + Sync {
    /// The common name a bootstrap certificate carries, derived from the bootstrap CA's
    /// (ADR-0028 clause 28).
    fn leaf_common_name(&self) -> String;

    /// When the bootstrap CA ends, in milliseconds since the epoch.
    fn ca_not_after_ms(&self) -> u64;

    /// The SHA-256 of the bootstrap CA's public key, hex: what tells this CA from the one it
    /// replaced, whose name it shares.
    fn ca_key_fingerprint(&self) -> String;

    /// Makes a fresh key and signs a client-authentication leaf for it, valid from `now_ms`
    /// less the allowance for clocks that disagree until `not_after_ms`. With a `password` the key
    /// is sealed with the certificate into a PKCS#12 file and not handed back in PEM (ADR-0029).
    ///
    /// # Errors
    /// Returns an error when no key can be made, the certificate cannot be signed, or the file
    /// cannot be written.
    fn sign(
        &self,
        subject: &[(Attribute, String)],
        now_ms: u64,
        not_after_ms: u64,
        password: Option<&str>,
    ) -> Result<Pair, String>;
}

/// What the record keeps of a bootstrap certificate — never its key in the clear (ADR-0028
/// clause 24, ADR-0029 clause 4).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Entry {
    pub id: CertId,
    pub serial: String,
    pub subject: String,
    pub key_fingerprint: String,
    pub issued_ms: u64,
    pub not_after_ms: u64,
    /// The issuing CA's key fingerprint: an entry of a CA since replaced is forgotten.
    #[serde(default)]
    pub ca_key: String,
    /// Whether the store keeps its PKCS#12 file, for download (ADR-0029 clause 4).
    #[serde(default)]
    pub p12: bool,
}

/// The port the record persists through: one document per certificate.
pub trait BootstrapStore: Send + Sync {
    /// Every entry persisted.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    fn load(&self) -> Result<Vec<Entry>, String>;

    /// Persists one entry.
    ///
    /// # Errors
    /// Returns an error when it cannot be written.
    fn put(&self, entry: &Entry) -> Result<(), String>;

    /// Forgets one entry, and its PKCS#12 file with it.
    ///
    /// # Errors
    /// Returns an error when it cannot be removed.
    fn remove(&self, serial: &str) -> Result<(), String>;

    /// Keeps the PKCS#12 file of one entry, owner-only.
    ///
    /// # Errors
    /// Returns an error when it cannot be written.
    fn put_p12(&self, serial: &str, file: &[u8]) -> Result<(), String>;

    /// The PKCS#12 file of one entry, `None` when there is none.
    ///
    /// # Errors
    /// Returns an error when it cannot be read.
    fn p12(&self, serial: &str) -> Result<Option<Vec<u8>>, String>;

    /// Forgets the PKCS#12 file of one entry, keeping the entry.
    ///
    /// # Errors
    /// Returns an error when it cannot be removed.
    fn remove_p12(&self, serial: &str) -> Result<(), String>;
}

/// Why a bootstrap certificate was not issued.
#[derive(Debug)]
pub enum IssueError {
    /// No `[enrolment] bootstrap_ca_key_file` — `409`.
    NoKey,
    /// The request is not one this Server signs — `400`.
    Invalid(String),
    /// [`MAX_VALID`] valid certificates are on record — `409`.
    Full,
    /// The bootstrap CA has ended; what it signs now would be dead on arrival — `409`.
    CaEnded,
    /// The record or the signer failed — `500`.
    Failed(String),
}

/// Why a bootstrap certificate's entry was not deleted (ADR-0029 clause 15).
#[derive(Debug, PartialEq, Eq)]
pub enum DeleteError {
    /// No certificate on record under that serial — `404`.
    NotFound,
    /// The certificate is not revoked — `409`.
    NotRevoked,
    /// The audit record cannot be written — `503`.
    Unrecorded,
    /// The store failed — `500`.
    Failed(String),
}

/// The bootstrap certificates the Server issued and still holds valid.
pub struct BootstrapCertificates {
    signer: Option<Box<dyn BootstrapSigner>>,
    store: Box<dyn BootstrapStore>,
    clock: Arc<dyn Clock>,
    entries: Mutex<BTreeMap<String, Entry>>,
}

impl BootstrapCertificates {
    /// Opens the record and forgets what has ended, and — with a signer — what a bootstrap CA
    /// since replaced issued, which no longer passes the handshake. Without a signer the Server
    /// issues nothing and still lists what it issued before.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    pub fn open(
        signer: Option<Box<dyn BootstrapSigner>>,
        store: Box<dyn BootstrapStore>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, String> {
        let current = signer.as_ref().map(|signer| signer.ca_key_fingerprint());
        let mut entries = BTreeMap::new();
        for entry in store.load()? {
            if current
                .as_ref()
                .is_some_and(|ca_key| *ca_key != entry.ca_key)
            {
                store.remove(&entry.serial)?;
            } else {
                entries.insert(entry.serial.clone(), entry);
            }
        }
        let record = BootstrapCertificates {
            signer,
            store,
            clock,
            entries: Mutex::new(entries),
        };
        record.prune(&mut record.entries.lock().expect("bootstrap record lock"))?;
        Ok(record)
    }

    /// Whether the Server holds the bootstrap CA's key.
    #[must_use]
    pub fn can_issue(&self) -> bool {
        self.signer.is_some()
    }

    /// Issues one bootstrap certificate and records it before answering (ADR-0028 clauses 21 to 24).
    ///
    /// # Errors
    /// See [`IssueError`].
    pub fn issue(&self, request: &IssueRequest) -> Result<Pair, IssueError> {
        let Some(signer) = &self.signer else {
            return Err(IssueError::NoKey);
        };
        let days = request.validity_days.unwrap_or(DEFAULT_VALIDITY_DAYS);
        if !(1..=MAX_VALIDITY_DAYS).contains(&days) {
            return Err(IssueError::Invalid(format!(
                "validity_days must be 1 to {MAX_VALIDITY_DAYS}"
            )));
        }
        let attributes = request
            .subject
            .attributes(&signer.leaf_common_name())
            .map_err(IssueError::Invalid)?;
        let password = request.password.as_ref().map(|p| p.0.as_str());
        if let Some(password) = password {
            fleet_core::pkcs12::check_password(password).map_err(IssueError::Invalid)?;
        }
        self.room()?;
        let now = self.clock.now_ms();
        if signer.ca_not_after_ms() <= now {
            return Err(IssueError::CaEnded);
        }
        // Never past the CA (ADR-0028 clause 23).
        let not_after = (now + u64::from(days) * DAY_MS).min(signer.ca_not_after_ms());
        // Outside the record's lock: sealing a PKCS#12 file derives three keys of 600 000
        // iterations, and the list and the download must not wait for it (ADR-0029).
        let pair = signer
            .sign(&attributes, now, not_after, password)
            .map_err(IssueError::Failed)?;
        let mut entries = self.entries.lock().expect("bootstrap record lock");
        self.prune(&mut entries).map_err(IssueError::Failed)?;
        if entries.len() >= MAX_VALID {
            return Err(IssueError::Full);
        }
        let entry = Entry {
            id: pair.id.clone(),
            serial: pair.serial.clone(),
            subject: pair.subject.clone(),
            key_fingerprint: pair.key_fingerprint.clone(),
            issued_ms: now,
            not_after_ms: pair.not_after_ms,
            ca_key: signer.ca_key_fingerprint(),
            p12: pair.pkcs12.is_some(),
        };
        // The file before the entry that says it is there (ADR-0029 clause 4), and gone again
        // when the entry cannot be written: no file is kept that no entry ends.
        if let Some(file) = &pair.pkcs12 {
            self.store
                .put_p12(&entry.serial, file)
                .map_err(IssueError::Failed)?;
        }
        if let Err(e) = self.store.put(&entry) {
            let _ = self.store.remove_p12(&entry.serial);
            return Err(IssueError::Failed(e));
        }
        entries.insert(entry.serial.clone(), entry);
        Ok(pair)
    }

    /// Whether the record has room for one more valid certificate.
    fn room(&self) -> Result<(), IssueError> {
        let mut entries = self.entries.lock().expect("bootstrap record lock");
        self.prune(&mut entries).map_err(IssueError::Failed)?;
        if entries.len() >= MAX_VALID {
            return Err(IssueError::Full);
        }
        Ok(())
    }

    /// The valid bootstrap certificates on record, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<Entry> {
        let mut entries = self.entries.lock().expect("bootstrap record lock");
        // A failed removal leaves an ended entry for the next call; the list still shows only
        // what is valid.
        let _ = self.prune(&mut entries);
        let now = self.clock.now_ms();
        let mut list: Vec<Entry> = entries
            .values()
            .filter(|entry| entry.not_after_ms > now)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.issued_ms.cmp(&b.issued_ms).then(a.serial.cmp(&b.serial)));
        list
    }

    /// The entry of a valid certificate and its PKCS#12 file, `None` when the certificate is not
    /// on record, has ended, or has no file (ADR-0029 clause 4). Whether it is revoked is the
    /// caller's to tell, from the revocation list.
    ///
    /// # Errors
    /// Returns an error when the file cannot be read.
    pub fn p12(&self, serial: &str) -> Result<Option<(Entry, Vec<u8>)>, String> {
        let mut entries = self.entries.lock().expect("bootstrap record lock");
        self.prune(&mut entries)?;
        let Some(entry) = entries.get(serial).filter(|entry| entry.p12) else {
            return Ok(None);
        };
        Ok(self.store.p12(serial)?.map(|file| (entry.clone(), file)))
    }

    /// Deletes a revoked certificate's entry and any PKCS#12 file of it (ADR-0029 clause 15).
    /// `revoked` tells whether the entry is revoked, and `recorded` puts the deletion on the audit
    /// record; both run under the record's lock with the removal, so a second request, or a
    /// revocation lifted meanwhile, cannot slip between the check and the deletion.
    ///
    /// # Errors
    /// See [`DeleteError`]; on every error the entry is kept.
    pub fn delete(
        &self,
        serial: &str,
        revoked: impl FnOnce(&Entry) -> bool,
        recorded: impl FnOnce(&Entry) -> bool,
    ) -> Result<Entry, DeleteError> {
        let mut entries = self.entries.lock().expect("bootstrap record lock");
        self.prune(&mut entries).map_err(DeleteError::Failed)?;
        let entry = entries.get(serial).cloned().ok_or(DeleteError::NotFound)?;
        if !revoked(&entry) {
            return Err(DeleteError::NotRevoked);
        }
        if !recorded(&entry) {
            return Err(DeleteError::Unrecorded);
        }
        self.store.remove(serial).map_err(DeleteError::Failed)?;
        entries.remove(serial);
        Ok(entry)
    }

    /// Forgets the PKCS#12 file of a certificate, which a revocation does before it is answered
    /// (ADR-0029 clause 4). A certificate without one is left as it is.
    ///
    /// # Errors
    /// Returns an error when the file or the entry cannot be written.
    pub fn forget_p12(&self, serial: &str) -> Result<(), String> {
        let mut entries = self.entries.lock().expect("bootstrap record lock");
        let Some(entry) = entries.get_mut(serial).filter(|entry| entry.p12) else {
            return Ok(());
        };
        // The entry first, so it never names a file that is gone — and a file left by a failed
        // removal is never handed out, since the entry no longer says it is there.
        let mut forgotten = entry.clone();
        forgotten.p12 = false;
        self.store.put(&forgotten)?;
        *entry = forgotten;
        self.store.remove_p12(serial)
    }

    /// Forgets every certificate that has ended (ADR-0028 clause 24).
    fn prune(&self, entries: &mut BTreeMap<String, Entry>) -> Result<(), String> {
        let now = self.clock.now_ms();
        let ended: Vec<String> = entries
            .values()
            .filter(|entry| entry.not_after_ms <= now)
            .map(|entry| entry.serial.clone())
            .collect();
        for serial in ended {
            self.store.remove(&serial)?;
            entries.remove(&serial);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct FixedClock(AtomicU64);
    impl Clock for FixedClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[derive(Default)]
    struct MemoryStore(
        Mutex<BTreeMap<String, Entry>>,
        Mutex<BTreeMap<String, Vec<u8>>>,
        std::sync::atomic::AtomicBool,
    );
    impl BootstrapStore for Arc<MemoryStore> {
        fn load(&self) -> Result<Vec<Entry>, String> {
            Ok(self.0.lock().unwrap().values().cloned().collect())
        }
        fn put(&self, entry: &Entry) -> Result<(), String> {
            if self.2.load(Ordering::SeqCst) {
                return Err("the disk is full".to_string());
            }
            self.0
                .lock()
                .unwrap()
                .insert(entry.serial.clone(), entry.clone());
            Ok(())
        }
        fn remove(&self, serial: &str) -> Result<(), String> {
            self.0.lock().unwrap().remove(serial);
            self.1.lock().unwrap().remove(serial);
            Ok(())
        }
        fn put_p12(&self, serial: &str, file: &[u8]) -> Result<(), String> {
            self.1
                .lock()
                .unwrap()
                .insert(serial.to_string(), file.to_vec());
            Ok(())
        }
        fn p12(&self, serial: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.1.lock().unwrap().get(serial).cloned())
        }
        fn remove_p12(&self, serial: &str) -> Result<(), String> {
            self.1.lock().unwrap().remove(serial);
            Ok(())
        }
    }

    /// Signs nothing real: hands back what it was asked for, so the rules around it can be read.
    struct EchoSigner {
        ca_end_ms: u64,
        serial: AtomicU64,
    }
    impl BootstrapSigner for EchoSigner {
        fn leaf_common_name(&self) -> String {
            fleet_core::naming::leaf_common_name("Acme Bootstrap CA")
        }
        fn ca_not_after_ms(&self) -> u64 {
            self.ca_end_ms
        }
        fn ca_key_fingerprint(&self) -> String {
            format!("ca-{}", self.ca_end_ms)
        }
        fn sign(
            &self,
            subject: &[(Attribute, String)],
            now_ms: u64,
            not_after_ms: u64,
            password: Option<&str>,
        ) -> Result<Pair, String> {
            let serial = format!("{:x}", self.serial.fetch_add(1, Ordering::SeqCst) + 1);
            Ok(Pair {
                id: CertId::new(b"issuer", &serial),
                serial,
                subject: subject
                    .iter()
                    .map(|(_, v)| v.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                key_fingerprint: String::new(),
                not_before_ms: now_ms,
                not_after_ms,
                certificate_pem: String::new(),
                private_key_pem: password.is_none().then(String::new),
                pkcs12: password.map(|p| format!("sealed under {p}").into_bytes()),
            })
        }
    }

    fn fixture(ca_end_ms: u64) -> (BootstrapCertificates, Arc<FixedClock>, Arc<MemoryStore>) {
        let clock = Arc::new(FixedClock(AtomicU64::new(1_000)));
        let store = Arc::new(MemoryStore::default());
        let record = BootstrapCertificates::open(
            Some(Box::new(EchoSigner {
                ca_end_ms,
                serial: AtomicU64::new(0),
            })),
            Box::new(store.clone()),
            clock.clone(),
        )
        .expect("open");
        (record, clock, store)
    }

    fn request(json: serde_json::Value) -> IssueRequest {
        serde_json::from_value(json).expect("request")
    }

    /// Verifies: ADR-0028
    #[test]
    fn a_subject_takes_only_the_organization_and_its_unit_within_bounds() {
        for unknown in ["common_name", "country", "email"] {
            let refused = serde_json::from_value::<IssueRequest>(
                serde_json::json!({"subject": {unknown: "x"}}),
            );
            assert!(refused.is_err(), "{unknown} is refused");
        }
        for bad in [
            serde_json::json!({"organization": ""}),
            serde_json::json!({"organization": "x".repeat(65)}),
            serde_json::json!({"organizational_unit": "a\nb"}),
        ] {
            let subject: Subject = serde_json::from_value(bad.clone()).expect("subject");
            assert!(subject.attributes("cn").is_err(), "{bad} is refused");
        }
        let subject: Subject = serde_json::from_value(serde_json::json!({
            "organization": "Kunde GmbH", "organizational_unit": "Wien"
        }))
        .expect("subject");
        assert_eq!(
            subject.attributes("acme bootstrap").expect("valid"),
            vec![
                (Attribute::Organization, "Kunde GmbH".to_string()),
                (Attribute::OrganizationalUnit, "Wien".to_string()),
                (Attribute::CommonName, "acme bootstrap".to_string()),
            ]
        );
        assert_eq!(
            Subject::default()
                .attributes("acme bootstrap")
                .expect("empty is fine"),
            vec![(Attribute::CommonName, "acme bootstrap".to_string())]
        );
    }

    /// Verifies: ADR-0028
    #[test]
    fn a_certificate_lives_ten_days_by_default_at_most_ninety_and_never_past_its_ca() {
        let (record, _, _) = fixture(u64::MAX);
        let pair = record.issue(&IssueRequest::default()).expect("issued");
        assert_eq!(pair.not_after_ms, 1_000 + 10 * DAY_MS);
        assert_eq!(
            pair.subject, "Acme Bootstrap Certificate",
            "the CA's name with ` CA` turned into ` Certificate`"
        );
        for days in [0, 91] {
            let refused = record.issue(&request(serde_json::json!({"validity_days": days})));
            assert!(matches!(refused, Err(IssueError::Invalid(_))), "{days}");
        }
        let (short, _, _) = fixture(1_000 + 3 * DAY_MS);
        let pair = short
            .issue(&request(serde_json::json!({"validity_days": 90})))
            .expect("issued");
        assert_eq!(pair.not_after_ms, 1_000 + 3 * DAY_MS, "cut to the CA's end");
    }

    /// A CA that has ended signs nothing: its certificate would be dead on arrival.
    /// Verifies: ADR-0028
    #[test]
    fn an_ended_ca_issues_nothing() {
        let (record, _, _) = fixture(1_000);
        assert!(matches!(
            record.issue(&IssueRequest::default()),
            Err(IssueError::CaEnded)
        ));
    }

    /// After the bootstrap CA is replaced, what the old one issued no longer passes the
    /// handshake, so it is no longer listed.
    /// Verifies: ADR-0028
    #[test]
    fn what_a_replaced_ca_issued_is_forgotten() {
        let (record, clock, store) = fixture(u64::MAX);
        record.issue(&IssueRequest::default()).expect("issued");
        let reopened = BootstrapCertificates::open(
            Some(Box::new(EchoSigner {
                ca_end_ms: u64::MAX - 1,
                serial: AtomicU64::new(100),
            })),
            Box::new(store.clone()),
            clock,
        )
        .expect("open");
        assert!(reopened.list().is_empty());
        assert!(
            store.0.lock().unwrap().is_empty(),
            "removed from the store too"
        );
    }

    /// Verifies: ADR-0028
    #[test]
    fn the_record_lists_what_is_valid_and_forgets_what_ended() {
        let (record, clock, store) = fixture(u64::MAX);
        let first = record
            .issue(&request(serde_json::json!({"validity_days": 1})))
            .expect("issued");
        record
            .issue(&request(serde_json::json!({"validity_days": 5})))
            .expect("issued");
        assert_eq!(record.list().len(), 2);
        assert_eq!(store.0.lock().unwrap().len(), 2, "written before answered");
        clock.0.store(1_000 + 2 * DAY_MS, Ordering::SeqCst);
        let listed = record.list();
        assert_eq!(listed.len(), 1);
        assert_ne!(listed[0].serial, first.serial);
        assert_eq!(store.0.lock().unwrap().len(), 1, "the ended one is removed");
    }

    /// Verifies: ADR-0028
    #[test]
    fn without_the_key_nothing_is_issued() {
        let store = Arc::new(MemoryStore::default());
        let record = BootstrapCertificates::open(
            None,
            Box::new(store),
            Arc::new(FixedClock(AtomicU64::new(0))),
        )
        .expect("open");
        assert!(!record.can_issue());
        assert!(matches!(
            record.issue(&IssueRequest::default()),
            Err(IssueError::NoKey)
        ));
    }

    /// A password of four characters is taken and one of three is not.
    /// Verifies: ADR-0030
    #[test]
    fn a_password_of_four_characters_issues_a_file_and_one_of_three_nothing() {
        let (record, _clock, _store) = fixture(u64::MAX);
        let short = record.issue(&request(serde_json::json!({"password": "abc"})));
        assert!(matches!(short, Err(IssueError::Invalid(_))));
        assert!(record.list().is_empty(), "nothing issued");
        let sealed = record
            .issue(&request(serde_json::json!({"password": "four"})))
            .expect("issued");
        let (_, file) = record.p12(&sealed.serial).expect("read").expect("kept");
        assert_eq!(file, b"sealed under four");
    }

    /// With a password the Server keeps the file, not the key, and hands it out until the
    /// certificate ends or a revocation forgets it; a password out of bounds issues nothing.
    /// Verifies: ADR-0029
    #[test]
    fn a_password_keeps_a_pkcs12_file_until_the_certificate_ends_or_is_forgotten() {
        let (record, clock, store) = fixture(u64::MAX);
        let short = record.issue(&request(serde_json::json!({"password": "abc"})));
        assert!(matches!(short, Err(IssueError::Invalid(_))));
        assert!(record.list().is_empty(), "nothing issued");

        let plain = record.issue(&IssueRequest::default()).expect("issued");
        assert!(plain.private_key_pem.is_some() && plain.pkcs12.is_none());
        assert_eq!(
            record.p12(&plain.serial).expect("read"),
            None,
            "no file without a password"
        );

        let sealed = record
            .issue(&request(
                serde_json::json!({"validity_days": 1, "password": "twelve chars"}),
            ))
            .expect("issued");
        assert!(
            sealed.private_key_pem.is_none(),
            "no PEM key with a password"
        );
        let (entry, file) = record.p12(&sealed.serial).expect("read").expect("kept");
        assert!(entry.p12);
        assert_eq!(file, b"sealed under twelve chars");
        assert!(
            store.0.lock().unwrap()[&sealed.serial].p12,
            "the entry says so"
        );

        let other = record
            .issue(&request(serde_json::json!({"password": "twelve chars"})))
            .expect("issued");
        record.forget_p12(&other.serial).expect("forgotten");
        assert_eq!(record.p12(&other.serial).expect("read"), None);
        assert!(
            store.1.lock().unwrap().get(&other.serial).is_none(),
            "the file is gone"
        );
        assert!(!store.0.lock().unwrap()[&other.serial].p12);

        clock.0.store(1_000 + 2 * DAY_MS, Ordering::SeqCst);
        assert_eq!(record.p12(&sealed.serial).expect("read"), None, "ended");
        assert!(
            store.1.lock().unwrap().get(&sealed.serial).is_none(),
            "removed with its entry"
        );
    }

    /// Verifies: ADR-0029
    #[test]
    fn no_debug_shows_a_password_or_a_key() {
        let request = request(serde_json::json!({"password": "do not show me"}));
        assert!(!format!("{request:?}").contains("do not show me"));
        let (record, _, _) = fixture(u64::MAX);
        let pair = record.issue(&IssueRequest::default()).expect("issued");
        assert!(!format!("{pair:?}").contains("private_key_pem"));
    }

    /// An entry that cannot be written leaves no PKCS#12 file behind, which nothing would end.
    /// Verifies: ADR-0029
    #[test]
    fn a_failed_entry_keeps_no_pkcs12_file() {
        let (record, _, store) = fixture(u64::MAX);
        store.2.store(true, Ordering::SeqCst);
        let failed = record.issue(&request(serde_json::json!({"password": "twelve chars"})));
        assert!(matches!(failed, Err(IssueError::Failed(_))));
        assert!(
            store.1.lock().unwrap().is_empty(),
            "no file without its entry"
        );
    }

    /// Only a revoked entry is deleted, and only with its deletion on the record; it goes from the
    /// list and the store with its PKCS#12 file, and a second deletion finds nothing.
    /// Verifies: ADR-0029
    #[test]
    fn a_revoked_entry_is_deleted_with_its_pkcs12_file_once() {
        let (record, _, store) = fixture(u64::MAX);
        let sealed = record
            .issue(&request(serde_json::json!({"password": "twelve chars"})))
            .expect("issued");
        let serial = sealed.serial.as_str();
        assert_eq!(
            record.delete(serial, |_| false, |_| true),
            Err(DeleteError::NotRevoked)
        );
        assert_eq!(
            record.delete(serial, |_| true, |_| false),
            Err(DeleteError::Unrecorded)
        );
        assert_eq!(record.list().len(), 1, "kept on either refusal");
        let deleted = record.delete(serial, |_| true, |_| true).expect("deleted");
        assert_eq!(deleted.serial, sealed.serial);
        assert!(record.list().is_empty());
        assert!(store.0.lock().unwrap().is_empty() && store.1.lock().unwrap().is_empty());
        assert_eq!(
            record.delete(serial, |_| true, |_| true),
            Err(DeleteError::NotFound)
        );
    }
}
