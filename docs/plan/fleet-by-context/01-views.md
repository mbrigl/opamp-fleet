# Ticket: The views of the fleet in their own module, and the shared test helpers (tracer bullet)

## What to build

The views and listings of the fleet move from the module root into `fleet::views`: `AgentQuery`,
`AgentListing`, `HostAgents`, `HostAgentView`, `AgentView`, the pending and package-status views and
`DeploymentReach`, with the `AppState` methods that build them (`host_agents`, `snapshot`,
`fleet_view`, `agent_view`, `list_agents`, `reported_values` and their private helpers) and their
unit tests. The test helpers that several contexts share (`insert`, `record_at`, `record_with`,
`report` and the like) move into a test-only submodule of `fleet` that the later tickets import.
This proves the pattern before the larger contexts follow.

## Acceptance criteria

- [ ] Given the branch, when its diff against `main` is listed, then it touches only
      `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/`, this ticket's file.
- [ ] Given the unit tests of `fleet` on `main` and on the branch, when they are counted and
      listed by name, then the count and every name are the same.
- [ ] Given every public item of `fleet` on `main`, when the branch is built, then each is
      reachable at its old path, and no crate outside `fleet-server` and no module outside
      `fleet` needed a change to build.
- [ ] Given the branch, when `fleet::views` is read, then it holds the views and their `impl
      AppState` block, and the module root holds none of them.
- [ ] Given the branch, when `scripts/check-all.sh` runs, then it passes.

## Read first

- Touches: `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/views.rs`, a test-only
  helper submodule under `crates/fleet-server/src/fleet/`
- The golden path: `crates/fleet-server/src/fleet/placement.rs`, a context module behind the
  same `AppState` entry points.
- The spec next to this ticket, *Implementation decisions*.
- [`docs/CONVENTIONS.md`](../../CONVENTIONS.md): a comment that relies on a decision names
  its clause.

## Blocked by

- None (can start immediately)

## Mode

with the human: [ADR-0007](../../adr/0007-the-harness.md) clause 5 keeps every ticket with the
human. The stop: when a method or a test cannot be placed in one context without changing its
signature or its body beyond its visibility, it stays in the module root and the hand-off names
it.
