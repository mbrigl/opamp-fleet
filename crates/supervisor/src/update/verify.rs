//! Content-hash of a staged binary (ADR-0007).
//!
//! A new binary is only applied after its SHA-256 matches the expected content hash — the same hash
//! OpAMP's `DownloadableFile.content_hash` carries — so a corrupted or unexpected artifact never
//! reaches the pointer switch (the comparison lives in [`super::stage_and_verify`]). Signature
//! verification is a deliberate follow-up (ADR-0007).

use sha2::{Digest, Sha256};

/// The lowercase hex SHA-256 of the given bytes.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        // SHA-256 of the empty input.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn distinct_inputs_hash_differently() {
        assert_ne!(sha256_hex(b"a"), sha256_hex(b"b"));
    }
}
