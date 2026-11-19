//! A bootstrap certificate and its key as PEM (SEC-bootstrap-certificates-and-enrolment §15), and one PEM block written from
//! DER. Every key is held in a buffer wiped when it is dropped
//! (SEC-secrets-wiped-on-drop). A value only, so a core module of the Client
//! may name it (DEV-ports-and-adapters); [`pkcs12`] opens one from a file.

pub mod pkcs12;

use zeroize::Zeroizing;

/// A bootstrap certificate and its key as PEM, held in memory (SEC-bootstrap-certificates-and-enrolment §15), the key in a
/// buffer wiped when it is dropped (SEC-secrets-wiped-on-drop). Its `Debug` names neither.
#[derive(Clone, PartialEq, Eq)]
pub struct BootstrapIdentity {
    pub cert_pem: Vec<u8>,
    pub key_pem: zeroize::Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for BootstrapIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapIdentity").finish_non_exhaustive()
    }
}

/// `der` as one PEM block, in buffers wiped when they are dropped and sized up front, so none is
/// grown and left behind (SEC-secrets-wiped-on-drop).
pub fn pem_block(label: &str, der: &[u8]) -> Zeroizing<String> {
    use base64::Engine as _;
    let body = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(der));
    let (begin, end) = (
        format!("-----BEGIN {label}-----\n"),
        format!("-----END {label}-----\n"),
    );
    let lines = body.len().div_ceil(64);
    let mut pem = Zeroizing::new(String::with_capacity(
        begin.len() + body.len() + lines + end.len(),
    ));
    pem.push_str(&begin);
    for line in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str(&end);
    pem
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PEM block is the label's frame around the base64 of the DER, 64 characters a line, and
    /// its `Debug` names neither the certificate nor the key.
    #[test]
    fn a_pem_block_frames_its_der_in_lines_of_64() {
        let der = vec![0xA5_u8; 100];
        let pem = pem_block("CERTIFICATE", &der);
        let lines: Vec<&str> = pem.lines().collect();
        assert_eq!(lines.first(), Some(&"-----BEGIN CERTIFICATE-----"));
        assert_eq!(lines.last(), Some(&"-----END CERTIFICATE-----"));
        assert!(lines[1..lines.len() - 1].iter().all(|l| l.len() <= 64));
        use base64::Engine as _;
        let body: String = lines[1..lines.len() - 1].concat();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(body)
                .expect("b64"),
            der
        );
        let identity = BootstrapIdentity {
            cert_pem: b"cert".to_vec(),
            key_pem: Zeroizing::new(b"key".to_vec()),
        };
        assert_eq!(format!("{identity:?}"), "BootstrapIdentity { .. }");
    }
}
