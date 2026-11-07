# Ticket: The fleet's PostgreSQL tests run in CI

## What to build

`scripts/check-store.sh postgres` runs the library tests under `postgres::` and the `instances`
and `stateless_instance` binaries; the `Checks` workflow sets the `FLEET_TEST_POSTGRES_*`
variables only in its postgres-store job. The PostgreSQL tests in `fleet.rs`'s `tests` module
fall outside that filter, and in the build-and-test job they return early through
`TestDatabase::named()` and pass without running. Eleven tests guard the take-up, the re-key, the
stored read of an Agent behind a Gateway and the collecting of what a forget leaves, among them
`a_take_up_never_undoes_a_newer_act`, `a_rekey_whose_move_fails` and
`a_take_up_reads_again_what_differs_in_one_read`. The store check runs every test that needs the
database, wherever it lives, and a test that returns early for want of one says so where the job
that should run it can see it.

Beside it, `tests/stateless_instance.rs` creates a role and a schema owned by it, so against a
developer database whose user lacks `CREATEROLE` it fails rather than skips; it skips, naming the
missing right, or the manual's developer setup names the right.

## Acceptance criteria

- [ ] Given the postgres-store job, when it runs, then every test of `fleet.rs` that opens a
  `TestDatabase` runs against the database, and its log shows them
- [ ] Given one of them broken on purpose, when the store check runs, then it fails
- [ ] Given a developer database whose user lacks `CREATEROLE`, when the store check runs
  `stateless_instance`, then the run says which right is missing

## Read first

- ADR-0002 on tests that cite decisions; the README's *Build, Test & Run* and the store checks
- `scripts/check-store.sh`; the postgres-store job in `.github/workflows/checks.yml`;
  `TestDatabase` in `postgres/mod.rs`

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
