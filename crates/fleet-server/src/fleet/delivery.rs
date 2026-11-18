//! Packages in the fleet (ADR-0033, ADR-0034): the Package Sets, their entries and signatures the
//! Operator plane writes, the upload limit of the store, the offer of a Package to an Agent and of
//! an artifact to a Host, and the Packages a Deployment holds. Its entry points are methods of
//! `AppState`.

use std::sync::atomic::Ordering;

use opamp::proto::PackagesAvailable;
use opamp::uid::InstanceUid;
use tracing::{info, warn};

use crate::agent_store::Pin;
use crate::deployments::{Deployment, DeploymentError, DeploymentStore};
use crate::packages::{PackageError, PackageId, PackageStore, Platform, Refusal, SignatureState};
use crate::revocation::SpeaksFor;

use super::{parse_hex_uid, AgentRecord, AppState};

impl AppState {
    /// The total bytes of stored package artifacts right now, or `0` when package delivery is not
    /// configured. What the upload route checks against [`max_total_package_bytes`](Self::max_total_package_bytes).
    pub fn stored_package_bytes(&self) -> u64 {
        self.packages
            .as_ref()
            .map_or(0, |p| p.store().total_bytes())
    }

    /// Whether the package store takes another upload at all (ADR-0033): one already at its
    /// ceiling, counting the uploads still arriving, takes nothing more. Asked before an upload is
    /// streamed, so a gibibyte that would only be rejected is never read. The upload's bytes are
    /// counted through the answer as they arrive, and no longer once it is dropped.
    ///
    /// # Errors
    /// [`StoreFull::AtLimit`] when the store is at or past [`max_total_package_bytes`](Self::max_total_package_bytes).
    pub fn admit_upload(&self) -> Result<UploadReservation<'_>, StoreFull> {
        let limit = self.max_total_package_bytes;
        let stored = self.stored_package_bytes();
        if stored.saturating_add(self.uploading_bytes.load(Ordering::SeqCst)) >= limit {
            return Err(StoreFull::AtLimit { limit });
        }
        Ok(UploadReservation {
            state: self,
            stored,
            bytes: 0,
        })
    }

    /// The package offer for one Agent, composed from its **assignments** (ADR-0032), or `None`
    /// when it cannot accept packages, is assigned nothing it fits, or the aggregate hash it last
    /// reported already matches what it is assigned.
    ///
    /// Both the offer and the aggregate are computed over *this Agent's* assignments: comparing
    /// against a fleet-wide aggregate would re-offer, on every exchange, packages this Agent is
    /// never given.
    pub(super) fn packages_offer(&self, record: &AgentRecord) -> Option<PackagesAvailable> {
        let offering = self.packages.as_ref()?;
        if !accepts_packages(record) {
            return None;
        }
        let effective = record.effective_description();
        let description = effective.as_deref();
        let assigned = record.assigned_package();
        let reported = record
            .package_statuses
            .as_ref()
            .map(|s| s.server_provided_all_packages_hash.as_slice())
            .unwrap_or_default();
        if reported
            == offering
                .store
                .assigned_hash_for(assigned, description)
                .as_slice()
        {
            return None;
        }
        offering.store.offer_for_assigned(assigned, description)
    }

    /// Whether a certificate naming `host` may fetch the artifact `(id, platform)` from the download
    /// route (ADR-0033): it is offered to an Agent the host speaks for. The offer's own test
    /// decides (ADR-0033 clause 35), for each such Agent — the `instance_uid`s bound to the host, or
    /// every Agent for a host marked as a Gateway. Nothing is offered without package delivery or a
    /// host register.
    ///
    /// The package store and the host register are read first and released before the fleet is
    /// locked: no two of those locks are held together here.
    ///
    /// On a shared store the Agents are read from it, whichever instance carries them: the
    /// Agents bound to the host in one read, placed among themselves, which are the group their
    /// Client Agent is found in, or for a Gateway every Agent as the Operator plane reads it. One
    /// that cannot be read is logged, and nothing is offered.
    pub fn offers_artifact(&self, host: &str, id: &PackageId, platform: &Platform) -> bool {
        let (Some(offering), Some(revocations)) = (&self.packages, &self.revocations) else {
            return false;
        };
        // What does not depend on the Agent is resolved once, before the fleet is locked.
        let Some(artifact) = offering.store.offerable(id, platform) else {
            return false;
        };
        let speaks_for = revocations.speaks_for(host);
        let stored = match (self.agent_store.is_shared(), &speaks_for) {
            (false, _) => None,
            (true, SpeaksFor::Any) => Some(self.arrived_over(host).map(|found| self.placed(found))),
            (true, SpeaksFor::Agents(uids)) => {
                let uids: Vec<InstanceUid> =
                    uids.iter().filter_map(|uid| parse_hex_uid(uid)).collect();
                Some(self.agent_store.get_many(&uids).map(|found| {
                    self.placed(
                        found
                            .into_iter()
                            .map(|(uid, record)| (uid, AgentRecord::from_persisted(record)))
                            .collect(),
                    )
                }))
            }
        };
        let stored = match stored.transpose() {
            Ok(stored) => stored,
            Err(e) => {
                warn!(host = %host, error = %e, "cannot read the agents the host speaks for");
                return false;
            }
        };
        let held;
        let fleet = match &stored {
            Some(stored) => stored,
            None => {
                held = self.fleet.lock().expect("fleet lock");
                &*held
            }
        };
        let offered = |record: &AgentRecord| {
            // The cheap half of the test first: most Agents are assigned something else.
            accepts_packages(record)
                && record.assigned_package() == Some(id)
                && artifact.offered_to(
                    record.assigned_package(),
                    record.effective_description().as_deref(),
                )
        };
        match speaks_for {
            SpeaksFor::Any => fleet.values().any(offered),
            SpeaksFor::Agents(uids) => uids
                .iter()
                .filter_map(|uid| parse_hex_uid(uid))
                .filter_map(|uid| fleet.get(&uid))
                .any(offered),
        }
    }

    /// Why this Agent is **proposed** nothing: more than one Deployment claims it, and an Agent
    /// belongs to at most one (ADR-0033 clause 28). `None` when nothing is wrong. Every Agent can
    /// be in conflict, since a Deployment carries Configurations as well as Packages (ADR-0032).
    ///
    /// A conflict takes the *candidate* away and never a standing assignment: an Agent already
    /// rolled out to keeps its offer, because nothing distributes or un-distributes by itself
    /// (ADR-0032). Creating an overlapping channel must not withdraw anything from a running host.
    ///
    /// `claim` is what [`deployment_for`](crate::deployments::deployment_for) answered for this
    /// Agent — `Ok(None)` when package delivery, which arms the Deployments, is not configured.
    pub(super) fn package_conflict(claim: &Result<Option<&Deployment>, String>) -> Option<String> {
        claim.as_ref().err().cloned()
    }

    /// Package delivery, or the error every package route and rollout act answers when it is not
    /// configured.
    pub(super) fn offering(&self) -> Result<&PackageOffering, String> {
        self.packages
            .as_ref()
            .ok_or_else(|| "package delivery is not configured on this Server".to_string())
    }

    /// The package store, or the same refusal as [`offering`](Self::offering).
    fn package_store(&self) -> Result<&PackageStore, String> {
        self.offering().map(PackageOffering::store)
    }

    /// Whether any Agent's assignment references this Package — the gate that makes an assigned
    /// Package's bytes immutable (ADR-0032 clause 13). The Agent store answers it (ADR-0023
    /// clause 2); the fleet lock keeps a rollout of this Server from pinning it meanwhile. A
    /// store that other Servers write too refuses the write again from the stored assignments, in
    /// the act that writes.
    fn package_set_assigned(&self, id: &PackageId) -> bool {
        let _fleet = self.fleet.lock().expect("fleet lock");
        self.agent_store.pinning(Pin::Package(id)) > 0
    }

    /// How many stored assignments pin `pin`, as the refusal before a deletion counts them: this
    /// Server's count where it is the store's one writer. On a store other Servers write too, its
    /// count of an Agent it does not hold may be stale after another instance re-released or
    /// forgot it, so it refuses nothing from it, and leaves the refusal to the act, which counts the
    /// stored assignments again in its own transaction.
    pub(super) fn pinning(&self, pin: Pin<'_>) -> usize {
        if self.agent_store.is_shared() {
            return 0;
        }
        self.agent_store.pinning(pin)
    }

    /// Puts a Package into a channel (ADR-0033 clause 25). Not gated: adding one for an Agent type
    /// the channel does not hold surfaces as waiting on the Agents of that type, and replacing the
    /// one it holds is how a rollout proceeds — an Agent already rolled out to keeps the Package
    /// its assignment pins until the next act. A store that other Servers write too reads the
    /// Package in the act that adds it, so one deleted through another instance is `Missing`.
    pub fn put_deployment_package(
        &self,
        name: &str,
        id: &PackageId,
        replace: bool,
    ) -> Result<Deployment, DeploymentError> {
        self.deployment_store()
            .ok_or(DeploymentError::NotFound)?
            .put_package(name, id, replace)
    }

    /// Takes a Package out of a channel. Not gated: nothing a Deployment holds is frozen
    /// (ADR-0032 clause 11) — an assignment pins the Package itself, so an Agent already released
    /// keeps its offer until the next press.
    pub fn remove_deployment_package(
        &self,
        name: &str,
        id: &PackageId,
    ) -> Result<Deployment, DeploymentError> {
        self.deployment_store()
            .ok_or(DeploymentError::NotFound)?
            .remove_package(name, id)
    }

    /// The package store for a route of the Package REST API, or a `404` where package delivery is
    /// not configured.
    fn packages_for_a_route(&self) -> Result<&PackageStore, PackageError> {
        self.package_store()
            .map_err(|e| PackageError::refused(Refusal::NotFound, e))
    }

    /// The refusal every write to an assigned Package answers with (ADR-0032 clause 13).
    fn refuse_if_assigned(&self, id: &PackageId) -> Result<(), PackageError> {
        if self.package_set_assigned(id) {
            return Err(crate::packages::assigned(id));
        }
        Ok(())
    }

    /// Creates a Package (ADR-0033). **Nothing is distributed** (ADR-0032), and there is nothing
    /// to update: a Package is its identity and its entries, so creating one that exists is the
    /// same request arriving twice.
    pub fn create_package_set(&self, id: &PackageId) -> Result<(), PackageError> {
        self.packages_for_a_route()?.create(id)?;
        info!(package = %id, "package stored — nothing distributed");
        Ok(())
    }

    /// Stores one streamed upload as an entry of a Package (ADR-0033, ADR-0023). Refused while the
    /// Package is assigned to an Agent (ADR-0032 clause 13); no push, because saving never
    /// distributes.
    pub fn put_package_entry(
        &self,
        id: &PackageId,
        platform: &Platform,
        bytes: &mut dyn std::io::Read,
    ) -> Result<(), PackageError> {
        self.refuse_if_assigned(id)?;
        self.packages_for_a_route()?
            .put_entry_from(id, platform, bytes)?;
        info!(set = %id, platform = %format!("{}-{}", platform.os, platform.arch), "package entry stored");
        Ok(())
    }

    /// Whether an upload for one entry of `id` may begin: the Package exists and is not assigned
    /// to an Agent (ADR-0032 clause 13), so a refused upload is refused before its bytes are
    /// streamed.
    pub fn check_package_upload(&self, id: &PackageId) -> Result<(), PackageError> {
        self.refuse_if_assigned(id)?;
        self.packages_for_a_route()?.writable(id)
    }

    /// Writes the operator's signature onto one entry of a Package (ADR-0033), and answers what the
    /// Server found when it checked it. Refused while the Package is assigned to an Agent, like
    /// any other write to its entries (ADR-0032 clause 13): the re-offer gate does not cover the
    /// signature, so one changed under a standing offer would never reach the Agent.
    pub fn put_package_entry_signature(
        &self,
        id: &PackageId,
        platform: &Platform,
        signature: Vec<u8>,
    ) -> Result<SignatureState, PackageError> {
        self.refuse_if_assigned(id)?;
        let state = self
            .packages_for_a_route()?
            .put_signature(id, platform, signature)?;
        info!(set = %id, platform = %platform.tag(), state = state.as_str(), "package entry signed");
        Ok(state)
    }

    /// Takes one entry's signature away (ADR-0033); `Ok(false)` when it carries none. Refused
    /// while the Package is assigned to an Agent (ADR-0032 clause 13).
    pub fn remove_package_entry_signature(
        &self,
        id: &PackageId,
        platform: &Platform,
    ) -> Result<bool, PackageError> {
        self.refuse_if_assigned(id)?;
        let removed = self
            .packages_for_a_route()?
            .remove_signature(id, platform)?;
        if removed {
            info!(set = %id, platform = %platform.tag(), "package entry signature removed");
        }
        Ok(removed)
    }

    /// Deletes one entry of a Package; `Ok(false)` when the Package or entry does not exist.
    /// Refused while the Package is assigned to an Agent (ADR-0032 clause 13).
    pub fn delete_package_entry(
        &self,
        id: &PackageId,
        platform: &Platform,
    ) -> Result<bool, PackageError> {
        self.refuse_if_assigned(id)?;
        let deleted = self.packages_for_a_route()?.delete_entry(id, platform)?;
        if deleted {
            info!(set = %id, "package entry deleted");
        }
        Ok(deleted)
    }

    /// Deletes a Package (ADR-0032 clause 12); `Ok(false)` when none of that identity exists.
    /// **Refused while any Agent's assignment pins it** — the message says how many — because a
    /// deletion must not change what a running Agent receives. A deleted Package leaves every
    /// Deployment that held it. Nothing is uninstalled either way (ADR-0033).
    ///
    /// The fleet lock is held across the check and the delete, so a rollout of this Server cannot
    /// pin it in between; a store that other Servers write too counts the stored assignments again
    /// in the act that deletes, and takes it out of every Deployment in that act too.
    pub fn delete_package_set(&self, id: &PackageId) -> Result<bool, PackageError> {
        let fleet = self.fleet.lock().expect("fleet lock");
        let pinning = self.pinning(Pin::Package(id));
        if pinning > 0 {
            return Err(crate::packages::pinned(id, pinning));
        }
        let packages = self.packages_for_a_route()?;
        let sweep = packages.remove_set(id)?;
        let deleted = sweep.is_some();
        let dropped = if deleted {
            self.deployment_store()
                .map(|store| {
                    store
                        .drop_package(id)
                        .map_err(|e| PackageError::from(e.to_string()))
                })
                .transpose()
        } else {
            Ok(None)
        };
        drop(fleet);
        // The sweep may wait for the blob store's lock, which another instance holds as long as it
        // sweeps: not under the fleet lock, which every act of this Server waits for.
        if let Some(sweep) = sweep {
            packages.sweep_after(sweep);
        }
        if let Some(left) = dropped? {
            if !left.is_empty() {
                info!(set = %id, deployments = ?left, "package left the deployments that held it");
            }
        }
        if deleted {
            info!(set = %id, "package set deleted");
        }
        Ok(deleted)
    }
}

