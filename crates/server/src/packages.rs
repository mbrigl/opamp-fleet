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

use opamp::proto::{
    AgentDescription, DownloadableFile, Header, Headers, PackageAvailable, PackageType,
    PackagesAvailable,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::info;

use crate::configs::{matches, validate_name};

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

/// A Set's identity (ADR-0052): stated at creation, never edited. A new version is a **new Set**,
/// and the type is as constitutive of "what is this artifact" as the version — an attribute would
/// be editable, and retyping published bytes to another kind of Agent is exactly the mistake an
/// immutable identity forecloses.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SetId {
    /// The name a map key on the wire carries, so it follows the ADR-0010 grammar.
    pub name: String,
    /// The Agent type this Set is built for, matched **raw** against the `service.name` an Agent
    /// reports (ADR-0034) — there is no canonical set of Agent types to normalise against.
    pub service_name: String,
    /// The version every entry of this Set shares. A release is one version across its platforms;
    /// the Set is the object that *is* that release.
    pub version: String,
}

impl SetId {
    /// Validates all three parts; the one gate every write goes through.
    pub fn new(name: &str, service_name: &str, version: &str) -> Result<Self, String> {
        validate_name(name).map_err(|e| format!("invalid name {name:?}: {e}"))?;
        validate_identity_token(service_name, "agent type")?;
        validate_identity_token(version, "version")?;
        Ok(SetId {
            name: name.to_string(),
            service_name: service_name.to_string(),
            version: version.to_string(),
        })
    }

    /// The Set's directory name: `<name>@<version>@<type>`. The name grammar admits no `@` and the
    /// identity tokens admit no `@` either, so the triple parses back unambiguously — and stays
    /// readable in a directory listing, which an opaque hash would not be.
    fn dir_name(&self) -> String {
        format!("{}@{}@{}", self.name, self.version, self.service_name)
    }

    /// Parses the `<name>@<version>@<type>` form the [`Display`] impl and the Set directory use —
    /// also the persisted shape of a package assignment (ADR-0061).
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut parts = text.split('@');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(name), Some(version), Some(service_name), None) => {
                SetId::new(name, service_name, version)
            }
            _ => Err(format!(
                "{text:?} is not a <name>@<version>@<type> set identity"
            )),
        }
    }
}

impl fmt::Display for SetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}@{}", self.name, self.version, self.service_name)
    }
}

/// A stored Set (ADR-0052): its identity, whom it targets, whether the fleet may have it, and one
/// entry per Platform.
///
/// The split ADR-0031 named still holds: **the Selector aims, the Platform fits** — and the Set
/// adds *the version releases*. Artifacts stay on disk (`<set>/<os>-<arch>.bin`) and are streamed
/// to whoever asks — a program weighs hundreds of megabytes, and a fleet server holding every one
/// of them in memory, plus a copy per download, is the shape this deliberately avoids.
#[derive(Clone)]
pub struct PackageSet {
    pub id: SetId,
    /// The Selector (ADR-0012 semantics, ADR-0017): equality pairs that must all match an
    /// attribute the Agent reported. **Empty matches every Agent** (of this Set's type). Always
    /// editable — aim is not bytes, and since ADR-0061 it only steers whom a rollout act would
    /// reach, never a running offer.
    pub selector: BTreeMap<String, String>,
    /// `false` is the Baseline's `TopLevel` (a Managed Process's binary), `true` an `Addon`. One
    /// flag for the Set: what kind of package this is does not vary by platform.
    pub addon: bool,
    /// One entry per Platform — a map, so a duplicate for the same combination is unrepresentable
    /// (ADR-0052). A Set may sit empty while it is being assembled; publishing it empty is refused.
    pub entries: BTreeMap<Platform, Entry>,
}

/// One platform's artifact of a Set: **either** an uploaded file **or** a source reference, with
/// the hash — and optionally the signature — that protect what an Agent installs.
#[derive(Clone)]
pub struct Entry {
    pub platform: Platform,
    /// SHA-256 of the artifact bytes: computed here for an upload, the operator's word (verified
    /// by every Agent) for a source reference (ADR-0018).
    pub content_hash: Vec<u8>,
    /// Optional Ed25519 signature over the artifact, supplied by the operator.
    pub signature: Option<Vec<u8>>,
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

impl PackageSet {
    /// The per-package hash the Agent compares to decide whether to download: over the fields that
    /// identify the offer (type, version) and the content. Framed length-prefixed so no boundary
    /// is ambiguous. The Platform needs no place in it — two platforms' artifacts differ in their
    /// content hash by construction.
    fn package_hash(&self, entry: &Entry) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update([u8::from(self.addon)]);
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
                signature: entry.signature.clone().unwrap_or_default(),
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
                    "{download_base}/api/v1/packages/{}/{}/{}/file?os={}&arch={}",
                    self.id.name,
                    self.id.service_name,
                    self.id.version,
                    entry.platform.os,
                    entry.platform.arch
                ),
                content_hash: entry.content_hash.clone(),
                signature: entry.signature.clone().unwrap_or_default(),
                headers,
            },
        };
        PackageAvailable {
            r#type: if self.addon {
                PackageType::Addon as i32
            } else {
                PackageType::TopLevel as i32
            },
            version: self.id.version.clone(),
            file: Some(file),
            hash: self.package_hash(entry),
        }
    }
}

/// One Set as the REST API lists it (ADR-0052): its identity, whom it targets, and what it holds
/// for each platform — never the artifact bytes.
pub struct SetSummary {
    pub name: String,
    pub service_name: String,
    pub version: String,
    pub selector: BTreeMap<String, String>,
    pub addon: bool,
    /// One entry per Platform, in platform order.
    pub entries: Vec<EntrySummary>,
}

/// One entry of a Set as the REST API shows it.
pub struct EntrySummary {
    pub os: String,
    pub arch: String,
    pub size: u64,
    /// The address an Agent fetches this from when the Server does not hold it (ADR-0018).
    pub source_url: Option<String>,
    /// Whether the operator supplied an Ed25519 signature for this entry.
    pub signed: bool,
}

impl SetSummary {
    fn of(set: &PackageSet) -> Self {
        SetSummary {
            name: set.id.name.clone(),
            service_name: set.id.service_name.clone(),
            version: set.id.version.clone(),
            selector: set.selector.clone(),
            addon: set.addon,
            entries: set
                .entries
                .values()
                .map(|entry| EntrySummary {
                    os: entry.platform.os.clone(),
                    arch: entry.platform.arch.clone(),
                    size: entry.size,
                    source_url: entry.source.as_ref().map(|s| s.url.clone()),
                    signed: entry.signature.is_some(),
                })
                .collect(),
        }
    }
}

/// A Set as persisted: `<name>@<version>@<type>/set.json`, entries inline. One document per Set —
/// what ADR-0019 kept secretly (other versions), this store keeps openly, as more Sets.
#[derive(Serialize, Deserialize)]
struct SetMeta {
    name: String,
    service_name: String,
    version: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    selector: BTreeMap<String, String>,
    /// The pre-ADR-0061 publication state. Read for exactly one purpose — seeding the per-Agent
    /// assignments of Agent records that predate the ADR (point 9) — and written only `true`, by
    /// the pre-ADR-0052 migration below, so a store that migrates twice in one upgrade does not
    /// lose what was in force. Every ordinary write drops the field.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    published: bool,
    #[serde(default)]
    addon: bool,
    #[serde(default)]
    entries: Vec<EntryMeta>,
}

