# Ticket: The round of an Agent's report in its own module, and the module root as what remains

## What to build

The round of an Agent's report moves into `fleet::session`: `process`, `process_presented`,
`report_round`, `follow_move`, `settle`, `reply_of`, `finish`, `new_record`, `hold_from_store`,
`end_session`, `forget_agent`, `flush_agents` and the persist path, with `Waiting`, `Backoff` and
their unit tests. What is left in the module root is the state, its construction and limits, the
lease, and the types every context shares. The *Building blocks* of `docs/ARCHITECTURE.md` name the
submodules of `fleet` beside `fleet::held` and `fleet::placement`.

## Acceptance criteria

- [ ] Given the branch, when its diff against `main` is listed, then it touches only
      `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/`, this ticket's file, and
      `docs/ARCHITECTURE.md`.
- [ ] Given the unit tests of `fleet` on `main` and on the branch, when they are counted and
      listed by name, then the count and every name are the same.
- [ ] Given every public item of `fleet` on `main`, when the branch is built, then each is
      reachable at its old path, and no crate outside `fleet-server` and no module outside
      `fleet` needed a change to build.
- [ ] Given the branch, when the module root of `fleet` is read, then it holds no `impl AppState`
      method beyond construction, limits and the lease, apart from those the hand-offs of 02 to 05
      named as shared.
- [ ] Given `docs/ARCHITECTURE.md`, when its *Building blocks* are read with the tree open, then
      every submodule of `fleet` is named with what it holds.
- [ ] Given the branch, when `scripts/check-all.sh` runs, then it passes.

## Read first

- Touches: `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/session.rs`,
  `docs/ARCHITECTURE.md` (*Building blocks*, the Server's core)
- The golden path: `crates/fleet-server/src/fleet/placement.rs`, a context module behind the
  same `AppState` entry points.
- The spec next to this ticket, *Implementation decisions*.
- [`docs/CONVENTIONS.md`](../../CONVENTIONS.md): a comment that relies on a decision names
  its clause.

## Blocked by

- docs/plan/fleet-by-context/02-delivery.md
- docs/plan/fleet-by-context/03-rollout.md
- docs/plan/fleet-by-context/04-approval.md

## Mode

with the human: [ADR-0007](../../adr/0007-the-harness.md) clause 5 keeps every ticket with the
human. The stop: when a method or a test cannot be placed in one context without changing its
signature or its body beyond its visibility, it stays in the module root and the hand-off names
it.
