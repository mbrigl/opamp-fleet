# Ticket: The Server's messages name the cause

## What to build

Five messages of the Server on PostgreSQL and S3 send the operator the wrong way:

- the refusals of a shared start beside a Server that holds the database alone
  (`HELD_ALONE_BEFORE_MIGRATING` in `postgres/mod.rs`, the refusal of `take_lease` in
  `postgres/lease.rs`) name 0.7.1, file blobs and a CA key file, not a running
  `server migrate-to-postgres` or `server blobs-to-s3`, which hold the lock alone too
- `migrate-to-postgres` closes, with S3, by telling the operator to remove `data_dir` from
  `server.toml` (`main.rs`), which the configuration's check already refuses beside PostgreSQL
  and S3
- a bucket check that times out inside an entry's transaction reports "the database or the network
  to it does not answer"
- migration 5 aborts with a raw primary-key or `NOT NULL` violation on a Deployment whose
  `packages` hold two entries of one `agent_type`, or one without `agent_type` or `version`,
  instead of naming the Deployment; it rolls back, so nothing is lost
- `blobs-to-s3`'s final check compares names, not sizes (`upload` in `postgres/import.rs`), so
  an object of the wrong length under `blobs/<sha256>` passes it, and the next start fails naming
  the size

Each names what it found and what the operator does about it.

## Acceptance criteria

- [ ] Given a `blobs-to-s3` running, when an instance starts, then its refusal names the command
  as a possible holder
- [ ] Given `migrate-to-postgres` with S3, when it ends, then its last lines name no step the
  configuration already refuses
- [ ] Given a bucket that does not answer during an entry's write, when the bound passes, then the
  error names the bucket
- [ ] Given a Deployment whose `packages` migration 5 cannot split, when it runs, then it fails
  naming the Deployment and the reason
- [ ] Given an object of the wrong size under an artifact's key, when `blobs-to-s3` checks, then
  it fails naming the artifact

## Read first

- ADR-0050 clauses 1, 13 and 22; the manual's pages on moving to PostgreSQL and S3
- `postgres/mod.rs`, `postgres/lease.rs`, `postgres/import.rs`, `postgres/schema.rs`,
  `main.rs`

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
