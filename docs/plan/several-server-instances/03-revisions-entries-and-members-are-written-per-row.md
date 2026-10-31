# Ticket: Revisions, entries and members are written per row

## What to build

A Configuration's revisions, a Package's entries and a Deployment's members become rows of their
own. An act adds or removes the rows it names, and the revisions a Configuration keeps are reckoned
inside the transaction from those rows and the assignments that pin them. Two operators editing one
Configuration, one Package or one Deployment through two instances keep both changes.

## Acceptance criteria

- [ ] Given two instances, when each adds a revision to the same Configuration at once, then both
  revisions are kept and the bound on kept revisions is reckoned in the transaction
- [ ] Given two instances, when each adds a different entry to one Package at once, then the Package
  holds both
- [ ] Given two instances, when each adds a member to one Deployment at once, then the Deployment
  holds both
- [ ] Given the configuration, package and deployment port contracts, when they run on both stores,
  then they pass, the whole-or-nothing cases included
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clause
  19; ADR-draft-the-servers-state-and-its-stores clauses 2, 5, 6 and 21
- ADR-0025, ADR-0026 and ADR-0027 for what a revision, a Deployment and a Package are
- Golden path: the port contract tests of the configurations, packages and deployments
- Seam: the port contracts, plus keep-both cases on two instances

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes the schema and the stores the rollout reads.
