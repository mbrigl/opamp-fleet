//! The Server as a local certificate authority (ADR-0017).
//!
//! The Baseline's CSR flow lets an Agent keep its private key and ask for a certificate over the
//! connection it already has: it sends a PEM certificate signing request, and the Server "creates a
//! client certificate … either by issuing a self-signed certificate (acting as a local CA) or
//! proxies the CSR to a CA". This is the first of those; proxying to an external CA is future work
//! and would sit behind the same `[client_ca]` seam.
//!
//! Nothing here decides *who* may enrol. A CSR from an Agent holding a certificate of the client
//! CA is a renewal and is signed at once; a CSR on an enrolment connection waits until an operator
//! approves it ([`crate::enrolment`], ADR-0039).

use rcgen::{
    CertificateSigningRequestParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType, SerialNumber,
};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::config::ClientCaConfig;
use crate::revocation::{CertId, Facts, Signed};

/// The SAN URI prefix naming the host a certificate was issued to (ADR-0039 clause 7).
pub const HOST_URI_PREFIX: &str = "urn:opamp-fleet:host:";

/// The issuing authority, loaded once at startup. Holding it parsed is what makes `AppState`'s
/// capability honest: `AcceptsConnectionSettingsRequest` is declared only while this exists.
pub struct ClientCa {
    issuer: Issuer<'static, KeyPair>,
    /// The CA's own certificate, DER: what a renewal proof's certificate must be signed by.
    ca_der: Vec<u8>,
    validity_days: u32,
    /// A life shorter than a day, in place of `validity_days` — what a test of renewal waits out.
    validity: Option<time::Duration>,
}

impl ClientCa {
    /// Loads the CA from `[client_ca]`. A key that does not match its certificate, or either file
    /// being unreadable, fails startup rather than the first enrolment (ADR-0011).
    pub fn from_config(config: &ClientCaConfig) -> Result<Self, String> {
        let cert_pem = std::fs::read_to_string(&config.cert_file)
            .map_err(|e| format!("cannot read {}: {e}", config.cert_file.display()))?;
        let key_pem = std::fs::read_to_string(&config.key_file)
            .map_err(|e| format!("cannot read {}: {e}", config.key_file.display()))?;
        let key = KeyPair::from_pem(&key_pem)
            .map_err(|e| format!("cannot read {}: {e}", config.key_file.display()))?;
        let issuer = Issuer::from_ca_cert_pem(&cert_pem, key)
            .map_err(|e| format!("cannot use {} as a CA: {e}", config.cert_file.display()))?;
        let ca_der = opamp::tls::certificates(cert_pem.as_bytes())
            .map_err(|e| format!("cannot read {}: {e}", config.cert_file.display()))?
            .first()
            .map(|der| der.as_ref().to_vec())
            .ok_or_else(|| format!("{} holds no certificate", config.cert_file.display()))?;
        Ok(ClientCa {
            issuer,
            ca_der,
            validity_days: config.validity_days,
            validity: None,
        })
    }

