//! The Operator plane's use cases (SRV-two-use-case-layers §1): every operation the REST API, the
//! bundled UI and any later Operator-plane endpoint read and change, as the methods of one type,
//! [`OperatorCases`]. `api` carries HTTP and JSON and calls these methods and nothing else that
//! holds state (SRV-two-use-case-layers §2); the core's values, records, views and errors cross
//! unchanged (SRV-two-use-case-layers §3). Audit and the throttle stay in the core and are reached
//! here (SRV-two-use-case-layers §4).
//!
//! The plane's credential check is [`OperatorAuth`]; the root builds it from `[rest.auth]` and
//! hands it to [`OperatorCases`] with the fleet's state.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

use axum::http::HeaderMap;
use opamp::uid::InstanceUid;

use crate::agent_store::ReportedValues;
use crate::audit::{Audit, Entry, Permit, Unavailable};
use crate::bootstrap::{
    DeleteError as BootstrapDeleteError, Entry as BootstrapEntry, IssueError, IssueRequest, Pair,
};
use crate::config::RestAuthConfig;
use crate::configs::{Configuration, Revision};
use crate::credentials::{Credentials, Verdict};
use crate::deployments::{Deployment, DeploymentError};
use crate::enrolment::{DecisionError, Pending, Rejection, WindowError};
use crate::fleet::{
    AgentListing, AgentQuery, AgentView, AppState, DeleteError, DeploymentReach, ForgetError,
    HostAgents, RestartError, RolloutError, StoreFull, UploadReservation,
};
use crate::floors::{Floor, FloorError};
use crate::labels::LabelError;
use crate::packages::{PackageError, PackageId, PackageSummary, Platform, SignatureState};
use crate::revocation::{CertId, Host, Issued, Revocation, RevokeError};
use crate::throttle::Throttle;

/// The Operator plane's credential check (SEC-admission-by-client-certificate), precomputed from `[rest.auth]`. Basic only,
/// and it guards the whole plane — the API, its document, the docs page, and the UI — because a
/// browser answers a Basic challenge by itself, which is what spares the rudimentary UI a login
/// page and a session.
pub struct OperatorAuth(
    Arc<Credentials>,
    Option<Arc<Throttle>>,
    Option<Arc<dyn Audit>>,
);

impl OperatorAuth {
    /// # Errors
    /// Returns an error naming an entry of `[rest.auth]` that is not a hash this Server keeps.
    pub fn from_config(auth: &RestAuthConfig) -> Result<Self, String> {
        Ok(OperatorAuth(Arc::new(auth.credentials()?), None, None))
    }

    /// Records refused sign-ins in `audit` (SEC-append-only-audit-record §1).
    #[must_use]
    pub fn with_audit(mut self, audit: Option<Arc<dyn Audit>>) -> Self {
        self.2 = audit;
        self
    }

    /// Counts this plane's failures in a table of its own (SEC-admission-by-client-certificate §19).
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<Throttle>) -> Self {
        self.1 = Some(throttle);
        self
    }
}

/// A bootstrap certificate's entry with its PKCS#12 file.
type P12 = (BootstrapEntry, Vec<u8>);

/// What the plane's guard decided about one request.
pub enum SignIn {
    /// A configured credential came with it, or no credential is configured.
    Admitted,
    /// The peer address has failed too often; it may ask again after this many seconds.
    Throttled(u64),
    /// Too many password hashes are being computed; the request may be sent again shortly.
    Busy,
    /// No configured credential came with it; the challenge to answer it with.
    Refused(String),
}

/// The Operator plane's operations over the fleet's state.
pub struct OperatorCases {
    state: Arc<AppState>,
    auth: Option<OperatorAuth>,
}

impl OperatorCases {
    /// The Operator plane's use cases over `state`, guarded by `auth` where one is configured.
    #[must_use]
    pub fn new(state: Arc<AppState>, auth: Option<OperatorAuth>) -> Self {
        Self { state, auth }
    }

