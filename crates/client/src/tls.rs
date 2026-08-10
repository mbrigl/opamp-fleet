//! TLS for the Client's two transports (ADR-0007, ADR-0035): rustls everywhere, an optional CA
//! file that *replaces* the built-in roots for self-signed deployments, and the optional client
//! certificate a Server demanding mutual TLS asks for.
//!
//! Both transports are served from here rather than each building its own: `wss://` takes a rustls
//! `ClientConfig`, `https://` takes a reqwest identity, and the two must be built from the same
//! resolution of "which identity is in force" — the one the Server issued, else the one the
//! operator configured.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::config::ClientConfig;

/// The Server-issued client certificate, in the state directory beside the connection settings —
/// it belongs to the Client's one upstream connection, not to any single Agent (ADR-0035).
pub const ISSUED_CERT_FILE: &str = "client-cert.pem";
/// The private key of [`ISSUED_CERT_FILE`]. Generated on this host and never sent anywhere: what
/// leaves is a CSR over its public half.
pub const ISSUED_KEY_FILE: &str = "client-key.pem";

/// The rustls configuration the WebSocket transport connects with, or `None` when nothing about
/// TLS is configured and the transport's own defaults (webpki roots, no client certificate) are
/// exactly right.
pub fn rustls_client_config(
    config: &ClientConfig,
) -> Result<Option<Arc<rustls::ClientConfig>>, String> {
    rustls_client_config_for(config, None)
}

/// The same, for a **candidate** identity: an offered certificate is proved by connecting with it
/// before it is stored (ADR-0014's MUST, applied to the certificate in ADR-0035), so the
/// certificate under test comes from the offer while its key is the pending one on disk.
pub fn rustls_client_config_for(
    config: &ClientConfig,
    candidate_cert: Option<&[u8]>,
) -> Result<Option<Arc<rustls::ClientConfig>>, String> {
    let ca_file = config.ca_file();
    let identity = candidate_identity(config, candidate_cert)?;
    if ca_file.is_none() && identity.is_none() {
        return Ok(None);
    }

    let roots = match ca_file {
        Some(ca_file) => root_store(ca_file)?,
        // A configured identity does not imply a private CA: presenting a client certificate to a
        // Server with a publicly trusted one is an ordinary deployment.
        None => rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        },
    };
    let builder = rustls::ClientConfig::builder().with_root_certificates(roots);
    let config = match identity {
        None => builder.with_no_client_auth(),
        Some((cert_pem, key_file)) => builder
            .with_client_auth_cert(opamp::pem::certificates(&cert_pem)?, read_key(&key_file)?)
            .map_err(|e| format!("cannot present the client certificate: {e}"))?,
    };
    Ok(Some(Arc::new(config)))
}

/// The certificate PEM and key path in force: the candidate when one is under test, else whatever
/// [`ClientConfig::client_identity`] resolves to.
fn candidate_identity(
    config: &ClientConfig,
    candidate_cert: Option<&[u8]>,
) -> Result<Option<(Vec<u8>, std::path::PathBuf)>, String> {
    if let Some(cert) = candidate_cert {
        // An offered certificate belongs to the key this Client generated for its request; without
        // that key there is nothing to prove possession with, and the offer cannot be honoured.
        let key = config.state_dir.join(ISSUED_KEY_FILE);
        if !key.exists() {
            return Err(format!(
                "an offered certificate has no key to go with it — {} is missing",
                key.display()
            ));
        }
        return Ok(Some((cert.to_vec(), key)));
    }
    let Some((cert_file, key_file)) = config.client_identity() else {
        return Ok(None);
    };
    let cert = std::fs::read(&cert_file)
        .map_err(|e| format!("cannot read {}: {e}", cert_file.display()))?;
    Ok(Some((cert, key_file)))
}

