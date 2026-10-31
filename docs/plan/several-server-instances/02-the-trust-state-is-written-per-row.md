# Ticket: The trust state is written per row

## What to build

Certificates, revocations, Hosts, the enrolment queue and the package floors stop being saved whole.
Each act is one transaction on the rows it names; an act that decides from what is stored reads it
under a row lock in the same transaction. Two operators revoking through two instances at once keep
both revocations; an instance with a stale view can no longer lower a floor or admit the renewal of
a revoked certificate. A unique constraint holds one `instance_uid` to one Host, and the register's
and the queue's bounds count the whole fleet. On the filesystem store nothing changes for the
operator.

## Acceptance criteria

- [ ] Given two instances, when each commits a different revocation at the same moment, then the
  list holds both
- [ ] Given a floor stored on the database and instance B holding an older one in memory, when B is
  asked to store a floor below the stored one, then it answers `409`
- [ ] Given a certificate revoked through A, when its successor is presented for renewal on B, then
  B follows it back to the revoked predecessor and refuses it
- [ ] Given two enrolments binding one `instance_uid` to two Hosts on two instances, when both are
  approved, then one is refused naming the binding
- [ ] Given the register at its bound across the fleet, when either instance would add to it, then
  the request is answered as the bound says
- [ ] Given the port contracts of the ledger, the enrolment and the floors, when they run on both
  stores, then they pass, the whole-or-nothing cases included
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clause
  19; ADR-draft-the-servers-state-and-its-stores clauses 2, 9, 10, 11 and 21
- ADR-draft-admission-by-client-certificate-and-revocation clauses 27, 28 and 30; ADR-0028 clause 15
- Golden path: the port contract tests of the ledger and the enrolment, which run one case set
  against both stores
- Seam: the port contracts, plus a two-instance case that holds a notification back

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It rewrites the stores that answer security decisions: revocation,
renewal, enrolment and the floors.
