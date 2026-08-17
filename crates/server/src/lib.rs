//! The OpAMP Fleet Server (ADR-0005, ADR-0007): the control plane that tells Agents which
//! configuration they should run and records what they report back.
//!
//! A library crate so integration tests can assemble the exact router the binary serves.

pub mod agent_store;
pub mod api;
pub mod ca;
pub mod config;
pub mod configs;
pub mod credentials;
pub mod fleet;
pub mod labels;
pub mod packages;
pub mod tls;
pub mod transport;

use std::sync::Arc;

use axum::Router;

use fleet::AppState;

/// The **Agent plane** (ADR-0066): the OpAMP endpoint, guarded by Admission (ADR-0013, ADR-0035),
/// and the package download route beside it — outside that guard, because a downloading Client
/// presents neither credential nor certificate and the artifact's hash and signature are what
/// protect it (ADR-0015).
///
/// The download lives here rather than with the rest of `/api/v1` because the split between the
/// two planes is by *audience*, not by path: this route is the one an Agent calls, and its
/// `download_url` is resolved against the Agent's own endpoint.
pub fn agent_app(state: Arc<AppState>, admission: transport::Admission) -> Router {
    transport::router(state.clone(), admission).merge(api::download_router(state))
}

/// The **Operator plane** (ADR-0066): the REST API, its OpenAPI document and docs page, and the
/// bundled UI — on their own listener, guarded as a whole by `[rest.auth]` when one is configured
/// (ADR-0067). Without it the plane is open, which is what its loopback default is for.
pub fn operator_app(state: Arc<AppState>, auth: Option<api::OperatorAuth>) -> Router {
    api::router(state, auth)
}
