//! The package store's metadata on the filesystem (ADR-0039, ADR-0023): the default adapter behind
//! [`PackageBackend`](crate::packages::PackageBackend) — one JSON file per Package under
//! `<data_dir>/packages/`, its entries inside, each naming its artifact by hash. The bytes are the
//! blob store's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::packages::{Entry, Package, PackageBackend, PackageError, PackageId, Platform, Signed};

/// One file per Package, `<agent_type>@<version>.json`, owner-only.
pub struct FsPackageBackend {
    dir: PathBuf,
}

impl FsPackageBackend {
    /// Opens the store in the data directory `root`, creating `packages/` owner-only: what the
    /// fleet installs is not another local user's business on the Server host.
    pub fn open(root: &Path) -> Result<Self, String> {
        let dir = root.join(super::PACKAGES);
        super::create_private_dir(&dir)?;
        Ok(FsPackageBackend { dir })
    }

    fn path(&self, id: &PackageId) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }
}

impl FsPackageBackend {
    /// Creates or replaces one Package's file whole — the write of `server migrate`, which moves a
    /// data directory no Server is running on.
    pub(crate) fn put(&self, package: &Package) -> Result<(), String> {
        let meta =
            serde_json::to_vec_pretty(&PackageMeta::of(package)).expect("package serializes");
        super::replace_owner_only(&self.path(&package.id), &meta)
    }

