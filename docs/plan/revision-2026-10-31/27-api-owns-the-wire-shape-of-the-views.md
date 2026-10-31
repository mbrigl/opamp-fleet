# Ticket: `api` alone owns the wire shape of the fleet views

## What to build

The view types (`AgentView`, `PendingConfigurationView`, `PendingPackageView`, `PackageStatusView`)
derive `Serialize` in the core, while `api.rs` mirrors each with a response type and a
field-by-field `From`, the doc comments copied. The core's `Serialize` looks unused: the tests
reference `AgentView` only as the OpenAPI schema name.

Drop the core's serialization of the views where nothing needs it, so `api` owns the wire shape
alone (ADR-0025).

## Acceptance criteria

- [ ] Given the REST API's OpenAPI document, when the change lands, then it is byte-identical.
- [ ] Given the core view types, when the crate is built, then none derives `Serialize` unless a
  caller needs it, and the hand-off names any that stays and why.

## Read first

- ADR-0025, ADR-0009.
- `crates/fleet-server/src/fleet.rs` (the views), `crates/fleet-server/src/api.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
