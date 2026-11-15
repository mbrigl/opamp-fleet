# Ticket: A CSR with both proofs is an error kind, not a text

## What to build

`fleet_core::csr::proof` (`crates/fleet-core/src/csr.rs`, near line 116) returns
`Err(BOTH_PROOFS.to_string())`. The Gateway tells it apart by comparing the text
(`crates/fleet-agent/src/gateway/mod.rs`, near line 154: `Err(why) if why == BOTH_PROOFS`),
and the Server's `transport.rs` (near line 1341) folds it into "unparsable" with `.ok()?`. This
breaks the convention "A caller tells errors apart by their kind, never by their text": a
rewording of the message turns the Gateway's refusal into a forward without a test noticing.

`proof` returns an error with a kind (`BothProofs`, `Unparsable`) whose `Display` gives
today's text; the Gateway and the Server match on the kind.

## Acceptance criteria

- [ ] Given a CSR that carries both proofs, when a Gateway receives it, then it refuses it without
  forwarding, as today, and the decision matches on the kind.
- [ ] Given the change, when the tree is searched for `== fleet_core::csr::BOTH_PROOFS` or a
  comparison with its text outside tests, then none is found.
- [ ] Given the existing tests in `crates/fleet-server/tests/mutual_tls.rs`, when the change
  lands, then the reason the Server records reads as today.

## Read first

- Touches: `crates/fleet-core/src/csr.rs`, `crates/fleet-agent/src/gateway/mod.rs`,
  `crates/fleet-server/src/transport.rs`, `crates/fleet-server/src/ca.rs`
- CONVENTIONS: "A caller tells errors apart by their kind, never by their text".
- ADR-0040 (enrolment through a Gateway), ADR-0008 clause 17.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
