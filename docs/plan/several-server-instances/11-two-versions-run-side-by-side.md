# Ticket: Two versions run side by side

## What to build

A rolling update replaces instances one by one. Every schema change is expand-then-contract over two
releases; migrations run under an advisory lock, so two new instances starting together migrate
once. An instance refuses a schema older than its version expects or newer than the next one, naming
both versions.

Ticket 02 left this ticket one such pair: migration 4 moved the bindings of `instance_uid`s into
`host_bindings`, and the release before it binds through the Hosts' arrays only. The new release
honours the arrays when it binds, lists each binding in its Host's array in the same transaction,
and takes a released `instance_uid` out of them. A Host's first binding makes its row in the same
transaction, so the binding and its array are committed together. What is left: an instance of
the release before reads only the arrays, and binds through them alone, so a binding it makes is
seen by the new release only as an array entry, and one the new release makes is seen by it once
committed. Migration 4 still names 0.6.1 as the oldest release it serves; decide whether it should
name this release until the contract step drops the arrays.

## Acceptance criteria

- [ ] Given version N on the database, when two instances of N+1 start at once, then the migration
  runs once and both serve
- [ ] Given instances of N and N+1 on one database, when Agents report to either, then both serve
  them
- [ ] Given a schema newer than the next version, when an instance starts, then it fails naming both
  versions; likewise for a schema older than it expects
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clause
  16; ADR-draft-the-servers-state-and-its-stores clause 22
- Golden path: `the_server_applies_its_schema_and_refuses_a_newer_one` and the migration numbering
  test
- Seam: the schema module against a real database

## Blocked by

- docs/plan/several-server-instances/01-two-instances-share-one-database.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes how the schema is migrated.
