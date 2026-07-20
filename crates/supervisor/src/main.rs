//! OpAMP Fleet Supervisor Host — the client process that runs many Supervisors.
//!
//! ADR-0003 establishes this binary as part of the workspace. The tokio runtime, the OpAMP HTTP
//! client, and the Supervisor report loop that closes the control loop (ADR-0004) are added in the
//! following steps; for now it only proves the crate builds and runs.

fn main() {
    println!(
        "OpAMP Fleet Supervisor Host (scaffold, targeting OpAMP spec {}). \
         OpAMP client and report loop arrive with ADR-0004.",
        opamp::OPAMP_SPEC_VERSION
    );
}
