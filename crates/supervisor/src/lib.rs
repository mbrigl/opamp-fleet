//! OpAMP Fleet Supervisor Host library.
//!
//! The [`Supervisor`] holds one Managed Agent's state and builds/handles OpAMP messages; the
//! [`OpampHttpClient`] carries those messages to the Server over plain HTTP (ADR-0004). The
//! `supervisor-host` binary wires them into a report loop.

pub mod client;
mod supervisor;

pub use client::OpampHttpClient;
pub use supervisor::{Supervisor, DEFAULT_POLL};

/// The version this build reports to the fleet.
///
/// The release pipeline (ADR-0008) bakes the tag-derived version in via the `OPAMP_FLEET_VERSION`
/// environment variable at compile time; local and CI builds, which do not set it, fall back to the
/// crate version. This is what the OpAMP `service.version` attribute and the self-update health
/// report announce.
#[must_use]
pub fn version() -> &'static str {
    option_env!("OPAMP_FLEET_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_falls_back_to_the_crate_version_when_not_baked_in() {
        assert!(!super::version().is_empty());
        // The release pipeline sets OPAMP_FLEET_VERSION; local and CI builds do not.
        if option_env!("OPAMP_FLEET_VERSION").is_none() {
            assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        }
    }
}
