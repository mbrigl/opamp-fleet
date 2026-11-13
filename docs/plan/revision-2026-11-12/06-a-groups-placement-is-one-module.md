# Ticket: How a group is placed is one module

## What to build

1a186b14e wrote the placement rule of a group once, in `listing`. What uses it is still spread
over three places:

- the rule: `crates/fleet-server/src/listing.rs`;
- the `groups` index and the placing in memory: `crates/fleet-server/src/fleet.rs`, with two read
  modes for a Client Agent another instance carries (`Elsewhere::Now` and `Elsewhere::Read`);
- the placing again after a notice: `place_again_for` in `crates/fleet-server/src/fleet/held.rs`.

A defect of the last count (06efe2866) needed changes in three of them. Give placement a submodule
of `fleet` that owns the `groups` index, `Elsewhere` and the placing again, behind the same
`AppState` entry points, and name it in `docs/ARCHITECTURE.md`. Moving the `Now` read out from
under the lock is `docs/plan/fleet-at-scale-costs/03-an-act-on-one-agent-reads-one-agent.md`, not
this ticket.

## Acceptance criteria

- [ ] Given `fleet.rs` and `fleet/held.rs`, when the change lands, then the placing of groups,
  their index and the placing again live in one module, and `dependency_direction.rs` gives it its
  role.
- [ ] Given the existing tests and the store checks, when the change lands, then they pass
  unchanged.

## Read first

- Touches: `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/held.rs`,
  `crates/fleet-server/src/listing.rs`, `crates/fleet-core/tests/dependency_direction.rs`,
  `docs/ARCHITECTURE.md`
- ADR-0030 clauses 18 to 21, ADR-0009; ADR-0045 clause 8.
- Tickets 03 and 04 of this directory touch the same modules; work them one after the other.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