    // The guard and the record.

    /// Whether the plane is guarded by `[rest.auth]` at all.
    #[must_use]
    pub fn guarded(&self) -> bool {
        self.auth.is_some()
    }

    /// Checks the credential a request carries, after the peer address's back-off. A refusal and
    /// a throttled attempt are recorded as refusals, named by `operator`, the user name of the
    /// request's Basic header and never its password (SEC-append-only-audit-record §1).
    pub async fn sign_in(
        &self,
        peer: Option<IpAddr>,
        operator: Option<String>,
        headers: &HeaderMap,
    ) -> SignIn {
        let Some(auth) = &self.auth else {
            return SignIn::Admitted;
        };
        let refusal = |event: &str, outcome: &str| {
            if let Some(audit) = &auth.2 {
                audit.refusal(
                    Entry::new(event, outcome)
                        .peer(peer)
                        .with("plane", "operator")
                        .with("operator", operator.clone()),
                );
            }
        };
        if let (Some(throttle), Some(peer)) = (&auth.1, peer) {
            if let Some(wait) = throttle.retry_after(peer) {
                refusal("operator.throttled", "throttled");
                return SignIn::Throttled(wait);
            }
        }
        // The attempt counts before the password is hashed, so attempts sent at once cannot all pass
        // the back-off check first (SEC-admission-by-client-certificate §2).
        let _attempt = match (&auth.1, peer) {
            (Some(throttle), Some(peer)) => match throttle.begin(peer) {
                Some(attempt) => Some(attempt),
                None => {
                    refusal("operator.throttled", "throttled");
                    return SignIn::Throttled(throttle.retry_after(peer).unwrap_or(1));
                }
            },
            _ => None,
        };
        let verdict = auth.0.check(peer, headers).await;
        if verdict == Verdict::Busy {
            return SignIn::Busy;
        }
        if verdict == Verdict::Refused {
            if let (Some(throttle), Some(peer)) = (&auth.1, peer) {
                throttle.failed(peer);
            }
            refusal("operator.refused", "refused");
            return SignIn::Refused(auth.0.challenge().to_string());
        }
        SignIn::Admitted
    }

    /// Whether the Server keeps an audit record.
    #[must_use]
    pub fn audits(&self) -> bool {
        self.state.audit().is_some()
    }

    /// Records `entry` before the act it names runs; an act that cannot be recorded does not run
    /// (SEC-append-only-audit-record §1 and §5). Without a record there is nothing to refuse.
    ///
    /// # Errors
    /// [`Unavailable`] when the record cannot take the entry now.
    pub fn record(&self, entry: Entry) -> Result<(), Unavailable> {
        self.state
            .audit()
            .map_or(Ok(()), |audit| audit.record(entry))
    }

    /// Records what an operator act did, beside the act itself (SEC-append-only-audit-record §1), and drops it where
    /// the record cannot take it by then: the act was recorded before it ran.
    pub fn note(&self, entry: Entry) {
        if let Some(audit) = self.state.audit() {
            audit.refusal(entry);
        }
    }

    /// Holds a place in the record for the entry of an act about to run (SEC-append-only-audit-record §5).
    /// `None` where the Server keeps no record.
    ///
    /// # Errors
    /// [`Unavailable`] when the record cannot hold a place now; the act does not run.
    pub fn hold_place(&self) -> Result<Option<Permit>, Unavailable> {
        self.state.audit().map(|audit| audit.reserve()).transpose()
    }

    // Agents.

    /// # Errors
    /// Returns an error when the fleet cannot be read from the store.
    pub fn agent_view(&self, uid: &InstanceUid) -> Result<Option<AgentView>, String> {
        self.state.agent_view(uid)
    }

    /// # Errors
    /// Returns an error when the fleet cannot be read from the store.
    pub fn list_agents(&self, query: &AgentQuery) -> Result<AgentListing, String> {
        self.state.list_agents(query)
    }

