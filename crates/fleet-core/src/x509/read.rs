//! A client certificate read with `x509-parser`: the adapter the Server's admission and a
//! Gateway's downstream admission read a peer's certificate through, so both take and name one
//! alike.

use x509_parser::prelude::{FromDer as _, SubjectPublicKeyInfo, X509Certificate};

use super::{name_hash, normalize_serial, HOST_URI_PREFIX, MAX_SERIAL_OCTETS};

/// What a certificate's serial past [`MAX_SERIAL_OCTETS`] is refused with.
pub const SERIAL_TOO_LONG: &str = "the certificate's serial is longer than 20 octets";

/// The certificate `der`, when it parses and its serial is at most 20 octets. A longer serial —
/// only a foreign CA could issue one — could not be named by the register or a Gateway's list, so
/// the certificate is not taken (RFC 5280 4.1.2.2, Q-2).
///
/// # Errors
/// Returns why the certificate is not taken.
pub fn certificate(der: &[u8]) -> Result<X509Certificate<'_>, String> {
    let (_, certificate) = X509Certificate::from_der(der)
        .map_err(|e| format!("the certificate does not parse: {e}"))?;
    if certificate.raw_serial().len() > MAX_SERIAL_OCTETS {
        return Err(SERIAL_TOO_LONG.to_string());
    }
    Ok(certificate)
}

/// A certificate's issuer and serial as the Server's register and revocation list name it: the
/// [`name_hash`] of its issuer, and its serial [normalized](normalize_serial).
#[must_use]
pub fn id(certificate: &X509Certificate<'_>) -> (String, String) {
    (
        name_hash(certificate.issuer().as_raw()),
        normalize_serial(&hex::encode(certificate.raw_serial())),
    )
}

/// The host a certificate names (`urn:opamp-fleet:host:<id>`), if it names one: the first SAN URI
/// with [`HOST_URI_PREFIX`].
#[must_use]
pub fn host(certificate: &X509Certificate<'_>) -> Option<String> {
    let san = certificate.subject_alternative_name().ok()??;
    san.value.general_names.iter().find_map(|name| match name {
        x509_parser::extensions::GeneralName::URI(uri) => {
            uri.strip_prefix(HOST_URI_PREFIX).map(str::to_string)
        }
        _ => None,
    })
}

/// One certificate of a CA file, as far as it is read: its subject, raw and as text, and its
/// public key (`SubjectPublicKeyInfo`, DER).
#[derive(Clone, Debug)]
pub struct Ca {
    pub subject: Vec<u8>,
    pub name: String,
    pub key: Vec<u8>,
}

impl Ca {
    /// The CA certificate `der`.
    ///
    /// # Errors
    /// Returns the parser's error; what the file means, and so the wording, is the caller's.
    pub fn of(der: &[u8]) -> Result<Ca, String> {
        let (_, certificate) = X509Certificate::from_der(der).map_err(|e| e.to_string())?;
        Ok(Ca {
            subject: certificate.subject().as_raw().to_vec(),
            name: certificate.subject().to_string(),
            key: certificate.public_key().raw.to_vec(),
        })
    }

    /// Whether this CA issued `certificate` directly: named as its issuer and signed with its key.
    /// A certificate from an intermediate below it is not, whatever the intermediate is named.
    #[must_use]
    pub fn issued(&self, certificate: &X509Certificate<'_>) -> bool {
        certificate.issuer().as_raw() == self.subject.as_slice()
            && SubjectPublicKeyInfo::from_der(&self.key)
                .is_ok_and(|(_, key)| certificate.verify_signature(Some(&key)).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_serial(octets: usize) -> Vec<u8> {
        let key = rcgen::KeyPair::generate().expect("key");
        let mut params = rcgen::CertificateParams::new(vec!["edge".to_string()]).expect("params");
        let mut serial = vec![0x11; octets];
        serial[0] = 0x01;
        params.serial_number = Some(rcgen::SerialNumber::from(serial));
        params.self_signed(&key).expect("cert").der().to_vec()
    }

    /// A serial of 20 octets is taken, one of 21 is not.
    /// Verifies: Q-2
    #[test]
    fn a_certificate_with_a_serial_past_twenty_octets_is_not_taken() {
        assert!(certificate(&with_serial(20)).is_ok());
        let refused = certificate(&with_serial(21)).expect_err("21 octets");
        assert_eq!(refused, SERIAL_TOO_LONG);
    }

    /// The host is the first SAN URI with the prefix; the id is the issuer's hash and the serial
    /// without leading zeros.
    /// Verifies: ADR-0022
    #[test]
    fn a_certificate_is_known_by_its_host_issuer_and_serial() {
        let key = rcgen::KeyPair::generate().expect("key");
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
        for uri in [
            "urn:other:x",
            "urn:opamp-fleet:host:edge-01",
            "urn:opamp-fleet:host:b",
        ] {
            params
                .subject_alt_names
                .push(rcgen::SanType::URI(uri.try_into().expect("uri")));
        }
        params.serial_number = Some(rcgen::SerialNumber::from_slice(&[0x00, 0x0a, 0x1b]));
        let der = params.self_signed(&key).expect("cert").der().to_vec();
        let parsed = certificate(&der).expect("taken");
        assert_eq!(host(&parsed).as_deref(), Some("edge-01"));
        let (issuer, serial) = id(&parsed);
        assert_eq!(issuer, name_hash(parsed.issuer().as_raw()));
        assert_eq!(serial, "a1b");
    }

    /// A CA issued what it signed under its own name, and not what another key signed under it.
    /// Verifies: ADR-0015
    #[test]
    fn a_ca_issued_only_what_it_signed_directly() {
        let ca_key = rcgen::KeyPair::generate().expect("ca key");
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "client ca");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca");
        let ca = Ca::of(ca_cert.der()).expect("ca");
        let issuer = rcgen::Issuer::new(ca_params.clone(), ca_key);
        let leaf_key = rcgen::KeyPair::generate().expect("leaf key");
        let leaf = rcgen::CertificateParams::new(vec!["edge".to_string()])
            .expect("params")
            .signed_by(&leaf_key, &issuer)
            .expect("leaf");
        assert!(ca.issued(&certificate(leaf.der()).expect("leaf")));
        // The same name, another key.
        let impostor = rcgen::Issuer::new(ca_params, rcgen::KeyPair::generate().expect("key"));
        let forged = rcgen::CertificateParams::new(vec!["edge".to_string()])
            .expect("params")
            .signed_by(&leaf_key, &impostor)
            .expect("forged");
        assert!(!ca.issued(&certificate(forged.der()).expect("forged")));
    }
}
