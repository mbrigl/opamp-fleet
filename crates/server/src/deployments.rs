//! Deployments (ADR-0096): what reaches a channel of hosts, and the only thing rolled out.
//!
//! A Package is what an Agent type runs at a version and nothing else (ADR-0095) — no aim, no
//! signature, no act of its own. All three live here. A Deployment carries a **name**, a
//! **Selector** over the channel it addresses, **one Package per Agent type**, and the **signature**
//! of each artifact it offers.
//!
//! Two rules give the object its shape, and both are refusals:
//!
//! **An Agent belongs to at most one Deployment.** Where two match, that is a conflict and the
//! Agent is offered nothing new — not the most specific, not the newest, none. ADR-0017's
//! specificity ranking is withdrawn with no successor: it decided "which artifact does this host
//! get" by a computation across every stored object, which is an answer no operator could read off
//! anything. A refusal that names both Deployments is worse for nobody and legible to everyone.
//!
//! **A Selector is never empty.** An empty one is the channel that collides with every other, and a
//! forgotten field would quietly become the base for the whole fleet — the class of accident
//! ADR-0061 was built to prevent. Channels are therefore a *partition*: a Selector cannot express
//! "not", so disjoint channels come from membership, which is what ADR-0042's labels already are.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use opamp::proto::AgentDescription;
use serde::{Deserialize, Serialize};

use crate::configs::{matches, validate_name};
use crate::packages::{PackageId, Platform};

/// A named set of Packages, aimed at a channel and carrying each artifact's signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    /// The operator's name for this channel. The one human-chosen label in the model, which is why
    /// it keeps the ADR-0010 grammar a Package gave up (ADR-0095).
    pub name: String,
    /// Equality pairs that must all match an attribute the Agent reported, labels included
    /// (ADR-0012 semantics, unchanged). **Never empty** — see the module note.
    pub selector: BTreeMap<String, String>,
    /// At most one Package per Agent type, keyed by that type. Two of one type would collide on
    /// the wire map key *and* fit the same Agent, so the second is refused at the moment it is
    /// written rather than puzzled over at resolution.
    pub packages: BTreeMap<String, PackageId>,
    /// The Ed25519 signature of one artifact, per `(Package, Platform)`. Held here rather than on
    /// the entry because what an operator signs off on is a release to a set of machines, not a
    /// pile of bytes; the same Package in two Deployments is signed in each.
    pub signatures: BTreeMap<(PackageId, Platform), Vec<u8>>,
}

impl Deployment {
    /// The signature to offer with one Package's artifact, if this Deployment holds one.
    pub fn signature(&self, id: &PackageId, platform: &Platform) -> Option<&[u8]> {
        self.signatures
            .get(&(id.clone(), platform.clone()))
            .map(Vec::as_slice)
    }

    /// The Package this Deployment holds for an Agent of `agent_type`, if any.
    pub fn package_for(&self, agent_type: &str) -> Option<&PackageId> {
        self.packages.get(agent_type)
    }
}

/// Why a write against a Deployment was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum DeploymentError {
    /// The name, the Selector, or an identity did not pass its grammar — a `400`.
    Invalid(String),
    /// No Deployment of that name, or it holds no such Package or signature — a `404`.
    NotFound,
    /// The Deployment already holds a Package for that Agent type — a `409`.
    TypeTaken { agent_type: String, held: PackageId },
    /// The write collides with what this channel has already released — also a `409`.
    Conflict(String),
    /// The store could not be written.
    Storage(String),
}

impl std::fmt::Display for DeploymentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeploymentError::Invalid(why) => write!(f, "{why}"),
            DeploymentError::NotFound => write!(f, "no such deployment"),
            DeploymentError::TypeTaken { agent_type, held } => write!(
                f,
                "this deployment already holds {held} for Agent type {agent_type:?} — an Agent has \
                 one binary to replace, so remove that one first or use another deployment"
            ),
            DeploymentError::Conflict(why) => write!(f, "{why}"),
            DeploymentError::Storage(why) => write!(f, "{why}"),
        }
    }
}

/// A Deployment as persisted: `<packages_dir>/deployments/<name>.json`.
///
/// The signatures are a **list** rather than a map, because their key is a pair and JSON keys are
/// strings. Folding them into the in-memory map on load keeps the uniqueness where it belongs
/// without inventing a composite key nobody would read.
#[derive(Serialize, Deserialize)]
struct DeploymentMeta {
    name: String,
    selector: BTreeMap<String, String>,
    #[serde(default)]
    packages: Vec<PackageRefMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    signatures: Vec<SignatureMeta>,
}