    /// Whether the store keeps the holders an Agent list may be filtered by.
    #[must_use]
    pub fn keeps_holders(&self) -> bool {
        self.state.keeps_holders()
    }

    /// # Errors
    /// Returns an error when the fleet cannot be read from the store.
    pub fn reported_values(&self) -> Result<ReportedValues, String> {
        self.state.reported_values()
    }

    /// # Errors
    /// See [`RestartError`].
    pub fn request_restart(&self, uid: &InstanceUid) -> Result<(), RestartError> {
        self.state.request_restart(uid)
    }

    /// # Errors
    /// See [`LabelError`].
    pub fn set_labels(
        &self,
        uid: &InstanceUid,
        set: BTreeMap<String, String>,
    ) -> Result<(), LabelError> {
        self.state.set_labels(uid, set)
    }

    /// # Errors
    /// See [`RolloutError`].
    pub fn rollout_to_agent(
        &self,
        uid: &InstanceUid,
        named: Option<&str>,
    ) -> Result<(), RolloutError> {
        self.state.rollout_to_agent(uid, named)
    }

    /// # Errors
    /// See [`ForgetError`].
    pub fn forget_agent(&self, uid: &InstanceUid) -> Result<(), ForgetError> {
        self.state.forget_agent(uid)
    }

    // Configurations.

    /// Every stored Configuration, in name order.
    #[must_use]
    pub fn configurations(&self) -> Vec<Configuration> {
        self.state.configurations().list()
    }

    /// One stored Configuration.
    #[must_use]
    pub fn configuration(&self, name: &str) -> Option<Configuration> {
        self.state.configurations().get(name)
    }

    /// The Deployments that hold each Configuration, by its name.
    #[must_use]
    pub fn deployments_holding_configurations(&self) -> BTreeMap<String, Vec<String>> {
        self.state.deployments_holding_configurations()
    }

    /// # Errors
    /// Returns an error when the Configuration cannot be stored.
    pub fn save_configuration(
        &self,
        name: &str,
        revision: Revision,
    ) -> Result<Configuration, String> {
        self.state.save_configuration(name, revision)
    }

    /// # Errors
    /// See [`DeleteError`].
    pub fn delete_configuration(&self, name: &str) -> Result<bool, DeleteError> {
        self.state.delete_configuration(name)
    }

    // Packages.

    /// Every stored Package, in identity order; `None` where package delivery is not configured.
    #[must_use]
    pub fn packages(&self) -> Option<Vec<PackageSummary>> {
        self.state
            .packages()
            .map(crate::packages::PackageStore::list)
    }

    /// One stored Package; `None` where it is not stored or package delivery is not configured.
    #[must_use]
    pub fn package(&self, id: &PackageId) -> Option<PackageSummary> {
        self.state.packages().and_then(|store| store.summary(id))
    }

    /// The Deployments that hold each Package, by its identity.
    #[must_use]
    pub fn deployments_holding(&self) -> BTreeMap<String, Vec<String>> {
        self.state.deployments_holding()
    }

    /// # Errors
    /// See [`PackageError`].
    pub fn create_package_set(&self, id: &PackageId) -> Result<(), PackageError> {
        self.state.create_package_set(id)
    }

    /// # Errors
    /// See [`PackageError`].
    pub fn delete_package_set(&self, id: &PackageId) -> Result<bool, PackageError> {
        self.state.delete_package_set(id)
    }

    /// # Errors
    /// See [`PackageError`].
    pub fn check_package_upload(&self, id: &PackageId) -> Result<(), PackageError> {
        self.state.check_package_upload(id)
    }

