# Ticket: The REST API module is split by resource

## What to build

`crates/fleet-server/src/api.rs` is 3700 lines for about ten resources and has no unit tests; every
route, view and conversion lives in it.

Split `api` into one module per resource behind the same router and OpenAPI derivation, and update
the overview's REST views paragraph.

## Acceptance criteria

- [ ] Given the REST API's OpenAPI document, when the change lands, then it is byte-identical.
- [ ] Given `crates/fleet-server/tests/rest_api.rs`, when the change lands, then it passes
  unchanged.
- [ ] Given `dependency_direction.rs`, when it runs, then every new module has its role.

## Read first

- ADR-0025, ADR-0009.
- `crates/fleet-server/src/api.rs`.

## Blocked by

- docs/plan/revision-2026-10-31/27-api-owns-the-wire-shape-of-the-views.md

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
