//! The package store (ADR-0028, reorganised by ADR-0028): the Server's software artifacts,
//! organised as **Sets**. A Set is identified by *(name, Agent type, version)*, may define a
//! Selector, and holds **one entry per Platform** (ADR-0028) — an uploaded artifact or a source
//!
//! Package *bodies* are opaque bytes: what a package contains and how it is applied is the Agent's
//! business (the specification forbids the Server abstracting over it). The Server's job is to
//! store, hash, offer, and serve — and to hand each Agent the artifact built for the machine it
//! runs on, never another one.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use opamp::proto::{
    AgentDescription, DownloadableFile, Header, Headers, PackageAvailable, PackageType,
    PackagesAvailable,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The operating system and architecture an artifact is built for (ADR-0028) — and the pair an
/// Agent reports about itself, so the two can be compared.
///
/// Both tokens are **canonical**: the semantic conventions' `os.type` and `host.arch` values, which
/// is what the Baseline points at ("keys/values are according to OpenTelemetry semantic
/// conventions") and what the release artifacts are named by. Older and foreign spellings are
/// folded onto them by [`Platform::new`], on the way in and on the way out alike.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Platform {
    pub os: String,
    pub arch: String,
}

impl Platform {
    /// Canonicalises a spelling into a Platform.
    ///
    /// Unknown tokens are **not** refused, only normalised in case and checked for shape: the fleet
    /// may run a system this table has never heard of, and refusing to serve it would be a worse
    /// failure than serving it under its own name. What is refused is a token that could not be
    /// half of a file name, since that is what an entry is stored as.
    ///
    /// # Errors
    /// Returns an error when either token is empty, longer than 16 characters, or carries anything
    /// but lowercase letters, digits, and `_`.
    pub fn new(os: &str, arch: &str) -> Result<Self, String> {
        Ok(Platform {
            os: token(os, "os", opamp::attributes::canonical_os)?,
            arch: token(arch, "arch", opamp::attributes::canonical_arch)?,
        })
    }

    /// The Platform an Agent reports, from the two attributes the Baseline names for it: `os.type`
    /// and `host.arch`. `None` when it reports neither — such an Agent fits no artifact, and is
    /// offered none rather than being guessed at (ADR-0028).
    ///
    /// The reported values go through the same canonicalisation as an uploaded one, which is what
    /// makes a Collector reporting `amd64` and a Supervisor reporting `x86_64` the same machine.
    pub fn reported(description: Option<&AgentDescription>) -> Option<Self> {
        let description = description?;
        // Non-identifying first: that is where an Agent reports its platform, and an identifying
        // copy is the fallback rather than the answer.
        let attribute = |key: &str| {
            opamp::attributes::string_value(&description.non_identifying_attributes, key).or_else(
                || opamp::attributes::string_value(&description.identifying_attributes, key),
            )
        };
        Platform::new(
            attribute(opamp::attributes::OS_TYPE)?,
            attribute(opamp::attributes::HOST_ARCH)?,
        )
        .ok()
    }

    /// How this Platform is written in a file name and a query: `linux-amd64`.
    fn tag(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

fn token(raw: &str, what: &str, canonicalise: fn(&str) -> &str) -> Result<String, String> {
    let lowered = raw.trim().to_ascii_lowercase();
    // The spelling table is the Client's too (ADR-0009): what an Agent reports and what an artifact
    // is stored under have to fold onto the same token, or the offer misses.
    let canonical = canonicalise(&lowered).to_string();
    if canonical.is_empty() || canonical.len() > 16 {
        return Err(format!("{what} {raw:?} must be 1–16 characters"));
    }
    if !canonical
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(format!(
            "{what} {raw:?} may hold only lowercase letters, digits, and '_'"
        ));
    }
    Ok(canonical)
}

/// A Set's version or Agent type as it may appear in its identity (ADR-0028): a bounded token that
/// embeds losslessly in file names and URLs. `@` is excluded so the Set directory name —
/// `<name>@<version>@<type>` — parses back unambiguously, exactly the trick ADR-0028 played for
/// variants; path separators are excluded because the value becomes half a directory name.
pub fn validate_identity_token(value: &str, what: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 64 {
        return Err(format!("the {what} must be 1–64 characters"));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(format!(
            "the {what} {value:?} may hold only letters, digits, '.', '_', '+', and '-'"
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub version: String,
}

        validate_identity_token(version, "version")?;
            version: version.to_string(),
        })
    }

