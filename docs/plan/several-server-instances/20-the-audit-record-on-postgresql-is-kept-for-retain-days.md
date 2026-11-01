# Ticket: The audit record on PostgreSQL is kept for `retain_days`

## What to build

On PostgreSQL the audit record is one hash chain per lease, and nothing removes old entries: the
table grows without bound, and the filesystem store's `[audit]` keys are silently ignored there.
Entries older than `[audit] retain_days` (default 365, at least 1) are removed, one instance at a
time, from every chain, those closed at a shutdown and those closed for a lapsed lease included;
the instance records `audit.pruned` in its own chain, naming each chain it pruned and the last hash
it removed from it, so that `audit-verify` still links what is left. A closed chain with no entries
left keeps its record. `retain_days` with the filesystem store, or `keep_files` or
`max_file_bytes` with PostgreSQL, fails startup naming the key.

## Acceptance criteria

- [ ] Given entries older than `retain_days`, when the prune runs, then they are removed,
  `audit.pruned` records the last removed hash, and `audit-verify` still passes
- [ ] Given two instances and old entries in a chain closed at a shutdown and in one closed for a
  lapsed lease, when both prune, then one at a time removes them, its `audit.pruned` entries name
  both chains, and a closed chain with no entries left keeps its record
- [ ] Given `retain_days` with `store = "fs"`, or `keep_files` or `max_file_bytes` with
  `store = "postgres"`, when the Server starts, then it fails naming the key
- [ ] Given a pruned chain whose first retained entry's `prev` is not the hash its `audit.pruned`
  recorded, or a closed chain with no entries left whose closing hash differs from it, when
  `audit-verify` runs, then it fails naming the chain
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance clauses 4 and 5 and
  its verification; ADR-draft-the-servers-state-and-its-stores clause 12
- Golden path: ticket 08's chain per lease and `audit-verify --config`
- Seam: the audit store on PostgreSQL, plus two instances in one test process

## Blocked by

- docs/plan/several-server-instances/08-the-audit-record-is-one-chain-per-lease.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It removes audit entries and changes which configurations start.
