//! The OpAMP Fleet Server (ADR-0005, ADR-0007): the control plane that tells Agents which
//! configuration they should run and records what they report back.
//!
//! A library crate so integration tests can assemble the exact router the binary serves.

pub mod agent_store;
pub mod api;
pub mod ca;
pub mod config;
pub mod configs;
pub mod fleet;
pub mod labels;
pub mod packages;
pub mod tls;
pub mod transport;

use std::sync::Arc;

use axum::Router;

use fleet::AppState;

/// The complete application: OpAMP endpoint, REST API, and UI on one router (ADR-0005). Admission
/// guards the OpAMP endpoint alone (ADR-0013, ADR-0035) — REST API and UI stay open,
/// operator-facing auth being a separate decision.
pub fn app(state: Arc<AppState>, admission: transport::Admission) -> Router {
    transport::router(state.clone(), admission).merge(api::router(state))
}