/// A rustls configuration trusting exactly the given PEM bundle, with no client certificate — the
/// probe path, which verifies a *candidate* endpoint before it is adopted (ADR-0014).
pub fn rustls_config_with_ca(ca_file: &Path) -> Result<Arc<rustls::ClientConfig>, String> {
    Ok(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(root_store(ca_file)?)
            .with_no_client_auth(),
    ))
}

/// The client identity for the plain-HTTP transport, in reqwest's shape, or `None` when there is
/// none to present.
///
/// reqwest's rustls backend takes key and certificate as **one** PEM buffer — `from_pkcs8_pem` is
/// the native-tls constructor and does not exist here — so the two files are concatenated rather
/// than passed separately. The key comes first, as its own documented example does.
pub fn reqwest_identity(config: &ClientConfig) -> Result<Option<reqwest::Identity>, String> {
    reqwest_identity_for(config, None)
}

fn reqwest_identity_for(
    config: &ClientConfig,
    candidate_cert: Option<&[u8]>,
) -> Result<Option<reqwest::Identity>, String> {
    let Some((cert_pem, key_file)) = candidate_identity(config, candidate_cert)? else {
        return Ok(None);
    };
    let mut pem =
        std::fs::read(&key_file).map_err(|e| format!("cannot read {}: {e}", key_file.display()))?;
    if !pem.ends_with(b"\n") {
        pem.push(b'\n');
    }
    pem.extend_from_slice(&cert_pem);
    reqwest::Identity::from_pem(&pem)
        .map(Some)
        .map_err(|e| format!("cannot present the client certificate: {e}"))
}

/// Applies the configured CA to a reqwest builder — the trust half, which every outbound HTTPS of
/// this Client needs.
pub fn trust(
    builder: reqwest::ClientBuilder,
    config: &ClientConfig,
) -> Result<reqwest::ClientBuilder, String> {
    let Some(ca_file) = config.ca_file() else {
        return Ok(builder);
    };
    let pem =
        std::fs::read(ca_file).map_err(|e| format!("cannot read {}: {e}", ca_file.display()))?;
    let ca = reqwest::Certificate::from_pem(&pem)
        .map_err(|e| format!("cannot parse {}: {e}", ca_file.display()))?;
    Ok(builder
        .tls_built_in_root_certs(false)
        .add_root_certificate(ca))
}

/// Applies both halves — trust and this Client's own identity — to a reqwest builder: what the
/// plain-HTTP transport talks to the Server with.
///
/// Package downloads deliberately use [`trust`] alone: a `download_url` may point at a mirror this
/// project knows nothing about (ADR-0018), and an identity is for the Server, not for whoever
/// happens to host an artifact.
pub fn trust_and_identity(
    builder: reqwest::ClientBuilder,
    config: &ClientConfig,
) -> Result<reqwest::ClientBuilder, String> {
    trust_and_identity_for(builder, config, None)
}

/// The same, with a candidate certificate under test (ADR-0035).
pub fn trust_and_identity_for(
    builder: reqwest::ClientBuilder,
    config: &ClientConfig,
    candidate_cert: Option<&[u8]>,
) -> Result<reqwest::ClientBuilder, String> {
    let builder = trust(builder, config)?;
    match reqwest_identity_for(config, candidate_cert)? {
        Some(identity) => Ok(builder.identity(identity)),
        None => Ok(builder),
    }
}

fn root_store(ca_file: &Path) -> Result<rustls::RootCertStore, String> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in read_certs(ca_file)? {
        roots
            .add(cert)
            .map_err(|e| format!("cannot trust a certificate from {}: {e}", ca_file.display()))?;
    }
    Ok(roots)
}

fn read_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let pem = std::fs::read(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    opamp::pem::certificates(&pem).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_key(path: &Path) -> Result<PrivateKeyDer<'static>, String> {
    let pem = std::fs::read(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    opamp::pem::private_key(&pem).map_err(|_| format!("{} contains no private key", path.display()))
}
