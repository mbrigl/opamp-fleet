# Ticket: The audit record is one chain per lease

## What to build

On PostgreSQL each lease opens a chain of its own, written by one writer thread in the order of the
decisions. An orderly shutdown closes it; the instance that takes over a lapsed lease closes that
lease's chain. A take removes lapsed leases too (`take_lease` in `postgres/lease.rs`, naming
each in `Taken::taken_over`), the instance's own lapsed one included, and their chains are closed
there as well. Entries queued under a lease that lapses go into the next chain, marked with the
lease they were taken under. `server audit-verify --config` walks every chain and lists the open
ones. A record moved from files by `server migrate-to-postgres` stays linked: the first chain names
the archive's last entry as its predecessor, and `audit-verify --archive` checks the link. On the
filesystem store the record stays one chain in rotating files.

## Acceptance criteria

- [ ] Given two instances, when each records decisions, then each writes its own chain and
  `audit-verify` passes over both
- [ ] Given an instance killed without a shutdown, when another takes over its lapsed lease, then
  the dead chain ends with `audit.chain_closed` and reason `lease_lapsed`
- [ ] Given entries queued when a lease lapses, when the instance takes a new lease, then they land
  in the new chain marked with the old lease
- [ ] Given a file record moved to PostgreSQL, when `audit-verify --archive <dir>` runs, then the
  link from the first chain to the archive's last entry verifies
- [ ] Given an entry removed from the middle of a chain, when `audit-verify` runs, then it names the
  chain and the entry and exits non-zero
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance clauses 1 to 6;
  ADR-draft-an-append-only-audit-record clauses 1 to 6;
  ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clause
  18; ADR-draft-the-servers-state-and-its-stores clauses 12 and 27
- ADR-0023, which the restatement replaces
- Golden path: the audit writer's chain, rotation and `audit-verify` tests
- Seam: the audit port on both stores, and `audit-verify` as a command

## Blocked by

- docs/plan/several-server-instances/01-two-instances-share-one-database.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes the record every security decision is written to.
