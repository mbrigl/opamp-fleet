//! Enrolment: the client certificate this Client asks the Server to issue (ADR-0026).
//!
//! The Baseline's CSR flow keeps the private key on the host. This module generates that key and
//! the request over its public half, and takes in the certificate that comes back through the
//! ordinary connection-settings offer. What leaves the host is a signing request; what is written
//! beside it is a key nothing ever sends.
//!
//! Enrolment is driven by capability, not by a switch in `supervisor.toml`: a Client asks when the
//! Server says it signs (`AcceptsConnectionSettingsRequest`) and it has no certificate, or the one
//! it holds is two thirds through its life. A Server that declares nothing is never asked, which is
//! the protocol's own negotiation rule — and a certificate obtained before mutual TLS is switched
//! on is exactly what makes switching it on uneventful.

use std::path::Path;

use rcgen::{CertificateParams, KeyPair};
use tracing::{info, warn};

use crate::config::ClientConfig;
use crate::tls::{ISSUED_CERT_FILE, ISSUED_KEY_FILE, PENDING_CSR_FILE, PENDING_KEY_FILE};

/// A certificate is renewed once it is this far through its validity — early enough that a Server
/// that cannot sign for a while costs nothing, and that the attempt can fail twice before the
/// certificate it replaces actually expires.
const RENEW_AFTER: f64 = 2.0 / 3.0;

/// The PEM certificate signing request to send, or `None` when this Client needs no certificate
/// right now.
///
/// A request in flight is re-sent unchanged until it is answered: an enrolment waits for an
/// operator, and the Server's queue knows a request by its public key (ADR-0026 clause 21).
/// Generating one **writes the private key** to the state directory before the request goes out,
/// as a pending key beside the one in force, so an answer arriving after a restart is still usable
/// and the certificate in force keeps its own key until the new one is proved.
pub fn request(config: &ClientConfig) -> Option<Vec<u8>> {
    if !needs_certificate(config) {
        return None;
    }
    if let Some(pending) = pending(config) {
        return Some(pending);
    }
    match generate(config) {
        Ok(csr) => {
            info!("requesting a client certificate from the Server");
            Some(csr)
        }
        Err(e) => {
            warn!(error = %e, "cannot build a certificate signing request");
            None
        }
    }
}

/// Stores an issued certificate beside the key it was requested for, putting it in force on the
/// next connection (ADR-0026).
///
/// # Errors
/// Returns an error when the certificate cannot be written, or when no pending key is there to go
/// with it — a certificate this Client did not ask for is refused rather than stored.
pub fn accept(state_dir: &Path, cert_pem: &[u8]) -> Result<(), String> {
    let pending = state_dir.join(PENDING_KEY_FILE);
    if !pending.exists() {
        return Err(format!(
            "an offered certificate has no key to go with it — {} is missing",
            pending.display()
        ));
    }
    std::fs::write(state_dir.join(ISSUED_CERT_FILE), cert_pem)
        .map_err(|e| format!("cannot store the issued certificate: {e}"))?;
    // The pending key becomes the key in force, and the request is answered.
    std::fs::rename(&pending, state_dir.join(ISSUED_KEY_FILE))
        .map_err(|e| format!("cannot put the issued key in force: {e}"))?;
    let _ = std::fs::remove_file(state_dir.join(PENDING_CSR_FILE));
    Ok(())
}

/// The request in flight, if one is: its key and its CSR are both on disk.
fn pending(config: &ClientConfig) -> Option<Vec<u8>> {
    if !config.state_dir.join(PENDING_KEY_FILE).exists() {
        return None;
    }
    std::fs::read(config.state_dir.join(PENDING_CSR_FILE)).ok()
}

/// Whether this Client should ask for a certificate: it has none, or the one it has is into its
/// renewal window.
fn needs_certificate(config: &ClientConfig) -> bool {
    let cert = config.state_dir.join(ISSUED_CERT_FILE);
    let key = config.state_dir.join(ISSUED_KEY_FILE);
    if !cert.exists() || !key.exists() {
        return true;
    }
    match renewal_due(&cert) {
        Ok(due) => due,
        Err(e) => {
            // A certificate that cannot be read is one that cannot be presented either; asking for
            // a new one is the recovery, and refusing to ask would strand the host.
            warn!(error = %e, "cannot read the stored client certificate; requesting a new one");
            true
        }
    }
}

