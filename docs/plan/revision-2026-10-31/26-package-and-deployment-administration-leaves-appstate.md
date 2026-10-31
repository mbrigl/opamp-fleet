# Ticket: Package and Deployment administration leaves `AppState`

## What to build

Package and Deployment administration in `fleet.rs` mostly forwards to `PackageStore` and
`DeploymentStore` and adds `refuse_if_assigned`, inside an `AppState` of about 70 public methods.

Move it into a module of its own that `api` calls, keeping the refusal of what an assignment still
names, and update the overview.

## Acceptance criteria

- [ ] Given an operator's package or Deployment change through the REST API, when it lands, then the
  responses are unchanged, the refusal of an assigned Package included.
- [ ] Given `AppState`, when the change lands, then it no longer carries the administration methods.

## Read first

- ADR-0026, ADR-0027, ADR-0009.
- `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/tests/rest_api.rs`.

## Blocked by

- docs/plan/revision-2026-10-31/13-the-servers-report-processing-has-a-module.md

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