/// One entry as persisted inside `set.json`; an uploaded entry's bytes are `<os>-<arch>.bin`
/// beside it.
#[derive(Serialize, Deserialize)]
struct EntryMeta {
    os: String,
    arch: String,
    content_hash_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature_hex: Option<String>,
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
            signature_hex: entry.signature.as_ref().map(hex::encode),
            source_url: entry.source.as_ref().map(|s| s.url.clone()),
            source_headers: entry
                .source
                .as_ref()
                .map(|s| s.headers.clone())
                .unwrap_or_default(),
        }
    }
}

impl SetMeta {
    fn of(set: &PackageSet) -> Self {
        SetMeta {
            name: set.id.name.clone(),
            service_name: set.id.service_name.clone(),
            version: set.id.version.clone(),
            selector: set.selector.clone(),
            published: false,
            addon: set.addon,
            entries: set.entries.values().map(EntryMeta::of).collect(),
        }
    }
}

/// The persistent package store (ADR-0052): one directory per Set under `packages_dir`, holding
/// `set.json` and one `<os>-<arch>.bin` per uploaded entry, restored at startup. The in-memory
/// map is what the control loop reads.
pub struct PackageStore {
    dir: PathBuf,
    sets: RwLock<BTreeMap<SetId, PackageSet>>,
    /// The Sets a pre-ADR-0061 store said were published — what was in force at upgrade time.
    /// Read once by the fleet to seed the assignments of Agent records that predate the ADR
    /// (point 9); empty for a store born under it.
    formerly_published: Vec<SetId>,
}

impl PackageStore {
    /// Opens the store, creating the directory, **migrating a pre-ADR-0052 store** where one is
    /// found, and loading every persisted Set. A metadata or artifact file that cannot be read,
    /// does not parse, or whose artifact no longer matches its recorded hash is a startup error —
    /// a corrupt distribution artifact must never ship.
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
        migrate_legacy(&dir)?;