    /// Signs an Agent's CSR and returns the issued certificate as PEM.
    ///
    /// The subject comes from the request: it is descriptive, and this Server does not require it
    /// to match anything the Agent reports. Binding a certificate to an `instance_uid` would mean
    /// it dies the moment the Server re-keys that Agent through `AgentIdentification` — an outage
    /// of the Server's own making (ADR-0017).
    ///
    /// What the request may *not* dictate is the shape of the certificate. `rcgen` carries the
    /// CSR's `basicConstraints`, `keyUsage`, `extendedKeyUsage`, and SANs into the signed output,
    /// so a request asking for `CA:TRUE` would otherwise be handed a certificate that chains to the
    /// fleet CA and can mint more — a privilege this Server never means to grant. Enrolment issues
    /// one thing only: a client-authentication leaf. So the constraints are overwritten here rather
    /// than trusted from the request — forced to a non-CA cert with `clientAuth` and a plain
    /// signing key usage, and any requested SANs dropped, before it is signed.
    ///
    /// # Errors
    /// A request that cannot be parsed, or that this CA cannot sign, is an error the caller turns
    /// into the Baseline's `ServerErrorResponse` of type `BadRequest`.
    pub fn sign(&self, csr_pem: &str, host: &str) -> Result<Signed, String> {
        let mut request = CertificateSigningRequestParams::from_pem(csr_pem)
            .map_err(|e| format!("the certificate signing request does not parse: {e}"))?;
        // The life starts now, less a few minutes for clocks that disagree: a start in the past
        // would put a fresh certificate into its renewal window at once (ADR-0039 clause 11), and
        // would lengthen what a stolen one is good for.
        // A life shorter than the skew allowance keeps a tenth of itself as its allowance, or it
        // would start out in its renewal window.
        let skew = match self.validity {
            Some(validity) => {
                let tenth: time::Duration = validity / 10_i32;
                tenth.min(CLOCK_SKEW)
            }
            None => CLOCK_SKEW,
        };
        request.params.not_before = time::OffsetDateTime::now_utc() - skew;
        request.params.not_after = match self.validity {
            Some(validity) => time::OffsetDateTime::now_utc() + validity,
            None => not_after(self.validity_days)?,
        };
        // Never trust the request for the certificate's powers: force a client-auth leaf.
        request.params.is_ca = IsCa::ExplicitNoCa;
        request.params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        request.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        // None of the request's names — one could name another host. The one name this Server
        // puts in is the host the certificate is issued to (ADR-0039 clause 7), which a renewal
        // carries on.
        request.params.subject_alt_names.clear();
        request.params.subject_alt_names.push(SanType::URI(
            format!("{HOST_URI_PREFIX}{host}")
                .try_into()
                .map_err(|e| format!("cannot name the host {host:?}: {e}"))?,
        ));
        // A serial of its own for every certificate, so two certificates from one key are two
        // revocable things (ADR-0049 clause 1). rcgen would derive it from the key.
        request.params.serial_number = Some(random_serial()?);
        let certificate = request
            .signed_by(&self.issuer)
            .map_err(|e| format!("cannot sign the certificate signing request: {e}"))?;
        Ok(Signed {
            facts: facts(certificate.der())?,
            pem: certificate.pem(),
        })
    }

    /// Issues certificates that live `validity` instead of `validity_days` — seconds, for a test
    /// that watches renewal happen before expiry (ADR-0039 clause 9).
    #[must_use]
    pub fn with_validity(mut self, validity: time::Duration) -> Self {
        self.validity = Some(validity);
        self
    }

    /// The certificate a CSR proves it renews, when it carries a renewal proof (ADR-0039 clause
    /// 27): the proof's certificate must have been signed by this CA and be valid now, and its key
    /// must have signed the request's new key. `Ok(None)` for a request that carries no proof.
    ///
    /// # Errors
    /// Returns the `BadRequest` text for a proof that does not hold.
    pub fn renewal_proof(&self, csr_pem: &str) -> Result<Option<Facts>, String> {
        use base64::Engine as _;
        use sha2::{Digest, Sha256};
        let request = CertificateSigningRequestParams::from_pem(csr_pem)
            .map_err(|e| format!("the certificate signing request does not parse: {e}"))?;
        let Some(encoded) = request
            .params
            .subject_alt_names
            .iter()
            .find_map(|san| match san {
                SanType::URI(uri) => uri.as_str().strip_prefix(fleet_core::renewal::URI_PREFIX),
                _ => None,
            })
        else {
            return Ok(None);
        };
        let refused = |why: &str| format!("the renewal proof does not hold: {why}");
        let content = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| refused("it is not base64url"))?;
        let (old_der, signature) =
            fleet_core::renewal::decode(&content).ok_or_else(|| refused("it does not parse"))?;
        let (_, old) = X509Certificate::from_der(old_der)
            .map_err(|_| refused("its certificate does not parse"))?;
        let (_, ca) = X509Certificate::from_der(&self.ca_der)
            .map_err(|_| refused("the CA does not parse"))?;
        old.verify_signature(Some(ca.public_key()))
            .map_err(|_| refused("its certificate was not issued by this CA"))?;
        if !old.validity().is_valid() {
            return Err(refused("its certificate is not valid now"));
        }
        let csr_der = pem_der(csr_pem)?;
        let (_, csr) =
            x509_parser::certification_request::X509CertificationRequest::from_der(&csr_der)
                .map_err(|_| refused("the request does not parse"))?;
        let new_key = Sha256::digest(csr.certification_request_info.subject_pki.raw);
        let statement = fleet_core::renewal::statement(&new_key);
        verify_with(old.public_key(), &statement, signature)
            .map_err(|()| refused("its signature does not verify with the certificate's key"))?;
        facts(old_der).map(Some)
    }

    pub fn validity_days(&self) -> u32 {
        self.validity_days
    }
}

