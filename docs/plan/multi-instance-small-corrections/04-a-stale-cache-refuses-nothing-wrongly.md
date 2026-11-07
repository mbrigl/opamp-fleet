# Ticket: A stale cache refuses nothing wrongly

## What to build

Two caches of one instance can be stale about Agents another instance holds:

- the deletions of a Configuration, a Deployment and a Package set check the `Pins` cache first
  (`delete_configuration`, `delete_deployment`, `delete_package_set`, `refuse_if_assigned` in
  `fleet.rs`). On an instance that does not hold an Agent, its pins change only on a read of the
  whole fleet, since `take_up_agents` reads held uids only; after another instance re-released or
  forgot such an Agent the deletion is refused `409` with a wrong count until the next whole read.
  It refuses in the safe direction.
- a forget of an Agent this instance does not hold leaves a revision uncollected when another
  instance releases between the `get` and `remove_if_silent`, or when the record's read fails;
  the revision stays until the next `retain_only` of its Configurations. It is garbage, never a
  pin lost.

A refusal from the cache is checked against the database before it is answered, as the
Deployment's deletion already counts in its transaction, and the forget collects what the stored
record named.

## Acceptance criteria

- [ ] Given an Agent re-released through A that B does not hold, when its old Configuration is
  deleted through B, then B decides from the database and does not refuse with a stale count
- [ ] Given a forget through B racing a release on A, when both commit, then no revision of the
  forgotten Agent's assignment is left uncollected
- [ ] Given the deletion tests in `tests/instances.rs`, when they run, then they pass unchanged

## Read first

- ADR-0026 on what a rollout pins; ADR-0041 clauses 12 and 19
- `fleet.rs` the deletions and `forget_agent`; `postgres/agents.rs` `Pins`;
  `postgres/deployments.rs` `remove`, the golden path

## Blocked by

- docs/plan/multi-instance-small-corrections/03-the-manual-states-the-remaining-bounds.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