        let mut sets = BTreeMap::new();
        let mut formerly_published = Vec::new();
        let listing =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in listing {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
                .path();
            if !path.is_dir() {
                continue;
            }
            let meta_path = path.join("set.json");
            if !meta_path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&meta_path)
                .map_err(|e| format!("cannot read {}: {e}", meta_path.display()))?;
            let meta: SetMeta = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", meta_path.display()))?;
            let id = SetId::new(&meta.name, &meta.service_name, &meta.version)
                .map_err(|e| format!("invalid identity in {}: {e}", meta_path.display()))?;
            // The directory name is derived from the identity; a mismatch means the artifacts
            // will not be found where the store looks for them, so it is refused by name.
            if path.file_name().and_then(|n| n.to_str()) != Some(id.dir_name().as_str()) {
                return Err(format!(
                    "{} does not match the identity {} its set.json states — rename the \
                     directory or fix the file",
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
                let signature = match &entry_meta.signature_hex {
                    Some(hex_text) => Some(hex::decode(hex_text).map_err(|e| {
                        format!("invalid signature in {}: {e}", meta_path.display())
                    })?),
                    None => None,
                };
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
                        signature,
                        size,
                        source,
                    },
                );
            }
            if meta.published {
                // A pre-ADR-0061 store said this Set was in force; remembered for seeding the
                // assignments of Agent records that predate the ADR (point 9).
                formerly_published.push(id.clone());
            }
            sets.insert(
                id.clone(),
                PackageSet {
                    id,
                    selector: meta.selector,
                    addon: meta.addon,
                    entries,
                },
            );
        }
        Ok(PackageStore {
            dir,
            sets: RwLock::new(sets),
            formerly_published,
        })
    }

    /// The Sets a pre-ADR-0061 store said were published, for seeding the assignments of Agent
    /// records that predate the ADR. Empty for a store born under ADR-0061.
    pub fn formerly_published(&self) -> &[SetId] {
        &self.formerly_published
    }

    fn set_dir(&self, id: &SetId) -> PathBuf {
        self.dir.join(id.dir_name())
    }

    /// Every Set, in identity order — the REST list view; never the artifact bytes.
    pub fn list(&self) -> Vec<SetSummary> {
        self.sets
            .read()
            .expect("sets lock")
            .values()
            .map(SetSummary::of)
            .collect()
    }

    /// One stored Set as the REST API presents it; `None` when no such Set exists.
    pub fn summary(&self, id: &SetId) -> Option<SetSummary> {
        self.sets
            .read()
            .expect("sets lock")
            .get(id)
            .map(SetSummary::of)
    }

    /// Where one uploaded artifact lives, for the download endpoint to stream from. `None` when no
    /// Set of that identity holds one for that Platform, or holds it as a reference.
    pub fn artifact_path(&self, id: &SetId, platform: &Platform) -> Option<PathBuf> {
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

    /// Creates a Set, or updates an existing one's Selector and kind. **Saving never
    /// distributes** (ADR-0061): a Set reaches an Agent only through a rollout act. The fleet
    /// refuses the kind change on a Set assigned somewhere before calling here — the addon flag
    /// is part of what the offer's hash covers, so it is frozen with the bytes.
    pub fn create_or_update(
        &self,
        id: &SetId,
        selector: BTreeMap<String, String>,
        addon: bool,
    ) -> Result<(), String> {
        let mut sets = self.sets.write().expect("sets lock");
        if let Some(set) = sets.get_mut(id) {
            set.selector = selector;
            set.addon = addon;
            let meta = serde_json::to_vec_pretty(&SetMeta::of(set)).expect("set serializes");
            return self.write_meta(id, &meta);
        }
        let set = PackageSet {
            id: id.clone(),
            selector,
            addon,
            entries: BTreeMap::new(),
        };
        std::fs::create_dir_all(self.set_dir(id))
            .map_err(|e| format!("cannot create {}: {e}", self.set_dir(id).display()))?;
        let meta = serde_json::to_vec_pretty(&SetMeta::of(&set)).expect("set serializes");
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
    pub fn staging_path(&self, id: &SetId, platform: &Platform) -> Result<PathBuf, String> {
        self.writable(id)?;
        Ok(self.set_dir(id).join(format!("{}.upload", platform.tag())))
    }

    /// The gate every entry write passes: the Set must exist. The immutability of an assigned
    /// Set (ADR-0061) is the fleet's to enforce — only it knows the assignments.
    fn writable(&self, id: &SetId) -> Result<(), String> {
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
        id: &SetId,
        platform: &Platform,
        signature: Option<Vec<u8>>,
        staged: &Path,
    ) -> Result<(), String> {
        let result = self.store_staged(id, platform, signature, staged);
        if result.is_err() {
            let _ = std::fs::remove_file(staged);
        }
        result
    }

    fn store_staged(
        &self,
        id: &SetId,
        platform: &Platform,
        signature: Option<Vec<u8>>,
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
                signature,
                size,
                source: None,
            },
        )
    }

    /// Creates or replaces one entry from bytes already in hand — the shape the tests and any
    /// small artifact use. A real upload takes [`put_staged`](Self::put_staged) instead.
    pub fn put_entry(
        &self,
        id: &SetId,
        platform: &Platform,
        signature: Option<Vec<u8>>,
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
                signature,
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
        id: &SetId,
        platform: &Platform,
        content_hash: Vec<u8>,
        signature: Option<Vec<u8>>,
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
                signature,
                size: 0,
                source: Some(source),
            },
        )
    }

    /// Writes one entry into the Set's map and its `set.json` — the single path every entry write
    /// converges on. Replacing the entry for a Platform the Set already holds is what "no
    /// duplicate entries" means in a map: the combination stays unique by construction.
    fn put_entry_record(&self, id: &SetId, entry: Entry) -> Result<(), String> {
        let mut sets = self.sets.write().expect("sets lock");
        let set = sets
            .get_mut(id)
            .ok_or_else(|| format!("no package set {id}"))?;
        set.entries.insert(entry.platform.clone(), entry);
        let meta = serde_json::to_vec_pretty(&SetMeta::of(set)).expect("set serializes");
        self.write_meta(id, &meta)
    }

    /// Deletes one entry; `Ok(false)` when the Set or the entry does not exist. The fleet refuses
    /// this before calling here when the Set is assigned to an Agent (ADR-0061). The last entry
    /// taken away leaves an **empty Set**, kept: a Set being reassembled is a normal state, and
    /// deleting the Set is its own act.
    pub fn delete_entry(&self, id: &SetId, platform: &Platform) -> Result<bool, String> {
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
        let meta = serde_json::to_vec_pretty(&SetMeta::of(set)).expect("set serializes");
        self.write_meta(id, &meta)?;
        Ok(true)
    }

    /// Deletes a whole Set — entries, artifacts, and metadata; `Ok(false)` when none of that
    /// identity exists. The fleet removes every assignment that referenced it, which withdraws
    /// the offer; Agents that installed it keep running it (ADR-0017).
    pub fn delete_set(&self, id: &SetId) -> Result<bool, String> {
        let mut sets = self.sets.write().expect("sets lock");
        if sets.remove(id).is_none() {
            return Ok(false);
        }
        let dir = self.set_dir(id);
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("cannot delete {}: {e}", dir.display()))?;
        Ok(true)
    }

    /// Sets a Set's Selector (ADR-0017) — always editable, because aim is not bytes. Since
    /// ADR-0061 it steers only whom a rollout act would reach; no offer changes with it.
    pub fn set_selector(
        &self,
        id: &SetId,
        selector: BTreeMap<String, String>,
    ) -> Result<(), String> {
        let mut sets = self.sets.write().expect("sets lock");
        let set = sets
            .get_mut(id)
            .ok_or_else(|| format!("no package set {id}"))?;
        set.selector = selector;
        let meta = serde_json::to_vec_pretty(&SetMeta::of(set)).expect("set serializes");
        self.write_meta(id, &meta)
    }

    /// One Agent's offer, composed from its **assignments** (ADR-0061): for each assigned Set,
    /// the entry built for the platform the Agent reports, plus the `all_packages_hash` over that
    /// set (the Baseline's per-Agent aggregate). `None` when the Agent is assigned nothing it
    /// fits — it is offered nothing and keeps running what it runs. An assignment whose Set is
    /// gone composes nothing rather than failing: deletion removes assignments, so the case is a
    /// race, not a state.
    pub fn offer_for_assigned(
        &self,
        assigned: &BTreeMap<String, SetId>,
        description: Option<&AgentDescription>,
        download_base: &str,
        headers: Option<Headers>,
    ) -> Option<PackagesAvailable> {
        let sets = self.sets.read().expect("sets lock");
        let matching = assigned_entries(&sets, assigned, description);
        if matching.is_empty() {
            return None;
        }
        Some(PackagesAvailable {
            packages: matching
                .iter()
                .map(|(set, entry)| {
                    (
                        set.id.name.clone(),
                        set.to_available(entry, download_base, headers.clone()),
                    )
                })
                .collect(),
            all_packages_hash: aggregate_hash(&matching),
        })
    }

    /// The aggregate hash over one Agent's assignments, to gate re-offering without building the
    /// whole message. Empty when the Agent is assigned nothing it fits — it is offered nothing,
    /// and has nothing to be in sync with.
    pub fn assigned_hash_for(
        &self,
        assigned: &BTreeMap<String, SetId>,
        description: Option<&AgentDescription>,
    ) -> Vec<u8> {
        let sets = self.sets.read().expect("sets lock");
        let matching = assigned_entries(&sets, assigned, description);
        if matching.is_empty() {
            return Vec::new();
        }
        aggregate_hash(&matching)
    }

    /// The identities of the Sets a rollout act would release to this Agent — the **candidates**
    /// (ADR-0061): fitted by type, platform and Selector, held to an upgrade over what the Agent
    /// reports installed (ADR-0076), then resolved by specificity and version (ADR-0052). Never
    /// an offer. `Err` when the targeting is ambiguous and the Server refuses to guess; the fleet
    /// view shows the refusal.
    pub fn candidate_ids(
        &self,
        description: Option<&AgentDescription>,
        installed: &InstalledVersions,
    ) -> Result<Vec<SetId>, String> {
        let sets = self.sets.read().expect("sets lock");
        resolve(&sets, description, installed)
            .map(|matching| matching.iter().map(|(set, _)| set.id.clone()).collect())
    }

    /// The identities of the Sets that **fit and aim at** this Agent, version-blind: its type,
    /// an entry for its platform, and a Selector that matches — no ranking, and not ADR-0076's
    /// upgrade test.
    ///
    /// This is the other half of the answer the Set view needs (ADR-0076 point 8). A Set reaching
    /// nobody means one of two unrelated things — it aims at nobody, or everyone it aims at is
    /// already at this version or newer — and only the first is a mistake to go looking for.
    pub fn aiming_at(&self, description: Option<&AgentDescription>) -> Vec<SetId> {
        let sets = self.sets.read().expect("sets lock");
        let (Some(platform), Some(service_name)) = (
            Platform::reported(description),
            reported_service_name(description),
        ) else {
            return Vec::new();
        };
        sets.values()
            .filter(|set| fits_and_aims(set, &platform, service_name, description))
            .map(|set| set.id.clone())
            .collect()
    }

    /// The Sets a **pre-ADR-0061** offer would have released to this Agent: the candidate
    /// resolution restricted to the formerly published Sets. The migration seed for an Agent
    /// record that predates the ADR (point 9); ambiguous targeting seeds nothing, exactly as the
    /// old offer composed nothing.
    pub fn formerly_offered(&self, description: Option<&AgentDescription>) -> Vec<SetId> {
        if self.formerly_published.is_empty() {
            return Vec::new();
        }
        let sets = self.sets.read().expect("sets lock");
        let subset: BTreeMap<SetId, PackageSet> = sets
            .iter()
            .filter(|(id, _)| self.formerly_published.contains(id))
            .map(|(id, set)| (id.clone(), set.clone()))
            .collect();
        // Version-blind on purpose (ADR-0076 point 6): this reproduces what the old publication
        // model *had* offered, and reading history through today's test would seed a different
        // fleet than the one that was actually running.
        resolve(&subset, description, &InstalledVersions::new())
            .map(|matching| matching.iter().map(|(set, _)| set.id.clone()).collect())
            .unwrap_or_default()
    }

    /// Whether an explicit rollout act may release this Set to this Agent: the Set must exist,
    /// hold an entry for the platform the Agent reports, be built for its type, its Selector must
    /// match (ADR-0061), and its version must be an **upgrade** over what the Agent reports
    /// installed under that name (ADR-0076).
    ///
    /// Still **not** the version *ranking* of [`resolve`]: rolling out a Set older than a sibling
    /// the store also holds stays the operator's to make. What ADR-0076 forbids is aiming an act
    /// at an Agent it would move backwards, or not move at all — the count beside the button and
    /// the button itself now answer the same question.
    pub fn fits_agent(
        &self,
        id: &SetId,
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
        if reported_service_name(description) != Some(set.id.service_name.as_str()) {
            return Err(format!(
                "set {id} is built for Agent type {:?}, which this Agent does not report",
                set.id.service_name
            ));
        }
        if !set.entries.contains_key(&platform) {
            return Err(format!(
                "set {id} holds no entry for {}-{}, which this Agent reports",
                platform.os, platform.arch
            ));
        }
        if !matches(&set.selector, description) {
            return Err(format!(
                "the Selector of set {id} does not match this Agent"
            ));
        }
        if !upgrades(set, installed, description) {
            // Which of the two versions was compared is the operator's first question here, so the
            // refusal says both what it read and where it read it (ADR-0079).
            return match installed.get(&set.id.name).filter(|has| !has.is_empty()) {
                Some(has) => Err(format!(
                    "set {id} is not an upgrade for this Agent, which reports {has:?} installed \
                     for package {:?}",
                    set.id.name
                )),
                None => Err(format!(
                    "set {id} is not an upgrade for this Agent, which reports no version for \
                     package {:?} and runs {:?}",
                    set.id.name,
                    reported_service_version(description).unwrap_or_default()
                )),
            };
        }
        Ok(())
    }

    /// Whether this Set is an addon; `None` when no Set of that identity exists. The fleet uses
    /// it for the Baseline's "one top-level package" rule when it writes an assignment.
    pub fn is_addon(&self, id: &SetId) -> Option<bool> {
        self.sets
            .read()
            .expect("sets lock")
            .get(id)
            .map(|set| set.addon)
    }

    fn write_meta(&self, id: &SetId, bytes: &[u8]) -> Result<(), String> {
        let dir = self.set_dir(id);
        let path = dir.join("set.json");
        let temp = dir.join("set.json.tmp");
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

/// The entries one Agent's assignments compose (ADR-0061): each assigned Set that still exists
/// and holds an entry for the platform the Agent reports. No ranking runs here — the operator's
/// assignment already chose — and a Set that stopped fitting (the host was reinstalled on another
/// platform) simply composes nothing for it.
fn assigned_entries<'a>(
    sets: &'a BTreeMap<SetId, PackageSet>,
    assigned: &BTreeMap<String, SetId>,
    description: Option<&AgentDescription>,
) -> Vec<(&'a PackageSet, &'a Entry)> {
    let Some(platform) = Platform::reported(description) else {
        return Vec::new();
    };
    assigned
        .values()
        .filter_map(|id| {
            let set = sets.get(id)?;
            let entry = set.entries.get(&platform)?;
            Some((set, entry))
        })
        .collect()
}

