//! What every certificate this project makes gets, by the Server's client and bootstrap CAs and by
//! the operator tool alike (ADR-0008 clause 18, ADR-0032 clause 5): a random serial, a life that
//! starts a little before it is signed, the shape of a leaf by its role, and the authority that
//! signs it, loaded from a CA's certificate and its key file.

use std::path::Path;

use rcgen::{
    CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    SerialNumber,
};
use zeroize::Zeroizing;

/// How far an issued certificate's life starts before the moment it is signed, so an Agent whose
/// clock runs a little behind the signer's does not hold a certificate that is not yet valid.
pub const CLOCK_SKEW: time::Duration = time::Duration::minutes(5);

/// 16 bytes from the system's secure random source, the top bit cleared so the serial is a
/// positive integer (RFC 5280 4.1.2.2).
///
/// # Errors
/// Returns an error when the system has no secure random source.
pub fn random_serial() -> Result<SerialNumber, String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "no secure random source for a serial number".to_string())?;
    bytes[0] &= 0x7f;
    Ok(SerialNumber::from_slice(&bytes))
}

/// What a leaf certificate is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A Client or a Gateway authenticating to the Server: `clientAuth`.
    Client,
    /// A Server's listener: `serverAuth`.
    Server,
}

/// Makes `params` a leaf of `role` and nothing more, whatever they held before: `CA:FALSE`,
/// `digitalSignature`, the extended key usage of its role alone, and a random serial of its own,
/// which it returns. Its names and its life are the caller's.
///
/// # Errors
/// Returns an error when the system has no secure random source.
pub fn leaf(params: &mut CertificateParams, role: Role) -> Result<SerialNumber, String> {
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![match role {
        Role::Client => ExtendedKeyUsagePurpose::ClientAuth,
        Role::Server => ExtendedKeyUsagePurpose::ServerAuth,
    }];
    let serial = random_serial()?;
    params.serial_number = Some(serial.clone());
    Ok(serial)
}

/// The private key in `key_file`, PKCS#8 PEM. The text is wiped when it is dropped; what `rcgen`
/// and `ring` keep of it is theirs (ADR-0014).
///
/// # Errors
/// Returns an error naming `key_file` when it cannot be read or holds no key `rcgen` reads.
pub fn read_key(key_file: &Path) -> Result<KeyPair, String> {
    let key_pem = std::fs::read_to_string(key_file)
        .map(Zeroizing::new)
        .map_err(|e| format!("cannot read {}: {e}", key_file.display()))?;
    KeyPair::from_pem(&key_pem).map_err(|e| format!("cannot read {}: {e}", key_file.display()))
}

/// The authority that signs with the key in `key_file` under the CA certificate `cert_der`, which
/// the caller read as the first certificate of `cert_file`. Whether the key belongs to the
/// certificate is not checked here.
///
/// # Errors
/// Returns an error naming the file when the key cannot be read, or the certificate cannot serve
/// as a CA.
pub fn authority(
    cert_file: &Path,
    cert_der: &[u8],
    key_file: &Path,
) -> Result<Issuer<'static, KeyPair>, String> {
    let key = read_key(key_file)?;
    Issuer::from_ca_cert_der(&cert_der.to_vec().into(), key)
        .map_err(|e| format!("cannot use {} as a CA: {e}", cert_file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CA's certificate and key, written to `dir`.
    fn ca(dir: &Path) -> (std::path::PathBuf, Vec<u8>, std::path::PathBuf) {
        let key = KeyPair::generate().expect("key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let cert = params.self_signed(&key).expect("self-sign");
        let (cert_file, key_file) = (dir.join("ca.pem"), dir.join("ca-key.pem"));
        std::fs::write(&cert_file, cert.pem()).expect("write");
        std::fs::write(&key_file, key.serialize_pem()).expect("write");
        (cert_file, cert.der().to_vec(), key_file)
    }

    /// The authority a CA's files load to signs a leaf that verifies under that CA, and a key file
    /// that cannot be read or holds no key is refused, naming it.
    /// Verifies: ADR-0032
    #[test]
    fn an_authority_loaded_from_its_files_signs_what_its_certificate_verifies() {
        use x509_parser::prelude::{FromDer as _, X509Certificate};
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert_file, cert_der, key_file) = ca(dir.path());
        let issuer = authority(&cert_file, &cert_der, &key_file).expect("authority");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        leaf(&mut params, Role::Client).expect("leaf");
        let signed = params
            .signed_by(&KeyPair::generate().expect("key"), &issuer)
            .expect("sign");
        let (_, ca) = X509Certificate::from_der(&cert_der).expect("ca");
        let (_, issued) = X509Certificate::from_der(signed.der()).expect("leaf");
        issued
            .verify_signature(Some(ca.public_key()))
            .expect("signed by the CA's key");

        let missing = dir.path().join("none.pem");
        let error = authority(&cert_file, &cert_der, &missing).expect_err("refused");
        assert!(error.contains("none.pem"), "{error}");
        let error = authority(&cert_file, &cert_der, &cert_file).expect_err("not a key");
        assert!(error.contains("ca.pem"), "{error}");
    }

    /// A leaf is what its role says and nothing more, whatever the parameters asked for.
    /// Verifies: ADR-0032
    #[test]
    fn a_leaf_is_its_role_alone() {
        for (role, purpose) in [
            (Role::Client, ExtendedKeyUsagePurpose::ClientAuth),
            (Role::Server, ExtendedKeyUsagePurpose::ServerAuth),
        ] {
            let mut params = CertificateParams::default();
            params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::Any];
            let serial = leaf(&mut params, role).expect("leaf");
            assert!(matches!(params.is_ca, IsCa::ExplicitNoCa));
            assert_eq!(params.key_usages, [KeyUsagePurpose::DigitalSignature]);
            assert_eq!(params.extended_key_usages, [purpose]);
            assert_eq!(params.serial_number, Some(serial));
        }
    }

    /// A serial is 16 bytes, positive, and not the one before.
    /// Verifies: ADR-0032
    #[test]
    fn a_serial_is_sixteen_random_positive_bytes() {
        let first = random_serial().expect("serial").to_bytes();
        let second = random_serial().expect("serial").to_bytes();
        assert_eq!(first.len(), 16);
        assert!(first[0] < 0x80, "positive");
        assert_ne!(first, second);
    }
}
