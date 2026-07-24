
use std::path::Path;
use std::sync::Arc;

            .with_client_auth_cert(opamp::pem::certificates(&cert_pem)?, read_key(&key_file)?)
pub fn rustls_config_with_ca(ca_file: &Path) -> Result<Arc<rustls::ClientConfig>, String> {
    Ok(Arc::new(
        rustls::ClientConfig::builder()
    let mut roots = rustls::RootCertStore::empty();
        roots
            .add(cert)
            .map_err(|e| format!("cannot trust a certificate from {}: {e}", ca_file.display()))?;
    }
    opamp::pem::certificates(&pem).map_err(|e| format!("{}: {e}", path.display()))
    opamp::pem::private_key(&pem).map_err(|_| format!("{} contains no private key", path.display()))
}
