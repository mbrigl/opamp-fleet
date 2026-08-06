//! The package store (ADR-0015): the Server's software artifacts, mirroring the Configuration
//! store (ADR-0012). Each package is a binary artifact plus its metadata — version, type, the
//! SHA-256 content hash, an optional Ed25519 signature — persisted so a Server restart keeps
//! offering what the fleet should run.
//!
//! Package *bodies* are opaque bytes: what a package contains and how it is applied is the Agent's
//! business (the specification forbids the Server abstracting over it). The Server's job is to
//! store, hash, offer, and serve.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use opamp::proto::{
    AgentDescription, DownloadableFile, Header, Headers, PackageAvailable, PackageType,
    PackagesAvailable,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::configs::{matches, validate_name};

/// A stored package: its metadata. The artifact itself stays on disk (`<name>.bin`) and is
/// streamed to whoever asks — a program weighs hundreds of megabytes, and a fleet server holding
/// every one of them in memory, plus a copy per download, is the shape this deliberately avoids.
/// The name is a map key on the wire and a file name here, so it follows the ADR-0010 grammar.
#[derive(Clone)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// `false` is the Baseline's `TopLevel` (a Managed Process's binary), `true` an `Addon`.
    pub addon: bool,
    /// SHA-256 of the artifact bytes.
    pub content_hash: Vec<u8>,
    /// Optional Ed25519 signature over the artifact, supplied by the operator.
    pub signature: Option<Vec<u8>>,
    /// The artifact's size in bytes, for the fleet view and the logs.
    pub size: u64,
    /// The Selector (ADR-0012 semantics, ADR-0017): equality pairs that must all match an
    /// attribute the Agent reported. **Empty matches every Agent** — which is what every package
    /// stored before Selectors existed has, so it keeps behaving as it did.
    pub selector: BTreeMap<String, String>,
    /// Where the artifact lives when it is **not** here (ADR-0018). `None` is an uploaded package,
    /// whose bytes this Server holds and serves; `Some` is a reference, offered to Agents as the
    /// address it names — the Server never downloads it and has nothing to serve.
    pub source: Option<Source>,
    /// What this package was before it was last replaced (ADR-0019), or `None` for one that has
    /// never been replaced — the state of every package at its first upload. Exactly one step is
    /// remembered: a rollback swaps it with the current version, so pressing the button twice
    /// returns to where it started.
    pub previous: Option<Version>,
}

/// One version of a package: everything needed to offer it, which is everything except the name
/// and the Selector — those belong to the package, not to the bytes it currently carries. Used to
/// remember the version a package replaced (ADR-0019).
#[derive(Clone)]
pub struct Version {
    pub version: String,
    /// `false` is the Baseline's `TopLevel`, `true` an `Addon`.
    pub addon: bool,
    pub content_hash: Vec<u8>,
    pub signature: Option<Vec<u8>>,
    /// Zero for a referenced version, whose bytes this Server never holds.
    pub size: u64,
    pub source: Option<Source>,
}

/// An artifact that lives somewhere else (ADR-0018): the address Agents fetch it from, and what
/// they must send to be allowed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub url: String,
    /// Sent with the download — a token for a private source. It reaches every Agent the package
    /// targets, which is the exposure the operator accepts by using one.
    pub headers: BTreeMap<String, String>,
}

impl Package {
    /// This package as persisted. The one place the on-disk shape is written, so a field added to
    /// a package cannot be forgotten in one of the several ways a package is stored.
    fn meta(&self) -> PackageMeta {
        PackageMeta {
            name: self.name.clone(),
            version: self.version.clone(),
            addon: self.addon,
            content_hash_hex: hex::encode(&self.content_hash),
            signature_hex: self.signature.as_ref().map(hex::encode),
            selector: self.selector.clone(),
            source_url: self.source.as_ref().map(|s| s.url.clone()),
            source_headers: self
                .source
                .as_ref()
                .map(|s| s.headers.clone())
                .unwrap_or_default(),
            previous: self.previous.as_ref().map(VersionMeta::of),
        }
    }

    /// What this package currently is, as the descriptor another version can remember it by.
    fn current(&self) -> Version {
        Version {
            version: self.version.clone(),
            addon: self.addon,
            content_hash: self.content_hash.clone(),
            signature: self.signature.clone(),
            size: self.size,
            source: self.source.clone(),
        }
    }

