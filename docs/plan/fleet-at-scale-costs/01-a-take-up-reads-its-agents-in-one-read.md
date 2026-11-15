# Ticket: A take-up reads its Agents in one read

## What to build

A take-up after a rollout reads each held Agent with its own `agent_store.get`: `take_up_agents`
(`crates/fleet-server/src/fleet/held.rs`, near line 424) calls it once per Agent (near lines 435 to
438). That is about 3 300 sequential round trips per instance for a rollout to 10 000 Agents on
three instances. The take-up reads the Agents with one `get_many` instead.

**Effort.** S.

## Acceptance criteria

- [ ] Given a rollout to many Agents held by one instance, when the instance takes the rollout up,
  then it reads them in one `get_many`, not one read per Agent.
- [ ] Given the existing coordination tests, when they run, then they pass unchanged.

## Read first

- Touches: `crates/fleet-server/src/fleet/held.rs`
- ADR-0045 clauses 9 and 10; ADR-0052 clause 2 (`get_many`).
- Seam: the coordination tests in `crates/fleet-server/tests/instances.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
