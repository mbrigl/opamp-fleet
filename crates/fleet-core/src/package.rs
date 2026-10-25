//! What a package signature covers (ADR-0026): not the artifact's bytes alone, but which Agent type
//! the artifact is for, at which version, and its SHA-256 — so a signed artifact cannot be offered
//! as another type's program, or under another version.
//!
//! Beside it, what a floor's signature covers (ADR-0060 clause 1): an Agent type and the version
//! below which nothing of that type is taken. The two statements open with different prefixes, so
//! no package signature verifies as a floor and no floor as a package.

/// The bytes an operator signs and a Client verifies for one artifact:
/// `opamp-fleet-package-v1`, the Agent type, the version and the artifact's SHA-256 in lowercase
/// hex, one per line, each line ended by a newline.
#[must_use]
pub fn statement(agent_type: &str, version: &str, content_hash: &[u8]) -> Vec<u8> {
    let mut hex = String::with_capacity(content_hash.len() * 2);
    for byte in content_hash {
        hex.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        hex.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    format!("opamp-fleet-package-v1\n{agent_type}\n{version}\n{hex}\n").into_bytes()
}

/// The bytes an operator signs and a Client verifies for one floor (ADR-0060 clause 1):
/// `opamp-fleet-floor-v1`, the Agent type and the version, one per line, each line ended by a
/// newline.
#[must_use]
pub fn floor_statement(agent_type: &str, version: &str) -> Vec<u8> {
    format!("opamp-fleet-floor-v1\n{agent_type}\n{version}\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair as _, UnparsedPublicKey, ED25519};

    /// Verifies: ADR-0026
    #[test]
    fn the_statement_names_type_version_and_hash() {
        assert_eq!(
            statement("otelcol-contrib", "0.110.0", &[0x0a, 0xff]),
            b"opamp-fleet-package-v1\notelcol-contrib\n0.110.0\n0aff\n".to_vec()
        );
        assert_ne!(
            statement("a", "1", &[1]),
            statement("b", "1", &[1]),
            "the type is covered"
        );
        assert_ne!(
            statement("a", "1", &[1]),
            statement("a", "2", &[1]),
            "the version is covered"
        );
    }

    /// The floor statement is the prefix, the type and the version, one per line; the type and
    /// the version are covered.
    /// Verifies: ADR-0060
    #[test]
    fn the_floor_statement_is_the_prefix_the_type_and_the_version() {
        assert_eq!(
            floor_statement("otelcol-contrib", "0.110.0"),
            b"opamp-fleet-floor-v1\notelcol-contrib\n0.110.0\n".to_vec()
        );
        assert_ne!(
            floor_statement("a", "1.0.0"),
            floor_statement("b", "1.0.0"),
            "the type is covered"
        );
        assert_ne!(
            floor_statement("a", "1.0.0"),
            floor_statement("a", "2.0.0"),
            "the version is covered"
        );
    }

    /// One key signs both kinds of statement, and neither signature verifies as the other kind:
    /// a package signature is no floor, and a floor signature is no package.
    /// Verifies: ADR-0060
    #[test]
    fn a_package_signature_is_no_floor_and_a_floor_no_package() {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).expect("key");
        let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("keypair");
        let public = UnparsedPublicKey::new(&ED25519, keypair.public_key().as_ref().to_vec());
        let package = statement("otelcol", "2.0.0", &[0xab; 32]);
        let floor = floor_statement("otelcol", "2.0.0");
        let package_signature = keypair.sign(&package);
        let floor_signature = keypair.sign(&floor);

        assert!(public.verify(&package, package_signature.as_ref()).is_ok());
        assert!(public.verify(&floor, floor_signature.as_ref()).is_ok());
        assert!(
            public.verify(&floor, package_signature.as_ref()).is_err(),
            "a package signature does not verify as a floor"
        );
        assert!(
            public.verify(&package, floor_signature.as_ref()).is_err(),
            "a floor signature does not verify as a package"
        );
    }
}
