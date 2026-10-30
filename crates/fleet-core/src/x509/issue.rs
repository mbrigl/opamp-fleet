//! What every certificate this project makes gets, by the Server's client and bootstrap CAs and by
//! the operator tool alike (ADR-0008 clause 18, ADR-0032 clause 5): a random serial, and a life
//! that starts a little before it is signed.

use rcgen::SerialNumber;

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

#[cfg(test)]
mod tests {
    use super::*;

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
