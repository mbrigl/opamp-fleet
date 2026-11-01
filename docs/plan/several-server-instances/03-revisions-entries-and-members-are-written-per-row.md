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

## Where it stands

Worked with the human on
`ticket/several-server-instances/03-revisions-entries-and-members-are-written-per-row`; the session
stopped where the rest needs the human's decisions.

Done:

- Criterion 1: the Configuration port speaks in parts (`save`, `retain`, `collect`, `remove`), a
  rollout's `release` adds the revisions it pins by hash and deletes none, and on PostgreSQL the
  collection reckons the kept revisions in its own transaction from `configuration_revisions` and
  `agent_assignment_configurations`, holding the Configuration's row `FOR UPDATE`; a rollout holds
  each pinned Configuration's row `FOR KEY SHARE`. No schema change: the revisions were rows
  already. Decided by `two_revisions_rolled_out_at_once_on_two_instances_are_both_kept` in
  `tests/instances.rs` (fails when the collection ignores the stored assignments) and
  `a_collection_on_postgres_keeps_what_a_stored_assignment_pins`.
- Criterion 2: the Package port writes, signs and removes one entry by platform; a signature is
  written only while the stored entry names the bytes it covers, else a `409`. No schema change.
  Decided by `two_entries_added_at_once_on_two_instances_are_both_kept`.
- Criterion 4, for Configurations and Packages: both port contracts and their whole-or-nothing
  cases run on both stores with the new calls.

Open, each a decision for the human before it is built:

- Criterion 3, a Deployment's members per row. Today `deployments` keeps them in its own row, the
  Packages as a `json` column and the Configurations as a `text[]`. The schema of the rows:
  (a) two tables, `deployment_packages (deployment, agent_type, version)` keyed by
  `(deployment, agent_type)`, so "one Package per Agent type" is the key and a type already held
  is decided from the stored row under `FOR UPDATE`, and `deployment_configurations (deployment,
  configuration)` keyed by both; or (b) one table of members with a kind column. Recommended: (a).
- How the old columns migrate. Migration 5 fills the new tables from them, and only adds. Then
  either (a) the new release keeps writing the old columns as well, recomputed from the rows in
  the same transaction under the Deployment's row lock, so an instance of the release before
  reads what is stored, as migration 4 left the Hosts' arrays written; or (b) it stops writing
  them, and the release before sees the members as they were at the migration. Recommended: (a),
  with what the release before writes into the columns left to ticket 11, which already asks
  whether a migration should name this release as its oldest Server.
- Criteria 4 and 5 for Deployments follow once the rest is built.
