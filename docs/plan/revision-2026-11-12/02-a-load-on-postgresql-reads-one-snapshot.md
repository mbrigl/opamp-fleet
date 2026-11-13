# Ticket: A load of several statements on PostgreSQL reads one snapshot

## What to build

6c5146c6b had the Configurations loaded from PostgreSQL in one read-only snapshot, because a load
of several statements on autocommit sees another instance's commit between two of them. Three
loads still read their tables one after another, each on autocommit:

- `PgPackageBackend::load` (`postgres/packages.rs`, `read_all`): the Packages, then the entries.
  A Package and its entry committed between the two reads fail the load with "an entry of a
  Package that is not there".
- `PgDeploymentBackend::load` (`postgres/deployments.rs`, `read`): the Deployments, then their
  Packages, then their Configurations, with the same failure for a member committed in between.
- `PgEnrolmentStore::load` (`postgres/enrolment.rs`, `read`): the window, the rejected keys and
  the queue, which can come from three different moments.

A failed load is tried again by the notice loop, so the cost is a delay and a logged error, and a
torn enrolment state until the next read. The read-only snapshot is also written by hand six
times: the helper `snapshot` sits in `postgres/agents.rs`, where `query` calls it, and
`configs.rs` calls it, while `ledger.rs`, `audit.rs`, `agent_page.rs` (twice) and `checkpoint.rs`
build the same transaction themselves.

Move `snapshot` into `postgres/mod.rs`, read every load of several statements through it, and
have the six hand-written builders call it. Correct the sentence of `docs/ARCHITECTURE.md`,
"Server stores", that names the loads which do not read one snapshot.

`docs/plan/fleet-at-scale-costs/01-a-notice-reads-what-it-names.md` moves the same reloads out
from under their write lock. Whichever lands second keeps the other's change.

## Acceptance criteria

- [ ] Given a Package and its entry, or a Deployment and its member, committed by another
  connection between the statements of a load (a held statement as the seam, as the stores' race
  tests hold one), when the load runs, then it answers either the state before or the state after
  and does not fail.
- [ ] Given the PostgreSQL adapters, when the change lands, then one function builds the
  read-only snapshot and every load of several statements reads through it.
- [ ] Given `scripts/check-store.sh postgres`, when it runs, then every store contract passes
  unchanged.

## Read first

- Touches: `crates/fleet-server/src/postgres/mod.rs`, `crates/fleet-server/src/postgres/agents.rs`,
  `crates/fleet-server/src/postgres/packages.rs`, `crates/fleet-server/src/postgres/deployments.rs`,
  `crates/fleet-server/src/postgres/enrolment.rs`, `crates/fleet-server/src/postgres/configs.rs`,
  `crates/fleet-server/src/postgres/ledger.rs`, `crates/fleet-server/src/postgres/audit.rs`,
  `crates/fleet-server/src/postgres/agent_page.rs`,
  `crates/fleet-server/src/postgres/checkpoint.rs`,
  `docs/ARCHITECTURE.md`
- ADR-0023, ADR-0045 clause 10; commit 6c5146c6b, the golden
  path of this change.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