    /// # Errors
    /// See [`StoreFull`].
    pub fn admit_upload(&self) -> Result<UploadReservation<'_>, StoreFull> {
        self.state.admit_upload()
    }

    /// The largest artifact an upload may carry.
    #[must_use]
    pub fn max_package_size(&self) -> usize {
        self.state.max_package_size()
    }

    /// # Errors
    /// See [`PackageError`].
    pub fn put_package_entry(
        &self,
        id: &PackageId,
        platform: &Platform,
        bytes: &mut dyn std::io::Read,
    ) -> Result<(), PackageError> {
        self.state.put_package_entry(id, platform, bytes)
    }

    /// # Errors
    /// See [`PackageError`].
    pub fn delete_package_entry(
        &self,
        id: &PackageId,
        platform: &Platform,
    ) -> Result<bool, PackageError> {
        self.state.delete_package_entry(id, platform)
    }

    /// # Errors
    /// See [`PackageError`].
    pub fn put_package_entry_signature(
        &self,
        id: &PackageId,
        platform: &Platform,
        signature: Vec<u8>,
    ) -> Result<SignatureState, PackageError> {
        self.state
            .put_package_entry_signature(id, platform, signature)
    }

    /// # Errors
    /// See [`PackageError`].
    pub fn remove_package_entry_signature(
        &self,
        id: &PackageId,
        platform: &Platform,
    ) -> Result<bool, PackageError> {
        self.state.remove_package_entry_signature(id, platform)
    }

    // Package floors.

    /// Whether package floors are configured.
    #[must_use]
    pub fn has_floors(&self) -> bool {
        self.state.floors().is_some()
    }

    /// Every floor, by Agent type, with its signature state; `None` where floors are not
    /// configured.
    #[must_use]
    pub fn floors(&self) -> Option<Vec<(String, Floor, SignatureState)>> {
        self.state.floors().map(|floors| floors.list())
    }

    /// Stores a floor; `None` where floors are not configured.
    pub fn put_floor(
        &self,
        agent_type: &str,
        version: &str,
        signature: Vec<u8>,
    ) -> Option<Result<SignatureState, FloorError>> {
        self.state
            .floors()
            .map(|floors| floors.put(agent_type, version, signature))
    }

    /// Removes a floor; `None` where floors are not configured.
    pub fn remove_floor(&self, agent_type: &str) -> Option<Result<bool, String>> {
        self.state.floors().map(|floors| floors.remove(agent_type))
    }

    // Deployments.

    /// Whether Deployments are kept, which package delivery decides.
    #[must_use]
    pub fn has_deployments(&self) -> bool {
        self.state.deployment_store().is_some()
    }

    /// Every Deployment, in name order; `None` where package delivery is not configured.
    #[must_use]
    pub fn deployments(&self) -> Option<Vec<Deployment>> {
        self.state.deployment_store().map(|store| store.list())
    }

    /// One Deployment; `None` where it does not exist or package delivery is not configured.
    #[must_use]
    pub fn deployment(&self, name: &str) -> Option<Deployment> {
        self.state
            .deployment_store()
            .and_then(|store| store.get(name))
    }

    /// Creates or re-aims a Deployment; `None` where package delivery is not configured.
    pub fn put_deployment(
        &self,
        name: &str,
        selector: BTreeMap<String, String>,
    ) -> Option<Result<Deployment, DeploymentError>> {
        self.state
            .deployment_store()
            .map(|store| store.put(name, selector))
    }

    /// Whom each Deployment reaches, by its name.
    ///
    /// # Errors
    /// Returns an error when the fleet cannot be read from the store.
    pub fn deployment_reach(&self) -> Result<BTreeMap<String, DeploymentReach>, String> {
        self.state.deployment_reach()
    }

    /// # Errors
    /// See [`DeploymentError`].
    pub fn delete_deployment(&self, name: &str) -> Result<bool, DeploymentError> {
        self.state.delete_deployment(name)
    }

    /// # Errors
    /// See [`DeploymentError`].
    pub fn put_deployment_package(
        &self,
        name: &str,
        id: &PackageId,
        replace: bool,
    ) -> Result<Deployment, DeploymentError> {
        self.state.put_deployment_package(name, id, replace)
    }

    /// # Errors
    /// See [`DeploymentError`].
    pub fn remove_deployment_package(
        &self,
        name: &str,
        id: &PackageId,
    ) -> Result<Deployment, DeploymentError> {
        self.state.remove_deployment_package(name, id)
    }

    /// # Errors
    /// See [`DeploymentError`].
    pub fn put_deployment_configuration(
        &self,
        name: &str,
        configuration: &str,
    ) -> Result<Deployment, DeploymentError> {
        self.state.put_deployment_configuration(name, configuration)
    }

    /// # Errors
    /// See [`DeploymentError`].
    pub fn remove_deployment_configuration(
        &self,
        name: &str,
        configuration: &str,
    ) -> Result<Deployment, DeploymentError> {
        self.state
            .remove_deployment_configuration(name, configuration)
    }

    /// # Errors
    /// See [`RolloutError`].
    pub fn rollout_deployment(&self, name: &str) -> Result<usize, RolloutError> {
        self.state.rollout_deployment(name)
    }

    // Enrolment.

    /// Whether enrolment is configured.
    #[must_use]
    pub fn has_enrolment(&self) -> bool {
        self.state.enrolment().is_some()
    }

    /// Until when the enrolment window is open, `None` while it is closed; the outer `None` where
    /// enrolment is not configured.
    #[must_use]
    pub fn enrolment_window(&self) -> Option<Option<u64>> {
        self.state.enrolment().map(|enrolment| enrolment.window())
    }

    /// Opens the enrolment window; `None` where enrolment is not configured.
    pub fn open_enrolment_window(&self, secs: u64) -> Option<Result<u64, WindowError>> {
        self.state.enrolment().map(|enrolment| enrolment.open(secs))
    }

    /// Closes the enrolment window and answers how many requests expired with it, and whether
    /// the closing was persisted; `None` where enrolment is not configured.
    pub fn close_enrolment_window(&self) -> Option<(usize, Result<(), String>)> {
        self.state.enrolment().map(|enrolment| {
            let expired = enrolment.pending().len();
            (expired, enrolment.close())
        })
    }

    /// The waiting enrolment requests; `None` where enrolment is not configured.
    #[must_use]
    pub fn enrolments(&self) -> Option<Vec<Pending>> {
        self.state.enrolment().map(|enrolment| enrolment.pending())
    }

    /// # Errors
    /// See [`DecisionError`].
    pub fn approve_enrolment(&self, id: &str) -> Result<String, DecisionError> {
        self.state.approve_enrolment(id)
    }

    /// The Gateway a waiting enrolment request came through, where it came through one.
    #[must_use]
    pub fn enrolment_gateway(&self, id: &str) -> Option<String> {
        self.state
            .enrolment()?
            .pending()
            .into_iter()
            .find(|pending| pending.id == id)?
            .requester
            .gateway
    }

    /// Withdraws the waiting requests a revoked bootstrap certificate carried, and answers how
    /// many; none where enrolment is not configured.
    #[must_use]
    pub fn withdraw_revoked(&self, issuers: &[String], serial: &str) -> usize {
        self.state
            .enrolment()
            .map_or(0, |enrolment| enrolment.withdraw_revoked(issuers, serial))
    }

    /// Rejects an enrolment request; `None` where enrolment is not configured.
    pub fn reject_enrolment(&self, id: &str) -> Option<Result<Rejection, DecisionError>> {
        self.state.enrolment().map(|enrolment| enrolment.reject(id))
    }

    // Bootstrap certificates.

    /// Whether this Server issues bootstrap certificates.
    #[must_use]
    pub fn has_bootstrap(&self) -> bool {
        self.state.bootstrap().is_some()
    }

    /// Every bootstrap certificate on record; `None` where this Server issues none.
    #[must_use]
    pub fn bootstrap_certificates(&self) -> Option<Vec<BootstrapEntry>> {
        self.state.bootstrap().map(|bootstrap| bootstrap.list())
    }

    /// Issues a bootstrap certificate; `None` where this Server issues none. Sealing its PKCS#12
    /// file takes the better part of a second, so the caller runs it off the async workers.
    pub fn issue_bootstrap(&self, request: &IssueRequest) -> Option<Result<Pair, IssueError>> {
        self.state
            .bootstrap()
            .map(|bootstrap| bootstrap.issue(request))
    }

    /// The entry and PKCS#12 file of a valid bootstrap certificate; `None` where this Server
    /// issues none.
    pub fn bootstrap_p12(&self, serial: &str) -> Option<Result<Option<P12>, String>> {
        self.state
            .bootstrap()
            .map(|bootstrap| bootstrap.p12(serial))
    }

    /// Removes a bootstrap certificate's PKCS#12 file; `None` where this Server issues none.
    pub fn forget_bootstrap_p12(&self, serial: &str) -> Option<Result<(), String>> {
        self.state
            .bootstrap()
            .map(|bootstrap| bootstrap.forget_p12(serial))
    }

    /// Deletes a revoked bootstrap certificate from the record; `None` where this Server issues
    /// none.
    pub fn delete_bootstrap(
        &self,
        serial: &str,
        revoked: impl FnOnce(&BootstrapEntry) -> bool,
        recorded: impl FnOnce(&BootstrapEntry) -> bool,
    ) -> Option<Result<BootstrapEntry, BootstrapDeleteError>> {
        self.state
            .bootstrap()
            .map(|bootstrap| bootstrap.delete(serial, revoked, recorded))
    }

    // Certificates, Hosts and revocations.

    /// Revokes a certificate; `None` where revocation is not configured.
    pub fn revoke_certificate(
        &self,
        authority: &str,
        serial: &str,
    ) -> Option<Result<Revocation, RevokeError>> {
        self.state
            .revocations()
            .map(|revocations| revocations.revoke_certificate(authority, serial))
    }

    /// Whether revocation is configured.
    #[must_use]
    pub fn has_revocations(&self) -> bool {
        self.state.revocations().is_some()
    }

    /// Whether the certificate is revoked; never where revocation is not configured.
    #[must_use]
    pub fn is_certificate_revoked(&self, id: &CertId) -> bool {
        self.state
            .revocations()
            .is_some_and(|revocations| revocations.is_certificate_revoked(id))
    }

    /// The role of the authority that issued certificates of `issuer`, where one is known.
    #[must_use]
    pub fn authority_role(&self, issuer: &str) -> Option<String> {
        self.state
            .revocations()
            .and_then(|revocations| revocations.authority_of(issuer).map(|a| a.role.clone()))
    }

    /// Every certificate the client CA signed; `None` where revocation is not configured.
    #[must_use]
    pub fn issued_certificates(&self) -> Option<Vec<Issued>> {
        self.state
            .revocations()
            .map(|revocations| revocations.issued())
    }

    /// Every Host, with the count of its certificates; `None` where revocation is not configured.
    #[must_use]
    pub fn hosts(&self) -> Option<Vec<(String, Host, usize)>> {
        self.state
            .revocations()
            .map(|revocations| revocations.hosts())
    }

    /// The Agents each Host carries, by host.
    ///
    /// # Errors
    /// Returns an error when the fleet cannot be read from the store.
    pub fn host_agents(&self) -> Result<BTreeMap<String, HostAgents>, String> {
        self.state.host_agents()
    }

    /// # Errors
    /// Returns an error when the mark cannot be stored.
    pub fn set_gateway(&self, host: &str, gateway: bool) -> Result<bool, String> {
        self.state.set_gateway(host, gateway)
    }

    /// Every revocation; `None` where revocation is not configured.
    #[must_use]
    pub fn revocation_list(&self) -> Option<Vec<Revocation>> {
        self.state
            .revocations()
            .map(|revocations| revocations.list())
    }

    /// Lifts a revocation; `None` where revocation is not configured.
    pub fn lift_revocation(&self, id: &str) -> Option<Result<bool, String>> {
        self.state
            .revocations()
            .map(|revocations| revocations.lift(id))
    }
}