#[derive(Serialize, Deserialize)]
struct PackageRefMeta {
    agent_type: String,
    version: String,
}

#[derive(Serialize, Deserialize)]
struct SignatureMeta {
    agent_type: String,
    version: String,
    os: String,
    arch: String,
    signature_hex: String,
}

impl DeploymentMeta {
    fn of(deployment: &Deployment) -> Self {
        DeploymentMeta {
            name: deployment.name.clone(),
            selector: deployment.selector.clone(),
            packages: deployment
                .packages
                .values()
                .map(|id| PackageRefMeta {
                    agent_type: id.agent_type.clone(),
                    version: id.version.clone(),
                })
                .collect(),
            signatures: deployment
                .signatures
                .iter()
                .map(|((id, platform), signature)| SignatureMeta {
                    agent_type: id.agent_type.clone(),
                    version: id.version.clone(),
                    os: platform.os.clone(),
                    arch: platform.arch.clone(),
                    signature_hex: hex::encode(signature),
                })
                .collect(),
        }
    }

    fn into_deployment(self, path: &std::path::Path) -> Result<Deployment, String> {
        let named = |e: String| format!("invalid deployment in {}: {e}", path.display());
        validate_name(&self.name).map_err(|e| named(format!("name {:?}: {e}", self.name)))?;
        check_selector(&self.selector).map_err(named)?;
        let mut packages = BTreeMap::new();
        for reference in self.packages {
            let id = PackageId::new(&reference.agent_type, &reference.version).map_err(named)?;
            packages.insert(id.agent_type.clone(), id);
        }
        let mut signatures = BTreeMap::new();
        for signature in self.signatures {
            let id = PackageId::new(&signature.agent_type, &signature.version).map_err(named)?;
            let platform = Platform::new(&signature.os, &signature.arch).map_err(named)?;
            let bytes = hex::decode(&signature.signature_hex)
                .map_err(|e| named(format!("signature of {id}: {e}")))?;
            signatures.insert((id, platform), bytes);
        }
        Ok(Deployment {
            name: self.name,
            selector: self.selector,
            packages,
            signatures,
        })
    }
}

/// Whether a Selector may aim a Deployment: it must name at least one pair, and no pair may be
/// blank.
///
/// The emptiness rule is the load-bearing one. An empty Selector matches every Agent, so it would
/// collide with every other Deployment and make the one-Deployment-per-Agent rule unsatisfiable
/// the moment a second channel exists — and it is what a forgotten field looks like.
pub fn check_selector(selector: &BTreeMap<String, String>) -> Result<(), String> {
    if selector.is_empty() {
        return Err(
            "a deployment must name the channel it aims at: give its Selector at least one pair, \
             such as `channel = \"stable\"`, `region = \"eu-central\"` or `tenant = \"acme\"` \
             — the key is yours to invent, this Server prescribes none. There is no fleet-wide \
             default: two deployments matching one Agent is a conflict, and an empty Selector \
             matches everything"
                .to_string(),
        );
    }
    for (key, value) in selector {
        if key.trim().is_empty() || value.trim().is_empty() {
            return Err(format!(
                "the Selector pair {key:?} = {value:?} has an empty half — a Selector is equality \
                 over reported attributes, and neither side can be blank"
            ));
        }
    }
    Ok(())
}

