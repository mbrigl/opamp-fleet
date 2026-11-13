# Ticket: What a shared store adds to the fleet record's port is a port of its own

## What to build

`AgentStore` (`crates/fleet-server/src/agent_store.rs`) carries the methods only a store several
instances share needs, each with a default for the store one Server alone writes: `take`,
`holders`, `takes`, `release`, `moved`, `write_presence` and `remove_if_silent`. The fleet tells
the two apart by the flag `is_shared()`, on 26 branches of `fleet.rs` and `fleet/held.rs`, which
the work of several instances grew one at a time.

The port's surface is decided by ADR-0030 clause 2. A narrower port that only the PostgreSQL store
implements, which `AppState` holds where there is one, changes that clause, so the first step is a
superseding ADR through `propose-adr`, for the human to decide before anything is built.

## Acceptance criteria

- [ ] Given the human's decision on a draft superseding ADR-0030 clause 2, when it is accepted, then
  the fleet holds the shared store's port where there is one, and no branch on `is_shared()` is
  left.
- [ ] Given the existing tests and the store checks, when the change lands, then they pass.

## Read first

- Touches: `crates/fleet-server/src/agent_store.rs`, `crates/fleet-server/src/fleet.rs`,
  `crates/fleet-server/src/fleet/held.rs`, `crates/fleet-server/src/postgres/agents.rs`,
  `crates/fleet-server/src/fs/`, `docs/adr/`
- ADR-0030 clause 2, ADR-0009; ADR-0045.

## Blocked by

- docs/plan/revision-2026-11-12/03-a-missed-notification-is-caught-up-from-one-read.md
- docs/plan/revision-2026-11-12/04-what-an-instance-keeps-of-an-agent-is-one-entry.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes an accepted ADR, which only the human decides.
