//! Support shared by the integration tests that start the Client binary.

use std::path::Path;

/// Writes a throwaway self-signed client certificate and key into `dir` and returns the
/// `[auth]` and `[tls]` tables that name them, to be appended at the end of a `supervisor.toml`.
///
/// The Client refuses to start without the fleet credential and a client certificate (ADR-0039).
/// The test Servers serve plaintext with an open admission and check neither, so the files only
/// have to exist and parse.
pub fn credentials(dir: &Path) -> String {
    let key = rcgen::KeyPair::generate().expect("generate a key");
    let cert = rcgen::CertificateParams::new(vec!["test-agent".into()])
        .expect("certificate parameters")
        .self_signed(&key)
        .expect("self-sign the certificate");
    let cert_file = dir.join("test-client.pem");
    let key_file = dir.join("test-client-key.pem");
    std::fs::write(&cert_file, cert.pem()).expect("write the client certificate");
    std::fs::write(&key_file, key.serialize_pem()).expect("write the client key");
    format!(
        "\n[auth]\nbearer_token = \"test-fleet-token\"\n\n[tls]\ncert_file = {:?}\nkey_file = {:?}\n",
        cert_file.display().to_string(),
        key_file.display().to_string(),
    )
}
