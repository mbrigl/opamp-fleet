//! What a client certificate is known by, on the Server and on a Gateway alike: the host it names,
//! its issuer and serial as the Server's register and revocation list name it, and the bound on
//! its serial (ADR-0015, ADR-0021, ADR-0022 clause 11).
//!
//! This module is the rule. [`read`] reads it from a certificate with `x509-parser`, and [`issue`]
//! holds what every certificate this project makes gets: its serial and how far its life starts
//! before it is signed.

pub mod issue;
pub mod read;

use sha2::{Digest as _, Sha256};

/// The SAN URI prefix naming the host a certificate was issued to (ADR-0022 clause 11).
pub const HOST_URI_PREFIX: &str = "urn:opamp-fleet:host:";

/// The most octets a serial has: RFC 5280 4.1.2.2's 20, the length the register and every
/// Gateway's list are bounded on (Q-2).
pub const MAX_SERIAL_OCTETS: usize = 20;

/// The most hex digits a serial has: [`MAX_SERIAL_OCTETS`] of them.
pub const MAX_SERIAL_HEX: usize = 2 * MAX_SERIAL_OCTETS;

/// The SHA-256 of a DER-encoded name, hex: how a certificate's issuer and a CA's subject are
/// named, so no text form of a name can fail to match.
#[must_use]
pub fn name_hash(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

/// A serial as lowercase hex without separators or leading zeros: `00:0A:1b` and `a1b` are one
/// serial.
#[must_use]
pub fn normalize_serial(serial: &str) -> String {
    let hex: String = serial
        .chars()
        .filter(|c| !matches!(c, ':' | ' '))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let trimmed = hex.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Separators, case and leading zeros do not make another serial; zero stays one digit.
    /// Verifies: ADR-0021
    #[test]
    fn a_serial_is_named_one_way() {
        assert_eq!(normalize_serial("00:0A:1b"), "a1b");
        assert_eq!(normalize_serial("a1b"), "a1b");
        assert_eq!(normalize_serial("00 00"), "0");
    }
}
