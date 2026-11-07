# Ticket: The Hosts' arrays are removed

## What to build

Once ticket 01's release writes the Hosts' `instance_uids` arrays no more, the release after it
removes them by a migration, together with the code and the comments that keep them. That is the
third release after 0.8.0. It cannot come earlier, because the release after 0.8.0 still takes
`instance_uid`s out of the arrays while it runs beside ticket 01's (the stores draft's clause 22).

## Acceptance criteria

- [ ] Given a database migrated by this release, when the schema is read, then `hosts` has no
  `instance_uids` column, and the migration names ticket 01's release as the oldest Server
- [ ] Given the store checks, when they run, then they pass

## Read first

- The stores draft,
  ADR-draft-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision
  clause 22
- `postgres/schema.rs`, `postgres/ledger.rs`

## Blocked by

- docs/plan/the-contract-pair-is-removed/01-the-contract-pair-is-removed.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It is released only in the release after ticket 01's, which the human
cuts.
