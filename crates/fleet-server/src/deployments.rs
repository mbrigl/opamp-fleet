//! Deployments (ADR-0028): what reaches a channel of hosts, and the only thing rolled out.
//!
//! A Package is what an Agent type runs at a version and nothing else (ADR-0028) — no aim, no
//! signature, no act of its own. All three live here. A Deployment carries a **name**, a
//! **Selector** over the channel it addresses, **one Package per Agent type**, and the **signature**
//! of each artifact it offers.
//!
//! Two rules give the object its shape, and both are refusals:
//!
//! **An Agent belongs to at most one Deployment.** Where two match, that is a conflict and the
//! Agent is offered nothing new — not the most specific, not the newest, none. ADR-0028's
//! specificity ranking is withdrawn with no successor: it decided "which artifact does this host
//! get" by a computation across every stored object, which is an answer no operator could read off
//! anything. A refusal that names both Deployments is worse for nobody and legible to everyone.
//!
//! **A Selector is never empty.** An empty one is the channel that collides with every other, and a
//! forgotten field would quietly become the base for the whole fleet — the class of accident
//! ADR-0027 exists to prevent. Channels are therefore a *partition*: a Selector cannot express
//! "not", so disjoint channels come from membership, which is what ADR-0026's labels already are.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::RwLock;

use opamp::proto::AgentDescription;

use crate::configs::{matches, validate_name};
use crate::packages::{PackageId, Platform};

