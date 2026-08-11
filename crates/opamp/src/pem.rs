//! Reading certificates and a private key out of PEM bytes (ADR-0044).
//!
//! Both ends parse PEM: the Client for its trust file and its own identity, the Server for the
//! listener's certificate and the client CA that turns mutual TLS on
//! ([ADR-0035](../../../docs/adr/0035-mutual-tls-and-the-server-issued-client-certificate.md)).
//! The parsing is one thing written twice; what the file *means* is not, so this module takes bytes
//! and each end keeps its own path-based wrapper — the error naming a trust anchor, a listener's
//! key or a client CA is written where that is known.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Every certificate in `pem`, in the order it appears — a chain, or a bundle of trust anchors.
///
/// An empty file is an error rather than an empty chain: a trust bundle that parsed to nothing
/// would configure a TLS stack that trusts nobody, and it would do it silently.
///
/// The PEM reader is `rustls-pki-types`' own (the crate `rustls::pki_types` re-exports), which
/// absorbed `rustls-pemfile` when that was retired (RUSTSEC-2025-0134) — so the parsing stays the
/// one rustls itself uses, without the unmaintained dependency.
pub fn certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs: Result<Vec<_>, _> = CertificateDer::pem_slice_iter(pem).collect();
    let certs = certs.map_err(|e| format!("cannot parse a certificate: {e}"))?;
    if certs.is_empty() {
        return Err("no certificates".to_string());
    }
    Ok(certs)
}

/// The first private key in `pem`, in any of the encodings rustls accepts (PKCS#8, PKCS#1, SEC1).
///
/// A file with no key in it is an error, not an absence: the `NoItemsFound` case and a malformed
/// key both surface as `Err`, which the path-based callers wrap with what the file was meant to be.
pub fn private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, String> {
    PrivateKeyDer::from_pem_slice(pem).map_err(|e| format!("cannot parse a private key: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed certificate and its key, generated rather than pasted: a fixture with an
    /// expiry date is a test that starts failing on a date nobody chose.
    fn pair() -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("generate a key");
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("parameters")
            .self_signed(&key)
            .expect("self-sign");
        (cert.pem(), key.serialize_pem())
    }

    #[test]
    fn reads_a_certificate_and_a_key() {
        let (cert_pem, key_pem) = pair();
        assert_eq!(certificates(cert_pem.as_bytes()).expect("certs").len(), 1);
        private_key(key_pem.as_bytes()).expect("a key");
    }

    /// A bundle is several anchors in one file, and all of them have to arrive — a CA file read as
    /// its first certificate alone would silently stop trusting the rest of the fleet's issuers.
    #[test]
    fn reads_every_certificate_of_a_bundle_in_order() {
        let (first, _) = pair();
        let (second, _) = pair();
        let bundle = format!("{first}{second}");
        let certs = certificates(bundle.as_bytes()).expect("certs");
        assert_eq!(certs.len(), 2);
        assert_eq!(certs[0], certificates(first.as_bytes()).expect("first")[0]);
    }

    /// Fail closed: a file with no certificate in it is not an empty trust store.
    #[test]
    fn a_file_holding_no_certificate_is_an_error() {
        assert!(certificates(b"").is_err());
        assert!(certificates(b"not pem at all").is_err());
        let (_, key_pem) = pair();
        assert!(
            certificates(key_pem.as_bytes()).is_err(),
            "a key is not a certificate"
        );
    }

    #[test]
    fn a_file_holding_no_key_is_an_error() {
        assert!(private_key(b"").is_err());
        let (cert_pem, _) = pair();
        assert!(
            private_key(cert_pem.as_bytes()).is_err(),
            "a certificate is not a key"
        );
    }
}
