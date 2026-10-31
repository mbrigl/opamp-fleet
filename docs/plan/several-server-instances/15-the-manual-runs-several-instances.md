# Ticket: The manual runs several instances

## What to build

The operator manual says how to run several instances: the balancer with TLS passthrough that keeps
the peer address, the probes, `drain_secs` and the grace period of an orchestrator, a database with
synchronous replication, the token and the network HSM, the rolling update, and the order of moving
one Server on files to several instances. It says that backing up and restoring the database and the
bucket is the operator's, and that after a restore the operator sets again the revocations the audit
record holds since the backup.

## Acceptance criteria

- [ ] Given the manual, when an operator follows it from one Server on files, then they reach three
  instances on PostgreSQL, S3 and a token in the order it gives
- [ ] Given the manual check, when it runs, then it passes

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
  Follow-ups; ADR-draft-the-servers-state-and-its-stores Follow-ups
- The manual's conventions: no ADR cited, every configuration key in its table
- Golden path: the manual's Server pages on the stores and the container

## Blocked by

- docs/plan/several-server-instances/09-an-instance-on-postgresql-and-s3-needs-no-disk.md
- docs/plan/several-server-instances/11-two-versions-run-side-by-side.md
- docs/plan/several-server-instances/12-the-ca-keys-live-on-a-pkcs11-token.md
- docs/plan/several-server-instances/14-a-workflow-decides-g-20.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. The five conditions of
[`AGENTS.md` §4](../../../AGENTS.md#4-working-style) would otherwise hold for a page of the manual,
with the stop at the manual check failing twice.
