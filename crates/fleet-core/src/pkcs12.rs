//! The bootstrap certificate as a password-protected PKCS#12 file (ADR-0029): what the Server
//! writes when an operator issues a bootstrap certificate with a password, and what the Client
//! enrols with.
//!
//! One file holds one private key and the one certificate it belongs to, nothing else — neither the
//! bootstrap CA nor the server CA (ADR-0029 clauses 1 and 12). Both are encrypted with PBES2
//! (PBKDF2-HMAC-SHA-256, AES-256-CBC) and the file carries an HMAC-SHA-256 MAC, each at
//! [`ITERATIONS`].
//!
//! This module is the rule — the name, the bounds, the password — and [`file`] writes and reads the
//! file with `p12-keystore`, the one technology here.

pub mod file;

/// The name of the file, wherever it lies: in the Server's download and beside the Client's
/// program (ADR-0029 clauses 4 and 6).
pub const FILE_NAME: &str = "bootstrap.p12";

/// The largest file the Client reads (ADR-0029 clause 7). A bootstrap certificate and its P-256
/// key make about 2 KiB.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;

/// The shortest password the Server takes (ADR-0030 clause 1).
pub const MIN_PASSWORD_CHARS: usize = 4;

/// The longest password the Server takes (ADR-0030 clause 1).
pub const MAX_PASSWORD_CHARS: usize = 128;

/// The PBKDF2 iterations of the encryption, and those of the MAC (ADR-0029 clause 1).
pub const ITERATIONS: u32 = 600_000;

/// Whether `password` is one the Server takes: 4 to 128 characters without a control character.
///
/// # Errors
/// Returns the `400` text naming the rule it breaks.
pub fn check_password(password: &str) -> Result<(), String> {
    let chars = password.chars().count();
    if !(MIN_PASSWORD_CHARS..=MAX_PASSWORD_CHARS).contains(&chars) {
        return Err(format!(
            "password must be {MIN_PASSWORD_CHARS} to {MAX_PASSWORD_CHARS} characters"
        ));
    }
    if password.chars().any(char::is_control) {
        return Err("password must not carry a control character".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies: ADR-0030
    #[test]
    fn a_password_is_four_to_128_characters_without_control_characters() {
        assert!(check_password("abc").is_err());
        assert!(check_password("four").is_ok());
        assert!(
            check_password(&"ä".repeat(128)).is_ok(),
            "characters, not bytes"
        );
        assert!(check_password(&"a".repeat(129)).is_err());
        assert!(check_password("fo\tur").is_err());
    }
}
