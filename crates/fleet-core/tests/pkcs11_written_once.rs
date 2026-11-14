//! The PKCS#11 primitives are written once, in `fleet_core::pkcs11`, as ADR-0008 clause 16 has
//! code both an end and the operator tool would implement identically live, and clause 18 has a
//! measurement that finds one written twice move it there. The Server's token adapter, the
//! operator program's, and the operator program's test token all use it, and none restates the
//! curve, the error text, the module's context and its initialisation, the slot lookup, the user's
//! login, the point or the signature's DER. The test reads source text without its comments, as the
//! dependency-direction test reads source.

use std::fs;
use std::path::Path;

/// The files that reach a token, by their path from the workspace's `crates` directory.
const USERS: &[&str] = &[
    "fleet-server/src/pkcs11.rs",
    "fleet-tools/src/token.rs",
    "fleet-tools/src/pki.rs",
];

/// What `fleet_core::pkcs11` defines or does and a user of a token must not write again.
const RESTATED: &[&str] = &[
    "P256_PARAMS: &[u8]",
    "fn text(",
    "fn contexts(",
    "fn context(",
    "fn slot_of(",
    "fn log_in(",
    "fn ec_point(",
    "fn der_signature(",
    "Pkcs11::new(",
    ".initialize(",
    "get_slots_with_token(",
    "UserType::User",
];

/// `source` without its comments: a line that is one, and what follows ` //` on a line. None of the
/// files read holds `//` inside a string.
fn code(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| line.split(" //").next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// ADR-0008 clauses 16 and 18: one PKCS#11 adapter, which every token user calls.
/// Verifies: ADR-0008
#[test]
fn every_user_of_a_token_calls_the_primitives_of_the_core_and_restates_none() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for user in USERS {
        let source = code(&fs::read_to_string(crates.join(user)).expect(user));
        assert!(
            source.contains("fleet_core::pkcs11::"),
            "{user} does not call fleet_core::pkcs11"
        );
        for restated in RESTATED {
            assert!(
                !source.contains(restated),
                "{user} restates {restated:?}, which fleet_core::pkcs11 holds"
            );
        }
    }
}
