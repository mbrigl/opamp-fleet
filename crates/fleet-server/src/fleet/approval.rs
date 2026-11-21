//! Enrolment and certificates (SRV-state-stores-contract-pair §9, SEC-admission-by-client-certificate, SEC-bootstrap-certificates-and-enrolment): the approval of an
//! enrolment and its take-back, the answer an enrolling Agent is given, the record of what an
//! approval signed, written with its decision, and the mark of a host as a Gateway (SEC-bootstrap-certificates-and-enrolment §11).
//! Its entry points are methods of `AppState`.
//!
//! The issuing and renewal of a host certificate on the connection the request arrived over live
//! in `session`, whose round issues it and which ranks below `approval`
//! (SRV-fleet-contexts-call-by-rank).

use opamp::proto::{OpAmpConnectionSettings, ServerCapabilities, ServerToAgent, TlsCertificate};
use opamp::uid::InstanceUid;

use crate::audit::Entry;
use crate::enrolment::{DecisionError, Recorder, Registrar};
use crate::revocation::{CertId, Facts, Signed};

use super::session::compose_settings_offer;
use super::{AppState, TelemetryOffer};

impl AppState {
    /// Approves one pending enrolment request: the client CA signs it (SEC-bootstrap-certificates-and-enrolment §10), with
    /// the subject values of the bootstrap certificate the request came with (CLT-modes-and-gateway §23).
    ///
    /// # Errors
    /// Returns [`DecisionError::NotFound`] when enrolment is off or the id is unknown,
    /// [`DecisionError::Subject`] when the bootstrap certificate's subject cannot be carried, and
    /// [`DecisionError::Sign`] when no CA is configured or it refuses the request.
    pub fn approve_enrolment(&self, id: &str) -> Result<String, DecisionError> {
        let enrolment = self.enrolment.as_ref().ok_or(DecisionError::NotFound)?;
        let signer = self.client_ca.as_deref().ok_or_else(|| {
            DecisionError::Sign("this Server issues no client certificates".into())
        })?;
        // A new host: its identity is minted here and carried by every renewal (SEC-bootstrap-certificates-and-enrolment §11).
        let host = InstanceUid::default().to_string();
        let revoked = |bootstrap: &CertId| {
            self.revocations
                .as_ref()
                .is_some_and(|revocations| revocations.is_certificate_revoked(bootstrap))
        };
        // The Gateway the request came through, named in the approval's audit entry.
        let gateway = enrolment
            .pending()
            .into_iter()
            .find(|pending| pending.id == id)
            .and_then(|pending| pending.requester.gateway);
        let serial = enrolment.approve(id, signer, &host, &Approving(self), &revoked)?;
        // Recorded before the approval is answered (SRV-state-stores-contract-pair §9). The decision is written
        // already, so a record that cannot take it refuses nothing: the register answers for it.
        if let Err(e) = self.audited(
            Entry::new("enrolment.approved", "approved")
                .with("id", id.to_string())
                .with("serial", serial.clone())
                .with("gateway", gateway),
        ) {
            tracing::warn!(error = %e, request = %id, serial = %serial, "cannot record an approval");
        }
        Ok(serial)
    }

    /// Takes back every approved request whose certificate the register does not hold, so it
    /// waits again, and records each signature as undecided: what a start does with a store that
    /// writes the decision before the register entry, after a write between the two failed
    /// (SRV-state-stores-contract-pair §9). `read` reads what a certificate, PEM, says about itself. Answers how
    /// many it took back.
    ///
    /// # Errors
    /// Returns an error when a decision cannot be taken back.
    pub fn take_back_unregistered_approvals(
        &self,
        read: &dyn Fn(&str) -> Result<Facts, String>,
    ) -> Result<usize, String> {
        let (Some(enrolment), Some(revocations)) = (&self.enrolment, &self.revocations) else {
            return Ok(0);
        };
        if enrolment.registers_with_its_decision() {
            return Ok(0);
        }
        let mut taken = 0;
        for (id, pem) in enrolment.approved() {
            let facts = match read(&pem) {
                Ok(facts) => facts,
                Err(e) => {
                    tracing::warn!(error = %e, request = %id, "an approved request's certificate does not read");
                    continue;
                }
            };
            if revocations.issued_entry(&facts.id).is_some() {
                continue;
            }
            enrolment.take_back(&id)?;
            Approving(self).undecided(
                &Signed { pem, facts },
                &id,
                crate::enrolment::Undecided::RolledBack,
            );
            taken += 1;
        }
        Ok(taken)
    }

