//! The package store on the filesystem (ADR-0020, ADR-0030): the default adapter behind
//! [`PackageBackend`](crate::packages::PackageBackend) — one directory per Package under
//! `packages_dir`, holding `package.json` and one `<os>-<arch>.bin` per uploaded entry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::packages::{
    Entry, Package, PackageBackend, PackageId, Platform, Source, DEPLOYMENTS_DIR,
};

/// One directory per Package, owner-only, with its metadata and its uploaded artifacts.
pub struct FsPackageBackend {
    dir: PathBuf,
}

impl FsPackageBackend {
    /// Opens the directory, creating it owner-only: a referenced entry's metadata carries the
    /// private source's headers — a bearer token (ADR-0019) — so the store must not be readable by
    /// other local users on the Server host. The metadata files are written `0600` as well.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        super::create_private_dir(&dir)?;
        Ok(FsPackageBackend { dir })
    }

    fn set_dir(&self, id: &PackageId) -> PathBuf {
        self.dir.join(id.to_string())
    }
}

impl PackageBackend for FsPackageBackend {
    /// A metadata or artifact file that cannot be read, does not parse, or whose artifact no longer
    /// matches its recorded hash fails the open — a corrupt distribution artifact must never ship.
    /// There is **no migration**: a directory in a shape this Server does not write is named in
    /// that error rather than skipped, so a store left over from an older layout is reported
    /// instead of quietly appearing empty.
    fn load(&self) -> Result<Vec<Package>, String> {
        let mut sets = Vec::new();
        let listing = std::fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?;
        for entry in listing {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?
                .path();
            if !path.is_dir() {
                // A Package is a directory. A loose file at the top level is what the pre-ADR-0020
                // store wrote (`<name>.json`, `<name>@<os>-<arch>.json`/`.bin`), and there is no
                // reader for it any more — so it is named rather than skipped. Skipping would turn
                // an old store into one that merely looks empty, which is the failure an operator
                // cannot see (ADR-0011: loud, never silently ignored).
                return Err(format!(
                    "{} is not a Package directory — this Server reads no other package store \
                     layout. \
                     Move it aside or delete it; nothing here will be migrated.",
                    path.display()
                ));
            }
            // The one directory here that is deliberately not a Package: the channel store the
            // Deployments live in (ADR-0030), armed by this same `packages_dir`.
            if path.file_name().and_then(|n| n.to_str()) == Some(DEPLOYMENTS_DIR) {
                continue;
            }
            let meta_path = path.join("package.json");
            if !meta_path.exists() {
                // Skipping is the dangerous half. A store written by an older layout —
                // `<name>@<version>@<type>/set.json` — would open *successfully and empty*: no
                // offer, no error, and a package list an operator reads as "nothing uploaded yet"
                // (ADR-0030 point 5). So the directory is named instead.
                return Err(format!(
                    "{} holds no package.json — this Server reads no other package store layout. \
                     Move it aside or delete it; nothing here will be migrated.",
                    path.display()
                ));
            }
            let text = std::fs::read_to_string(&meta_path)
                .map_err(|e| format!("cannot read {}: {e}", meta_path.display()))?;
            let meta: PackageMeta = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", meta_path.display()))?;
            let id = PackageId::new(&meta.agent_type, &meta.version)
                .map_err(|e| format!("invalid identity in {}: {e}", meta_path.display()))?;
            // The directory name is derived from the identity; a mismatch means the artifacts
            // will not be found where the store looks for them, so it is refused by name.
            if path.file_name().and_then(|n| n.to_str()) != Some(id.to_string().as_str()) {
                return Err(format!(
                    "{} does not match the identity {} its package.json states — rename \
                     the directory or fix the file",
                    path.display(),
                    id
                ));
            }
            let mut entries = BTreeMap::new();
            for entry_meta in meta.entries {
                let platform = Platform::new(&entry_meta.os, &entry_meta.arch)
                    .map_err(|e| format!("invalid platform in {}: {e}", meta_path.display()))?;
                let content_hash = hex::decode(&entry_meta.content_hash_hex)
                    .map_err(|e| format!("invalid content hash in {}: {e}", meta_path.display()))?;
                let source = entry_meta.source_url.map(|url| Source {
                    url,
                    headers: entry_meta.source_headers,
                });
                // An uploaded artifact is re-hashed by streaming, so a corrupt one never ships. A
                // referenced one has nothing here to check: its hash is the operator's word,
                // verified by every Agent that downloads it (ADR-0019).
                let size = match &source {
                    Some(_) => 0,
                    None => {
                        let artifact = path.join(format!("{}.bin", platform.tag()));
                        let (size, actual) = hash_file(&artifact)?;
                        if actual != content_hash {
                            return Err(format!(
                                "set {id} for {}: artifact does not match its recorded content hash",
                                platform.tag()
                            ));
                        }
                        size
                    }
                };
                entries.insert(
                    platform.clone(),
                    Entry {
                        platform,
                        content_hash,
                        size,
                        source,
                    },
                );
            }
            sets.push(Package { id, entries });
        }
        Ok(sets)
    }

