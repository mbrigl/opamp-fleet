# Ticket: The rule that places a group under its Client Agent is written once

## What to build

ADR-0043 clause 19 places the Agents of a group under the group's one Client Agent, marks every
member ambiguous where there are several, and lets the others inherit its labels. The rule is
written twice: `inherit` in `crates/fleet-server/src/fleet.rs` places the Agents an instance
holds, in memory, and `place_on_host` in `crates/fleet-server/src/listing.rs` places the Agents a
page reads from PostgreSQL (`postgres/agent_page.rs`). A change to the rule made in one of them
leaves the fleet view and the Selector's match disagreeing on PostgreSQL.

One function decides the placement of a group from its members, and both callers use it.

## Acceptance criteria

- [ ] Given a group with one, several or no Client Agent, when it is placed in memory and when a
  page of it is read from PostgreSQL, then both answers come from one function of `listing`.
- [ ] Given the existing placement tests of `fleet.rs` and `listing.rs` and the store checks,
  when the change lands, then they pass unchanged.

## Read first

- ADR-0043 clause 19;
  ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server
  clause 8 (the core owns the derivation).
- `fleet.rs` `inherit`, `inherit_held`; `listing.rs` `place_on_host`; `postgres/agent_page.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
