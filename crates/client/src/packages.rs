//! Server-offered packages (ADR-0015): resolving a download URL, downloading the artifact, and
//! **verifying it before it is applied** — the content hash always, the Ed25519 signature when the
//! operator configured a verification key. What protects an installed binary is verification, not
//! transport secrecy, so this is where the security of the feature lives.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use opamp::proto::PackageDownloadDetails;
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

/// Downloads the artifact to a file and verifies it (ADR-0015). Returns the path of the verified
/// artifact, ready for the Supervisor to swap over the Managed Process's binary.
///
/// The artifact is a program — tens or hundreds of megabytes — so it is streamed to
/// `<state_dir>/packages/` and hashed as it arrives, never assembled in memory. Only a signature
/// check reads it back, because Ed25519 verifies over the whole message.
pub async fn download_and_verify(
    package: &PackageDownload,
    config: &ClientConfig,
    progress: &Progress,
) -> Result<PathBuf, String> {
    let url = resolve_url(&package.download_url, &config.endpoint)?;
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        // Per-operation timeouts, not one for the whole transfer: a large artifact over a modest
        // link legitimately takes minutes, and a total timeout would abort it forever while a
        // stalled connection is what actually needs cutting.
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60));
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
    let mut response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("cannot download {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }

    let dir = config.state_dir.join("packages");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.staged", package.name));

    // Stream to disk, hashing on the way past: peak memory is one chunk, whatever the artifact
    // weighs. A failure anywhere leaves no half-written file behind for the next attempt to trip
    // over.
    let staged = match write_stream(&path, &mut response, progress).await {
        Ok(hash) => hash,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    if let Err(e) = verify_staged(&path, &staged, package, config.package_key()) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    info!(package = %package.name, version = %package.version, bytes = staged.len, "package verified");
    Ok(path)
}

/// What streaming the download to disk produced: its size and its content hash.
struct Staged {
    len: u64,
    content_hash: Vec<u8>,
}

/// How far an artifact download has got, shared with whoever reports it.
///
/// The download is a plain `await` inside the transport loop; this is how that loop learns the
/// progress of the future it is polling without the download having to know anything about
/// reporting. Cheap enough to update per chunk.
#[derive(Debug, Default)]
pub struct Progress {
    /// Bytes written so far.
    downloaded: AtomicU64,
    /// The artifact's total size, from `Content-Length`; `0` when the Server did not say.
    total: AtomicU64,
}

impl Progress {
    /// The Baseline's `PackageDownloadDetails` as of now: how far along, and how fast since
    /// `started`. Percent stays `0` while the total is unknown — a made-up percentage would be
    /// worse than none.
    pub fn details(&self, started: std::time::Instant) -> PackageDownloadDetails {
        let downloaded = self.downloaded.load(Ordering::Relaxed);
        let total = self.total.load(Ordering::Relaxed);
        let seconds = started.elapsed().as_secs_f64();
        PackageDownloadDetails {
            download_percent: if total > 0 {
                (downloaded as f64 / total as f64) * 100.0
            } else {
                0.0
            },
            download_bytes_per_second: if seconds > 0.0 {
                downloaded as f64 / seconds
            } else {
                0.0
            },
        }
    }
}

async fn write_stream(
    path: &Path,
    response: &mut reqwest::Response,
    progress: &Progress,
) -> Result<Staged, String> {
    use tokio::io::AsyncWriteExt;

    // What the Server advertises, so a percentage is possible at all.
    progress
        .total
        .store(response.content_length().unwrap_or(0), Ordering::Relaxed);
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut len = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("cannot read the download: {e}"))?
    {
        hasher.update(&chunk);
        len += chunk.len() as u64;
        progress.downloaded.store(len, Ordering::Relaxed);
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(Staged {
        len,
        content_hash: hasher.finalize().to_vec(),
    })
}

/// Verifies the streamed artifact: the content hash from the stream, and — only when the policy
/// demands a signature check — the file read back, since Ed25519 verifies over the whole message.
fn verify_staged(
    path: &Path,
    staged: &Staged,
    package: &PackageDownload,
    key: Option<&[u8]>,
) -> Result<(), String> {
    if staged.content_hash != package.content_hash {
        return Err("the downloaded artifact does not match its content hash".to_string());
    }
    if !signature_required(&package.signature, key)? {
        return Ok(());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    check_signature(&bytes, &package.signature, key.unwrap_or_default())
}

/// The signature half of the verification policy (ADR-0015): the content hash must always match.
/// If a verification key is configured, the artifact MUST carry a valid signature — an unsigned or
/// badly signed artifact is refused. If no key is configured, a signature that was nonetheless
/// offered is refused (it cannot be checked); an unsigned artifact is accepted on its content hash
/// alone.
///
/// Decided without touching the artifact, so the file is only read back when it must be: `Ok(true)`
/// means a
/// signature must now be checked, `Ok(false)` that the content hash was the whole of it, `Err`
/// that the pairing of key and signature is refused outright.
fn signature_required(signature: &[u8], key: Option<&[u8]>) -> Result<bool, String> {
    match (key, signature.is_empty()) {
        (Some(_), false) => Ok(true),
        (Some(_), true) => {
            Err("a verification key is configured but the artifact is unsigned".to_string())
        }
        (None, false) => {
            Err("the artifact is signed but no verification key is configured".to_string())
        }
        (None, true) => Ok(false),
    }
}

fn check_signature(bytes: &[u8], signature: &[u8], key: &[u8]) -> Result<(), String> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(bytes, signature)
        .map_err(|_| "the artifact's signature is invalid".to_string())
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

    /// A staged artifact, written where a real download would leave it.
    fn stage(dir: &tempfile::TempDir, bytes: &[u8]) -> (PathBuf, Staged) {
        let path = dir.path().join("otelcol.staged");
        std::fs::write(&path, bytes).expect("write");
        (
            path,
            Staged {
                len: bytes.len() as u64,
                content_hash: Sha256::digest(bytes).to_vec(),
            },
        )
    }

    fn offer(content_hash: Vec<u8>, signature: Vec<u8>) -> PackageDownload {
        PackageDownload {
            name: "otelcol".to_string(),
            version: "1.0.0".to_string(),
            hash: b"pkg".to_vec(),
            download_url: "/x".to_string(),
            content_hash,
            signature,
        }
    }

    #[test]
    fn content_hash_mismatch_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, staged) = stage(&dir, b"artifact");

        let wrong = offer(Sha256::digest(b"other").to_vec(), Vec::new());
        assert!(verify_staged(&path, &staged, &wrong, None).is_err());

        let right = offer(staged.content_hash.clone(), Vec::new());
        assert!(verify_staged(&path, &staged, &right, None).is_ok());
    }

    #[test]
    fn signature_policy_is_enforced() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("keypair");
        let public = keypair.public_key().as_ref().to_vec();

        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = b"the-binary";
        let (path, staged) = stage(&dir, bytes);
        let hash = staged.content_hash.clone();
        let signature = keypair.sign(bytes).as_ref().to_vec();

        let check = |signature: Vec<u8>, key: Option<&[u8]>| {
            verify_staged(&path, &staged, &offer(hash.clone(), signature), key)
        };
        // Valid signature against the configured key: accepted.
        assert!(check(signature.clone(), Some(&public)).is_ok());
        // Tampered signature: refused.
        let mut bad = signature.clone();
        bad[0] ^= 0xff;
        assert!(check(bad, Some(&public)).is_err());
        // Key configured but artifact unsigned: refused.
        assert!(check(Vec::new(), Some(&public)).is_err());
        // Signed but no key to check it: refused.
        assert!(check(signature, None).is_err());
        // Unsigned and no key: accepted on the content hash alone.
        assert!(check(Vec::new(), None).is_ok());
    }
}