    /// The per-package hash the Agent compares to decide whether to download: over the fields that
    /// identify the offer (type, version) and the content. Framed length-prefixed so no boundary
    /// is ambiguous.
    fn package_hash(&self) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update([u8::from(self.addon)]);
        hasher.update((self.version.len() as u64).to_le_bytes());
        hasher.update(self.version.as_bytes());
        hasher.update(&self.content_hash);
        hasher.finalize().to_vec()
    }

    /// This package as a wire `PackageAvailable`.
    ///
    /// An uploaded package is offered from this Server: `download_base` prefixes the artifact
    /// endpoint, and an empty prefix yields a path the Agent resolves against its own OpAMP
    /// endpoint. A **referenced** package is offered as the address it names, with whatever headers
    /// the operator gave — the Baseline's Download Server "may be on the same host as the OpAMP
    /// Server or a different host", and this is that other host (ADR-0018).
    fn to_available(&self, download_base: &str, headers: Option<Headers>) -> PackageAvailable {
        let file = match &self.source {
            Some(source) => DownloadableFile {
                download_url: source.url.clone(),
                content_hash: self.content_hash.clone(),
                signature: self.signature.clone().unwrap_or_default(),
                // The Server's own credential has no business at someone else's address; what
                // travels is what the operator said that source needs.
                headers: (!source.headers.is_empty()).then(|| Headers {
                    headers: source
                        .headers
                        .iter()
                        .map(|(key, value)| Header {
                            key: key.clone(),
                            value: value.clone(),
                        })
                        .collect(),
                }),
            },
            None => DownloadableFile {
                download_url: format!("{download_base}/api/v1/packages/{}/file", self.name),
                content_hash: self.content_hash.clone(),
                signature: self.signature.clone().unwrap_or_default(),
                headers,
            },
        };
        PackageAvailable {
            r#type: if self.addon {
                PackageType::Addon as i32
            } else {
                PackageType::TopLevel as i32
            },
            version: self.version.clone(),
            file: Some(file),
            hash: self.package_hash(),
        }
    }
}

/// One package as the REST API lists it (ADR-0017): what it is and whom it targets.
pub struct PackageSummary {
    pub name: String,
    pub version: String,
    pub addon: bool,
    pub selector: BTreeMap<String, String>,
    /// The address an Agent fetches this from when the Server does not hold it (ADR-0018).
    pub source_url: Option<String>,
    /// The version a rollback would put back (ADR-0019), and — for a referenced one — where it
    /// comes from. Absent when there is nothing to go back to, which is what makes the rollback
    /// unavailable rather than a surprise: an operator sees `0.157.0 ← 0.156.0` before choosing.
    pub previous_version: Option<String>,
    pub previous_source_url: Option<String>,
}

impl PackageSummary {
    fn of(package: &Package) -> Self {
        PackageSummary {
            name: package.name.clone(),
            version: package.version.clone(),
            addon: package.addon,
            selector: package.selector.clone(),
            source_url: package.source.as_ref().map(|s| s.url.clone()),
            previous_version: package.previous.as_ref().map(|v| v.version.clone()),
            previous_source_url: package
                .previous
                .as_ref()
                .and_then(|v| v.source.as_ref().map(|s| s.url.clone())),
        }
    }
}

/// Metadata as persisted next to the artifact (`<name>.json`); the artifact is `<name>.bin` and
/// the version it replaced, when that one was uploaded too, `<name>.previous.bin`.
#[derive(Serialize, Deserialize)]
struct PackageMeta {
    name: String,
    version: String,
    #[serde(default)]
    addon: bool,
    content_hash_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature_hex: Option<String>,
    /// The Selector (ADR-0017). Absent in a file written before Selectors existed, which reads as
    /// empty — the whole fleet, exactly what that package did.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    selector: BTreeMap<String, String>,
    /// The source of a referenced package (ADR-0018); absent for an uploaded one, which is what
    /// every package written before this existed is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    source_headers: BTreeMap<String, String>,
    /// The version this package replaced (ADR-0019). Absent for one that never replaced anything,
    /// and in every file written before that decision — which reads as "nothing to go back to",
    /// exactly the truth for a package whose predecessor was never kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<VersionMeta>,
}

/// One remembered version, as persisted.
#[derive(Serialize, Deserialize)]
struct VersionMeta {
    version: String,
    #[serde(default)]
    addon: bool,
    content_hash_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    source_headers: BTreeMap<String, String>,
}

impl VersionMeta {
    fn of(version: &Version) -> Self {
        VersionMeta {
            version: version.version.clone(),
            addon: version.addon,
            content_hash_hex: hex::encode(&version.content_hash),
            signature_hex: version.signature.as_ref().map(hex::encode),
            source_url: version.source.as_ref().map(|s| s.url.clone()),
            source_headers: version
                .source
                .as_ref()
                .map(|s| s.headers.clone())
                .unwrap_or_default(),
        }
    }

    /// The stored form back into a [`Version`]. `size` is recovered by the caller, which is the
    /// only party that can measure the artifact on disk.
    fn into_version(self, size: u64) -> Result<Version, String> {
        Ok(Version {
            content_hash: hex::decode(&self.content_hash_hex)
                .map_err(|e| format!("invalid content hash for the previous version: {e}"))?,
            signature: match &self.signature_hex {
                Some(hex) => Some(
                    hex::decode(hex)
                        .map_err(|e| format!("invalid signature for the previous version: {e}"))?,
                ),
                None => None,
            },
            source: self.source_url.map(|url| Source {
                url,
                headers: self.source_headers,
            }),
            version: self.version,
            addon: self.addon,
            size,
        })
    }
}