/// 16 bytes from the system's secure random source, the top bit cleared so the serial is a
/// positive integer (RFC 5280 §4.1.2.2).
fn random_serial() -> Result<SerialNumber, String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "no secure random source for a serial number".to_string())?;
    bytes[0] &= 0x7f;
    Ok(SerialNumber::from_slice(&bytes))
}

/// What a certificate (DER) says about itself: issuer and serial, subject, the SHA-256 of its
/// public key, and when it expires — for the register, and for a peer's certificate at admission.
///
/// # Errors
/// Returns an error when the certificate does not parse.
pub fn facts(der: &[u8]) -> Result<Facts, String> {
    use sha2::{Digest, Sha256};
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|e| format!("the certificate does not parse: {e}"))?;
    let not_after = cert.validity().not_after.timestamp();
    let host = cert
        .subject_alternative_name()
        .ok()
        .flatten()
        .and_then(|san| {
            san.value.general_names.iter().find_map(|name| match name {
                x509_parser::extensions::GeneralName::URI(uri) => {
                    uri.strip_prefix(HOST_URI_PREFIX).map(str::to_string)
                }
                _ => None,
            })
        });
    Ok(Facts {
        id: CertId::new(cert.issuer().as_raw(), &hex::encode(cert.raw_serial())),
        issuer_name: cert.issuer().to_string(),
        subject: cert.subject().to_string(),
        // The key itself, not its SubjectPublicKeyInfo: the fingerprint an enrolment request is
        // listed by and the Client logs.
        key_fingerprint: hex::encode(Sha256::digest(&cert.public_key().subject_public_key.data)),
        not_after_ms: u64::try_from(not_after).unwrap_or(0).saturating_mul(1000),
        host,
    })
}

/// Checks a CSR's claims to an `instance_uid` against its sender's (ADR-0050): every canonical
/// UUID in the subject or in a requested SAN that carries text must be `sender`.
///
/// # Errors
/// Returns the `BadRequest` text for a request that does not parse or claims another identity.
pub fn check_claims(csr_pem: &str, sender: &[u8]) -> Result<(), String> {
    let request = CertificateSigningRequestParams::from_pem(csr_pem)
        .map_err(|e| format!("the certificate signing request does not parse: {e}"))?;
    let mut values: Vec<String> = request
        .params
        .distinguished_name
        .iter()
        .map(|(_, value)| dn_text(value))
        .collect();
    for san in &request.params.subject_alt_names {
        match san {
            SanType::DnsName(name) => values.push(name.as_str().to_string()),
            // A renewal proof is base64 this project wrote, never a claim (ADR-0050).
            SanType::URI(uri) if uri.as_str().starts_with(fleet_core::renewal::URI_PREFIX) => {}
            SanType::URI(uri) => values.push(uri.as_str().to_string()),
            SanType::Rfc822Name(mail) => values.push(mail.as_str().to_string()),
            _ => {}
        }
    }
    for value in &values {
        for claim in uuid_claims(value) {
            if claim.as_slice() != sender {
                return Err(format!(
                    "the certificate signing request claims the instance_uid {}, which is not \
                     the sender's",
                    opamp::uid::InstanceUid::from_wire(&claim)
                        .map_or_else(|| hex::encode(claim), |uid| uid.to_string())
                ));
            }
        }
    }
    Ok(())
}