/// What an Agent reports it has installed, per package name: `PackageStatuses.packages[name]
/// .agent_has_version` as the Agent last sent it (ADR-0015). A name that is absent — and a name
/// whose reported version is empty, which is how an Agent that has installed nothing reports a
/// package it was offered — means *nothing is installed under that name*.
pub type InstalledVersions = BTreeMap<String, String>;

/// ADR-0076's fourth matching test: a Set reaches an Agent only as an **upgrade**.
///
/// The Set's version must be strictly greater than the version the Agent has, as ADR-0029 compares
/// versions. `Equal` does not match: a Set the Agent already runs would reach it with nothing.
///
/// *What the Agent has* is read in two steps (ADR-0079). The **package status** for this Set's name
/// is the authority whenever the Agent reports one: it is a statement about this very package, so a
/// value that cannot be ordered refuses the match — the safe direction, and the Client's own
/// (`selfupdate::install_offer`): what cannot be ordered must not be installed over what is
/// running. Where there is no such status — every Client released before the one that reports its
/// own version, and every Agent whose program a package never installed — the **`service.version`
/// the Agent reports** stands in. That one is a best-effort signal rather than a claim about the
/// package, so an unorderable value there says nothing at all instead of refusing: a program that
/// numbers itself `1.19` or `24.04.1` must stay reachable by packages.
///
/// An Agent that reports neither has nothing to be greater than: the first rollout, which matches.
fn upgrades(
    set: &PackageSet,
    installed: &InstalledVersions,
    description: Option<&AgentDescription>,
) -> bool {
    let greater = |has: &str| {
        opamp::version::precedence(&set.id.version, has) == Some(std::cmp::Ordering::Greater)
    };
    match installed.get(&set.id.name).filter(|has| !has.is_empty()) {
        Some(has) => greater(has),
        None => match reported_service_version(description) {
            // Unorderable: the stand-in abstains, and the Set matches on the other three tests.
            Some(running) => greater(running) || opamp::version::parse(running).is_none(),
            None => true,
        },
    }
}

/// The version an Agent reports as `service.version` — its program's own number, and since ADR-0079
/// what a Set is held against when the Agent reports no version for the package itself.
fn reported_service_version(description: Option<&AgentDescription>) -> Option<&str> {
    opamp::attributes::string_value(
        &description?.identifying_attributes,
        opamp::attributes::SERVICE_VERSION,
    )
    .filter(|version| !version.is_empty())
}

/// Whether a Set **fits** this Agent and its Selector **aims** at it (ADR-0034, ADR-0031,
/// ADR-0017) — the three version-blind tests, shared by everything that matches a Set to an Agent
/// so they cannot drift apart.
fn fits_and_aims(
    set: &PackageSet,
    platform: &Platform,
    service_name: &str,
    description: Option<&AgentDescription>,
) -> bool {
    set.id.service_name == service_name
        && set.entries.contains_key(platform)
        && matches(&set.selector, description)
}

