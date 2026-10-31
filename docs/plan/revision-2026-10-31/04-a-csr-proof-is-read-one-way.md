# Ticket: A proof a CSR carries is read one way, on the Server and on a Gateway

## What to build

The bootstrap proof is pulled out of a CSR by hand three times: twice in
`crates/fleet-server/src/ca.rs` (`bootstrap_proof`, `bootstrap_proof_certificate`) and once in
`crates/fleet-agent/src/gateway/mod.rs`, each as DER, requested URI by prefix, base64url decoding
and `bootstrap_proof::decode`. The renewal proof is found a second way, through rcgen's subject
alternative names, with its own check that a request carries not both. `fleet_core::csr` itself
warns against reading one request two ways.

Build one reader in `fleet_core::csr` that returns the proof of a given kind, and one refusal of a
request that carries both, and have the Server and the Gateway use them.

## Acceptance criteria

- [ ] Given a CSR with a bootstrap proof, when the Server and a Gateway read it, then both call the
  same `fleet_core::csr` function and reach the same verdict.
- [ ] Given a CSR that carries both proofs, when it reaches the Server or a Gateway, then both
  refuse it by the same function.
- [ ] Given the existing enrolment and renewal tests, when the change lands, then they pass
  unchanged.

## Read first

- ADR-0015 (the clauses on the enrolling peer), ADR-0022, ADR-0008 clause 16.
- `crates/fleet-core/src/csr.rs`, `crates/fleet-core/src/bootstrap_proof.rs`,
  `crates/fleet-core/src/renewal.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
