//! The package store (ADR-0015, reorganised by ADR-0052): the Server's software artifacts,
//! organised as **Sets**. A Set is identified by *(name, Agent type, version)*, may define a
//! Selector, and holds **one entry per Platform** (ADR-0031) — an uploaded artifact or a source
//! reference (ADR-0018), with the SHA-256 content hash and an optional Ed25519 signature. A
//! saved Set reaches nobody by itself (ADR-0061): what an Agent is offered is composed from the
//! per-Agent assignments the operator's rollout acts wrote; [`resolve`] only computes the
//! **candidates** such an act would release. Since ADR-0076 a Set reaches an Agent only as an
//! **upgrade**: what the Agent reports as installed is the fourth matching test, beside type,
//! platform and Selector. The immutability of an assigned Set's entries is enforced by the fleet,
//! which knows the assignments.
//!
//! Package *bodies* are opaque bytes: what a package contains and how it is applied is the Agent's
//! business (the specification forbids the Server abstracting over it). The Server's job is to
//! store, hash, offer, and serve — and to hand each Agent the artifact built for the machine it
//! runs on, never another one.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::deployments::Deployment;
use opamp::proto::{
    AgentDescription, DownloadableFile, Header, Headers, PackageAvailable, PackageType,
    PackagesAvailable,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The operating system and architecture an artifact is built for (ADR-0031) — and the pair an
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
    /// offered none rather than being guessed at (ADR-0031).
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
    // The spelling table is the Client's too (ADR-0044): what an Agent reports and what an artifact
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

/// A Set's version or Agent type as it may appear in its identity (ADR-0052): a bounded token that
/// embeds losslessly in file names and URLs. `@` is excluded so the Set directory name —
/// `<name>@<version>@<type>` — parses back unambiguously, exactly the trick ADR-0031 played for
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

/// A Package's identity (ADR-0095): the Agent type it is built for and its version, stated at
/// creation and never edited. A new version is a **new Package**, and the type is as constitutive
/// of "what is this artifact" as the version — an attribute would be editable, and retyping stored
/// bytes to another kind of Agent is exactly the mistake an immutable identity forecloses.
///
/// There is no name beside these two. What a name would have added is a second identity for a
/// thing that already has one, and the only thing it could express — two Packages of one Agent
/// type at one version — is the ambiguity resolution used to have to rank its way out of.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageId {
    /// The Agent type this Package is built for, matched **raw** against the `service.name` an
    /// Agent reports (ADR-0034) — there is no canonical set of Agent types to normalise against.
    /// It is also the **wire name**: the `PackagesAvailable` map key and the key an Agent reports
    /// its `PackageStatuses` under, which is why it must not carry the version.
    pub agent_type: String,
    /// The version every entry of this Package shares. A release is one version across its
    /// platforms; the Package is the object that *is* that release.
    pub version: String,
}

/// The subdirectory of `packages_dir` the Deployments live in (ADR-0096) — skipped by the package
/// loader, which owns every *other* entry in that directory.
pub const DEPLOYMENTS_DIR: &str = "deployments";

impl PackageId {
    /// Validates both parts; the one gate every write goes through.
    pub fn new(agent_type: &str, version: &str) -> Result<Self, String> {
        validate_identity_token(agent_type, "agent type")?;
        validate_identity_token(version, "version")?;
        Ok(PackageId {
            agent_type: agent_type.to_string(),
            version: version.to_string(),
        })
    }

    /// What an operator reads: the Agent type **and** its version. Never the wire name — that one
    /// is stable across versions on purpose (see [`PackageId::agent_type`]), and using it here
    /// would make every version of a Package look like the same row.
    pub fn display_name(&self) -> String {
        format!("{} {}", self.agent_type, self.version)
    }

    /// The Package's directory name: `<agent_type>@<version>`. Neither token admits an `@`, so the
    /// pair parses back unambiguously — and stays readable in a directory listing, which an opaque
    /// hash would not be.
    fn dir_name(&self) -> String {
        format!("{}@{}", self.agent_type, self.version)
    }

    /// Parses the `<agent_type>@<version>` form the [`Display`] impl and the Package directory use
    /// — also the persisted shape of a package assignment (ADR-0061).
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut parts = text.split('@');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(agent_type), Some(version), None) => PackageId::new(agent_type, version),
            _ => Err(format!(
                "{text:?} is not an <agent type>@<version> package identity"
            )),
        }
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.agent_type, self.version)
    }
}

/// A stored Package (ADR-0095): its identity, and one entry per Platform. Nothing else.
///
/// **The Platform fits, the Deployment aims** — the split ADR-0031 named, with the aiming half
/// moved out (ADR-0096). What a Package is has nothing to say about who gets it, which is why
/// there is no Selector here and no kind flag: an Agent's Deployment holds one Package for its
/// type, and that is the whole of the decision.
///
/// Artifacts stay on disk (`<package>/<os>-<arch>.bin`) and are streamed to whoever asks — a
/// program weighs hundreds of megabytes, and a fleet server holding every one of them in memory,
/// plus a copy per download, is the shape this deliberately avoids.
#[derive(Clone)]
pub struct Package {
    pub id: PackageId,
    /// One entry per Platform — a map, so a duplicate for the same combination is unrepresentable.
    /// A Package may sit empty while it is being assembled; rolling one out empty is refused.
    pub entries: BTreeMap<Platform, Entry>,
}

/// One platform's artifact of a Set: **either** an uploaded file **or** a source reference, with
/// the hash that identifies what an Agent installs. The **signature** is not here: what an
/// operator signs off on is a release to a set of machines, so it belongs to the Deployment that
/// offers these bytes (ADR-0096 point 7), and the same artifact in two channels is signed in each.
#[derive(Clone)]
pub struct Entry {
    pub platform: Platform,
    /// SHA-256 of the artifact bytes: computed here for an upload, the operator's word (verified
    /// by every Agent) for a source reference (ADR-0018).
    pub content_hash: Vec<u8>,
    /// The artifact's size in bytes; zero for a referenced one, whose bytes this Server never
    /// holds.
    pub size: u64,
    /// Where the artifact lives when it is **not** here (ADR-0018). `None` is an uploaded entry,
    /// whose bytes this Server holds and serves; `Some` is a reference, offered to Agents as the
    /// address it names — the Server never downloads it and has nothing to serve.
    pub source: Option<Source>,
}

/// An artifact that lives somewhere else (ADR-0018): the address Agents fetch it from, and what
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

