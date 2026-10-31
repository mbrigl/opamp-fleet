# Ticket: Two instances share one database, and a revocation reaches both

## What to build

The tracer bullet. Two Server instances start against one PostgreSQL database and one bucket, with
no CA key on the Server, each with an instance id and a lease. Both admit Agents. A revocation made
through the Operator plane of one ends the session it concerns on the other, through one `NOTIFY`.
An instance that cannot renew its lease closes both listeners and opens them again under a new
lease. An instance that must be alone, because its blobs or its CA keys are files, holds the
exclusive lock that takes the place of today's schema lock. The glossary gains Server instance and
lease.

## Acceptance criteria

- [ ] Given two instances on one database with S3 and no CA key, when both start, then both hold a
  live lease and both answer the TCP probe on the Agent plane
- [ ] Given a WebSocket session on instance B, when a revocation of its certificate is committed on
  instance A, then B ends that session within one lease renewal interval and no other
- [ ] Given a running instance, when its database stops answering past its lease, then both its
  listeners are closed, and when the database answers again it takes a new lease under a new
  instance id and opens them
- [ ] Given an instance with file blobs or a file CA key, when a second instance starts on the same
  database, then the second fails startup naming the key, and so does one that must be alone while
  another live lease exists
- [ ] Given `store = "fs"`, when a lease key is set in `server.toml`, then startup fails naming it
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clauses
  1, 2, 3 and 9 (the revocation kind only), 11
- ADR-draft-admission-by-client-certificate-and-revocation clause 32;
  ADR-draft-the-servers-state-and-its-stores clause 23
- ADR-0029 and ADR-0021, whose clauses the restatements replace
- Glossary: Agent, Instance UID; conventions on startup errors naming the key
- Golden path: the PostgreSQL adapter's schema lock and its two lock tests, which this ticket turns
  into the exclusive lock and the lease
- Seam: two instances in one test process through their real listeners, as the mutual-TLS and
  WebSocket transport tests start one

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes admission, revocation and the database's locking, which the
human reviews line by line.