    fn dir_name(&self) -> String {
    }
}

    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    }
}

///
#[derive(Clone)]
    pub entries: BTreeMap<Platform, Entry>,
}

/// One platform's artifact of a Set: **either** an uploaded file **or** a source reference, with
#[derive(Clone)]
pub struct Entry {
    pub platform: Platform,
    /// SHA-256 of the artifact bytes: computed here for an upload, the operator's word (verified
    /// by every Agent) for a source reference (ADR-0028).
    pub content_hash: Vec<u8>,
    /// The artifact's size in bytes; zero for a referenced one, whose bytes this Server never
    /// holds.
    pub size: u64,
    /// Where the artifact lives when it is **not** here (ADR-0028). `None` is an uploaded entry,
    /// whose bytes this Server holds and serves; `Some` is a reference, offered to Agents as the
    /// address it names — the Server never downloads it and has nothing to serve.
    pub source: Option<Source>,
}

/// An artifact that lives somewhere else (ADR-0028): the address Agents fetch it from, and what
/// they must send to be allowed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub url: String,
    /// Sent with the download — a token for a private source. Two exposures the operator accepts by
    /// using one: it is stored **in cleartext** in the package store (owner-only on disk, but not
    /// encrypted), and it is delivered to **every** Agent the Set targets. Prefer a
    /// narrowly-scoped, rotatable token over a long-lived credential.
    pub headers: BTreeMap<String, String>,
}

    /// The per-package hash the Agent compares to decide whether to download: over the fields that
    /// identify the offer (type, version) and the content. Framed length-prefixed so no boundary
    /// is ambiguous. The Platform needs no place in it — two platforms' artifacts differ in their
    /// content hash by construction.
    fn package_hash(&self, entry: &Entry) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update((self.id.version.len() as u64).to_le_bytes());
        hasher.update(self.id.version.as_bytes());
        hasher.update(&entry.content_hash);
        hasher.finalize().to_vec()
    }

    /// One entry as a wire `PackageAvailable`.
    ///
    /// An uploaded artifact is offered from this Server: `download_base` prefixes the artifact
    /// endpoint, and an empty prefix yields a path the Agent resolves against its own OpAMP
    /// endpoint. A **referenced** artifact is offered as the address it names, with whatever
    /// headers the operator gave — the Baseline's Download Server "may be on the same host as the
    /// OpAMP Server or a different host", and this is that other host (ADR-0028).
    fn to_available(
        &self,
        entry: &Entry,
        download_base: &str,
        headers: Option<Headers>,
    ) -> PackageAvailable {
        let file = match &entry.source {
            Some(source) => DownloadableFile {
                download_url: source.url.clone(),
                content_hash: entry.content_hash.clone(),
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
                download_url: format!(
                ),
                content_hash: entry.content_hash.clone(),
                headers,
            },
        };
        PackageAvailable {
            version: self.id.version.clone(),
            file: Some(file),
            hash: self.package_hash(entry),
        }
    }
}

/// One Set as the REST API lists it (ADR-0028): its identity, whom it targets, and what it holds
/// for each platform — never the artifact bytes.
    pub version: String,
    /// One entry per Platform, in platform order.
    pub entries: Vec<EntrySummary>,
}

/// One entry of a Set as the REST API shows it.
pub struct EntrySummary {
    pub os: String,
    pub arch: String,
    pub size: u64,
    /// The address an Agent fetches this from when the Server does not hold it (ADR-0028).
    pub source_url: Option<String>,
}

            version: set.id.version.clone(),
            entries: set
                .entries
                .values()
                .map(|entry| EntrySummary {
                    os: entry.platform.os.clone(),
                    arch: entry.platform.arch.clone(),
                    size: entry.size,
                    source_url: entry.source.as_ref().map(|s| s.url.clone()),
                })
                .collect(),
        }
    }
}

