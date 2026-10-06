//! The OpAMP Fleet Server (ADR-0011, ADR-0012): the control plane that tells Agents which
//! configuration they should run and records what they report back.
//!
//! A library crate so integration tests can assemble the exact router the binary serves.

pub mod agent_rate;
pub mod agent_store;
pub mod api;
pub mod audit;
pub mod audit_log;
pub mod ca;
pub mod clock;
pub mod config;
pub mod configs;
pub mod credentials;
pub mod deployments;
pub mod enrolment;
pub mod fleet;
pub mod fs;
pub mod labels;
pub mod listen;
pub mod packages;
pub mod revocation;
pub mod throttle;
pub mod tls;
pub mod transport;

use std::sync::Arc;

use axum::Router;

use deployments::DeploymentStore;
use fleet::{AppState, PackageOffering};
use fs::{FsAgentStore, FsConfigBackend, FsDeploymentBackend, FsLabelStore, FsPackageBackend};

/// The default wiring of the fleet's state to its storage adapters (ADR-0006): it lives here, in
/// the composition root, so the fleet logic names the ports — [`AgentStore`](agent_store::AgentStore)
/// and [`LabelStore`](labels::LabelStore) — and never the filesystem behind them.
impl AppState {
    /// Builds the state on the default storage adapters — one JSON file per Agent under
    /// `<config_dir>/agents/` and per labelled Agent under `<config_dir>/labels/` (ADR-0026) — restoring every persisted Configuration and Agent
    /// record. A store that cannot be opened (or holds an unparsable file) fails startup loudly.
    ///
    /// # Errors
    /// Returns an error when a store cannot be opened or holds an unparsable record.
    pub fn new(config_dir: std::path::PathBuf) -> Result<Self, String> {
        let agents = FsAgentStore::open(config_dir.join("agents"))?;
        let labels = FsLabelStore::open(config_dir.join("labels"))?;
        let configs = FsConfigBackend::open(config_dir)?;
        Self::with_stores(
            Box::new(agents),
            Box::new(labels),
            Box::new(configs),
            Box::new(clock::SystemClock),
        )
    }
}

/// The default wiring of the package store to its filesystem adapter (ADR-0006).
impl packages::PackageStore {
    /// Opens the store on `dir`, loading every persisted Package (ADR-0020).
    ///
    /// # Errors
    /// Returns an error when the directory cannot be opened, or a Package in it cannot be read,
    /// does not match its recorded hash, or is in a layout this Server does not write.
    pub fn open(dir: std::path::PathBuf) -> Result<Self, String> {
        Self::with_backend(Box::new(FsPackageBackend::open(dir)?))
    }
}

/// The default wiring of the package offering to the Deployments' filesystem adapter (ADR-0006).
impl PackageOffering {
    /// Opening the package store arms the **Deployments** too, from `deployments/` beneath it
    /// (ADR-0030): a Deployment is meaningless without the artifacts it signs, so the two share a
    /// directory and a configuration key rather than acquiring one of their own. See
    /// [`with_deployments`](PackageOffering::with_deployments) for `download_base`.
    ///
    /// # Errors
    /// Returns an error when the Deployments cannot be opened or one of them cannot be read.
    pub fn new(store: packages::PackageStore, download_base: String) -> Result<Self, String> {
        let backend = FsDeploymentBackend::open(store.dir().join(packages::DEPLOYMENTS_DIR))?;
        let deployments = DeploymentStore::open(Box::new(backend))?;
        Ok(Self::with_deployments(store, deployments, download_base))
    }
}

/// The **Agent plane** (ADR-0038): the OpAMP endpoint, guarded by Admission (ADR-0059), and the
/// package download route beside it — behind the same handshake, and reached only with a
/// certificate of the fleet (ADR-0059 clause 23).
///
/// The download lives here rather than with the rest of `/api/v1` because the split between the
/// two planes is by *audience*, not by path: this route is the one an Agent calls, and its
/// `download_url` is resolved against the Agent's own endpoint.
pub fn agent_app(state: Arc<AppState>, admission: transport::Admission) -> Router {
    let guard = admission
        .download_guard()
        .with_agent_rate(state.agent_rate().cloned());
    transport::router(state.clone(), admission).merge(transport::guard_download(
        api::download_router(state),
        guard,
    ))
}

/// The **Operator plane** (ADR-0012): the REST API, its OpenAPI document and docs page, and the
/// bundled UI — on their own listener, guarded as a whole by `[rest.auth]` when one is configured
/// (ADR-0017). Without it the plane is open, which is what its loopback default is for.
pub fn operator_app(state: Arc<AppState>, auth: Option<api::OperatorAuth>) -> Router {
    api::router(state, auth)
}