    /// The Package `id` as its file holds it; `None` when there is no file.
    fn read(&self, id: &PackageId) -> Result<Option<Package>, String> {
        let path = self.path(id);
        match std::fs::read_to_string(&path) {
            Ok(text) => parse(&id.to_string(), &path, &text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    /// Reads the Package `id`, lets `change` edit it, and writes it back when `change` answers
    /// that it changed: the file is the record, and this Server its only writer. No file is an
    /// error naming the Package.
    fn amend<T>(
        &self,
        id: &PackageId,
        change: impl FnOnce(&mut Package) -> (bool, T),
    ) -> Result<T, String> {
        let mut package = self
            .read(id)?
            .ok_or_else(|| format!("no package set {id}"))?;
        let (changed, answer) = change(&mut package);
        if changed {
            self.put(&package)?;
        }
        Ok(answer)
    }
}

/// One stored Package from the text of its file `path`, whose stem is `stem`.
fn parse(stem: &str, path: &Path, text: &str) -> Result<Package, String> {
    let meta: PackageMeta =
        serde_json::from_str(text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
    let id = PackageId::load(&meta.agent_type, &meta.version)
        .map_err(|e| format!("invalid identity in {}: {e}", path.display()))?;
    if id.to_string() != stem {
        return Err(super::misnamed(path, "identity"));
    }
    let mut entries = BTreeMap::new();
    let mut stored: BTreeMap<Platform, (String, String)> = BTreeMap::new();
    let mut hidden: Vec<(Platform, (String, String))> = Vec::new();
    for entry in meta.entries {
        let platform = Platform::new(&entry.os, &entry.arch)
            .map_err(|e| format!("invalid platform in {}: {e}", path.display()))?;
        let content_hash = hex::decode(&entry.sha256)
            .ok()
            .filter(|_| crate::blobs::is_hash(&entry.sha256))
            .ok_or_else(|| format!("invalid sha256 in {}", path.display()))?;
        let signature = entry
            .signature_hex
            .map(hex::decode)
            .transpose()
            .map_err(|e| format!("invalid signature in {}: {e}", path.display()))?;
        // Held to the length a write is held to (ADR-0039 clause 18).
        if let Some(signature) = &signature {
            if signature.len() != crate::packages::ED25519_SIGNATURE_LEN {
                return Err(format!(
                    "invalid signature in {}: {} bytes, not {}",
                    path.display(),
                    signature.len(),
                    crate::packages::ED25519_SIGNATURE_LEN
                ));
            }
        }
        // Of two spellings that fold onto one platform, the one stored under the canonical
        // spelling is served, else the first in the file, as the database store chooses.
        let spelling = (entry.os.clone(), entry.arch.clone());
        let canonical =
            |(os, arch): &(String, String)| *os == platform.os && *arch == platform.arch;
        match stored.get(&platform).cloned() {
            Some(held) if !(canonical(&spelling) && !canonical(&held)) => {
                hidden.push((platform, spelling));
                continue;
            }
            Some(held) => hidden.push((platform.clone(), held)),
            None => {}
        }
        stored.insert(platform.clone(), spelling);
        entries.insert(
            platform.clone(),
            Entry {
                platform,
                content_hash,
                size: entry.size,
                signature,
            },
        );
    }
    // Named once the choice is made, so an entry is never said to be served and then not.
    for (platform, (os, arch)) in &stored {
        crate::packages::warn_stored_spelling(&id, os, arch, platform, false);
    }
    for (platform, (os, arch)) in &hidden {
        crate::packages::warn_stored_spelling(&id, os, arch, platform, true);
    }
    Ok(Package { id, entries })
}

impl PackageBackend for FsPackageBackend {
    /// A file that cannot be read, does not parse, or whose name disagrees with the identity it
    /// holds fails the open naming it; so does anything else in the directory (ADR-0023 clause 6).
    fn load(&self) -> Result<Vec<Package>, String> {
        let mut packages = Vec::new();
        for (stem, path) in super::json_files(&self.dir)? {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            packages.push(parse(&stem, &path, &text)?);
        }
        Ok(packages)
    }

    fn create(&self, id: &PackageId) -> Result<(), PackageError> {
        if self.read(id)?.is_some() {
            return Ok(());
        }
        Ok(self.put(&Package {
            id: id.clone(),
            entries: BTreeMap::new(),
        })?)
    }

    /// Its one Server is its only writer: `stored` is asked once, before the entry is written.
    fn put_entry(
        &self,
        id: &PackageId,
        entry: &Entry,
        stored: crate::packages::Stored,
    ) -> Result<(), PackageError> {
        if !stored()? {
            return Err(crate::packages::removed_while_uploaded());
        }
        Ok(self.amend(id, |package| {
            package
                .entries
                .insert(entry.platform.clone(), entry.clone());
            (true, ())
        })?)
    }

    fn sign(
        &self,
        id: &PackageId,
        platform: &Platform,
        content_hash: &[u8],
        signature: Option<&[u8]>,
    ) -> Result<Signed, PackageError> {
        if self.read(id)?.is_none() {
            return Ok(Signed::Changed(None));
        }
        Ok(
            self.amend(id, |package| match package.entries.get_mut(platform) {
                Some(entry) if entry.content_hash == content_hash => {
                    entry.signature = signature.map(<[u8]>::to_vec);
                    (true, Signed::Written)
                }
                stored => (false, Signed::Changed(stored.cloned())),
            })?,
        )
    }

    fn remove_entry(&self, id: &PackageId, platform: &Platform) -> Result<(), PackageError> {
        if self.read(id)?.is_none() {
            return Ok(());
        }
        Ok(self.amend(id, |package| {
            (package.entries.remove(platform).is_some(), ())
        })?)
    }

    fn remove(&self, id: &PackageId) -> Result<(), PackageError> {
        Ok(super::remove_file(&self.path(id))?)
    }
}

/// A Package as persisted: `packages/<agent_type>@<version>.json`, entries inline.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageMeta {
    agent_type: String,
    version: String,
    #[serde(default)]
    entries: Vec<EntryMeta>,
}

/// One entry as persisted: its platform, the blob that holds its bytes, named by hash only
/// (ADR-0023 clause 13), their size, and the operator's signature.
///
/// Unknown fields are refused, so an entry an older Server wrote — a reference to a source, or a
/// `content_hash_hex` beside an artifact file — fails the open naming the file rather than loading
/// as something it is not.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryMeta {
    os: String,
    arch: String,
    /// The SHA-256 of the artifact, lowercase hex: the blob's name, and what the Agent verifies.
    sha256: String,
    size: u64,
    /// The operator's Ed25519 signature over this entry, hex (ADR-0039); absent while unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature_hex: Option<String>,
}

impl PackageMeta {
    fn of(package: &Package) -> Self {
        PackageMeta {
            agent_type: package.id.agent_type.clone(),
            version: package.id.version.clone(),
            entries: package
                .entries
                .values()
                .map(|entry| EntryMeta {
                    os: entry.platform.os.clone(),
                    arch: entry.platform.arch.clone(),
                    sha256: hex::encode(&entry.content_hash),
                    size: entry.size,
                    signature_hex: entry.signature.as_ref().map(hex::encode),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Package an older Server stored with a `linux/arm` and a `linux/armv7` entry opens: both
    /// spellings fold onto `linux/arm32`, the first is served under it, and the second is not, so
    /// nothing fails the start and nothing serves the wrong bytes under one platform
    /// (ADR-0039 clause 6).
    /// Verifies: ADR-0039
    #[test]
    fn entries_stored_under_an_older_spelling_open_folded() {
        let hash = |c: char| c.to_string().repeat(64);
        let text = serde_json::json!({
            "agent_type": "otelcol",
            "version": "0.140.0",
            "entries": [
                {"os": "linux", "arch": "arm", "sha256": hash('a'), "size": 1},
                {"os": "linux", "arch": "armv7", "sha256": hash('b'), "size": 2},
                {"os": "linux", "arch": "amd64", "sha256": hash('c'), "size": 3},
            ],
        })
        .to_string();
        let package = parse("otelcol@0.140.0", Path::new("otelcol@0.140.0.json"), &text)
            .expect("an older spelling opens");
        let arm32 = Platform::new("linux", "arm32").expect("platform");
        assert_eq!(package.entries.len(), 2);
        assert_eq!(
            package.entries[&arm32].size, 1,
            "the first spelling is served"
        );
        assert!(package
            .entries
            .contains_key(&Platform::new("linux", "amd64").expect("platform")));

        // An entry stored as `linux/arm32`, as 0.8.2 passed it through, wins over an older one.
        let text = serde_json::json!({
            "agent_type": "otelcol",
            "version": "0.141.0",
            "entries": [
                {"os": "linux", "arch": "arm", "sha256": hash('a'), "size": 1},
                {"os": "linux", "arch": "arm32", "sha256": hash('b'), "size": 2},
            ],
        })
        .to_string();
        let package = parse("otelcol@0.141.0", Path::new("otelcol@0.141.0.json"), &text)
            .expect("both spellings open");
        assert_eq!(package.entries.len(), 1);
        assert_eq!(
            package.entries[&arm32].size, 2,
            "the canonical spelling is served"
        );
    }
}