/// Every canonical UUID text — 8-4-4-4-12 hex digits, either case — in `value`, standing alone or
/// inside a longer value, but not as part of a longer run of hex digits (ADR-0050 clause 1).
fn uuid_claims(value: &str) -> Vec<[u8; 16]> {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let bytes = value.as_bytes();
    let mut claims = Vec::new();
    let mut start = 0;
    while start + 36 <= bytes.len() {
        let bounded_before = start == 0 || !bytes[start - 1].is_ascii_hexdigit();
        let bounded_after = bytes.get(start + 36).is_none_or(|b| !b.is_ascii_hexdigit());
        let mut at = start;
        let mut shaped = bounded_before && bounded_after;
        for (index, len) in GROUPS.iter().enumerate() {
            if !shaped {
                break;
            }
            shaped = bytes[at..at + len].iter().all(u8::is_ascii_hexdigit);
            at += len;
            if index < 4 {
                shaped = shaped && bytes[at] == b'-';
                at += 1;
            }
        }
        if shaped {
            let hex: String = value[start..start + 36]
                .chars()
                .filter(|c| *c != '-')
                .collect();
            let mut uid = [0u8; 16];
            if hex::decode_to_slice(&hex, &mut uid).is_ok() {
                claims.push(uid);
            }
            start += 36;
        } else {
            start += 1;
        }
    }
    claims
}

/// The DER of a PEM block.
fn pem_der(pem: &str) -> Result<Vec<u8>, String> {
    let (_, block) = x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map_err(|e| format!("the certificate signing request is not PEM: {e}"))?;
    Ok(block.contents)
}

/// Verifies `signature` over `message` with the key of `spki`: ECDSA P-256 or P-384 with the
/// matching SHA-2, Ed25519, or RSA PKCS#1 with SHA-256 — the keys a Client signs its renewals with.
fn verify_with(
    spki: &x509_parser::x509::SubjectPublicKeyInfo<'_>,
    message: &[u8],
    signature: &[u8],
) -> Result<(), ()> {
    use ring::signature as sig;
    let key_oid = spki.algorithm.algorithm.to_id_string();
    let curve = spki
        .algorithm
        .parameters
        .as_ref()
        .and_then(|p| p.as_oid().ok())
        .map(|oid| oid.to_id_string());
    let algorithm: &dyn sig::VerificationAlgorithm = match (key_oid.as_str(), curve.as_deref()) {
        ("1.2.840.10045.2.1", Some("1.2.840.10045.3.1.7")) => &sig::ECDSA_P256_SHA256_ASN1,
        ("1.2.840.10045.2.1", Some("1.3.132.0.34")) => &sig::ECDSA_P384_SHA384_ASN1,
        ("1.3.101.112", _) => &sig::ED25519,
        ("1.2.840.113549.1.1.1", _) => &sig::RSA_PKCS1_2048_8192_SHA256,
        _ => return Err(()),
    };
    sig::UnparsedPublicKey::new(algorithm, spki.subject_public_key.data.as_ref())
        .verify(message, signature)
        .map_err(|_| ())
}

/// How far an issued certificate's life starts before the moment it is signed, so an Agent whose
/// clock runs a little behind the Server's does not hold a certificate that is not yet valid.
const CLOCK_SKEW: time::Duration = time::Duration::minutes(5);