/// Which Sets a rollout act would release to one Agent — **fit, aim, then version** (ADR-0052),
/// at most one Set per package name. Since ADR-0061 this computes the **candidates** the fleet
/// view shows as waiting and the bulk acts assign; it never composes an offer.
///
/// *Fit* comes first and cannot be switched off: a Set built for another Agent type (ADR-0034)
/// or another operating system or architecture (ADR-0031) is not a candidate for anyone; an
/// Agent that reports no platform or no type fits nothing.
///
/// *Aim* is ADR-0017 unchanged, over what is left, now ranking Sets: among candidates **sharing a
/// name**, the most specific Selector wins; among equally specific ones the **greater version**
/// wins, compared as ADR-0029 compares versions. A tie the version comparison cannot break is a
/// conflict — nothing is proposed under that name, and it is reported rather than guessed.
///
/// *Upgrade* is ADR-0076, and it runs with the fit: a Set whose version is not greater than what
/// the Agent reports installed under that name is no candidate at all, so it never enters the
/// ranking and never raises a conflict. Ranking what the Agent cannot receive would propose an
/// act that changes nothing.
///
/// Then the Baseline's own shape: every matching addon, and **one** top-level package across all
/// names — "normally only one top-level package", and a Supervisor has one binary to replace.
/// Between top-level winners of *different* names, the most specific Selector wins and an equal
/// tie is refused: different names are genuinely different packages, and no version can order
/// them.
fn resolve<'a>(
    sets: &'a BTreeMap<SetId, PackageSet>,
    description: Option<&AgentDescription>,
    installed: &InstalledVersions,
) -> Result<Vec<(&'a PackageSet, &'a Entry)>, String> {
    let Some(platform) = Platform::reported(description) else {
        return Ok(Vec::new());
    };
    // The Agent type this host presents (ADR-0033). Reporting none fits nothing, exactly as
    // reporting no platform does: "unknown type, so anything goes" is the hole ADR-0031 refused.
    let Some(service_name) = reported_service_name(description) else {
        return Ok(Vec::new());
    };
    let mut by_name: BTreeMap<&str, Vec<(&PackageSet, &Entry)>> = BTreeMap::new();
    for set in sets.values() {
        if !fits_and_aims(set, &platform, service_name, description) {
            continue;
        }
        if !upgrades(set, installed, description) {
            continue;
        }
        let entry = set
            .entries
            .get(&platform)
            .expect("the fit proved the entry");
        by_name.entry(&set.id.name).or_default().push((set, entry));
    }

    // Within one name: specificity, then version — at most one Set survives per name.
    let mut winners: Vec<(&PackageSet, &Entry)> = Vec::new();
    for (name, candidates) in by_name {
        let most_specific = candidates
            .iter()
            .map(|(set, _)| set.selector.len())
            .max()
            .expect("a name only exists with a candidate");
        let mut contenders = candidates
            .into_iter()
            .filter(|(set, _)| set.selector.len() == most_specific);
        let mut best = contenders.next().expect("at least one contender");
        for candidate in contenders {
            match opamp::version::precedence(&candidate.0.id.version, &best.0.id.version) {
                Some(std::cmp::Ordering::Greater) => best = candidate,
                Some(std::cmp::Ordering::Less) => {}
                // Equal or not orderable: the one case with no defensible answer. Offer nothing
                // under this name, and say why (ADR-0052).
                _ => {
                    return Err(format!(
                        "sets {} and {} of package {name:?} are equally specific for this Agent \
                         and their versions cannot be ordered; narrow one Selector or retract one \
                         set",
                        best.0.id, candidate.0.id
                    ));
                }
            }
        }
        winners.push(best);
    }

    let (top_level, addons): (Vec<_>, Vec<_>) =
        winners.into_iter().partition(|(set, _)| !set.addon);

    let mut chosen: Option<(&PackageSet, &Entry)> = None;
    for candidate in &top_level {
        match chosen {
            None => chosen = Some(*candidate),
            Some((current, _)) if candidate.0.selector.len() > current.selector.len() => {
                chosen = Some(*candidate)
            }
            Some((current, _)) if candidate.0.selector.len() == current.selector.len() => {
                return Err(format!(
                    "packages {:?} and {:?} are equally specific for this Agent; \
                     narrow one of their Selectors — an Agent has one binary to replace",
                    current.id.name, candidate.0.id.name
                ));
            }
            Some(_) => {}
        }
    }

    Ok(chosen.into_iter().chain(addons).collect())
}

/// The Agent type an Agent reports, as `service.name` (ADR-0033) — the identifying attribute the
/// Baseline reserves for "a reverse FQDN that uniquely identifies the Agent type".
///
/// `None` for an Agent that has not described itself or reports no type, which fits no Set
/// (ADR-0034). An empty value is `None` too: it is not a type.
fn reported_service_name(description: Option<&AgentDescription>) -> Option<&str> {
    opamp::attributes::string_value(
        &description?.identifying_attributes,
        opamp::attributes::SERVICE_NAME,
    )
}

/// The aggregate over all offered packages — name and content — in name order.
fn aggregate_hash(offered: &[(&PackageSet, &Entry)]) -> Vec<u8> {
    let mut sorted: Vec<&(&PackageSet, &Entry)> = offered.iter().collect();
    sorted.sort_by_key(|(set, _)| set.id.name.as_str());
    let mut hasher = Sha256::new();
    for (set, entry) in sorted {
        hasher.update((set.id.name.len() as u64).to_le_bytes());
        hasher.update(set.id.name.as_bytes());
        hasher.update(set.package_hash(entry));
    }
    hasher.finalize().to_vec()
}

// ---------------------------------------------------------------------------------------------
// Migration of a pre-ADR-0052 store: `<name>.json` rollout files and `<name>@<os>-<arch>.json` /
// `.bin` / `.previous.bin` variants in the store root become Set directories.
// ---------------------------------------------------------------------------------------------

/// A package's rollout as the pre-ADR-0052 store persisted it in `<name>.json`.
#[derive(Deserialize)]
struct LegacyPackageMeta {
    name: String,
    #[serde(default)]
    selector: BTreeMap<String, String>,
    #[serde(default)]
    service_name: String,
    #[serde(default = "legacy_published_default")]
    published: bool,
}

/// A file written before ADR-0043 had no publication state and was in flight: published.
fn legacy_published_default() -> bool {
    true
}

/// One artifact as the pre-ADR-0052 store persisted it in `<name>@<os>-<arch>.json`.
#[derive(Deserialize)]
struct LegacyVariantMeta {
    name: String,
    os: String,
    arch: String,
    version: String,
    #[serde(default)]
    addon: bool,
    content_hash_hex: String,
    #[serde(default)]
    signature_hex: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    source_headers: BTreeMap<String, String>,
    #[serde(default)]
    previous: Option<LegacyVersionMeta>,
}

/// The one remembered previous version (ADR-0019), which the migration turns into an
/// **unpublished** Set of that version — nothing an operator could roll back to is lost.
#[derive(Deserialize)]
struct LegacyVersionMeta {
    version: String,
    #[serde(default)]
    addon: bool,
    content_hash_hex: String,
    #[serde(default)]
    signature_hex: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    source_headers: BTreeMap<String, String>,
}

