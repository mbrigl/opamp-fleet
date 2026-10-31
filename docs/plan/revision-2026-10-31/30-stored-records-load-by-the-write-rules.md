# Ticket: A stored record is loaded by the rules it was written by

## What to build

Both Server stores load a record through the core's constructors, but three rules a write enforces
are not checked on load, and on one point the two stores differ:

- a Package reference with an unknown field loads from the filesystem and is refused by
  PostgreSQL;
- a stored Deployment with two Packages of one Agent type loads on both stores, the last one kept,
  though a write refuses it;
- no store checks an assignment's Deployment name, its Configuration names or its revision hashes
  on load.

The human decided:

- a Package reference with an unknown field is refused by both stores;
- a Deployment with two Packages of one Agent type is refused on load;
- an assignment's Deployment name, Configuration names and the form of its revision hashes are
  checked on load; whether a revision still exists is not.

The load is made as strict as the write for each. Where it is, a
record an existing data directory or database holds and the stricter load would refuse is carried
by `server migrate` or reported by it, so a Server that loaded before does not fail its next start
without being told why (ADR-0029 clause 26).

## Acceptance criteria

- [ ] Given a Package reference with an unknown field, when the filesystem and the PostgreSQL store
  load it, then both decide it alike.
- [ ] Given each rule made strict on load, when a record breaking it is planted in each store, then
  the `contract` test of that store refuses it with the core's message, naming the record.
- [ ] Given an existing data directory or database holding such a record, when `server migrate` or
  the Server's start meets it, then the operator is told which record and why, before anything is
  refused.

## Read first

- ADR-0009, ADR-0029 (header, Decision, Enforcement, clause 26).
- `crates/fleet-server/src/packages.rs` `Entry::load`, `deployments.rs` `Deployment::load`,
  `fleet.rs` `Assignment::load`; the `fs/` and `postgres/` adapters that call them; their
  `contract` tests.

## Blocked by

- docs/plan/revision-2026-10-31/05-both-store-adapters-load-through-the-core.md

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, and which rule is made strict on
load is the human's decision, since it can refuse records an existing Server holds.