    /// What an enrolment connection is told (SEC-bootstrap-certificates-and-enrolment §9): the capabilities that say it may
    /// send a CSR, and — once its request is approved — the issued certificate, as an ordinary
    /// connection-settings offer with no private key in it.
    pub fn enrolment_answer(
        &self,
        instance_uid: &[u8],
        certificate: Option<String>,
    ) -> ServerToAgent {
        let mut capabilities = ServerCapabilities::AcceptsStatus as u64;
        if self.client_ca.is_some() {
            capabilities |= ServerCapabilities::AcceptsConnectionSettingsRequest as u64
                | ServerCapabilities::OffersConnectionSettings as u64;
        }
        ServerToAgent {
            instance_uid: instance_uid.to_vec(),
            capabilities,
            connection_settings: certificate.map(|cert| {
                compose_settings_offer(
                    Some(OpAmpConnectionSettings {
                        certificate: Some(TlsCertificate {
                            cert: cert.into_bytes(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    TelemetryOffer::default(),
                )
            }),
            ..Default::default()
        }
    }

    /// Marks `host` as a Gateway, or not (SEC-bootstrap-certificates-and-enrolment §11); answers whether this Server knows
    /// the host. The mark decides whether the Agents arriving over it carry its values and how
    /// they group (SRV-fleet-record-agents-hosts §18 and §19), so every Agent is placed again.
    ///
    /// # Errors
    /// Returns an error when there is no host register or the change cannot be written.
    pub fn set_gateway(&self, host: &str, gateway: bool) -> Result<bool, String> {
        let revocations = self
            .revocations
            .as_ref()
            .ok_or_else(|| "revocation is not configured".to_string())?;
        let known = revocations.set_gateway(host, gateway)?;
        if known {
            self.place_all();
        }
        Ok(known)
    }
}

/// What an approval keeps of what it signed (SRV-state-stores-contract-pair §9): the signature in the audit
/// record before its decision is written, and its register entry and Host, prepared here, written
/// with the decision and taken up once it is.
struct Approving<'a>(&'a AppState);

/// An approval's refusal by the register.
fn register_refusal(e: String) -> DecisionError {
    if e == crate::revocation::BOOTSTRAP_REVOKED {
        DecisionError::Revoked
    } else {
        DecisionError::Sign(format!("cannot record the issued certificate: {e}"))
    }
}

impl Registrar for Approving<'_> {
    fn register(&self, approved: &crate::revocation::Approved) -> Result<(), DecisionError> {
        match &self.0.revocations {
            Some(revocations) => revocations
                .register_approved(approved)
                .map_err(register_refusal),
            None => Ok(()),
        }
    }

    fn committed(&self, approved: &crate::revocation::Approved, written: Option<u64>) {
        if let Some(revocations) = &self.0.revocations {
            revocations.took_up_approved(approved, written);
        }
    }
}

impl Recorder for Approving<'_> {
    fn signed(
        &self,
        signed: &Signed,
        request: &str,
        instance_uid: &[u8],
        bootstrap: &CertId,
    ) -> Result<Option<crate::revocation::Approved>, DecisionError> {
        let approved = match &self.0.revocations {
            Some(revocations) => Some(
                revocations
                    .prepare_approved(signed.facts.clone(), instance_uid, bootstrap)
                    .map_err(register_refusal)?,
            ),
            None => None,
        };
        self.0
            .audited(
                self.0
                    .signature_entry(signed, instance_uid, None, Some(request)),
            )
            .map_err(DecisionError::Sign)?;
        Ok(approved)
    }

    fn undecided(&self, signed: &Signed, request: &str, outcome: crate::enrolment::Undecided) {
        let entry = Entry::new("issuance.undecided", outcome.outcome())
            .with("issuer", signed.facts.issuer_name.clone())
            .with("serial", signed.facts.id.serial.clone())
            .with("key_fingerprint", signed.facts.key_fingerprint.clone())
            .with("request", request.to_string());
        if let Err(e) = self.0.audited(entry) {
            tracing::warn!(error = %e, request = %request, serial = %signed.facts.id.serial, "cannot record a signature whose decision was not written");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrolment::Enrolment;
    use crate::fleet::fixtures::*;
    use crate::fleet::Clock;
    use std::sync::Arc;
    use std::time::Duration;

    // ---- An approval records its certificate with its decision (SRV-state-stores-contract-pair §9) ----

    /// A fleet on files in `dir` that enrols: its register, its window and queue, its audit record
    /// in `dir/audit`, and a client CA kept in `dir` across a reopen.
    fn enrolling_fleet(
        dir: &std::path::Path,
    ) -> (AppState, Arc<Enrolment>, Arc<crate::audit_log::AuditLog>) {
        let (cert_file, key_file) = (dir.join("client-ca.pem"), dir.join("client-ca-key.pem"));
        if !cert_file.exists() {
            let key = rcgen::KeyPair::generate().expect("ca key");
            let mut params = rcgen::CertificateParams::default();
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, "Test Fleet Client CA");
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
            let cert = params.self_signed(&key).expect("ca");
            std::fs::write(&cert_file, cert.pem()).expect("ca file");
            std::fs::write(&key_file, key.serialize_pem()).expect("ca key file");
        }
        let ca = crate::ca::ClientCa::from_config(&crate::config::ClientCaConfig {
            cert_file,
            key_file: Some(key_file),
            key_label: None,
            validity_days: 30,
        })
        .expect("client CA");
        let clock: Arc<dyn Clock> = Arc::new(crate::clock::SystemClock);
        let enrolment = Arc::new(
            Enrolment::open_on(
                clock.clone(),
                Box::new(crate::fs::FsEnrolmentStore::open(dir)),
            )
            .expect("enrolment"),
        );
        let audit = Arc::new(
            crate::audit_log::AuditLog::start(
                Box::new(crate::fs::FsAuditStore::open(dir).expect("audit")),
                crate::audit_log::Limits::default(),
                clock,
            )
            .expect("audit"),
        );
        enrolment.set_audit(audit.clone());
        let state = hosted_fleet(dir)
            .with_client_ca(Some(ca))
            .with_enrolment(Some(enrolment.clone()))
            .with_audit(Some(audit.clone()));
        (state, enrolment, audit)
    }

    /// Asks for a certificate for a key of its own under `key`, while the window is open.
    fn enrol(enrolment: &Enrolment, key: &str) {
        let pair = rcgen::KeyPair::generate().expect("key");
        let csr = rcgen::CertificateParams::new(vec!["edge".to_string()])
            .expect("params")
            .serialize_request(&pair)
            .expect("csr")
            .pem()
            .expect("pem");
        assert_eq!(
            enrolment.submit(
                crate::enrolment::Request {
                    csr_pem: csr,
                    subject: String::new(),
                    key_fingerprint: key.to_string(),
                    instance_uid: vec![9; 16],
                },
                crate::enrolment::tests::requester(),
            ),
            crate::enrolment::Submitted::Waiting
        );
    }

    /// What a certificate, PEM, says about itself, as the Server reads it.
    fn read_certificate(pem: &str) -> Result<Facts, String> {
        let der = opamp::tls::certificates(pem.as_bytes())?;
        crate::ca::facts(der.first().ok_or("no certificate")?.as_ref())
    }

    /// The decision `enrolment.json` in `dir` holds on the request `id`.
    fn stored_decision(dir: &std::path::Path, id: &str) -> Option<crate::enrolment::Decision> {
        use crate::enrolment::EnrolmentStore;
        crate::fs::FsEnrolmentStore::open(dir)
            .read_back(id)
            .expect("read")
            .expect("held")
            .decision
    }

    /// The register's directory, which refuses a new entry while it may not be written.
    fn writable(dir: &std::path::Path, writable: bool) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("certificates"),
            std::fs::Permissions::from_mode(if writable { 0o700 } else { 0o500 }),
        )
        .expect("chmod");
    }

    /// On files the decision is written before the register entry: a decision that cannot be
    /// written registers nothing, and an entry that cannot be written takes the decision back, so
    /// the request waits again with nothing registered, each signature is recorded as rolled back,
    /// and approved again it is signed anew (SRV-state-stores-contract-pair §9).
    /// Verifies: SRV-state-stores-contract-pair
    #[cfg(unix)]
    #[test]
    fn an_approval_on_files_whose_register_entry_fails_takes_its_decision_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, enrolment, audit) = enrolling_fleet(dir.path());
        enrolment.open(600).expect("window");
        enrol(&enrolment, "k1");
        let queue = dir.path().join("enrolment.json");
        crate::fs::testing::refuse(&queue);
        let unwritten = state.approve_enrolment("k1");
        crate::fs::testing::mend(&queue);
        assert!(
            matches!(unwritten, Err(DecisionError::Store(_))),
            "{unwritten:?}"
        );
        assert!(state
            .revocations
            .as_ref()
            .expect("register")
            .issued()
            .is_empty());
        assert_eq!(
            audited_events(&audit, dir.path(), "issuance.undecided").len(),
            1,
            "the signature whose decision was not written"
        );
        writable(dir.path(), false);
        let refused = state.approve_enrolment("k1");
        writable(dir.path(), true);
        assert!(
            matches!(refused, Err(DecisionError::Sign(_))),
            "{refused:?}"
        );
        assert_eq!(
            stored_decision(dir.path(), "k1"),
            None,
            "the decision is taken back"
        );
        assert_eq!(enrolment.pending().len(), 1, "the request waits again");
        let revocations = state.revocations.as_ref().expect("register");
        assert!(revocations.issued().is_empty());
        assert!(revocations.hosts().is_empty(), "no Host minted");

