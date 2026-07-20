//! Capability bitmasks (specification vocabulary).
//!
//! Neither side may assume an undeclared capability, so each end advertises what it supports in the
//! `capabilities` bitmask. These helpers define the sets the first version of OpAMP Fleet declares.

use crate::v1::{AgentCapabilities, ServerCapabilities};

/// The capabilities a Supervisor declares in `AgentToServer.capabilities`.
///
/// Covers the required status reporting plus the pieces the first version implements: health,
/// effective configuration, and accepting/reporting remote configuration (the control loop).
#[must_use]
pub fn required_agent_capabilities() -> u64 {
    bits(&[
        AgentCapabilities::ReportsStatus,
        AgentCapabilities::ReportsHealth,
        AgentCapabilities::ReportsEffectiveConfig,
        AgentCapabilities::AcceptsRemoteConfig,
        AgentCapabilities::ReportsRemoteConfig,
    ])
}

/// The capabilities the Server declares in `ServerToAgent.capabilities`.
#[must_use]
pub fn server_capabilities() -> u64 {
    ((ServerCapabilities::AcceptsStatus as i32) as u64)
        | ((ServerCapabilities::OffersRemoteConfig as i32) as u64)
        | ((ServerCapabilities::AcceptsEffectiveConfig as i32) as u64)
}

fn bits(caps: &[AgentCapabilities]) -> u64 {
    caps.iter().fold(0, |acc, c| acc | (*c as i32) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_mask_sets_the_expected_bits() {
        let mask = required_agent_capabilities();
        for expected in [
            AgentCapabilities::ReportsStatus,
            AgentCapabilities::ReportsHealth,
            AgentCapabilities::ReportsEffectiveConfig,
            AgentCapabilities::AcceptsRemoteConfig,
            AgentCapabilities::ReportsRemoteConfig,
        ] {
            assert_ne!(mask & (expected as i32) as u64, 0, "missing {expected:?}");
        }
        // ReportsStatus MUST be set for every Agent.
        assert_ne!(mask & (AgentCapabilities::ReportsStatus as i32) as u64, 0);
    }

    #[test]
    fn server_mask_accepts_status_and_offers_config() {
        let mask = server_capabilities();
        assert_ne!(mask & (ServerCapabilities::AcceptsStatus as i32) as u64, 0);
        assert_ne!(
            mask & (ServerCapabilities::OffersRemoteConfig as i32) as u64,
            0
        );
    }
}
