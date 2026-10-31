# Ticket: The Server's report processing and its REST views each have a module of their own

## What to build

`AppState` in `crates/fleet-server/src/fleet.rs` carries about 70 public methods: report processing,
enrolment approval, package and Deployment administration, the upload quota and the REST views.
`process_presented` is one function of about 515 lines. The view types (`AgentView` and three more)
derive `Serialize` in the core, while `api.rs` mirrors each one with a response type and a
field-by-field `From`, the doc comments copied; the core's `Serialize` looks unused. `api.rs` is
3700 lines for about ten resources and has no unit tests.

Move report processing into a module of named steps, and the package and Deployment administration
beside it; let `api` own the wire shape alone, dropping the core's serialization of the views; split
`api` by resource. Update the overview in the same change.

## Acceptance criteria

- [ ] Given the REST API's OpenAPI document, when the change lands, then it is byte-identical.
- [ ] Given a report, when it is processed, then each step is a function a unit test can call.
- [ ] Given the core view types, when the crate is built, then none derives `Serialize` unless a
  caller needs it.

## Read first

- ADR-0009, ADR-0024, ADR-0025.
- `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/api.rs`,
  `crates/fleet-server/tests/rest_api.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. The cut is wide; the human chooses the order of the slices
([`AGENTS.md` §4](../../../AGENTS.md#4-working-style)).