/// what ADR-0028 kept secretly (other versions), this store keeps openly, as more Sets.
#[derive(Serialize, Deserialize)]
    version: String,
    #[serde(default)]
    entries: Vec<EntryMeta>,
}

/// beside it.
#[derive(Serialize, Deserialize)]
struct EntryMeta {
    os: String,
    arch: String,
    content_hash_hex: String,
    /// The source of a referenced entry (ADR-0028); absent for an uploaded one.
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

            version: set.id.version.clone(),
            entries: set.entries.values().map(EntryMeta::of).collect(),
        }
    }
}

/// The persistent package store (ADR-0028): one directory per Set under `packages_dir`, holding
/// map is what the control loop reads.
pub struct PackageStore {
    dir: PathBuf,
}

impl PackageStore {
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        // Owner-only: a referenced entry's metadata carries the private source's headers — a
        // bearer token (ADR-0028) — so the store must not be readable by other local users on the
        // Server host. The metadata files are also written 0600 (see `write_atomic`).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
        }
        let mut sets = BTreeMap::new();
        let listing =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in listing {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
                .path();
            if !path.is_dir() {
                continue;
            }
            if !meta_path.exists() {
            }
            let text = std::fs::read_to_string(&meta_path)
                .map_err(|e| format!("cannot read {}: {e}", meta_path.display()))?;
                .map_err(|e| format!("cannot parse {}: {e}", meta_path.display()))?;
                .map_err(|e| format!("invalid identity in {}: {e}", meta_path.display()))?;
            // The directory name is derived from the identity; a mismatch means the artifacts
            // will not be found where the store looks for them, so it is refused by name.
            if path.file_name().and_then(|n| n.to_str()) != Some(id.dir_name().as_str()) {
                return Err(format!(
                    path.display(),
                    id.dir_name()
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
                // verified by every Agent that downloads it (ADR-0028).
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
        }
        Ok(PackageStore {
            dir,
            sets: RwLock::new(sets),
        })
    }

        self.dir.join(id.dir_name())
    }

    /// Every Set, in identity order — the REST list view; never the artifact bytes.
        self.sets
            .read()
            .expect("sets lock")
            .values()
            .collect()
    }

    /// One stored Set as the REST API presents it; `None` when no such Set exists.
        self.sets
            .read()
            .expect("sets lock")
            .get(id)
    }

    /// Where one uploaded artifact lives, for the download endpoint to stream from. `None` when no
    /// Set of that identity holds one for that Platform, or holds it as a reference.
        self.sets
            .read()
            .expect("sets lock")
            .get(id)?
            .entries
            .get(platform)
            // A referenced artifact is not served from here; the Agents were given its address.
            .filter(|entry| entry.source.is_none())
            .map(|_| self.set_dir(id).join(format!("{}.bin", platform.tag())))
    }

    /// `true` when the store holds no Set — the Server then leaves `OffersPackages` undeclared.
    pub fn is_empty(&self) -> bool {
        self.sets.read().expect("sets lock").is_empty()
    }

    /// The total bytes of stored artifacts: every `.bin` in every Set's directory. The in-flight
    /// `.upload` staging file is deliberately not counted — it is not yet an artifact, and the
    /// per-upload limit already bounds it.
    ///
    /// A best-effort walk of the directory: a file racing deletion simply is not counted, which is
    /// the safe direction for a ceiling that gates *new* uploads.
    pub fn total_bytes(&self) -> u64 {
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

        let mut sets = self.sets.write().expect("sets lock");
        }
            id: id.clone(),
            entries: BTreeMap::new(),
        };
        std::fs::create_dir_all(self.set_dir(id))
            .map_err(|e| format!("cannot create {}: {e}", self.set_dir(id).display()))?;
        self.write_meta(id, &meta)?;
        sets.insert(id.clone(), set);
        Ok(())
    }

    /// Where an upload for one entry is streamed before it becomes an artifact. In the Set's own
    /// directory, so [`put_staged`](Self::put_staged) can move it into place with a rename — and
    /// named per Platform, so uploading a release's five artifacts at once cannot have them
    /// overwrite each other while they are still in flight.
    ///
    /// # Errors
        self.writable(id)?;
        Ok(self.set_dir(id).join(format!("{}.upload", platform.tag())))
    }

        let sets = self.sets.read().expect("sets lock");
        Ok(())
    }

    /// Turns a streamed upload into an entry: hashed by streaming, moved into place with a rename,
    /// then visible to the control loop. The artifact never passes through memory — an agent
    /// binary is far too big to buffer twice just to store it once.
    ///
    /// The staged file is consumed on success and removed on failure, so a rejected upload leaves
    /// nothing behind.
    pub fn put_staged(
        &self,
        platform: &Platform,
        staged: &Path,
    ) -> Result<(), String> {
        if result.is_err() {
            let _ = std::fs::remove_file(staged);
        }
        result
    }

    fn store_staged(
        &self,
        platform: &Platform,
        staged: &Path,
    ) -> Result<(), String> {
        self.writable(id)?;
        let (size, content_hash) = hash_file(staged)?;
        if size == 0 {
            return Err("the package artifact is empty; refusing to distribute it".to_string());
        }
        let artifact = self.set_dir(id).join(format!("{}.bin", platform.tag()));
        std::fs::rename(staged, &artifact)
            .map_err(|e| format!("cannot persist {}: {e}", artifact.display()))?;
        self.put_entry_record(
            id,
            Entry {
                platform: platform.clone(),
                content_hash,
                size,
                source: None,
            },
        )
    }

    /// Creates or replaces one entry from bytes already in hand — the shape the tests and any
    /// small artifact use. A real upload takes [`put_staged`](Self::put_staged) instead.
    pub fn put_entry(
        &self,
        platform: &Platform,
        artifact: Vec<u8>,
    ) -> Result<(), String> {
        self.writable(id)?;
        if artifact.is_empty() {
            return Err("the package artifact is empty; refusing to distribute it".to_string());
        }
        let content_hash = Sha256::digest(&artifact).to_vec();
        let size = artifact.len() as u64;
        let path = self.set_dir(id).join(format!("{}.bin", platform.tag()));
        std::fs::write(&path, &artifact)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        self.put_entry_record(
            id,
            Entry {
                platform: platform.clone(),
                content_hash,
                size,
                source: None,
            },
        )
    }

    /// Points one entry at a file that lives somewhere else (ADR-0028): no bytes are stored or
    /// fetched, and Agents are given `url` — with `headers`, when the source needs them — plus the
    /// `content_hash` the operator supplied, which is the only thing that will check what they
    /// receive.
    pub fn set_entry_source(
        &self,
        platform: &Platform,
        content_hash: Vec<u8>,
        source: Source,
    ) -> Result<(), String> {
        self.writable(id)?;
        if content_hash.len() != 32 {
            return Err(
                "the content hash must be a SHA-256: 64 hex characters, as published in a \
                 release's checksums file"
                    .to_string(),
            );
        }
        if !source.url.starts_with("http://") && !source.url.starts_with("https://") {
            return Err(format!(
                "the source url {:?} must start with http:// or https://",
                source.url
            ));
        }
        // Bytes this Server was holding are no longer what the fleet gets; the reference replaces
        // them wholesale. Another version is another Set — nothing is remembered here (ADR-0028).
        let displaced = self.set_dir(id).join(format!("{}.bin", platform.tag()));
        if displaced.exists() {
            std::fs::remove_file(&displaced)
                .map_err(|e| format!("cannot delete {}: {e}", displaced.display()))?;
        }
        self.put_entry_record(
            id,
            Entry {
                platform: platform.clone(),
                content_hash,
                size: 0,
                source: Some(source),
            },
        )
    }

    /// converges on. Replacing the entry for a Platform the Set already holds is what "no
    /// duplicate entries" means in a map: the combination stays unique by construction.
        let mut sets = self.sets.write().expect("sets lock");
        let set = sets
            .get_mut(id)
            .ok_or_else(|| format!("no package set {id}"))?;
        set.entries.insert(entry.platform.clone(), entry);
        self.write_meta(id, &meta)
    }

        let mut sets = self.sets.write().expect("sets lock");
        let Some(set) = sets.get_mut(id) else {
            return Ok(false);
        };
        if set.entries.remove(platform).is_none() {
            return Ok(false);
        }
        let artifact = self.set_dir(id).join(format!("{}.bin", platform.tag()));
        if artifact.exists() {
            std::fs::remove_file(&artifact)
                .map_err(|e| format!("cannot delete {}: {e}", artifact.display()))?;
        }
        self.write_meta(id, &meta)?;
        Ok(true)
    }

    /// Deletes a whole Set — entries, artifacts, and metadata; `Ok(false)` when none of that
        let mut sets = self.sets.write().expect("sets lock");
        if sets.remove(id).is_none() {
            return Ok(false);
        }
        let dir = self.set_dir(id);
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("cannot delete {}: {e}", dir.display()))?;
        Ok(true)
    }

        &self,
        description: Option<&AgentDescription>,
        download_base: &str,
        headers: Option<Headers>,
        let sets = self.sets.read().expect("sets lock");
    }

        description: Option<&AgentDescription>,
        let sets = self.sets.read().expect("sets lock");
        description: Option<&AgentDescription>,
        let sets = self.sets.read().expect("sets lock");
        description: Option<&AgentDescription>,
        let sets = self.sets.read().expect("sets lock");
        let set = sets.get(id).ok_or_else(|| format!("no package set {id}"))?;
            return Err(format!(
                "set {id} holds no entries — a set contains one or more entries before it can be \
        }
        let dir = self.set_dir(id);
        // Metadata can carry a private source's headers (a bearer token, ADR-0028), so it is
        // written owner-only — the mode is set in the open call so the token is never briefly
        // world-readable, and the rename onto `path` carries the mode with it.
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temp)
                .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
            file.write_all(bytes)
                .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&temp, bytes)
                .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        }
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

    description: Option<&AgentDescription>,
    description: Option<&AgentDescription>,
    opamp::attributes::string_value(
        &description?.identifying_attributes,
fn resolve<'a>(
    description: Option<&AgentDescription>,
}

