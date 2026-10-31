# Ticket: The Server's report processing is a module of named steps

## What to build

`AppState` in `crates/fleet-server/src/fleet.rs` carries about 70 public methods, and
`process_presented` is one function of about 515 lines that processes an Agent's report. This is the
first of four slices of the module's split:
docs/plan/revision-2026-10-31/26-package-and-deployment-administration-leaves-appstate.md,
docs/plan/revision-2026-10-31/27-api-owns-the-wire-shape-of-the-views.md and
docs/plan/revision-2026-10-31/28-api-is-split-by-resource.md follow.

Move report processing out of `fleet.rs` into a module of its own, cut into named steps a unit test
can call, behind the same `AppState` entry point. Update the overview in the same change.

## Acceptance criteria

- [ ] Given a report, when it is processed, then each step is a function a unit test can call, and
  `process_presented` only sequences them.
- [ ] Given the existing `fleet.rs` tests and `crates/fleet-server/tests/`, when the change lands,
  then they pass unchanged.

## Read first

- ADR-0009, ADR-0024.
- `crates/fleet-server/src/fleet.rs` `process_presented`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
