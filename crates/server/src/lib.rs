//! The OpAMP Fleet Server (ADR-0005, ADR-0007): the control plane that tells Agents which
//! configuration they should run and records what they report back.
//!
//! A library crate so integration tests can assemble the exact router the binary serves.

pub mod api;
pub mod config;
pub mod fleet;
pub mod transport;

use std::sync::Arc;

use axum::Router;

use fleet::AppState;

/// The complete application: OpAMP endpoint, REST API, and UI on one router (ADR-0005).
pub fn app(state: Arc<AppState>) -> Router {
    transport::router(state.clone()).merge(api::router(state))
}
