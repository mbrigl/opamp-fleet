# Ticket: Every change reaches every instance

## What to build

Every change that concerns a live session is one `NOTIFY` on one channel in the transaction that
commits it: an assignment, a Package or Deployment published, a restart, a revocation added or
lifted, an enrolment decided, the window opened or closed, labels, an Agent forgotten or taken, a
floor set or removed. Each instance acts on the sessions it holds. A lost listening connection, or a
kind it does not know, counts as every notification: it drops its caches, re-reads the revocation
list, the assignments, the holders, the labels, the enrolment state and the floors. The revocation
list has one version, so a Gateway's `ETag` answered by one instance is answered `304` by any.

## Acceptance criteria

- [ ] Given a rollout committed on A, when an Agent is carried by B, then B offers it the new
  configuration within one second
- [ ] Given an enrolment request waiting on B, when it is approved on A, then B hands the
  certificate to the connection that asked
- [ ] Given an instance whose listening connection is cut and restored, when a revocation was
  committed in between, then the instance ends the session it concerns once it is back
- [ ] Given a notification of a kind the instance does not know, when it arrives, then the instance
  re-reads as after a lost connection
- [ ] Given a Gateway holding the revocation list's `ETag` from A, when it asks B, then B answers
  `304`
- [ ] Given a floor set on A, when an Agent asks B for the floors, then B serves the new floor
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clauses
  9, 10, 11 and 12; ADR-draft-the-servers-state-and-its-stores clause 10;
  ADR-draft-admission-by-client-certificate-and-revocation clauses 32 and 34
- ADR-0026 for what a rollout offers, ADR-0022 for enrolment
- Golden path: the in-process push channel and the revocation list's subscription, which this ticket
  feeds from the database
- Seam: two instances in one test process, the listening connection cut by the test

## Blocked by

- docs/plan/several-server-instances/01-two-instances-share-one-database.md
- docs/plan/several-server-instances/02-the-trust-state-is-written-per-row.md
- docs/plan/several-server-instances/03-revisions-entries-and-members-are-written-per-row.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It decides when a revoked session ends and when an approved
certificate is handed out.
