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
    AgentDescription, DownloadableFile, Headers, PackageAvailable, PackageType, PackagesAvailable,
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
}

impl Package {
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

    /// This package as a wire `PackageAvailable`; `download_base` is the URL prefix the artifact is
    /// served under, so the Agent's `download_url` is `{base}/{name}/file`. Empty `download_base`
    /// yields a path the Agent resolves against its own OpAMP endpoint.
    fn to_available(&self, download_base: &str, headers: Option<Headers>) -> PackageAvailable {
        PackageAvailable {
            r#type: if self.addon {
                PackageType::Addon as i32
            } else {
                PackageType::TopLevel as i32
            },
            version: self.version.clone(),
            file: Some(DownloadableFile {
                download_url: format!("{download_base}/api/v1/packages/{}/file", self.name),
                content_hash: self.content_hash.clone(),
                signature: self.signature.clone().unwrap_or_default(),
                headers,
            }),
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
}

/// Metadata as persisted next to the artifact (`<name>.json`); the artifact is `<name>.bin`.
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
            let artifact_path = dir.join(format!("{}.bin", meta.name));
            let content_hash = hex::decode(&meta.content_hash_hex)
                .map_err(|e| format!("invalid content hash in {}: {e}", path.display()))?;
            // Re-hash by streaming: the check must not depend on the artifact fitting in memory.
            let (size, actual) = hash_file(&artifact_path)?;
            if actual != content_hash {
                return Err(format!(
                    "package {:?}: artifact does not match its recorded content hash",
                    meta.name
                ));
            }
            let signature = match &meta.signature_hex {
                Some(hex) => Some(
                    hex::decode(hex)
                        .map_err(|e| format!("invalid signature in {}: {e}", path.display()))?,
                ),
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
            .map(|p| PackageSummary {
                name: p.name.clone(),
                version: p.version.clone(),
                addon: p.addon,
                selector: p.selector.clone(),
            })
            .collect()
    }

    /// Where a package's artifact lives, for the download endpoint to stream from. `None` when no
    /// package of that name is stored.
    pub fn artifact_path(&self, name: &str) -> Option<PathBuf> {
        self.packages
            .read()
            .expect("packages lock")
            .get(name)
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
        // A new artifact for an existing package keeps that package's Selector: replacing the
        // bytes must never silently widen a targeted rollout to the whole fleet.
        let selector = self.selector_of(name);
        let meta = PackageMeta {
            name: name.to_string(),
            version: version.to_string(),
            addon,
            content_hash_hex: hex::encode(&content_hash),
            signature_hex: signature.as_ref().map(hex::encode),
            selector: selector.clone(),
        };
        let json = serde_json::to_vec_pretty(&meta).expect("package metadata serializes");
        let artifact = self.dir.join(format!("{name}.bin"));
        std::fs::rename(staged, &artifact)
            .map_err(|e| format!("cannot persist {}: {e}", artifact.display()))?;
        self.write_atomic(&format!("{name}.json"), &json)?;
        self.packages.write().expect("packages lock").insert(
            name.to_string(),
            Package {
                name: name.to_string(),
                version: version.to_string(),
                addon,
                content_hash,
                signature,
                size,
                selector,
            },
        );
        Ok(())
    }

    /// A stored package's Selector for the REST views; `None` when no such package exists.
    pub fn selector_of_package(&self, name: &str) -> Option<BTreeMap<String, String>> {
        self.packages
            .read()
            .expect("packages lock")
            .get(name)
            .map(|p| p.selector.clone())
    }

    /// The Selector a stored package already carries, or empty when it is new.
    fn selector_of(&self, name: &str) -> BTreeMap<String, String> {
        self.packages
            .read()
            .expect("packages lock")
            .get(name)
            .map(|p| p.selector.clone())
            .unwrap_or_default()
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
        let selector = self.selector_of(&name);
        let meta = PackageMeta {
            name: name.clone(),
            version: version.clone(),
            addon,
            content_hash_hex: hex::encode(&content_hash),
            signature_hex: signature.as_ref().map(hex::encode),
            selector: selector.clone(),
        };
        let json = serde_json::to_vec_pretty(&meta).expect("package metadata serializes");
        self.write_atomic(&format!("{name}.bin"), &artifact)?;
        self.write_atomic(&format!("{name}.json"), &json)?;
        self.packages.write().expect("packages lock").insert(
            name.clone(),
            Package {
                name,
                version,
                addon,
                content_hash,
                signature,
                size,
                selector,
            },
        );
        Ok(())
    }

    /// Deletes a package; `Ok(false)` when none of that name exists.
    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let mut packages = self.packages.write().expect("packages lock");
        if packages.remove(name).is_none() {
            return Ok(false);
        }
        for suffix in ["json", "bin"] {
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
        let meta = PackageMeta {
            name: package.name.clone(),
            version: package.version.clone(),
            addon: package.addon,
            content_hash_hex: hex::encode(&package.content_hash),
            signature_hex: package.signature.as_ref().map(hex::encode),
            selector: package.selector.clone(),
        };
        let json = serde_json::to_vec_pretty(&meta).expect("package metadata serializes");
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
