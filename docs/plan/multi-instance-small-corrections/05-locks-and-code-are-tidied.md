# Ticket: Locks are let go and the code is tidied

## What to build

Small corrections in the code of several instances, none of which changes an answer:

- the blob store's `LockHold` (`postgres/mod.rs`) clears its flag on drop and sends no
  `pg_advisory_unlock`, so `BLOB_LOCK` lingers until the session ends and delays the next
  holder, which can stretch a shared upload's 5-second wait; a prune round cut off by
  `PRUNE_TIMEOUT`, or whose unlock fails, leaves `PRUNE_LOCK` the same way
- `S3BlobStore::staged_by` (`s3.rs`), `gateway_list` and `gateway_list_of` (`transport.rs`) and
  `prune_round` (`postgres/audit_prune.rs`) are `pub` and reached only from tests or the crate
- `plan` in `postgres/audit_prune.rs` checks `budget == 0` twice
- the doc comment of `Deployments::delete` (`deployments.rs`) says whether anything names a
  Deployment is not the store's business, while the PostgreSQL backend counts the assignments in
  its transaction and refuses
- the forwarding closures `let sessions = |condition| test.sessions(condition)` in
  `postgres/agents.rs` and `postgres/deployments.rs`, and the regression test's advisory key
  `4_711_000_151` in `postgres/mod.rs`, which breaks the pattern of the other keys
- `write_presence` in `fleet.rs` extends what it wrote after its commit unconditionally, so a
  batch read before a reconnect costs one extra write

## Acceptance criteria

- [ ] Given a blob hold or a prune round that ends, when the next holder asks for the lock, then it
  is free without waiting for the session to end
- [ ] Given the crate, when it builds and its tests and lints run, then they pass with each item
  above corrected

## Read first

- ADR-0050 clauses 12 to 14
- The files named above; the store checks for PostgreSQL and S3

## Blocked by

- docs/plan/multi-instance-small-corrections/04-a-stale-cache-refuses-nothing-wrongly.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
