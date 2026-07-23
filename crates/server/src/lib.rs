//! The OpAMP Fleet Server (ADR-0005): the control plane that tells Agents which configuration
//! they should run and records what they report back.
//!
//! A library crate so integration tests can assemble the exact router the binary serves. This
//! commit carries the fleet state, the REST API, and the bundled UI; the OpAMP endpoint itself —
//! both transports on `/v1/opamp` — arrives with ADR-0007.

pub mod api;
pub mod config;
pub mod fleet;

use std::sync::Arc;

use axum::Router;

use fleet::AppState;

/// The application: REST API and UI on one router (ADR-0005).
pub fn app(state: Arc<AppState>) -> Router {
    api::router(state)
}