/// The persistent package store: one `<name>.json` + `<name>.bin` pair per package under
/// `packages_dir`, restored at startup. The in-memory map is what the control loop reads.
pub struct PackageStore {
    dir: PathBuf,
    packages: RwLock<BTreeMap<String, Package>>,
}

impl PackageStore {
    /// Opens the store, creating the directory and loading every persisted package. A metadata or
    /// artifact file that cannot be read, does not parse, or whose artifact no longer matches its
    /// recorded hash is a startup error — a corrupt distribution artifact must never ship.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let mut packages = BTreeMap::new();
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let meta: PackageMeta = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            validate_name(&meta.name)
                .map_err(|e| format!("invalid package name in {}: {e}", path.display()))?;
            let content_hash = hex::decode(&meta.content_hash_hex)
                .map_err(|e| format!("invalid content hash in {}: {e}", path.display()))?;
            let source = meta.source_url.clone().map(|url| Source {
                url,
                headers: meta.source_headers.clone(),
            });
            // An uploaded artifact is re-hashed by streaming, so a corrupt one never ships and the
            // check never depends on its size. A referenced one has nothing here to check: its
            // hash is the operator's word, verified by every Agent that downloads it (ADR-0018).
            let size = match &source {
                Some(_) => 0,
                None => {
                    let artifact_path = dir.join(format!("{}.bin", meta.name));
                    let (size, actual) = hash_file(&artifact_path)?;
                    if actual != content_hash {
                        return Err(format!(
                            "package {:?}: artifact does not match its recorded content hash",
                            meta.name
                        ));
                    }
                    size
                }
            };
            let signature = match &meta.signature_hex {
                Some(hex) => Some(
                    hex::decode(hex)
                        .map_err(|e| format!("invalid signature in {}: {e}", path.display()))?,
                ),
                None => None,
            };
            // The remembered version (ADR-0019) is restored the same way, and its artifact — when
            // it has one here — is re-hashed just like the current one: a rollback ships it, so a
            // corrupt one must not survive startup any more than a corrupt current artifact does.
            let previous = match meta.previous {
                Some(previous) => {
                    let size = match previous.source_url {
                        Some(_) => 0,
                        None => {
                            let kept = dir.join(format!("{}.previous.bin", meta.name));
                            let (size, actual) = hash_file(&kept)?;
                            let expected =
                                hex::decode(&previous.content_hash_hex).map_err(|e| {
                                    format!(
                                        "invalid previous content hash in {}: {e}",
                                        path.display()
                                    )
                                })?;
                            if actual != expected {
                                return Err(format!(
                                    "package {:?}: the kept previous artifact does not match its \
                                     recorded content hash",
                                    meta.name
                                ));
                            }
                            size
                        }
                    };
                    Some(
                        previous
                            .into_version(size)
                            .map_err(|e| format!("{}: {e}", path.display()))?,
                    )
                }
                None => None,
            };
            packages.insert(
                meta.name.clone(),
                Package {
                    name: meta.name,
                    version: meta.version,
                    addon: meta.addon,
                    content_hash,
                    signature,
                    size,
                    selector: meta.selector,
                    source,
                    previous,
                },
            );
        }
        Ok(PackageStore {
            dir,
            packages: RwLock::new(packages),
        })
    }

    /// Every package's name, version, addon flag, and Selector, in name order — the REST list
    /// view; never the artifact bytes.
    pub fn list(&self) -> Vec<PackageSummary> {
        self.packages
            .read()
            .expect("packages lock")
            .values()
            .map(PackageSummary::of)
            .collect()
    }

    /// Where a package's artifact lives, for the download endpoint to stream from. `None` when no
    /// package of that name is stored.
    pub fn artifact_path(&self, name: &str) -> Option<PathBuf> {
        self.packages
            .read()
            .expect("packages lock")
            .get(name)
            // A referenced package is not served from here; the Agents were given its address.
            .filter(|p| p.source.is_none())
            .map(|p| self.dir.join(format!("{}.bin", p.name)))
    }

    /// `true` when the store holds no package — the Server then leaves `OffersPackages` undeclared.
    pub fn is_empty(&self) -> bool {
        self.packages.read().expect("packages lock").is_empty()
    }

    /// Where an upload is streamed to before it becomes a package. In the store's own directory,
    /// so [`put_staged`](Self::put_staged) can move it into place with a rename.
    ///
    /// # Errors
    /// Returns an error when the name is not a valid package name.
    pub fn staging_path(&self, name: &str) -> Result<PathBuf, String> {
        validate_name(name).map_err(|e| format!("invalid name {name:?}: {e}"))?;
        Ok(self.dir.join(format!("{name}.upload")))
    }

    /// Turns a streamed upload into a package: hashed by streaming, moved into place with a
    /// rename, then visible to the control loop. The artifact never passes through memory — an
    /// agent binary is far too big to buffer twice just to store it once.
    ///
    /// The staged file is consumed on success and removed on failure, so a rejected upload leaves
    /// nothing behind.
    pub fn put_staged(
        &self,
        name: String,
        version: String,
        addon: bool,
        signature: Option<Vec<u8>>,
        staged: &Path,
    ) -> Result<(), String> {
        let result = self.store_staged(&name, &version, addon, signature, staged);
        if result.is_err() {
            let _ = std::fs::remove_file(staged);
        }
        result
    }

    fn store_staged(
        &self,
        name: &str,
        version: &str,
        addon: bool,
        signature: Option<Vec<u8>>,
        staged: &Path,
    ) -> Result<(), String> {
        validate_name(name).map_err(|e| format!("invalid name {name:?}: {e}"))?;
        let (size, content_hash) = hash_file(staged)?;
        if size == 0 {
            return Err("the package artifact is empty; refusing to distribute it".to_string());
        }
        self.replace(
            name,
            version,
            addon,
            content_hash,
            signature,
            size,
            None,
            || {
                let artifact = self.dir.join(format!("{name}.bin"));
                std::fs::rename(staged, &artifact)
                    .map_err(|e| format!("cannot persist {}: {e}", artifact.display()))
            },
        )
    }

    /// The one path by which a package's bytes are replaced — an upload, a staged upload, or a
    /// source (ADR-0018). It is what keeps the two invariants that hold across all three:
    ///
    /// - **The Selector survives.** Replacing the bytes must never silently widen a targeted
    ///   rollout to the whole fleet (ADR-0017).
    /// - **The version it replaced is remembered** (ADR-0019), including its artifact when that one
    ///   was uploaded — so an operator can go one step back without producing the old file again.
    ///
    /// `install` puts the new artifact in place, once the one it displaces has been set aside.
    #[allow(clippy::too_many_arguments)]
    fn replace(
        &self,
        name: &str,
        version: &str,
        addon: bool,
        content_hash: Vec<u8>,
        signature: Option<Vec<u8>>,
        size: u64,
        source: Option<Source>,
        install: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let existing = self
            .packages
            .read()
            .expect("packages lock")
            .get(name)
            .cloned();
        let selector = existing
            .as_ref()
            .map(|p| p.selector.clone())
            .unwrap_or_default();
        let previous = existing.as_ref().map(Package::current);

        // Set the displaced artifact aside before the new one lands on it. A referenced version
        // has no artifact here, and the file it would displace is simply gone.
        let artifact = self.dir.join(format!("{name}.bin"));
        if artifact.exists() {
            let kept = self.dir.join(format!("{name}.previous.bin"));
            std::fs::rename(&artifact, &kept)
                .map_err(|e| format!("cannot keep the previous artifact: {e}"))?;
        }
        install()?;

        let package = Package {
            name: name.to_string(),
            version: version.to_string(),
            addon,
            content_hash,
            signature,
            size,
            selector,
            source,
            previous,
        };
        let json = serde_json::to_vec_pretty(&package.meta()).expect("package metadata serializes");
        self.write_atomic(&format!("{name}.json"), &json)?;
        self.packages
            .write()
            .expect("packages lock")
            .insert(name.to_string(), package);
        Ok(())
    }

    /// Puts a package back to the version it replaced (ADR-0019), which becomes the next version to
    /// go back to — so pressing this twice returns to where it started. The Selector is untouched:
    /// which Agents a package reaches is a separate decision from which bytes they get.
    ///
    /// Distribution follows from state, as every package change does: matching Agents are offered
    /// the restored version on their next exchange.
    ///
    /// # Errors
    /// Returns an error when no package of that name exists, or when it has no previous version —
    /// the state of every package at its first upload.
    pub fn rollback(&self, name: &str) -> Result<(), String> {
        let mut packages = self.packages.write().expect("packages lock");
        let package = packages
            .get(name)
            .ok_or_else(|| format!("no package {name:?}"))?;
        let restore = package
            .previous
            .clone()
            .ok_or_else(|| format!("package {name:?} has no previous version to go back to"))?;
        let displaced = package.current();

        // Swap the artifacts the two versions own. Either side may own none — a referenced version
        // keeps its bytes elsewhere — so the swap is over whichever files actually exist.
        let artifact = self.dir.join(format!("{name}.bin"));
        let kept = self.dir.join(format!("{name}.previous.bin"));
        let swapping = self.dir.join(format!("{name}.swap.tmp"));
        match (displaced.source.is_none(), restore.source.is_none()) {
            (true, true) => {
                std::fs::rename(&artifact, &swapping)
                    .and_then(|()| std::fs::rename(&kept, &artifact))
                    .and_then(|()| std::fs::rename(&swapping, &kept))
                    .map_err(|e| format!("cannot swap the artifacts of {name:?}: {e}"))?;
            }
            (true, false) => std::fs::rename(&artifact, &kept)
                .map_err(|e| format!("cannot keep the artifact of {name:?}: {e}"))?,
            (false, true) => std::fs::rename(&kept, &artifact)
                .map_err(|e| format!("cannot restore the artifact of {name:?}: {e}"))?,
            (false, false) => {}
        }

        let package = Package {
            name: name.to_string(),
            selector: package.selector.clone(),
            version: restore.version,
            addon: restore.addon,
            content_hash: restore.content_hash,
            signature: restore.signature,
            size: restore.size,
            source: restore.source,
            previous: Some(displaced),
        };
        let json = serde_json::to_vec_pretty(&package.meta()).expect("package metadata serializes");
        self.write_atomic(&format!("{name}.json"), &json)?;
        packages.insert(name.to_string(), package);
        Ok(())
    }

    /// One stored package as the REST API presents it; `None` when no such package exists. The
    /// single source every handler that answers with a package reads, so no response can describe
    /// one from what its caller happened to know.
    pub fn summary(&self, name: &str) -> Option<PackageSummary> {
        self.packages
            .read()
            .expect("packages lock")
            .get(name)
            .map(PackageSummary::of)
    }

    /// Points a package at an artifact that lives somewhere else (ADR-0018): no bytes are stored
    /// or fetched, and Agents are given `url` — with `headers`, when the source needs them — plus
    /// the `content_hash` the operator supplied, which is the only thing that will check what they
    /// receive.
    ///
    /// Creates the package when it does not exist, and replaces an uploaded one's bytes with the
    /// reference. An existing Selector is kept: re-pointing a targeted package must not widen it.
    ///
    /// # Errors
    /// Returns an error when the name is invalid, the hash is not a SHA-256, or the metadata cannot
    /// be persisted.
    pub fn set_source(
        &self,
        name: &str,
        version: &str,
        addon: bool,
        content_hash: Vec<u8>,
        signature: Option<Vec<u8>>,
        source: Source,
    ) -> Result<(), String> {
        validate_name(name).map_err(|e| format!("invalid name {name:?}: {e}"))?;
        if content_hash.len() != 32 {
            return Err(
                "the content hash must be a SHA-256: 64 hex characters, as published in a                  release's checksums file"
                    .to_string(),
            );
        }
        if !source.url.starts_with("http://") && !source.url.starts_with("https://") {
            return Err(format!(
                "the source url {:?} must start with http:// or https://",
                source.url
            ));
        }
        // Bytes this Server was holding are no longer what the fleet gets — but they are what a
        // rollback goes back to, so `replace` keeps them as the previous version rather than
        // deleting them (ADR-0019).
        self.replace(
            name,
            version,
            addon,
            content_hash,
            signature,
            0,
            Some(source),
            || Ok(()),
        )
    }

    /// Creates or replaces a package from bytes already in hand — the shape the tests and any
    /// small artifact use. A real upload takes [`put_staged`](Self::put_staged) instead.
    pub fn put(
        &self,
        name: String,
        version: String,
        addon: bool,
        signature: Option<Vec<u8>>,
        artifact: Vec<u8>,
    ) -> Result<(), String> {
        validate_name(&name).map_err(|e| format!("invalid name {name:?}: {e}"))?;
        if artifact.is_empty() {
            return Err("the package artifact is empty; refusing to distribute it".to_string());
        }
        let content_hash = Sha256::digest(&artifact).to_vec();
        let size = artifact.len() as u64;
        // Bytes arrived: this package is held here now, whatever it referred to before.
        self.replace(
            &name,
            &version,
            addon,
            content_hash,
            signature,
            size,
            None,
            || self.write_atomic(&format!("{name}.bin"), &artifact),
        )
    }

    /// Deletes a package; `Ok(false)` when none of that name exists.
    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let mut packages = self.packages.write().expect("packages lock");
        if packages.remove(name).is_none() {
            return Ok(false);
        }
        for suffix in ["json", "bin", "previous.bin"] {
            let path = self.dir.join(format!("{name}.{suffix}"));
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("cannot delete {}: {e}", path.display()))?;
            }
        }
        Ok(true)
    }

    /// One Agent's offer: the packages whose Selector matches it, plus the `all_packages_hash` —
    /// the Baseline's "aggregate of all packages names and content" — over *that* set (ADR-0017).
    /// `Ok(None)` when nothing matches, which is what an Agent outside every Selector must see;
    /// `Err` when the targeting is ambiguous and the Server refuses to guess.
    ///
    /// The Baseline calls this "the packages that are available on the Server **for this Agent**",
    /// so both the map and its aggregate are per-Agent; the aggregate is what gates re-offering,
    /// and computing it over the whole store would re-offer an Agent packages it never gets.
    /// `download_base` prefixes each `download_url`; `headers` (the `[auth]` credential) ride the
    /// download so it is authenticated like every other request.
    pub fn offer_for(
        &self,
        description: Option<&AgentDescription>,
        download_base: &str,
        headers: Option<Headers>,
    ) -> Result<Option<PackagesAvailable>, String> {
        let packages = self.packages.read().expect("packages lock");
        let matching = resolve(&packages, description)?;
        if matching.is_empty() {
            return Ok(None);
        }
        Ok(Some(PackagesAvailable {
            packages: matching
                .iter()
                .map(|p| {
                    (
                        p.name.clone(),
                        p.to_available(download_base, headers.clone()),
                    )
                })
                .collect(),
            all_packages_hash: aggregate_hash(matching.iter().copied()),
        }))
    }

    /// The aggregate hash for one Agent, to gate re-offering without building the whole message.
    /// Empty when nothing matches or the targeting is ambiguous — in both cases the Agent is
    /// offered nothing, and has nothing to be in sync with.
    pub fn all_packages_hash_for(&self, description: Option<&AgentDescription>) -> Vec<u8> {
        let packages = self.packages.read().expect("packages lock");
        match resolve(&packages, description) {
            Ok(matching) if !matching.is_empty() => aggregate_hash(matching.into_iter()),
            _ => Vec::new(),
        }
    }

    /// Sets a package's Selector (ADR-0017). Which Agents that newly reaches — or stops reaching —
    /// follows from state on their next exchange; nothing is pushed from here.
    pub fn set_selector(
        &self,
        name: &str,
        selector: BTreeMap<String, String>,
    ) -> Result<(), String> {
        let mut packages = self.packages.write().expect("packages lock");
        if !packages.contains_key(name) {
            return Err(format!("no package {name:?}"));
        }
        let mut package = packages
            .remove(name)
            .expect("the package was just looked up");
        package.selector = selector;
        let json = serde_json::to_vec_pretty(&package.meta()).expect("package metadata serializes");
        let written = self.write_atomic(&format!("{name}.json"), &json);
        packages.insert(name.to_string(), package);
        written
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.dir.join(name);
        let temp = self.dir.join(format!("{name}.tmp"));
        std::fs::write(&temp, bytes)
            .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path).map_err(|e| format!("cannot persist {}: {e}", path.display()))
    }
}

