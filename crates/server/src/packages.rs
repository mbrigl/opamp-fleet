//! The package store (ADR-0015): the Server's software artifacts, mirroring the Configuration
//! store (ADR-0012). A package is a **name**, whom it targets, and one artifact per **Platform**
//! (ADR-0031) — version, type, the SHA-256 content hash, an optional Ed25519 signature — persisted
//! so a Server restart keeps offering what the fleet should run.
//!
//! Package *bodies* are opaque bytes: what a package contains and how it is applied is the Agent's
//! business (the specification forbids the Server abstracting over it). The Server's job is to
//! store, hash, offer, and serve — and to hand each Agent the artifact built for the machine it
//! runs on, never another one.

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

/// Spellings that mean a canonical token. Deliberately short: it exists for what this project does
/// **not** control — an older release file name, a foreign build system, an Agent that predates the
/// convention — not as a general vocabulary. Everything this project produces is already canonical.
const OS_ALIASES: &[(&str, &str)] = &[
    ("macos", "darwin"),
    ("osx", "darwin"),
    ("win", "windows"),
    ("win32", "windows"),
    ("win64", "windows"),
];

const ARCH_ALIASES: &[(&str, &str)] = &[
    ("x86_64", "amd64"),
    ("x86-64", "amd64"),
    ("x64", "amd64"),
    ("aarch64", "arm64"),
];

