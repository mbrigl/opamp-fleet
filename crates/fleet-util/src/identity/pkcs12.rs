//! A bootstrap identity opened from a bootstrap PKCS#12 file, the way the Client's install opens it
//! (ADR-0027 clause 15). The operator tool's `pki p12-extract` opens a file with the same reader
//! (ADR-0043 clause 24).

use std::path::Path;

use zeroize::Zeroizing;

use super::{pem_block, BootstrapIdentity};

/// The identity in `bytes`, the PKCS#12 file at `path`, under `password`, as PEM in memory
/// (ADR-0027 clause 15). The caller reads the file, through no link the service's account placed
/// and at most [`fleet_core::pkcs12::MAX_FILE_BYTES`] of it (ADR-0042 clause 11).
///
/// # Errors
/// [`OpenError::WrongPassword`](fleet_core::pkcs12::file::OpenError) when the password does not
/// open it, and `Invalid` naming the file for everything else.
pub fn open(
    path: &Path,
    bytes: &[u8],
    password: &str,
) -> Result<BootstrapIdentity, fleet_core::pkcs12::file::OpenError> {
    use fleet_core::pkcs12::file::OpenError;
    let invalid = |why: String| OpenError::Invalid(format!("{}: {why}", path.display()));
    let opened = fleet_core::pkcs12::file::open(bytes, password).map_err(|e| match e {
        OpenError::WrongPassword => OpenError::WrongPassword,
        OpenError::Invalid(why) => invalid(why),
    })?;
    let mut cert_pem = pem_block("CERTIFICATE", &opened.certificate_der);
    let mut key_pem = pem_block("PRIVATE KEY", &opened.private_key_der);
    Ok(BootstrapIdentity {
        cert_pem: std::mem::take(&mut *cert_pem).into_bytes(),
        // The buffer moves; nothing is copied.
        key_pem: Zeroizing::new(std::mem::take(&mut *key_pem).into_bytes()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_core::pkcs12::file::OpenError;

    /// A sealed bootstrap file opens to its certificate and key as PEM under its password; a
    /// wrong password is `WrongPassword`, and anything else is `Invalid`, naming the file
    /// (ADR-0027 clause 15).
    /// Verifies: ADR-0027
    #[test]
    fn a_bootstrap_file_opens_under_its_password_alone() {
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec!["bootstrap".to_string()])
            .expect("params")
            .self_signed(&key)
            .expect("cert");
        let path = Path::new("bootstrap.p12");
        let bytes =
            fleet_core::pkcs12::file::seal(cert.der(), &key.serialize_der(), "twelve chars")
                .expect("sealed");
        assert_eq!(
            open(path, &bytes, "not the password"),
            Err(OpenError::WrongPassword)
        );
        let identity = open(path, &bytes, "twelve chars").expect("opens");
        assert_eq!(
            identity.cert_pem,
            pem_block("CERTIFICATE", cert.der()).as_bytes()
        );
        assert_eq!(
            identity.key_pem.as_slice(),
            pem_block("PRIVATE KEY", &key.serialize_der()).as_bytes()
        );
        match open(path, b"not a PKCS#12 file", "twelve chars") {
            Err(OpenError::Invalid(why)) => assert!(why.contains("bootstrap.p12"), "{why}"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
}
