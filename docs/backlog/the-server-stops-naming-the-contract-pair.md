# The Server stops naming the contract pair in its SQL

**Kind:** schema, release; decision deferred.

**Decided.** The human deferred release X until after the next normalization (2026-11-15). The
columns stay until then. How ADR-0023 clause 22 reads on `SET DEFAULT`, and that removal moves to
release X+1, is settled then by an ADR against the restated record. Until then no release drops the
columns: a 0.8.2 instance beside it would fail, as below.

**What was ticketed.**

The contract pair is the Deployments' mirror columns `packages` and `configurations` and the
Hosts' `instance_uids` arrays. The first step shipped in 0.8.1 and 0.8.2: no release writes the
mirror columns any more (the test `the_mirror_columns_are_written_no_more` in
`crates/fleet-server/src/postgres/deployments.rs`), and no release adds to the arrays.

`main` and 0.8.2 still name the columns in the SQL they run while serving:

- `INSERT INTO deployments (name, selector, packages, configurations)` in
  `postgres/deployments.rs` (lines 61 and 436). The columns are `NOT NULL` without a default
  (`postgres/schema.rs`, near lines 333 to 338).
- `hosts.instance_uids` is `NOT NULL` without a default (`postgres/schema.rs`, line 384). The
  `INSERT`s into `hosts` name it (`postgres/ledger.rs`, lines 512, 655 and 1057), and the upsert
  filters it by the bindings (near line 1060). An `UPDATE` takes a released Agent out with
  `array_remove` (near line 636), and the mark of a Gateway resets it with
  `UPDATE hosts SET instance_uids = '{}'` (line 532). The load `SELECT`s `instance_uids` and
  discards it (near line 785).

Test SQL names them too: `postgres/deployments.rs` line 741, `postgres/mod.rs` lines 1515 and
2545, `postgres/agents.rs` line 3620, `postgres/ledger.rs` line 1543 and near lines 2729 to 2784,
`fleet.rs` near line 9694, and `tests/instances.rs` line 1384.

A Server of 0.8.1 or 0.8.2 decodes `instance_uids` with `row.get::<_, Vec<String>>(2)`
(`postgres/ledger.rs`, near line 770 in both tags), which panics on `NULL`. A migration that drops
the columns, or lets them be `NULL`, while such a Server runs beside it would break that Server.

This ticket is therefore one release, X, which the human cuts:

- a migration gives each column of the pair a default: `'{}'` for `instance_uids` and
  `configurations`, `'[]'` for `packages`;
- the code stops naming the columns in the SQL it runs while serving;
- the migration names 0.8.1 as the oldest Server.

The oldest Server is 0.8.1, not 0.8.0. A Server of 0.8.0 still reads an array for an
`instance_uid` that has no binding, and X no longer takes a released `instance_uid` out of the
arrays. Every migration on `main` names 0.8.0 or older, so X's is the first to name 0.8.1. An
instance of X therefore refuses to start beside a live lease of 0.8.0.

The human decides these questions first, before the next normalization of the ADR record:

- whether `SET DEFAULT` on an existing column counts as a migration that "only adds" under ADR-0023
  clause 22; the alternative `DROP NOT NULL` does not, and it breaks 0.8.1 and 0.8.2 as said
  above;
- whether the restated clause 22 keeps concrete release steps;
- whether the ticket may depart from clause 22's sequence, in which the release after 0.8.1
  removes the mirror columns. The ticket defers the removal to X+1. ADR-0045 clause 16 allows that,
  since what N+1 stops using is removed by N+2 "at the earliest".

The first cheap step needs no decision: the load stops selecting `instance_uids`.

The release after X drops the columns; the backlog entry
`docs/backlog/the-contract-pair-is-dropped.md` holds it.

**Effort.** S.

**Its acceptance criteria.**

- [ ] Given a database migrated by release X, when the schema is read, then each column of the
  contract pair has its default, and the migration names 0.8.1 as the oldest Server.
- [ ] Given the code of release X, when the statements the Server runs while serving are searched
  for the columns of the pair, then none names them. The historical migrations
  (`postgres/schema.rs`, lines 471, 484 and 532) and X's own migration still name them.
- [ ] Given a Server of 0.8.2 beside an instance of release X, when each writes a Deployment and a
  Host, then both succeed.
- [ ] Given a Server of 0.8.2 beside an instance of release X, when the 0.8.2 Server loads the
  ledger and reads the Deployments that release X wrote, then it reads them without an error.
- [ ] Given a live lease of 0.8.0, when an instance of release X starts, then it refuses to start.
- [ ] Given the store checks, when they run, then they pass.

**Its reading.**

- Touches: `crates/fleet-server/src/postgres/schema.rs`,
  `crates/fleet-server/src/postgres/deployments.rs`, `crates/fleet-server/src/postgres/ledger.rs`,
  `crates/fleet-server/src/postgres/mod.rs`, `crates/fleet-server/src/postgres/agents.rs`,
  `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/tests/instances.rs`
- ADR-0023 clause 22; ADR-0045 clause 16.

**Its mode.**

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. Its migration ships only in a release the human cuts, and the reading
of ADR-0023 clause 22 is the human's.

**From.** Ticket
`docs/plan/the-contract-pair-is-removed/01-the-server-stops-naming-the-contract-pair.md`, moved to
the backlog on the human's decision of 2026-11-15.