/// Why the package store refuses an upload: its whole-store ceiling (ADR-0033).
#[derive(Debug, PartialEq, Eq)]
pub enum StoreFull {
    /// The store is already at its ceiling.
    AtLimit { limit: u64 },
    /// Committing an artifact of `size` bytes would take it past the ceiling.
    WouldExceed { size: u64, limit: u64 },
}

impl std::fmt::Display for StoreFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreFull::AtLimit { limit } => write!(
                f,
                "the package store is at its {limit}-byte limit (max_total_package_bytes)"
            ),
            StoreFull::WouldExceed { size, limit } => write!(
                f,
                "storing this {size}-byte artifact would take the package store past its \
                 {limit}-byte limit (max_total_package_bytes)"
            ),
        }
    }
}

/// The bytes of one upload still arriving, counted against the package store's ceiling until it is
/// dropped (ADR-0033). Taken from [`AppState::admit_upload`].
pub struct UploadReservation<'a> {
    state: &'a AppState,
    /// The stored artifacts' bytes when the upload began; walking the store per chunk would cost
    /// more than the chunk.
    stored: u64,
    bytes: u64,
}

impl UploadReservation<'_> {
    /// Counts `more` bytes of this upload as they arrive.
    ///
    /// # Errors
    /// [`StoreFull::WouldExceed`] once the store and every upload in progress pass the ceiling.
    pub fn grow(&mut self, more: u64) -> Result<(), StoreFull> {
        self.bytes += more;
        let uploading = self.state.uploading_bytes.fetch_add(more, Ordering::SeqCst) + more;
        self.within(self.stored, uploading)
    }

    /// Whether the upload, whole now, may be committed: the store as it stands and every upload in
    /// progress, this one included, within the ceiling. With
    /// [`admit_upload`](AppState::admit_upload) before the stream, this is what stops a caller
    /// filling the disk under distinct names.
    ///
    /// # Errors
    /// [`StoreFull::WouldExceed`] when committing it would pass the ceiling.
    pub fn admit_artifact(&self) -> Result<(), StoreFull> {
        self.within(
            self.state.stored_package_bytes(),
            self.state.uploading_bytes.load(Ordering::SeqCst),
        )
    }

    fn within(&self, stored: u64, uploading: u64) -> Result<(), StoreFull> {
        let limit = self.state.max_total_package_bytes;
        if stored.saturating_add(uploading) > limit {
            return Err(StoreFull::WouldExceed {
                size: self.bytes,
                limit,
            });
        }
        Ok(())
    }
}