    fn put(&self, package: &Package) -> Result<(), String> {
        let dir = self.set_dir(&package.id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let meta =
            serde_json::to_vec_pretty(&PackageMeta::of(package)).expect("package serializes");
        super::replace_owner_only(&dir.join("package.json"), &meta)
    }

    fn remove(&self, id: &PackageId) -> Result<(), String> {
        let dir = self.set_dir(id);
        std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot delete {}: {e}", dir.display()))
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    /// In the Package's own directory, so the commit is a rename — and named per Platform, so
    /// uploading a release's five artifacts at once cannot have them overwrite each other while
    /// they are still in flight.
    fn staging_path(&self, id: &PackageId, platform: &Platform) -> PathBuf {
        self.set_dir(id).join(format!("{}.upload", platform.tag()))
    }

    fn hash_staged(&self, staged: &Path) -> Result<(u64, Vec<u8>), String> {
        hash_file(staged)
    }

    fn commit_staged(
        &self,
        id: &PackageId,
        platform: &Platform,
        staged: &Path,
    ) -> Result<(), String> {
        let artifact = self.artifact_path(id, platform);
        std::fs::rename(staged, &artifact)
            .map_err(|e| format!("cannot persist {}: {e}", artifact.display()))
    }

    fn discard_staged(&self, staged: &Path) {
        let _ = std::fs::remove_file(staged);
    }

    fn write_artifact(
        &self,
        id: &PackageId,
        platform: &Platform,
        bytes: &[u8],
    ) -> Result<(), String> {
        let path = self.artifact_path(id, platform);
        std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    fn remove_artifact(&self, id: &PackageId, platform: &Platform) -> Result<(), String> {
        let path = self.artifact_path(id, platform);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot delete {}: {e}", path.display())),
        }
    }

    /// `<package>/<os>-<arch>.bin`.
    fn artifact_path(&self, id: &PackageId, platform: &Platform) -> PathBuf {
        self.set_dir(id).join(format!("{}.bin", platform.tag()))
    }

    /// Every `.bin` in every Package's directory; the `.upload` staging file is not one. A best-effort
    /// walk: a file racing deletion simply is not counted, which is the safe direction for a
    /// ceiling that gates *new* uploads.
    fn total_bytes(&self) -> u64 {
        let Ok(dirs) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        dirs.flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| std::fs::read_dir(entry.path()).ok())
            .flat_map(|files| files.flatten())
            .filter(|file| {
                file.file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".bin"))
            })
            .filter_map(|file| file.metadata().ok().map(|m| m.len()))
            .sum()
    }
}

/// Hashes a file by streaming it, returning `(size, sha256)`. Used where an artifact's integrity
/// must be checked without the artifact having to fit in memory.
fn hash_file(path: &Path) -> Result<(u64, Vec<u8>), String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let size = std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok((size, hasher.finalize().to_vec()))
}

/// A Package as persisted: `<agent_type>@<version>/package.json`, entries inline. One document per
/// Package — what ADR-0020 kept secretly (other versions), this store keeps openly, as more
/// Packages.
#[derive(Serialize, Deserialize)]
struct PackageMeta {
    agent_type: String,
    version: String,
    #[serde(default)]
    entries: Vec<EntryMeta>,
}

/// One entry as persisted inside `package.json`; an uploaded entry's bytes are `<os>-<arch>.bin`
/// beside it.
#[derive(Serialize, Deserialize)]
struct EntryMeta {
    os: String,
    arch: String,
    content_hash_hex: String,
    /// The source of a referenced entry (ADR-0019); absent for an uploaded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    source_headers: BTreeMap<String, String>,
}

impl EntryMeta {
    fn of(entry: &Entry) -> Self {
        EntryMeta {
            os: entry.platform.os.clone(),
            arch: entry.platform.arch.clone(),
            content_hash_hex: hex::encode(&entry.content_hash),
            source_url: entry.source.as_ref().map(|s| s.url.clone()),
            source_headers: entry
                .source
                .as_ref()
                .map(|s| s.headers.clone())
                .unwrap_or_default(),
        }
    }
}

impl PackageMeta {
    fn of(set: &Package) -> Self {
        PackageMeta {
            agent_type: set.id.agent_type.clone(),
            version: set.id.version.clone(),
            entries: set.entries.values().map(EntryMeta::of).collect(),
        }
    }
}
