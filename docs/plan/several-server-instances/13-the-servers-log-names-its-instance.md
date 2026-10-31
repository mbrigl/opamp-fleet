# Ticket: The Server's log names its instance

## What to build

Every log line the Server writes on PostgreSQL carries its instance id, and the lease's take,
renewal failure, release and takeover are logged with the instance ids involved, so an operator
reading several instances' logs can tell who did what. On files nothing changes.

## Acceptance criteria

- [ ] Given an instance on PostgreSQL, when it logs, then each line carries its instance id
- [ ] Given an instance that takes over a lapsed lease, when it does, then it logs its own id and
  the lapsed one
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clauses
  1, 3 and 18
- Golden path: the Server's log setup at startup

## Blocked by

- docs/plan/several-server-instances/01-two-instances-share-one-database.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. The harness allows no unattended work.
