//! OpAMP Fleet Server — the API-first control plane.
//!
//! ADR-0003 establishes this binary as part of the workspace. Its runtime (axum + tokio), the
//! in-memory fleet state, the rudimentary UI (ADR-0005) and the OpAMP HTTP endpoint (ADR-0004)
//! are added in the following steps; for now it only proves the crate builds and runs.

fn main() {
    println!(
        "OpAMP Fleet Server (scaffold, targeting OpAMP spec {}). \
         Runtime and OpAMP endpoint arrive with ADR-0004/ADR-0005.",
        opamp::OPAMP_SPEC_VERSION
    );
}