impl Package {
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
    /// OpAMP Server or a different host", and this is that other host (ADR-0018).
    /// One entry as a wire `PackageAvailable`. `signature` is the Deployment's, for these exact
    /// bytes on this exact platform (ADR-0096 point 7) — empty where the channel holds none, which is
    /// a policy the Server reports rather than refuses (ADR-0015).
    fn to_available(
        &self,
        entry: &Entry,
        signature: &[u8],
        download_base: &str,
        headers: Option<Headers>,
    ) -> PackageAvailable {
        let file = match &entry.source {
            Some(source) => DownloadableFile {
                download_url: source.url.clone(),
                content_hash: entry.content_hash.clone(),
                signature: signature.to_vec(),
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
                    "{download_base}/api/v1/packages/{}/{}/file?os={}&arch={}",
                    self.id.agent_type, self.id.version, entry.platform.os, entry.platform.arch
                ),
                content_hash: entry.content_hash.clone(),
                signature: signature.to_vec(),
                headers,
            },
        };
        PackageAvailable {
            // Always top-level. An Agent has one binary to replace, its Deployment holds one
            // Package for its type, and no Client this project ships installs an addon — so the
            // kind is structural rather than a flag anyone could set (ADR-0095 point 4).
            r#type: PackageType::TopLevel as i32,
            version: self.id.version.clone(),
            file: Some(file),
            hash: self.package_hash(entry),
        }
    }
}

/// One Set as the REST API lists it (ADR-0052): its identity, whom it targets, and what it holds
/// for each platform — never the artifact bytes.
pub struct PackageSummary {
    /// The Agent type this Package is built for — its identity, and its wire name.
    pub agent_type: String,
    pub version: String,
    /// One entry per Platform, in platform order.
    pub entries: Vec<EntrySummary>,
}

/// One entry of a Set as the REST API shows it.
pub struct EntrySummary {
    pub os: String,
    pub arch: String,
    pub size: u64,
    /// The SHA-256 of the artifact, hex — **the exact value the Agent verifies against**.
    pub content_hash: String,
    /// The per-package hash this entry is offered under, hex — what an Agent echoes back once it
    /// is in sync, and what gates re-offering.
    pub package_hash: String,
    /// The address an Agent fetches this from when the Server does not hold it (ADR-0018).
    pub source_url: Option<String>,
}

impl PackageSummary {
    fn of(set: &Package) -> Self {
        PackageSummary {
            agent_type: set.id.agent_type.clone(),
            version: set.id.version.clone(),
            entries: set
                .entries
                .values()
                .map(|entry| EntrySummary {
                    os: entry.platform.os.clone(),
                    arch: entry.platform.arch.clone(),
                    size: entry.size,
                    content_hash: hex::encode(&entry.content_hash),
                    package_hash: hex::encode(set.package_hash(entry)),
                    source_url: entry.source.as_ref().map(|s| s.url.clone()),
                })
                .collect(),
        }
    }
}

/// A Set as persisted: `<agent_type>@<version>/package.json`, entries inline. One document per Set —
/// what ADR-0019 kept secretly (other versions), this store keeps openly, as more Sets.
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
    /// The source of a referenced entry (ADR-0018); absent for an uploaded one.
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

/// The persistent package store (ADR-0052): one directory per Set under `packages_dir`, holding
/// `package.json` and one `<os>-<arch>.bin` per uploaded entry, restored at startup. The in-memory
/// map is what the control loop reads.
pub struct PackageStore {
    dir: PathBuf,
    sets: RwLock<BTreeMap<PackageId, Package>>,
}

