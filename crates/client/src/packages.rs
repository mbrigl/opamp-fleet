//! Server-offered packages (ADR-0015): resolving a download URL, downloading the artifact, and
//! **verifying it before it is applied** — the content hash always, the Ed25519 signature when the
//! operator configured a verification key. What protects an installed binary is verification, not
//! transport secrecy, so this is where the security of the feature lives.

use sha2::{Digest, Sha256};
use tracing::info;

use crate::config::ClientConfig;

/// One offered package the transport must download, verify, and hand to the Supervisor. Built by
/// the Agent state machine from a `PackageAvailable`; the raw fields it needs travel here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDownload {
    pub name: String,
    pub version: String,
    /// The package hash the reported status refers to.
    pub hash: Vec<u8>,
    /// The `download_url` from the offer — absolute, or a path resolved against the endpoint.
    pub download_url: String,
    /// The expected SHA-256 of the artifact.
    pub content_hash: Vec<u8>,
    /// The Ed25519 signature over the artifact; empty means unsigned.
    pub signature: Vec<u8>,
}

/// Resolves an offered `download_url` to an absolute URL. An absolute `http(s)://` URL is used as
/// given; a path (the Server's zero-config default) is resolved against the Client's own OpAMP
/// endpoint host, `ws(s)://` mapped to `http(s)://`.
pub fn resolve_url(download_url: &str, endpoint: &str) -> Result<String, String> {
    if download_url.starts_with("http://") || download_url.starts_with("https://") {
        return Ok(download_url.to_string());
    }
    let (scheme, rest) = endpoint
        .split_once("://")
        .ok_or_else(|| format!("cannot resolve {download_url} against endpoint {endpoint}"))?;
    let http_scheme = match scheme {
        "ws" | "http" => "http",
        "wss" | "https" => "https",
        other => return Err(format!("unexpected endpoint scheme {other}")),
    };
    let host_port = rest.split(['/', '?']).next().unwrap_or("");
    let path = if download_url.starts_with('/') {
        download_url.to_string()
    } else {
        format!("/{download_url}")
    };
    Ok(format!("{http_scheme}://{host_port}{path}"))
}

/// Downloads the artifact and verifies it (ADR-0015). Returns the verified bytes, ready to swap
/// over the Managed Process's binary.
pub async fn download_and_verify(
    package: &PackageDownload,
    config: &ClientConfig,
) -> Result<Vec<u8>, String> {
    let url = resolve_url(&package.download_url, &config.endpoint)?;
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(std::time::Duration::from_secs(120));
    if let Some(tls) = &config.tls {
        let pem = std::fs::read(&tls.ca_file)
            .map_err(|e| format!("cannot read {}: {e}", tls.ca_file.display()))?;
        let ca = reqwest::Certificate::from_pem(&pem)
            .map_err(|e| format!("cannot parse {}: {e}", tls.ca_file.display()))?;
        builder = builder
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca);
    }
    let client = builder
        .build()
        .map_err(|e| format!("cannot build the download client: {e}"))?;
    info!(package = %package.name, url = %url, "downloading package");
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("cannot download {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("cannot read the download: {e}"))?
        .to_vec();

    verify(
        &bytes,
        &package.content_hash,
        &package.signature,
        config.package_key(),
    )?;
    info!(package = %package.name, version = %package.version, "package verified");
    Ok(bytes)
}

/// Verifies an artifact against its content hash and, per the configured key, its signature.
///
/// Policy (ADR-0015): the content hash must always match. If a verification key is configured,
/// the artifact MUST carry a valid signature — an unsigned or badly signed artifact is refused. If
/// no key is configured, a signature that was nonetheless offered is refused (it cannot be
/// checked); an unsigned artifact is accepted on its content hash alone.
pub fn verify(
    bytes: &[u8],
    content_hash: &[u8],
    signature: &[u8],
    key: Option<&[u8]>,
) -> Result<(), String> {
    if Sha256::digest(bytes).as_slice() != content_hash {
        return Err("the downloaded artifact does not match its content hash".to_string());
    }
    match (key, signature.is_empty()) {
        (Some(key), false) => {
            ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
                .verify(bytes, signature)
                .map_err(|_| "the artifact's signature is invalid".to_string())
        }
        (Some(_), true) => {
            Err("a verification key is configured but the artifact is unsigned".to_string())
        }
        (None, false) => {
            Err("the artifact is signed but no verification key is configured".to_string())
        }
        (None, true) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    #[test]
    fn resolve_url_absolutizes_a_path_against_the_endpoint() {
        assert_eq!(
            resolve_url("/api/v1/packages/otelcol/file", "ws://host:4320/v1/opamp").unwrap(),
            "http://host:4320/api/v1/packages/otelcol/file"
        );
        assert_eq!(
            resolve_url("/api/v1/packages/x/file", "wss://host:4320/v1/opamp").unwrap(),
            "https://host:4320/api/v1/packages/x/file"
        );
        // An absolute URL is used as given.
        assert_eq!(
            resolve_url("https://cdn.example/x.bin", "ws://host/v1/opamp").unwrap(),
            "https://cdn.example/x.bin"
        );
    }

    #[test]
    fn content_hash_mismatch_is_refused() {
        let bytes = b"artifact";
        let wrong = Sha256::digest(b"other").to_vec();
        assert!(verify(bytes, &wrong, &[], None).is_err());
        let right = Sha256::digest(bytes).to_vec();
        assert!(verify(bytes, &right, &[], None).is_ok());
    }

    #[test]
    fn signature_policy_is_enforced() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("keypair");
        let public = keypair.public_key().as_ref().to_vec();

        let bytes = b"the-binary";
        let hash = Sha256::digest(bytes).to_vec();
        let signature = keypair.sign(bytes).as_ref().to_vec();

        // Valid signature against the configured key: accepted.
        assert!(verify(bytes, &hash, &signature, Some(&public)).is_ok());
        // Tampered signature: refused.
        let mut bad = signature.clone();
        bad[0] ^= 0xff;
        assert!(verify(bytes, &hash, &bad, Some(&public)).is_err());
        // Key configured but artifact unsigned: refused.
        assert!(verify(bytes, &hash, &[], Some(&public)).is_err());
        // Signed but no key to check it: refused.
        assert!(verify(bytes, &hash, &signature, None).is_err());
        // Unsigned and no key: accepted on the content hash alone.
        assert!(verify(bytes, &hash, &[], None).is_ok());
    }
}