/// The Deployment one Agent belongs to.
///
/// `Ok(None)` — no channel claims it; it waits, which after a fresh enrolment is the ordinary state.
/// `Err` — **two or more** claim it. That is the conflict, and the message names them all, because
/// a rollout that silently never starts is worse than one that explains itself.
pub fn deployment_for<'a>(
    deployments: &'a BTreeMap<String, Deployment>,
    description: Option<&AgentDescription>,
) -> Result<Option<&'a Deployment>, String> {
    let mut claiming = deployments
        .values()
        .filter(|deployment| matches(&deployment.selector, description));
    let Some(first) = claiming.next() else {
        return Ok(None);
    };
    let rest: Vec<&str> = claiming.map(|d| d.name.as_str()).collect();
    if rest.is_empty() {
        return Ok(Some(first));
    }
    let mut names: Vec<&str> = vec![first.name.as_str()];
    names.extend(rest);
    Err(format!(
        "deployments {} all match this Agent — an Agent belongs to at most one, so narrow their \
         Selectors until exactly one claims it",
        names
            .iter()
            .map(|n| format!("{n:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The persistent Deployment store: one JSON file per Deployment under
/// `<packages_dir>/deployments/`.
///
/// It lives beside the Packages rather than beside the Configurations because a Deployment is
/// meaningless without the artifacts it signs — one directory is one backup — and it needs no
/// configuration key of its own: it is armed by `packages_dir`, exactly as the package store is.
pub struct DeploymentStore {
    dir: PathBuf,
    deployments: RwLock<BTreeMap<String, Deployment>>,
}

impl DeploymentStore {
    /// Opens the store, creating the directory owner-only and loading every persisted Deployment.
    /// A file that does not parse fails startup rather than being skipped: a channel that silently
    /// vanished would withdraw nothing and offer nothing, and say neither (ADR-0008's principle).
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
        }
        let mut deployments = BTreeMap::new();
        let listing =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in listing {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let meta: DeploymentMeta = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            let deployment = meta.into_deployment(&path)?;
            // The file name is derived from the name; a mismatch means a write would land
            // somewhere else, so it is refused rather than silently corrected.
            if path.file_stem().and_then(|n| n.to_str()) != Some(deployment.name.as_str()) {
                return Err(format!(
                    "{} does not match the name {:?} it states — rename the file or fix it",
                    path.display(),
                    deployment.name
                ));
            }
            deployments.insert(deployment.name.clone(), deployment);
        }
        Ok(DeploymentStore {
            dir,
            deployments: RwLock::new(deployments),
        })
    }

    /// Every Deployment, in name order.
    pub fn list(&self) -> Vec<Deployment> {
        self.deployments
            .read()
            .expect("deployments lock")
            .values()
            .cloned()
            .collect()
    }

    /// One Deployment by name.
    pub fn get(&self, name: &str) -> Option<Deployment> {
        self.deployments
            .read()
            .expect("deployments lock")
            .get(name)
            .cloned()
    }

    /// A snapshot of the whole store, for one resolution pass.
    pub fn snapshot(&self) -> BTreeMap<String, Deployment> {
        self.deployments.read().expect("deployments lock").clone()
    }

    pub fn is_empty(&self) -> bool {
        self.deployments
            .read()
            .expect("deployments lock")
            .is_empty()
    }

    /// Creates a Deployment or replaces its Selector. Distributes nothing: a Deployment reaches an
    /// Agent only through a rollout act (ADR-0061), and this is the save.
    pub fn put(
        &self,
        name: &str,
        selector: BTreeMap<String, String>,
    ) -> Result<Deployment, DeploymentError> {
        validate_name(name)
            .map_err(|e| DeploymentError::Invalid(format!("invalid name {name:?}: {e}")))?;
        check_selector(&selector).map_err(DeploymentError::Invalid)?;
        let mut deployments = self.deployments.write().expect("deployments lock");
        let deployment = match deployments.get(name) {
            Some(existing) => Deployment {
                selector,
                ..existing.clone()
            },
            None => Deployment {
                name: name.to_string(),
                selector,
                packages: BTreeMap::new(),
                signatures: BTreeMap::new(),
            },
        };
        self.write(&deployment)?;
        deployments.insert(name.to_string(), deployment.clone());
        Ok(deployment)
    }

    /// Adds a Package to a Deployment, or replaces the one held for its Agent type when `replace`
    /// is set. Without `replace`, a type already held is refused by name (ADR-0096 point 2).
    pub fn put_package(
        &self,
        name: &str,
        id: &PackageId,
        replace: bool,
    ) -> Result<Deployment, DeploymentError> {
        self.amend(name, |deployment| {
            if !replace {
                if let Some(held) = deployment.packages.get(&id.agent_type) {
                    if held != id {
                        return Err(DeploymentError::TypeTaken {
                            agent_type: id.agent_type.clone(),
                            held: held.clone(),
                        });
                    }
                }
            }
            deployment
                .packages
                .insert(id.agent_type.clone(), id.clone());
            Ok(())
        })
    }

    /// Removes a Package from a Deployment, and every signature that named it — a signature over
    /// an artifact this channel no longer offers has nothing left to say.
    pub fn remove_package(
        &self,
        name: &str,
        id: &PackageId,
    ) -> Result<Deployment, DeploymentError> {
        self.amend(name, |deployment| {
            match deployment.packages.get(&id.agent_type) {
                Some(held) if held == id => {}
                _ => return Err(DeploymentError::NotFound),
            }
            deployment.packages.remove(&id.agent_type);
            deployment.signatures.retain(|(held, _), _| held != id);
            Ok(())
        })
    }

    /// Records the Ed25519 signature of one artifact this Deployment offers.
    pub fn put_signature(
        &self,
        name: &str,
        id: &PackageId,
        platform: &Platform,
        signature: Vec<u8>,
    ) -> Result<Deployment, DeploymentError> {
        if signature.is_empty() {
            return Err(DeploymentError::Invalid(
                "an empty signature says nothing — omit it, or delete the one held".to_string(),
            ));
        }
        self.amend(name, |deployment| {
            if deployment.packages.get(&id.agent_type) != Some(id) {
                return Err(DeploymentError::NotFound);
            }
            deployment
                .signatures
                .insert((id.clone(), platform.clone()), signature.clone());
            Ok(())
        })
    }

    /// Takes one artifact's signature away. The Package stays; what it is offered with changes.
    pub fn remove_signature(
        &self,
        name: &str,
        id: &PackageId,
        platform: &Platform,
    ) -> Result<Deployment, DeploymentError> {
        self.amend(name, |deployment| {
            deployment
                .signatures
                .remove(&(id.clone(), platform.clone()))
                .map(|_| ())
                .ok_or(DeploymentError::NotFound)
        })
    }

    /// Deletes a Deployment. What it was rolled out to is the fleet's business, not the store's:
    /// an assignment that named it is withdrawn there, and nothing is uninstalled (ADR-0061).
    pub fn delete(&self, name: &str) -> Result<bool, DeploymentError> {
        let mut deployments = self.deployments.write().expect("deployments lock");
        if deployments.remove(name).is_none() {
            return Ok(false);
        }
        let path = self.dir.join(format!("{name}.json"));
        if let Err(e) = std::fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(DeploymentError::Storage(format!(
                    "cannot delete {}: {e}",
                    path.display()
                )));
            }
        }
        Ok(true)
    }

    /// Reads one Deployment, lets `change` edit a copy, persists it, and swaps it in — the single
    /// path every amendment goes through, so a refused edit never reaches the file or the map.
    fn amend(
        &self,
        name: &str,
        change: impl FnOnce(&mut Deployment) -> Result<(), DeploymentError>,
    ) -> Result<Deployment, DeploymentError> {
        let mut deployments = self.deployments.write().expect("deployments lock");
        let mut deployment = deployments
            .get(name)
            .ok_or(DeploymentError::NotFound)?
            .clone();
        change(&mut deployment)?;
        self.write(&deployment)?;
        deployments.insert(name.to_string(), deployment.clone());
        Ok(deployment)
    }

    fn write(&self, deployment: &Deployment) -> Result<(), DeploymentError> {
        let path = self.dir.join(format!("{}.json", deployment.name));
        let temp = self.dir.join(format!("{}.json.tmp", deployment.name));
        let bytes = serde_json::to_vec_pretty(&DeploymentMeta::of(deployment))
            .expect("deployment serializes");
        let failed = |e: std::io::Error, what: &std::path::Path| {
            DeploymentError::Storage(format!("cannot write {}: {e}", what.display()))
        };
        // Owner-only, and the mode is set in the open call rather than after the write: the file
        // is never briefly readable by another local user, and the rename carries the mode with it.
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
                .map_err(|e| failed(e, &temp))?;
            file.write_all(&bytes).map_err(|e| failed(e, &temp))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&temp, &bytes).map_err(|e| failed(e, &temp))?;
        }
        std::fs::rename(&temp, &path).map_err(|e| {
            DeploymentError::Storage(format!("cannot persist {}: {e}", path.display()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::proto::{any_value, AnyValue, KeyValue};

    fn agent(pairs: &[(&str, &str)]) -> AgentDescription {
        AgentDescription {
            identifying_attributes: Vec::new(),
            non_identifying_attributes: pairs
                .iter()
                .map(|(key, value)| KeyValue {
                    key: key.to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(value.to_string())),
                    }),
                })
                .collect(),
        }
    }

    fn channel(store: &DeploymentStore, name: &str, pairs: &[(&str, &str)]) -> Deployment {
        let selector = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        store.put(name, selector).expect("put")
    }

    fn id(agent_type: &str, version: &str) -> PackageId {
        PackageId::new(agent_type, version).expect("package id")
    }

    fn linux() -> Platform {
        Platform::new("linux", "amd64").expect("platform")
    }

    /// An Agent belongs to at most one Deployment. Two claiming it is the conflict, and the
    /// message names **both** — a rollout that silently never starts is worse than one that says
    /// why (ADR-0096 point 5).
    #[test]
    fn two_deployments_matching_one_agent_are_a_conflict_that_names_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DeploymentStore::open(dir.path().to_path_buf()).expect("open");
        channel(&store, "stable", &[("channel", "stable")]);
        channel(&store, "linux-hosts", &[("os.type", "linux")]);
        let all = store.snapshot();

        let both = agent(&[("channel", "stable"), ("os.type", "linux")]);
        let error = deployment_for(&all, Some(&both)).expect_err("two claim it");
        assert!(
            error.contains("stable") && error.contains("linux-hosts"),
            "the reason names every deployment in the way: {error}"
        );

        let one = agent(&[("channel", "stable"), ("os.type", "windows")]);
        assert_eq!(
            deployment_for(&all, Some(&one))
                .expect("exactly one claims it")
                .map(|d| d.name.as_str()),
            Some("stable")
        );
    }

    /// Specificity does not break the tie, and that is the decision — not an oversight. The wider
    /// Selector used to win by being narrower; now neither does.
    #[test]
    fn a_narrower_selector_does_not_win_over_a_wider_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DeploymentStore::open(dir.path().to_path_buf()).expect("open");
        channel(&store, "stable", &[("channel", "stable")]);
        channel(
            &store,
            "canary",
            &[("channel", "stable"), ("host.name", "edge-01")],
        );

        deployment_for(
            &store.snapshot(),
            Some(&agent(&[("channel", "stable"), ("host.name", "edge-01")])),
        )
        .expect_err("the narrower one does not win — both match, so neither is chosen");
    }

    /// An Agent no channel claims waits, and that is not an error: after a fresh enrolment it is the
    /// ordinary state (ADR-0096 point 4).
    #[test]
    fn an_agent_no_ring_claims_is_not_a_conflict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DeploymentStore::open(dir.path().to_path_buf()).expect("open");
        channel(&store, "stable", &[("channel", "stable")]);
        assert_eq!(
            deployment_for(&store.snapshot(), Some(&agent(&[("os.type", "linux")])))
                .expect("no claim is not a conflict"),
            None
        );
        assert_eq!(
            deployment_for(&store.snapshot(), None).expect("nor is reporting nothing"),
            None
        );
    }

    /// An empty Selector is refused, and the message says what to write instead. It is the channel
    /// that collides with every other, and it is what a forgotten field looks like.
    #[test]
    fn a_deployment_must_name_the_ring_it_aims_at() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DeploymentStore::open(dir.path().to_path_buf()).expect("open");
        let refused = store
            .put("everyone", BTreeMap::new())
            .expect_err("an empty Selector is refused");
        assert!(
            matches!(&refused, DeploymentError::Invalid(why)
                if why.contains("channel") && why.contains("prescribes none")),
            "the refusal tells the operator what to write: {refused}"
        );
        assert!(store.is_empty(), "and nothing was stored");

        assert!(matches!(
            store.put(
                "blank",
                BTreeMap::from([("channel".to_string(), "  ".to_string())])
            ),
            Err(DeploymentError::Invalid(_))
        ));
    }

    /// One Package per Agent type, refused at the write rather than puzzled over at resolution —
    /// and the refusal names what is already held.
    #[test]
    fn a_deployment_holds_one_package_per_agent_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DeploymentStore::open(dir.path().to_path_buf()).expect("open");
        channel(&store, "stable", &[("channel", "stable")]);
        store
            .put_package("stable", &id("telegraf", "1.30.0"), false)
            .expect("the first of its type");
        store
            .put_package("stable", &id("supervisor", "0.4.5"), false)
            .expect("another type is another package");

        let refused = store
            .put_package("stable", &id("telegraf", "1.31.0"), false)
            .expect_err("a second telegraf is refused");
        assert!(
            matches!(&refused, DeploymentError::TypeTaken { held, .. } if held.version == "1.30.0"),
            "the refusal names what is in the way: {refused}"
        );

        // Writing the same one again is not a collision — it is the request arriving twice.
        store
            .put_package("stable", &id("telegraf", "1.30.0"), false)
            .expect("idempotent");
        // And replacing is what the operator asks for explicitly.
        let replaced = store
            .put_package("stable", &id("telegraf", "1.31.0"), true)
            .expect("replace");
        assert_eq!(
            replaced.package_for("telegraf"),
            Some(&id("telegraf", "1.31.0"))
        );
    }

    /// A signature belongs to an artifact this Deployment actually offers, and it goes when the
    /// Package does — a signature over something no longer offered has nothing left to say.
    #[test]
    fn a_signature_needs_its_package_and_leaves_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DeploymentStore::open(dir.path().to_path_buf()).expect("open");
        channel(&store, "stable", &[("channel", "stable")]);
        let telegraf = id("telegraf", "1.30.0");

        assert_eq!(
            store.put_signature("stable", &telegraf, &linux(), vec![7; 64]),
            Err(DeploymentError::NotFound),
            "a signature for a package this channel does not hold is refused"
        );

        store
            .put_package("stable", &telegraf, false)
            .expect("package");
        let signed = store
            .put_signature("stable", &telegraf, &linux(), vec![7; 64])
            .expect("signature");
        assert_eq!(signed.signature(&telegraf, &linux()), Some(&[7u8; 64][..]));

        let stripped = store
            .remove_package("stable", &telegraf)
            .expect("remove the package");
        assert!(
            stripped.signatures.is_empty(),
            "the signature left with the package it was about"
        );
    }

    /// The whole store survives a reopen — the signatures included, which is the part that had to
    /// be flattened to be persisted at all.
    #[test]
    fn a_deployment_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let telegraf = id("telegraf", "1.30.0");
        {
            let store = DeploymentStore::open(dir.path().to_path_buf()).expect("open");
            channel(&store, "canary", &[("channel", "canary"), ("env", "prod")]);
            store
                .put_package("canary", &telegraf, false)
                .expect("package");
            store
                .put_signature("canary", &telegraf, &linux(), vec![9; 64])
                .expect("signature");
        }

        let reopened = DeploymentStore::open(dir.path().to_path_buf()).expect("reopen");
        let canary = reopened.get("canary").expect("canary");
        assert_eq!(canary.selector["channel"], "canary");
        assert_eq!(canary.selector["env"], "prod");
        assert_eq!(canary.package_for("telegraf"), Some(&telegraf));
        assert_eq!(canary.signature(&telegraf, &linux()), Some(&[9u8; 64][..]));

        assert!(reopened.delete("canary").expect("delete"));
        assert!(!reopened
            .delete("canary")
            .expect("deleting twice is not an error"));
        assert!(DeploymentStore::open(dir.path().to_path_buf())
            .expect("reopen")
            .is_empty());
    }

    /// Editing the Selector is not editing the bytes: a Deployment's aim stays writable, and
    /// changing it keeps everything the channel holds.
    #[test]
    fn the_selector_stays_editable_and_keeps_what_the_ring_holds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DeploymentStore::open(dir.path().to_path_buf()).expect("open");
        let telegraf = id("telegraf", "1.30.0");
        channel(&store, "canary", &[("channel", "canary")]);
        store
            .put_package("canary", &telegraf, false)
            .expect("package");
        store
            .put_signature("canary", &telegraf, &linux(), vec![3; 64])
            .expect("signature");

        let widened = channel(&store, "canary", &[("channel", "stable")]);
        assert_eq!(widened.selector["channel"], "stable");
        assert_eq!(widened.package_for("telegraf"), Some(&telegraf));
        assert_eq!(widened.signature(&telegraf, &linux()), Some(&[3u8; 64][..]));
    }

    /// A file this Server did not write fails the open, naming it. A channel that silently vanished
    /// would withdraw nothing and offer nothing, and say neither.
    #[test]
    fn an_unreadable_file_fails_the_open_and_names_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("broken.json"), "{").expect("write");
        let error = DeploymentStore::open(dir.path().to_path_buf())
            .map(|_| ())
            .expect_err("refused");
        assert!(error.contains("broken.json"), "{error}");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("stable.json"),
            r#"{"name":"canary","selector":{"channel":"canary"}}"#,
        )
        .expect("write");
        let error = DeploymentStore::open(dir.path().to_path_buf())
            .map(|_| ())
            .expect_err("a file whose name disagrees with its content is refused");
        assert!(error.contains("stable.json"), "{error}");
    }

    /// The store is owner-only, and so is every file in it: a Selector says which hosts a fleet
    /// operator considers a channel, which is not another local user's business.
    #[cfg(unix)]
    #[test]
    fn the_store_and_its_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DeploymentStore::open(dir.path().to_path_buf()).expect("open");
        channel(&store, "stable", &[("channel", "stable")]);
        assert_eq!(
            std::fs::metadata(dir.path())
                .expect("dir")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("stable.json"))
                .expect("file")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
