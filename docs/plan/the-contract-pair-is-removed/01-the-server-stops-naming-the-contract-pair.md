# Ticket: The Server stops naming the contract pair in its SQL

## What to build

The contract pair is the Deployments' mirror columns `packages` and `configurations` and the
Hosts' `instance_uids` arrays. The first step shipped in 0.8.1 and 0.8.2: no release writes the
mirror columns any more (the test `the_mirror_columns_are_written_no_more` in
`crates/fleet-server/src/postgres/deployments.rs`).

`main` and 0.8.2 still name the columns in their SQL:

- `INSERT INTO deployments (name, selector, packages, configurations)` in
  `postgres/deployments.rs` (lines 61 and 436), and in a test of `fleet.rs` (near line 9694). The
  columns are `NOT NULL` without a default (`postgres/schema.rs`, near lines 333 to 338).
- The `INSERT`s into `hosts` name `instance_uids` (`postgres/ledger.rs`, lines 512, 655 and 1057).
  An `UPDATE` takes a released Agent out with `array_remove` (near line 636). The load `SELECT`s
  `instance_uids` and discards it (near line 785).

A migration that drops the columns while a Server of 0.8.2 runs beside it would fail that Server's
SQL. This ticket is therefore one release, X, which the human cuts:

- a migration gives the columns defaults, or drops their `NOT NULL`;
- the code stops naming the columns;
- the migration names 0.8.1 as the oldest Server.

The human decides two questions first, before the next normalization of the ADR record:

- whether `SET DEFAULT` counts as a migration that "only adds" under ADR-0023 clause 22;
- whether the restated clause 22 keeps concrete release steps.

The first cheap step needs neither decision: the load stops selecting `instance_uids`.

The release after X drops the columns; the backlog entry
`docs/backlog/the-contract-pair-is-dropped.md` holds it.

**Effort.** S, and gated by a release.

## Acceptance criteria

- [ ] Given a database migrated by release X, when the schema is read, then each column of the
  contract pair has a default or allows `NULL`, and the migration names 0.8.1 as the oldest Server.
- [ ] Given the code of release X, when it is searched for `packages, configurations` and
  `instance_uids` in SQL, then no statement names them.
- [ ] Given a Server of 0.8.2 beside an instance of release X, when each writes a Deployment and a
  Host, then both succeed.
- [ ] Given the store checks, when they run, then they pass.

## Read first

- Touches: `crates/fleet-server/src/postgres/schema.rs`,
  `crates/fleet-server/src/postgres/deployments.rs`, `crates/fleet-server/src/postgres/ledger.rs`,
  `crates/fleet-server/src/fleet.rs`
- ADR-0023 clause 22; ADR-0045 clause 16.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. Its migration ships only in a release the human cuts, and the reading
of ADR-0023 clause 22 is the human's.
