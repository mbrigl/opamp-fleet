# What a shared store adds to the fleet record is a port of its own

**Kind:** idea, structure.

**Observed.** `AgentStore` (`crates/fleet-server/src/agent_store.rs`) carries the methods that only
a store several instances share needs, each with a default for the store one Server alone writes
(near lines 495 to 751): `take`, `holders`, `takes`, `release`, `moved`, `write_presence` and
`remove_if_silent`. The fleet tells the two apart by `is_shared()`, on 26 branches: 18 in
`fleet.rs`, 4 in `fleet/held.rs` and 4 in `fleet/placement.rs`.

**Idea.** A narrower port that only the PostgreSQL store implements, which `AppState` holds where
there is one. It supersedes ADR-0029 clause 2. The human parked it.

**Effort.** L.

**From.** Ticket 15 of `revision-2026-11-12`, moved to the backlog by the human (2026-11-15).
