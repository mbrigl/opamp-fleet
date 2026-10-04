//! Which TLS material the Client uses (ADR-0023, ADR-0026), read from disk and handed to `opamp` as
//! [`ClientTls`] (ADR-0024).
//!
//! The decisions are the Client's: an optional CA file that *replaces* the built-in roots, and the
//! identity in force — the one the Server issued, else the one the operator configured. Building a
//! rustls configuration or an HTTP client from that material is `opamp`'s.

use std::path::Path;

use opamp::client::ClientTls;
use opamp::tls::Identity;

use crate::config::ClientConfig;

/// The Server-issued client certificate, in the state directory beside the connection settings —
/// it belongs to the Client's one upstream connection, not to any single Agent (ADR-0026).
pub const ISSUED_CERT_FILE: &str = "client-cert.pem";
/// The private key of [`ISSUED_CERT_FILE`]. Generated on this host and never sent anywhere: what
/// leaves is a CSR over its public half.
pub const ISSUED_KEY_FILE: &str = "client-key.pem";
/// The key a request in flight asks to be certified, kept apart from [`ISSUED_KEY_FILE`] so the
/// certificate in force keeps its own key until the new one is proved (ADR-0026 clause 11).
pub const PENDING_KEY_FILE: &str = "client-key.pending.pem";
/// The request in flight, re-sent unchanged until it is answered — the enrolment queue knows a
/// request by its public key (ADR-0026 clause 21).
pub const PENDING_CSR_FILE: &str = "client-csr.pending.pem";

/// The trust and the identity in force.
///
/// # Errors
/// Returns an error naming the file that cannot be read.
pub fn client_tls(config: &ClientConfig) -> Result<ClientTls, String> {
    client_tls_for(config, None)
}

/// The same, for a **candidate** identity: an offered certificate is proved by connecting with it
/// before it is stored (ADR-0027's MUST, applied to the certificate in ADR-0026), so the
/// certificate under test comes from the offer while its key is the pending one on disk.
///
/// # Errors
/// Returns an error naming the file that cannot be read, or the missing key of a candidate.
pub fn client_tls_for(
    config: &ClientConfig,
    candidate_cert: Option<&[u8]>,
) -> Result<ClientTls, String> {
    let ca_pem = config.ca_file().map(certificates_file).transpose()?;
    let identity = match candidate_cert {
        Some(cert) => {
            // An offered certificate belongs to the key this Client generated for its request;
            // without that key there is nothing to prove possession with.
            // The key of the request in flight, or — for a certificate offered over a key already in
            // force — that one.
            let pending = config.state_dir.join(PENDING_KEY_FILE);
            let key = if pending.exists() {
                pending
            } else {
                config.state_dir.join(ISSUED_KEY_FILE)
            };
            if !key.exists() {
                return Err(format!(
                    "an offered certificate has no key to go with it — {} is missing",
                    key.display()
                ));
            }
            Some(Identity {
                cert_pem: cert.to_vec(),
                key_pem: key_file(&key)?,
            })
        }
        None => match config.client_identity() {
            Some((cert, key)) => Some(Identity {
                cert_pem: certificates_file(&cert)?,
                key_pem: key_file(&key)?,
            }),
            None => None,
        },
    };
    Ok(ClientTls { ca_pem, identity })
}

/// The trust anchors alone — what a download from a host that is not the Server needs. Reads no
/// identity, so a client certificate that cannot be read does not stop a download.
///
/// # Errors
/// Returns an error naming the CA file that cannot be read or holds no certificate.
pub fn trust(config: &ClientConfig) -> Result<ClientTls, String> {
    Ok(ClientTls {
        ca_pem: config.ca_file().map(certificates_file).transpose()?,
        identity: None,
    })
}

/// A PEM file of certificates, parsed here so that a bad one is named by its path.
pub(crate) fn certificates_file(path: &Path) -> Result<Vec<u8>, String> {
    let pem = read(path)?;
    opamp::tls::certificates(&pem).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(pem)
}

/// A PEM file holding a private key, parsed here so that a bad one is named by its path.
pub(crate) fn key_file(path: &Path) -> Result<Vec<u8>, String> {
    let pem = read(path)?;
    opamp::tls::private_key(&pem)
        .map_err(|_| format!("{} contains no private key", path.display()))?;
    Ok(pem)
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}
