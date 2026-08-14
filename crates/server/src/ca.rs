//! The Server as a local certificate authority (ADR-0035).
//!
//! The Baseline's CSR flow lets an Agent keep its private key and ask for a certificate over the
//! connection it already has: it sends a PEM certificate signing request, and the Server "creates a
//! client certificate … either by issuing a self-signed certificate (acting as a local CA) or
//! proxies the CSR to a CA". This is the first of those; proxying to an external CA is future work
//! and would sit behind the same `[client_ca]` seam.
//!
//! Nothing here decides *who* may enrol. Admission does that, before a message reaches this module
//! (`transport::Admission`): a CSR that arrives has already proved everything the endpoint asks of
//! any other message, and that is the approval the specification's flow calls for.

use rcgen::{
    CertificateSigningRequestParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};

use crate::config::ClientCaConfig;

/// The issuing authority, loaded once at startup. Holding it parsed is what makes `AppState`'s
/// capability honest: `AcceptsConnectionSettingsRequest` is declared only while this exists.
pub struct ClientCa {
    issuer: Issuer<'static, KeyPair>,
    validity_days: u32,
}

impl ClientCa {
    /// Loads the CA from `[client_ca]`. A key that does not match its certificate, or either file
    /// being unreadable, fails startup rather than the first enrolment (ADR-0008).
    pub fn from_config(config: &ClientCaConfig) -> Result<Self, String> {
        let cert_pem = std::fs::read_to_string(&config.cert_file)
            .map_err(|e| format!("cannot read {}: {e}", config.cert_file.display()))?;
        let key_pem = std::fs::read_to_string(&config.key_file)
            .map_err(|e| format!("cannot read {}: {e}", config.key_file.display()))?;
        let key = KeyPair::from_pem(&key_pem)
            .map_err(|e| format!("cannot read {}: {e}", config.key_file.display()))?;
        let issuer = Issuer::from_ca_cert_pem(&cert_pem, key)
            .map_err(|e| format!("cannot use {} as a CA: {e}", config.cert_file.display()))?;
        Ok(ClientCa {
            issuer,
            validity_days: config.validity_days,
        })
    }

    /// Signs an Agent's CSR and returns the issued certificate as PEM.
    ///
    /// The subject comes from the request: it is descriptive, and this Server does not require it
    /// to match anything the Agent reports. Binding a certificate to an `instance_uid` would mean
    /// it dies the moment the Server re-keys that Agent through `AgentIdentification` — an outage
    /// of the Server's own making (ADR-0035).
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
    pub fn sign(&self, csr_pem: &str) -> Result<String, String> {
        let mut request = CertificateSigningRequestParams::from_pem(csr_pem)
            .map_err(|e| format!("the certificate signing request does not parse: {e}"))?;
        request.params.not_before = rcgen::date_time_ymd(1975, 1, 1);
        request.params.not_after = not_after(self.validity_days)?;
        // Never trust the request for the certificate's powers: force a client-auth leaf.
        request.params.is_ca = IsCa::ExplicitNoCa;
        request.params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        request.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        request.params.subject_alt_names.clear();
        let certificate = request
            .signed_by(&self.issuer)
            .map_err(|e| format!("cannot sign the certificate signing request: {e}"))?;
        Ok(certificate.pem())
    }

    pub fn validity_days(&self) -> u32 {
        self.validity_days
    }
}

/// `now + validity_days`, in the time type rcgen speaks.
fn not_after(validity_days: u32) -> Result<time::OffsetDateTime, String> {
    let seconds = i64::from(validity_days) * 24 * 60 * 60;
    time::OffsetDateTime::now_utc()
        .checked_add(time::Duration::seconds(seconds))
        .ok_or_else(|| format!("validity_days = {validity_days} is out of range"))
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

    #[test]
    fn signs_a_request_into_a_certificate() {
        let issued = client_ca(90).sign(&csr("edge-01")).expect("issued");
        assert!(issued.starts_with("-----BEGIN CERTIFICATE-----"));
        // What came back is a certificate, not the request echoed back.
        assert!(!issued.contains("CERTIFICATE REQUEST"));
    }

    /// A CSR asking for CA powers is signed into a plain client-auth leaf: the request does not get
    /// to choose the certificate's powers (a CA cert chaining to the fleet CA could mint more). The
    /// issued certificate must be non-CA, carry only `clientAuth`, and none of the CSR's SANs.
    #[test]
    fn the_request_cannot_dictate_the_certificates_powers() {
        let issued = client_ca(90).sign(&hostile_csr()).expect("issued");
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
        assert!(
            cert.subject_alternative_name()
                .expect("san readable")
                .is_none(),
            "the CSR's SAN was dropped"
        );
    }

    /// The Baseline makes this a MUST on the Server: a request it cannot act on is answered with a
    /// `BadRequest` error response, which is what the caller does with this `Err`.
    #[test]
    fn refuses_a_request_that_does_not_parse() {
        let error = client_ca(90)
            .sign("-----BEGIN CERTIFICATE REQUEST-----\nnot base64\n-----END CERTIFICATE REQUEST-----")
            .expect_err("refused");
        assert!(error.contains("does not parse"), "{error}");
    }
}