/// `now + validity_days`, in the time type rcgen speaks.
fn not_after(validity_days: u32) -> Result<time::OffsetDateTime, String> {
    let seconds = i64::from(validity_days) * 24 * 60 * 60;
    time::OffsetDateTime::now_utc()
        .checked_add(time::Duration::seconds(seconds))
        .ok_or_else(|| format!("validity_days = {validity_days} is out of range"))
}

/// What an enrolment request says about itself (ADR-0039 clause 22): its subject and the SHA-256
/// fingerprint of the public key it asks to be certified, by which the queue knows a re-sent
/// request.
///
/// # Errors
/// Returns an error when the request does not parse or its signature does not verify.
pub fn enrolment_request(
    csr_pem: &str,
    instance_uid: &[u8],
) -> Result<crate::enrolment::Request, String> {
    use rcgen::PublicKeyData as _;
    use sha2::{Digest, Sha256};
    let request = CertificateSigningRequestParams::from_pem(csr_pem)
        .map_err(|e| format!("the certificate signing request does not parse: {e}"))?;
    let subject = match request
        .params
        .distinguished_name
        .get(&rcgen::DnType::CommonName)
    {
        Some(value) => format!("CN={}", dn_text(value)),
        None => String::new(),
    };
    Ok(crate::enrolment::Request {
        csr_pem: csr_pem.to_string(),
        subject,
        key_fingerprint: hex::encode(Sha256::digest(request.public_key.der_bytes())),
        instance_uid: instance_uid.to_vec(),
    })
}

/// A distinguished-name value as text, whatever string type the request chose.
fn dn_text(value: &rcgen::DnValue) -> String {
    match value {
        rcgen::DnValue::Utf8String(s) => s.clone(),
        rcgen::DnValue::PrintableString(s) => s.as_str().to_string(),
        rcgen::DnValue::Ia5String(s) => s.as_str().to_string(),
        rcgen::DnValue::TeletexString(s) => s.as_str().to_string(),
        // Read as text, so a claim in either is found as in any other (ADR-0050 clause 2).
        rcgen::DnValue::BmpString(s) => String::from_utf16_lossy(
            &s.as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect::<Vec<_>>(),
        ),
        rcgen::DnValue::UniversalString(s) => s
            .as_bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|quad| {
                char::from_u32(u32::from_be_bytes(*quad)).unwrap_or(char::REPLACEMENT_CHARACTER)
            })
            .collect(),
        other => format!("{other:?}"),
    }
}

impl crate::fleet::CertificateSigner for ClientCa {
    fn sign(&self, csr_pem: &str, host: &str) -> Result<Signed, String> {
        ClientCa::sign(self, csr_pem, host)
    }

    fn renewal_proof(&self, csr_pem: &str) -> Result<Option<Facts>, String> {
        ClientCa::renewal_proof(self, csr_pem)
    }

    fn check_claims(&self, csr_pem: &str, sender: &[u8]) -> Result<(), String> {
        check_claims(csr_pem, sender)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CA and a Client, end to end: the Client's key never leaves it, and what comes back is a
    /// certificate the CA signed over the public half it was sent.
    fn ca() -> (String, String) {
        let key = KeyPair::generate().expect("ca key");
        let mut params =
            rcgen::CertificateParams::new(vec!["opamp-fleet-ca".to_string()]).expect("ca params");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).expect("self-signed ca");
        (cert.pem(), key.serialize_pem())
    }

    fn csr(common_name: &str) -> String {
        let key = KeyPair::generate().expect("client key");
        let params =
            rcgen::CertificateParams::new(vec![common_name.to_string()]).expect("client params");
        params
            .serialize_request(&key)
            .expect("csr")
            .pem()
            .expect("csr pem")
    }

    fn client_ca(validity_days: u32) -> ClientCa {
        let (cert_pem, key_pem) = ca();
        let key = KeyPair::from_pem(&key_pem).expect("ca key");
        ClientCa {
            issuer: Issuer::from_ca_cert_pem(&cert_pem, key).expect("issuer"),
            validity_days,
            validity: None,
            ca_der: opamp::tls::certificates(cert_pem.as_bytes()).expect("ca")[0]
                .as_ref()
                .to_vec(),
        }
    }

