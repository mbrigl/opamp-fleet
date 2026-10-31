# Ticket: Two versions run side by side

## What to build

A rolling update replaces instances one by one. Every schema change is expand-then-contract over two
releases; migrations run under an advisory lock, so two new instances starting together migrate
once. An instance refuses a schema older than its version expects or newer than the next one, naming
both versions.

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