impl Platform {
    /// Canonicalises a spelling into a Platform.
    ///
    /// Unknown tokens are **not** refused, only normalised in case and checked for shape: the fleet
    /// may run a system this table has never heard of, and refusing to serve it would be a worse
    /// failure than serving it under its own name. What is refused is a token that could not be
    /// half of a file name, since that is what a variant is stored as.
    ///
    /// # Errors
    /// Returns an error when either token is empty, longer than 16 characters, or carries anything
    /// but lowercase letters, digits, and `_`.
    pub fn new(os: &str, arch: &str) -> Result<Self, String> {
        Ok(Platform {
            os: token(os, "os", OS_ALIASES)?,
            arch: token(arch, "arch", ARCH_ALIASES)?,
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
        let attribute = |key: &str| {
            description
                .non_identifying_attributes
                .iter()
                .chain(&description.identifying_attributes)
                .find(|kv| kv.key == key)
                .and_then(|kv| kv.value.as_ref())
                .and_then(|value| value.value.as_ref())
                .and_then(|value| match value {
                    opamp::proto::any_value::Value::StringValue(s) => Some(s.as_str()),
                    _ => None,
                })
        };
        Platform::new(attribute("os.type")?, attribute("host.arch")?).ok()
    }

    /// How this Platform is written in a file name and a query: `linux-amd64`.
    fn tag(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

fn token(raw: &str, what: &str, aliases: &[(&str, &str)]) -> Result<String, String> {
    let lowered = raw.trim().to_ascii_lowercase();
    let canonical = aliases
        .iter()
        .find(|(from, _)| *from == lowered)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or(lowered);
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

/// A stored package: a name, whom it targets, and its artifacts — **one per Platform** (ADR-0031).
///
/// The split is the decision: **the Selector aims, the Platform fits.** Which Agents this package
/// is for is one choice, held here for every variant at once; which bytes each of them gets is not
/// a choice at all but a property of their machine. So a fleet-wide Collector rollout is one
/// package with one Selector and five artifacts, not five packages an operator has to keep aligned.
///
/// Artifacts stay on disk (`<name>@<os>-<arch>.bin`) and are streamed to whoever asks — a program
/// weighs hundreds of megabytes, and a fleet server holding every one of them in memory, plus a
/// copy per download, is the shape this deliberately avoids.
#[derive(Clone)]
pub struct Package {
    /// The name a map key on the wire carries, so it follows the ADR-0010 grammar.
    pub name: String,
    /// The Selector (ADR-0012 semantics, ADR-0017): equality pairs that must all match an
    /// attribute the Agent reported. **Empty matches every Agent.** It belongs to the package, not
    /// to one artifact of it — every variant of a name is aimed at the same Agents.
    pub selector: BTreeMap<String, String>,
    /// One artifact per Platform. A package with none offers nothing and is not kept.
    pub variants: BTreeMap<Platform, Variant>,
}

/// One artifact of a package: everything that belongs to *bytes* rather than to the rollout.
#[derive(Clone)]
pub struct Variant {
    pub platform: Platform,
    pub version: String,
    /// `false` is the Baseline's `TopLevel` (a Managed Process's binary), `true` an `Addon`.
    pub addon: bool,
    /// SHA-256 of the artifact bytes.
    pub content_hash: Vec<u8>,
    /// Optional Ed25519 signature over the artifact, supplied by the operator.
    pub signature: Option<Vec<u8>>,
    /// The artifact's size in bytes, for the fleet view and the logs.
    pub size: u64,
    /// Where the artifact lives when it is **not** here (ADR-0018). `None` is an uploaded variant,
    /// whose bytes this Server holds and serves; `Some` is a reference, offered to Agents as the
    /// address it names — the Server never downloads it and has nothing to serve.
    pub source: Option<Source>,
    /// What this variant was before it was last replaced (ADR-0019), or `None` for one that has
    /// never been replaced. Exactly one step is remembered, and it is remembered **per Platform**:
    /// a rollout that reached Linux only must not push macOS back to a version it never left.
    pub previous: Option<Version>,
}

/// One version of a variant: everything needed to offer it, which is everything except the name,
/// the Selector, and the Platform — those belong to the package and to the machine, not to the
/// bytes it currently carries. Used to remember the version a variant replaced (ADR-0019).
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

impl Variant {
    /// This variant as persisted. The one place the on-disk shape is written, so a field added to
    /// a variant cannot be forgotten in one of the several ways one is stored.
    fn meta(&self, name: &str) -> VariantMeta {
        VariantMeta {
            name: name.to_string(),
            os: self.platform.os.clone(),
            arch: self.platform.arch.clone(),
            version: self.version.clone(),
            addon: self.addon,
            content_hash_hex: hex::encode(&self.content_hash),
            signature_hex: self.signature.as_ref().map(hex::encode),
            source_url: self.source.as_ref().map(|s| s.url.clone()),
            source_headers: self
                .source
                .as_ref()
                .map(|s| s.headers.clone())
                .unwrap_or_default(),
            previous: self.previous.as_ref().map(VersionMeta::of),
        }
    }

    /// What this variant currently is, as the descriptor another version can remember it by.
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
    /// is ambiguous. The Platform needs no place in it — two platforms' artifacts differ in their
    /// content hash by construction.
    fn package_hash(&self) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update([u8::from(self.addon)]);
        hasher.update((self.version.len() as u64).to_le_bytes());
        hasher.update(self.version.as_bytes());
        hasher.update(&self.content_hash);
        hasher.finalize().to_vec()
    }

    /// This variant as a wire `PackageAvailable`.
    ///
    /// An uploaded artifact is offered from this Server: `download_base` prefixes the artifact
    /// endpoint, and an empty prefix yields a path the Agent resolves against its own OpAMP
    /// endpoint. The Platform rides the query, because the name alone no longer names one file. A
    /// **referenced** artifact is offered as the address it names, with whatever headers the
    /// operator gave — the Baseline's Download Server "may be on the same host as the OpAMP Server
    /// or a different host", and this is that other host (ADR-0018).
    fn to_available(
        &self,
        name: &str,
        download_base: &str,
        headers: Option<Headers>,
    ) -> PackageAvailable {
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
                download_url: format!(
                    "{download_base}/api/v1/packages/{name}/file?os={}&arch={}",
                    self.platform.os, self.platform.arch
                ),
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

/// One package as the REST API lists it (ADR-0017, ADR-0031): whom it targets, and what it holds
/// for each platform.
pub struct PackageSummary {
    pub name: String,
    pub selector: BTreeMap<String, String>,
    /// One entry per Platform, in platform order.
    pub variants: Vec<VariantSummary>,
}

/// One artifact of a package as the REST API shows it — never its bytes.
pub struct VariantSummary {
    pub os: String,
    pub arch: String,
    pub version: String,
    pub addon: bool,
    pub size: u64,
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
            selector: package.selector.clone(),
            variants: package
                .variants
                .values()
                .map(|variant| VariantSummary {
                    os: variant.platform.os.clone(),
                    arch: variant.platform.arch.clone(),
                    version: variant.version.clone(),
                    addon: variant.addon,
                    size: variant.size,
                    source_url: variant.source.as_ref().map(|s| s.url.clone()),
                    previous_version: variant.previous.as_ref().map(|v| v.version.clone()),
                    previous_source_url: variant
                        .previous
                        .as_ref()
                        .and_then(|v| v.source.as_ref().map(|s| s.url.clone())),
                })
                .collect(),
        }
    }
}

/// A package's rollout, as persisted in `<name>.json`. It holds what belongs to the *name* — which
/// is only the Selector — so the aim cannot drift apart between one package's artifacts.
#[derive(Serialize, Deserialize)]
struct PackageMeta {
    name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    selector: BTreeMap<String, String>,
}

/// One artifact as persisted next to itself (`<name>@<os>-<arch>.json`); the artifact is
/// `<name>@<os>-<arch>.bin` and the version it replaced, when that one was uploaded too,
/// `<name>@<os>-<arch>.previous.bin`.
#[derive(Serialize, Deserialize)]
struct VariantMeta {
    name: String,
    os: String,
    arch: String,
    version: String,
    #[serde(default)]
    addon: bool,
    content_hash_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature_hex: Option<String>,
    /// The source of a referenced artifact (ADR-0018); absent for an uploaded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    source_headers: BTreeMap<String, String>,
    /// The version this artifact replaced (ADR-0019). Absent for one that never replaced anything.
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

/// The persistent package store: a `<name>.json` per package and a
/// `<name>@<os>-<arch>.json` + `.bin` pair per artifact under `packages_dir`, restored at startup.
/// The in-memory map is what the control loop reads.
pub struct PackageStore {
    dir: PathBuf,
    packages: RwLock<BTreeMap<String, Package>>,
}

impl PackageStore {
    /// Opens the store, creating the directory and loading every persisted package. A metadata or
    /// artifact file that cannot be read, does not parse, or whose artifact no longer matches its
    /// recorded hash is a startup error — a corrupt distribution artifact must never ship.
    ///
    /// A package stored **without a Platform** — everything written before ADR-0031 — is refused
    /// here rather than treated as fitting every machine. "The rollout is always platform-filtered"
    /// has to hold for what is already in the store, or it is not a guarantee an operator can rely
    /// on; so the Server says which file is in the way and what to do about it.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let mut selectors: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut variants: BTreeMap<String, BTreeMap<Platform, Variant>> = BTreeMap::new();
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
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();

            if !stem.contains('@') {
                // A package's rollout — or, if it names an artifact, a store written before
                // artifacts had a Platform.
                if text.contains("\"content_hash_hex\"") {
                    return Err(format!(
                        "{}: this package was stored without an operating system and architecture, \
                         from before they were required. Upload it again with `os` and `arch`, or \
                         delete the file — the Server will not offer an artifact it cannot fit to \
                         a machine",
                        path.display()
                    ));
                }
                let meta: PackageMeta = serde_json::from_str(&text)
                    .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
                validate_name(&meta.name)
                    .map_err(|e| format!("invalid package name in {}: {e}", path.display()))?;
                selectors.insert(meta.name, meta.selector);
                continue;
            }

            let meta: VariantMeta = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            validate_name(&meta.name)
                .map_err(|e| format!("invalid package name in {}: {e}", path.display()))?;
            let platform = Platform::new(&meta.os, &meta.arch)
                .map_err(|e| format!("invalid platform in {}: {e}", path.display()))?;
            let content_hash = hex::decode(&meta.content_hash_hex)
                .map_err(|e| format!("invalid content hash in {}: {e}", path.display()))?;
            let source = meta.source_url.clone().map(|url| Source {
                url,
                headers: meta.source_headers.clone(),
            });
            // An uploaded artifact is re-hashed by streaming, so a corrupt one never ships and the
            // check never depends on its size. A referenced one has nothing here to check: its
            // hash is the operator's word, verified by every Agent that downloads it (ADR-0018).
            let stem_of = variant_stem(&meta.name, &platform);
            let size = match &source {
                Some(_) => 0,
                None => {
                    let artifact_path = dir.join(format!("{stem_of}.bin"));
                    let (size, actual) = hash_file(&artifact_path)?;
                    if actual != content_hash {
                        return Err(format!(
                            "package {:?} for {}: artifact does not match its recorded content hash",
                            meta.name,
                            platform.tag()
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
                            let kept = dir.join(format!("{stem_of}.previous.bin"));
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
                                    "package {:?} for {}: the kept previous artifact does not \
                                     match its recorded content hash",
                                    meta.name,
                                    platform.tag()
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
            variants.entry(meta.name.clone()).or_default().insert(
                platform.clone(),
                Variant {
                    platform,
                    version: meta.version,
                    addon: meta.addon,
                    content_hash,
                    signature,
                    size,
                    source,
                    previous,
                },
            );
        }

        // A package is its artifacts: a rollout file left behind by the deletion of the last one
        // targets nothing and is simply not a package.
        let packages = variants
            .into_iter()
            .map(|(name, variants)| {
                let selector = selectors.get(&name).cloned().unwrap_or_default();
                (
                    name.clone(),
                    Package {
                        name,
                        selector,
                        variants,
                    },
                )
            })
            .collect();
        Ok(PackageStore {
            dir,
            packages: RwLock::new(packages),
        })
    }

    /// Every package's Selector and artifacts, in name order — the REST list view; never the
    /// artifact bytes.
    pub fn list(&self) -> Vec<PackageSummary> {
        self.packages
            .read()
            .expect("packages lock")
            .values()
            .map(PackageSummary::of)
            .collect()
    }

    /// Where one artifact lives, for the download endpoint to stream from. `None` when no package
    /// of that name holds one for that Platform.
    pub fn artifact_path(&self, name: &str, platform: &Platform) -> Option<PathBuf> {
        self.packages
            .read()
            .expect("packages lock")
            .get(name)?
            .variants
            .get(platform)
            // A referenced artifact is not served from here; the Agents were given its address.
            .filter(|variant| variant.source.is_none())
            .map(|_| {
                self.dir
                    .join(format!("{}.bin", variant_stem(name, platform)))
            })
    }

    /// `true` when the store holds no package — the Server then leaves `OffersPackages` undeclared.
    pub fn is_empty(&self) -> bool {
        self.packages.read().expect("packages lock").is_empty()
    }

    /// Where an upload is streamed to before it becomes an artifact. In the store's own directory,
    /// so [`put_staged`](Self::put_staged) can move it into place with a rename — and named per
    /// Platform, so uploading a release's five artifacts at once cannot have them overwrite each
    /// other while they are still in flight.
    ///
    /// # Errors
    /// Returns an error when the name is not a valid package name.
    pub fn staging_path(&self, name: &str, platform: &Platform) -> Result<PathBuf, String> {
        validate_name(name).map_err(|e| format!("invalid name {name:?}: {e}"))?;
        Ok(self
            .dir
            .join(format!("{}.upload", variant_stem(name, platform))))
    }

    /// Turns a streamed upload into an artifact: hashed by streaming, moved into place with a
    /// rename, then visible to the control loop. The artifact never passes through memory — an
    /// agent binary is far too big to buffer twice just to store it once.
    ///
    /// The staged file is consumed on success and removed on failure, so a rejected upload leaves
    /// nothing behind.
    pub fn put_staged(
        &self,
        name: String,
        platform: Platform,
        version: String,
        addon: bool,
        signature: Option<Vec<u8>>,
        staged: &Path,
    ) -> Result<(), String> {
        let result = self.store_staged(&name, &platform, &version, addon, signature, staged);
        if result.is_err() {
            let _ = std::fs::remove_file(staged);
        }
        result
    }

    fn store_staged(
        &self,
        name: &str,
        platform: &Platform,
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
            platform,
            version,
            addon,
            content_hash,
            signature,
            size,
            None,
            || {
                let artifact = self
                    .dir
                    .join(format!("{}.bin", variant_stem(name, platform)));
                std::fs::rename(staged, &artifact)
                    .map_err(|e| format!("cannot persist {}: {e}", artifact.display()))
            },
        )
    }

    /// The one path by which an artifact's bytes are replaced — an upload, a staged upload, or a
    /// source (ADR-0018). It is what keeps the three invariants that hold across all of them:
    ///
    /// - **The Selector survives.** Replacing bytes must never silently widen a targeted rollout
    ///   to the whole fleet (ADR-0017) — and a *new* platform of an existing package inherits that
    ///   package's aim, because the aim is the name's (ADR-0031).
    /// - **The version it replaced is remembered** (ADR-0019), per Platform, including its artifact
    ///   when that one was uploaded — so an operator can go one step back without producing the old
    ///   file again.
    /// - **Only this Platform moves.** The other artifacts of the same package are untouched.
    ///
    /// `install` puts the new artifact in place, once the one it displaces has been set aside.
    #[allow(clippy::too_many_arguments)]
    fn replace(
        &self,
        name: &str,
        platform: &Platform,
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
        let previous = existing
            .as_ref()
            .and_then(|p| p.variants.get(platform))
            .map(Variant::current);

        let stem = variant_stem(name, platform);
        // Set the displaced artifact aside before the new one lands on it. A referenced version
        // has no artifact here, and the file it would displace is simply gone.
        let artifact = self.dir.join(format!("{stem}.bin"));
        if artifact.exists() {
            let kept = self.dir.join(format!("{stem}.previous.bin"));
            std::fs::rename(&artifact, &kept)
                .map_err(|e| format!("cannot keep the previous artifact: {e}"))?;
        }
        install()?;

        let variant = Variant {
            platform: platform.clone(),
            version: version.to_string(),
            addon,
            content_hash,
            signature,
            size,
            source,
            previous,
        };
        let json =
            serde_json::to_vec_pretty(&variant.meta(name)).expect("variant metadata serializes");
        self.write_atomic(&format!("{stem}.json"), &json)?;
        // A package new to the store gets its rollout file too, so its Selector has somewhere to
        // live before anyone sets one.
        if existing.is_none() {
            let meta = PackageMeta {
                name: name.to_string(),
                selector: selector.clone(),
            };
            let json = serde_json::to_vec_pretty(&meta).expect("package metadata serializes");
            self.write_atomic(&format!("{name}.json"), &json)?;
        }

        let mut packages = self.packages.write().expect("packages lock");
        let package = packages.entry(name.to_string()).or_insert_with(|| Package {
            name: name.to_string(),
            selector,
            variants: BTreeMap::new(),
        });
        package.variants.insert(platform.clone(), variant);
        Ok(())
    }

    /// Puts one artifact back to the version it replaced (ADR-0019), which becomes the next version
    /// to go back to — so pressing this twice returns to where it started. The Selector is
    /// untouched: which Agents a package reaches is a separate decision from which bytes they get.
    ///
    /// It is per Platform on purpose. A rollout that reached Linux first and went badly is taken
    /// back on Linux; rolling the *name* back would push every other platform to a predecessor it
    /// never left.
    ///
    /// Distribution follows from state, as every package change does: matching Agents are offered
    /// the restored version on their next exchange.
    ///
    /// # Errors
    /// Returns an error when no package of that name holds an artifact for that Platform, or when
    /// that artifact has no previous version — the state of every one at its first upload.
    pub fn rollback(&self, name: &str, platform: &Platform) -> Result<(), String> {
        let mut packages = self.packages.write().expect("packages lock");
        let package = packages
            .get(name)
            .ok_or_else(|| format!("no package {name:?}"))?;
        let variant = package
            .variants
            .get(platform)
            .ok_or_else(|| format!("package {name:?} holds no artifact for {}", platform.tag()))?;
        let restore = variant.previous.clone().ok_or_else(|| {
            format!(
                "package {name:?} for {} has no previous version to go back to",
                platform.tag()
            )
        })?;
        let displaced = variant.current();

        // Swap the artifacts the two versions own. Either side may own none — a referenced version
        // keeps its bytes elsewhere — so the swap is over whichever files actually exist.
        let stem = variant_stem(name, platform);
        let artifact = self.dir.join(format!("{stem}.bin"));
        let kept = self.dir.join(format!("{stem}.previous.bin"));
        let swapping = self.dir.join(format!("{stem}.swap.tmp"));
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

        let variant = Variant {
            platform: platform.clone(),
            version: restore.version,
            addon: restore.addon,
            content_hash: restore.content_hash,
            signature: restore.signature,
            size: restore.size,
            source: restore.source,
            previous: Some(displaced),
        };
        let json =
            serde_json::to_vec_pretty(&variant.meta(name)).expect("variant metadata serializes");
        self.write_atomic(&format!("{stem}.json"), &json)?;
        packages
            .get_mut(name)
            .expect("the package was just looked up")
            .variants
            .insert(platform.clone(), variant);
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

    /// Points one artifact at a file that lives somewhere else (ADR-0018): no bytes are stored or
    /// fetched, and Agents are given `url` — with `headers`, when the source needs them — plus the
    /// `content_hash` the operator supplied, which is the only thing that will check what they
    /// receive.
    ///
    /// Creates the package when it does not exist, and replaces an uploaded artifact's bytes with
    /// the reference. An existing Selector is kept: re-pointing a targeted package must not widen
    /// it.
    ///
    /// # Errors
    /// Returns an error when the name is invalid, the hash is not a SHA-256, or the metadata cannot
    /// be persisted.
    #[allow(clippy::too_many_arguments)]
    pub fn set_source(
        &self,
        name: &str,
        platform: &Platform,
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
            platform,
            version,
            addon,
            content_hash,
            signature,
            0,
            Some(source),
            || Ok(()),
        )
    }

    /// Creates or replaces one artifact from bytes already in hand — the shape the tests and any
    /// small artifact use. A real upload takes [`put_staged`](Self::put_staged) instead.
    pub fn put(
        &self,
        name: String,
        platform: Platform,
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
        let stem = variant_stem(&name, &platform);
        // Bytes arrived: this artifact is held here now, whatever it referred to before.
        self.replace(
            &name,
            &platform,
            &version,
            addon,
            content_hash,
            signature,
            size,
            None,
            || self.write_atomic(&format!("{stem}.bin"), &artifact),
        )
    }

    /// Deletes a whole package — every platform's artifact and its rollout; `Ok(false)` when none
    /// of that name exists.
    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let mut packages = self.packages.write().expect("packages lock");
        let Some(package) = packages.remove(name) else {
            return Ok(false);
        };
        for platform in package.variants.keys() {
            self.remove_variant_files(name, platform)?;
        }
        self.remove_file(&self.dir.join(format!("{name}.json")))?;
        Ok(true)
    }

    /// Deletes one platform's artifact; `Ok(false)` when the package holds none for it. The last
    /// one taken away takes the package with it — a name with no artifacts offers nothing.
    pub fn delete_variant(&self, name: &str, platform: &Platform) -> Result<bool, String> {
        let mut packages = self.packages.write().expect("packages lock");
        let Some(package) = packages.get_mut(name) else {
            return Ok(false);
        };
        if package.variants.remove(platform).is_none() {
            return Ok(false);
        }
        self.remove_variant_files(name, platform)?;
        if package.variants.is_empty() {
            packages.remove(name);
            self.remove_file(&self.dir.join(format!("{name}.json")))?;
        }
        Ok(true)
    }

    fn remove_variant_files(&self, name: &str, platform: &Platform) -> Result<(), String> {
        let stem = variant_stem(name, platform);
        for suffix in ["json", "bin", "previous.bin"] {
            self.remove_file(&self.dir.join(format!("{stem}.{suffix}")))?;
        }
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<(), String> {
        if path.exists() {
            std::fs::remove_file(path)
                .map_err(|e| format!("cannot delete {}: {e}", path.display()))?;
        }
        Ok(())
    }

    /// One Agent's offer: the artifacts that fit its platform and whose Selector matches it, plus
    /// the `all_packages_hash` — the Baseline's "aggregate of all packages names and content" —
    /// over *that* set (ADR-0017, ADR-0031). `Ok(None)` when nothing matches, which is what an
    /// Agent outside every Selector must see; `Err` when the targeting is ambiguous and the Server
    /// refuses to guess.
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
                .map(|(name, variant)| {
                    (
                        (*name).to_string(),
                        variant.to_available(name, download_base, headers.clone()),
                    )
                })
                .collect(),
            all_packages_hash: aggregate_hash(&matching),
        }))
    }

    /// The aggregate hash for one Agent, to gate re-offering without building the whole message.
    /// Empty when nothing matches or the targeting is ambiguous — in both cases the Agent is
    /// offered nothing, and has nothing to be in sync with.
    pub fn all_packages_hash_for(&self, description: Option<&AgentDescription>) -> Vec<u8> {
        let packages = self.packages.read().expect("packages lock");
        match resolve(&packages, description) {
            Ok(matching) if !matching.is_empty() => aggregate_hash(&matching),
            _ => Vec::new(),
        }
    }

    /// Sets a package's Selector (ADR-0017) — for every platform of it at once, because the aim
    /// belongs to the name. Which Agents that newly reaches, or stops reaching, follows from state
    /// on their next exchange; nothing is pushed from here.
    pub fn set_selector(
        &self,
        name: &str,
        selector: BTreeMap<String, String>,
    ) -> Result<(), String> {
        let mut packages = self.packages.write().expect("packages lock");
        let package = packages
            .get_mut(name)
            .ok_or_else(|| format!("no package {name:?}"))?;
        let meta = PackageMeta {
            name: name.to_string(),
            selector: selector.clone(),
        };
        let json = serde_json::to_vec_pretty(&meta).expect("package metadata serializes");
        self.write_atomic(&format!("{name}.json"), &json)?;
        package.selector = selector;
        Ok(())
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.dir.join(name);
        let temp = self.dir.join(format!("{name}.tmp"));
        std::fs::write(&temp, bytes)
            .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path).map_err(|e| format!("cannot persist {}: {e}", path.display()))
    }
}

/// How one artifact's files are named: `<name>@<os>-<arch>`. The ADR-0010 name grammar admits
/// neither `@` nor `_`, and a canonical platform token admits neither `@` nor `-`, so the parts of
/// this never run together.
fn variant_stem(name: &str, platform: &Platform) -> String {
    format!("{name}@{}", platform.tag())
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

/// Which artifacts one Agent is offered — **fit, then aim** (ADR-0031).
///
/// *Fit* comes first and cannot be switched off: an artifact built for another operating system or
/// architecture is dropped before anything else is considered, and an Agent that reports no
/// platform at all fits nothing. A binary that cannot run on the machine it is sent to is not a
/// targeting mistake to be resolved by precedence — it is not a candidate.
///
/// *Aim* is then ADR-0017 unchanged, over what is left: every matching addon, and **one** top-level
/// package — the Baseline knows "normally only one top-level package", and a Supervisor has one
/// binary to replace. Where several top-level packages match, the **most specific Selector wins**:
/// the one naming the most attributes. That is what makes the pattern an operator actually wants
/// work — a fleet-wide package with an empty Selector, and a narrower one aimed at the hosts a
/// rollout starts on, which overrides it for exactly those. A tie between two equally specific
/// Selectors is the one case with no defensible answer, so it is refused and reported rather than
/// guessed. Two platforms of *one* package can never tie: only one of them fits.
fn resolve<'a>(
    packages: &'a BTreeMap<String, Package>,
    description: Option<&AgentDescription>,
) -> Result<Vec<(&'a str, &'a Variant)>, String> {
    let Some(platform) = Platform::reported(description) else {
        return Ok(Vec::new());
    };
    let fitting: Vec<(&Package, &Variant)> = packages
        .values()
        .filter(|p| matches(&p.selector, description))
        .filter_map(|p| p.variants.get(&platform).map(|variant| (p, variant)))
        .collect();

    let (top_level, addons): (Vec<_>, Vec<_>) = fitting.into_iter().partition(|(_, v)| !v.addon);

    let mut chosen: Option<(&Package, &Variant)> = None;
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
                    current.name, candidate.0.name
                ));
            }
            Some(_) => {}
        }
    }

    Ok(chosen
        .into_iter()
        .chain(addons)
        .map(|(package, variant)| (package.name.as_str(), variant))
        .collect())
}

