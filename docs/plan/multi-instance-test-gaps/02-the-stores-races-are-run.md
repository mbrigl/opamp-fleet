# Ticket: The stores' races are run

## What to build

Several bounds of the PostgreSQL stores are reasoned, not raced:

- two `create_within` at once against `max_agents` (`postgres/agents.rs`; the tests are
  sequential)
- a release against a presence batch; only release against release and batch against batch exist
- the `Pins` guard of `postgres/agents.rs` under real threads; the tests stage the overlap on one
  thread through `began()`
- `server audit-verify` racing a chain's close against its `REPEATABLE READ` snapshot
- two sweeps after two acts at once:
  `the_next_sweep_on_any_instance_deletes_a_released_leases_staging_object_once` in `s3.rs`
  drives two cleanups, not two sweeps, so its name says more than it tests

Each gets a test that runs the two sides on two connections at once, as the release-order tests do.

## Acceptance criteria

- [ ] Given two creations of new Agents at once with one place left, when both commit, then one is
  refused
- [ ] Given a release and a presence batch on the same rows at once, when both run, then neither
  deadlocks and both writes stay
- [ ] Given two threads reading the pins while a commit lands, when both answer, then neither
  holds an older count than the commit
- [ ] Given a close committed during `audit-verify`, when it answers, then it reports the chain as
  it was in its snapshot
- [ ] Given two acts on two instances that each sweep, when both sweeps run, then each staging
  object is deleted once and the test's name says what it tests

## Read first

- ADR-0045 clause 19; ADR-0023 clauses 13 and 14; ADR-0029 clause 5
- Golden path: the release-order tests in `postgres/agents.rs`, which wait on
  `pg_stat_activity`

## Blocked by

- docs/plan/multi-instance-test-gaps/01-cross-instance-paths-take-their-routes.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
