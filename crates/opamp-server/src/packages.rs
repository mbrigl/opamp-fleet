//! The packages the Server offers agents for installation (ADR-0018).
//!
//! Software distribution is the OpAMP `PackagesAvailable` flow: the Server offers a set of packages, an
//! agent downloads each file over HTTP, verifies it, installs it, and reports back. This module holds the
//! configured packages, computes the content and aggregate hashes the protocol turns on, builds the
//! `PackagesAvailable` offer, and serves each package's bytes.
//!
//! Scope of this increment (ADR-0018): a single **top-level** package (the collector binary), served from
//! a local file over the Server's own `:4321` surface, with **content-hash** integrity only —
//! cryptographic signatures are deferred to a follow-up ADR, so the `signature` field is left empty.

use std::io;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::proto::{
    DownloadableFile, Header, Headers, PackageAvailable, PackageType, PackagesAvailable,
};

/// One configured package: its identity, its file's bytes (read once at load), and the hashes the agent
/// uses to tell whether it already has this exact package.
struct Package {
    name: String,
    version: String,
    package_type: PackageType,
    /// The file bytes, held in memory so the download handler serves them without touching disk again.
    content: Vec<u8>,
    /// SHA-256 of the file content — the integrity check the agent runs before installing (ADR-0018).
    content_hash: Vec<u8>,
    /// The package hash: SHA-256 over the identity and the content hash. The agent compares this to the
    /// package it holds to decide whether the offer is a different version.
    package_hash: Vec<u8>,
}

/// The packages the Server offers, with the aggregate hash that drives the offer loop.
pub struct PackageSource {
    packages: Vec<Package>,
    /// The base URL agents download from, e.g. `http://dev:4321`. The offer's `download_url` is
    /// `{base}/packages/{name}`; it must be an address the agents can reach.
    base_url: String,
    /// The shared bearer token the download surface requires, attached to the offer's headers so the
    /// agent's download is authenticated (ADR-0012/0018); `None` when the surface is unauthenticated.
    auth_token: Option<String>,
    /// SHA-256 over the sorted package hashes — the aggregate the agent echoes back so the Server knows
    /// whether the fleet already has the offered set (ADR-0018).
    all_packages_hash: Vec<u8>,
}

/// One package to load: a name, version, type, and the file to read its bytes from.
pub struct PackageSpec {
    pub name: String,
    pub version: String,
    pub package_type: PackageType,
    pub path: PathBuf,
}

impl PackageSource {
    /// An empty source — the Server offers no packages (the default, ADR-0018).
    pub fn empty() -> Self {
        Self {
            packages: Vec::new(),
            base_url: String::new(),
            auth_token: None,
            all_packages_hash: Vec::new(),
        }
    }

    /// Loads the configured packages, reading each file and computing its hashes. Fails if a file cannot
    /// be read, so a mis-configured package is reported at startup rather than as a broken offer later.
    pub fn load(
        specs: Vec<PackageSpec>,
        base_url: String,
        auth_token: Option<String>,
    ) -> io::Result<Self> {
        let mut packages = Vec::new();
        for spec in specs {
            let content = std::fs::read(&spec.path)?;
            let content_hash = Sha256::digest(&content).to_vec();
            let package_hash =
                package_hash(&spec.name, &spec.version, spec.package_type, &content_hash);
            packages.push(Package {
                name: spec.name,
                version: spec.version,
                package_type: spec.package_type,
                content,
                content_hash,
                package_hash,
            });
        }
        // A stable order so the aggregate hash is deterministic regardless of configuration order.
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        let all_packages_hash = aggregate_hash(&packages);
        Ok(Self {
            packages,
            base_url,
            auth_token,
            all_packages_hash,
        })
    }

    /// Whether any package is configured; when empty the Server never offers packages.
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// The aggregate `all_packages_hash` — compared against what an agent reports to decide whether to
    /// send an offer.
    pub fn all_packages_hash(&self) -> &[u8] {
        &self.all_packages_hash
    }

    /// The bytes of the package with this name, for the download handler.
    pub fn file(&self, name: &str) -> Option<&[u8]> {
        self.packages
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.content.as_slice())
    }

    /// Builds the `PackagesAvailable` offer: every configured package, each with a `download_url` on the
    /// Server's own surface, its content hash, and (when the surface is authenticated) the `Authorization`
    /// header the download must present. The `signature` field is left empty — signatures are deferred
    /// (ADR-0018).
    pub fn offer(&self) -> PackagesAvailable {
        let headers = self.auth_token.as_ref().map(|token| Headers {
            headers: vec![Header {
                key: "Authorization".to_string(),
                value: format!("Bearer {token}"),
            }],
        });
        let packages = self
            .packages
            .iter()
            .map(|p| {
                let file = DownloadableFile {
                    download_url: format!("{}/packages/{}", self.base_url, p.name),
                    content_hash: p.content_hash.clone(),
                    signature: Vec::new(),
                    headers: headers.clone(),
                };
                (
                    p.name.clone(),
                    PackageAvailable {
                        r#type: p.package_type as i32,
                        version: p.version.clone(),
                        file: Some(file),
                        hash: p.package_hash.clone(),
                    },
                )
            })
            .collect();
        PackagesAvailable {
            packages,
            all_packages_hash: self.all_packages_hash.clone(),
        }
    }
}