/// `None` for an Agent that has not described itself or reports no type, which fits no Set
/// (ADR-0028). An empty value is `None` too: it is not a type.
    opamp::attributes::string_value(
        &description?.identifying_attributes,
        opamp::attributes::SERVICE_NAME,
    )
    let mut hasher = Sha256::new();
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::proto::{any_value, AnyValue, KeyValue};

    fn linux() -> Platform {
        Platform::new("linux", "amd64").expect("platform")
    }

    fn windows() -> Platform {
        Platform::new("windows", "amd64").expect("platform")
    }

    }

    /// An Agent description reporting a platform and a type, plus whatever else a Selector
    /// should see.
    fn agent(os: &str, arch: &str, extra: &[(&str, &str)]) -> AgentDescription {
        let attr = |key: &str, value: &str| KeyValue {
            key: key.to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(value.to_string())),
            }),
        };
        AgentDescription {
            identifying_attributes: vec![attr("service.name", "otelcol")],
            non_identifying_attributes: [("os.type", os), ("host.arch", arch)]
                .iter()
                .map(|(k, v)| attr(k, v))
                .chain(extra.iter().map(|(k, v)| attr(k, v)))
                .collect(),
        }
    }

        let id = id(name, version);
            .expect("entry");
        id
            .map(|offer| {
                    .packages
                    .iter()
                    .map(|(name, p)| (name.clone(), p.version.clone()))
            })
            .unwrap_or_default()
    }

    /// ADR-0028: the identity is the triple, entries are per platform, and the whole Set —
    #[test]
    fn a_set_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
            let set = id("otelcol", "1.2.3");
            store
                .expect("linux entry");
            store
                .set_entry_source(
                    &set,
                    &windows(),
                    vec![0u8; 32],
                    Source {
                        url: "https://example.com/w.7z".into(),
                        headers: BTreeMap::new(),
                    },
                .expect("windows entry");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        let summary = store.summary(&id("otelcol", "1.2.3")).expect("summary");
        assert_eq!(summary.entries.len(), 2);
        assert_eq!(summary.entries[0].os, "linux");
            summary.entries[1].source_url.as_deref(),
            Some("https://example.com/w.7z")
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
            [("otelcol".to_string(), "1.0.0".to_string())],
            [("otelcol".to_string(), "1.0.0".to_string())]
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
            "no platform and no type fits nothing"
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        assert_eq!(
    }

    /// Fit before aim (ADR-0028): an entry for another platform, or a Set for another
    /// Agent type, is never a candidate — and an Agent reporting neither fits nothing.
    #[test]
    fn fit_is_mandatory_platform_and_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        store
            .expect("entry");

        assert_eq!(
            [("otelcol".to_string(), "1.0.0".to_string())],
            "the promtail set fits another type and is not a candidate"
        );
            "no platform and no type fits nothing"
        );
    }

    /// Both sides of the platform comparison are canonicalised (ADR-0028): an artifact uploaded
    /// as `macos`/`x86_64` reaches an Agent reporting `darwin`/`amd64`.
    #[test]
    fn both_sides_of_the_comparison_are_canonicalised() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let set = id("otelcol", "1.0.0");
        let mac = Platform::new("macos", "x86_64").expect("canonicalised");
        assert_eq!((mac.os.as_str(), mac.arch.as_str()), ("darwin", "amd64"));
        assert_eq!(
            [("otelcol".to_string(), "1.0.0".to_string())]
        );
            [("otelcol".to_string(), "1.0.0".to_string())]
        );
    }

    /// The offered download URL names the whole identity, so two versions of one name never serve
    /// each other's bytes.
    #[test]
    fn the_offer_carries_a_download_url_naming_the_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let offer = store
                Some(&agent("linux", "amd64", &[])),
                "https://fleet.example",
                None,
            )
            .expect("an offer");
        let url = &offer.packages["otelcol"]
            .file
            .as_ref()
            .expect("file")
            .download_url;
        assert_eq!(
            url,
        );
    }

    #[test]
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        assert!(!before.is_empty());
        assert!(store
            .is_empty());
        assert!(store
    }

    /// Deleting an entry frees its artifact; deleting the Set takes the directory with it.
    #[test]
    fn deletion_frees_entries_and_sets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let set = id("otelcol", "1.0.0");
        store
            .expect("entry");
        assert!(store.total_bytes() > 0);
        assert!(store.delete_entry(&set, &linux()).expect("delete entry"));
        assert_eq!(store.total_bytes(), 0);
        assert!(!store.delete_entry(&set, &linux()).expect("gone already"));
        assert!(store.delete_set(&set).expect("delete set"));
        assert!(store.summary(&set).is_none());
        assert!(!dir.path().join(set.to_string()).exists());
        assert!(!store.delete_set(&set).expect("gone already"));
    }

    /// A corrupt artifact fails the reopen loudly — a corrupt distribution artifact must never
    /// ship (ADR-0009's principle).
    #[test]
    fn a_corrupt_artifact_fails_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let set = id("otelcol", "1.0.0");
        {
            let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
            store
                .expect("entry");
        }
        std::fs::write(
            dir.path().join(set.to_string()).join("linux-amd64.bin"),
            b"tampered",
        )
        .expect("tamper");
        let err = PackageStore::open(dir.path().to_path_buf())
            .map(|_| ())
            .expect_err("must refuse");
        assert!(err.contains("does not match"), "{err}");
    }

    /// The store directory and each Set's metadata are owner-only (ADR-0028): a referenced
    /// source's headers may carry a token.
    #[cfg(unix)]
    #[test]
    fn the_store_and_its_metadata_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let set = id("otelcol", "1.0.0");
        assert_eq!(
            std::fs::metadata(dir.path())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("otelcol.json"),
        )
        assert!(
        );

        let dir = tempfile::tempdir().expect("tempdir");
            .map(|_| ())
        let dir = tempfile::tempdir().expect("tempdir");
    }

    /// The identity grammar keeps the triple a safe directory name and an unambiguous parse:
    /// `@` and path separators are refused.
    #[test]
    fn identity_tokens_are_bounded() {
    }
}