/// The aggregate over all offered packages — name and content — in name order.
fn aggregate_hash(offered: &[(&str, &Variant)]) -> Vec<u8> {
    let mut sorted: Vec<&(&str, &Variant)> = offered.iter().collect();
    sorted.sort_by_key(|(name, _)| *name);
    let mut hasher = Sha256::new();
    for (name, variant) in sorted {
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(variant.package_hash());
    }
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

    /// An Agent description reporting a platform, plus whatever else a Selector should see.
    fn agent(os: &str, arch: &str, extra: &[(&str, &str)]) -> AgentDescription {
        let attr = |key: &str, value: &str| KeyValue {
            key: key.to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(value.to_string())),
            }),
        };
        let mut non_identifying = vec![attr("os.type", os), attr("host.arch", arch)];
        non_identifying.extend(extra.iter().map(|(k, v)| attr(k, v)));
        AgentDescription {
            identifying_attributes: vec![attr("service.name", "otelcol")],
            non_identifying_attributes: non_identifying,
        }
    }

    /// Why the store refused to open. `PackageStore` is not `Debug` — it holds a lock — so the
    /// error is unwrapped by hand rather than through `expect_err`.
    fn open_err(dir: &std::path::Path) -> String {
        match PackageStore::open(dir.to_path_buf()) {
            Ok(_) => panic!("the store should have refused to open"),
            Err(e) => e,
        }
    }

    fn offered(store: &PackageStore, description: &AgentDescription) -> Vec<(String, String)> {
        match store.offer_for(Some(description), "", None) {
            Ok(Some(offer)) => {
                let mut names: Vec<(String, String)> = offer
                    .packages
                    .into_iter()
                    .map(|(name, available)| (name, available.version))
                    .collect();
                names.sort();
                names
            }
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_package_holds_one_artifact_per_platform_and_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        store
            .put(
                "otelcol".to_string(),
                linux(),
                "1.0.0".to_string(),
                false,
                None,
                b"linux-binary".to_vec(),
            )
            .expect("put linux");
        store
            .put(
                "otelcol".to_string(),
                windows(),
                "1.0.0".to_string(),
                false,
                None,
                b"windows-binary".to_vec(),
            )
            .expect("put windows");

        // One package, two artifacts — not two packages.
        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].variants.len(), 2);

        let reopened = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        let summary = reopened.summary("otelcol").expect("the package survives");
        assert_eq!(summary.variants.len(), 2);
        let platforms: Vec<String> = summary
            .variants
            .iter()
            .map(|v| format!("{}-{}", v.os, v.arch))
            .collect();
        assert_eq!(platforms, vec!["linux-amd64", "windows-amd64"]);
    }

    /// The decision this exists for: an artifact built for another machine is not a targeting
    /// mistake to be resolved by precedence — it is not a candidate at all (ADR-0031).
    #[test]
    fn an_agent_is_offered_the_artifact_that_fits_it_and_never_another() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        for (platform, bytes, version) in [
            (linux(), b"linux-binary".to_vec(), "1.0.0-linux"),
            (windows(), b"windows-binary".to_vec(), "1.0.0-windows"),
        ] {
            store
                .put(
                    "otelcol".to_string(),
                    platform,
                    version.to_string(),
                    false,
                    None,
                    bytes,
                )
                .expect("put");
        }

        assert_eq!(
            offered(&store, &agent("linux", "amd64", &[])),
            vec![("otelcol".to_string(), "1.0.0-linux".to_string())]
        );
        assert_eq!(
            offered(&store, &agent("windows", "amd64", &[])),
            vec![("otelcol".to_string(), "1.0.0-windows".to_string())]
        );
        // A platform nothing was built for is offered nothing at all, rather than whatever else
        // happened to be stored under the name.
        assert!(offered(&store, &agent("darwin", "arm64", &[])).is_empty());
    }

    /// Fit is not optional, so an Agent that reports no platform fits nothing. Guessing here would
    /// put the mismatched-binary failure straight back.
    #[test]
    fn an_agent_that_reports_no_platform_is_offered_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        store
            .put(
                "otelcol".to_string(),
                linux(),
                "1.0.0".to_string(),
                false,
                None,
                b"binary".to_vec(),
            )
            .expect("put");

        let mut silent = agent("linux", "amd64", &[]);
        silent.non_identifying_attributes.clear();
        assert!(offered(&store, &silent).is_empty());
        assert!(store.all_packages_hash_for(Some(&silent)).is_empty());
        assert!(store.offer_for(None, "", None).expect("no error").is_none());
    }

    /// Three naming worlds meet at this comparison: a release file name (`macos`), Rust's constant
    /// (`x86_64`) and the convention (`darwin`/`amd64`). Both sides are canonicalised, so an
    /// upload spelled one way still fits an Agent spelling it another.
    #[test]
    fn both_sides_of_the_comparison_are_canonicalised() {
        assert_eq!(Platform::new("macOS", "x86_64").expect("platform"), {
            Platform::new("darwin", "amd64").expect("platform")
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        store
            .put(
                "otelcol".to_string(),
                Platform::new("macos", "aarch64").expect("platform"),
                "1.0.0".to_string(),
                false,
                None,
                b"binary".to_vec(),
            )
            .expect("put");

        // Stored under a release file name's spelling; found by an Agent reporting the convention.
        assert_eq!(
            offered(&store, &agent("darwin", "arm64", &[])),
            vec![("otelcol".to_string(), "1.0.0".to_string())]
        );
        // …and by one still reporting Rust's spelling, which is what a Client did before ADR-0031.
        assert_eq!(
            offered(&store, &agent("darwin", "aarch64", &[])),
            vec![("otelcol".to_string(), "1.0.0".to_string())]
        );
        // What the API answers with is the canonical pair, so a typo shows up in the response.
        let summary = store.summary("otelcol").expect("summary");
        assert_eq!(summary.variants[0].os, "darwin");
        assert_eq!(summary.variants[0].arch, "arm64");
    }

    /// A token that could not be half of a file name is refused; one this table simply does not
    /// know is not — the fleet may run a system nobody here has heard of.
    #[test]
    fn an_unknown_platform_is_served_but_an_unusable_token_is_refused() {
        assert_eq!(
            Platform::new("freebsd", "ppc64").expect("an unknown platform is usable"),
            Platform {
                os: "freebsd".to_string(),
                arch: "ppc64".to_string()
            }
        );
        assert!(Platform::new("linux", "arm-v7").is_err(), "'-' separates");
        assert!(Platform::new("linux", "").is_err());
        assert!(Platform::new("li/nux", "amd64").is_err());
    }

    /// The Selector aims, the Platform fits: aiming a package moves every one of its artifacts,
    /// and a platform added later inherits the aim rather than starting fleet-wide.
    #[test]
    fn the_selector_belongs_to_the_name_and_the_platform_to_the_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        store
            .put(
                "otelcol".to_string(),
                linux(),
                "1.0.0".to_string(),
                false,
                None,
                b"linux".to_vec(),
            )
            .expect("put");
        store
            .set_selector(
                "otelcol",
                [("env".to_string(), "canary".to_string())].into(),
            )
            .expect("selector");
        store
            .put(
                "otelcol".to_string(),
                windows(),
                "1.0.0".to_string(),
                false,
                None,
                b"windows".to_vec(),
            )
            .expect("put a second platform");

        // The Windows artifact was uploaded after the Selector and is aimed the same way.
        assert!(offered(&store, &agent("windows", "amd64", &[])).is_empty());
        assert_eq!(
            offered(&store, &agent("windows", "amd64", &[("env", "canary")])).len(),
            1
        );
        let reopened = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(
            reopened.summary("otelcol").expect("summary").selector.len(),
            1,
            "the aim survives a restart"
        );
    }

    /// ADR-0017's specificity rule, unchanged, over what fits. Two platforms of one package can
    /// never tie, because only one of them is ever a candidate.
    #[test]
    fn a_narrower_selector_wins_and_an_equally_specific_one_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        for (name, version) in [("fleetwide", "1.0.0"), ("canary", "2.0.0")] {
            store
                .put(
                    name.to_string(),
                    linux(),
                    version.to_string(),
                    false,
                    None,
                    format!("{name}-binary").into_bytes(),
                )
                .expect("put");
        }
        store
            .set_selector("canary", [("env".to_string(), "canary".to_string())].into())
            .expect("selector");

        assert_eq!(
            offered(&store, &agent("linux", "amd64", &[])),
            vec![("fleetwide".to_string(), "1.0.0".to_string())]
        );
        assert_eq!(
            offered(&store, &agent("linux", "amd64", &[("env", "canary")])),
            vec![("canary".to_string(), "2.0.0".to_string())],
            "the narrower Selector overrides the fleet-wide package"
        );

        // Equally specific: refused and reported, never guessed.
        store
            .set_selector(
                "fleetwide",
                [("env".to_string(), "canary".to_string())].into(),
            )
            .expect("selector");
        let refused = store.offer_for(
            Some(&agent("linux", "amd64", &[("env", "canary")])),
            "",
            None,
        );
        assert!(refused.is_err(), "{refused:?}");
    }

    /// ADR-0019 per platform — the point of making the rollback name one. A canary that reached
    /// Linux and went badly is taken back there, and nowhere else.
    #[test]
    fn a_rollback_moves_only_the_platform_it_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        let put = |platform: Platform, version: &str, bytes: &[u8]| {
            store
                .put(
                    "otelcol".to_string(),
                    platform,
                    version.to_string(),
                    false,
                    None,
                    bytes.to_vec(),
                )
                .expect("put")
        };
        put(linux(), "1.0.0", b"linux-old");
        put(windows(), "1.0.0", b"windows-old");
        put(linux(), "2.0.0", b"linux-new");

        store.rollback("otelcol", &linux()).expect("rollback");

        assert_eq!(
            offered(&store, &agent("linux", "amd64", &[])),
            vec![("otelcol".to_string(), "1.0.0".to_string())]
        );
        assert_eq!(
            offered(&store, &agent("windows", "amd64", &[])),
            vec![("otelcol".to_string(), "1.0.0".to_string())],
            "Windows never took 2.0.0 and must not be pushed off what it runs"
        );
        // The restored bytes are what is served, and the swap is one step: back again returns.
        let path = store
            .artifact_path("otelcol", &linux())
            .expect("an artifact");
        assert_eq!(std::fs::read(&path).expect("read"), b"linux-old");
        store.rollback("otelcol", &linux()).expect("rollback again");
        assert_eq!(std::fs::read(&path).expect("read"), b"linux-new");

        // A platform that never replaced anything has nothing to go back to.
        let err = store
            .rollback("otelcol", &windows())
            .expect_err("nothing to go back to");
        assert!(err.contains("no previous version"), "{err}");
        // Nor does a platform the package does not hold.
        let err = store
            .rollback("otelcol", &Platform::new("darwin", "arm64").expect("p"))
            .expect_err("no such artifact");
        assert!(err.contains("holds no artifact"), "{err}");
    }

    /// A corrupt artifact must never ship, so it is caught at startup rather than on an Agent —
    /// and the message names the platform, since a package now has several artifacts to blame.
    #[test]
    fn a_corrupt_artifact_fails_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        store
            .put(
                "otelcol".to_string(),
                linux(),
                "1.0.0".to_string(),
                false,
                None,
                b"binary".to_vec(),
            )
            .expect("put");
        std::fs::write(dir.path().join("otelcol@linux-amd64.bin"), b"tampered").expect("tamper");

        let err = open_err(dir.path());
        assert!(err.contains("linux-amd64"), "{err}");
        assert!(err.contains("content hash"), "{err}");
    }

    /// Strict, with no legacy: a package stored before artifacts had a platform is refused at
    /// startup rather than silently treated as fitting every machine, and the message says what to
    /// do about it (ADR-0031).
    #[test]
    fn a_package_stored_without_a_platform_refuses_to_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("otelcol.json"),
            br#"{"name":"otelcol","version":"1.0.0","content_hash_hex":"00"}"#,
        )
        .expect("write a pre-ADR-0031 package");

        let err = open_err(dir.path());
        assert!(err.contains("without an operating system"), "{err}");
        assert!(err.contains("Upload it again"), "{err}");
    }

    /// The aggregate is over what *this* Agent is offered, so two platforms of one package do not
    /// share one — otherwise the gate that stops re-offering would fire on the wrong set.
    #[test]
    fn the_aggregate_hash_is_per_agent_and_follows_the_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        store
            .put(
                "otelcol".to_string(),
                linux(),
                "1.0.0".to_string(),
                false,
                None,
                b"linux".to_vec(),
            )
            .expect("put");
        store
            .put(
                "otelcol".to_string(),
                windows(),
                "1.0.0".to_string(),
                false,
                None,
                b"windows".to_vec(),
            )
            .expect("put");

        let on_linux = store.all_packages_hash_for(Some(&agent("linux", "amd64", &[])));
        let on_windows = store.all_packages_hash_for(Some(&agent("windows", "amd64", &[])));
        assert!(!on_linux.is_empty());
        assert_ne!(
            on_linux, on_windows,
            "different bytes are a different offer, even under one name"
        );

        // Replacing one platform's bytes moves that Agent's aggregate and leaves the other's.
        store
            .put(
                "otelcol".to_string(),
                linux(),
                "2.0.0".to_string(),
                false,
                None,
                b"linux-new".to_vec(),
            )
            .expect("put");
        assert_ne!(
            store.all_packages_hash_for(Some(&agent("linux", "amd64", &[]))),
            on_linux
        );
        assert_eq!(
            store.all_packages_hash_for(Some(&agent("windows", "amd64", &[]))),
            on_windows
        );
    }

    /// The offer names where to fetch each artifact — including which platform's, since the name
    /// alone no longer names one file.
    #[test]
    fn the_offer_carries_a_download_url_naming_the_platform() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        store
            .put(
                "otelcol".to_string(),
                linux(),
                "1.0.0".to_string(),
                false,
                None,
                b"binary".to_vec(),
            )
            .expect("put");

        let offer = store
            .offer_for(Some(&agent("linux", "amd64", &[])), "https://fleet", None)
            .expect("no error")
            .expect("an offer");
        let file = offer.packages["otelcol"]
            .file
            .as_ref()
            .expect("a downloadable file");
        assert_eq!(
            file.download_url,
            "https://fleet/api/v1/packages/otelcol/file?os=linux&arch=amd64"
        );
    }

    /// Deleting one platform leaves the others; deleting the last one takes the package with it,
    /// because a name with nothing to offer is not a package.
    #[test]
    fn deleting_the_last_artifact_deletes_the_package() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::open(dir.path().to_path_buf()).expect("store");
        for platform in [linux(), windows()] {
            store
                .put(
                    "otelcol".to_string(),
                    platform,
                    "1.0.0".to_string(),
                    false,
                    None,
                    b"binary".to_vec(),
                )
                .expect("put");
        }

        assert!(store.delete_variant("otelcol", &linux()).expect("delete"));
        assert_eq!(
            store
                .summary("otelcol")
                .expect("still there")
                .variants
                .len(),
            1
        );
        assert!(store.delete_variant("otelcol", &windows()).expect("delete"));
        assert!(store.summary("otelcol").is_none());
        assert!(store.is_empty());

        // And nothing is left on disk to be restored by a reopen.
        let reopened = PackageStore::open(dir.path().to_path_buf()).expect("reopen");
        assert!(reopened.is_empty());
    }
}