        let serial = state.approve_enrolment("k1").expect("approved again");
        let issued = revocations.issued();
        assert_eq!(issued.len(), 1);
        assert_eq!(issued[0].facts.id.serial, serial);
        assert!(matches!(
            stored_decision(dir.path(), "k1"),
            Some(crate::enrolment::Decision::Approved(_))
        ));
        let undecided = audited_events(&audit, dir.path(), "issuance.undecided");
        assert_eq!(undecided.len(), 2, "{undecided:#?}");
        assert!(undecided
            .iter()
            .all(|entry| entry["outcome"] == "rolled_back"));
    }

    /// The audit lines in `dir/audit` whose event is `event`, once the writer has written them.
    fn audited_events(
        audit: &crate::audit_log::AuditLog,
        dir: &std::path::Path,
        event: &str,
    ) -> Vec<serde_json::Value> {
        assert!(
            audit.drain(Duration::from_secs(10)),
            "the audit record was not written"
        );
        let files = crate::fs::audit_files(&dir.join("audit")).expect("audit files");
        crate::fs::audit_lines(&files)
            .expect("audit lines")
            .into_iter()
            .map(|(_, line)| serde_json::from_str::<serde_json::Value>(&line).expect("json"))
            .filter(|line| line["event"] == event)
            .collect()
    }

    /// The audit record of an approval whose decision was not written still verifies, and names
    /// that signature as undecided by its serial and request, beside the signature of the approval
    /// that was (SRV-state-stores-contract-pair §9).
    /// Verifies: SRV-state-stores-contract-pair
    #[cfg(unix)]
    #[test]
    fn the_audit_record_of_an_approval_that_did_not_commit_verifies_and_names_it_undecided() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, enrolment, audit) = enrolling_fleet(dir.path());
        enrolment.open(600).expect("window");
        enrol(&enrolment, "k1");
        writable(dir.path(), false);
        state.approve_enrolment("k1").expect_err("not registered");
        writable(dir.path(), true);
        let serial = state.approve_enrolment("k1").expect("approved again");

        let signed = audited_events(&audit, dir.path(), "issuance.signed");
        assert_eq!(signed.len(), 2, "{signed:#?}");
        assert!(
            signed.iter().all(|entry| entry["request"] == "k1"),
            "{signed:#?}"
        );
        let undecided = audited_events(&audit, dir.path(), "issuance.undecided");
        assert_eq!(undecided.len(), 1, "{undecided:#?}");
        assert_eq!(undecided[0]["outcome"], "rolled_back");
        assert_eq!(undecided[0]["request"], "k1");
        assert_eq!(undecided[0]["serial"], signed[0]["serial"]);
        assert_ne!(undecided[0]["serial"], serial.as_str());
        assert_eq!(signed[1]["serial"], serial.as_str());
        let (_, verified) = crate::fs::verify_audit(&dir.path().join("audit")).expect("read");
        verified.expect("the chain holds");
    }

    /// A decision found at startup whose certificate the register does not hold, as after a crash
    /// between the two writes, is taken back: the request waits again and its signature is
    /// recorded as undecided, while an approval whose certificate is registered stays decided
    /// (SRV-state-stores-contract-pair §9).
    /// Verifies: SRV-state-stores-contract-pair
    #[test]
    fn a_decision_found_at_startup_without_its_register_entry_waits_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (lost, kept) = {
            let (state, enrolment, audit) = enrolling_fleet(dir.path());
            enrolment.open(600).expect("window");
            enrol(&enrolment, "lost");
            enrol(&enrolment, "kept");
            let lost = state.approve_enrolment("lost").expect("approve");
            let kept = state.approve_enrolment("kept").expect("approve");
            assert!(audit.close(Duration::from_secs(10)));
            (lost, kept)
        };
        // What a crash after the decision and before the register entry leaves.
        let entry = std::fs::read_dir(dir.path().join("certificates"))
            .expect("register")
            .map(|file| file.expect("file").path())
            .find(|path| path.to_string_lossy().ends_with(&format!("-{lost}.json")))
            .expect("the lost approval's entry");
        std::fs::remove_file(entry).expect("remove");

        let (state, enrolment, audit) = enrolling_fleet(dir.path());
        assert_eq!(
            state.take_back_unregistered_approvals(&read_certificate),
            Ok(1)
        );
        let waiting: Vec<String> = enrolment.pending().into_iter().map(|p| p.id).collect();
        assert_eq!(waiting, ["lost"]);
        assert_eq!(stored_decision(dir.path(), "lost"), None);
        assert!(
            stored_decision(dir.path(), "kept").is_some(),
            "a registered approval stays"
        );
        let undecided = audited_events(&audit, dir.path(), "issuance.undecided");
        assert_eq!(undecided.len(), 1, "{undecided:#?}");
        assert_eq!(undecided[0]["serial"], lost.as_str());
        assert_eq!(undecided[0]["request"], "lost");
        assert_ne!(lost, kept);
        assert_eq!(
            state.take_back_unregistered_approvals(&read_certificate),
            Ok(0),
            "once"
        );
    }
}
