# Ticket: Package errors keep their status on PostgreSQL

## What to build

`Database::run_within` in `postgres/mod.rs` prefixes every error with the database's name, and
`package_error` in `api.rs` maps an error to its status by how its message begins. On
PostgreSQL the prefixes no longer match: `REMOVED_WHILE_UPLOADED` from `put_entry` answers `500`
instead of `400`, "no package set" answers `500` instead of `404`; the other way round, "the
deletion ended: …" after a panicked deletion answers `400`. The status comes from the kind of the
error, not from its text.

## Acceptance criteria

- [ ] Given PostgreSQL, when an entry is written to a Package set removed meanwhile, then it is
  answered `400`, and an unknown set `404`, as with the filesystem store
- [ ] Given a deletion whose task panicked, when it answers, then it is answered `500`
- [ ] Given both stores, when the Package REST tests run, then they answer the same statuses

## Read first

- ADR-0027 on the package routes; ADR-0050 clause 13
- `api.rs` `package_error`; `postgres/mod.rs` `run_within`; `postgres/packages.rs`
  `put_entry`

## Blocked by

- docs/plan/multi-instance-small-corrections/01-the-servers-messages-name-the-cause.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