impl Drop for UploadReservation<'_> {
    fn drop(&mut self) {
        self.state
            .uploading_bytes
            .fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

/// The package store and the Deployments that release from it (ADR-0033). Every `download_url`
/// is a path the Client resolves against its own endpoint — the Agent plane, where the download
/// is served behind the same handshake as `/v1/opamp` (ADR-0033).
pub struct PackageOffering {
    pub(super) store: PackageStore,
    deployments: DeploymentStore,
}

impl PackageOffering {
    pub fn with_deployments(store: PackageStore, deployments: DeploymentStore) -> Self {
        PackageOffering { store, deployments }
    }

    pub fn store(&self) -> &PackageStore {
        &self.store
    }

    pub fn deployments(&self) -> &DeploymentStore {
        &self.deployments
    }
}

/// Whether this Agent declared that it accepts packages — the first condition of every offer
/// (ADR-0033 clause 35).
fn accepts_packages(record: &AgentRecord) -> bool {
    record.capabilities & opamp::proto::AgentCapabilities::AcceptsPackages as u64 != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::fixtures::*;
    use crate::fleet::session::Processed;
    use crate::fleet::Transport;
    use crate::revocation::{CertId, Presented};
    use opamp::proto::{AgentDescription, AgentToServer, PackageStatuses};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    /// Without package delivery every write of the Package routes is a `404`, the signature's
    /// included: what each route's contract names, whatever the text.
    /// Verifies: ADR-0033
    #[test]
    fn without_package_delivery_every_package_write_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = AppState::new(dir.path().join("configs")).expect("state");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        let linux = Platform::new("linux", "amd64").expect("platform");
        let kinds = [
            state.create_package_set(&id).map(|_| ()),
            state.check_package_upload(&id),
            state.put_package_entry(&id, &linux, &mut &b"bytes"[..]),
            state
                .put_package_entry_signature(&id, &linux, vec![1; 64])
                .map(|_| ()),
            state
                .remove_package_entry_signature(&id, &linux)
                .map(|_| ()),
            state.delete_package_entry(&id, &linux).map(|_| ()),
            state.delete_package_set(&id).map(|_| ()),
        ]
        .map(|answer| answer.expect_err("not configured").refusal);
        assert_eq!(kinds, [Some(Refusal::NotFound); 7]);
    }

    /// The whole-store ceiling (ADR-0033), decided without a request: a store at its limit takes
    /// no upload, an artifact is refused when it would take the store past it, and an upload still
    /// arriving counts until it is dropped.
    #[test]
    fn the_package_store_ceiling_admits_up_to_its_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = AppState::new(dir.path().join("configs"))
            .expect("state")
            .with_max_total_package_bytes(100);
        {
            let mut upload = state
                .admit_upload()
                .expect("an empty store takes an upload");
            assert_eq!(upload.grow(100), Ok(()));
            assert_eq!(upload.admit_artifact(), Ok(()));
            assert!(
                matches!(state.admit_upload(), Err(StoreFull::AtLimit { limit: 100 })),
                "an upload in progress fills the store as much as a stored one"
            );
            assert_eq!(
                upload.grow(1),
                Err(StoreFull::WouldExceed {
                    size: 101,
                    limit: 100
                })
            );
            assert_eq!(
                upload.admit_artifact(),
                Err(StoreFull::WouldExceed {
                    size: 101,
                    limit: 100
                })
            );
        }
        assert!(
            state.admit_upload().is_ok(),
            "a dropped upload counts no more"
        );
        let full = state.with_max_total_package_bytes(0);
        assert!(matches!(
            full.admit_upload(),
            Err(StoreFull::AtLimit { limit: 0 })
        ));
    }

    // ---- Who may fetch an uploaded artifact (ADR-0033) ----

    /// One report of an `otelcol` Agent on linux/amd64 that accepts packages, over a certificate
    /// naming `host`, echoing `echoed` as the aggregate hash it holds.
    fn report_from(state: &AppState, host: &str, uid: InstanceUid, echoed: &[u8]) -> Processed {
        let attr = opamp::attributes::string_attr;
        state.process_presented(
            AgentToServer {
                instance_uid: uid.as_bytes().to_vec(),
                capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64
                    | opamp::proto::AgentCapabilities::AcceptsPackages as u64
                    | opamp::proto::AgentCapabilities::ReportsPackageStatuses as u64,
                agent_description: Some(AgentDescription {
                    identifying_attributes: vec![attr("service.name", "otelcol")],
                    non_identifying_attributes: vec![
                        attr("os.type", "linux"),
                        attr("host.arch", "amd64"),
                    ],
                }),
                package_statuses: (!echoed.is_empty()).then(|| PackageStatuses {
                    server_provided_all_packages_hash: echoed.to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&Presented {
                id: CertId::new(b"CA", host),
                host: Some(host.to_string()),
                subject_values: crate::revocation::SubjectValues::default(),
                not_after_ms: u64::MAX,
            }),
        )
    }

    /// An artifact is fetched by a host only through an Agent it speaks for: the host whose Agent
    /// it was released to, not another host, not a host the register does not know, and not for
    /// another Platform or version — until that other host is marked as a Gateway.
    /// Verifies: ADR-0033, Q-4
    #[test]
    fn an_artifact_is_offered_to_a_host_only_through_an_agent_it_speaks_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, revocations, id) = delivering_fleet(dir.path());
        report_from(&state, "h1", InstanceUid([1; 16]), &[]);
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);
        // h2 reports an Agent of its own, released nothing.
        state.process_presented(
            AgentToServer {
                instance_uid: vec![2; 16],
                capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&Presented {
                id: CertId::new(b"CA", "h2"),
                host: Some("h2".to_string()),
                subject_values: crate::revocation::SubjectValues::default(),
                not_after_ms: u64::MAX,
            }),
        );

        assert!(state.offers_artifact("h1", &id, &linux()));
        assert!(!state.offers_artifact("h2", &id, &linux()));
        assert!(!state.offers_artifact("unknown", &id, &linux()));
        let windows = Platform::new("windows", "amd64").expect("platform");
        assert!(!state.offers_artifact("h1", &id, &windows));
        let v2 = PackageId::new("otelcol", "2.0.0").expect("id");
        assert!(!state.offers_artifact("h1", &v2, &linux()));

        assert!(revocations.set_gateway("h2", true).expect("mark"));
        assert!(
            state.offers_artifact("h2", &id, &linux()),
            "a Gateway speaks for any Agent"
        );
    }

    /// The offer and the download agree on the capability half of the test (ADR-0033 clause 35): an
    /// Agent that does not declare `AcceptsPackages` is offered nothing, and its host may fetch
    /// nothing through it, although its assignment names a signed Package that fits it.
    /// Verifies: ADR-0033
    #[test]
    fn an_agent_that_accepts_no_packages_is_neither_offered_nor_fetched_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, _, id) = delivering_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_from(&state, "h1", uid, &[]);
        assert_eq!(state.rollout_deployment("stable").expect("rollout"), 1);
        assert!(state.offers_artifact("h1", &id, &linux()));
        let attr = opamp::attributes::string_attr;
        let reply = state.process_presented(
            AgentToServer {
                instance_uid: uid.as_bytes().to_vec(),
                sequence_num: 1,
                capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
                agent_description: Some(AgentDescription {
                    identifying_attributes: vec![attr("service.name", "otelcol")],
                    non_identifying_attributes: vec![
                        attr("os.type", "linux"),
                        attr("host.arch", "amd64"),
                    ],
                }),
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&Presented {
                id: CertId::new(b"CA", "h1"),
                host: Some("h1".to_string()),
                subject_values: crate::revocation::SubjectValues::default(),
                not_after_ms: u64::MAX,
            }),
        );
        assert!(reply.reply.packages_available.is_none(), "no offer");
        assert!(!state.offers_artifact("h1", &id, &linux()), "and no fetch");
    }

    /// An Agent that echoes the aggregate hash of its offer is not sent it again, and can still
    /// fetch it — a retry after a failed install re-reads an offer no longer re-sent.
    /// Verifies: ADR-0033
    #[test]
    fn an_offer_still_stands_after_its_hash_is_echoed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, _, id) = delivering_fleet(dir.path());
        let uid = InstanceUid([1; 16]);
        report_from(&state, "h1", uid, &[]);
        state.rollout_deployment("stable").expect("rollout");
        let offer = report_from(&state, "h1", uid, &[])
            .reply
            .packages_available
            .expect("an offer");
        let echoed = report_from(&state, "h1", uid, &offer.all_packages_hash);
        assert!(echoed.reply.packages_available.is_none(), "the hash gate");
        assert!(state.offers_artifact("h1", &id, &linux()));
    }

    /// A version saved into the channel but not yet released by an operator's press is no one's
    /// offer: not the Agent's host's, not a Gateway's — while the version released before stays.
    /// Verifies: ADR-0033
    #[test]
    fn a_version_waiting_for_its_press_is_offered_to_no_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, revocations, v1) = delivering_fleet(dir.path());
        report_from(&state, "h1", InstanceUid([1; 16]), &[]);
        report_from(&state, "gw", InstanceUid([9; 16]), &[]);
        assert!(revocations.set_gateway("gw", true).expect("mark"));
        state.rollout_deployment("stable").expect("rollout");

        let v2 = PackageId::new("otelcol", "2.0.0").expect("id");
        let store = state.packages().expect("store");
        store.create(&v2).expect("create");
        store
            .put_entry(&v2, &linux(), b"v2".to_vec())
            .expect("entry");
        put_signed(&state, &v2);
        for host in ["h1", "gw"] {
            assert!(!state.offers_artifact(host, &v2, &linux()), "{host}");
            assert!(state.offers_artifact(host, &v1, &linux()), "{host}");
        }
        assert_eq!(state.rollout_deployment("stable").expect("the press"), 2);
        assert!(state.offers_artifact("h1", &v2, &linux()));
    }

    /// A Package backend on files whose sweeps, once armed, wait at a gate for the blob store's
    /// lock, as a sweep on PostgreSQL waits for another instance's.
    struct HeldSweeps {
        inner: crate::fs::FsPackageBackend,
        armed: Arc<std::sync::atomic::AtomicBool>,
        reached: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        go: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl crate::packages::PackageBackend for HeldSweeps {
        fn load(&self) -> Result<Vec<crate::packages::Package>, String> {
            self.inner.load()
        }
        fn create(&self, id: &PackageId) -> Result<(), crate::packages::PackageError> {
            self.inner.create(id)
        }
        fn put_entry(
            &self,
            id: &PackageId,
            entry: &crate::packages::Entry,
            stored: crate::packages::Stored,
        ) -> Result<(), crate::packages::PackageError> {
            self.inner.put_entry(id, entry, stored)
        }
        fn sign(
            &self,
            id: &PackageId,
            platform: &Platform,
            content_hash: &[u8],
            signature: Option<&[u8]>,
        ) -> Result<crate::packages::Signed, crate::packages::PackageError> {
            self.inner.sign(id, platform, content_hash, signature)
        }
        fn remove_entry(
            &self,
            id: &PackageId,
            platform: &Platform,
        ) -> Result<(), crate::packages::PackageError> {
            self.inner.remove_entry(id, platform)
        }
        fn remove(&self, id: &PackageId) -> Result<(), crate::packages::PackageError> {
            self.inner.remove(id)
        }
        fn hold_blobs(&self) -> Result<crate::blobs::BlobHold, String> {
            if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
                // Taken out first: the guard of `reached` is not held through the wait, so a
                // second sweep passes the gate.
                let reached = self.reached.lock().expect("reached").take();
                if let Some(reached) = reached {
                    reached.send(()).expect("reached");
                    self.go.lock().expect("go").recv().expect("go");
                }
            }
            Ok(crate::blobs::BlobHold::none())
        }
    }

    /// The deletion of a Package sweeps its blobs once it has let the fleet lock go: while the
    /// sweep waits for the blob store's lock, every other act of the fleet goes on.
    /// Verifies: ADR-0023
    #[test]
    fn a_package_deletion_sweeps_once_it_lets_the_fleet_lock_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let packages = dir.path().join("packages");
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (reached, reaches) = std::sync::mpsc::channel();
        let (go, goes) = std::sync::mpsc::channel();
        let store = PackageStore::with_backend(
            Box::new(HeldSweeps {
                inner: crate::fs::FsPackageBackend::open(&packages).expect("backend"),
                armed: armed.clone(),
                reached: Mutex::new(Some(reached)),
                go: Mutex::new(goes),
            }),
            Box::new(crate::fs::FsBlobStore::open(&packages).expect("blobs")),
            crate::clock::Elapsed::monotonic(),
        )
        .expect("store");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        store.create(&id).expect("create");
        store
            .put_entry(&id, &linux(), b"v1".to_vec())
            .expect("entry");
        let state = Arc::new(
            AppState::new(dir.path().join("configs"))
                .expect("state")
                .with_packages(Some(
                    PackageOffering::new(&packages, store).expect("offering"),
                )),
        );
        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let deleting = std::thread::spawn({
            let (state, id) = (state.clone(), id.clone());
            move || state.delete_package_set(&id)
        });
        reaches
            .recv()
            .expect("the deletion's sweep waits for the lock");
        let (acted, acts) = std::sync::mpsc::channel();
        std::thread::spawn({
            let state = state.clone();
            move || {
                let _ = acted.send(state.delete_configuration("none").map_err(|_| ()));
            }
        });
        assert_eq!(
            acts.recv_timeout(std::time::Duration::from_secs(30)),
            Ok(Ok(false)),
            "an act under the fleet lock goes on while the sweep waits"
        );
        go.send(()).expect("go on");
        assert_eq!(deleting.join().expect("the deletion"), Ok(true));
    }

    /// An entry's deletion sweeps once it has let go of every lock of the Package store: while
    /// its sweep waits for the blob store's lock, the deletion of another Package, which takes
    /// the fleet lock and then the store's, goes on, and so does every act behind the fleet lock.
    /// Verifies: ADR-0023
    #[test]
    fn an_entry_deletion_sweeps_once_it_lets_the_store_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let packages = dir.path().join("packages");
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (reached, reaches) = std::sync::mpsc::channel();
        let (go, goes) = std::sync::mpsc::channel();
        let store = PackageStore::with_backend(
            Box::new(HeldSweeps {
                inner: crate::fs::FsPackageBackend::open(&packages).expect("backend"),
                armed: armed.clone(),
                reached: Mutex::new(Some(reached)),
                go: Mutex::new(goes),
            }),
            Box::new(crate::fs::FsBlobStore::open(&packages).expect("blobs")),
            crate::clock::Elapsed::monotonic(),
        )
        .expect("store");
        let (first, second) = (
            PackageId::new("otelcol", "1.0.0").expect("id"),
            PackageId::new("otelcol", "2.0.0").expect("id"),
        );
        for (id, bytes) in [(&first, b"v1"), (&second, b"v2")] {
            store.create(id).expect("create");
            store
                .put_entry(id, &linux(), bytes.to_vec())
                .expect("entry");
        }
        let state = Arc::new(
            AppState::new(dir.path().join("configs"))
                .expect("state")
                .with_packages(Some(
                    PackageOffering::new(&packages, store).expect("offering"),
                )),
        );
        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let deleting = std::thread::spawn({
            let (state, id) = (state.clone(), first.clone());
            move || state.delete_package_entry(&id, &linux())
        });
        reaches
            .recv()
            .expect("the entry deletion's sweep waits for the lock");
        let (acted, acts) = std::sync::mpsc::channel();
        std::thread::spawn({
            let (state, id) = (state.clone(), second.clone());
            move || {
                let deleted = state.delete_package_set(&id);
                let other = state.delete_configuration("none").map_err(|_| ());
                let _ = acted.send((deleted, other));
            }
        });
        assert_eq!(
            acts.recv_timeout(std::time::Duration::from_secs(30)),
            Ok((Ok(true), Ok(false))),
            "another Package's deletion and an act under the fleet lock go on while the sweep \
             waits"
        );
        go.send(()).expect("go on");
        assert_eq!(deleting.join().expect("the entry deletion"), Ok(true));
    }

    /// On PostgreSQL the check of a Gateway's package download, and the view of an Agent behind
    /// it, read the Agents that arrived over the Gateway's Host through the port's `query_page`,
    /// not the whole fleet, and still find the Client Agent behind the Gateway whose labels the
    /// Agent inherits.
    /// Verifies: ADR-0030, ADR-0033
    #[test]
    fn a_gateways_download_and_an_agent_behind_it_read_the_gateways_agents_and_not_the_fleet() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let dir = tempfile::tempdir().expect("blobs");
        let a = delivering_on(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
            dir.path(),
        );
        let (client, agent, other) = (
            InstanceUid([0xb1; 16]),
            InstanceUid([0xb2; 16]),
            InstanceUid([0xb3; 16]),
        );
        let none = crate::revocation::SubjectValues::default;
        let takes = opamp::proto::AgentCapabilities::ReportsStatus as u64
            | opamp::proto::AgentCapabilities::AcceptsPackages as u64
            | opamp::proto::AgentCapabilities::ReportsPackageStatuses as u64;
        report_listed(
            &a,
            "gw",
            none(),
            client,
            1,
            takes,
            &[("service.name", "fleet-agent")],
            &[("supervisor.kind.otelcol", "true"), ("host.name", "m1")],
        );
        assert_eq!(a.set_gateway("gw", true), Ok(true));
        report_listed(
            &a,
            "gw",
            none(),
            agent,
            1,
            takes,
            &[("service.name", "otelcol")],
            &[
                ("os.type", "linux"),
                ("host.arch", "amd64"),
                ("host.name", "m1"),
            ],
        );
        report_listed(
            &a,
            "h2",
            none(),
            other,
            1,
            takes,
            &[("service.name", "otelcol")],
            &[("os.type", "linux"), ("host.arch", "amd64")],
        );
        label(&a, &client, &[("ring", "early")]);
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        let linux = Platform::new("linux", "amd64").expect("platform");
        a.create_package_set(&id).expect("set");
        a.put_package_entry(&id, &linux, &mut &b"v1"[..])
            .expect("entry");
        a.put_package_entry_signature(&id, &linux, vec![1; 64])
            .expect("signature");
        a.deployment_store()
            .expect("deployments")
            .put(
                "early",
                BTreeMap::from([("ring".to_string(), "early".to_string())]),
            )
            .expect("deployment");
        a.put_deployment_package("early", &id, true)
            .expect("hold it");
        assert_eq!(
            a.rollout_deployment("early").expect("rollout"),
            1,
            "the inheriting Agent"
        );

        let hooked = Hooked::new(Box::new(
            crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open"),
        ));
        let whole = hooked.whole_queries.clone();
        let b = delivering_on(&database, Box::new(hooked), dir.path());
        assert!(
            b.offers_artifact("gw", &id, &linux),
            "the Gateway fetches what its Agent was released"
        );
        assert!(
            !b.offers_artifact("h2", &id, &linux),
            "nothing was released over h2"
        );
        let view = b.agent_view(&agent).expect("read").expect("known");
        assert_eq!(
            view.inherited_labels.get("ring").map(String::as_str),
            Some("early")
        );
        assert_eq!(view.client_agent, client.to_string());
        assert_eq!(
            whole.load(Ordering::SeqCst),
            0,
            "no read of the whole fleet"
        );
    }
}
