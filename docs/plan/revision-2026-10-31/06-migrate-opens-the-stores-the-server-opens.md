# Ticket: `server migrate` checks every store the Server opens, opened the way the Server opens it

## What to build

The list of the Server's stores is written three times: `Records` in
`crates/fleet-server/src/lib.rs`, `postgres/import.rs`, and `check_opens` in `migrate.rs`, which
says it opens them "as the Server would" but leaves out the floor store and the enrolment store, and
goes through the filesystem-only constructors `AppState::new` and `PackageStore::open`, whose only
other callers are tests.

Have `check_opens` open the stores through `Records` and the same constructors `main.rs` uses, so a
store added to the Server is checked by `migrate` without a second edit, and move the
filesystem-only constructors to the tests if nothing else needs them.

## Acceptance criteria

- [ ] Given a migrated `data_dir` whose floor or enrolment file is unreadable, when `server migrate`
  checks it, then it fails, as the Server would at startup.
- [ ] Given a new store added to `Records`, when `server migrate` runs, then it is opened without a
  change to `migrate.rs`.

## Read first

- ADR-0029 (header, Decision, Enforcement).
- `crates/fleet-server/src/lib.rs` `Records`; `crates/fleet-server/src/migrate.rs` `check_opens`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