/// A named set of Packages, aimed at a channel and carrying each artifact's signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    /// The operator's name for this channel. The one human-chosen label in the model, which is why
    /// it keeps the ADR-0021 grammar a Package gave up (ADR-0028).
    pub name: String,
    /// Equality pairs that must all match an attribute the Agent reported, labels included
    /// (ADR-0025 semantics). **Never empty** — see the module note.
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
            DeploymentError::Invalid(why)
            | DeploymentError::Conflict(why)
            | DeploymentError::Storage(why) => write!(f, "{why}"),
            DeploymentError::NotFound => write!(f, "no such deployment"),
            DeploymentError::TypeTaken { agent_type, held } => write!(
                f,
                "this deployment already holds {held} for Agent type {agent_type:?} — an Agent has \
                 one binary to replace, so remove that one first or use another deployment"
            ),
        }
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
    let claiming: Vec<&Deployment> = deployments
        .values()
        .filter(|deployment| matches(&deployment.selector, description))
        .collect();
    match claiming.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(*only)),
        all => Err(format!(
            "deployments {} all match this Agent — an Agent belongs to at most one, so narrow \
             their Selectors until exactly one claims it",
            all.iter()
                .map(|d| format!("{:?}", d.name))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The Deployment store's port (ADR-0006): where Deployments persist. The filesystem adapter
/// ([`FsDeploymentBackend`](crate::fs::FsDeploymentBackend)) is what the composition root wires.
pub trait DeploymentBackend: Send + Sync {
    /// Every persisted Deployment, once, when the store opens. One that cannot be read fails the
    /// open and names where it is: a channel that silently vanished would withdraw nothing and
    /// offer nothing, and say neither (ADR-0009's principle).
    fn load(&self) -> Result<Vec<Deployment>, String>;

    /// Creates or replaces one Deployment, in one step.
    fn put(&self, deployment: &Deployment) -> Result<(), String>;

    /// Deletes one Deployment; removing what is already absent is not an error.
    fn remove(&self, name: &str) -> Result<(), String>;
}

/// The Deployments (ADR-0028), held in memory and persisted through a [`DeploymentBackend`].
pub struct DeploymentStore {
    backend: Box<dyn DeploymentBackend>,
    deployments: RwLock<BTreeMap<String, Deployment>>,
}

impl DeploymentStore {
    /// Opens the store on `backend`, loading every persisted Deployment.
    pub fn open(backend: Box<dyn DeploymentBackend>) -> Result<Self, String> {
        let deployments = backend
            .load()?
            .into_iter()
            .map(|deployment| (deployment.name.clone(), deployment))
            .collect();
        Ok(DeploymentStore {
            backend,
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

    /// The names of the Deployments that sign the artifact `(id, platform)` — what the download
    /// route asks once per request rather than once per Agent (ADR-0028 clause 35).
    pub fn signing(&self, id: &PackageId, platform: &Platform) -> BTreeSet<String> {
        self.deployments
            .read()
            .expect("deployments lock")
            .values()
            .filter(|deployment| deployment.signature(id, platform).is_some())
            .map(|deployment| deployment.name.clone())
            .collect()
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
    /// Agent only through a rollout act (ADR-0027), and this is the save.
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
    /// is set. Without `replace`, a type already held is refused by name (ADR-0028 point 23).
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
            if deployment.packages.get(&id.agent_type) != Some(id) {
                return Err(DeploymentError::NotFound);
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

    /// Deletes a Deployment. Whether anything still names it is the fleet's business, not the
    /// store's — the fleet refuses the delete while an assignment does.
    pub fn delete(&self, name: &str) -> Result<bool, DeploymentError> {
        let mut deployments = self.deployments.write().expect("deployments lock");
        if deployments.remove(name).is_none() {
            return Ok(false);
        }
        self.backend
            .remove(name)
            .map_err(DeploymentError::Storage)?;
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
        self.backend
            .put(deployment)
            .map_err(DeploymentError::Storage)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// The backend as memory: what a second store opened on a clone of it reads is what the
    /// first one wrote.
    #[derive(Clone, Default)]
    struct Memory(Arc<Mutex<BTreeMap<String, Deployment>>>);

    impl DeploymentBackend for Memory {
        fn load(&self) -> Result<Vec<Deployment>, String> {
            Ok(self.0.lock().expect("memory").values().cloned().collect())
        }

        fn put(&self, deployment: &Deployment) -> Result<(), String> {
            self.0
                .lock()
                .expect("memory")
                .insert(deployment.name.clone(), deployment.clone());
            Ok(())
        }

        fn remove(&self, name: &str) -> Result<(), String> {
            self.0.lock().expect("memory").remove(name);
            Ok(())
        }
    }

    fn agent(pairs: &[(&str, &str)]) -> AgentDescription {
        AgentDescription {
            identifying_attributes: Vec::new(),
            non_identifying_attributes: pairs
                .iter()
                .map(|(key, value)| opamp::attributes::string_attr(key, value))
                .collect(),
        }
    }

    pub(crate) fn channel(
        store: &DeploymentStore,
        name: &str,
        pairs: &[(&str, &str)],
    ) -> Deployment {
        let selector = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        store.put(name, selector).expect("put")
    }

    pub(crate) fn id(agent_type: &str, version: &str) -> PackageId {
        PackageId::new(agent_type, version).expect("package id")
    }

    pub(crate) fn linux() -> Platform {
        Platform::new("linux", "amd64").expect("platform")
    }

    /// An Agent belongs to at most one Deployment. Two claiming it is the conflict, and the
    /// message names **both** — a rollout that silently never starts is worse than one that says
    /// why (ADR-0028 point 26).
    /// Verifies: ADR-0037
    #[test]
    fn two_deployments_matching_one_agent_are_a_conflict_that_names_them() {
        let memory = Memory::default();
        let store = DeploymentStore::open(Box::new(memory.clone())).expect("open");
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

    /// Specificity does not break the tie, and that is the decision — not an oversight: neither
    /// Selector wins by being narrower (ADR-0028 clause 26).
    /// Verifies: ADR-0037
    #[test]
    fn a_narrower_selector_does_not_win_over_a_wider_one() {
        let memory = Memory::default();
        let store = DeploymentStore::open(Box::new(memory.clone())).expect("open");
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
    /// ordinary state (ADR-0028 point 25).
    /// Verifies: ADR-0037
    #[test]
    fn an_agent_no_ring_claims_is_not_a_conflict() {
        let memory = Memory::default();
        let store = DeploymentStore::open(Box::new(memory.clone())).expect("open");
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
    /// Verifies: ADR-0037
    #[test]
    fn a_deployment_must_name_the_ring_it_aims_at() {
        let memory = Memory::default();
        let store = DeploymentStore::open(Box::new(memory.clone())).expect("open");
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
    /// Verifies: ADR-0037
    #[test]
    fn a_deployment_holds_one_package_per_agent_type() {
        let memory = Memory::default();
        let store = DeploymentStore::open(Box::new(memory.clone())).expect("open");
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
    /// Verifies: ADR-0037
    #[test]
    fn a_signature_needs_its_package_and_leaves_with_it() {
        let memory = Memory::default();
        let store = DeploymentStore::open(Box::new(memory.clone())).expect("open");
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

    /// Editing the Selector is not editing the bytes: a Deployment's aim stays writable, and
    /// changing it keeps everything the channel holds.
    /// Verifies: ADR-0037
    #[test]
    fn the_selector_stays_editable_and_keeps_what_the_ring_holds() {
        let memory = Memory::default();
        let store = DeploymentStore::open(Box::new(memory.clone())).expect("open");
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
}
