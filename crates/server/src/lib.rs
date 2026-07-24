//! The OpAMP Fleet Server (ADR-0005, ADR-0007): the control plane that tells Agents which
//! configuration they should run and records what they report back.
//!
//! A library crate so integration tests can assemble the exact router the binary serves.

pub mod api;
pub mod config;
pub mod configs;
pub mod fleet;
pub mod transport;

use std::sync::Arc;

use axum::Router;

use fleet::AppState;

/// The complete application: OpAMP endpoint, REST API, and UI on one router (ADR-0005). The
/// credential check guards the OpAMP endpoint alone (ADR-0013) — REST API and UI stay open,
/// operator-facing auth being a separate decision.
pub fn app(state: Arc<AppState>, auth: Option<transport::OpampAuth>) -> Router {
    transport::router(state.clone(), auth).merge(api::router(state))
}
