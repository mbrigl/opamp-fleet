//! Config hash — the identity of a configuration (specification vocabulary).
//!
//! An Agent reports the hash it last received; the Server compares it against the hash of the
//! configuration the Agent should have, and sends a new configuration only when they differ. This
//! comparison is the control loop, so both ends MUST compute the hash the same way — hence it lives
//! in the shared crate.

use sha2::{Digest, Sha256};

/// Compute the Config hash of a configuration's bytes (SHA-256, 32 bytes).
#[must_use]
pub fn config_hash(config: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(config);
    hasher.finalize().to_vec()
}

/// Lowercase hex encoding of a byte slice — for logging and the UI (e.g. a Config hash).
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_and_32_bytes() {
        let a = config_hash(b"receivers: {}");
        let b = config_hash(b"receivers: {}");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn different_input_different_hash() {
        assert_ne!(config_hash(b"one"), config_hash(b"two"));
    }

    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
