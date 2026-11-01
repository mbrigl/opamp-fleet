# Ticket: What an assignment pins is decided in the transaction

## What to build

Two checks still decide from the instance's memory what another instance may have changed: the
refusal to delete a Configuration or change a Package that an assignment pins, and the sweep of
package blobs no entry names. On several instances, one may delete a Configuration that another
just pinned, and the sweep on one may delete a blob that a new entry written through another names.
Both checks move into the transaction that acts: the pins are read from the stored assignments
under the row lock of the Configuration or the Package, and a blob is swept only when no stored
entry names it, read in the same transaction that removes it.
Both the sweep and the cleanup of staged uploads take the advisory lock the stores draft's clause
14 names, so that two instances never delete the same keys at once. A staged upload whose instance
released its lease without deleting it, or whose takeover cleanup failed, is not left until some
instance starts: the sweep removes the staged uploads of every lease that is not live.

## Acceptance criteria

- [ ] Given a revision pinned through instance A, when instance B is asked to delete its
  Configuration, then B refuses it, though its own memory holds no such pin
- [ ] Given an entry written through A that names a new blob, when B sweeps its blobs before it has
  read that entry, then the blob is kept
- [ ] Given a staged upload of a lease released at an orderly shutdown, when the next sweep runs on
  any instance, then the upload is removed, and two instances sweeping at once delete it once
- [ ] Given a test that removes the read of the stored pins or entries, when it runs, then the two
  cases above fail
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
  clause 19; ADR-draft-the-servers-state-and-its-stores clauses 2, 5 and 6
- ADR-0025 and ADR-0027 for what a revision and a Package are
- Golden path: ticket 03's collection of revisions, which reads the stored assignments under the
  Configuration's row lock
- Seam: the configuration and package port contracts, plus two instances in one test process

## Blocked by

- docs/plan/several-server-instances/03-revisions-entries-and-members-are-written-per-row.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It decides when package data is deleted.