    /// A request that asks for the powers of a CA — `basicConstraints: CA`, a name of its own, and
    /// a wider extended key usage.
    fn hostile_csr() -> String {
        let key = KeyPair::generate().expect("client key");
        let mut params =
            rcgen::CertificateParams::new(vec!["edge-01".to_string()]).expect("client params");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.subject_alt_names.push(rcgen::SanType::DnsName(
            "evil.example".try_into().expect("san"),
        ));
        params
            .serialize_request(&key)
            .expect("csr")
            .pem()
            .expect("csr pem")
    }

    /// Verifies: ADR-0039
    #[test]
    fn signs_a_request_into_a_certificate() {
        let issued = client_ca(90)
            .sign(&csr("edge-01"), "host-1")
            .expect("issued")
            .pem;
        assert!(issued.starts_with("-----BEGIN CERTIFICATE-----"));
        // What came back is a certificate, not the request echoed back.
        assert!(!issued.contains("CERTIFICATE REQUEST"));
    }

    /// A CSR asking for CA powers is signed into a plain client-auth leaf: the request does not get
    /// to choose the certificate's powers (a CA cert chaining to the fleet CA could mint more). The
    /// issued certificate must be non-CA, carry only `clientAuth`, and none of the CSR's SANs — only
    /// its host.
    /// Verifies: ADR-0039
    #[test]
    fn the_request_cannot_dictate_the_certificates_powers() {
        let issued = client_ca(90)
            .sign(&hostile_csr(), "host-1")
            .expect("issued")
            .pem;
        let (_, pem) = x509_parser::pem::parse_x509_pem(issued.as_bytes()).expect("pem");
        let cert = pem.parse_x509().expect("der");

        // Absent basic constraints means non-CA, which is fine; present, it must say non-CA.
        if let Some(bc) = cert
            .basic_constraints()
            .expect("basic constraints readable")
        {
            assert!(!bc.value.ca, "the issued certificate must not be a CA");
        }
        let eku = cert
            .extended_key_usage()
            .expect("eku readable")
            .expect("eku present")
            .value;
        assert!(eku.client_auth, "the leaf authenticates a client");
        assert!(!eku.server_auth, "the CSR's serverAuth request was dropped");
        assert!(!eku.any, "no anyExtendedKeyUsage");
        let names: Vec<String> = cert
            .subject_alternative_name()
            .expect("san readable")
            .expect("the host")
            .value
            .general_names
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            names,
            vec![format!("URI({HOST_URI_PREFIX}host-1)")],
            "the CSR's SAN was dropped"
        );
    }

    /// An issued certificate's life is `validity_days` from now — not from a date long past, which
    /// would put it into its renewal window the moment it is issued and loop the Client into
    /// asking again and again.
    #[test]
    fn an_issued_certificate_lives_validity_days_from_now() {
        let issued = client_ca(90)
            .sign(&csr("edge-01"), "host-1")
            .expect("issued")
            .pem;
        let (_, pem) = x509_parser::pem::parse_x509_pem(issued.as_bytes()).expect("pem");
        let cert = pem.parse_x509().expect("der");
        let now = time::OffsetDateTime::now_utc();
        let not_before = cert.validity().not_before.to_datetime();
        let not_after = cert.validity().not_after.to_datetime();
        assert!(
            now - not_before <= time::Duration::minutes(6),
            "{not_before}"
        );
        assert!(not_before <= now, "valid from the moment it is issued");
        let life = not_after - not_before;
        assert!(
            life >= time::Duration::days(90)
                && life <= time::Duration::days(90) + time::Duration::minutes(6)
        );
    }

    /// The Baseline makes this a MUST on the Server: a request it cannot act on is answered with a
    /// `BadRequest` error response, which is what the caller does with this `Err`.
    /// Verifies: ADR-0039
    #[test]
    fn refuses_a_request_that_does_not_parse() {
        let error = client_ca(90)
            .sign(
                "-----BEGIN CERTIFICATE REQUEST-----\nnot base64\n-----END CERTIFICATE REQUEST-----",
                "host-1",
            )
            .expect_err("refused");
        assert!(error.contains("does not parse"), "{error}");
    }

    /// Verifies: ADR-0056
    #[test]
    fn two_certificates_from_one_key_have_two_serials() {
        let ca = client_ca(90);
        let request = csr("edge-01");
        let first = ca.sign(&request, "host-1").expect("first");
        let second = ca.sign(&request, "host-1").expect("second");
        assert_ne!(first.facts.id.serial, second.facts.id.serial);
        assert_eq!(first.facts.key_fingerprint, second.facts.key_fingerprint);
        assert_eq!(first.facts.id.issuer, second.facts.id.issuer);
        assert!(
            first.facts.issuer_name.starts_with("CN="),
            "{}",
            first.facts.issuer_name
        );
    }

    /// The register knows a certificate's key by the fingerprint its request was listed and
    /// approved by, which is also the one the Client logs: one key, one fingerprint.
    /// Verifies: ADR-0039
    #[test]
    fn a_certificate_carries_the_key_fingerprint_of_its_request() {
        let request = csr("edge-01");
        let signed = client_ca(90).sign(&request, "host-1").expect("signed");
        let enrolling = enrolment_request(&request, &[]).expect("request");
        assert_eq!(signed.facts.key_fingerprint, enrolling.key_fingerprint);
    }

    const UID: &str = "0192a3b4-c5d6-7e8f-9a0b-1c2d3e4f5a6b";

    fn uid_bytes() -> Vec<u8> {
        hex::decode(UID.replace('-', "")).expect("hex")
    }

    /// Verifies: ADR-0050
    #[test]
    fn a_canonical_uuid_anywhere_in_a_value_is_a_claim() {
        let upper = UID.to_uppercase();
        for value in [
            UID.to_string(),
            format!("agent {UID}"),
            format!("urn:uuid:{upper}"),
        ] {
            assert_eq!(
                uuid_claims(&value),
                vec![<[u8; 16]>::try_from(uid_bytes()).expect("16")]
            );
        }
        assert!(
            uuid_claims(&format!("a{UID}")).is_empty(),
            "a longer run of hex digits is no claim"
        );
    }

    /// Verifies: ADR-0050
    #[test]
    fn hex_without_hyphens_is_no_claim() {
        assert!(uuid_claims(&UID.replace('-', "")).is_empty());
        assert!(uuid_claims("edge-01.example").is_empty());
    }

    fn csr_with(common_name: &str, sans: Vec<SanType>) -> String {
        let key = KeyPair::generate().expect("client key");
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, common_name);
        params.subject_alt_names = sans;
        params
            .serialize_request(&key)
            .expect("csr")
            .pem()
            .expect("csr pem")
    }

    /// Verifies: ADR-0050
    #[test]
    fn a_san_is_read_for_claims() {
        let other = "0192a3b4-c5d6-7e8f-9a0b-000000000000";
        let request = csr_with(
            "edge-01",
            vec![SanType::URI(
                format!("urn:uuid:{other}").try_into().expect("uri"),
            )],
        );
        let error = check_claims(&request, &uid_bytes()).expect_err("another identity");
        assert!(error.contains(other), "{error}");
        assert!(check_claims(&csr_with(UID, Vec::new()), &uid_bytes()).is_ok());
        assert!(check_claims(&csr_with("edge-01", Vec::new()), &uid_bytes()).is_ok());
    }

    /// A claim in a subject attribute of another string type is read as any other.
    /// Verifies: ADR-0050
    #[test]
    fn a_claim_in_a_bmp_or_universal_string_is_read() {
        let other = "0192a3b4-c5d6-7e8f-9a0b-000000000000";
        for value in [
            rcgen::DnValue::BmpString(other.try_into().expect("bmp")),
            rcgen::DnValue::UniversalString(other.try_into().expect("universal")),
        ] {
            let key = KeyPair::generate().expect("client key");
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, value);
            let request = params
                .serialize_request(&key)
                .expect("csr")
                .pem()
                .expect("pem");
            let error = check_claims(&request, &uid_bytes()).expect_err("a hidden claim");
            assert!(error.contains(other), "{error}");
        }
    }

    /// A CSR made with the key of a certificate this CA issued
    /// proves which certificate it renews.
    fn renewing(old_cert_pem: &str, old_key: &KeyPair) -> String {
        use base64::Engine as _;
        use rcgen::{PublicKeyData as _, SigningKey as _};
        use sha2::{Digest, Sha256};
        let new_key = KeyPair::generate().expect("new key");
        let old_der = pem_der(old_cert_pem).expect("der");
        let signature = old_key
            .sign(&fleet_core::renewal::statement(&Sha256::digest(
                new_key.subject_public_key_info(),
            )))
            .expect("sign");
        let proof = format!(
            "{}{}",
            fleet_core::renewal::URI_PREFIX,
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(fleet_core::renewal::encode(&old_der, &signature))
        );
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
        params
            .subject_alt_names
            .push(SanType::URI(proof.try_into().expect("uri")));
        params
            .serialize_request(&new_key)
            .expect("csr")
            .pem()
            .expect("pem")
    }

    /// A renewal proof names the certificate it renews and its host; a proof signed with another
    /// key, or over a certificate of another CA, does not hold, and the issued certificate names
    /// its host and nothing the request asked for.
    /// Verifies: ADR-0039
    #[test]
    fn a_renewal_proof_names_the_certificate_and_its_host() {
        let ca = client_ca(30);
        let old_key = KeyPair::generate().expect("old key");
        let params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
        let old_csr = params
            .serialize_request(&old_key)
            .expect("csr")
            .pem()
            .expect("pem");
        let old = ca.sign(&old_csr, "host-7").expect("sign");
        assert_eq!(old.facts.host.as_deref(), Some("host-7"));

        assert_eq!(ca.renewal_proof(&csr("edge-01")).expect("no proof"), None);
        let proven = ca
            .renewal_proof(&renewing(&old.pem, &old_key))
            .expect("holds")
            .expect("a proof");
        assert_eq!(proven.id, old.facts.id);
        assert_eq!(proven.host.as_deref(), Some("host-7"));
        let renewed = ca
            .sign(&renewing(&old.pem, &old_key), "host-7")
            .expect("sign");
        let renewed_der = pem_der(&renewed.pem).expect("der");
        let (_, cert) = X509Certificate::from_der(&renewed_der).expect("parse");
        let names = cert
            .subject_alternative_name()
            .expect("san")
            .expect("san")
            .value
            .general_names
            .len();
        assert_eq!(names, 1, "the proof was copied into the certificate");
        let mut forged = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
        forged.subject_alt_names.push(SanType::URI(
            format!("{HOST_URI_PREFIX}host-other")
                .try_into()
                .expect("uri"),
        ));
        let forged = forged
            .serialize_request(&KeyPair::generate().expect("key"))
            .expect("csr")
            .pem()
            .expect("pem");
        assert_eq!(
            ca.sign(&forged, "host-7")
                .expect("sign")
                .facts
                .host
                .as_deref(),
            Some("host-7"),
            "a request named its own host"
        );

        let stranger = KeyPair::generate().expect("stranger");
        assert!(ca.renewal_proof(&renewing(&old.pem, &stranger)).is_err());
        let other = client_ca(30);
        let foreign = other.sign(&old_csr, "host-7").expect("sign");
        assert!(ca.renewal_proof(&renewing(&foreign.pem, &old_key)).is_err());
    }
}
