# Ticket: An instance on PostgreSQL and S3 needs no disk

## What to build

With `[database] store = "postgres"` and `[blobs] store = "s3"` the Server creates no data
directory, and a `data_dir` named in the configuration fails startup naming the key. Upload staging
in the bucket is keyed by lease, and the cleanup deletes only the staging objects of leases that are
no longer live. The Server image stops setting the data directory in its environment; a Server on
files in a container is given `data_dir = "/var/lib/opamp-fleet"` by its operator, on the declared
volume.

## Acceptance criteria

- [ ] Given PostgreSQL and S3, when the Server starts with a read-only root filesystem and no
  volume, then it serves
- [ ] Given PostgreSQL and S3, when `data_dir` is named, then startup fails naming it
- [ ] Given an upload in progress on A, when B starts, then A's staging object survives and A's
  upload completes
- [ ] Given staging objects of a lease that lapsed, when the cleanup runs, then they are deleted
- [ ] Given the image, when its environment is read, then it sets the two listen addresses and
  nothing else
- [ ] Given a Server on files in the container without `data_dir`, when it starts, then it fails
  naming the directory it cannot write
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-the-servers-state-and-its-stores clauses 1, 14 and 16;
  ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clause
  2; ADR-draft-releases-attestations-and-the-server-image clauses 13 to 17
- ADR-0034, which the restatement replaces
- Golden path: the S3 store's staging tests and the Containerfile check
- Seam: the store check against a real bucket, and the image's checks

## Blocked by

- docs/plan/several-server-instances/01-two-instances-share-one-database.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes the image and the startup rules of the stores.
