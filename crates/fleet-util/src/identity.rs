//! A bootstrap certificate and its key as PEM (ADR-0027 clause 15), and one PEM block written from
//! DER. Every key is held in a buffer wiped when it is dropped
//! (ADR-0040). A value only, so a core module of the Client
//! may name it (ADR-0009); [`pkcs12`] opens one from a file.

pub mod pkcs12;

use zeroize::Zeroizing;

/// A bootstrap certificate and its key as PEM, held in memory (ADR-0027 clause 15), the key in a
/// buffer wiped when it is dropped (ADR-0040). Its `Debug` names neither.
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
/// grown and left behind (ADR-0040).
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