/// Migrates a pre-ADR-0052 store in place, at first open — loudly where it cannot (ADR-0052):
/// each stored package becomes one Set per distinct variant version; a package with **no Agent
/// type fails startup**, because the type is identity and inventing one would aim bytes this
/// Server cannot judge.
fn migrate_legacy(dir: &Path) -> Result<(), String> {
    let mut rollouts: BTreeMap<String, LegacyPackageMeta> = BTreeMap::new();
    let mut variants: Vec<(PathBuf, LegacyVariantMeta)> = Vec::new();
    let listing =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in listing {
        let path = entry
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
            .path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if stem.contains('@') {
            if !text.contains("\"content_hash_hex\"") {
                continue;
            }
            let meta: LegacyVariantMeta = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            variants.push((path, meta));
        } else {
            if text.contains("\"content_hash_hex\"") {
                return Err(format!(
                    "{}: this package was stored without an operating system and architecture, \
                     from before they were required. Delete the file and re-create the package \
                     as a Set — the Server will not offer an artifact it cannot fit to a machine",
                    path.display()
                ));
            }
            let Ok(meta) = serde_json::from_str::<LegacyPackageMeta>(&text) else {
                continue;
            };
            rollouts.insert(meta.name.clone(), meta);
        }
    }
    if variants.is_empty() {
        return Ok(());
    }
    info!(
        variants = variants.len(),
        "migrating a pre-ADR-0052 package store to Sets"
    );

    // What each migrated Set will hold, and whether any of its entries is the *current* version of
    // a variant — which is what carries the legacy publication state over.
    struct Migrated {
        selector: BTreeMap<String, String>,
        addon: bool,
        published: bool,
        entries: Vec<EntryMeta>,
        /// `(from, to)` artifact moves, executed when the Set is written.
        moves: Vec<(PathBuf, PathBuf)>,
    }
    let mut migrated: BTreeMap<SetId, Migrated> = BTreeMap::new();
    let mut consumed: Vec<PathBuf> = Vec::new();

    for (path, meta) in variants {
        let rollout = rollouts.get(&meta.name).ok_or_else(|| {
            format!(
                "{}: no {}.json rollout file for this artifact — delete it or restore the file",
                path.display(),
                meta.name
            )
        })?;
        if rollout.service_name.is_empty() {
            return Err(format!(
                "package {:?} has no Agent type, which is now part of a Set's identity \
                 (ADR-0052) — the store cannot migrate it. Delete its files under {} and \
                 re-create it as a Set stating the type",
                meta.name,
                dir.display()
            ));
        }
        let platform = Platform::new(&meta.os, &meta.arch)
            .map_err(|e| format!("invalid platform in {}: {e}", path.display()))?;
        let legacy_stem = format!("{}@{}", meta.name, platform.tag());

        let mut place = |version: &str,
                         addon: bool,
                         published: bool,
                         entry: EntryMeta,
                         artifact: Option<PathBuf>|
         -> Result<(), String> {
            let id = SetId::new(&meta.name, &rollout.service_name, version)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let target = migrated.entry(id.clone()).or_insert_with(|| Migrated {
                selector: rollout.selector.clone(),
                addon,
                published: false,
                entries: Vec::new(),
                moves: Vec::new(),
            });
            if target.addon != addon {
                return Err(format!(
                    "package {:?} version {version:?} mixes addon and top-level artifacts — \
                     delete one side and re-create it as its own Set",
                    meta.name
                ));
            }
            target.published |= published;
            if let Some(from) = artifact {
                target.moves.push((
                    from,
                    dir.join(id.dir_name())
                        .join(format!("{}.bin", platform.tag())),
                ));
            }
            target.entries.push(entry);
            Ok(())
        };

        // The current version of this variant.
        let current_artifact = meta
            .source_url
            .is_none()
            .then(|| dir.join(format!("{legacy_stem}.bin")));
        place(
            &meta.version,
            meta.addon,
            rollout.published,
            EntryMeta {
                os: platform.os.clone(),
                arch: platform.arch.clone(),
                content_hash_hex: meta.content_hash_hex.clone(),
                signature_hex: meta.signature_hex.clone(),
                source_url: meta.source_url.clone(),
                source_headers: meta.source_headers.clone(),
            },
            current_artifact,
        )?;

        // The remembered previous version (ADR-0019) becomes an unpublished Set of its own.
        if let Some(previous) = &meta.previous {
            let previous_artifact = previous
                .source_url
                .is_none()
                .then(|| dir.join(format!("{legacy_stem}.previous.bin")));
            place(
                &previous.version,
                previous.addon,
                false,
                EntryMeta {
                    os: platform.os.clone(),
                    arch: platform.arch.clone(),
                    content_hash_hex: previous.content_hash_hex.clone(),
                    signature_hex: previous.signature_hex.clone(),
                    source_url: previous.source_url.clone(),
                    source_headers: previous.source_headers.clone(),
                },
                previous_artifact,
            )?;
        }
        consumed.push(path);
        consumed.push(dir.join(format!("{}.json", meta.name)));
    }

    for (id, set) in migrated {
        let set_dir = dir.join(id.dir_name());
        std::fs::create_dir_all(&set_dir)
            .map_err(|e| format!("cannot create {}: {e}", set_dir.display()))?;
        for (from, to) in set.moves {
            std::fs::rename(&from, &to)
                .map_err(|e| format!("cannot move {} to {}: {e}", from.display(), to.display()))?;
        }
        let meta = SetMeta {
            name: id.name.clone(),
            service_name: id.service_name.clone(),
            version: id.version.clone(),
            selector: set.selector,
            published: set.published,
            addon: set.addon,
            entries: set.entries,
        };
        let json = serde_json::to_vec_pretty(&meta).expect("set serializes");
        std::fs::write(set_dir.join("set.json"), json)
            .map_err(|e| format!("cannot write {}: {e}", set_dir.join("set.json").display()))?;
        info!(set = %id, published = meta.published, "migrated to a package Set");
    }
    for path in consumed {
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("cannot delete {}: {e}", path.display()))?;
        }
    }
    Ok(())
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

    fn id(name: &str, version: &str) -> SetId {
        SetId::new(name, "otelcol", version).expect("set id")
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
    fn stored_set(store: &PackageStore, name: &str, version: &str, artifact: &[u8]) -> SetId {
        let id = id(name, version);
        store
            .create_or_update(&id, BTreeMap::new(), false)
            .expect("create");
        store
            .put_entry(&id, &linux(), None, artifact.to_vec())
            .expect("entry");
        id
    }

    /// The assignment map of an Agent the operator rolled these Sets out to.
    fn assigned(ids: &[&SetId]) -> BTreeMap<String, SetId> {
        ids.iter()
            .map(|id| (id.name.clone(), (*id).clone()))
            .collect()
    }

    /// What an Agent reports installed, as the record hands it to the store (ADR-0076).
    fn installed(versions: &[(&str, &str)]) -> InstalledVersions {
        versions
            .iter()
            .map(|(name, version)| ((*name).to_string(), (*version).to_string()))
            .collect()
    }

    /// The candidates a rollout act would release to an Agent that has installed nothing.
    fn candidates(store: &PackageStore, description: &AgentDescription) -> Vec<(String, String)> {
        candidates_for(store, description, &InstalledVersions::new())
    }

    /// The candidates a rollout act would release to this Agent, as `(name, version)`.
    fn candidates_for(
        store: &PackageStore,
        description: &AgentDescription,
        installed: &InstalledVersions,
    ) -> Vec<(String, String)> {
        let mut names: Vec<(String, String)> = store
            .candidate_ids(Some(description), installed)
            .expect("resolution")
            .into_iter()
            .map(|id| (id.name, id.version))
            .collect();
        names.sort();
        names
    }

    /// What this Agent is offered, given its assignments, as `(name, version)`.
    fn offered(
        store: &PackageStore,
        assigned: &BTreeMap<String, SetId>,
        description: &AgentDescription,
    ) -> Vec<(String, String)> {
        store
            .offer_for_assigned(assigned, Some(description), "", None)
            .map(|offer| {
                let mut names: Vec<(String, String)> = offer
                    .packages
                    .iter()
                    .map(|(name, p)| (name.clone(), p.version.clone()))
                    .collect();
                names.sort();
                names
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
            store
                .create_or_update(
                    &set,
                    BTreeMap::from([("ring".into(), "canary".into())]),
                    false,
                )
                .expect("create");
            store
                .put_entry(&set, &linux(), Some(vec![9, 9]), b"linux-bytes".to_vec())
                .expect("linux entry");
            store
                .set_entry_source(
                    &set,
                    &windows(),
                    vec![0u8; 32],
                    None,
                    Source {
                        url: "https://example.com/w.7z".into(),
                        headers: BTreeMap::new(),
                    },
                )
                .expect("windows entry");
        }
        let store = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        let summary = store.summary(&id("otelcol", "1.2.3")).expect("summary");
        assert_eq!(summary.selector["ring"], "canary");
        assert!(
            store.formerly_published().is_empty(),
            "a store born under ADR-0061 seeds no migration"
        );
        assert_eq!(summary.entries.len(), 2);
        assert_eq!(summary.entries[0].os, "linux");
        assert!(summary.entries[0].signed);
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
            offered(&store, &BTreeMap::new(), &agent("linux", "amd64", &[])).is_empty(),
            "no assignment, no offer"
        );
        assert_eq!(
            offered(&store, &assigned(&[&set]), &agent("linux", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())]
        );
    }

    /// The gate an explicit rollout act runs (ADR-0061): the Set must hold entries, fit the
    /// Agent's type and platform, and its Selector must match. The version *ranking* stays out —
    /// an Agent that has installed nothing takes the older Set as readily as the newer one.
    #[test]
    fn fits_agent_checks_fit_and_aim_but_not_the_ranking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let empty = id("otelcol", "0.9.0");
        store
            .create_or_update(&empty, BTreeMap::new(), false)
            .expect("create");
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
        store
            .set_selector(&old, BTreeMap::from([("ring".into(), "canary".into())]))
            .expect("aim");

        let ringed = agent("linux", "amd64", &[("ring", "canary")]);
        assert!(
            store
                .fits_agent(&old, Some(&ringed), &InstalledVersions::new())
                .is_ok(),
            "the older Set fits an Agent that runs nothing yet"
        );
        assert!(store
            .fits_agent(&new, Some(&ringed), &InstalledVersions::new())
            .is_ok());
        assert!(store
            .fits_agent(
                &old,
                Some(&agent("linux", "amd64", &[])),
                &InstalledVersions::new()
            )
            .expect_err("outside the ring")
            .contains("Selector"));
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
                Some(&ringed),
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

    /// The package status stays the authority where there is one (ADR-0079 point 1): it is a
    /// statement about *this package*, which a program's own number is not — an addon or a repacked
    /// tree may be numbered nothing like the program it belongs to.
    #[test]
    fn a_reported_package_version_wins_over_the_version_the_agent_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "2.0.0", b"v2");

        assert_eq!(
            candidates_for(
                &store,
                &running_agent("9.9.9"),
                &installed(&[("otelcol", "1.0.0")])
            ),
            [("otelcol".to_string(), "2.0.0".to_string())],
            "the package this Agent has is at 1.0.0, whatever the program calls itself"
        );
        assert!(
            candidates_for(
                &store,
                &running_agent("1.0.0"),
                &installed(&[("otelcol", "2.0.0")])
            )
            .is_empty(),
            "and a package already at 2.0.0 is not re-proposed because the program says otherwise"
        );
    }

    /// ADR-0079 point 3: the stand-in abstains where it cannot be ordered, rather than refusing.
    /// A GLPI Agent numbers itself `1.19` and an appliance `24.04.1`; failing closed on those would
    /// make a program's numbering habit into a fleet that cannot deliver to it at all.
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

    /// A Set that is no upgrade leaves the ranking altogether, so it cannot tie with one that is
    /// (ADR-0076): the conflict of two equally specific, unorderable versions disappears once the
    /// Agent has installed something neither of them beats.
    #[test]
    fn what_is_no_upgrade_never_raises_a_conflict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "nightly-a", b"a");
        stored_set(&store, "otelcol", "nightly-b", b"b");
        let host = agent("linux", "amd64", &[]);
        assert!(store
            .candidate_ids(Some(&host), &InstalledVersions::new())
            .expect_err("two versions nothing can order tie")
            .contains("cannot be ordered"));
        assert!(
            store
                .candidate_ids(Some(&host), &installed(&[("otelcol", "1.0.0")]))
                .expect("neither is an upgrade")
                .is_empty(),
            "neither can be ordered against what runs, so neither is ranked at all"
        );
    }

    /// The count beside the button needs both answers (ADR-0076 point 8): whom the Set aims at,
    /// version-blind, and whom it would actually reach. Aiming stays blind to what is installed
    /// and to the sibling ranking — it answers "is this Set aimed at anybody at all".
    #[test]
    fn aiming_at_is_version_blind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let old = stored_set(&store, "otelcol", "1.0.0", b"v1");
        let new = stored_set(&store, "otelcol", "2.0.0", b"v2");
        let host = agent("linux", "amd64", &[]);

        let mut aimed = store.aiming_at(Some(&host));
        aimed.sort();
        assert_eq!(aimed, vec![old, new.clone()], "both aim at this Agent");
        assert!(
            store
                .aiming_at(Some(&agent("windows", "amd64", &[])))
                .is_empty(),
            "no entry for the reported platform is the aim mistake worth seeing"
        );
        assert_eq!(
            store.aiming_at(Some(&host)).len(),
            2,
            "what the Agent already runs does not narrow the aim"
        );
        assert!(
            store
                .candidate_ids(Some(&host), &installed(&[("otelcol", "2.0.0")]))
                .expect("resolution")
                .is_empty(),
            "though it does narrow the reach"
        );
        let _ = new;
    }

    /// ADR-0052's candidate ladder within one name: the most specific Selector wins; among
    /// equally specific Sets the greater version wins. The offer itself follows the assignment
    /// alone — an Agent left assigned the older Set keeps being offered it (ADR-0061).
    #[test]
    fn specificity_wins_then_the_greater_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let stable = stored_set(&store, "otelcol", "1.0.0", b"v1");
        let canary = id("otelcol", "2.0.0");
        store
            .create_or_update(
                &canary,
                BTreeMap::from([("ring".into(), "canary".into())]),
                false,
            )
            .expect("create");
        store
            .put_entry(&canary, &linux(), None, b"v2".to_vec())
            .expect("entry");

        // The ring's candidate is the canary — more specific — everyone else's the stable one.
        assert_eq!(
            candidates(&store, &agent("linux", "amd64", &[("ring", "canary")])),
            [("otelcol".to_string(), "2.0.0".to_string())]
        );
        assert_eq!(
            candidates(&store, &agent("linux", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())]
        );

        // The Selector widens: the greater version becomes everyone's candidate.
        store.set_selector(&canary, BTreeMap::new()).expect("widen");
        assert_eq!(
            candidates(&store, &agent("linux", "amd64", &[])),
            [("otelcol".to_string(), "2.0.0".to_string())]
        );

        // The offer follows the assignment, not the ranking: an Agent still assigned the stable
        // Set keeps it, and assigning the stable Set anew is the rollback.
        assert_eq!(
            offered(&store, &assigned(&[&stable]), &agent("linux", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())]
        );
    }

    /// The tie the version comparison cannot break is a conflict: nothing is proposed under that
    /// name, and the refusal is reported rather than guessed (ADR-0052).
    #[test]
    fn versions_that_cannot_be_ordered_are_a_conflict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "nightly-a", b"a");
        stored_set(&store, "otelcol", "nightly-b", b"b");
        let err = store
            .candidate_ids(
                Some(&agent("linux", "amd64", &[])),
                &InstalledVersions::new(),
            )
            .expect_err("refused");
        assert!(err.contains("cannot be ordered"), "{err}");
    }

    /// Two equally specific top-level packages of *different* names still tie, as they always did
    /// (ADR-0017): no version can order genuinely different packages.
    #[test]
    fn equally_specific_names_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol-a", "1.0.0", b"a");
        stored_set(&store, "otelcol-b", "1.0.0", b"b");
        let err = store
            .candidate_ids(
                Some(&agent("linux", "amd64", &[])),
                &InstalledVersions::new(),
            )
            .expect_err("refused");
        assert!(err.contains("equally specific"), "{err}");
    }

    /// Fit before aim (ADR-0031, ADR-0034): an entry for another platform, or a Set for another
    /// Agent type, is never a candidate — and an Agent reporting neither fits nothing.
    #[test]
    fn fit_is_mandatory_platform_and_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        stored_set(&store, "otelcol", "1.0.0", b"linux-only");
        let foreign = SetId::new("promtail", "promtail", "1.0.0").expect("id");
        store
            .create_or_update(&foreign, BTreeMap::new(), false)
            .expect("create");
        store
            .put_entry(&foreign, &linux(), None, b"p".to_vec())
            .expect("entry");

        assert!(candidates(&store, &agent("windows", "amd64", &[])).is_empty());
        assert_eq!(
            candidates(&store, &agent("linux", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())],
            "the promtail set fits another type and is not a candidate"
        );
        assert!(
            store
                .candidate_ids(
                    Some(&AgentDescription::default()),
                    &InstalledVersions::new()
                )
                .expect("resolution")
                .is_empty(),
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
        store
            .create_or_update(&set, BTreeMap::new(), false)
            .expect("create");
        let mac = Platform::new("macos", "x86_64").expect("canonicalised");
        assert_eq!((mac.os.as_str(), mac.arch.as_str()), ("darwin", "amd64"));
        store
            .put_entry(&set, &mac, None, b"mac".to_vec())
            .expect("entry");
        assert_eq!(
            candidates(&store, &agent("darwin", "amd64", &[])),
            [("otelcol".to_string(), "1.0.0".to_string())]
        );
        assert_eq!(
            offered(&store, &assigned(&[&set]), &agent("darwin", "amd64", &[])),
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
                &assigned(&[&set]),
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
            "https://fleet.example/api/v1/packages/otelcol/otelcol/1.2.3/file?os=linux&arch=amd64"
        );
    }

    /// The aggregate hash is per Agent and follows its assignments (ADR-0061): it changes when
    /// the assigned Set changes, and is empty for an Agent assigned nothing it fits.
    #[test]
    fn the_aggregate_hash_is_per_agent_and_follows_the_assignment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let v1 = stored_set(&store, "otelcol", "1.0.0", b"v1");
        let before =
            store.assigned_hash_for(&assigned(&[&v1]), Some(&agent("linux", "amd64", &[])));
        assert!(!before.is_empty());
        assert!(store
            .assigned_hash_for(&assigned(&[&v1]), Some(&agent("windows", "amd64", &[])))
            .is_empty());
        assert!(store
            .assigned_hash_for(&BTreeMap::new(), Some(&agent("linux", "amd64", &[])))
            .is_empty());

        let v2 = stored_set(&store, "otelcol", "2.0.0", b"v2");
        let after = store.assigned_hash_for(&assigned(&[&v2]), Some(&agent("linux", "amd64", &[])));
        assert_ne!(before, after, "a new assigned version moves the aggregate");
    }

    /// Deleting an entry frees its artifact; deleting the Set takes the directory with it.
    #[test]
    fn deletion_frees_entries_and_sets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("open");
        let set = id("otelcol", "1.0.0");
        store
            .create_or_update(&set, BTreeMap::new(), false)
            .expect("create");
        store
            .put_entry(&set, &linux(), None, b"bytes".to_vec())
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
            store
                .create_or_update(&set, BTreeMap::new(), false)
                .expect("create");
            store
                .put_entry(&set, &linux(), None, b"good bytes".to_vec())
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
        store
            .create_or_update(&set, BTreeMap::new(), false)
            .expect("create");
        assert_eq!(
            std::fs::metadata(dir.path())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(dir.path().join(set.to_string()).join("set.json"))
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    /// ADR-0052's migration: a pre-Set store becomes one Set per variant version — the current
    /// version keeps its publication state, the remembered previous version (ADR-0019) becomes an
    /// **unpublished** Set, and nothing an operator could roll back to is lost.
    #[test]
    fn a_legacy_store_is_migrated_to_sets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let current = b"current-bytes";
        let previous = b"previous-bytes";
        std::fs::write(
            dir.path().join("otelcol.json"),
            serde_json::json!({
                "name": "otelcol",
                "selector": {"ring": "canary"},
                "service_name": "otelcol",
                "published": true
            })
            .to_string(),
        )
        .expect("rollout");
        std::fs::write(
            dir.path().join("otelcol@linux-amd64.json"),
            serde_json::json!({
                "name": "otelcol",
                "os": "linux",
                "arch": "amd64",
                "version": "2.0.0",
                "content_hash_hex": hex::encode(Sha256::digest(current)),
                "previous": {
                    "version": "1.0.0",
                    "content_hash_hex": hex::encode(Sha256::digest(previous)),
                }
            })
            .to_string(),
        )
        .expect("variant");
        std::fs::write(dir.path().join("otelcol@linux-amd64.bin"), current).expect("bin");
        std::fs::write(
            dir.path().join("otelcol@linux-amd64.previous.bin"),
            previous,
        )
        .expect("previous bin");

        let store = PackageStore::open(dir.path().to_path_buf()).expect("migrates");
        let migrated = store.summary(&id("otelcol", "2.0.0")).expect("current set");
        assert_eq!(migrated.selector["ring"], "canary");
        assert_eq!(
            store.formerly_published(),
            std::slice::from_ref(&id("otelcol", "2.0.0")),
            "what was in force seeds the assignment migration (ADR-0061 point 9)"
        );
        store
            .summary(&id("otelcol", "1.0.0"))
            .expect("the rollback target is kept as its own set");
        assert!(
            !dir.path().join("otelcol.json").exists(),
            "legacy files are gone"
        );
        // And both artifacts still verify: a second open re-hashes them — and the migration seed
        // survives the double hop, because the migrated set.json keeps its publication state.
        drop(store);
        let reopened = PackageStore::open(dir.path().to_path_buf()).expect("reopen clean");
        assert_eq!(
            reopened.formerly_published(),
            std::slice::from_ref(&id("otelcol", "2.0.0"))
        );
    }

    /// A legacy package without an Agent type cannot become a Set — the type is identity — and
    /// the store says which package is in the way rather than guessing (ADR-0052).
    #[test]
    fn a_legacy_package_without_a_type_refuses_to_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("otelcol.json"),
            serde_json::json!({"name": "otelcol"}).to_string(),
        )
        .expect("rollout");
        std::fs::write(
            dir.path().join("otelcol@linux-amd64.json"),
            serde_json::json!({
                "name": "otelcol",
                "os": "linux",
                "arch": "amd64",
                "version": "2.0.0",
                "content_hash_hex": hex::encode(Sha256::digest(b"bytes")),
            })
            .to_string(),
        )
        .expect("variant");
        std::fs::write(dir.path().join("otelcol@linux-amd64.bin"), b"bytes").expect("bin");
        let err = PackageStore::open(dir.path().to_path_buf())
            .map(|_| ())
            .expect_err("must refuse");
        assert!(err.contains("no Agent type"), "{err}");
    }

    /// The identity grammar keeps the triple a safe directory name and an unambiguous parse:
    /// `@` and path separators are refused.
    #[test]
    fn identity_tokens_are_bounded() {
        assert!(SetId::new("otelcol", "otelcol", "1.2.3-rc.1+abc").is_ok());
        assert!(SetId::new("otelcol", "a@b", "1.0.0").is_err());
        assert!(SetId::new("otelcol", "otelcol", "1.0.0/../evil").is_err());
        assert!(SetId::new("otelcol", "", "1.0.0").is_err());
        assert!(SetId::new("not a name", "otelcol", "1.0.0").is_err());
    }
}