/// Hashes a file by streaming it, returning `(size, sha256)`. Used where an artifact's integrity
/// must be checked without the artifact having to fit in memory.
fn hash_file(path: &Path) -> Result<(u64, Vec<u8>), String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((size, hasher.finalize().to_vec()))
}

/// Which packages one Agent is offered (ADR-0017): every matching addon, and **one** top-level
/// package — the Baseline knows "normally only one top-level package", and a Supervisor has one
/// binary to replace.
///
/// When several top-level packages match, the **most specific Selector wins**: the one naming the
/// most attributes. That is what makes the pattern an operator actually wants work — a fleet-wide
/// package with an empty Selector, and a narrower one aimed at the hosts a rollout starts on, which
/// overrides it for exactly those. A tie between two equally specific Selectors is the one case
/// with no defensible answer, so it is refused and reported rather than guessed.
fn resolve<'a>(
    packages: &'a BTreeMap<String, Package>,
    description: Option<&AgentDescription>,
) -> Result<Vec<&'a Package>, String> {
    let (top_level, addons): (Vec<&Package>, Vec<&Package>) = packages
        .values()
        .filter(|p| matches(&p.selector, description))
        .partition(|p| !p.addon);

    let mut chosen: Option<&Package> = None;
    for package in &top_level {
        match chosen {
            None => chosen = Some(package),
            Some(current) if package.selector.len() > current.selector.len() => {
                chosen = Some(package)
            }
            Some(current) if package.selector.len() == current.selector.len() => {
                return Err(format!(
                    "packages {:?} and {:?} are equally specific for this Agent; \
                     narrow one of their Selectors — an Agent has one binary to replace",
                    current.name, package.name
                ));
            }
            Some(_) => {}
        }
    }

    Ok(chosen.into_iter().chain(addons).collect())
}

