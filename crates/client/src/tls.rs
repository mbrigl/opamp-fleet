//! TLS trust for the Client's two transports (ADR-0007): rustls everywhere, and an optional CA
//! file that *replaces* the built-in roots for self-signed deployments.

use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

/// A rustls client configuration trusting exactly the given PEM bundle — the WebSocket transport's
/// connector for `wss://` against a private CA.
pub fn rustls_config_with_ca(ca_file: &Path) -> Result<Arc<rustls::ClientConfig>, String> {
    let file = std::fs::File::open(ca_file)
        .map_err(|e| format!("cannot open {}: {e}", ca_file.display()))?;
    let mut roots = rustls::RootCertStore::empty();
    let mut added = 0;
    for cert in rustls_pemfile::certs(&mut BufReader::new(file)) {
        let cert = cert.map_err(|e| format!("cannot parse {}: {e}", ca_file.display()))?;
        roots
            .add(cert)
            .map_err(|e| format!("cannot trust a certificate from {}: {e}", ca_file.display()))?;
        added += 1;
    }
    if added == 0 {
        return Err(format!("{} contains no certificates", ca_file.display()));
    }
    Ok(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}
