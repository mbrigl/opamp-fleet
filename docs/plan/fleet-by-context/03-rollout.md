# Ticket: Configurations, Deployments and rollouts in their own module

## What to build

Configurations, Deployments and rollouts move into `fleet::rollout`: the catalogue, saving and
deleting a Configuration, the Deployment's Configurations, `release`, `rollout_to_agent`,
`rollout_deployment`, the restart command, the labels, `deployments_holding`, `deployment_reach`,
`delete_deployment` and the settings offer, with their unit tests.

## Acceptance criteria

- [ ] Given the branch, when its diff against `main` is listed, then it touches only
      `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/`, this ticket's file.
- [ ] Given the unit tests of `fleet` on `main` and on the branch, when they are counted and
      listed by name, then the count and every name are the same.
- [ ] Given every public item of `fleet` on `main`, when the branch is built, then each is
      reachable at its old path, and no crate outside `fleet-server` and no module outside
      `fleet` needed a change to build.
- [ ] Given the branch, when `fleet::rollout` is read, then it holds the rollout methods of
      `AppState`, and the module root holds none of them.
- [ ] Given the branch, when `scripts/check-all.sh` runs, then it passes.

## Read first

- Touches: `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/rollout.rs`
- The golden path: `crates/fleet-server/src/fleet/placement.rs`, a context module behind the
  same `AppState` entry points.
- The spec next to this ticket, *Implementation decisions*.
- [`docs/CONVENTIONS.md`](../../CONVENTIONS.md): a comment that relies on a decision names
  its clause.

## Blocked by

- docs/plan/fleet-by-context/01-views.md

## Mode

with the human: [ADR-0007](../../adr/0007-the-harness.md) clause 5 keeps every ticket with the
human. The stop: when a method or a test cannot be placed in one context without changing its
signature or its body beyond its visibility, it stays in the module root and the hand-off names
it.