/// The aggregate over all packages — name and content — in name order (the map iterates sorted).
fn aggregate_hash<'a>(packages: impl Iterator<Item = &'a Package>) -> Vec<u8> {
    let mut hasher = Sha256::new();
    for package in packages {
        hasher.update((package.name.len() as u64).to_le_bytes());
        hasher.update(package.name.as_bytes());
        hasher.update(package.package_hash());
    }
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_list_round_trip_and_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
            assert!(store.is_empty());
            store
                .put(
                    "otelcol".to_string(),
                    "1.2.3".to_string(),
                    false,
                    None,
                    b"binary".to_vec(),
                )
                .expect("put");
            assert!(!store.is_empty());
            let listed = store.list();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].name, "otelcol");
            assert_eq!(listed[0].version, "1.2.3");
            assert!(
                listed[0].selector.is_empty(),
                "a new package targets everyone"
            );
            let path = store.artifact_path("otelcol").expect("an artifact path");
            assert_eq!(std::fs::read(&path).expect("read"), b"binary");
        }
        // A fresh store over the same directory restores the package and re-verifies its artifact
        // against the recorded hash — by streaming it, so the check never depends on its size.
        let reopened = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        let path = reopened.artifact_path("otelcol").expect("an artifact path");
        assert_eq!(std::fs::read(&path).expect("read"), b"binary");
        assert!(
            reopened.artifact_path("nothing-stored").is_none(),
            "no path for a package that does not exist"
        );
    }

    #[test]
    fn the_aggregate_hash_changes_with_content_and_is_order_independent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        store
            .put(
                "a".to_string(),
                "1".to_string(),
                false,
                None,
                b"one".to_vec(),
            )
            .expect("put a");
        let before = store.all_packages_hash_for(None);
        store
            .put(
                "b".to_string(),
                "1".to_string(),
                false,
                None,
                b"two".to_vec(),
            )
            .expect("put b");
        let after = store.all_packages_hash_for(None);
        assert_ne!(before, after, "a new package changes the aggregate");

        // Re-putting the same content yields the same aggregate — the hash is content-defined.
        store
            .put(
                "a".to_string(),
                "1".to_string(),
                false,
                None,
                b"one".to_vec(),
            )
            .expect("re-put a");
        assert_eq!(store.all_packages_hash_for(None), after);
    }

    #[test]
    fn the_offer_carries_download_urls_and_the_aggregate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        assert!(
            store
                .offer_for(None, "", None)
                .expect("no ambiguity")
                .is_none(),
            "an empty store offers nothing"
        );
        store
            .put(
                "otelcol".to_string(),
                "1.2.3".to_string(),
                false,
                None,
                b"bin".to_vec(),
            )
            .expect("put");
        let offer = store
            .offer_for(None, "https://fleet.example:4320", None)
            .expect("no ambiguity")
            .expect("offer");
        assert_eq!(offer.all_packages_hash, store.all_packages_hash_for(None));
        let available = &offer.packages["otelcol"];
        assert_eq!(available.version, "1.2.3");
        assert_eq!(
            available.file.as_ref().unwrap().download_url,
            "https://fleet.example:4320/api/v1/packages/otelcol/file"
        );
    }

    /// The shape ADR-0019 promises: one step back, and pressing it twice returns to where it
    /// started.
    #[test]
    fn a_rollback_swaps_the_current_and_previous_versions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let put = |version: &str, bytes: &[u8]| {
            store.put(
                "otelcol".to_string(),
                version.to_string(),
                false,
                None,
                bytes.to_vec(),
            )
        };
        put("0.156.0", b"old").expect("put");
        assert!(
            store
                .summary("otelcol")
                .expect("stored")
                .previous_version
                .is_none(),
            "a package at its first upload has nothing to go back to"
        );
        assert!(
            store.rollback("otelcol").is_err(),
            "and rolling it back is refused"
        );

        put("0.157.0", b"new").expect("replace");
        let summary = store.summary("otelcol").expect("stored");
        assert_eq!(summary.version, "0.157.0");
        assert_eq!(summary.previous_version.as_deref(), Some("0.156.0"));

        store.rollback("otelcol").expect("rollback");
        let summary = store.summary("otelcol").expect("stored");
        assert_eq!(summary.version, "0.156.0");
        assert_eq!(
            summary.previous_version.as_deref(),
            Some("0.157.0"),
            "what we rolled back from is what we go back to next"
        );
        let path = store.artifact_path("otelcol").expect("an artifact path");
        assert_eq!(std::fs::read(&path).expect("read"), b"old");

        // Twice returns to where it started.
        store.rollback("otelcol").expect("rollback again");
        let summary = store.summary("otelcol").expect("stored");
        assert_eq!(summary.version, "0.157.0");
        let path = store.artifact_path("otelcol").expect("an artifact path");
        assert_eq!(std::fs::read(&path).expect("read"), b"new");
    }

    /// Only one step is kept: a third version displaces the first, which is then gone for good.
    #[test]
    fn only_one_step_is_remembered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        for version in ["1", "2", "3"] {
            store
                .put(
                    "otelcol".to_string(),
                    version.to_string(),
                    false,
                    None,
                    format!("bytes-{version}").into_bytes(),
                )
                .expect("put");
        }
        assert_eq!(
            store
                .summary("otelcol")
                .expect("stored")
                .previous_version
                .as_deref(),
            Some("2")
        );
        store.rollback("otelcol").expect("rollback");
        assert_eq!(store.summary("otelcol").expect("stored").version, "2");
        store.rollback("otelcol").expect("rollback");
        assert_eq!(
            store.summary("otelcol").expect("stored").version,
            "3",
            "the first version is not reachable; one step means one step"
        );
    }

    /// The Selector belongs to the package, not to the bytes (ADR-0019 point 3).
    #[test]
    fn a_rollback_leaves_the_selector_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        store
            .put(
                "otelcol".to_string(),
                "1".to_string(),
                false,
                None,
                b"a".to_vec(),
            )
            .expect("put");
        store
            .set_selector(
                "otelcol",
                [("os.type".to_string(), "linux".to_string())].into(),
            )
            .expect("target it");
        store
            .put(
                "otelcol".to_string(),
                "2".to_string(),
                false,
                None,
                b"b".to_vec(),
            )
            .expect("replace");
        store.rollback("otelcol").expect("rollback");

        let summary = store.summary("otelcol").expect("stored");
        assert_eq!(summary.version, "1");
        assert_eq!(
            summary.selector,
            [("os.type".to_string(), "linux".to_string())].into(),
            "an undo does only what it says"
        );
    }

    /// A referenced version costs a URL and a checksum to remember, and rolls back to and from an
    /// uploaded one without either side needing bytes the other has (ADR-0018 + ADR-0019).
    #[test]
    fn a_rollback_works_across_uploaded_and_referenced_versions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        store
            .put(
                "otelcol".to_string(),
                "1".to_string(),
                false,
                None,
                b"here".to_vec(),
            )
            .expect("put");
        store
            .set_source(
                "otelcol",
                "2",
                false,
                vec![7u8; 32],
                None,
                Source {
                    url: "https://cdn.example/otelcol-2.tar.gz".to_string(),
                    headers: BTreeMap::new(),
                },
            )
            .expect("point at a source");

        let summary = store.summary("otelcol").expect("stored");
        assert_eq!(
            summary.source_url.as_deref(),
            Some("https://cdn.example/otelcol-2.tar.gz")
        );
        assert_eq!(summary.previous_version.as_deref(), Some("1"));
        assert!(
            store.artifact_path("otelcol").is_none(),
            "a referenced package is not served from here"
        );

        // Back to the uploaded one: its bytes were kept, so nothing has to be produced again.
        store.rollback("otelcol").expect("rollback");
        let summary = store.summary("otelcol").expect("stored");
        assert_eq!(summary.version, "1");
        assert!(summary.source_url.is_none());
        assert_eq!(
            summary.previous_source_url.as_deref(),
            Some("https://cdn.example/otelcol-2.tar.gz"),
            "the reference is what a further rollback goes back to"
        );
        let path = store.artifact_path("otelcol").expect("an artifact path");
        assert_eq!(std::fs::read(&path).expect("read"), b"here");
    }

    #[test]
    fn a_remembered_version_survives_a_reopen_and_is_re_verified() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
            store
                .put(
                    "otelcol".to_string(),
                    "1".to_string(),
                    false,
                    None,
                    b"old".to_vec(),
                )
                .expect("put");
            store
                .put(
                    "otelcol".to_string(),
                    "2".to_string(),
                    false,
                    None,
                    b"new".to_vec(),
                )
                .expect("replace");
        }
        let reopened = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(
            reopened
                .summary("otelcol")
                .expect("stored")
                .previous_version
                .as_deref(),
            Some("1")
        );
        reopened.rollback("otelcol").expect("rollback");
        let path = reopened.artifact_path("otelcol").expect("an artifact path");
        assert_eq!(std::fs::read(&path).expect("read"), b"old");

        // A kept artifact is shipped by a rollback, so a corrupt one must fail startup exactly as
        // a corrupt current one does.
        std::fs::write(dir.path().join("otelcol.previous.bin"), b"tampered").expect("tamper");
        let err = match PackageStore::open(dir.path().to_path_buf()) {
            Ok(_) => panic!("a tampered kept artifact must fail to open"),
            Err(e) => e,
        };
        assert!(err.contains("does not match"), "got {err:?}");
    }

    #[test]
    fn deleting_a_package_takes_its_remembered_version_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        store
            .put(
                "otelcol".to_string(),
                "1".to_string(),
                false,
                None,
                b"old".to_vec(),
            )
            .expect("put");
        store
            .put(
                "otelcol".to_string(),
                "2".to_string(),
                false,
                None,
                b"new".to_vec(),
            )
            .expect("replace");
        assert!(store.delete("otelcol").expect("delete"));
        assert!(
            !dir.path().join("otelcol.previous.bin").exists(),
            "nothing of a deleted package is left on disk"
        );
        assert!(PackageStore::open(dir.path().to_path_buf())
            .expect("reopen")
            .is_empty());
    }

    #[test]
    fn a_corrupt_artifact_fails_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
            store
                .put(
                    "otelcol".to_string(),
                    "1".to_string(),
                    false,
                    None,
                    b"good".to_vec(),
                )
                .expect("put");
        }
        // Tamper with the artifact without updating the recorded hash.
        std::fs::write(dir.path().join("otelcol.bin"), b"tampered").expect("tamper");
        let err = match PackageStore::open(dir.path().to_path_buf()) {
            Ok(_) => panic!("a tampered artifact must fail to open"),
            Err(e) => e,
        };
        assert!(err.contains("does not match"), "got {err:?}");
    }
}