/// Whether the stored certificate is at least [`RENEW_AFTER`] through its validity.
///
/// Read with `x509-parser` — the parser rcgen itself uses, whose own X.509 entry point is private.
fn renewal_due(cert_file: &Path) -> Result<bool, String> {
    let pem = std::fs::read(cert_file)
        .map_err(|e| format!("cannot read {}: {e}", cert_file.display()))?;
    let (_, block) = x509_parser::pem::parse_x509_pem(&pem)
        .map_err(|e| format!("cannot read {}: {e}", cert_file.display()))?;
    let certificate = block
        .parse_x509()
        .map_err(|e| format!("cannot read {}: {e}", cert_file.display()))?;
    let validity = certificate.validity();
    let (not_before, not_after) = (
        validity.not_before.to_datetime(),
        validity.not_after.to_datetime(),
    );
    let life = not_after - not_before;
    if life <= time::Duration::ZERO {
        return Ok(true);
    }
    let elapsed = time::OffsetDateTime::now_utc() - not_before;
    Ok(elapsed.as_seconds_f64() >= life.as_seconds_f64() * RENEW_AFTER)
}

/// Generates the keypair and the request over it. The key lands in the state directory; only the
/// request travels.
fn generate(config: &ClientConfig) -> Result<Vec<u8>, String> {
    let key = KeyPair::generate().map_err(|e| format!("cannot generate a key: {e}"))?;
    // The subject says which Client this is, for a human reading a certificate — never for the
    // Server to match on (ADR-0026): identity is `instance_uid`, and the Server may re-key it.
    let mut params = CertificateParams::new(vec![config.name.clone()])
        .map_err(|e| format!("cannot build a certificate request: {e}"))?;
    // The subject an operator reads in the enrolment queue (ADR-0026 clause 22); rcgen's default
    // names every host alike.
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, config.name.clone());
    // A renewal says which certificate it renews, signed with that certificate's key (ADR-0026
    // clause 27): the Server carries the host on from it even when a Gateway is in between.
    if let Some(proof) = renewal_proof(config, &key) {
        let uri = rcgen::string::Ia5String::try_from(proof)
            .map_err(|e| format!("cannot build a renewal proof: {e}"))?;
        params.subject_alt_names.push(rcgen::SanType::URI(uri));
    }
    let csr = params
        .serialize_request(&key)
        .map_err(|e| format!("cannot build a certificate request: {e}"))?
        .pem()
        .map_err(|e| format!("cannot encode the certificate request: {e}"))?;

    std::fs::create_dir_all(&config.state_dir)
        .map_err(|e| format!("cannot create {}: {e}", config.state_dir.display()))?;
    // The key is written no wider than its owner, the mode set in the open call so it is never on
    // disk at the umask default even briefly; a key file left at a wider mode by a prior enrolment
    // is narrowed too. On Windows the state directory's ACL under `%ProgramData%` protects it
    // (ADR-0028).
    crate::storage::write_private(
        &config.state_dir.join(PENDING_KEY_FILE),
        key.serialize_pem().as_bytes(),
    )
    .map_err(|e| format!("cannot store the private key: {e}"))?;
    std::fs::write(config.state_dir.join(PENDING_CSR_FILE), csr.as_bytes())
        .map_err(|e| format!("cannot store the certificate request: {e}"))?;
    // What the operator matches in the enrolment queue (ADR-0026 clause 22): the same SHA-256 the
    // Server lists as the request's key fingerprint.
    {
        use sha2::{Digest, Sha256};
        info!(
            key_fingerprint = %hex::encode(Sha256::digest(key.public_key_raw())),
            "certificate request generated; an enrolling host waits for an operator's approval"
        );
    }
    Ok(csr.into_bytes())
}

