# Ticket: Every route that changes something documents its `503`

## What to build

Every mutating act on the Operator plane is recorded before it runs, and an act that cannot be
recorded answers `503` (`record_act` in `crates/fleet-server/src/api.rs`, ADR-0046 clauses 1 and
5). Five acts also hold a place in the audit channel first and answer `503` without one
(`hold_place`: a revocation, a Gateway mark, a floor set, a rejection, the window closed). The
OpenAPI document names a `503` on three routes only, so a client generated from it does not know
the answer the others give.

Every `POST`, `PUT`, `PATCH` and `DELETE` of the document names its `503`, and a test of the
generated document keeps it so.

## Acceptance criteria

- [ ] Given the generated OpenAPI document, when a test reads every mutating operation, then each
  names a `503` response, and the test fails naming the operation that does not.
- [ ] Given the existing REST tests, when the change lands, then they pass unchanged.

## Read first

- ADR-0025 (the REST API and its OpenAPI document); ADR-0046 clauses 1 and 5.
- `api.rs` `record_act`, `hold_place`, `ApiDoc`; `crates/fleet-server/tests/rest_api.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