impl PackageStore {
    /// Opens the store, creating the directory and loading every persisted Set. A metadata or
    /// artifact file that cannot be read, does not parse, or whose artifact no longer matches its
    /// recorded hash is a startup error — a corrupt distribution artifact must never ship. There
    /// is **no migration**: a directory in a shape this Server does not write is named in that
    /// error rather than skipped, so a store left over from an older layout is reported instead of
    /// quietly appearing empty.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        // Owner-only: a referenced entry's metadata carries the private source's headers — a
        // bearer token (ADR-0018) — so the store must not be readable by other local users on the
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
                // A Set is a directory. A loose file at the top level is what the pre-ADR-0052
                // store wrote (`<name>.json`, `<name>@<os>-<arch>.json`/`.bin`), and there is no
                // reader for it any more — so it is named rather than skipped. Skipping would turn
                // an old store into one that merely looks empty, which is the failure an operator
                // cannot see (ADR-0008: loud, never silently ignored).
                return Err(format!(
                    "{} is not a Package directory — this Server reads no other package store \
                     layout. \
                     Move it aside or delete it; nothing here will be migrated.",
                    path.display()
                ));
            }
            // The one directory here that is deliberately not a Package: the channel store the
            // Deployments live in (ADR-0096), armed by this same `packages_dir`.
            if path.file_name().and_then(|n| n.to_str()) == Some(DEPLOYMENTS_DIR) {
                continue;
            }
            let meta_path = path.join("package.json");
            if !meta_path.exists() {
                // Skipping is the dangerous half. A store written by an older layout —
                // `<name>@<version>@<type>/set.json` — would open *successfully and empty*: no
                // offer, no error, and a package list an operator reads as "nothing uploaded yet"
                // (ADR-0095 point 5). So the directory is named instead.
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
            if path.file_name().and_then(|n| n.to_str()) != Some(id.dir_name().as_str()) {
                return Err(format!(
                    "{} does not match the identity {} its package.json states — rename \
                     the directory or fix the file",
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
                // verified by every Agent that downloads it (ADR-0018).
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
            sets.insert(id.clone(), Package { id, entries });
        }
        Ok(PackageStore {
            dir,
            sets: RwLock::new(sets),
        })
    }

    /// Where this store keeps its Packages — the directory the Deployments sit beneath
    /// (ADR-0096), so the two are armed by one configuration key.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn set_dir(&self, id: &PackageId) -> PathBuf {
        self.dir.join(id.dir_name())
    }

    /// Every Set, in identity order — the REST list view; never the artifact bytes.
    pub fn list(&self) -> Vec<PackageSummary> {
        self.sets
            .read()
            .expect("sets lock")
            .values()
            .map(PackageSummary::of)
            .collect()
    }

    /// One stored Set as the REST API presents it; `None` when no such Set exists.
    pub fn summary(&self, id: &PackageId) -> Option<PackageSummary> {
        self.sets
            .read()
            .expect("sets lock")
            .get(id)
            .map(PackageSummary::of)
    }

    /// Where one uploaded artifact lives, for the download endpoint to stream from. `None` when no
    /// Set of that identity holds one for that Platform, or holds it as a reference.
    pub fn artifact_path(&self, id: &PackageId, platform: &Platform) -> Option<PathBuf> {
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

    /// Creates a Package. **Saving never distributes** (ADR-0061), and there is nothing to update:
    /// a Package is its identity and its entries, so creating one that exists is the same request
    /// arriving twice.
    pub fn create(&self, id: &PackageId) -> Result<(), String> {
        let mut sets = self.sets.write().expect("sets lock");
        if sets.contains_key(id) {
            return Ok(());
        }
        let set = Package {
            id: id.clone(),
            entries: BTreeMap::new(),
        };
        std::fs::create_dir_all(self.set_dir(id))
            .map_err(|e| format!("cannot create {}: {e}", self.set_dir(id).display()))?;
        let meta = serde_json::to_vec_pretty(&PackageMeta::of(&set)).expect("package serializes");
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
    /// Returns an error when no Set of that identity exists. The fleet refuses the upload before
    /// this when the Set is assigned to an Agent — an assigned Set's bytes are immutable
    /// (ADR-0061), so there would be nothing an upload could become.
    pub fn staging_path(&self, id: &PackageId, platform: &Platform) -> Result<PathBuf, String> {
        self.writable(id)?;
        Ok(self.set_dir(id).join(format!("{}.upload", platform.tag())))
    }

    /// The gate every entry write passes: the Set must exist. The immutability of an assigned
    /// Set (ADR-0061) is the fleet's to enforce — only it knows the assignments.
    fn writable(&self, id: &PackageId) -> Result<(), String> {
        let sets = self.sets.read().expect("sets lock");
        sets.get(id).ok_or_else(|| format!("no package set {id}"))?;
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
        id: &PackageId,
        platform: &Platform,
        staged: &Path,
    ) -> Result<(), String> {
        let result = self.store_staged(id, platform, staged);
        if result.is_err() {
            let _ = std::fs::remove_file(staged);
        }
        result
    }

    fn store_staged(
        &self,
        id: &PackageId,
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
        id: &PackageId,
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

    /// Points one entry at a file that lives somewhere else (ADR-0018): no bytes are stored or
    /// fetched, and Agents are given `url` — with `headers`, when the source needs them — plus the
    /// `content_hash` the operator supplied, which is the only thing that will check what they
    /// receive.
    pub fn set_entry_source(
        &self,
        id: &PackageId,
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
        // them wholesale. Another version is another Set — nothing is remembered here (ADR-0052).
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

    /// Writes one entry into the Set's map and its `package.json` — the single path every entry write
    /// converges on. Replacing the entry for a Platform the Set already holds is what "no
    /// duplicate entries" means in a map: the combination stays unique by construction.
    fn put_entry_record(&self, id: &PackageId, entry: Entry) -> Result<(), String> {
        let mut sets = self.sets.write().expect("sets lock");
        let set = sets
            .get_mut(id)
            .ok_or_else(|| format!("no package set {id}"))?;
        set.entries.insert(entry.platform.clone(), entry);
        let meta = serde_json::to_vec_pretty(&PackageMeta::of(set)).expect("set serializes");
        self.write_meta(id, &meta)
    }

    /// Deletes one entry; `Ok(false)` when the Set or the entry does not exist. The fleet refuses
    /// this before calling here when the Set is assigned to an Agent (ADR-0061). The last entry
    /// taken away leaves an **empty Set**, kept: a Set being reassembled is a normal state, and
    /// deleting the Set is its own act.
    pub fn delete_entry(&self, id: &PackageId, platform: &Platform) -> Result<bool, String> {
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
        let meta = serde_json::to_vec_pretty(&PackageMeta::of(set)).expect("set serializes");
        self.write_meta(id, &meta)?;
        Ok(true)
    }

    /// Deletes a whole Set — entries, artifacts, and metadata; `Ok(false)` when none of that
    /// identity exists. The fleet removes every assignment that referenced it, which withdraws
    /// the offer; Agents that installed it keep running it (ADR-0017).
    pub fn delete_set(&self, id: &PackageId) -> Result<bool, String> {
        let mut sets = self.sets.write().expect("sets lock");
        if sets.remove(id).is_none() {
            return Ok(false);
        }
        let dir = self.set_dir(id);
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("cannot delete {}: {e}", dir.display()))?;
        Ok(true)
    }

    /// One Agent's offer, composed from its **assignments** (ADR-0061): for each assigned Set,
    /// the entry built for the platform the Agent reports, plus the `all_packages_hash` over that
    /// set (the Baseline's per-Agent aggregate). `None` when the Agent is assigned nothing it
    /// fits — it is offered nothing and keeps running what it runs. An assignment whose Set is
    /// gone composes nothing rather than failing: deletion removes assignments, so the case is a
    /// race, not a state.
    pub fn offer_for_assigned(
        &self,
        assigned: Option<&PackageId>,
        deployment: Option<&Deployment>,
        description: Option<&AgentDescription>,
        download_base: &str,
        headers: Option<Headers>,
    ) -> Option<PackagesAvailable> {
        let sets = self.sets.read().expect("sets lock");
        let (set, entry) = assigned_entry(&sets, assigned, description)?;
        // The signature is the *assigned* Deployment's, not whichever channel claims the Agent now:
        // an offer travels with what the act released (ADR-0096 point 7). A channel that holds none
        // offers the artifact unsigned, which a Client with a verification key refuses — that is
        // the operator's policy meeting their omission, and both ends report it.
        let signature = deployment
            .and_then(|d| d.signature(&set.id, &entry.platform))
            .unwrap_or_default();
        Some(PackagesAvailable {
            packages: [(
                set.id.agent_type.clone(),
                set.to_available(entry, signature, download_base, headers),
            )]
            .into(),
            all_packages_hash: aggregate_hash(set, entry),
        })
    }

    /// The aggregate hash over one Agent's assignments, to gate re-offering without building the
    /// whole message. Empty when the Agent is assigned nothing it fits — it is offered nothing,
    /// and has nothing to be in sync with.
    pub fn assigned_hash_for(
        &self,
        assigned: Option<&PackageId>,
        description: Option<&AgentDescription>,
    ) -> Vec<u8> {
        let sets = self.sets.read().expect("sets lock");
        match assigned_entry(&sets, assigned, description) {
            Some((set, entry)) => aggregate_hash(set, entry),
            None => Vec::new(),
        }
    }

    /// The Package a rollout act would release to this Agent — the **candidate** (ADR-0061),
    /// never an offer.
    ///
    /// One Deployment claims the Agent (ADR-0096); the Package it holds for the Agent's type has
    /// to fit its platform and be an **upgrade** over what the Agent reports (ADR-0083). `None`
    /// where any of those is missing — including the ordinary case of an Agent no channel claims yet.
    pub fn candidate(
        &self,
        deployment: Option<&Deployment>,
        description: Option<&AgentDescription>,
        installed: &InstalledVersions,
    ) -> Option<PackageId> {
        let sets = self.sets.read().expect("sets lock");
        resolve(&sets, deployment?, description, installed).map(|(set, _)| set.id.clone())
    }

    /// Whether an explicit rollout act may release this Package to this Agent: it must exist,
    /// hold an entry for the platform the Agent reports, be built for its type, and be an
    /// **upgrade** over what the Agent reports installed under that type (ADR-0083).
    ///
    /// Aim is **not** checked here any more — whom a Package reaches is its Deployment's business
    /// (ADR-0096), and the act names the Deployment, so the channel has already been decided by the
    /// time this is asked.
    ///
    /// Still **not** the version *ranking* of [`resolve`]: rolling out a Set older than a sibling
    /// the store also holds stays the operator's to make. What ADR-0076 forbids is aiming an act
    /// at an Agent it would move backwards, or not move at all — the count beside the button and
    /// the button itself now answer the same question.
    pub fn fits_agent(
        &self,
        id: &PackageId,
        description: Option<&AgentDescription>,
        installed: &InstalledVersions,
    ) -> Result<(), String> {
        let sets = self.sets.read().expect("sets lock");
        let set = sets.get(id).ok_or_else(|| format!("no package set {id}"))?;
        if set.entries.is_empty() {
            return Err(format!(
                "set {id} holds no entries — a set contains one or more entries before it can be \
                 rolled out"
            ));
        }
        let Some(platform) = Platform::reported(description) else {
            return Err(format!(
                "set {id} fits no platform this Agent reports — it reports none"
            ));
        };
        if reported_agent_type(description) != Some(set.id.agent_type.as_str()) {
            return Err(format!(
                "set {id} is built for Agent type {:?}, which this Agent does not report",
                set.id.agent_type
            ));
        }
        if !set.entries.contains_key(&platform) {
            return Err(format!(
                "set {id} holds no entry for {}-{}, which this Agent reports",
                platform.os, platform.arch
            ));
        }
        if !upgrades(set, installed, description) {
            // *Which* of the two versions decided is the operator's first question here (ADR-0083
            // point 8): the running one wherever it can be ordered, the claim only where it cannot.
            // A refusal naming a number without saying which of the two it was would read like the
            // wrong rule applied — and where both are reported and they disagree, saying that the
            // claim was not consulted is the whole explanation.
            let running = reported_service_version(description)
                .filter(|version| opamp::version::parse(version).is_some());
            return Err(match (running, claimed_version(set, installed)) {
                (Some(runs), Some(has)) => format!(
                    "set {id} is not an upgrade for this Agent, which runs {runs:?}; its package \
                     status claims {has:?} for package {:?}, which is not consulted while the \
                     Agent reports what it runs",
                    set.id.agent_type
                ),
                (Some(runs), None) => {
                    format!("set {id} is not an upgrade for this Agent, which runs {runs:?}")
                }
                (None, Some(has)) => format!(
                    "set {id} is not an upgrade for this Agent, which reports {has:?} installed \
                     for package {:?} and no version it runs that can be ordered",
                    set.id.agent_type
                ),
                (None, None) => {
                    format!("set {id} is not an upgrade for this Agent, which reports no version")
                }
            });
        }
        Ok(())
    }

    fn write_meta(&self, id: &PackageId, bytes: &[u8]) -> Result<(), String> {
        let dir = self.set_dir(id);
        let path = dir.join("package.json");
        let temp = dir.join("package.json.tmp");
        // Metadata can carry a private source's headers (a bearer token, ADR-0018), so it is
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

/// The entry one Agent is offered: its assignment, narrowed to the artifact built for the
/// platform it reports.
///
/// The platform still has to fit. An assignment pins *which* release the operator released; which
/// artifact of it this host takes is the machine's own answer, and a host reporting a platform the
/// release does not hold is offered nothing rather than something else.
fn assigned_entry<'a>(
    sets: &'a BTreeMap<PackageId, Package>,
    assigned: Option<&PackageId>,
    description: Option<&AgentDescription>,
) -> Option<(&'a Package, &'a Entry)> {
    let platform = Platform::reported(description)?;
    let set = sets.get(assigned?)?;
    let entry = set.entries.get(&platform)?;
    Some((set, entry))
}

/// What an Agent reports it has installed, per package name: `PackageStatuses.packages[name]
/// .agent_has_version` as the Agent last sent it (ADR-0015). A name that is absent — and a name
/// whose reported version is empty, which is how an Agent that has installed nothing reports a
/// package it was offered — means *nothing is installed under that name*.
pub type InstalledVersions = BTreeMap<String, String>;

/// The fourth matching test: a Set reaches an Agent only as an **upgrade** (ADR-0083 points 2 to 4).
///
/// An Agent reports up to two versions, and they can contradict each other. **What it runs decides**
/// — the reported `service.version`, a statement about the present. Where it is there and can be
/// ordered, the Set must be strictly greater than it and the claim is not read at all: not to admit
/// a Set the running version refuses, and not to refuse one it admits.
///
/// *What it claims* is the **package status** for this Set's name, derived from what an install once
/// wrote. That record outlives the binary it describes — a staged update that did not take, a host
/// reinstalled from an older artifact — so it cannot overrule the program's own statement, and it
/// does not get a veto over it either. The Baseline defines the field as *"the version of the package
/// that the Agent has"*, which a record naming a version the program denies running is not.
///
/// Where no `service.version` can be ordered — a program numbering itself `1.19` or `24.04.1`, or an
/// Agent reporting none at all — the claim is the whole test, exactly as ADR-0076 wrote it: strictly
/// greater to match, and a claim that cannot itself be ordered **refuses** outright. That is the safe
/// direction for a claim about that very package, and the Client's own
/// (`selfupdate::install_offer`): what cannot be ordered must not be installed over what is running.
///
/// An Agent that reports neither has nothing to be greater than: the first rollout, which matches.
fn upgrades(
    set: &Package,
    installed: &InstalledVersions,
    description: Option<&AgentDescription>,
) -> bool {
    use std::cmp::Ordering;
    let greater =
        |has: &str| opamp::version::precedence(&set.id.version, has) == Some(Ordering::Greater);
    // An unorderable `service.version` says nothing at all — it is dropped here rather than
    // refusing, so what remains is either a version to be greater than or no statement.
    let runs = reported_service_version(description)
        .filter(|running| opamp::version::parse(running).is_some());
    match runs {
        // The program's own word about the program, in both directions.
        Some(running) => greater(running),
        // No statement about the present: fall back to the record, ADR-0076 unchanged. An
        // unorderable claim refuses, which `greater` already does by yielding `None`.
        None => match claimed_version(set, installed) {
            Some(claimed) => greater(claimed),
            None => true,
        },
    }
}

/// What an Agent claims to have installed under this Set's name, if it claims anything: a package
/// status reported with an empty version is no claim (ADR-0076).
fn claimed_version<'a>(set: &Package, installed: &'a InstalledVersions) -> Option<&'a str> {
    installed
        .get(&set.id.agent_type)
        .map(String::as_str)
        .filter(|has| !has.is_empty())
}

/// The version an Agent reports as `service.version` — its program's own number, and since ADR-0079
/// what a Set is held against when the Agent reports no version for the package itself. Since
/// ADR-0081 it is also read beside a reported one, as what the Agent actually runs.
fn reported_service_version(description: Option<&AgentDescription>) -> Option<&str> {
    opamp::attributes::string_value(
        &description?.identifying_attributes,
        opamp::attributes::SERVICE_VERSION,
    )
    .filter(|version| !version.is_empty())
}

/// Whether a Package fits an Agent at all: built for the type it reports, and holding an entry
/// for the platform it reports. Both are mandatory, and neither has an "unknown, so anything goes"
/// case (ADR-0031, ADR-0034) — an Agent reporting neither fits nothing.
///
/// Aim is not here. Whom a Package reaches is its Deployment's business (ADR-0096).
fn fits(set: &Package, platform: &Platform, service_name: &str) -> bool {
    set.id.agent_type == service_name && set.entries.contains_key(platform)
}

/// What one Agent's Deployment would release to it, if an operator rolled out now.
///
/// Four tests, and every one of them is a hard gate: the Agent reports a platform and a type
/// (ADR-0031, ADR-0034), its Deployment holds a Package for that type (ADR-0096), that Package fits,
/// and it is an **upgrade** over what the Agent runs (ADR-0083).
///
/// There is no ranking left. The Deployment holds at most one Package per Agent type, so the
/// specificity comparison and the version tie-break ADR-0017 and ADR-0052 needed have nothing to
/// choose between — a state that used to be ambiguous is now one a write refuses to create.
fn resolve<'a>(
    sets: &'a BTreeMap<PackageId, Package>,
    deployment: &Deployment,
    description: Option<&AgentDescription>,
    installed: &InstalledVersions,
) -> Option<(&'a Package, &'a Entry)> {
    let platform = Platform::reported(description)?;
    let service_name = reported_agent_type(description)?;
    let set = sets.get(deployment.package_for(service_name)?)?;
    if !fits(set, &platform, service_name) || !upgrades(set, installed, description) {
        return None;
    }
    let entry = set
        .entries
        .get(&platform)
        .expect("the fit proved the entry");
    Some((set, entry))
}

/// The Agent type an Agent reports, as `service.name` (ADR-0033) — the identifying attribute the
/// Baseline reserves for "a reverse FQDN that uniquely identifies the Agent type".
///
/// `None` for an Agent that has not described itself or reports no type, which fits no Set
/// (ADR-0034). An empty value is `None` too: it is not a type.
pub fn reported_agent_type(description: Option<&AgentDescription>) -> Option<&str> {
    opamp::attributes::string_value(
        &description?.identifying_attributes,
        opamp::attributes::SERVICE_NAME,
    )
}

/// The aggregate over what one Agent is offered — its name and its content. One Package per
/// Agent now, so there is nothing to sort; the shape is kept because it is what the Baseline's
/// `all_packages_hash` is and what the Agent echoes back.
fn aggregate_hash(set: &Package, entry: &Entry) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update((set.id.agent_type.len() as u64).to_le_bytes());
    hasher.update(set.id.agent_type.as_bytes());
    hasher.update(set.package_hash(entry));
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

    fn id(agent_type: &str, version: &str) -> PackageId {
        PackageId::new(agent_type, version).expect("package id")
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

    /// An Agent that reports what its program is, as a Client does: `service.version`, identifying,
    /// beside the type (ADR-0033).
    fn running_agent(version: &str) -> AgentDescription {
        let mut description = agent("linux", "amd64", &[]);
        description.identifying_attributes.push(KeyValue {
            key: "service.version".to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(version.to_string())),
            }),
        });
        description
    }

    /// A Set with one uploaded linux entry — stored, which since ADR-0061 reaches nobody until
    /// an assignment names it.
    fn stored_set(store: &PackageStore, name: &str, version: &str, artifact: &[u8]) -> PackageId {
        let id = id(name, version);
        store.create(&id).expect("create");
        store
            .put_entry(&id, &linux(), artifact.to_vec())
            .expect("entry");
        id
    }

    /// What an Agent reports installed, as the record hands it to the store (ADR-0076).
    fn installed(versions: &[(&str, &str)]) -> InstalledVersions {
        versions
            .iter()
            .map(|(name, version)| ((*name).to_string(), (*version).to_string()))
            .collect()
    }

    /// A Deployment holding one Package and aiming at everything the tests describe. Aim lives
    /// there now (ADR-0096), so a store test that wants a candidate has to say which channel the
    /// Agent is in — which is the model, not scaffolding.
    fn channel(id: &PackageId) -> Deployment {
        Deployment {
            name: "stable".to_string(),
            selector: BTreeMap::from([("channel".to_string(), "stable".to_string())]),
            packages: BTreeMap::from([(id.agent_type.clone(), id.clone())]),
            signatures: BTreeMap::new(),
        }
    }

    /// The candidate a rollout act would release to an Agent that has installed nothing.
    fn candidates(store: &PackageStore, description: &AgentDescription) -> Vec<(String, String)> {
        candidates_for(store, description, &InstalledVersions::new())
    }

    /// The candidate a rollout act would release to this Agent, as `(type, version)` — for every
    /// Package the store holds, each read through a channel that offers exactly it. At most one
    /// survives per call; collecting them is how a test asks "which of these would reach it".
    fn candidates_for(
        store: &PackageStore,
        description: &AgentDescription,
        installed: &InstalledVersions,
    ) -> Vec<(String, String)> {
        let ids: Vec<PackageId> = store
            .sets
            .read()
            .expect("sets lock")
            .keys()
            .cloned()
            .collect();
        let mut names: Vec<(String, String)> = ids
            .iter()
            .filter_map(|id| store.candidate(Some(&channel(id)), Some(description), installed))
            .map(|id| (id.agent_type, id.version))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// What this Agent is offered, given its assignment, as `(type, version)`.
    fn offered(
        store: &PackageStore,
        assigned: Option<&PackageId>,
        description: &AgentDescription,
    ) -> Vec<(String, String)> {
        let held = assigned.map(channel);
        let deployment = held.as_ref();
        store
            .offer_for_assigned(assigned, deployment, Some(description), "", None)
            .map(|offer| {
                offer
                    .packages
                    .iter()
                    .map(|(name, p)| (name.clone(), p.version.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// ADR-0052: the identity is the triple, entries are per platform, and the whole Set —
    /// entries and selector — survives a reopen.
    #[test]
    fn a_set_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
            let set = id("otelcol", "1.2.3");
            store.create(&set).expect("create");
            store
                .put_entry(&set, &linux(), b"linux-bytes".to_vec())
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
                )
                .expect("windows entry");
        }
        let store = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        let summary = store.summary(&id("otelcol", "1.2.3")).expect("summary");
        assert_eq!(summary.entries.len(), 2);
        assert_eq!(summary.entries[0].os, "linux");
        assert_eq!(
            summary.entries[1].source_url.as_deref(),
            Some("https://example.com/w.7z")
        );
    }

    /// ADR-0061: a saved Set reaches nobody by itself. It is a visible candidate, and only an
    /// assignment — the operator's rollout act — composes an offer from it.
    #[test]
    fn a_saved_set_reaches_nobody_without_an_assignment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let set = stored_set(&store, "otelcol", "1.0.0", b"bytes");
        assert_eq!(
            candidates(&store, &agent("linux", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())],
            "the candidate is visible"
        );
        assert!(
            offered(&store, None, &agent("linux", "amd64", &[])).is_empty(),
            "no assignment, no offer"
        );
        assert_eq!(
            offered(&store, Some(&set), &agent("linux", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())]
        );
    }

    /// The gate an explicit rollout act runs (ADR-0061): the Package must hold entries and fit
    /// the Agent's type and platform. **Aim is no longer among them** — whom a Package reaches is
    /// its Deployment's, and the act names the channel. The version *ranking* stays out too: an Agent
    /// that has installed nothing takes the older Package as readily as the newer one.
    #[test]
    fn fits_agent_checks_fit_but_neither_aim_nor_the_ranking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let empty = id("otelcol", "0.9.0");
        store.create(&empty).expect("create");
        assert!(store
            .fits_agent(
                &empty,
                Some(&agent("linux", "amd64", &[])),
                &InstalledVersions::new()
            )
            .expect_err("empty refused")
            .contains("holds no entries"));

        let old = stored_set(&store, "otelcol", "1.0.0", b"v1");
        let new = stored_set(&store, "otelcol", "2.0.0", b"v2");
        let in_channel = agent("linux", "amd64", &[("channel", "canary")]);
        assert!(
            store
                .fits_agent(&old, Some(&in_channel), &InstalledVersions::new())
                .is_ok(),
            "the older Set fits an Agent that runs nothing yet"
        );
        assert!(store
            .fits_agent(&new, Some(&in_channel), &InstalledVersions::new())
            .is_ok());
        assert!(
            store
                .fits_agent(
                    &old,
                    Some(&agent("linux", "amd64", &[])),
                    &InstalledVersions::new()
                )
                .is_ok(),
            "an Agent outside any channel still *fits* this Package — whom it reaches is the \
             Deployment's question, and the act has already answered it by naming one (ADR-0096)"
        );
        assert!(store
            .fits_agent(
                &new,
                Some(&agent("windows", "amd64", &[])),
                &InstalledVersions::new()
            )
            .expect_err("wrong platform")
            .contains("no entry for"));
        assert!(
            store
                .fits_agent(
                    &new,
                    Some(&AgentDescription::default()),
                    &InstalledVersions::new()
                )
                .is_err(),
            "no platform and no type fits nothing"
        );
        assert!(store
            .fits_agent(
                &id("otelcol", "9.9.9"),
                Some(&in_channel),
                &InstalledVersions::new(),
            )
            .expect_err("unknown set")
            .contains("no package set"));
    }

    /// ADR-0076's fourth test at the gate: an act may only be aimed at an Agent the Set would
    /// move *forward*. Equal is not greater — a Set the Agent already runs changes nothing — and
    /// a reported version that cannot be ordered is refused rather than guessed at.
    #[test]
    fn fits_agent_refuses_what_is_not_an_upgrade() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let old = stored_set(&store, "otelcol", "1.0.0", b"v1");
        let new = stored_set(&store, "otelcol", "2.0.0", b"v2");
        let host = agent("linux", "amd64", &[]);

        let running_v2 = installed(&[("otelcol", "2.0.0")]);
        assert!(store
            .fits_agent(&old, Some(&host), &running_v2)
            .expect_err("backwards")
            .contains("not an upgrade"));
        assert!(store
            .fits_agent(&new, Some(&host), &running_v2)
            .expect_err("the same version")
            .contains("not an upgrade"));
        assert!(
            store
                .fits_agent(&new, Some(&host), &installed(&[("otelcol", "1.0.0")]))
                .is_ok(),
            "forward is what a rollout act is for"
        );
        assert!(
            store
                .fits_agent(&new, Some(&host), &installed(&[("otelcol", "")]))
                .is_ok(),
            "an empty reported version is nothing installed, not an unorderable one"
        );
        assert!(
            store
                .fits_agent(&new, Some(&host), &installed(&[("otelcol", "nightly")]))
                .is_err(),
            "what cannot be ordered must not be installed over what is running"
        );
        assert!(
            store
                .fits_agent(&new, Some(&host), &installed(&[("promtail", "9.9.9")]))
                .is_ok(),
            "another package's version says nothing about this one"
        );
    }

    /// The same test on the way in (ADR-0076): a Set the Agent already runs is no candidate, so
    /// nothing proposes it and no count includes it. The Set the Agent is *behind* still is one.
    #[test]
    fn a_set_that_is_no_upgrade_is_no_candidate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "1.0.0", b"v1");
        stored_set(&store, "otelcol", "2.0.0", b"v2");
        let host = agent("linux", "amd64", &[]);

        assert_eq!(
            candidates_for(&store, &host, &installed(&[("otelcol", "1.0.0")])),
            [("otelcol".to_string(), "2.0.0".to_string())],
        );
        assert!(
            candidates_for(&store, &host, &installed(&[("otelcol", "2.0.0")])).is_empty(),
            "an Agent already at the greatest version is proposed nothing"
        );
        assert!(
            candidates_for(&store, &host, &installed(&[("otelcol", "3.0.0")])).is_empty(),
            "and one ahead of the store is proposed nothing either"
        );
    }

    /// ADR-0079: an Agent that reports no version for the package is held against the version it
    /// reports *running*. This is what reaches the Clients released before the one that reports its
    /// own package version — they cannot state it, and they all state `service.version`.
    #[test]
    fn a_set_is_held_against_the_version_an_agent_reports_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "2.0.0", b"v2");
        let nothing = InstalledVersions::new();

        // The build metadata every Client appends takes no part in it (ADR-0029).
        for running in ["2.0.0", "2.0.0+a1b2c3d", "3.0.0"] {
            assert!(
                candidates_for(&store, &running_agent(running), &nothing).is_empty(),
                "an Agent already running {running} is proposed nothing"
            );
        }
        assert_eq!(
            candidates_for(&store, &running_agent("1.0.0"), &nothing),
            [("otelcol".to_string(), "2.0.0".to_string())],
            "and one genuinely behind is still reached"
        );

        // The act at the gate says the same thing, and says which version it read.
        let refusal = store
            .fits_agent(
                &id("otelcol", "2.0.0"),
                Some(&running_agent("2.0.0")),
                &nothing,
            )
            .expect_err("the same version is no upgrade");
        assert!(
            refusal.contains("not an upgrade") && refusal.contains("runs \"2.0.0\""),
            "the refusal names the version it compared against: {refusal}"
        );
    }

    /// ADR-0083 points 2 and 3, as re-decided: **what an Agent runs decides, in both directions**,
    /// and the claim is not consulted beside it. The record a package status comes from outlives
    /// the binary it describes; the program's own number is the statement about the present.
    ///
    /// This is the direction the accepted text had the other way round, and both halves of the
    /// trade are asserted here rather than only the convenient one — including that a Managed
    /// Process numbered below its Set can now be moved backwards, which is the cost the ADR
    /// records under Consequences.
    #[test]
    fn the_version_an_agent_runs_wins_over_the_version_it_claims() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "2.0.0", b"v2");

        // A program above the Set holds it back now, where the claim used to admit it.
        assert!(
            candidates_for(
                &store,
                &running_agent("9.9.9"),
                &installed(&[("otelcol", "1.0.0")])
            )
            .is_empty(),
            "the program says it is at 9.9.9; a Set at 2.0.0 moves it nowhere"
        );

        // A Collector numbering itself far below the Package that carries it: every version above
        // that number clears the test, the claim notwithstanding. Which of them an Agent gets is
        // no longer decided here — a channel holds one, and there is nothing left to rank.
        let store = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        stored_set(&store, "otelcol", "1.5.0", b"v15");
        assert_eq!(
            candidates_for(
                &store,
                &running_agent("0.98.0"),
                &installed(&[("otelcol", "2.0.0")])
            ),
            [
                ("otelcol".to_string(), "1.5.0".to_string()),
                ("otelcol".to_string(), "2.0.0".to_string())
            ],
            "both clear 0.98.0 — the claim of 2.0.0 is not consulted while the Agent says what it \
             runs (ADR-0083)"
        );
        assert!(
            store
                .fits_agent(
                    &id("otelcol", "1.5.0"),
                    Some(&running_agent("0.98.0")),
                    &installed(&[("otelcol", "2.0.0")])
                )
                .is_ok(),
            "and the act admits the lower Set too: 1.5.0 is ahead of what the program reports, so \
             the claim of 2.0.0 no longer refuses it — the downgrade ADR-0083 admits as its cost"
        );
    }

    /// ADR-0081: a claim the Agent's own program denies no longer holds the Set back. A Client that
    /// reports `supervisor 0.4.1` installed while reporting that it runs 0.4.0 has a record about a
    /// binary that is gone — and until this rule it was offered nothing, for good.
    #[test]
    fn a_claim_the_running_program_denies_no_longer_holds_the_set_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "0.4.1", b"v041");
        let claims_041 = installed(&[("otelcol", "0.4.1")]);

        assert_eq!(
            candidates_for(&store, &running_agent("0.4.0"), &claims_041),
            [("otelcol".to_string(), "0.4.1".to_string())],
            "the version it runs is what it has; the record says only how it once got there"
        );
        assert!(
            store
                .fits_agent(
                    &id("otelcol", "0.4.1"),
                    Some(&running_agent("0.4.0")),
                    &claims_041
                )
                .is_ok(),
            "and the act at the gate agrees, as all three consumers must"
        );

        // Corroborated, and nothing changes: an Agent that runs what it claims is proposed nothing.
        assert!(
            candidates_for(&store, &running_agent("0.4.1"), &claims_041).is_empty(),
            "a claim its program confirms is still the end of it"
        );
        let refusal = store
            .fits_agent(
                &id("otelcol", "0.4.1"),
                Some(&running_agent("0.4.1+a1b2c3d")),
                &claims_041,
            )
            .expect_err("the version it runs is the version offered");
        assert!(
            refusal.contains("runs \"0.4.1+a1b2c3d\"") && refusal.contains("not consulted"),
            "the refusal names the version that decided, and says the claim was not: {refusal}"
        );

        // A program that cannot be ordered says nothing, and the claim becomes the whole test
        // (ADR-0083 point 4) — which here refuses the Set the claim already names.
        assert!(
            candidates_for(&store, &running_agent("nightly"), &claims_041).is_empty(),
            "an unorderable program version says nothing, and the claim stands"
        );
    }

    /// The case that re-opened ADR-0083, and the other face of the one above: a claim *above* the
    /// Set, over a program that denies running it. A self-update that staged 0.4.2 and did not take
    /// leaves `supervisor 0.4.2` recorded on a host whose program still reports 0.4.0 — and rolling
    /// 0.4.1 out to it was refused as a downgrade, so the host stayed where it was for good.
    #[test]
    fn a_claim_above_the_set_no_longer_holds_it_back_either() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "0.4.1", b"v041");
        let claims_042 = installed(&[("otelcol", "0.4.2")]);

        assert_eq!(
            candidates_for(&store, &running_agent("0.4.0"), &claims_042),
            [("otelcol".to_string(), "0.4.1".to_string())],
            "the record names a binary this host is not running; 0.4.1 still moves it forward"
        );
        assert!(
            store
                .fits_agent(
                    &id("otelcol", "0.4.1"),
                    Some(&running_agent("0.4.0")),
                    &claims_042
                )
                .is_ok(),
            "and the act at the gate agrees, as all three consumers must"
        );

        // The running version decides in *both* directions, so it still refuses what moves nobody:
        // a host already at 0.4.1 is proposed nothing, however high its record reads.
        assert!(
            candidates_for(&store, &running_agent("0.4.1"), &claims_042).is_empty(),
            "equal to what it runs is no upgrade, whatever the claim says"
        );
    }

    /// ADR-0083 point 4: a running version that cannot be ordered says nothing, rather than
    /// refusing, and the claim becomes the whole test. A GLPI Agent numbers itself `1.19` and an
    /// appliance `24.04.1`; failing closed on those would make a program's numbering habit into a
    /// fleet that cannot deliver to it at all.
    #[test]
    fn a_program_version_nothing_can_order_leaves_the_set_reaching() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "2.0.0", b"v2");
        let nothing = InstalledVersions::new();

        for running in ["1.19", "24.04.1", "unknown", "v2.0.0"] {
            assert_eq!(
                candidates_for(&store, &running_agent(running), &nothing),
                [("otelcol".to_string(), "2.0.0".to_string())],
                "{running:?} orders against nothing, so it says nothing"
            );
        }

        // And an Agent reporting no version at all is the first rollout, unchanged (ADR-0076).
        assert_eq!(
            candidates_for(&store, &agent("linux", "amd64", &[]), &nothing),
            [("otelcol".to_string(), "2.0.0".to_string())],
        );
    }

    /// Only the Package its channel holds is a candidate. Two versions of one Agent type used to be
    /// ranked against each other — and, when nothing could order them, refused as a tie. A
    /// Deployment holds one Package per type (ADR-0096), so there is no second contender to rank
    /// or refuse: the store answers what the channel points at, or nothing.
    #[test]
    fn only_the_package_its_ring_holds_is_a_candidate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let a = stored_set(&store, "otelcol", "nightly-a", b"a");
        let b = stored_set(&store, "otelcol", "nightly-b", b"b");
        let host = agent("linux", "amd64", &[]);

        // Neither version can be ordered against the other, which used to be the one case with no
        // defensible answer. It is now a question nobody asks.
        for held in [&a, &b] {
            assert_eq!(
                store.candidate(Some(&channel(held)), Some(&host), &InstalledVersions::new()),
                Some(held.clone()),
                "the channel decides, and it holds exactly one"
            );
        }
        assert_eq!(
            store.candidate(
                Some(&channel(&a)),
                Some(&host),
                &installed(&[("otelcol", "1.0.0")])
            ),
            None,
            "and what is no upgrade is still no candidate (ADR-0083)"
        );
    }

    /// An Agent no channel claims is a candidate for nothing — the ordinary state of a host that has
    /// enrolled and not been labelled yet, and not an error.
    #[test]
    fn an_agent_without_a_ring_is_offered_no_candidate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "1.0.0", b"v1");
        assert_eq!(
            store.candidate(
                None,
                Some(&agent("linux", "amd64", &[])),
                &InstalledVersions::new()
            ),
            None
        );
    }

    /// Fit before aim (ADR-0031, ADR-0034): an entry for another platform, or a Set for another
    /// Agent type, is never a candidate — and an Agent reporting neither fits nothing.
    #[test]
    fn fit_is_mandatory_platform_and_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "1.0.0", b"linux-only");
        let foreign = PackageId::new("promtail", "1.0.0").expect("id");
        store.create(&foreign).expect("create");
        store
            .put_entry(&foreign, &linux(), b"p".to_vec())
            .expect("entry");

        assert!(candidates(&store, &agent("windows", "amd64", &[])).is_empty());
        assert_eq!(
            candidates(&store, &agent("linux", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())],
            "the promtail set fits another type and is not a candidate"
        );
        assert_eq!(
            store.candidate(
                Some(&channel(&id("otelcol", "1.0.0"))),
                Some(&AgentDescription::default()),
                &InstalledVersions::new()
            ),
            None,
            "no platform and no type fits nothing"
        );
    }

    /// Both sides of the platform comparison are canonicalised (ADR-0031): an artifact uploaded
    /// as `macos`/`x86_64` reaches an Agent reporting `darwin`/`amd64`.
    #[test]
    fn both_sides_of_the_comparison_are_canonicalised() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let set = id("otelcol", "1.0.0");
        store.create(&set).expect("create");
        let mac = Platform::new("macos", "x86_64").expect("canonicalised");
        assert_eq!((mac.os.as_str(), mac.arch.as_str()), ("darwin", "amd64"));
        store.put_entry(&set, &mac, b"mac".to_vec()).expect("entry");
        assert_eq!(
            candidates(&store, &agent("darwin", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())]
        );
        assert_eq!(
            offered(&store, Some(&set), &agent("darwin", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())]
        );
    }

    /// The offered download URL names the whole identity, so two versions of one name never serve
    /// each other's bytes.
    #[test]
    fn the_offer_carries_a_download_url_naming_the_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let set = stored_set(&store, "otelcol", "1.2.3", b"bytes");
        let offer = store
            .offer_for_assigned(
                Some(&set),
                Some(&channel(&set)),
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
            "https://fleet.example/api/v1/packages/otelcol/1.2.3/file?os=linux&arch=amd64"
        );
    }

    /// The aggregate hash is per Agent and follows its assignments (ADR-0061): it changes when
    /// the assigned Set changes, and is empty for an Agent assigned nothing it fits.
    #[test]
    fn the_aggregate_hash_is_per_agent_and_follows_the_assignment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let v1 = stored_set(&store, "otelcol", "1.0.0", b"v1");
        let before = store.assigned_hash_for(Some(&v1), Some(&agent("linux", "amd64", &[])));
        assert!(!before.is_empty());
        assert!(store
            .assigned_hash_for(Some(&v1), Some(&agent("windows", "amd64", &[])))
            .is_empty());
        assert!(store
            .assigned_hash_for(None, Some(&agent("linux", "amd64", &[])))
            .is_empty());

        let v2 = stored_set(&store, "otelcol", "2.0.0", b"v2");
        let after = store.assigned_hash_for(Some(&v2), Some(&agent("linux", "amd64", &[])));
        assert_ne!(before, after, "a new assigned version moves the aggregate");
    }

    /// Deleting an entry frees its artifact; deleting the Set takes the directory with it.
    #[test]
    fn deletion_frees_entries_and_sets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let set = id("otelcol", "1.0.0");
        store.create(&set).expect("create");
        store
            .put_entry(&set, &linux(), b"bytes".to_vec())
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
    /// ship (ADR-0008's principle).
    #[test]
    fn a_corrupt_artifact_fails_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let set = id("otelcol", "1.0.0");
        {
            let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
            store.create(&set).expect("create");
            store
                .put_entry(&set, &linux(), b"good bytes".to_vec())
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

    /// The store directory and each Set's metadata are owner-only (ADR-0018): a referenced
    /// source's headers may carry a token.
    #[cfg(unix)]
    #[test]
    fn the_store_and_its_metadata_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let set = id("otelcol", "1.0.0");
        store.create(&set).expect("create");
        assert_eq!(
            std::fs::metadata(dir.path())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(dir.path().join(set.to_string()).join("package.json"))
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    /// There is no migration and no legacy reader: a store holding what an older layout wrote is
    /// **named at startup**, never skipped (ADR-0095 point 5). Skipping is the dangerous half — a
    /// store that merely looks empty offers nothing and says nothing about why, which an operator
    /// reads as "nothing uploaded yet".
    ///
    /// Both shapes an older Server left behind are covered: the loose files of a pre-ADR-0052
    /// store, and the `<name>@<version>@<type>/set.json` directories of an ADR-0052 one.
    #[test]
    fn a_store_in_an_older_layout_refuses_to_open_and_names_what_is_in_the_way() {
        // Pre-ADR-0052: loose files in the store root.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("otelcol.json"),
            serde_json::json!({"name": "otelcol", "service_name": "otelcol"}).to_string(),
        )
        .expect("write a pre-ADR-0052 rollout file");
        let error = PackageStore::open(dir.path().to_path_buf())
            .map(|_| ())
            .expect_err("an older layout is refused, never read as an empty store");
        assert!(
            error.contains("otelcol.json"),
            "the error must name what an operator has to move aside, got: {error}"
        );

        // ADR-0052: a Set directory holding `set.json`.
        let dir = tempfile::tempdir().expect("tempdir");
        let set = dir.path().join("otelcol@1.0.0@otelcol");
        std::fs::create_dir_all(&set).expect("set dir");
        std::fs::write(set.join("set.json"), "{}").expect("set.json");
        let error = PackageStore::open(dir.path().to_path_buf())
            .map(|_| ())
            .expect_err("a Set directory is refused too");
        assert!(
            error.contains("otelcol@1.0.0@otelcol"),
            "the error must name the directory, got: {error}"
        );

        // But the Deployments live here on purpose, and the loader steps over them.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(DEPLOYMENTS_DIR)).expect("deployments dir");
        PackageStore::open(dir.path().to_path_buf()).expect("the channel store is not a stray");
    }

    /// The identity grammar keeps the triple a safe directory name and an unambiguous parse:
    /// `@` and path separators are refused.
    #[test]
    fn identity_tokens_are_bounded() {
        assert!(PackageId::new("otelcol", "1.2.3-rc.1+abc").is_ok());
        assert!(PackageId::new("a@b", "1.0.0").is_err());
        assert!(PackageId::new("otelcol", "1.0.0/../evil").is_err());
        assert!(PackageId::new("", "1.0.0").is_err());
        assert!(PackageId::new("not a type", "1.0.0").is_err());
        assert!(PackageId::new(&"x".repeat(65), "1.0.0").is_err());
    }
}