/// The hash of one package: SHA-256 over its type, name, version, and the file's content hash — so a
/// change to any of them changes the package hash the agent compares against.
fn package_hash(
    name: &str,
    version: &str,
    package_type: PackageType,
    content_hash: &[u8],
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update((package_type as i32).to_le_bytes());
    hasher.update((name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
    hasher.update((version.len() as u64).to_le_bytes());
    hasher.update(version.as_bytes());
    hasher.update(content_hash);
    hasher.finalize().to_vec()
}

/// The aggregate hash: SHA-256 over the (name-sorted) package hashes, per the spec's aggregation.
fn aggregate_hash(packages: &[Package]) -> Vec<u8> {
    if packages.is_empty() {
        return Vec::new();
    }
    let mut hasher = Sha256::new();
    for p in packages {
        hasher.update(&p.package_hash);
    }
    hasher.finalize().to_vec()
}

/// Parses a `PackageType` from the CLI spelling `top_level` (the default) or `addon`.
pub fn parse_type(text: &str) -> Result<PackageType, String> {
    match text {
        "top_level" | "toplevel" => Ok(PackageType::TopLevel),
        "addon" => Ok(PackageType::Addon),
        other => Err(format!(
            "unknown package type {other:?} (expected top_level or addon)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A per-call unique subdirectory, so tests running in parallel never collide on a package file.
    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_file(name: &str, body: &[u8]) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("opamp-pkg-test-{}-{}", std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body).unwrap();
        path
    }

    fn source_with(body: &[u8]) -> PackageSource {
        let path = temp_file("otelcol", body);
        PackageSource::load(
            vec![PackageSpec {
                name: "otelcol".to_string(),
                version: "1.2.3".to_string(),
                package_type: PackageType::TopLevel,
                path,
            }],
            "http://dev:4321".to_string(),
            Some("s3cret".to_string()),
        )
        .unwrap()
    }

    #[test]
    fn empty_source_offers_nothing() {
        let src = PackageSource::empty();
        assert!(src.is_empty());
        assert!(src.all_packages_hash().is_empty());
    }

    #[test]
    fn loads_computes_hashes_and_serves_the_file() {
        let src = source_with(b"BINARY-BYTES");
        assert!(!src.is_empty());
        assert_eq!(src.file("otelcol"), Some(&b"BINARY-BYTES"[..]));
        assert!(src.file("missing").is_none());
        assert!(!src.all_packages_hash().is_empty());
    }

    #[test]
    fn offer_carries_url_content_hash_and_auth_header() {
        let src = source_with(b"BINARY-BYTES");
        let offer = src.offer();
        assert_eq!(offer.all_packages_hash, src.all_packages_hash());
        let pkg = &offer.packages["otelcol"];
        assert_eq!(pkg.r#type, PackageType::TopLevel as i32);
        assert_eq!(pkg.version, "1.2.3");
        let file = pkg.file.as_ref().unwrap();
        assert_eq!(file.download_url, "http://dev:4321/packages/otelcol");
        assert_eq!(file.content_hash, Sha256::digest(b"BINARY-BYTES").to_vec());
        // Signatures are deferred (ADR-0018) — the field is empty.
        assert!(file.signature.is_empty());
        // The download carries the shared token so it passes the authenticated surface.
        let headers = file.headers.as_ref().unwrap();
        assert_eq!(headers.headers[0].key, "Authorization");
        assert_eq!(headers.headers[0].value, "Bearer s3cret");
    }

    #[test]
    fn changing_the_file_changes_the_aggregate_hash() {
        let a = source_with(b"VERSION-A");
        let b = source_with(b"VERSION-B");
        assert_ne!(a.all_packages_hash(), b.all_packages_hash());
    }

    #[test]
    fn no_auth_token_means_no_offer_headers() {
        let path = temp_file("otelcol", b"x");
        let src = PackageSource::load(
            vec![PackageSpec {
                name: "otelcol".to_string(),
                version: "1".to_string(),
                package_type: PackageType::TopLevel,
                path,
            }],
            "http://dev:4321".to_string(),
            None,
        )
        .unwrap();
        assert!(src.offer().packages["otelcol"]
            .file
            .as_ref()
            .unwrap()
            .headers
            .is_none());
    }

    #[test]
    fn parse_type_reads_the_cli_spellings() {
        assert_eq!(parse_type("top_level").unwrap(), PackageType::TopLevel);
        assert_eq!(parse_type("addon").unwrap(), PackageType::Addon);
        assert!(parse_type("nonsense").is_err());
    }
}