/// The renewal proof for `new_key`, while a certificate and its key are in force; `None` for a
/// first enrolment, or when they cannot be read — the Server then renews what the connection
/// presents.
fn renewal_proof(config: &ClientConfig, new_key: &KeyPair) -> Option<String> {
    use base64::Engine as _;
    use rcgen::{PublicKeyData as _, SigningKey as _};
    use sha2::{Digest as _, Sha256};
    let cert_pem = std::fs::read(config.state_dir.join(ISSUED_CERT_FILE)).ok()?;
    let key_pem = std::fs::read_to_string(config.state_dir.join(ISSUED_KEY_FILE)).ok()?;
    let (_, block) = x509_parser::pem::parse_x509_pem(&cert_pem).ok()?;
    let old_key = KeyPair::from_pem(&key_pem).ok()?;
    let new_key_sha256 = Sha256::digest(new_key.subject_public_key_info());
    let signature = old_key
        .sign(&fleet_core::renewal::statement(&new_key_sha256))
        .ok()?;
    Some(format!(
        "{}{}",
        fleet_core::renewal::URI_PREFIX,
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(fleet_core::renewal::encode(&block.contents, &signature))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(state_dir: &Path) -> ClientConfig {
        ClientConfig {
            state_dir: state_dir.to_path_buf(),
            ..ClientConfig::default()
        }
    }

    /// A fresh host asks, and the key is on disk before the request leaves — but the Client still
    /// presents nothing, because half an identity is not one.
    /// Verifies: ADR-0026
    #[test]
    fn a_client_without_a_certificate_asks_and_keeps_its_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = config(dir.path());
        let csr = request(&config).expect("a request");
        assert!(String::from_utf8_lossy(&csr).contains("BEGIN CERTIFICATE REQUEST"));
        assert!(dir.path().join(PENDING_KEY_FILE).exists());
        assert!(config.client_identity().is_none(), "a key alone is inert");
        assert_eq!(
            request(&config).expect("asked again"),
            csr,
            "a request in flight is re-sent unchanged until it is answered"
        );
    }

    /// The private key is never on disk wider than its owner — set in the open call, so there is no
    /// window between a world-readable create and a later chmod for a local attacker to read it.
    #[cfg(unix)]
    /// Verifies: ADR-0026
    #[test]
    fn the_private_key_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let config = config(dir.path());
        request(&config).expect("a request");

        let mode = dir
            .path()
            .join(PENDING_KEY_FILE)
            .metadata()
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the private key must be owner-only");
    }

    /// What comes back is stored beside the key and is what the Client presents from then on.
    /// Verifies: ADR-0026
    #[test]
    fn an_issued_certificate_becomes_the_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = config(dir.path());
        let csr = request(&config).expect("a request");

        // Stand in for the Server: sign what was actually sent.
        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(vec!["test-ca".to_string()]).expect("ca params");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca");
        let issuer = rcgen::Issuer::from_ca_cert_pem(&ca_cert.pem(), ca_key).expect("issuer");
        let signed =
            rcgen::CertificateSigningRequestParams::from_pem(&String::from_utf8(csr).unwrap())
                .expect("csr")
                .signed_by(&issuer)
                .expect("signed");

        accept(dir.path(), signed.pem().as_bytes()).expect("accept");
        let (cert, key) = config.client_identity().expect("an identity");
        assert_eq!(cert, dir.path().join(ISSUED_CERT_FILE));
        assert_eq!(key, dir.path().join(ISSUED_KEY_FILE));
        // And with one in force and fresh, nothing more is asked for.
        assert!(request(&config).is_none());
    }

    /// A certificate the Client never asked for has no key to go with it, and is refused rather
    /// than written where the connection code would then try to present it.
    /// Verifies: ADR-0026
    #[test]
    fn a_certificate_without_a_pending_key_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = accept(dir.path(), b"-----BEGIN CERTIFICATE-----").expect_err("refused");
        assert!(error.contains("no key to go with it"), "{error}");
    }

    /// The renewal window: a certificate two thirds through its life is asked for again, and the
    /// old one stays in force until the new one is verified.
    /// Verifies: ADR-0026
    #[test]
    fn a_certificate_in_its_renewal_window_is_requested_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = config(dir.path());
        let _ = request(&config).expect("a request");

        let key = KeyPair::generate().expect("key");
        let mut params = CertificateParams::new(vec!["edge-01".to_string()]).expect("params");
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(80);
        params.not_after = now + time::Duration::days(10);
        let cert = params.self_signed(&key).expect("cert");
        std::fs::write(dir.path().join(ISSUED_CERT_FILE), cert.pem()).expect("write");

        assert!(
            request(&config).is_some(),
            "eight ninths through its life is past the renewal point"
        );
    }
}
